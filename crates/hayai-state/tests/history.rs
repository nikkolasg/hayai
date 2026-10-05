//! ZIP 221 history tree against independent vectors.
//!
//! - `zip_0221_v1.rs`, `zip_0221_v2.rs`, `zip_0221_v3.rs`: the zcash-test-vectors chain
//!   history vectors
//!   (`zcash_test_vectors/zip_0221.py`, an independent implementation of the
//!   specification), copied from the upstream `zcash_history` 0.5 crate. For each tree size
//!   they give the appended leaf's inputs, the work of its `nBits`, the serialized peaks and
//!   `hashChainHistoryRoot`.
//! - Mainnet blocks from Zebra's test vectors (`zebra-test/src/vectors`): the Heartwood
//!   activation block 903,000 with the header of block 903,001, and the Canopy activation
//!   block 1,046,400 with the header of block 1,046,401. The final Sapling roots are
//!   Zebra's `SAPLING_FINAL_ROOT_MAINNET_*` values. Each activation block starts a new tree,
//!   so the header of the next block commits to a tree of one leaf, which a node can compute
//!   without earlier peaks.

use bytes::Bytes;
use hayai_crypto::primitive_types::U256;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_state::history::{block_work, header_commitment, history_after, HeaderCommitment};
use hayai_state::{Anchors, HistoryError, HistoryLeaf, HistoryState};
use hayai_wire::header::BlockHeader;
use hayai_wire::RawBlock;

#[allow(dead_code)]
#[path = "vectors/zip_0221_v1.rs"]
mod zip_0221_v1;
#[allow(dead_code)]
#[path = "vectors/zip_0221_v2.rs"]
mod zip_0221_v2;
#[allow(dead_code)]
#[path = "vectors/zip_0221_v3.rs"]
mod zip_0221_v3;

/// Appends one vector leaf and checks the work, the peaks and the root.
#[allow(clippy::too_many_arguments)]
fn check_step(
    state: &HistoryState,
    branch_id: u32,
    leaf: HistoryLeaf,
    work: [u8; 32],
    peaks: &[&[u8]],
    root: [u8; 32],
) -> HistoryState {
    assert_eq!(
        block_work(leaf.bits).unwrap(),
        U256::from_little_endian(&work),
        "work of bits {:#x}",
        leaf.bits
    );
    let branch = BranchId::try_from(branch_id).unwrap();
    let next = state.append(branch, &leaf).unwrap();
    let got: Vec<&[u8]> = next.peaks().iter().map(|(_, b)| b.as_slice()).collect();
    assert_eq!(got, peaks, "peaks after height {}", leaf.height);
    assert_eq!(next.root(), root, "root after height {}", leaf.height);
    assert_eq!(next.last_height(), Some(u64::from(leaf.height)));
    // The saved peaks rebuild the same state.
    let restored = HistoryState::from_peaks(branch, next.length(), next.peaks().to_vec()).unwrap();
    assert_eq!(restored, next);
    next
}

#[test]
fn zip_0221_v1_vectors() {
    let first = BranchId::try_from(zip_0221_v1::TEST_VECTORS[0].consensus_branch_id).unwrap();
    assert_eq!(first, BranchId::Heartwood);
    // The Heartwood activation block starts from the empty Blossom state.
    let mut state = HistoryState::empty(BranchId::Blossom);
    for tv in zip_0221_v1::TEST_VECTORS {
        let leaf = HistoryLeaf {
            hash: tv.leaf_block_hash,
            time: tv.leaf_time,
            bits: tv.leaf_target_bits,
            height: u32::try_from(tv.leaf_height).unwrap(),
            sapling_root: tv.leaf_sapling_root,
            orchard_root: [0; 32],
            sapling_tx: tv.leaf_sapling_tx_count,
            orchard_tx: 0,
            ironwood_root: [0; 32],
            ironwood_tx: 0,
        };
        state = check_step(
            &state,
            tv.consensus_branch_id,
            leaf,
            tv.leaf_work,
            tv.peaks,
            tv.hash_chain_history_root,
        );
        assert_eq!(state.length(), 2 * tv.n_leaves - tv.n_leaves.count_ones());
    }
}

#[test]
fn zip_0221_v2_vectors() {
    let first = BranchId::try_from(zip_0221_v2::TEST_VECTORS[0].consensus_branch_id).unwrap();
    assert_eq!(first, BranchId::Nu5);
    let mut state = HistoryState::empty(BranchId::Canopy);
    for tv in zip_0221_v2::TEST_VECTORS {
        let leaf = HistoryLeaf {
            hash: tv.leaf_block_hash,
            time: tv.leaf_time,
            bits: tv.leaf_target_bits,
            height: u32::try_from(tv.leaf_height).unwrap(),
            sapling_root: tv.leaf_sapling_root,
            orchard_root: tv.leaf_orchard_root,
            sapling_tx: tv.leaf_sapling_tx_count,
            orchard_tx: tv.leaf_orchard_tx_count,
            ironwood_root: [0; 32],
            ironwood_tx: 0,
        };
        state = check_step(
            &state,
            tv.consensus_branch_id,
            leaf,
            tv.leaf_work,
            tv.peaks,
            tv.hash_chain_history_root,
        );
    }
}

/// Tree version 3 (NU6.3): a leaf holds the Ironwood root and the count of transactions
/// with Ironwood actions. The first leaf starts a new tree on the NU6.2 tree.
#[test]
fn zip_0221_v3_vectors() {
    let first = BranchId::try_from(zip_0221_v3::TEST_VECTORS[0].consensus_branch_id).unwrap();
    assert_eq!(first, BranchId::Nu6_3);
    assert_eq!(zip_0221_v3::TEST_VECTORS.len(), 16);
    // A NU6.2 tree with one leaf: the NU6.3 activation block starts a new tree.
    let mut state = HistoryState::empty(BranchId::Nu6_1)
        .append(
            BranchId::Nu6_2,
            &HistoryLeaf {
                hash: [7; 32],
                time: 1,
                bits: 0x1f07_ffff,
                height: u32::try_from(zip_0221_v3::TEST_VECTORS[0].leaf_height).unwrap() - 1,
                sapling_root: [3; 32],
                orchard_root: [4; 32],
                ironwood_root: [0; 32],
                sapling_tx: 2,
                orchard_tx: 1,
                ironwood_tx: 0,
            },
        )
        .unwrap();
    for tv in zip_0221_v3::TEST_VECTORS {
        let leaf = HistoryLeaf {
            hash: tv.leaf_block_hash,
            time: tv.leaf_time,
            bits: tv.leaf_target_bits,
            height: u32::try_from(tv.leaf_height).unwrap(),
            sapling_root: tv.leaf_sapling_root,
            orchard_root: tv.leaf_orchard_root,
            ironwood_root: tv.leaf_ironwood_root,
            sapling_tx: tv.leaf_sapling_tx_count,
            orchard_tx: tv.leaf_orchard_tx_count,
            ironwood_tx: tv.leaf_ironwood_tx_count,
        };
        if tv.n_leaves == 1 {
            // The serialized leaf of the vector is the only peak of a tree of one leaf.
            assert_eq!(tv.peaks, [tv.leaf_serialized]);
        }
        state = check_step(
            &state,
            tv.consensus_branch_id,
            leaf,
            tv.leaf_work,
            tv.peaks,
            tv.hash_chain_history_root,
        );
        assert_eq!(state.upgrade(), BranchId::Nu6_3);
        assert_eq!(state.length(), 2 * tv.n_leaves - tv.n_leaves.count_ones());
        // The Ironwood fields are in the root: another Ironwood root or count gives
        // another tree.
        for other in [
            HistoryLeaf {
                ironwood_root: [0xee; 32],
                ..leaf
            },
            HistoryLeaf {
                ironwood_tx: leaf.ironwood_tx + 1,
                ..leaf
            },
        ] {
            let parent = HistoryState::empty(BranchId::Nu6_2);
            assert_ne!(
                parent.append(BranchId::Nu6_3, &other).unwrap().root(),
                parent.append(BranchId::Nu6_3, &leaf).unwrap().root()
            );
        }
    }
}

fn hex_bytes(text: &str) -> Vec<u8> {
    hex::decode(text.trim()).unwrap()
}

/// A root in display order (as `zcash-cli` prints it), in byte order.
fn display_root(text: &str) -> [u8; 32] {
    let mut root: [u8; 32] = hex_bytes(text).try_into().unwrap();
    root.reverse();
    root
}

fn leaf_of(block: &RawBlock, height: u32, sapling_root: [u8; 32]) -> HistoryLeaf {
    HistoryLeaf::from_block(
        block,
        height,
        &Anchors {
            sapling: sapling_root,
            orchard: [0; 32],
            ironwood: [0; 32],
        },
    )
}

#[test]
fn mainnet_heartwood_activation() {
    let block = RawBlock::parse(
        Bytes::from(hex_bytes(include_str!("vectors/block-main-0-903-000.hex"))),
        BranchId::Heartwood,
    )
    .unwrap();
    let next = BlockHeader::parse(&hex_bytes(include_str!(
        "vectors/header-main-0-903-001.hex"
    )))
    .unwrap();
    assert_eq!(next.prev_hash, block.hash());
    let sapling_root =
        display_root("11e48300f0e2296d5c413340b26426eddada1155155f4e959ebe307396976c79");

    // Before Heartwood the tree is empty. The activation block's field is all zeros.
    let blossom = HistoryState::empty(BranchId::Blossom);
    assert_eq!(block.header.block_commitments, [0; 32]);
    assert_eq!(
        header_commitment(BranchId::Heartwood, Some(&blossom), &sapling_root, &[9; 32]),
        HeaderCommitment::Expected([0; 32])
    );

    // The tree after the activation block has one leaf. Block 903,001 commits to its root.
    let after = history_after(
        Some(&blossom),
        BranchId::Heartwood,
        &leaf_of(&block, 903_000, sapling_root),
    )
    .unwrap()
    .unwrap();
    assert_eq!(after.length(), 1);
    assert_eq!(after.upgrade(), BranchId::Heartwood);
    assert_eq!(
        header_commitment(BranchId::Heartwood, Some(&after), &[0; 32], &[0; 32]),
        HeaderCommitment::Expected(next.block_commitments)
    );
    // A wrong Sapling root gives another root.
    let wrong = blossom
        .append(BranchId::Heartwood, &leaf_of(&block, 903_000, [1; 32]))
        .unwrap();
    assert_ne!(wrong.root(), next.block_commitments);
    // Before Heartwood the field is the block's own final Sapling root, and no parent
    // state is necessary.
    assert_eq!(
        header_commitment(BranchId::Blossom, None, &sapling_root, &[0; 32]),
        HeaderCommitment::Expected(sapling_root)
    );
    assert_eq!(
        history_after(
            None,
            BranchId::Blossom,
            &leaf_of(&block, 902_999, sapling_root)
        ),
        Ok(Some(HistoryState::empty(BranchId::Blossom)))
    );
}

#[test]
fn mainnet_canopy_activation_resets_the_tree() {
    let block = RawBlock::parse(
        Bytes::from(hex_bytes(include_str!("vectors/block-main-1-046-400.hex"))),
        BranchId::Canopy,
    )
    .unwrap();
    let next = BlockHeader::parse(&hex_bytes(include_str!(
        "vectors/header-main-1-046-401.hex"
    )))
    .unwrap();
    assert_eq!(next.prev_hash, block.hash());
    let sapling_root =
        display_root("0f12c92f737e84142792bddc82e36481de4a7679d5778a27389793933c8742e1");

    // Any Heartwood tree: the Canopy block starts a new tree, so the old peaks do not count.
    let heartwood = HistoryState::empty(BranchId::Blossom)
        .append(
            BranchId::Heartwood,
            &HistoryLeaf {
                hash: [7; 32],
                time: 1,
                bits: 0x1f07_ffff,
                height: 1_046_399,
                sapling_root: [3; 32],
                orchard_root: [0; 32],
                sapling_tx: 2,
                orchard_tx: 0,
                ironwood_root: [0; 32],
                ironwood_tx: 0,
            },
        )
        .unwrap();
    // The activation block commits to the tree of the previous upgrade.
    assert_eq!(
        header_commitment(BranchId::Canopy, Some(&heartwood), &[0; 32], &[0; 32]),
        HeaderCommitment::Expected(heartwood.root())
    );
    let after = heartwood
        .append(BranchId::Canopy, &leaf_of(&block, 1_046_400, sapling_root))
        .unwrap();
    assert_eq!(after.length(), 1);
    assert_eq!(after.upgrade(), BranchId::Canopy);
    assert_eq!(after.root(), next.block_commitments);
}

#[test]
fn nu5_and_later_commit_to_the_root_and_the_auth_data_root() {
    let state = HistoryState::empty(BranchId::Canopy)
        .append(
            BranchId::Nu5,
            &HistoryLeaf {
                hash: [7; 32],
                time: 1,
                bits: 0x1f07_ffff,
                height: 1_687_104,
                sapling_root: [3; 32],
                orchard_root: [4; 32],
                sapling_tx: 2,
                orchard_tx: 1,
                ironwood_root: [0; 32],
                ironwood_tx: 0,
            },
        )
        .unwrap();
    let auth = [5; 32];
    for branch in [BranchId::Nu5, BranchId::Nu6_2, BranchId::Nu6_3] {
        assert_eq!(
            header_commitment(branch, Some(&state), &[0; 32], &auth),
            HeaderCommitment::Expected(hayai_wire::block_commitments(&state.root(), &auth))
        );
    }
    assert_eq!(
        header_commitment(BranchId::Nu5, None, &[0; 32], &auth),
        HeaderCommitment::ParentUnknown
    );
    assert_eq!(
        header_commitment(BranchId::Overwinter, None, &[0; 32], &auth),
        HeaderCommitment::Reserved
    );
}

#[test]
fn appends_check_heights_and_start_a_new_tree_per_upgrade() {
    let leaf = |height: u32| HistoryLeaf {
        hash: [height as u8; 32],
        time: height,
        bits: 0x1f07_ffff,
        height,
        sapling_root: [3; 32],
        orchard_root: [4; 32],
        sapling_tx: 0,
        orchard_tx: 0,
        ironwood_root: [0; 32],
        ironwood_tx: 0,
    };
    let one = HistoryState::empty(BranchId::Nu6_1)
        .append(BranchId::Nu6_2, &leaf(10))
        .unwrap();
    let Err(HistoryError::Height {
        last: 10,
        found: 12,
    }) = one.append(BranchId::Nu6_2, &leaf(12))
    else {
        panic!("a gap in heights is an error");
    };
    let two = one.append(BranchId::Nu6_2, &leaf(11)).unwrap();
    assert_eq!(two.length(), 3);
    // The NU6.3 activation block starts a tree of version 3 with one leaf.
    let nu6_3 = two.append(BranchId::Nu6_3, &leaf(12)).unwrap();
    assert_eq!(nu6_3.length(), 1);
    assert_eq!(nu6_3.upgrade(), BranchId::Nu6_3);
    assert_eq!(nu6_3.last_height(), Some(12));
    // A leaf of version 3 is longer than a leaf of version 2 by the Ironwood root (start
    // and end) and the count.
    assert_eq!(
        nu6_3.peaks()[0].1.len(),
        one.peaks()[0].1.len() + 32 + 32 + 1
    );
    // The saved peaks rebuild the tree, and the tree continues.
    let restored =
        HistoryState::from_peaks(BranchId::Nu6_3, nu6_3.length(), nu6_3.peaks().to_vec()).unwrap();
    assert_eq!(restored, nu6_3);
    assert_eq!(
        restored
            .append(BranchId::Nu6_3, &leaf(13))
            .unwrap()
            .length(),
        3
    );
    // NU7 keeps the tree version of NU6.3 and starts a new tree, when the backend has the
    // NU7 branch id.
    if let Some(nu7) = hayai_crypto::nu7_branch() {
        let after = nu6_3.append(nu7, &leaf(13)).expect("NU7 has a rule set");
        assert_eq!((after.upgrade(), after.length()), (nu7, 1));
    }
    // Unknown parent: no state after a block at or after Heartwood.
    assert_eq!(history_after(None, BranchId::Nu6_2, &leaf(12)), Ok(None));
}

#[test]
fn from_peaks_rejects_inconsistent_peaks() {
    let leaf = |height: u32| HistoryLeaf {
        hash: [height as u8; 32],
        time: height,
        bits: 0x1f07_ffff,
        height,
        sapling_root: [3; 32],
        orchard_root: [4; 32],
        sapling_tx: 0,
        orchard_tx: 0,
        ironwood_root: [0; 32],
        ironwood_tx: 0,
    };
    let mut state = HistoryState::empty(BranchId::Nu6_1);
    for h in 100..103 {
        state = state.append(BranchId::Nu6_2, &leaf(h)).unwrap();
    }
    // Three leaves: four nodes, peaks at 2 and 3.
    assert_eq!(state.length(), 4);
    let positions: Vec<u32> = state.peaks().iter().map(|(p, _)| *p).collect();
    assert_eq!(positions, vec![2, 3]);
    let peaks = state.peaks().to_vec();
    let Err(HistoryError::Peaks { length: 5 }) =
        HistoryState::from_peaks(BranchId::Nu6_2, 5, peaks.clone())
    else {
        panic!("wrong length");
    };
    let swapped = vec![(2, peaks[1].1.clone()), (3, peaks[0].1.clone())];
    let Err(HistoryError::Peaks { length: 4 }) =
        HistoryState::from_peaks(BranchId::Nu6_2, 4, swapped)
    else {
        panic!("peaks out of order");
    };
    let mut trailing = peaks.clone();
    trailing[1].1.push(0);
    let Err(HistoryError::Encoding { position: 3 }) =
        HistoryState::from_peaks(BranchId::Nu6_2, 4, trailing)
    else {
        panic!("a node with trailing bytes");
    };
    let Err(HistoryError::PreHeartwood(BranchId::Blossom)) =
        HistoryState::from_peaks(BranchId::Blossom, 4, peaks.clone())
    else {
        panic!("no tree before Heartwood");
    };
    // The restored state appends exactly as the original.
    let restored = HistoryState::from_peaks(BranchId::Nu6_2, 4, peaks).unwrap();
    assert_eq!(
        restored.append(BranchId::Nu6_2, &leaf(103)).unwrap(),
        state.append(BranchId::Nu6_2, &leaf(103)).unwrap()
    );
}
