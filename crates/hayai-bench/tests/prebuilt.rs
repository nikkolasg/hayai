//! Prebuilt bodies: a block whose body was prebuilt commits to the same layer, and leaves
//! the same window index state, as full validation; a block that is not the prebuilt one
//! is a mismatch, and an invalid header or coinbase is rejected as full validation rejects
//! it.

use hayai_bench::chain_fixture::{chain_with_layers, harness_with_history, Harness};
use hayai_bench::zakura_chain_clone::BlockShape;
use hayai_coins::{CoinsView, OutPoint, Pool};
use hayai_consensus::coinbase::CoinbaseError;
use hayai_fixtures::{mixed_block, orchard_block, transparent_block, CoinbaseChange, Fixture};
use hayai_state::{ContextError, Layer};
use hayai_validate::{
    commit_prebuilt, prebuild, validate_block, BlockError, CommitError, PrebuildError,
};
use hayai_wire::WtxId;

fn fixtures() -> Vec<Fixture> {
    vec![
        transparent_block(40, 2),
        orchard_block(2, 2),
        mixed_block(20, 1, 2, 2),
    ]
}

fn body(h: &Harness) -> Vec<WtxId> {
    h.block.txs[1..].iter().map(|t| t.wtxid()).collect()
}

fn same_layer(a: &Layer, b: &Layer) {
    assert_eq!(a.height, b.height);
    assert_eq!(a.hash, b.hash);
    assert_eq!(a.parent, b.parent);
    assert_eq!(a.time, b.time);
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
    assert_eq!(a.history_root(), b.history_root());
}

/// With the parent's history tree known, the swap commit checks the header commitment and
/// appends the same leaf.
#[test]
fn swap_commit_equals_full_validation() {
    for fixture in fixtures() {
        let h = harness_with_history(&fixture);
        h.fill_store();
        let view = h.chain.view();
        let prebuilt = prebuild(&body(&h), &h.store, &view, &h.cfg)
            .unwrap_or_else(|e| panic!("{}: prebuild: {e}", fixture.name));
        let (swapped, timings) = commit_prebuilt(&h.block, prebuilt, &view, &h.cfg)
            .unwrap_or_else(|e| panic!("{}: swap: {e}", fixture.name));
        assert_eq!(timings.known, h.block.txs.len() - 1);
        let (full, _) = validate_block(h.block.clone(), &h.store, &view, &h.cfg)
            .unwrap_or_else(|e| panic!("{}: full: {e}", fixture.name));
        same_layer(&swapped, &full);
        let Some(_) = swapped.history_root() else {
            panic!(
                "{}: the history tree after the block is known",
                fixture.name
            );
        };
    }
}

/// Over a window of layers, the chain that pushed the swapped layer answers every coin and
/// nullifier lookup as the chain that pushed the validated one.
#[test]
fn swap_commit_leaves_the_same_window_index_state() {
    for fixture in fixtures() {
        let swap_side = chain_with_layers(&fixture, 10, BlockShape::TYPICAL);
        let full_side = chain_with_layers(&fixture, 10, BlockShape::TYPICAL);
        swap_side.fill_store();
        full_side.fill_store();
        let view = swap_side.chain.view();
        let prebuilt = prebuild(&body(&swap_side), &swap_side.store, &view, &swap_side.cfg)
            .unwrap_or_else(|e| panic!("{}: prebuild: {e}", fixture.name));
        let (swapped, _) = commit_prebuilt(&swap_side.block, prebuilt, &view, &swap_side.cfg)
            .unwrap_or_else(|e| panic!("{}: swap: {e}", fixture.name));
        let (full, _) = validate_block(
            full_side.block.clone(),
            &full_side.store,
            &full_side.chain.view(),
            &full_side.cfg,
        )
        .unwrap_or_else(|e| panic!("{}: full: {e}", fixture.name));
        same_layer(&swapped, &full);

        let mut outpoints: Vec<OutPoint> = swapped.created.keys().cloned().collect();
        outpoints.extend(swapped.spent.iter().cloned());
        outpoints.extend(fixture.funding.iter().map(|(o, _)| o.clone()));
        let nullifiers: Vec<(Pool, Vec<[u8; 32]>)> = Pool::ALL
            .iter()
            .map(|pool| {
                let set = &swapped.nullifiers[pool.index()];
                (*pool, set.iter().copied().collect())
            })
            .collect();
        let mut swap_chain = swap_side.chain;
        let mut full_chain = full_side.chain;
        swap_chain.push(swapped).unwrap();
        full_chain.push(full).unwrap();
        let (a, b) = (swap_chain.view(), full_chain.view());
        assert_eq!(a.tip(), b.tip());
        assert_eq!(a.get_coins(&outpoints), b.get_coins(&outpoints));
        assert_eq!(a.get_coins(&outpoints), a.get_coins_by_walk(&outpoints));
        for (pool, keys) in &nullifiers {
            assert_eq!(
                a.contains_nullifier_many(*pool, keys),
                b.contains_nullifier_many(*pool, keys)
            );
            assert!(a.contains_nullifier_many(*pool, keys).iter().all(|f| *f));
        }
    }
}

#[test]
fn mismatches_and_invalid_blocks() {
    let fixture = mixed_block(20, 1, 2, 2);
    let h = harness_with_history(&fixture);
    h.fill_store();
    let view = h.chain.view();
    let ids = body(&h);
    let fresh = || prebuild(&ids, &h.store, &view, &h.cfg).unwrap();

    // Another body: the prebuilt body lacks the last transaction.
    let shorter = prebuild(&ids[..ids.len() - 1], &h.store, &view, &h.cfg).unwrap();
    let Err(CommitError::Mismatch(_)) = commit_prebuilt(&h.block, shorter, &view, &h.cfg) else {
        panic!("another body is a mismatch");
    };

    // A coinbase that does not pay its terms, on both paths: one zatoshi more than the
    // exact value of ZIP 236, one zatoshi less, and no funding stream output. The body is
    // the prebuilt one, so the swap path reaches the coinbase rules.
    type Expected = fn(&CoinbaseError) -> bool;
    let stream = CoinbaseChange::Remove { output: 1 };
    let cases: [(&[CoinbaseChange], Expected); 3] = [
        (
            &[CoinbaseChange::AddValue {
                output: 0,
                delta: 1,
            }],
            |e| matches!(e, CoinbaseError::ValueNotExact { paid, required } if *paid == required + 1),
        ),
        (
            &[CoinbaseChange::AddValue {
                output: 0,
                delta: -1,
            }],
            |e| matches!(e, CoinbaseError::ValueNotExact { paid, required } if *paid == required - 1),
        ),
        (&[stream], |e| {
            matches!(e, CoinbaseError::MissingOutput { .. })
        }),
    ];
    for (changes, expected) in cases {
        let block = fixture.with_coinbase(changes);
        let Err(CommitError::Invalid(BlockError::Context(ContextError::Coinbase(error)))) =
            commit_prebuilt(&block, fresh(), &view, &h.cfg)
        else {
            panic!("the swap path checks the coinbase terms: {changes:?}");
        };
        assert!(expected(&error), "{changes:?}: {error}");
        let Err(BlockError::Context(ContextError::Coinbase(error))) =
            validate_block(block, &h.store, &view, &h.cfg)
        else {
            panic!("the full path checks the coinbase terms: {changes:?}");
        };
        assert!(expected(&error), "{changes:?}: {error}");
    }

    // A header whose merkle root is not the block's.
    let mut wrong_root = h.block.clone();
    wrong_root.header.merkle_root = [0x42; 32];
    let Err(CommitError::Invalid(BlockError::MerkleRoot)) =
        commit_prebuilt(&wrong_root, fresh(), &view, &h.cfg)
    else {
        panic!("the swap path checks the merkle root");
    };

    // A header that commits to another history tree.
    let mut wrong_commitments = h.block.clone();
    wrong_commitments.header.block_commitments = [0x11; 32];
    let Err(CommitError::Invalid(BlockError::Context(ContextError::BlockCommitments))) =
        commit_prebuilt(&wrong_commitments, fresh(), &view, &h.cfg)
    else {
        panic!("the swap path checks the header commitment");
    };

    // A view that moved on: the body was prebuilt on the old tip.
    let prebuilt = fresh();
    let (layer, _) = validate_block(h.block.clone(), &h.store, &view, &h.cfg).unwrap();
    let mut chain = h.chain;
    chain.push(layer).unwrap();
    let Err(CommitError::Mismatch(_)) = commit_prebuilt(&h.block, prebuilt, &chain.view(), &h.cfg)
    else {
        panic!("a prebuilt body is valid on its parent only");
    };

    // A body with a transaction the store does not hold cannot be prebuilt.
    let missing = WtxId {
        txid: h.block.txs[0].txid,
        auth_digest: [0; 32],
    };
    let Err(PrebuildError::NotPrepared(id)) = prebuild(&[missing], &h.store, &view, &h.cfg) else {
        panic!("only prepared transactions are prebuilt");
    };
    assert_eq!(id, missing);
}

/// A block of a peer whose first transaction is not a coinbase, on a prebuilt body that
/// matches: the result is `NoCoinbase`, as on the full path. The first transaction has a
/// transparent input, and the commit has no coin for it.
#[test]
fn a_first_transaction_that_is_not_a_coinbase_is_refused() {
    let fixture = transparent_block(2, 1);
    let h = harness_with_history(&fixture);
    h.fill_store();
    let view = h.chain.view();
    let ids = body(&h);
    let with_txs = |txs: Vec<hayai_wire::RawTx>| {
        let mut block = h.block.clone();
        block.txs = txs;
        block.header.merkle_root = hayai_wire::merkle_root(&block.txids());
        block
    };

    // The prebuilt body of an empty mempool, and a block with one transaction that
    // spends a coin.
    let empty = prebuild(&[], &h.store, &view, &h.cfg).expect("an empty body");
    let alone = with_txs(vec![h.block.txs[1].clone()]);
    assert!(matches!(
        commit_prebuilt(&alone, empty, &view, &h.cfg),
        Err(CommitError::Invalid(BlockError::Context(
            ContextError::NoCoinbase
        )))
    ));
    assert!(matches!(
        validate_block(alone, &h.store, &view, &h.cfg),
        Err(BlockError::Context(ContextError::NoCoinbase))
    ));

    // The prebuilt body of the fixture, and a transaction of a peer in the place of the
    // coinbase.
    let prebuilt = prebuild(&ids, &h.store, &view, &h.cfg).expect("the body");
    let mut txs = h.block.txs.clone();
    txs[0] = txs[1].clone();
    let replaced = with_txs(txs);
    assert!(matches!(
        commit_prebuilt(&replaced, prebuilt, &view, &h.cfg),
        Err(CommitError::Invalid(BlockError::Context(
            ContextError::NoCoinbase
        )))
    ));

    // The block with its coinbase commits on the same prebuilt body.
    let prebuilt = prebuild(&ids, &h.store, &view, &h.cfg).expect("the body");
    commit_prebuilt(&h.block, prebuilt, &view, &h.cfg).expect("the block of the fixture");
}
