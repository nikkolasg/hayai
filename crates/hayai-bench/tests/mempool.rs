//! The mempool policy and the ZIP 401 store against the fixture transactions: real
//! signatures, real Orchard bundles.

use std::sync::Arc;

use bytes::Bytes;
use hayai_bench::chain_fixture::{harness, Harness};
use hayai_bench::fixtures::{mixed_block, orchard_block, transparent_block};
use hayai_consensus::{Network, COINBASE_MATURITY};
use hayai_prepared::{
    prepare, InsertError, MempoolPolicy, PolicyContext, PolicyReject, PreparedStore, PreparedTx,
    ScopedBatch, MEMPOOL_COST_THRESHOLD, TX_EXPIRING_SOON_THRESHOLD,
};
use hayai_template::{CandidateSource, SetEvent, Zip317Params};
use hayai_wire::RawTx;
use rand::rngs::StdRng;
use rand::SeedableRng;

fn admit(h: &Harness, tx: &PreparedTx, next_height: u32) -> Result<(), PolicyReject> {
    MempoolPolicy::of(Network::Mainnet).admit(
        tx,
        &PolicyContext {
            next_height,
            median_time_past: h.chain.view().median_time_past(),
            rules: &h.cfg.rules,
        },
    )
}

#[test]
fn every_fixture_transaction_is_standard() {
    for fixture in [
        transparent_block(5, 2),
        orchard_block(2, 2),
        mixed_block(3, 1, 2, 2),
    ] {
        let h = harness(&fixture);
        for tx in h.prepare_all() {
            assert_eq!(admit(&h, &tx, fixture.height), Ok(()), "{}", fixture.name);
            // The fixtures pay the conventional fee.
            let store = PreparedStore::new(h.cfg.epoch(), 1 << 20, Zip317Params::ZIP317);
            store.insert(tx.clone()).unwrap();
            let fees = store.fees(&tx.wtxid()).unwrap();
            assert_eq!(fees.fee, fees.conventional_fee);
            assert_eq!(fees.unpaid_actions, 0);
            assert_eq!(fees.eviction_weight, fees.cost);
            assert_eq!(
                fees.cost,
                (tx.raw.bytes.len() as u64).max(MEMPOOL_COST_THRESHOLD)
            );
        }
        // The coinbase is not a mempool transaction.
        let view = h.chain.view();
        let mut batch = ScopedBatch::new(&h.cfg.keys);
        let coinbase = prepare(h.block.txs[0].clone(), h.cfg.epoch(), &view, &mut batch).unwrap();
        assert_eq!(
            admit(&h, &coinbase, fixture.height),
            Err(PolicyReject::Coinbase)
        );
    }
}

#[test]
fn a_real_transaction_meets_the_context_rules_at_their_boundaries() {
    let fixture = transparent_block(2, 1);
    let h = harness(&fixture);
    let prepared = h.prepare_all();
    let tx = &prepared[0];
    let next = fixture.height;

    // Expiry: a copy with an expiry height (the signature is not checked again).
    let with_expiry = |expiry| {
        let mut copy = (**tx).clone();
        copy.expiry_height = expiry;
        admit(&h, &copy, next)
    };
    assert_eq!(with_expiry(next + TX_EXPIRING_SOON_THRESHOLD), Ok(()));
    let Err(PolicyReject::ExpiringSoon { .. }) = with_expiry(next + TX_EXPIRING_SOON_THRESHOLD - 1)
    else {
        panic!("2 blocks to the expiry height is too soon");
    };
    let Err(PolicyReject::Expired { .. }) = with_expiry(next - 1) else {
        panic!("an expiry height below the next block is expired");
    };

    // Coinbase maturity of the spent coin.
    let from_coinbase = |created| {
        let mut copy = (**tx).clone();
        copy.spent[0].is_coinbase = true;
        copy.spent[0].height = created;
        admit(&h, &copy, next)
    };
    let Err(PolicyReject::ImmatureCoinbase { .. }) = from_coinbase(next - COINBASE_MATURITY + 1)
    else {
        panic!("99 confirmations are not mature");
    };
    // Mature, and then the rule on transparent outputs applies to this transaction.
    assert_eq!(
        from_coinbase(next - COINBASE_MATURITY),
        Err(PolicyReject::UnshieldedCoinbaseSpend { input: 0 })
    );

    // Fee: 2 logical actions at 5,000 zatoshis. The unpaid action limit is 0, so the
    // transaction must pay 10,000 zatoshis. The minimum relay fee is below that value.
    let with_fee = |fee| {
        let mut copy = (**tx).clone();
        copy.fee = fee;
        admit(&h, &copy, next)
    };
    assert_eq!(with_fee(10_000), Ok(()));
    assert_eq!(
        with_fee(9_999),
        Err(PolicyReject::UnpaidActions {
            unpaid: 1,
            limit: 0
        })
    );
    assert_eq!(
        with_fee(100),
        Err(PolicyReject::UnpaidActions {
            unpaid: 2,
            limit: 0
        })
    );

    // A scriptSig with an opcode that is not a push: the transaction parses and is not
    // standard.
    let bundle = tx.raw.tx.transparent_bundle().unwrap();
    let script_sig = &bundle.vin[0].script_sig().0 .0;
    let at = tx
        .raw
        .bytes
        .windows(script_sig.len())
        .position(|w| w == &script_sig[..])
        .unwrap();
    let mut bytes = tx.raw.bytes.to_vec();
    // `OP_NOP` replaces the push of the public key: the push opcode and the 33 bytes.
    let key_push = at + script_sig.len() - 34;
    assert_eq!(bytes[key_push], 33);
    for byte in &mut bytes[key_push..key_push + 34] {
        *byte = 0x61;
    }
    let mut copy = (**tx).clone();
    copy.raw = Arc::new(RawTx::parse(Bytes::from(bytes), h.cfg.rules.branch_id).unwrap());
    assert_eq!(
        admit(&h, &copy, next),
        Err(PolicyReject::ScriptSigNotPushOnly { input: 0 })
    );
}

#[test]
fn zip_401_eviction_with_real_transactions() {
    let fixture = orchard_block(4, 2);
    let h = harness(&fixture);
    let prepared = h.prepare_all();
    // A 2-action Orchard transaction is below the cost threshold (ZIP 401 chose the
    // threshold for this).
    for tx in &prepared {
        assert!((tx.raw.bytes.len() as u64) < MEMPOOL_COST_THRESHOLD);
    }
    let mut stored = 0;
    let mut refused = 0;
    for seed in 0..40 {
        // Room for three transactions.
        let store = PreparedStore::with_rng(
            h.cfg.epoch(),
            3 * MEMPOOL_COST_THRESHOLD as usize,
            Zip317Params::ZIP317,
            Box::new(StdRng::seed_from_u64(seed)),
        );
        let events = store.events();
        for tx in &prepared[..3] {
            store.insert(tx.clone()).unwrap();
        }
        let new = &prepared[3];
        let result = store.insert(new.clone());
        assert_eq!(store.len(), 3);
        assert_eq!(store.total_cost(), 3 * MEMPOOL_COST_THRESHOLD);
        let removed: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                SetEvent::Removed(id) => Some(id),
                _ => None,
            })
            .collect();
        match result {
            Ok(()) => {
                stored += 1;
                // One stored transaction left, with its nullifiers and its event.
                let [victim] = removed[..] else {
                    panic!("one eviction, got {removed:?}");
                };
                let victim = prepared[..3].iter().find(|t| t.wtxid() == victim).unwrap();
                assert!(store.is_recently_evicted(&victim.raw.txid));
                for (pool, nullifier) in &victim.nullifiers {
                    assert_eq!(store.revealer(*pool, nullifier), None);
                }
                let Err(InsertError::RecentlyEvicted(_)) = store.insert(victim.clone()) else {
                    panic!("an evicted transaction is refused");
                };
            }
            Err(InsertError::Evicted) => {
                refused += 1;
                assert_eq!(removed, vec![]);
                assert!(store.is_recently_evicted(&new.raw.txid));
            }
            Err(other) => panic!("unexpected {other:?}"),
        }
    }
    // Four candidates of equal weight: the new transaction is selected 1 time in 4.
    assert!(stored > refused && refused > 0, "{stored} {refused}");
}
