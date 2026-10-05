//! hayai-validate end to end: fixtures validate cold and warm to identical layers, tampered
//! blocks fail at the right stage, timings are populated.

use bytes::Bytes;
use hayai_bench::chain_fixture::{chain_with_layers, harness, harness_with_history};
use hayai_bench::fixtures::{mixed_block, orchard_block, transparent_block, Fixture};
use hayai_bench::zakura_chain_clone::BlockShape;
use hayai_coins::{CoinsView, Pool};
use hayai_consensus::BlockLimits;
use hayai_prepared::PrepareError;
use hayai_state::{ContextError, Layer};
use hayai_validate::{block_commitments, validate_block, validate_bytes, BlockError};
use hayai_wire::{auth_data_root, RawBlock};

fn same_layer(a: &Layer, b: &Layer) {
    assert_eq!(a.height, b.height);
    assert_eq!(a.hash, b.hash);
    assert_eq!(a.parent, b.parent);
    assert_eq!(a.wtxids, b.wtxids);
    assert_eq!(a.created, b.created);
    assert_eq!(a.spent, b.spent);
    for pool in Pool::ALL {
        assert_eq!(a.nullifiers[pool.index()], b.nullifiers[pool.index()]);
    }
    assert_eq!(a.anchors, b.anchors);
    assert_eq!(a.value_pools, b.value_pools);
    assert_eq!(a.orchard_frontier, b.orchard_frontier);
    assert_eq!(a.sapling_frontier, b.sapling_frontier);
}

fn fixtures() -> Vec<Fixture> {
    vec![
        transparent_block(5, 2),
        orchard_block(2, 2),
        mixed_block(3, 1, 2, 2),
        transparent_block(1000, 2),
    ]
}

#[test]
fn cold_and_warm_validation_agree() {
    for fixture in fixtures() {
        let cold = harness(&fixture);
        let (cold_layer, cold_t) = validate_bytes(
            fixture.bytes.clone(),
            &cold.store,
            &cold.chain.view(),
            &cold.cfg,
        )
        .unwrap_or_else(|e| panic!("{} cold: {e}", fixture.name));
        assert_eq!(cold_t.known, 0);
        assert_eq!(cold_t.unknown, cold.block.txs.len());
        assert!(cold_t.parse > std::time::Duration::ZERO);
        // Scripts, the shielded batch and the context run concurrently, so the total bounds
        // each of them, not their sum.
        assert!(cold_t.total >= cold_t.scripts.max(cold_t.shielded).max(cold_t.context));

        let warm = harness(&fixture);
        warm.fill_store();
        let (warm_layer, warm_t) = validate_block(
            warm.block.clone(),
            &warm.store,
            &warm.chain.view(),
            &warm.cfg,
        )
        .unwrap_or_else(|e| panic!("{} warm: {e}", fixture.name));
        assert_eq!(warm_t.known, warm.block.txs.len() - 1);
        assert_eq!(warm_t.unknown, 1, "only the coinbase is prepared");
        assert_eq!(warm_t.parse, std::time::Duration::ZERO);
        assert!(warm_t.scripts <= cold_t.scripts);
        same_layer(&cold_layer, &warm_layer);

        // The layer commits and the mined transactions leave the store.
        let mut chain = warm.chain;
        let ids: Vec<_> = warm_layer.wtxids[1..].to_vec();
        chain.push(warm_layer).unwrap();
        assert_eq!(warm.store.remove_mined(&ids), ids.len());
        assert!(warm.store.is_empty());
    }
}

#[test]
fn tampered_blocks_fail_at_the_right_stage() {
    let fixture = mixed_block(3, 1, 2, 2);
    let h = harness(&fixture);
    let view = h.chain.view();

    // Merkle root.
    let mut bytes = fixture.bytes.to_vec();
    bytes[36] ^= 0x01;
    let Err(BlockError::MerkleRoot) = validate_bytes(Bytes::from(bytes), &h.store, &view, &h.cfg)
    else {
        panic!("merkle mismatch");
    };

    // A transparent signature: the first transaction's first input.
    let mut bytes = fixture.bytes.to_vec();
    let script_sig = &h.block.txs[1].tx.transparent_bundle().unwrap().vin[0]
        .script_sig()
        .0
         .0;
    let at = bytes
        .windows(script_sig.len())
        .position(|w| w == &script_sig[..])
        .unwrap()
        + 5;
    bytes[at] ^= 0x01;
    let mut block = RawBlock::parse(Bytes::from(bytes), fixture.branch_id).unwrap();
    block.header.merkle_root = hayai_wire::merkle_root(&block.txids());
    let Err(BlockError::Prepare {
        tx: 1,
        error: PrepareError::Script(0, _),
    }) = validate_block(block, &h.store, &view, &h.cfg)
    else {
        panic!("bad signature");
    };

    // An Orchard proof byte in the last transaction.
    let mut bytes = fixture.bytes.to_vec();
    let last = &h.block.txs[5];
    let start = bytes.len() - last.bytes.len();
    bytes[start + last.bytes.len() - 200] ^= 0x01;
    let mut block = RawBlock::parse(Bytes::from(bytes), fixture.branch_id).unwrap();
    block.header.merkle_root = hayai_wire::merkle_root(&block.txids());
    let bad = block.txs[5].wtxid();
    let Err(BlockError::Shielded(failed)) = validate_block(block, &h.store, &view, &h.cfg) else {
        panic!("bad proof");
    };
    assert_eq!(failed, vec![bad]);

    // hashBlockCommitments, when the parent's history tree is known.
    let known = harness_with_history(&fixture);
    let known_view = known.chain.view();
    let mut block = known.block.clone();
    block.header.block_commitments = [0x11; 32];
    let Err(BlockError::Context(ContextError::BlockCommitments)) =
        validate_block(block, &known.store, &known_view, &known.cfg)
    else {
        panic!("wrong commitments");
    };
    // The auth data root of another block does not match either.
    let mut block = known.block.clone();
    block.header.block_commitments = block_commitments(
        &known_view.history().unwrap().root(),
        &auth_data_root(&h.block.auth_digests()[1..]),
    );
    let Err(BlockError::Context(ContextError::BlockCommitments)) =
        validate_block(block, &known.store, &known_view, &known.cfg)
    else {
        panic!("commitments to another auth data root");
    };

    // A missing input is caught before any script runs.
    let other = harness(&orchard_block(2, 2));
    let Err(BlockError::Context(ContextError::MissingInput { tx: 1, input: 0 })) =
        validate_block(h.block.clone(), &other.store, &other.chain.view(), &h.cfg)
    else {
        panic!("inputs unknown to the view");
    };
}

/// CVE-2012-2459 on the full path: the body `[coinbase, a, b, b]` has the merkle root and
/// the hash of the block `[coinbase, a, b]`. The error is `DuplicateTxid`, which names a
/// fault of the body. The block with the same header and the true body is valid.
#[test]
fn a_body_with_a_txid_twice_is_a_duplicate_txid_on_the_full_path() {
    let fixture = transparent_block(2, 1);
    for h in [harness(&fixture), harness_with_history(&fixture)] {
        let view = h.chain.view();
        assert_eq!(h.block.txs.len(), 3);
        let mut mutated = h.block.clone();
        mutated.txs.push(mutated.txs[2].clone());
        assert_eq!(
            hayai_wire::merkle_root(&mutated.txids()),
            h.block.header.merkle_root
        );
        assert_eq!(mutated.hash(), h.block.hash());
        let twice = mutated.txs[2].txid;
        let Err(BlockError::Context(ContextError::DuplicateTxid(txid))) =
            validate_block(mutated, &h.store, &view, &h.cfg)
        else {
            panic!("a body with a txid twice");
        };
        assert_eq!(txid, twice);

        // A txid twice with the merkle root of that list: the same error, before a state
        // rule reads the body.
        let mut repeated = h.block.clone();
        repeated.txs.push(repeated.txs[1].clone());
        repeated.header.merkle_root = hayai_wire::merkle_root(&repeated.txids());
        let twice = repeated.txs[1].txid;
        let Err(BlockError::Context(ContextError::DuplicateTxid(txid))) =
            validate_block(repeated, &h.store, &view, &h.cfg)
        else {
            panic!("a txid twice, not in a pair");
        };
        assert_eq!(txid, twice);

        validate_block(h.block.clone(), &h.store, &view, &h.cfg).expect("the true body");
    }
}

/// A block that breaks a script, a proof and a contextual rule at once fails with the
/// script error: the three stages run concurrently and the verdict is the first error in
/// stage order, whichever task finishes first.
#[test]
fn concurrent_stages_report_the_first_error_in_stage_order() {
    let fixture = mixed_block(3, 1, 2, 2);
    let h = harness(&fixture);
    let view = h.chain.view();
    let mut bytes = fixture.bytes.to_vec();
    let script_sig = &h.block.txs[1].tx.transparent_bundle().unwrap().vin[0]
        .script_sig()
        .0
         .0;
    let at = bytes
        .windows(script_sig.len())
        .position(|w| w == &script_sig[..])
        .unwrap()
        + 5;
    bytes[at] ^= 0x01;
    let last = &h.block.txs[5];
    let start = bytes.len() - last.bytes.len();
    bytes[start + last.bytes.len() - 200] ^= 0x01;
    // The contextual rule: a block limit the Orchard bundles exceed.
    let mut cfg = h.cfg.clone();
    cfg.rules.limits = BlockLimits {
        orchard_actions: 1,
        ..BlockLimits::PRE_NU7
    };
    for _ in 0..5 {
        let mut block = RawBlock::parse(Bytes::from(bytes.clone()), fixture.branch_id).unwrap();
        block.header.merkle_root = hayai_wire::merkle_root(&block.txids());
        let Err(BlockError::Prepare {
            tx: 1,
            error: PrepareError::Script(0, _),
        }) = validate_block(block, &h.store, &view, &cfg)
        else {
            panic!("the script error comes first");
        };
    }
    // Without the script fault the shielded verdict wins over the contextual one.
    let mut bytes = fixture.bytes.to_vec();
    bytes[start + last.bytes.len() - 200] ^= 0x01;
    let mut block = RawBlock::parse(Bytes::from(bytes), fixture.branch_id).unwrap();
    block.header.merkle_root = hayai_wire::merkle_root(&block.txids());
    let Err(BlockError::Shielded(_)) = validate_block(block, &h.store, &view, &cfg) else {
        panic!("the shielded error comes before the contextual one");
    };
    // The contextual fault alone is reported.
    let Err(BlockError::Context(ContextError::TooManyOrchardActions(_))) =
        validate_block(h.block.clone(), &h.store, &view, &cfg)
    else {
        panic!("the contextual error alone");
    };
}

/// A chain whose window holds 100 layers under the fixture block validates the block to
/// the same layer as the chain without layers, cold and warm.
#[test]
fn validation_through_a_window_of_layers_agrees() {
    for fixture in [transparent_block(5, 2), mixed_block(3, 1, 2, 2)] {
        let flat = harness(&fixture);
        let (expected, _) = validate_block(
            flat.block.clone(),
            &flat.store,
            &flat.chain.view(),
            &flat.cfg,
        )
        .unwrap();
        let windowed = chain_with_layers(&fixture, 100, BlockShape::TYPICAL);
        assert_eq!(windowed.chain.layers().count(), 100);
        let view = windowed.chain.view();
        assert_eq!(view.tip().hash, windowed.block.header.prev_hash);
        let (cold, t) = validate_block(
            windowed.block.clone(),
            &windowed.store,
            &view,
            &windowed.cfg,
        )
        .unwrap();
        assert_eq!(t.unknown, windowed.block.txs.len());
        windowed.fill_store();
        let (warm, _) = validate_block(
            windowed.block.clone(),
            &windowed.store,
            &view,
            &windowed.cfg,
        )
        .unwrap();
        same_layer(&expected, &cold);
        same_layer(&expected, &warm);
        // The synthetic layers are visible through the view: the newest layer's coins
        // exist and the ones it spent do not.
        let newest = view.layers().last().unwrap();
        let (created, _) = newest.created.iter().next().unwrap();
        let Some(_) = view.get_coin(created) else {
            panic!("the newest layer's coin is visible");
        };
        let spent = newest.spent.iter().next().unwrap();
        assert_eq!(view.get_coin(spent), None);
    }
}

/// With a known parent history tree the layer carries the tree after the block: one more
/// leaf, ending at the block's height. Without it the layer's history is unknown and the
/// header commitment is not checked.
#[test]
fn layers_carry_the_history_tree() {
    let fixture = transparent_block(5, 2);
    let h = harness_with_history(&fixture);
    let parent = h.chain.view().history().unwrap();
    let (layer, t) = validate_block(h.block.clone(), &h.store, &h.chain.view(), &h.cfg).unwrap();
    let after = layer.history.as_ref().unwrap();
    assert_eq!(after.last_height(), Some(u64::from(fixture.height)));
    assert_eq!(after.upgrade(), fixture.branch_id);
    assert_ne!(after.root(), parent.root());
    assert_eq!(layer.history_root(), Some(after.root()));
    assert!(t.history > std::time::Duration::ZERO);

    let unknown = harness(&fixture);
    let (layer, _) = validate_block(
        unknown.block.clone(),
        &unknown.store,
        &unknown.chain.view(),
        &unknown.cfg,
    )
    .unwrap();
    let None = layer.history else {
        panic!("an unknown parent tree gives an unknown tree");
    };
}
