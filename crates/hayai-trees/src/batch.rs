//! Batched append on an `incrementalmerkletree::Frontier`, generic over the node type.
//!
//! A frontier at position `p` is the leaf at `p` plus one ommer per set bit of `p`. The ommer
//! is the root of the complete `2^k`-leaf subtree that ends just before the path. An append of
//! the leaves `p+1 .. p+1+n` therefore has these steps:
//!
//! 1. Expand the frontier into per-level slots (`slots[k]` = root of a complete `2^k` subtree
//!    that covers leaves `0..=p`). Merge the leaf at `p` upward through the occupied slots.
//! 2. Cut the new leaves, except the last one, into maximal aligned perfect subtrees. The
//!    alignment of the start position and the count force the sizes.
//! 3. Hash the subtrees. First, hash every aligned block of `2^b` leaves as one rayon task on
//!    one thread (its levels run locally, with lanes where they are wide enough). Then hash
//!    the block roots of each subtree level by level, with every level of every subtree in one
//!    pool call. The hash count is the number of internal nodes, about `n`.
//! 4. Merge each subtree root into the slots in leaf order, and carry upward.
//! 5. The last leaf becomes the frontier leaf, and the occupied slots become its ommers.
//!
//! The result is the frontier that `Frontier::append` would produce leaf by leaf. The tests
//! check this against upstream on random positions and counts. This module also computes the
//! root, so that the root uses the fast hasher and not `Hashable::combine`.

use hayai_crypto::incrementalmerkletree;
use incrementalmerkletree::{frontier::Frontier, Hashable, Level, Position};
use rayon::prelude::*;

use crate::TreeError;

/// Hashing of same-level pairs for one node type.
pub(crate) trait Combine<H> {
    /// `MerkleCRH(level, left, right)` for one pair.
    fn one(&self, level: Level, left: &H, right: &H) -> H;
    /// `MerkleCRH(level, ...)` of consecutive pairs of `nodes` (even length), in order, on the
    /// calling thread.
    fn pairs_local(&self, level: Level, nodes: &[H]) -> Vec<H>;
    /// The same as `pairs_local`, on the rayon pool.
    fn pairs(&self, level: Level, nodes: &[H]) -> Vec<H>;
}

/// Appends `leaves` to `frontier` and returns the new frontier.
pub(crate) fn append_many<H, C, const DEPTH: u8>(
    frontier: &Frontier<H, DEPTH>,
    leaves: &[H],
    combine: &C,
) -> Result<Frontier<H, DEPTH>, TreeError>
where
    H: Hashable + Clone + Send + Sync,
    C: Combine<H> + Sync,
{
    append_many_with_block(frontier, leaves, combine, block_level(leaves.len()))
}

/// `append_many` that hashes blocks of `2^block` leaves as single tasks.
pub(crate) fn append_many_with_block<H, C, const DEPTH: u8>(
    frontier: &Frontier<H, DEPTH>,
    leaves: &[H],
    combine: &C,
    block: u32,
) -> Result<Frontier<H, DEPTH>, TreeError>
where
    H: Hashable + Clone + Send + Sync,
    C: Combine<H> + Sync,
{
    let capacity = 1u64 << DEPTH;
    let size = frontier.tree_size();
    let requested = size + leaves.len() as u64;
    if requested > capacity {
        return Err(TreeError::Full {
            capacity,
            requested,
        });
    }
    let Some((tip, to_merge)) = leaves.split_last() else {
        return Ok(frontier.clone());
    };

    let mut slots: Vec<Option<H>> = vec![None; usize::from(DEPTH)];
    if let Some(current) = frontier.value() {
        let position = u64::from(current.position());
        let mut ommers = current.ommers().iter();
        for (level, slot) in slots.iter_mut().enumerate() {
            if position & (1u64 << level) != 0 {
                *slot = Some(
                    ommers
                        .next()
                        .expect("one ommer per set position bit")
                        .clone(),
                );
            }
        }
        merge(&mut slots, 0, current.leaf().clone(), combine);
    }

    let roots = subtree_roots(size, to_merge, combine, block);
    for (level, root) in roots {
        merge(&mut slots, level, root, combine);
    }

    let position = Position::from(size + to_merge.len() as u64);
    let ommers = slots.into_iter().flatten().collect();
    Frontier::from_parts(position, tip.clone(), ommers).map_err(TreeError::Frontier)
}

/// The root of `frontier` at depth `DEPTH`, as `Frontier::root` computes it.
pub(crate) fn root<H, C, const DEPTH: u8>(frontier: &Frontier<H, DEPTH>, combine: &C) -> H
where
    H: Hashable + Clone,
    C: Combine<H>,
{
    let Some(current) = frontier.value() else {
        return H::empty_root(Level::from(DEPTH));
    };
    let position = u64::from(current.position());
    let mut ommers = current.ommers().iter();
    let mut node = current.leaf().clone();
    for level in 0..DEPTH {
        let lvl = Level::from(level);
        node = if position & (1u64 << level) != 0 {
            let ommer = ommers.next().expect("one ommer per set position bit");
            combine.one(lvl, ommer, &node)
        } else {
            combine.one(lvl, &node, &H::empty_root(lvl))
        };
    }
    node
}

/// Merges the root of a complete `2^level` subtree into `slots`. It carries while the slot is
/// occupied. The existing slot content is older, so it is the left child.
fn merge<H, C>(slots: &mut [Option<H>], level: usize, node: H, combine: &C)
where
    H: Hashable + Clone,
    C: Combine<H>,
{
    let mut carry = node;
    let mut level = level;
    loop {
        match slots[level].take() {
            None => {
                slots[level] = Some(carry);
                return;
            }
            Some(left) => {
                carry = combine.one(Level::from(level as u8), &left, &carry);
                level += 1;
            }
        }
    }
}

/// Largest block level. Blocks of `2^BLOCK_LEVEL_MAX` leaves keep the lane widths of one task
/// useful, and the pool still has enough tasks.
const BLOCK_LEVEL_MAX: u32 = 6;

/// Block level for `n` leaves: about two blocks per pool thread, at most `2^BLOCK_LEVEL_MAX`
/// leaves each and at least pairs.
fn block_level(n: usize) -> u32 {
    let threads = rayon::current_num_threads().max(1);
    let per_block = n / (2 * threads);
    if per_block < 2 {
        1
    } else {
        per_block.ilog2().min(BLOCK_LEVEL_MAX)
    }
}

/// Cuts `leaves` (from tree position `start`) into maximal aligned perfect subtrees and
/// returns `(level, root)` for each, in leaf order. It hashes blocks of `2^block` leaves as
/// single tasks.
fn subtree_roots<H, C>(start: u64, leaves: &[H], combine: &C, block: u32) -> Vec<(usize, H)>
where
    H: Hashable + Clone + Send + Sync,
    C: Combine<H> + Sync,
{
    // (level, leaves) per subtree.
    let mut subtrees: Vec<(usize, &[H])> = Vec::new();
    let mut position = start;
    let mut offset = 0usize;
    while offset < leaves.len() {
        let remaining = (leaves.len() - offset) as u64;
        let by_count = 63 - remaining.leading_zeros();
        let by_alignment = if position == 0 {
            by_count
        } else {
            position.trailing_zeros()
        };
        let level = by_count.min(by_alignment) as usize;
        let len = 1usize << level;
        subtrees.push((level, &leaves[offset..offset + len]));
        offset += len;
        position += len as u64;
    }

    // Phase 1: every aligned block of at most 2^block leaves is one task. A subtree smaller
    // than a block is one task on its own.
    let block_leaves = 1usize << block;
    let blocks: Vec<&[H]> = subtrees
        .iter()
        .flat_map(|(_, nodes)| nodes.chunks(block_leaves))
        .collect();
    let block_roots: Vec<H> = blocks
        .into_par_iter()
        .map(|block| {
            let mut nodes = block.to_vec();
            let mut level = 0u8;
            while nodes.len() > 1 {
                nodes = combine.pairs_local(Level::from(level), &nodes);
                level += 1;
            }
            nodes.pop().expect("one root per block")
        })
        .collect();

    // Phase 2: the block roots of each subtree, level by level across all subtrees.
    let mut taken = 0usize;
    let mut subtrees: Vec<(usize, Vec<H>)> = subtrees
        .into_iter()
        .map(|(level, nodes)| {
            let count = nodes.len().div_ceil(block_leaves);
            let roots = block_roots[taken..taken + count].to_vec();
            taken += count;
            (level, roots)
        })
        .collect();
    debug_assert_eq!(taken, block_roots.len());

    let max_level = subtrees.iter().map(|(level, _)| *level).max().unwrap_or(0);
    for level in (block as usize)..max_level {
        // Every subtree with more than `level` levels contributes all its current nodes.
        let batch: Vec<H> = subtrees
            .iter()
            .filter(|(k, _)| *k > level)
            .flat_map(|(_, nodes)| nodes.iter().cloned())
            .collect();
        let hashed = combine.pairs(Level::from(level as u8), &batch);
        let mut taken = 0usize;
        for (k, nodes) in subtrees.iter_mut() {
            if *k > level {
                let half = nodes.len() / 2;
                *nodes = hashed[taken..taken + half].to_vec();
                taken += half;
            }
        }
        debug_assert_eq!(taken, hashed.len());
    }

    subtrees
        .into_iter()
        .map(|(level, mut nodes)| {
            debug_assert_eq!(nodes.len(), 1);
            (level, nodes.pop().expect("one root per subtree"))
        })
        .collect()
}
