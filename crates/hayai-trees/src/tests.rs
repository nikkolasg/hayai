use hayai_crypto::rng::{RngCore, SeedableRng, StdRng};
use hayai_crypto::{ff, incrementalmerkletree, orchard, pasta_curves, sapling_crypto};

use ff::Field;
use incrementalmerkletree::{frontier::Frontier, Position};
use orchard::tree::MerkleHashOrchard;
use pasta_curves::pallas;
use proptest::prelude::*;
use sapling_crypto::Node;

use crate::{OrchardFrontier, SaplingFrontier, TreeError, DEPTH};

fn orchard_leaf(rng: &mut StdRng) -> MerkleHashOrchard {
    crate::orchard_node(pallas::Base::random(rng))
}

fn sapling_leaf(rng: &mut StdRng) -> Node {
    // A canonical jubjub base element. The modulus starts with 0x73, so a clear of the top two
    // bits of the big end keeps the value in range.
    let mut bytes = [0u8; 32];
    rng.fill_bytes(&mut bytes);
    bytes[31] &= 0x3f;
    Option::from(Node::from_bytes(bytes)).expect("canonical bytes")
}

/// An upstream frontier at `position` with random contents. `None` is the empty tree.
fn frontier_at<H: Clone>(
    position: Option<u64>,
    leaf: &mut impl FnMut() -> H,
) -> Frontier<H, DEPTH> {
    let Some(position) = position else {
        return Frontier::empty();
    };
    let ommers = (0..position.count_ones()).map(|_| leaf()).collect();
    Frontier::from_parts(Position::from(position), leaf(), ommers).expect("consistent parts")
}

fn upstream_append<H: incrementalmerkletree::Hashable + Clone>(
    mut frontier: Frontier<H, DEPTH>,
    leaves: &[H],
) -> Frontier<H, DEPTH> {
    for leaf in leaves {
        assert!(frontier.append(leaf.clone()), "test trees never overflow");
    }
    frontier
}

const START_POSITIONS: [Option<u64>; 10] = [
    None,
    Some(0),
    Some(5),
    Some(63),
    Some(64),
    Some(1023),
    Some(65_535),
    Some(65_536),
    Some((1 << 20) - 7),
    Some(123_456_789),
];

fn check_orchard(rng: &mut StdRng, start: Option<u64>, count: usize) {
    let mut leaf = || orchard_leaf(rng);
    let frontier = frontier_at(start, &mut leaf);
    let leaves: Vec<MerkleHashOrchard> = (0..count).map(|_| leaf()).collect();

    let expected = upstream_append(frontier.clone(), &leaves);
    let mut hayai = OrchardFrontier::from_frontier(frontier);
    let anchor = hayai.append_many(&leaves).expect("fits");

    assert_eq!(hayai.frontier(), &expected, "start {start:?} count {count}");
    assert_eq!(anchor, orchard::tree::Anchor::from(expected.root()));
    assert_eq!(hayai.root(), anchor);
}

fn check_sapling(rng: &mut StdRng, start: Option<u64>, count: usize) {
    let mut leaf = || sapling_leaf(rng);
    let frontier = frontier_at(start, &mut leaf);
    let leaves: Vec<Node> = (0..count).map(|_| leaf()).collect();

    let expected = upstream_append(frontier.clone(), &leaves);
    let mut hayai = SaplingFrontier::from_frontier(frontier);
    let anchor = hayai.append_many(&leaves).expect("fits");

    assert_eq!(hayai.frontier(), &expected, "start {start:?} count {count}");
    assert_eq!(anchor, sapling_crypto::Anchor::from(expected.root()));
    assert_eq!(hayai.root(), anchor);
}

#[test]
fn orchard_matches_upstream_from_every_start_position() {
    let mut rng = StdRng::seed_from_u64(0x0c4a);
    for start in START_POSITIONS {
        for count in [0usize, 1, 2, 3, 7, 64, 65] {
            check_orchard(&mut rng, start, count);
        }
    }
}

#[test]
fn orchard_matches_upstream_across_subtree_boundaries() {
    let mut rng = StdRng::seed_from_u64(0xb0b0);
    for (start, count) in [
        (Some(1021), 10),
        (Some(65_533), 5),
        (Some(65_535), 330),
        (Some((1 << 20) - 100), 2048),
        (None, 1024),
        (None, 1025),
        (Some(0), 4999),
    ] {
        check_orchard(&mut rng, start, count);
    }
}

#[test]
fn sapling_matches_upstream() {
    let mut rng = StdRng::seed_from_u64(0x5a9);
    for (start, count) in [
        (None, 1),
        (None, 7),
        (Some(0), 1),
        (Some(63), 2),
        (Some(1023), 64),
        (Some(65_535), 33),
        (Some(123_456_789), 200),
    ] {
        check_sapling(&mut rng, start, count);
    }
}

#[test]
fn empty_frontier_root_is_the_empty_tree_anchor() {
    assert_eq!(
        OrchardFrontier::empty().root(),
        orchard::tree::Anchor::empty_tree()
    );
    assert_eq!(
        SaplingFrontier::empty().root(),
        sapling_crypto::Anchor::empty_tree()
    );
}

#[test]
fn full_tree_is_an_error_not_a_panic() {
    let mut rng = StdRng::seed_from_u64(9);
    let mut leaf = || orchard_leaf(&mut rng);
    let capacity = 1u64 << DEPTH;
    let mut hayai = OrchardFrontier::from_frontier(frontier_at(Some(capacity - 2), &mut leaf));
    let two = [leaf(), leaf()];
    assert!(matches!(
        hayai.append_many(&two),
        Err(TreeError::Full { requested, .. }) if requested == capacity + 1
    ));
    let last = [two[0]];
    let expected = upstream_append(hayai.frontier().clone(), &last);
    hayai.append_many(&last).expect("last leaf fits");
    assert_eq!(hayai.frontier(), &expected);
    assert!(matches!(
        hayai.append_many(&last),
        Err(TreeError::Full { .. })
    ));
}

/// Spec §3.8: the Sapling tree takes 2^32 leaves and not one more.
#[test]
fn a_full_sapling_tree_is_an_error() {
    let mut rng = StdRng::seed_from_u64(10);
    let mut leaf = || sapling_leaf(&mut rng);
    let capacity = 1u64 << DEPTH;
    let mut hayai = SaplingFrontier::from_frontier(frontier_at(Some(capacity - 2), &mut leaf));
    let two = [leaf(), leaf()];
    let before = hayai.clone();
    let Err(TreeError::Full { requested, .. }) = hayai.append_many(&two) else {
        panic!("two leaves do not fit");
    };
    assert_eq!(requested, capacity + 1);
    assert_eq!(hayai, before);
    hayai.append_many(&two[..1]).expect("the last leaf fits");
    let Err(TreeError::Full { .. }) = hayai.append_many(&two[1..]) else {
        panic!("a full tree takes no leaf");
    };
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(6))]
    #[test]
    fn prop_orchard_random_counts(
        start in prop::sample::select(START_POSITIONS.to_vec()),
        count in 1usize..5000,
        seed in any::<u64>(),
    ) {
        let mut rng = StdRng::seed_from_u64(seed);
        let count = if rng.next_u32() % 2 == 0 { count } else { count % 300 + 1 };
        check_orchard(&mut rng, start, count);
    }
}

#[test]
fn every_block_level_matches_upstream() {
    let mut rng = StdRng::seed_from_u64(0xb10c);
    for block in 0..=7u32 {
        for (start, count) in [(None, 100), (Some(65_533), 300), (Some(1021), 9)] {
            let mut leaf = || orchard_leaf(&mut rng);
            let frontier = frontier_at(start, &mut leaf);
            let leaves: Vec<MerkleHashOrchard> = (0..count).map(|_| leaf()).collect();
            let expected = upstream_append(frontier.clone(), &leaves);
            let got = crate::batch::append_many_with_block(
                &frontier,
                &leaves,
                &crate::OrchardCombine,
                block,
            )
            .expect("fits");
            assert_eq!(got, expected, "block {block} start {start:?} count {count}");
        }
    }
}
