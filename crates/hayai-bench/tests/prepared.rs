//! hayai-prepared against the fixtures: every transaction prepares, tampering is caught,
//! bisection isolates a bad bundle, the store detects conflicts and tracks dependencies.

use std::sync::Arc;

use bytes::Bytes;
use hayai_bench::chain_fixture::{harness, Harness};
use hayai_bench::fixtures::{mixed_block, orchard_block, transparent_block};
use hayai_coins::{CoinsView, OutPoint};
use hayai_crypto::{zcash_primitives, zcash_protocol, zcash_transparent};
use hayai_prepared::{prepare, InsertError, PrepareError, PreparedTx, ScopedBatch};
use hayai_template::{CandidateSource, SetEvent};
use hayai_wire::{RawTx, TxLookup, WtxId};
use zcash_primitives::transaction::{Authorized, TransactionData, TxVersion};
use zcash_protocol::consensus::BlockHeight;
use zcash_protocol::value::Zatoshis;
use zcash_transparent::address::Script;
use zcash_transparent::bundle::{Bundle, TxIn, TxOut};

fn reparse(h: &Harness, bytes: Vec<u8>) -> RawTx {
    RawTx::parse(Bytes::from(bytes), h.cfg.rules.branch_id).expect("still parses")
}

/// Index of the first byte of the first signature push in the first input's scriptSig.
fn signature_offset(raw: &RawTx) -> usize {
    let bundle = raw.tx.transparent_bundle().unwrap();
    let script_sig = &bundle.vin[0].script_sig().0 .0;
    let position = raw
        .bytes
        .windows(script_sig.len())
        .position(|w| w == &script_sig[..])
        .expect("scriptSig is in the wire bytes");
    // The scriptSig starts with the push opcode; the DER signature follows.
    position + 1 + 4
}

#[test]
fn every_fixture_transaction_prepares() {
    for fixture in [
        transparent_block(5, 2),
        orchard_block(2, 2),
        mixed_block(3, 1, 2, 2),
    ] {
        let h = harness(&fixture);
        let prepared = h.prepare_all();
        assert_eq!(prepared.len(), h.block.txs.len() - 1);
        let fees: u64 = prepared.iter().map(|p| p.fee).sum();
        let coinbase_out = h.block.txs[0].tx.transparent_bundle().unwrap().vout[0]
            .value()
            .into_u64();
        assert_eq!(
            coinbase_out,
            hayai_bench::fixtures::coinbase_terms(fixture.height).miner_subsidy() + fees,
            "{}: fees add up to what the coinbase claims",
            fixture.name
        );
        for p in &prepared {
            assert!(p.scripts_ok && p.shielded_ok);
            assert_eq!(p.spent.len(), p.spent_outpoints().count());
            // Every P2PKH input and output is one sigop.
            let b = p.raw.tx.transparent_bundle().unwrap();
            assert_eq!(p.sigops as usize, b.vout.len());
            match p.raw.tx.orchard_bundle() {
                Some(ob) => {
                    assert_eq!(p.orchard_actions as usize, ob.actions().len());
                    assert_eq!(p.commitments.orchard.len(), ob.actions().len());
                    assert_eq!(p.nullifiers.len(), ob.actions().len());
                    assert_eq!(p.anchors.len(), 1);
                }
                None => {
                    assert_eq!(p.orchard_actions, 0);
                    assert!(p.nullifiers.is_empty() && p.anchors.is_empty());
                }
            }
        }
        // The coinbase prepares too.
        let view = h.chain.view();
        let mut batch = ScopedBatch::new(&h.cfg.keys);
        let cb = prepare(h.block.txs[0].clone(), h.cfg.epoch(), &view, &mut batch).unwrap();
        assert!(cb.is_coinbase && cb.fee == 0 && cb.spent.is_empty());
    }
}

#[test]
fn a_flipped_signature_byte_is_a_script_error() {
    let fixture = transparent_block(3, 2);
    let h = harness(&fixture);
    let view = h.chain.view();
    let raw = &h.block.txs[1];
    let mut bytes = raw.bytes.to_vec();
    let at = signature_offset(raw);
    bytes[at] ^= 0x01;
    let tampered = reparse(&h, bytes);
    let mut batch = ScopedBatch::new(&h.cfg.keys);
    let Err(PrepareError::Script(0, _)) = prepare(tampered, h.cfg.epoch(), &view, &mut batch)
    else {
        panic!("tampered signature must fail input 0");
    };
    // The second input is untouched: tampering it instead names index 1.
    let bundle = raw.tx.transparent_bundle().unwrap();
    let second = &bundle.vin[1].script_sig().0 .0;
    let mut bytes = raw.bytes.to_vec();
    let pos = bytes
        .windows(second.len())
        .position(|w| w == &second[..])
        .unwrap();
    bytes[pos + 5] ^= 0x01;
    let tampered = reparse(&h, bytes);
    let Err(PrepareError::Script(1, _)) = prepare(tampered, h.cfg.epoch(), &view, &mut batch)
    else {
        panic!("tampered second signature must fail input 1");
    };
}

#[test]
fn missing_input_and_duplicate_input_are_reported() {
    let fixture = transparent_block(2, 2);
    let h = harness(&fixture);
    let view = h.chain.view();
    let mut batch = ScopedBatch::new(&h.cfg.keys);
    let raw = &h.block.txs[1];
    // Against a chain funding other transactions, the first input has no coin.
    let other = harness(&orchard_block(2, 2));
    let other_view = other.chain.view();
    let Err(PrepareError::MissingInput(0)) =
        prepare(raw.clone(), h.cfg.epoch(), &other_view, &mut batch)
    else {
        panic!("unknown coin must be a missing input");
    };
    // Duplicate the first input.
    let data: &TransactionData<Authorized> = &raw.tx;
    let bundle = data.transparent_bundle().unwrap();
    let mut vin = bundle.vin.clone();
    vin.push(vin[0].clone());
    let dup = TransactionData::<Authorized>::from_parts(
        TxVersion::V5,
        h.cfg.rules.branch_id,
        data.lock_time(),
        data.expiry_height(),
        Some(Bundle {
            vin,
            vout: bundle.vout.clone(),
            authorization: zcash_transparent::bundle::Authorized,
        }),
        None,
        None,
        None,
    )
    .freeze()
    .unwrap();
    let mut bytes = Vec::new();
    dup.write(&mut bytes).unwrap();
    let dup = reparse(&h, bytes);
    let Err(PrepareError::DuplicateInput(2)) = prepare(dup, h.cfg.epoch(), &view, &mut batch)
    else {
        panic!("duplicate input must be reported at its index");
    };
}

#[test]
fn bisection_isolates_the_tampered_orchard_bundle() {
    let fixture = orchard_block(4, 2);
    let h = harness(&fixture);
    let view = h.chain.view();
    let mut batch = ScopedBatch::new(&h.cfg.keys);
    let mut ids: Vec<WtxId> = Vec::new();
    let mut bad: Option<WtxId> = None;
    for (i, raw) in h.block.txs[1..].iter().enumerate() {
        let raw = if i == 2 {
            // Flip a byte deep inside the proof (the last 100 bytes of the bundle precede
            // the binding signature; the proof lies well before).
            let mut bytes = raw.bytes.to_vec();
            let n = bytes.len();
            bytes[n - 200] ^= 0x01;
            let t = reparse(&h, bytes);
            bad = Some(t.wtxid());
            t
        } else {
            raw.clone()
        };
        let p = prepare(raw, h.cfg.epoch(), &view, &mut batch).expect("context-free rules hold");
        assert!(!p.shielded_ok, "verdict is pending until the batch runs");
        ids.push(p.wtxid());
    }
    assert_eq!(batch.len(), 4);
    let outcome = batch.finalize();
    assert_eq!(outcome.failed, vec![bad.unwrap()]);
    assert_eq!(outcome.ok.len(), 3);
    assert!(outcome
        .ok
        .iter()
        .all(|id| ids.contains(id) && Some(*id) != bad));
}

#[test]
fn an_untouched_orchard_batch_passes_as_one() {
    let fixture = orchard_block(2, 2);
    let h = harness(&fixture);
    let prepared = h.prepare_all();
    assert!(prepared.iter().all(|p| p.shielded_ok));
}

/// A v5 transaction spending output 0 of `parent` with an empty scriptSig; it never passes
/// script checks, so it is inserted into the store as an already-verified value to exercise
/// the dependency index.
fn child_of(h: &Harness, parent: &PreparedTx) -> PreparedTx {
    let outpoint = OutPoint::new(*parent.raw.txid.as_ref(), 0);
    let value = parent.raw.tx.transparent_bundle().unwrap().vout[0].value();
    let data = TransactionData::<Authorized>::from_parts(
        TxVersion::V5,
        h.cfg.rules.branch_id,
        0,
        BlockHeight::from_u32(0),
        Some(Bundle {
            vin: vec![TxIn::from_parts(outpoint, Script::default(), u32::MAX)],
            vout: vec![TxOut::new(
                Zatoshis::const_from_u64(value.into_u64() - 10_000),
                Script::default(),
            )],
            authorization: zcash_transparent::bundle::Authorized,
        }),
        None,
        None,
        None,
    );
    let mut bytes = Vec::new();
    data.freeze().unwrap().write(&mut bytes).unwrap();
    let raw = reparse(h, bytes);
    let mut child = parent.clone();
    child.raw = Arc::new(raw);
    child.spent = vec![hayai_coins::Coin {
        value: value.into_u64(),
        script_pubkey: Bytes::new(),
        height: 0,
        is_coinbase: false,
    }];
    child.fee = 10_000;
    child.sigops = 0;
    child
}

#[test]
fn store_detects_conflicts_tracks_parents_and_streams_events() {
    let fixture = transparent_block(4, 1);
    let h = harness(&fixture);
    let events = h.store.events();
    let prepared = h.prepare_all();
    for p in &prepared {
        h.store.insert(p.clone()).unwrap();
    }
    assert_eq!(h.store.len(), 4);
    assert_eq!(TxLookup::len(&h.store), 4);

    // A second transaction spending the same coin conflicts.
    let mut twin = (*prepared[0]).clone();
    let mut bytes = twin.raw.bytes.to_vec();
    bytes[12] ^= 0x01; // lock time byte: a different txid, same inputs
    twin.raw = Arc::new(reparse(&h, bytes));
    let outpoint = twin.spent_outpoints().next().unwrap().clone();
    let Err(InsertError::Conflict { outpoint: o, by }) = h.store.insert(Arc::new(twin)) else {
        panic!("same outpoint must conflict");
    };
    assert_eq!(o, outpoint);
    assert_eq!(by, prepared[0].wtxid());
    let Err(InsertError::Duplicate(_)) = h.store.insert(prepared[0].clone()) else {
        panic!("re-inserting is a duplicate");
    };
    let mut unverified = (*prepared[1]).clone();
    unverified.shielded_ok = false;
    let Err(InsertError::Unverified) = h.store.insert(Arc::new(unverified)) else {
        panic!("unverified transactions are refused");
    };

    // A child of a stored parent depends on it; evicting the parent evicts the child.
    let child = Arc::new(child_of(&h, &prepared[1]));
    h.store.insert(child.clone()).unwrap();
    let candidates = h.store.candidates();
    let c = candidates
        .iter()
        .find(|c| c.wtxid == child.wtxid())
        .unwrap();
    assert_eq!(c.depends_on, vec![prepared[1].wtxid()]);
    assert_eq!(
        h.store
            .spender(&child.spent_outpoints().next().unwrap().clone()),
        Some(child.wtxid())
    );

    // Lookups.
    assert_eq!(
        TxLookup::get(&h.store, &prepared[2].wtxid()).unwrap().txid,
        prepared[2].raw.txid
    );
    assert_eq!(
        h.store.get_by_txid(&prepared[2].raw.txid).unwrap().wtxid(),
        prepared[2].wtxid()
    );

    // Events so far: five Added.
    let mut added = 0;
    while let Ok(e) = events.try_recv() {
        let SetEvent::Added(_) = e else {
            panic!("only additions so far");
        };
        added += 1;
    }
    assert_eq!(added, 5);

    // A tip that spends the parent's input drops the parent and its child.
    let dropped = h.store.remove_conflicting(
        &[prepared[1].spent_outpoints().next().unwrap().clone()],
        &[],
    );
    assert_eq!(dropped, vec![prepared[1].wtxid()]);
    assert_eq!(h.store.len(), 3);
    assert_eq!(h.store.get(&child.wtxid()).map(|_| ()), None);
    let removed: Vec<WtxId> = std::iter::from_fn(|| events.try_recv().ok())
        .map(|e| match e {
            SetEvent::Removed(id) => id,
            other => panic!("expected removal, got {other:?}"),
        })
        .collect();
    assert_eq!(removed, vec![prepared[1].wtxid(), child.wtxid()]);

    // Mining keeps descendants: re-add the pair, then mine the parent.
    h.store.insert(prepared[1].clone()).unwrap();
    h.store.insert(child.clone()).unwrap();
    assert_eq!(h.store.remove_mined(&[prepared[1].wtxid()]), 1);
    assert_eq!(
        h.store.get(&child.wtxid()).map(|p| p.wtxid()),
        Some(child.wtxid())
    );
    assert_eq!(h.store.get(&prepared[1].wtxid()).map(|_| ()), None);

    // An epoch change drops everything prepared under the old one.
    let other = hayai_prepared::RuleEpoch::consensus(zcash_protocol::consensus::BranchId::Nu6_3);
    assert_eq!(h.store.set_epoch(other).len(), 4);
    assert!(h.store.is_empty());
    assert_eq!(h.store.cost_bytes(), 0);
    let Err(InsertError::Epoch { .. }) = h.store.insert(prepared[0].clone()) else {
        panic!("old-epoch transactions are refused");
    };
}

#[test]
fn chain_view_serves_prepare_through_coins_view() {
    let fixture = transparent_block(1, 2);
    let h = harness(&fixture);
    let view = h.chain.view();
    let outpoints: Vec<OutPoint> = fixture.funding.iter().map(|(o, _)| o.clone()).collect();
    let coins = view.get_coins(&outpoints);
    assert_eq!(coins.len(), outpoints.len());
    for coin in coins {
        let Some(_) = coin else {
            panic!("every funding coin is in the view");
        };
    }
}

/// A tip block revealing a stored nullifier drops the transaction, as a spent outpoint does.
#[test]
fn a_tip_revealing_a_stored_nullifier_drops_the_transaction() {
    let fixture = orchard_block(2, 2);
    let h = harness(&fixture);
    let prepared = h.prepare_all();
    let first = prepared[0].clone();
    h.store.insert(first.clone()).unwrap();
    let (pool, nf) = first.nullifiers[0];
    assert_eq!(h.store.revealer(pool, &nf), Some(first.wtxid()));
    let dropped = h.store.remove_conflicting(&[], &[(pool, nf)]);
    assert_eq!(dropped, vec![first.wtxid()]);
    assert_eq!(h.store.revealer(pool, &nf), None);
    assert!(h.store.is_empty());
    // After a conflict removal the nullifier is free again.
    h.store.insert(first).unwrap();
}
