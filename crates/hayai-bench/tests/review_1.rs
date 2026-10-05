//! Review findings against hayai-prepared. Each test asserts the behaviour the consensus
//! rules require; a failing test is a confirmed defect (see the review report).

use std::sync::Arc;

use bytes::Bytes;
use hayai_bench::chain_fixture::{harness, Harness};
use hayai_bench::fixtures::{orchard_block, transparent_block};
use hayai_coins::{Coin, OutPoint};
use hayai_crypto::{zcash_primitives, zcash_protocol, zcash_transparent};
use hayai_prepared::{draft, PreparedStore, PreparedTx, RuleEpoch};
use hayai_template::Zip317Params;
use hayai_wire::RawTx;
use zcash_primitives::transaction::{Authorized, TransactionData, TxVersion};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zcash_protocol::value::Zatoshis;
use zcash_transparent::address::Script;
use zcash_transparent::bundle::{Bundle, TxIn, TxOut};

fn compact_size(out: &mut Vec<u8>, n: usize) {
    assert!(n < 253);
    out.push(n as u8);
}

/// A v4 (Sapling-format) transaction with one transparent input, one transparent output,
/// no Sapling spends or outputs, no JoinSplits, and `value_balance` as valueBalanceSapling.
fn v4_transparent_with_value_balance(value_balance: i64) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&0x8000_0004u32.to_le_bytes()); // fOverwintered | version 4
    b.extend_from_slice(&0x892F_2085u32.to_le_bytes()); // Sapling version group id
    compact_size(&mut b, 1); // vin
    b.extend_from_slice(&[0x11; 32]);
    b.extend_from_slice(&0u32.to_le_bytes());
    compact_size(&mut b, 0); // scriptSig
    b.extend_from_slice(&u32::MAX.to_le_bytes()); // sequence
    compact_size(&mut b, 1); // vout
    b.extend_from_slice(&1_000u64.to_le_bytes());
    compact_size(&mut b, 1); // scriptPubKey: OP_TRUE
    b.push(0x51);
    b.extend_from_slice(&0u32.to_le_bytes()); // lock_time
    b.extend_from_slice(&0u32.to_le_bytes()); // expiry_height
    b.extend_from_slice(&value_balance.to_le_bytes()); // valueBalanceSapling
    compact_size(&mut b, 0); // vShieldedSpend
    compact_size(&mut b, 0); // vShieldedOutput
    compact_size(&mut b, 0); // vJoinSplit
    b
}

/// Protocol spec §7.1.2 ([Sapling onward]): "If effectiveVersion = 4 and there are no Spend
/// descriptions or Output descriptions, then valueBalanceSapling MUST be 0." zcashd
/// `bad-txns-valuebalance-nonzero`; Zebra `SerializationError::BadTransactionBalance`.
/// Upstream `Transaction::read_v4` drops the value silently, and hayai adds no rule.
#[test]
fn v4_value_balance_without_sapling_components_is_rejected() {
    let bytes = v4_transparent_with_value_balance(5_000);
    let raw = RawTx::parse(Bytes::from(bytes), BranchId::Nu6_2).expect("upstream parses it");
    let None = raw.tx.sapling_bundle() else {
        panic!("no Sapling bundle is produced for empty spends and outputs");
    };
    let coins = vec![Coin {
        value: 2_000,
        script_pubkey: Bytes::from_static(&[0x51]),
        height: 1,
        is_coinbase: false,
    }];
    let result = draft(raw, RuleEpoch::consensus(BranchId::Nu6_2), coins).map(|d| d.tx().fee);
    let Err(_) = result else {
        panic!(
            "a v4 transaction with valueBalanceSapling = 5000 and no Sapling components must \
             be rejected; hayai accepted it with fee {result:?}"
        );
    };
}

/// Protocol spec §7.1.2 ([NU5 onward]): "If effectiveVersion >= 5 and nActionsOrchard > 0,
/// then at least one of enableSpendsOrchard and enableOutputsOrchard MUST be 1." Zebra
/// `has_enough_orchard_flags`. hayai checks the flags only for a coinbase (spends flag).
#[test]
fn orchard_bundle_with_both_flags_disabled_is_rejected() {
    let fixture = orchard_block(2, 2);
    let h = harness(&fixture);
    let raw = &h.block.txs[1];
    let bundle = raw.tx.orchard_bundle().expect("orchard fixture");
    let n = bundle.actions().len();
    let proof_len = bundle.authorization().proof().as_ref().len();
    assert!((253..0x1_0000).contains(&proof_len), "3-byte CompactSize");
    // Wire tail after the actions: flags, valueBalance, anchor, proof (CompactSize + bytes),
    // spendAuthSigs, bindingSig.
    let tail = 1 + 8 + 32 + 3 + proof_len + 64 * n + 64;
    let flags_at = raw.bytes.len() - tail;
    let mut bytes = raw.bytes.to_vec();
    assert_eq!(
        bytes[flags_at], 0b11,
        "fixture bundles enable spends and outputs"
    );
    bytes[flags_at] = 0;
    let mutated = RawTx::parse(Bytes::from(bytes), h.cfg.rules.branch_id).expect("parses");
    let flags = mutated.tx.orchard_bundle().expect("still present").flags();
    assert!(!flags.spends_enabled() && !flags.outputs_enabled());
    let spent: Vec<Coin> = fixture.funding[..1]
        .iter()
        .map(|(_, f)| hayai_bench::chain_fixture::coin(f))
        .collect();
    let result = draft(mutated, h.cfg.epoch(), spent).map(|_| ());
    let Err(_) = result else {
        panic!("an Orchard bundle with actions and both flags disabled must be rejected");
    };
}

/// Two stored transactions revealing the same nullifier can never be in one block
/// (`contextual_check` rejects the block), yet the store accepts both and the template has
/// no nullifier knowledge, so a miner can build an invalid block from its own store.
#[test]
fn store_rejects_a_nullifier_already_spent_by_a_stored_transaction() {
    let fixture = orchard_block(2, 2);
    let h = harness(&fixture);
    let prepared = h.prepare_all();
    let first = prepared[0].clone();
    let mut second: PreparedTx = (*prepared[1]).clone();
    second.nullifiers = first.nullifiers.clone();
    assert_ne!(first.wtxid(), second.wtxid());
    h.store.insert(first).unwrap();
    let Err(_) = h.store.insert(Arc::new(second)) else {
        panic!("a second transaction revealing a stored nullifier must be a conflict");
    };
}

fn reparse(h: &Harness, bytes: Vec<u8>) -> RawTx {
    RawTx::parse(Bytes::from(bytes), h.cfg.rules.branch_id).unwrap()
}

/// A child spending output 0 of `parent`, inserted as already verified (as the prepared
/// store tests do) to exercise the dependency index.
fn child_of(h: &Harness, parent: &PreparedTx, fee: u64) -> PreparedTx {
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
                Zatoshis::const_from_u64(value.into_u64() - fee),
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
    child.spent = vec![Coin {
        value: value.into_u64(),
        script_pubkey: Bytes::new(),
        height: 0,
        is_coinbase: false,
    }];
    child.fee = fee;
    child.sigops = 0;
    child
}

/// Eviction runs after the new transaction's parents were computed and before it is
/// indexed, so a high-fee child whose only parent is the eviction victim is stored with a
/// parent that is neither in the store nor in the chain. Such a child can never be mined,
/// and the `Added` event names a parent the template has just seen `Removed`.
#[test]
fn eviction_never_strands_the_inserted_transaction() {
    let fixture = transparent_block(2, 1);
    let h = harness(&fixture);
    let prepared = h.prepare_all();
    let parent = prepared[0].clone();
    let other = prepared[1].clone();
    let parent_value = parent.raw.tx.transparent_bundle().unwrap().vout[0]
        .value()
        .into_u64();
    let child = Arc::new(child_of(&h, &parent, parent_value / 2));
    // Room for the parent and the other transaction, not for a third (ZIP 401: each has
    // the cost 10,000).
    let limit = 2 * hayai_prepared::MEMPOOL_COST_THRESHOLD as usize;
    let store = PreparedStore::new(h.cfg.epoch(), limit, Zip317Params::ZIP317);
    store.insert(parent.clone()).unwrap();
    store.insert(other.clone()).unwrap();
    let result = store.insert(child.clone());
    let parent_present = store.get(&parent.wtxid()).map(|_| ());
    let child_present = store.get(&child.wtxid()).map(|_| ());
    let (Some(()), None) = (child_present, parent_present) else {
        return;
    };
    panic!(
        "child stored without its parent (insert result {result:?}); either the child is \
         rejected or its parent stays"
    );
}
