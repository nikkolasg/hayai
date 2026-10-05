//! Note-commitment frontiers with batched append (Orchard and Sapling), and the Sprout
//! frontier (`sprout.rs`).
//!
//! [`OrchardFrontier`] and [`SaplingFrontier`] wrap the upstream
//! `incrementalmerkletree::Frontier<_, 32>`. Frontiers, nodes and roots therefore stay the
//! upstream types. `append_many` appends the leaves of a block with about `N + 32` hashes. It
//! hashes each level of each new perfect subtree as one batch: Orchard through
//! `hayai_sinsemilla::merkle_crh_orchard_many`, and Sapling through the upstream Pedersen hash
//! in parallel over the independent nodes. The result is byte-identical to an append of the
//! leaves one by one with `Frontier::append` (see `batch.rs`).
//!
//! Type conversions. Orchard nodes carry a `pallas::Base` that upstream exposes only as bytes.
//! Every node therefore crosses into the hasher through `MerkleHashOrchard::to_bytes` and back
//! through `MerkleHashOrchard::from_bytes`. Both are canonical encodings, so the round trip is
//! exact. The hasher hashes Sapling nodes as nodes. The functions return roots as the upstream
//! anchor types.

mod batch;
mod sprout;

pub use sprout::{SproutFrontier, SproutNode, SPROUT_DEPTH};

use hayai_crypto::{ff, incrementalmerkletree, orchard, pasta_curves, sapling_crypto};

use ff::PrimeField;
use incrementalmerkletree::{frontier::Frontier, Hashable, Level};
use orchard::tree::MerkleHashOrchard;
use pasta_curves::pallas;
use rayon::prelude::*;
use sapling_crypto::Node;

use crate::batch::Combine;

/// Depth of the Orchard and the Sapling note commitment tree.
pub const DEPTH: u8 = 32;

const _: () = assert!(hayai_sinsemilla::MERKLE_DEPTH_ORCHARD == DEPTH as usize);
const _: () = assert!(sapling_crypto::NOTE_COMMITMENT_TREE_DEPTH == DEPTH);

/// Errors of a batched append.
#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    /// The leaves do not fit in the tree.
    #[error("tree holds {capacity} leaves, append would make {requested}")]
    Full { capacity: u64, requested: u64 },
    /// The upstream frontier rejected the reassembled parts. This is not reachable after the
    /// capacity check. It stays an error and not a panic.
    #[error("frontier reconstruction failed: {0:?}")]
    Frontier(incrementalmerkletree::frontier::FrontierError),
}

/// The Orchard note commitment frontier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrchardFrontier {
    inner: Frontier<MerkleHashOrchard, DEPTH>,
}

impl OrchardFrontier {
    /// The empty tree.
    pub fn empty() -> Self {
        Self {
            inner: Frontier::empty(),
        }
    }

    /// Wraps an upstream frontier.
    pub fn from_frontier(inner: Frontier<MerkleHashOrchard, DEPTH>) -> Self {
        Self { inner }
    }

    /// The upstream frontier.
    pub fn frontier(&self) -> &Frontier<MerkleHashOrchard, DEPTH> {
        &self.inner
    }

    /// Unwraps into the upstream frontier.
    pub fn into_frontier(self) -> Frontier<MerkleHashOrchard, DEPTH> {
        self.inner
    }

    /// Appends `leaves` in order and returns the new anchor.
    pub fn append_many(
        &mut self,
        leaves: &[MerkleHashOrchard],
    ) -> Result<orchard::tree::Anchor, TreeError> {
        self.inner = batch::append_many(&self.inner, leaves, &OrchardCombine)?;
        Ok(self.root())
    }

    /// The anchor of the current tree.
    pub fn root(&self) -> orchard::tree::Anchor {
        batch::root(&self.inner, &OrchardCombine).into()
    }
}

/// The frontier of the Ironwood note commitment tree (NU6.3). The Ironwood tree has the
/// node type and the hash of the Orchard tree (MerkleCRH^Orchard).
pub type IronwoodFrontier = OrchardFrontier;

/// The Sapling note commitment frontier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaplingFrontier {
    inner: Frontier<Node, DEPTH>,
}

impl SaplingFrontier {
    /// The empty tree.
    pub fn empty() -> Self {
        Self {
            inner: Frontier::empty(),
        }
    }

    /// Wraps an upstream frontier.
    pub fn from_frontier(inner: Frontier<Node, DEPTH>) -> Self {
        Self { inner }
    }

    /// The upstream frontier.
    pub fn frontier(&self) -> &Frontier<Node, DEPTH> {
        &self.inner
    }

    /// Unwraps into the upstream frontier.
    pub fn into_frontier(self) -> Frontier<Node, DEPTH> {
        self.inner
    }

    /// Appends `leaves` in order and returns the new anchor.
    pub fn append_many(&mut self, leaves: &[Node]) -> Result<sapling_crypto::Anchor, TreeError> {
        self.inner = batch::append_many(&self.inner, leaves, &SaplingCombine)?;
        Ok(self.root())
    }

    /// The anchor of the current tree.
    pub fn root(&self) -> sapling_crypto::Anchor {
        batch::root(&self.inner, &SaplingCombine).into()
    }
}

struct OrchardCombine;

fn orchard_base(node: &MerkleHashOrchard) -> pallas::Base {
    Option::from(pallas::Base::from_repr(node.to_bytes()))
        .expect("MerkleHashOrchard holds a canonical field element")
}

fn orchard_node(x: pallas::Base) -> MerkleHashOrchard {
    Option::from(MerkleHashOrchard::from_bytes(&x.to_repr()))
        .expect("a field element encodes canonically")
}

impl Combine<MerkleHashOrchard> for OrchardCombine {
    fn one(
        &self,
        level: Level,
        left: &MerkleHashOrchard,
        right: &MerkleHashOrchard,
    ) -> MerkleHashOrchard {
        orchard_node(hayai_sinsemilla::merkle_crh_orchard(
            level.into(),
            orchard_base(left),
            orchard_base(right),
        ))
    }

    fn pairs_local(&self, level: Level, nodes: &[MerkleHashOrchard]) -> Vec<MerkleHashOrchard> {
        let out = hayai_sinsemilla::merkle_crh_orchard_local(level.into(), &orchard_pairs(nodes));
        out.into_iter().map(orchard_node).collect()
    }

    fn pairs(&self, level: Level, nodes: &[MerkleHashOrchard]) -> Vec<MerkleHashOrchard> {
        let out = hayai_sinsemilla::merkle_crh_orchard_many(level.into(), &orchard_pairs(nodes));
        out.into_iter().map(orchard_node).collect()
    }
}

fn orchard_pairs(nodes: &[MerkleHashOrchard]) -> Vec<(pallas::Base, pallas::Base)> {
    nodes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[left, right]| (orchard_base(left), orchard_base(right)))
        .collect()
}

struct SaplingCombine;

/// Pairs below which the hasher hashes a Sapling level on the calling thread.
const SAPLING_PARALLEL_MIN: usize = 4;

impl Combine<Node> for SaplingCombine {
    fn one(&self, level: Level, left: &Node, right: &Node) -> Node {
        Node::combine(level, left, right)
    }

    fn pairs_local(&self, level: Level, nodes: &[Node]) -> Vec<Node> {
        nodes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[left, right]| Node::combine(level, left, right))
            .collect()
    }

    fn pairs(&self, level: Level, nodes: &[Node]) -> Vec<Node> {
        if nodes.len() / 2 < SAPLING_PARALLEL_MIN {
            self.pairs_local(level, nodes)
        } else {
            nodes
                .par_chunks_exact(2)
                .map(|pair| Node::combine(level, &pair[0], &pair[1]))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests;
