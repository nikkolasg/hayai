//! Inputs and bodies of the tree benchmarks (`benches/trees.rs`) and of the
//! `tree_append_2048` sysbench scenario: one block append plus the root from a frontier at a
//! fixed non-aligned position, hayai's `append_many` against zakura-chain's `append_batch`.

use ff::{Field, PrimeField};
use hayai_crypto::rng::{RngCore, SeedableRng, StdRng};
use hayai_crypto::{ff, incrementalmerkletree, orchard, pasta_curves, sapling_crypto};
use hayai_trees::{OrchardFrontier, DEPTH};
use incrementalmerkletree::{frontier::Frontier, Position};
use orchard::tree::MerkleHashOrchard;
use pasta_curves::pallas;
use sapling_crypto::Node;

use super::{Built, Impl};

pub const START_POSITION: u64 = 123_456_789;

pub fn orchard_leaf(rng: &mut StdRng) -> MerkleHashOrchard {
    MerkleHashOrchard::from_bytes(&pallas::Base::random(rng).to_repr()).expect("canonical")
}

pub fn sapling_leaf(rng: &mut StdRng) -> Node {
    let mut bytes = [0u8; 32];
    rng.fill_bytes(&mut bytes);
    bytes[31] &= 0x3f;
    Option::from(Node::from_bytes(bytes)).expect("canonical")
}

pub fn frontier_at<H: Clone>(position: u64, leaf: &mut impl FnMut() -> H) -> Frontier<H, DEPTH> {
    let ommers = (0..position.count_ones()).map(|_| leaf()).collect();
    Frontier::from_parts(Position::from(position), leaf(), ommers).expect("consistent parts")
}

fn zakura_node(node: &MerkleHashOrchard) -> zk_chain::orchard::tree::Node {
    zk_chain::orchard::tree::Node::try_from(node.to_bytes()).expect("canonical")
}

fn zakura_base(node: &MerkleHashOrchard) -> zk_pasta::pallas::Base {
    use zk_pasta::group::ff::PrimeField as _;
    zk_pasta::pallas::Base::from_repr(node.to_bytes()).expect("canonical")
}

/// The Orchard start frontier and `max_leaves` leaves, in both type systems.
pub struct OrchardInputs {
    pub start: Frontier<MerkleHashOrchard, DEPTH>,
    pub leaves: Vec<MerkleHashOrchard>,
    pub zakura_start: Frontier<zk_chain::orchard::tree::Node, DEPTH>,
    pub zakura_leaves: Vec<zk_pasta::pallas::Base>,
}

impl OrchardInputs {
    pub fn new(max_leaves: usize) -> Self {
        let mut rng = StdRng::seed_from_u64(0x7ee5);
        let mut leaf = || orchard_leaf(&mut rng);
        let start = frontier_at(START_POSITION, &mut leaf);
        let leaves: Vec<MerkleHashOrchard> = (0..max_leaves).map(|_| leaf()).collect();

        let (position, tip, ommers) = start.clone().take().expect("non-empty").into_parts();
        let zakura_start = Frontier::<zk_chain::orchard::tree::Node, DEPTH>::from_parts(
            position,
            zakura_node(&tip),
            ommers.iter().map(zakura_node).collect(),
        )
        .expect("consistent parts");
        let zakura_leaves = leaves.iter().map(zakura_base).collect();
        OrchardInputs {
            start,
            leaves,
            zakura_start,
            zakura_leaves,
        }
    }
}

/// The hayai body: a fresh frontier from `start`, one batched append, the root.
pub fn hayai_append(
    start: &Frontier<MerkleHashOrchard, DEPTH>,
    leaves: &[MerkleHashOrchard],
) -> orchard::tree::Anchor {
    let mut frontier = OrchardFrontier::from_frontier(start.clone());
    frontier.append_many(leaves).expect("fits")
}

/// The zakura-chain body: `NoteCommitmentTree::append_batch` then `root()`.
pub fn zakura_append(
    start: &Frontier<zk_chain::orchard::tree::Node, DEPTH>,
    leaves: &[zk_pasta::pallas::Base],
) -> zk_chain::orchard::tree::Root {
    let mut tree = zk_chain::orchard::tree::NoteCommitmentTree::from_frontier(start.clone());
    tree.append_batch(leaves).expect("fits");
    tree.root()
}

/// `tree_append_<n>`.
pub fn build_append(n: usize, imp: Impl) -> Built {
    let inputs = OrchardInputs::new(n);
    match imp {
        Impl::Hayai => Built::new(move |m| {
            let root = m.timed(|| hayai_append(&inputs.start, &inputs.leaves));
            std::hint::black_box(root);
        }),
        Impl::Zebra => unreachable!("{}", super::NO_ZEBRA),
        Impl::Zakura => Built::new(move |m| {
            let root = m.timed(|| zakura_append(&inputs.zakura_start, &inputs.zakura_leaves));
            std::hint::black_box(root);
        }),
    }
}
