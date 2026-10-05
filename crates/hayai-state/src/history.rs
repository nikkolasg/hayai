//! ZIP 221 chain history tree.
//!
//! The tree is a Merkle mountain range (MMR) over the blocks of one network upgrade. The
//! upstream `zcash_history` crate does the node arithmetic (combine, hash, append). This
//! module keeps the state between blocks: the peaks of the MMR, its length and its upgrade.
//! The peaks are sufficient to append a leaf and to compute the root.
//!
//! Rules (ZIP 221, its NU5 update in ZIP 244, protocol specification §7.6):
//!
//! - A leaf is one block: its hash, time, `nBits`, the work of `nBits`, its height, the
//!   final Sapling root and the count of transactions with Sapling spends or outputs. From
//!   NU5 (tree version 2) a leaf also holds the final Orchard root and the count of
//!   transactions with Orchard actions. From NU6.3 (tree version 3) a leaf also holds the
//!   final Ironwood root and the count of transactions with Ironwood actions.
//! - Each node hash uses the personalization `"ZcashHistory" || branch_id`, with the branch
//!   of the blocks under the node.
//! - The tree holds only the blocks of the current upgrade. The first block of an upgrade
//!   starts a new tree that holds only that block.
//! - The header field at offset 68 of block `n` commits to the tree of the blocks of the
//!   preceding upgrade activation up to block `n - 1`. Thus an activation block commits to
//!   the whole tree of the previous upgrade, and the block after it commits to a tree of one
//!   leaf. The Heartwood activation block has no previous tree: its field is all zeros.
//! - Heartwood and Canopy: the field is `hashChainHistoryRoot`. NU5 and later: the field is
//!   `hashBlockCommitments = BLAKE2b-256("ZcashBlockCommit", history_root ||
//!   auth_data_root || [0; 32])`. Sapling and Blossom: the field is the final Sapling root of
//!   the block itself. Sprout and Overwinter: the field is reserved.
//!
//! The rule set of an upgrade names its tree version (`hayai_consensus::RuleSet::history`).
//! An append for an upgrade without a rule set is [`HistoryError::Unsupported`].
//!
//! The peaks cannot be derived from headers: each leaf holds the final note commitment roots
//! of its block, and an inner node holds only hashes and sums. A node that starts from a
//! state after Heartwood seeds the tree with [`HistoryState::from_peaks`] (for example from
//! the peaks that Zebra or Zakura keep). Before Heartwood the tree is empty, so a node that
//! starts there needs no seed ([`HistoryState::empty`]).

use hayai_consensus::{HistoryVersion, RuleSet};
use hayai_crypto::primitive_types::U256;
use hayai_crypto::zcash_history::{
    Entry, EntryLink, NodeData, NodeDataV2, NodeDataV3, Tree, Version, V1 as TreeV1, V2 as TreeV2,
    V3 as TreeV3,
};
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_wire::RawBlock;

use crate::Anchors;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    #[error("bits {0:#010x} do not encode a valid target")]
    InvalidBits(u32),
    #[error("the history tree of {0:?} is not supported: the upgrade has no rule set")]
    Unsupported(BranchId),
    #[error("{0:?} has no history tree, so its state must be empty")]
    PreHeartwood(BranchId),
    #[error(
        "block at height {found} does not follow the history tree, which ends at height {last}"
    )]
    Height { last: u64, found: u32 },
    #[error("the peaks do not describe a tree of {length} nodes")]
    Peaks { length: u32 },
    #[error("peak at position {position} is not a canonical node encoding")]
    Encoding { position: u32 },
    #[error("zcash_history: {0}")]
    Tree(String),
}

/// The per-block values of a history leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryLeaf {
    pub hash: [u8; 32],
    pub time: u32,
    pub bits: u32,
    pub height: u32,
    /// Sapling note commitment tree root after the block.
    pub sapling_root: [u8; 32],
    /// Orchard note commitment tree root after the block (not used before NU5).
    pub orchard_root: [u8; 32],
    /// Ironwood note commitment tree root after the block (not used before NU6.3).
    pub ironwood_root: [u8; 32],
    /// Transactions with Sapling spends or Sapling outputs.
    pub sapling_tx: u64,
    /// Transactions with Orchard actions.
    pub orchard_tx: u64,
    /// Transactions with Ironwood actions.
    pub ironwood_tx: u64,
}

impl HistoryLeaf {
    /// The leaf of `raw` at `height`, with the tree roots after the block.
    pub fn from_block(raw: &RawBlock, height: u32, roots: &Anchors) -> Self {
        let mut sapling_tx = 0u64;
        let mut orchard_tx = 0u64;
        let mut ironwood_tx = 0u64;
        for t in &raw.txs {
            if let Some(b) = t.tx.sapling_bundle() {
                if !b.shielded_spends().is_empty() || !b.shielded_outputs().is_empty() {
                    sapling_tx += 1;
                }
            }
            // An Orchard or Ironwood bundle always has at least one action.
            orchard_tx += t.tx.orchard_bundle().map_or(0, |_| 1);
            ironwood_tx += t.tx.ironwood_bundle().map_or(0, |_| 1);
        }
        Self {
            hash: raw.hash().0,
            time: raw.header.time,
            bits: raw.header.bits,
            height,
            sapling_root: roots.sapling,
            orchard_root: roots.orchard,
            ironwood_root: roots.ironwood,
            sapling_tx,
            orchard_tx,
            ironwood_tx,
        }
    }
}

/// The work of a block with target `bits` (ZIP 221 field `nSubTreeTotalWork`):
/// `hayai_consensus::difficulty::block_work`, with the history error for `bits` that encode
/// no target.
pub fn block_work(bits: u32) -> Result<U256, HistoryError> {
    hayai_consensus::difficulty::block_work(bits).ok_or(HistoryError::InvalidBits(bits))
}

/// What the 32-byte header field at offset 68 must hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderCommitment {
    /// Sprout and Overwinter: a reserved field without a rule.
    Reserved,
    /// The field must equal this value.
    Expected([u8; 32]),
    /// The rule needs the history state after the parent, and the node does not know it.
    /// The block's layer then has no history state either.
    ParentUnknown,
}

/// The rule for the header field of a block of `branch` on top of a parent whose history
/// state is `parent`. `final_sapling_root` is the Sapling root after the block itself,
/// `auth_data_root` the ZIP 244 root of the block's authorizing data.
pub fn header_commitment(
    branch: BranchId,
    parent: Option<&HistoryState>,
    final_sapling_root: &[u8; 32],
    auth_data_root: &[u8; 32],
) -> HeaderCommitment {
    match (branch, parent) {
        (BranchId::Sprout | BranchId::Overwinter, _) => HeaderCommitment::Reserved,
        (BranchId::Sapling | BranchId::Blossom, _) => {
            HeaderCommitment::Expected(*final_sapling_root)
        }
        (_, None) => HeaderCommitment::ParentUnknown,
        (BranchId::Heartwood | BranchId::Canopy, Some(parent)) => {
            HeaderCommitment::Expected(parent.root())
        }
        (_, Some(parent)) => HeaderCommitment::Expected(hayai_wire::block_commitments(
            &parent.root(),
            auth_data_root,
        )),
    }
}

/// The tree version of an upgrade.
enum TreeVersion {
    /// Before Heartwood: no tree.
    None,
    /// Heartwood and Canopy.
    V1,
    /// NU5 to NU6.2.
    V2,
    /// From NU6.3.
    V3,
}

fn tree_version(branch: BranchId) -> Result<TreeVersion, HistoryError> {
    // The rule set of the upgrade names the version. An upgrade without a rule set has no
    // known version.
    match RuleSet::of_branch(branch).map(|rules| rules.history) {
        Ok(HistoryVersion::None) => Ok(TreeVersion::None),
        Ok(HistoryVersion::V1) => Ok(TreeVersion::V1),
        Ok(HistoryVersion::V2) => Ok(TreeVersion::V2),
        Ok(HistoryVersion::V3) => Ok(TreeVersion::V3),
        Err(_) => Err(HistoryError::Unsupported(branch)),
    }
}

/// A tree version and the leaf it builds from a block.
trait LeafVersion: Version {
    fn leaf(branch: u32, leaf: &HistoryLeaf, work: U256) -> Self::NodeData;
}

fn leaf_v1(branch: u32, leaf: &HistoryLeaf, work: U256) -> NodeData {
    NodeData {
        consensus_branch_id: branch,
        subtree_commitment: leaf.hash,
        start_time: leaf.time,
        end_time: leaf.time,
        start_target: leaf.bits,
        end_target: leaf.bits,
        start_sapling_root: leaf.sapling_root,
        end_sapling_root: leaf.sapling_root,
        subtree_total_work: work,
        start_height: u64::from(leaf.height),
        end_height: u64::from(leaf.height),
        sapling_tx: leaf.sapling_tx,
    }
}

impl LeafVersion for TreeV1 {
    fn leaf(branch: u32, leaf: &HistoryLeaf, work: U256) -> NodeData {
        leaf_v1(branch, leaf, work)
    }
}

fn leaf_v2(branch: u32, leaf: &HistoryLeaf, work: U256) -> NodeDataV2 {
    NodeDataV2 {
        v1: leaf_v1(branch, leaf, work),
        start_orchard_root: leaf.orchard_root,
        end_orchard_root: leaf.orchard_root,
        orchard_tx: leaf.orchard_tx,
    }
}

impl LeafVersion for TreeV2 {
    fn leaf(branch: u32, leaf: &HistoryLeaf, work: U256) -> NodeDataV2 {
        leaf_v2(branch, leaf, work)
    }
}

impl LeafVersion for TreeV3 {
    fn leaf(branch: u32, leaf: &HistoryLeaf, work: U256) -> NodeDataV3 {
        NodeDataV3 {
            v2: leaf_v2(branch, leaf, work),
            start_ironwood_root: leaf.ironwood_root,
            end_ironwood_root: leaf.ironwood_root,
            ironwood_tx: leaf.ironwood_tx,
        }
    }
}

/// The history tree after a block: the peaks of the MMR of the blocks of `upgrade` up to
/// that block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryState {
    upgrade: BranchId,
    /// Nodes in the array form of the MMR.
    length: u32,
    /// `(position, node)` for each peak, left to right. A node is the ZIP 221 serialization
    /// of the node data (the form that `Version::to_bytes` writes).
    peaks: Vec<(u32, Vec<u8>)>,
    root: [u8; 32],
    /// Height of the newest leaf. `None` for an empty tree.
    last_height: Option<u64>,
}

impl HistoryState {
    /// The empty tree of `upgrade`. Its root is all zeros: the value of the header field of
    /// the Heartwood activation block. Before Heartwood every state is empty.
    pub fn empty(upgrade: BranchId) -> Self {
        Self {
            upgrade,
            length: 0,
            peaks: Vec::new(),
            root: [0; 32],
            last_height: None,
        }
    }

    /// A state from saved peaks: `length` nodes in the array form, and the `(position,
    /// node)` of each peak left to right, with the node in its ZIP 221 serialization. A
    /// Zebra or Zakura `Entry` is this node after a 1-byte (leaf) or 9-byte (inner node)
    /// prefix.
    ///
    /// Fails when the positions are not the peaks of an MMR of `length` nodes, when a node
    /// does not decode to exactly its bytes, or when the peaks do not cover contiguous
    /// heights.
    pub fn from_peaks(
        upgrade: BranchId,
        length: u32,
        peaks: Vec<(u32, Vec<u8>)>,
    ) -> Result<Self, HistoryError> {
        if length == 0 && peaks.is_empty() {
            return Ok(Self::empty(upgrade));
        }
        match tree_version(upgrade)? {
            TreeVersion::None => Err(HistoryError::PreHeartwood(upgrade)),
            TreeVersion::V1 => Self::from_peaks_v::<TreeV1>(upgrade, length, peaks),
            TreeVersion::V2 => Self::from_peaks_v::<TreeV2>(upgrade, length, peaks),
            TreeVersion::V3 => Self::from_peaks_v::<TreeV3>(upgrade, length, peaks),
        }
    }

    fn from_peaks_v<V: LeafVersion>(
        upgrade: BranchId,
        length: u32,
        peaks: Vec<(u32, Vec<u8>)>,
    ) -> Result<Self, HistoryError> {
        let positions = peak_positions(length);
        if positions.len() != peaks.len() || positions.iter().zip(&peaks).any(|(a, (b, _))| a != b)
        {
            return Err(HistoryError::Peaks { length });
        }
        let entries = decode_peaks::<V>(u32::from(upgrade), &peaks)?;
        // The peaks cover heights without a gap, each peak holds a power of two of leaves,
        // and the leaf count gives `length` nodes.
        let mut leaves = 0u64;
        let mut next: Option<u64> = None;
        for (_, entry) in &entries {
            let start = V::start_height(entry.data());
            let end = V::end_height(entry.data());
            match next {
                Some(expected) if expected != start => return Err(HistoryError::Peaks { length }),
                _ => {}
            }
            if end < start || !(end - start + 1).is_power_of_two() {
                return Err(HistoryError::Peaks { length });
            }
            let count = end - start + 1;
            leaves += count;
            next = Some(end + 1);
        }
        if 2 * leaves - u64::from(leaves.count_ones()) != u64::from(length) {
            return Err(HistoryError::Peaks { length });
        }
        let tree = Tree::<V>::new(length, entries, Vec::new());
        let root = V::hash(tree.root_node().map_err(tree_error)?.data());
        Ok(Self {
            upgrade,
            length,
            peaks,
            root,
            last_height: next.map(|n| n - 1),
        })
    }

    /// The upgrade whose blocks the tree holds.
    pub fn upgrade(&self) -> BranchId {
        self.upgrade
    }

    /// Nodes in the array form of the MMR.
    pub fn length(&self) -> u32 {
        self.length
    }

    /// `(position, ZIP 221 node)` of each peak, left to right.
    pub fn peaks(&self) -> &[(u32, Vec<u8>)] {
        &self.peaks
    }

    /// `hashChainHistoryRoot` of the tree; all zeros for the empty tree.
    pub fn root(&self) -> [u8; 32] {
        self.root
    }

    /// Height of the newest block in the tree.
    pub fn last_height(&self) -> Option<u64> {
        self.last_height
    }

    /// The state after a block of `branch` with `leaf`. A block of another upgrade than the
    /// tree's starts a new tree. A block before Heartwood gives the empty state of its
    /// upgrade.
    pub fn append(&self, branch: BranchId, leaf: &HistoryLeaf) -> Result<Self, HistoryError> {
        let fresh = branch != self.upgrade || self.length == 0;
        if !fresh {
            let Some(last) = self.last_height else {
                unreachable!("a tree with nodes has a newest leaf");
            };
            if last + 1 != u64::from(leaf.height) {
                return Err(HistoryError::Height {
                    last,
                    found: leaf.height,
                });
            }
        }
        let peaks: &[(u32, Vec<u8>)] = if fresh { &[] } else { &self.peaks };
        let length = if fresh { 0 } else { self.length };
        match tree_version(branch)? {
            TreeVersion::None => Ok(Self::empty(branch)),
            TreeVersion::V1 => append_v::<TreeV1>(branch, length, peaks, leaf),
            TreeVersion::V2 => append_v::<TreeV2>(branch, length, peaks, leaf),
            TreeVersion::V3 => append_v::<TreeV3>(branch, length, peaks, leaf),
        }
    }
}

/// The history state after a block of `branch` with `leaf`, on top of a parent with state
/// `parent`. `None` when the parent's state is unknown and the block is at or after
/// Heartwood: the peaks are then unknown too.
pub fn history_after(
    parent: Option<&HistoryState>,
    branch: BranchId,
    leaf: &HistoryLeaf,
) -> Result<Option<HistoryState>, HistoryError> {
    match (parent, tree_version(branch)?) {
        (_, TreeVersion::None) => Ok(Some(HistoryState::empty(branch))),
        (None, _) => Ok(None),
        (Some(parent), _) => parent.append(branch, leaf).map(Some),
    }
}

fn tree_error(error: hayai_crypto::zcash_history::Error) -> HistoryError {
    HistoryError::Tree(error.to_string())
}

/// Decodes peaks into tree entries. A peak is a complete subtree, so the tree never reads
/// its children, and the entry is a leaf entry with the node's data.
fn decode_peaks<V: Version>(
    branch: u32,
    peaks: &[(u32, Vec<u8>)],
) -> Result<Vec<(u32, Entry<V>)>, HistoryError> {
    peaks
        .iter()
        .map(|(position, bytes)| {
            let encoding = HistoryError::Encoding {
                position: *position,
            };
            let Ok(data) = V::from_bytes(branch, bytes) else {
                return Err(encoding);
            };
            if V::to_bytes(&data) != *bytes {
                return Err(encoding);
            }
            Ok((*position, Entry::new_leaf(data)))
        })
        .collect()
}

fn append_v<V: LeafVersion>(
    branch: BranchId,
    length: u32,
    peaks: &[(u32, Vec<u8>)],
    leaf: &HistoryLeaf,
) -> Result<HistoryState, HistoryError> {
    let branch_id = u32::from(branch);
    let data = V::leaf(branch_id, leaf, block_work(leaf.bits)?);
    let (tree, length) = if length == 0 {
        (
            Tree::<V>::new(1, vec![(0, Entry::new_leaf(data))], Vec::new()),
            1,
        )
    } else {
        let mut tree = Tree::<V>::new(length, decode_peaks::<V>(branch_id, peaks)?, Vec::new());
        let appended = tree.append_leaf(data).map_err(tree_error)?;
        let added = u32::try_from(appended.len()).expect("at most 33 nodes per append");
        (tree, length + added)
    };
    let mut new_peaks = Vec::new();
    for position in peak_positions(length) {
        let node = tree
            .resolve_link(EntryLink::Stored(position))
            .map_err(tree_error)?;
        new_peaks.push((position, V::to_bytes(node.data())));
    }
    let root = V::hash(tree.root_node().map_err(tree_error)?.data());
    Ok(HistoryState {
        upgrade: branch,
        length,
        peaks: new_peaks,
        root,
        last_height: Some(u64::from(leaf.height)),
    })
}

/// Positions of the peaks of an MMR with `length` nodes in its array form, left to right.
///
/// The highest peak has altitude `alt = floor(log2(length + 1)) - 1` and position
/// `2^(alt + 1) - 2`. Each further peak is the right sibling of the previous one, or the
/// left child of that sibling when the sibling is past the end. (Zebra
/// `NonEmptyHistoryTree::prune`, after zcashd `CCoinsViewCache::PreloadHistoryTree`.)
fn peak_positions(length: u32) -> Vec<u32> {
    let mut peaks = Vec::new();
    if length == 0 {
        return peaks;
    }
    let length = u64::from(length);
    let mut alt = i64::from(64 - (length + 1).leading_zeros()) - 2;
    let mut pos: u64 = (1 << (alt + 1)) - 2;
    loop {
        if pos >= length {
            pos -= 1 << alt;
            alt -= 1;
        }
        if pos < length {
            peaks.push(u32::try_from(pos).expect("below length"));
            pos += (1 << (alt + 1)) - 1;
        }
        if alt <= 0 {
            break;
        }
    }
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Peak positions from a direct construction: an MMR of `n` leaves is the perfect
    /// subtrees of the binary decomposition of `n`, largest first.
    fn reference_positions(leaves: u64) -> Vec<u32> {
        let mut out = Vec::new();
        let mut offset = 0u64;
        for bit in (0..40).rev() {
            if leaves & (1 << bit) != 0 {
                let size = (1u64 << (bit + 1)) - 1;
                out.push(u32::try_from(offset + size - 1).unwrap());
                offset += size;
            }
        }
        out
    }

    #[test]
    fn peak_positions_match_the_binary_decomposition() {
        for leaves in 1..2_000u64 {
            let length = 2 * leaves - u64::from(leaves.count_ones());
            assert_eq!(
                peak_positions(u32::try_from(length).unwrap()),
                reference_positions(leaves),
                "{leaves} leaves"
            );
        }
    }

    #[test]
    fn work_of_known_targets() {
        // The minimum difficulty of mainnet (`0x1f07ffff`): target 0x07ffff * 2^224.
        let work = block_work(0x1f07_ffff).unwrap();
        let target = U256::from(0x07ffffu64) << 224;
        let expected = (U256::MAX - target) / (target + 1) + 1;
        assert_eq!(work, expected);
        // The largest target (`0x7fffff * 2^232`, just below 2^255) gives a work of 2, and
        // `0x7fff * 2^232` (about 2^247) a work of 512.
        assert_eq!(block_work(0x207f_ffff).unwrap(), U256::from(2u64));
        assert_eq!(block_work(0x2000_7fff).unwrap(), U256::from(512u64));
        let Err(HistoryError::InvalidBits(_)) = block_work(0x0100_0001) else {
            panic!("a target of zero has no work");
        };
        let Err(HistoryError::InvalidBits(_)) = block_work(0x1d80_0000) else {
            panic!("a negative target has no work");
        };
    }
}
