//! hayai-state contextual rules against fixture blocks: each rule is violated once and
//! rejected with its specific error; a valid block becomes a layer that the chain accepts.

use std::sync::Arc;

use bytes::Bytes;
use hayai_bench::chain_fixture::{harness, Harness};
use hayai_bench::fixtures::{
    coinbase_terms, mixed_block, orchard_block, transparent_block, CoinbaseChange, FIXTURE_HEIGHT,
};
use hayai_coins::{Coin, CoinsView, OutPoint, Pool};
use hayai_consensus::coinbase::{CoinbaseError, OutputKind};
use hayai_consensus::funding::Receiver;
use hayai_consensus::{rules_at, BlockLimits, Network, RuleSet, Upgrade, LOCKTIME_THRESHOLD};
use hayai_crypto::{zcash_primitives, zcash_transparent};
use hayai_prepared::PreparedTx;
use hayai_state::{
    block_outputs, contextual_check, contextual_check_with_outputs, prebuild_body, resolve_inputs,
    Base, Chain, CheckConfig, ContextError, PreparedBlock, SwapError, ValuePools,
};
use hayai_wire::RawTx;
use zcash_primitives::transaction::{Authorized, TransactionData, TxVersion};
use zcash_transparent::bundle::{Bundle, TxIn};

fn prepared_block(h: &Harness) -> PreparedBlock {
    let view = h.chain.view();
    let mut batch = hayai_prepared::ScopedBatch::new(&h.cfg.keys);
    let coinbase =
        hayai_prepared::prepare(h.block.txs[0].clone(), h.cfg.epoch(), &view, &mut batch).unwrap();
    let mut txs = vec![Arc::new(coinbase)];
    txs.extend(h.prepare_all());
    PreparedBlock::new(h.block.clone(), txs)
}

fn check(h: &Harness, block: &PreparedBlock) -> Result<hayai_state::Checked, ContextError> {
    check_with(h, block, BlockLimits::PRE_NU7)
}

fn check_with(
    h: &Harness,
    block: &PreparedBlock,
    limits: BlockLimits,
) -> Result<hayai_state::Checked, ContextError> {
    let rules = RuleSet {
        limits,
        ..h.cfg.rules
    };
    check_under(h, block, &rules)
}

/// The contextual check of `block` on the chain of `h` under `rules`.
fn check_under(
    h: &Harness,
    block: &PreparedBlock,
    rules: &RuleSet,
) -> Result<hayai_state::Checked, ContextError> {
    contextual_check(
        &h.chain.view(),
        block,
        &CheckConfig {
            network: h.cfg.network,
            rules,
        },
    )
}

/// Re-types transaction `i` of the block with new inputs and lock time; the scripts no
/// longer verify, which contextual checks do not look at.
fn rewrite_inputs(
    h: &Harness,
    block: &mut PreparedBlock,
    i: usize,
    vin: Vec<TxIn<zcash_transparent::bundle::Authorized>>,
    lock_time: u32,
    spent: Vec<Coin>,
) {
    let old = &block.txs[i];
    let data: &TransactionData<Authorized> = &old.raw.tx;
    let bundle = data.transparent_bundle().unwrap();
    let tx = TransactionData::<Authorized>::from_parts(
        TxVersion::V5,
        h.cfg.rules.branch_id,
        lock_time,
        data.expiry_height(),
        Some(Bundle {
            vin,
            vout: bundle.vout.clone(),
            authorization: zcash_transparent::bundle::Authorized,
        }),
        None,
        None,
        data.orchard_bundle().cloned(),
    )
    .freeze()
    .unwrap();
    let mut bytes = Vec::new();
    tx.write(&mut bytes).unwrap();
    let raw = RawTx::parse(Bytes::from(bytes), h.cfg.rules.branch_id).unwrap();
    let mut p: PreparedTx = (*block.txs[i]).clone();
    p.raw = Arc::new(raw.clone());
    p.spent = spent;
    p.lock_time = lock_time;
    block.raw.txs[i] = raw;
    block.txs[i] = Arc::new(p);
}

fn edit(block: &mut PreparedBlock, i: usize, f: impl FnOnce(&mut PreparedTx)) {
    let mut p: PreparedTx = (*block.txs[i]).clone();
    f(&mut p);
    block.txs[i] = Arc::new(p);
}

#[test]
fn valid_blocks_become_layers_the_chain_accepts() {
    for fixture in [
        transparent_block(5, 2),
        orchard_block(2, 2),
        mixed_block(3, 1, 2, 2),
    ] {
        let mut h = harness(&fixture);
        let block = prepared_block(&h);
        let checked = check(&h, &block).unwrap_or_else(|e| panic!("{}: {e}", fixture.name));
        let layer = checked.layer;
        assert_eq!(layer.height, FIXTURE_HEIGHT);
        assert_eq!(layer.hash, h.block.hash());
        assert_eq!(layer.spent.len(), fixture.funding.len());
        let outputs: usize = h
            .block
            .txs
            .iter()
            .map(|t| t.tx.transparent_bundle().unwrap().vout.len())
            .sum();
        assert_eq!(layer.created.len(), outputs);
        let actions: usize = block.txs.iter().map(|t| t.orchard_actions as usize).sum();
        assert_eq!(layer.nullifiers[Pool::Orchard.index()].len(), actions);
        let empty = hayai_trees::OrchardFrontier::empty().root().to_bytes();
        assert_eq!(layer.anchors.orchard != empty, actions > 0);
        let orchard_in: u64 = block
            .txs
            .iter()
            .filter_map(|t| t.raw.tx.orchard_bundle())
            .map(|b| u64::try_from(-i64::from(*b.value_balance())).unwrap())
            .sum();
        assert_eq!(layer.value_pools.orchard, orchard_in);

        let layer = h.chain.push(layer).unwrap();
        let view = h.chain.view();
        assert_eq!(view.tip_height(), FIXTURE_HEIGHT);
        assert!(view.has_anchor(Pool::Orchard, &layer.anchors.orchard));
        let (first, _) = &fixture.funding[0];
        assert_eq!(view.get_coin(first), None, "spent by the block");
        let cb = OutPoint::new(*h.block.txs[0].txid.as_ref(), 0);
        let coin = view.get_coin(&cb).expect("coinbase output is created");
        assert!(coin.is_coinbase && coin.height == FIXTURE_HEIGHT);
    }
}

#[test]
fn double_spend_across_blocks_is_a_missing_input() {
    let fixture = transparent_block(3, 1);
    let mut h = harness(&fixture);
    let block = prepared_block(&h);
    let layer = check(&h, &block).unwrap().layer;
    h.chain.push(layer).unwrap();
    // The same block again on top of itself: its parent matches, its inputs are gone.
    let mut again = PreparedBlock::new(block.raw.clone(), block.txs.clone());
    again.raw.header.prev_hash = h.block.hash();
    // Its coinbase has to commit to the new height (CScriptNum 3,400,001, pushed).
    let script_sig = [4u8, 3, 0x41, 0xe1, 0x33];
    let script = zcash_transparent::address::Script::read(&script_sig[..]).unwrap();
    let null = TxIn::from_parts(OutPoint::new([0; 32], u32::MAX), script, u32::MAX);
    rewrite_inputs(&h, &mut again, 0, vec![null], 0, Vec::new());
    edit(&mut again, 0, |p| p.expiry_height = FIXTURE_HEIGHT + 1);
    let Err(ContextError::MissingInput { tx: 1, input: 0 }) = check(&h, &again) else {
        panic!("spent coins must be missing");
    };
    // And off the tip it is the wrong parent.
    let Err(ContextError::WrongParent { .. }) = check(&h, &block) else {
        panic!("parent mismatch must be reported");
    };
}

#[test]
fn double_spend_within_a_block_is_rejected() {
    let fixture = transparent_block(3, 1);
    let h = harness(&fixture);
    let mut block = prepared_block(&h);
    let first_input = block.txs[1].raw.tx.transparent_bundle().unwrap().vin[0].clone();
    let spent = block.txs[1].spent.clone();
    rewrite_inputs(&h, &mut block, 2, vec![first_input], 0, spent);
    let Err(ContextError::DoubleSpend { tx: 2, input: 0 }) = check(&h, &block) else {
        panic!("second spend of the same outpoint must be rejected");
    };
}

#[test]
fn block_outputs_are_the_layers_created_map() {
    let fixture = transparent_block(4, 2);
    let h = harness(&fixture);
    let block = prepared_block(&h);
    let outputs = block_outputs(&block.raw, FIXTURE_HEIGHT);
    let expected: usize = block
        .raw
        .txs
        .iter()
        .map(|t| t.tx.transparent_bundle().unwrap().vout.len())
        .sum();
    assert_eq!(outputs.len(), expected);
    for (i, t) in block.raw.txs.iter().enumerate() {
        for (n, out) in t.tx.transparent_bundle().unwrap().vout.iter().enumerate() {
            let coin = &outputs[&OutPoint::new(*t.txid.as_ref(), n as u32)];
            assert_eq!(coin.value, out.value().into_u64());
            assert_eq!(&coin.script_pubkey[..], &out.script_pubkey().0 .0[..]);
            assert_eq!(coin.height, FIXTURE_HEIGHT);
            assert_eq!(coin.is_coinbase, i == 0);
        }
    }
    // The validator's entry point, given the map and the inputs it resolved, produces the
    // same layer as the plain check, and the map is the layer's `created`.
    let view = h.chain.view();
    let cfg = CheckConfig {
        network: h.cfg.network,
        rules: &h.cfg.rules,
    };
    let plain = contextual_check(&view, &block, &cfg).unwrap().layer;
    let inputs = resolve_inputs(&view, &block.raw, &outputs).unwrap();
    assert_eq!(inputs.len(), block.raw.txs.len());
    assert!(inputs[0].is_empty(), "the coinbase spends nothing");
    for (tx, coins) in block.txs.iter().zip(&inputs) {
        assert_eq!(&tx.spent, coins, "the view resolves the prepared coins");
    }
    let given = contextual_check_with_outputs(&view, &block, outputs, &inputs, &cfg)
        .unwrap()
        .layer;
    assert_eq!(given.created, plain.created);
    assert_eq!(given.spent, plain.spent);
    assert_eq!(given.anchors, plain.anchors);
    assert_eq!(given.created.len(), expected);
}

#[test]
fn spending_a_later_transaction_of_the_block_is_rejected() {
    let fixture = transparent_block(3, 1);
    let h = harness(&fixture);
    let mut block = prepared_block(&h);
    let later = OutPoint::new(*block.txs[3].raw.txid.as_ref(), 0);
    let out = block.txs[3].raw.tx.transparent_bundle().unwrap().vout[0].clone();
    let coin = Coin {
        value: out.value().into_u64(),
        script_pubkey: Bytes::copy_from_slice(&out.script_pubkey().0 .0),
        height: FIXTURE_HEIGHT,
        is_coinbase: false,
    };
    let txin = TxIn::from_parts(later, Default::default(), u32::MAX);
    rewrite_inputs(&h, &mut block, 1, vec![txin], 0, vec![coin]);
    let Err(ContextError::MissingInput { tx: 1, input: 0 }) = check(&h, &block) else {
        panic!("forward reference inside the block must be rejected");
    };
}

#[test]
fn immature_coinbase_spend_is_rejected() {
    let fixture = transparent_block(2, 1);
    let h = harness(&fixture);
    {
        let (outpoint, _) = &fixture.funding[1];
        let mut base = h.chain.base().write();
        let mut coin = base.coins.spend(outpoint).unwrap();
        coin.is_coinbase = true;
        coin.height = FIXTURE_HEIGHT - 99;
        base.coins.add(outpoint.clone(), coin).unwrap();
    }
    let block = prepared_block(&h);
    let Err(ContextError::ImmatureCoinbase {
        tx: 2,
        input: 0,
        created,
        height,
    }) = check(&h, &block)
    else {
        panic!("99-block-old coinbase must be immature");
    };
    assert_eq!((created, height), (FIXTURE_HEIGHT - 99, FIXTURE_HEIGHT));
    // One block older it is mature, but the spender has transparent outputs (zcashd
    // `bad-txns-coinbase-spend-has-transparent-outputs`).
    {
        let (outpoint, _) = &fixture.funding[1];
        let mut base = h.chain.base().write();
        let mut coin = base.coins.spend(outpoint).unwrap();
        coin.height = FIXTURE_HEIGHT - 100;
        base.coins.add(outpoint.clone(), coin).unwrap();
    }
    let Err(ContextError::UnshieldedCoinbaseSpend { tx: 2, input: 0 }) = check(&h, &block) else {
        panic!("coinbase spent to transparent outputs must be rejected");
    };
}

/// A mature coinbase spent by a transaction whose only outputs are Orchard is valid.
#[test]
fn mature_coinbase_spent_to_shielded_outputs_is_accepted() {
    let fixture = orchard_block(2, 2);
    let h = harness(&fixture);
    {
        let (outpoint, _) = &fixture.funding[1];
        let mut base = h.chain.base().write();
        let mut coin = base.coins.spend(outpoint).unwrap();
        coin.is_coinbase = true;
        coin.height = FIXTURE_HEIGHT - 100;
        base.coins.add(outpoint.clone(), coin).unwrap();
    }
    let block = prepared_block(&h);
    assert!(
        block.txs[2]
            .raw
            .tx
            .transparent_bundle()
            .unwrap()
            .vout
            .is_empty(),
        "the Orchard fixture spender has no transparent outputs"
    );
    check(&h, &block).expect("mature coinbase spends into the Orchard pool");
}

#[test]
fn prepared_against_a_different_coin_is_rejected() {
    let fixture = transparent_block(2, 1);
    let h = harness(&fixture);
    let mut block = prepared_block(&h);
    edit(&mut block, 1, |p| p.spent[0].value += 1);
    let Err(ContextError::SpentMismatch { tx: 1, input: 0 }) = check(&h, &block) else {
        panic!("stale prepared coin must be rejected");
    };
}

#[test]
fn duplicate_nullifiers_are_rejected_in_view_and_in_block() {
    let fixture = orchard_block(2, 2);
    let h = harness(&fixture);
    let block = prepared_block(&h);
    let (pool, nf) = block.txs[2].nullifiers[1];
    {
        let mut base = h.chain.base().write();
        base.nullifiers.pool_mut(pool).insert_many(&[nf]);
    }
    let Err(ContextError::DuplicateNullifier {
        pool: Pool::Orchard,
        tx: 2,
    }) = check(&h, &block)
    else {
        panic!("nullifier already in the view must be rejected");
    };

    let h = harness(&fixture);
    let mut block = prepared_block(&h);
    let first = block.txs[1].nullifiers.clone();
    edit(&mut block, 2, |p| p.nullifiers = first);
    let Err(ContextError::DuplicateNullifier {
        pool: Pool::Orchard,
        tx: 2,
    }) = check(&h, &block)
    else {
        panic!("nullifier repeated inside the block must be rejected");
    };
}

#[test]
fn unknown_anchor_is_rejected() {
    let fixture = orchard_block(2, 2);
    let h = harness(&fixture);
    let mut block = prepared_block(&h);
    edit(&mut block, 1, |p| p.anchors[0].1 = [0x5a; 32]);
    let Err(ContextError::BadAnchor {
        pool: Pool::Orchard,
        tx: 1,
    }) = check(&h, &block)
    else {
        panic!("an anchor that is no earlier treestate must be rejected");
    };
}

#[test]
fn expiry_and_lock_time_rules() {
    let fixture = transparent_block(2, 1);
    let h = harness(&fixture);
    let mut block = prepared_block(&h);
    edit(&mut block, 2, |p| p.expiry_height = FIXTURE_HEIGHT - 1);
    let Err(ContextError::Expired { tx: 2, expiry }) = check(&h, &block) else {
        panic!("expired transaction must be rejected");
    };
    assert_eq!(expiry, FIXTURE_HEIGHT - 1);
    let mut block = prepared_block(&h);
    edit(&mut block, 2, |p| p.expiry_height = FIXTURE_HEIGHT);
    check(&h, &block).expect("expiring at the block height is still valid");

    // Coinbase expiry must equal the height from NU5.
    let mut block = prepared_block(&h);
    edit(&mut block, 0, |p| p.expiry_height = FIXTURE_HEIGHT + 1);
    let Err(ContextError::CoinbaseExpiry { .. }) = check(&h, &block) else {
        panic!("coinbase expiry must be the block height");
    };

    // A height lock in the future with a non-final sequence is not final; with all
    // sequences final it is.
    let mut block = prepared_block(&h);
    let txin = block.txs[1].raw.tx.transparent_bundle().unwrap().vin[0].clone();
    let spent = block.txs[1].spent.clone();
    let open = TxIn::from_parts(txin.prevout().clone(), txin.script_sig().clone(), 0);
    rewrite_inputs(
        &h,
        &mut block,
        1,
        vec![open],
        FIXTURE_HEIGHT + 5,
        spent.clone(),
    );
    let Err(ContextError::NotFinal(1)) = check(&h, &block) else {
        panic!("future lock time with open sequence is not final");
    };
    let mut block = prepared_block(&h);
    rewrite_inputs(
        &h,
        &mut block,
        1,
        vec![txin.clone()],
        FIXTURE_HEIGHT + 5,
        spent.clone(),
    );
    check(&h, &block).expect("final sequences override the lock time");
    let mut block = prepared_block(&h);
    let open = TxIn::from_parts(txin.prevout().clone(), txin.script_sig().clone(), 0);
    rewrite_inputs(&h, &mut block, 1, vec![open], FIXTURE_HEIGHT - 1, spent);
    check(&h, &block).expect("a lock height below the block height has passed");
}

/// The lock time rule at its boundaries (zcashd `IsFinalTx` with the height and the time
/// of the block). A lock time below 500,000,000 is a height and must be below the block
/// height. A lock time from 500,000,000 is a time and must be below the block time.
#[test]
fn a_lock_time_is_a_height_or_a_time() {
    let fixture = transparent_block(2, 1);
    let h = harness(&fixture);
    let block_time = h.block.header.time;
    assert!(block_time > LOCKTIME_THRESHOLD);
    let with_lock = |lock_time: u32, sequence: u32| {
        let mut block = prepared_block(&h);
        let txin = block.txs[1].raw.tx.transparent_bundle().unwrap().vin[0].clone();
        let spent = block.txs[1].spent.clone();
        let vin = vec![TxIn::from_parts(
            txin.prevout().clone(),
            txin.script_sig().clone(),
            sequence,
        )];
        rewrite_inputs(&h, &mut block, 1, vin, lock_time, spent);
        check(&h, &block).map(|_| ())
    };
    let not_final = Err(ContextError::NotFinal(1));
    // A height.
    assert_eq!(with_lock(FIXTURE_HEIGHT - 1, 0), Ok(()));
    assert_eq!(with_lock(FIXTURE_HEIGHT, 0), not_final);
    assert_eq!(with_lock(LOCKTIME_THRESHOLD - 1, 0), not_final);
    // A time.
    assert_eq!(with_lock(LOCKTIME_THRESHOLD, 0), Ok(()));
    assert_eq!(with_lock(block_time - 1, 0), Ok(()));
    assert_eq!(with_lock(block_time, 0), not_final);
    assert_eq!(with_lock(u32::MAX, 0), not_final);
    // A final sequence number in each input switches the rule off.
    assert_eq!(with_lock(FIXTURE_HEIGHT, u32::MAX), Ok(()));
    assert_eq!(with_lock(block_time, u32::MAX), Ok(()));
}

/// A prebuilt body has no block time. The commit applies the time rule with the time of
/// the header: the largest lock time of the body is below it.
#[test]
fn a_prebuilt_body_keeps_its_time_lock_for_the_commit() {
    let fixture = transparent_block(2, 1);
    let h = harness(&fixture);
    let view = h.chain.view();
    let lock = h.block.header.time;
    let mut block = prepared_block(&h);
    let txin = block.txs[1].raw.tx.transparent_bundle().unwrap().vin[0].clone();
    let spent = block.txs[1].spent.clone();
    let open = TxIn::from_parts(txin.prevout().clone(), txin.script_sig().clone(), 0);
    rewrite_inputs(&h, &mut block, 1, vec![open], lock, spent);
    block.raw.header.merkle_root = hayai_wire::merkle_root(&block.raw.txids());
    let cfg = CheckConfig {
        network: h.cfg.network,
        rules: &h.cfg.rules,
    };
    let commit_at = |time: u32| {
        let mut raw = block.raw.clone();
        raw.header.time = time;
        prebuild_body(&view, &block.txs[1..], &cfg)
            .expect("the body has no block time")
            .commit(&view, &raw, &block.txs[0], &cfg)
            .map(|checked| checked.layer.time)
    };
    assert_eq!(
        commit_at(lock),
        Err(SwapError::Context(ContextError::NotFinal(1)))
    );
    assert_eq!(commit_at(lock + 1), Ok(lock + 1));
}

/// zcashd `MAX_BLOCK_SIGOPS`: a block with 20,000 sigops is valid, and a block with
/// 20,001 is not.
#[test]
fn a_block_has_at_most_20_000_sigops() {
    let fixture = transparent_block(2, 1);
    let h = harness(&fixture);
    assert_eq!(h.cfg.rules.limits.sigops, 20_000);
    let with_sigops = |total: u32| {
        let mut block = prepared_block(&h);
        let others: u32 = block.txs.iter().map(|t| t.sigops).sum::<u32>() - block.txs[2].sigops;
        edit(&mut block, 2, |p| p.sigops = total - others);
        check_under(&h, &block, &h.cfg.rules).map(|_| ())
    };
    assert_eq!(with_sigops(20_000), Ok(()));
    assert_eq!(
        with_sigops(20_001),
        Err(ContextError::TooManySigops(20_001))
    );
}

/// The coinbase-spend rule by network. Mainnet and Testnet: a transaction that spends a
/// mature coinbase output has no transparent output. Regtest does not have that rule
/// (Zakura `zakura-chain/src/transaction.rs:552-564`,
/// `parameters/network/testnet.rs:1426`). Coinbase maturity applies on every network.
#[test]
fn regtest_allows_a_coinbase_spend_with_transparent_outputs() {
    let fixture = transparent_block(2, 1);
    let h = harness(&fixture);
    let set_coin_height = |height: u32| {
        let (outpoint, _) = &fixture.funding[1];
        let mut base = h.chain.base().write();
        let mut coin = base.coins.spend(outpoint).unwrap();
        coin.is_coinbase = true;
        coin.height = height;
        base.coins.add(outpoint.clone(), coin).unwrap();
    };
    // The body alone, without a coinbase: the terms of a Regtest coinbase do not matter.
    let body_on = |network: Network| {
        let cfg = CheckConfig {
            network,
            rules: &h.cfg.rules,
        };
        prebuild_body(&h.chain.view(), &h.prepare_all(), &cfg).map(|_| ())
    };
    set_coin_height(FIXTURE_HEIGHT - 100);
    for network in [Network::Mainnet, Network::Testnet] {
        assert!(network.params().coinbase_must_be_shielded);
        assert_eq!(
            body_on(network),
            Err(ContextError::UnshieldedCoinbaseSpend { tx: 2, input: 0 })
        );
    }
    assert!(!Network::Regtest.params().coinbase_must_be_shielded);
    assert_eq!(body_on(Network::Regtest), Ok(()));
    set_coin_height(FIXTURE_HEIGHT - 99);
    assert_eq!(
        body_on(Network::Regtest),
        Err(ContextError::ImmatureCoinbase {
            tx: 2,
            input: 0,
            created: FIXTURE_HEIGHT - 99,
            height: FIXTURE_HEIGHT,
        })
    );
}

#[test]
fn block_totals() {
    let fixture = mixed_block(3, 1, 2, 2);
    let h = harness(&fixture);
    let block = prepared_block(&h);
    let Err(ContextError::TooManySigops(_)) = check_with(
        &h,
        &block,
        BlockLimits {
            sigops: 3,
            ..BlockLimits::PRE_NU7
        },
    ) else {
        panic!("sigop limit");
    };
    let Err(ContextError::TooManyOrchardActions(4)) = check_with(
        &h,
        &block,
        BlockLimits {
            orchard_actions: 3,
            ..BlockLimits::NU7
        },
    ) else {
        panic!("orchard action limit");
    };
    check_with(&h, &block, BlockLimits::NU7).expect("within NU7 limits");
}

/// The fixture block of `h` with the coinbase changes `changes`, through the contextual
/// check.
fn check_coinbase(
    fixture: &hayai_bench::fixtures::Fixture,
    changes: &[CoinbaseChange],
) -> Result<hayai_state::Checked, ContextError> {
    let mut h = harness(fixture);
    h.block = fixture.with_coinbase(changes);
    check(&h, &prepared_block(&h))
}

/// The coinbase terms of the fixture height (Mainnet, NU6.2): the value rule is the
/// equality of ZIP 236, and the coinbase has the funding stream output.
#[test]
fn the_coinbase_pays_its_terms_exactly() {
    let fixture = mixed_block(3, 1, 2, 2);
    let terms = coinbase_terms(FIXTURE_HEIGHT);
    assert!(terms.exact_value);
    assert_eq!(terms.subsidy.deferred, 18_750_000);
    let [stream] = &terms.required[..] else {
        panic!("one funding stream output at the fixture height");
    };
    let kind = OutputKind::FundingStream(Receiver::MajorGrants);
    assert_eq!((stream.kind, stream.value), (kind, 12_500_000));
    check_coinbase(&fixture, &[]).expect("the fixture coinbase pays its terms");

    // One zatoshi more or less in the miner output: the value is not the exact value.
    for delta in [1i64, -1] {
        let change = CoinbaseChange::AddValue { output: 0, delta };
        let Err(ContextError::Coinbase(CoinbaseError::ValueNotExact { paid, required })) =
            check_coinbase(&fixture, &[change])
        else {
            panic!("a coinbase that pays {delta} more than its terms");
        };
        assert_eq!(paid, required + i128::from(delta));
    }
    // The deferred part of the subsidy goes to the deferred pool, not to the miner.
    let deferred = i64::try_from(terms.subsidy.deferred).expect("fits");
    let claim = CoinbaseChange::AddValue {
        output: 0,
        delta: deferred,
    };
    let Err(ContextError::Coinbase(CoinbaseError::ValueNotExact { paid, required })) =
        check_coinbase(&fixture, &[claim])
    else {
        panic!("the deferred part is not payable to the miner");
    };
    assert_eq!(paid, required + i128::from(deferred));

    // The funding stream output: missing, with a wrong value, with a wrong script. The
    // miner output takes the difference, so the total value stays exact.
    let stream_value = i64::try_from(stream.value).expect("fits");
    let missing = [
        CoinbaseChange::Remove { output: 1 },
        CoinbaseChange::AddValue {
            output: 0,
            delta: stream_value,
        },
    ];
    assert_eq!(
        check_coinbase(&fixture, &missing).map(|_| ()),
        Err(ContextError::Coinbase(CoinbaseError::MissingOutput {
            kind,
            value: stream.value,
            script: stream.script.clone(),
        }))
    );
    let wrong_value = [
        CoinbaseChange::AddValue {
            output: 1,
            delta: -1,
        },
        CoinbaseChange::AddValue {
            output: 0,
            delta: 1,
        },
    ];
    assert_eq!(
        check_coinbase(&fixture, &wrong_value).map(|_| ()),
        Err(ContextError::Coinbase(CoinbaseError::WrongAmount {
            kind,
            expected: stream.value,
            found: stream.value - 1,
        }))
    );
    let Err(ContextError::Coinbase(CoinbaseError::WrongScript {
        kind: found_kind,
        expected,
        ..
    })) = check_coinbase(&fixture, &[CoinbaseChange::ChangeScript { output: 1 }])
    else {
        panic!("a funding stream output to another script");
    };
    assert_eq!((found_kind, expected), (kind, stream.script.clone()));
}

/// The six chain value pools after a block: the transparent pool gains the coinbase and
/// loses what the block shields, the Orchard pool gains it, and the deferred pool gains the
/// deferred part of the subsidy.
#[test]
fn the_value_pools_follow_the_block() {
    let fixture = mixed_block(3, 1, 2, 2);
    let h = harness(&fixture);
    let funding: u64 = fixture.funding.iter().map(|(_, f)| f.value).sum();
    let before = h.chain.view().value_pools();
    assert_eq!(
        before,
        ValuePools {
            transparent: funding,
            ..ValuePools::default()
        }
    );
    let block = prepared_block(&h);
    let after = check(&h, &block).expect("valid").layer.value_pools;
    let shielded: u64 = block
        .txs
        .iter()
        .filter_map(|t| t.raw.tx.orchard_bundle())
        .map(|b| u64::try_from(-i64::from(*b.value_balance())).unwrap())
        .sum();
    assert!(shielded > 0);
    let terms = coinbase_terms(FIXTURE_HEIGHT);
    // The coinbase adds the subsidy without its deferred part. The fees move from the
    // inputs to the coinbase output inside the transparent pool.
    let paid_out = terms.subsidy.total - terms.subsidy.deferred;
    assert_eq!(
        after,
        ValuePools {
            transparent: funding + paid_out - shielded,
            sprout: 0,
            sapling: 0,
            orchard: shielded,
            ironwood: 0,
            deferred: terms.subsidy.deferred,
        }
    );
    // The total of the pools grows by the block subsidy.
    let total = |p: ValuePools| p.transparent + p.sapling + p.orchard + p.ironwood + p.deferred;
    assert_eq!(total(after), total(before) + terms.subsidy.total);

    // The rule reads the pool after the whole block. The coinbase of this block adds more
    // than its transactions shield, so the block is valid on a pool of zero too. The unit
    // tests of hayai-state have the block that makes the pool negative.
    assert!(paid_out > shielded);
    h.chain.base().write().value_pools.transparent = 0;
    let after = check(&h, &block).expect("valid").layer.value_pools;
    assert_eq!(after.transparent, paid_out - shielded);
}

/// T10, the Orchard soft fork: under the rule set of a height from the soft fork until the
/// NU6.2 activation, a transaction with an Orchard bundle is not valid. The rule set of the
/// block before the soft fork and the rule set of NU6.2 accept it.
#[test]
fn the_orchard_pool_is_off_in_the_soft_fork_range() {
    let with_orchard = orchard_block(2, 2);
    let transparent = transparent_block(5, 2);
    for (network, start) in [(Network::Mainnet, 3_363_426), (Network::Testnet, 4_048_500)] {
        let nu6_2 = network.activation_height(Upgrade::Nu6_2).expect("a height");
        let rules = |height: u32| rules_at(network, height).expect("a rule set");
        for (height, accepted) in [
            (start - 1, true),
            (start, false),
            (start + 1, false),
            (nu6_2 - 1, false),
            (nu6_2, true),
        ] {
            let h = harness(&with_orchard);
            let verdict = check_under(&h, &prepared_block(&h), rules(height)).map(|_| ());
            let expected = match accepted {
                true => Ok(()),
                // Transaction 0 is the coinbase. Transaction 1 is the first one with an
                // Orchard bundle.
                false => Err(ContextError::PoolNotActive {
                    pool: Pool::Orchard,
                    tx: 1,
                }),
            };
            assert_eq!(verdict, expected, "{network:?} {height}");
            // A block without an Orchard bundle is valid in the whole range.
            let h = harness(&transparent);
            check_under(&h, &prepared_block(&h), rules(height))
                .unwrap_or_else(|e| panic!("{network:?} {height}: {e}"));
        }
    }
}

#[test]
fn coinbase_placement_and_height() {
    let fixture = transparent_block(2, 1);
    let h = harness(&fixture);
    let mut block = prepared_block(&h);
    block.txs.swap(0, 1);
    block.raw.txs.swap(0, 1);
    let Err(ContextError::NoCoinbase) = check(&h, &block) else {
        panic!("coinbase must be first");
    };
    let mut block = prepared_block(&h);
    edit(&mut block, 2, |p| p.is_coinbase = true);
    let Err(ContextError::ExtraCoinbase(2)) = check(&h, &block) else {
        panic!("only the first transaction may be a coinbase");
    };
    let mut block = prepared_block(&h);
    block.txs[2] = block.txs[1].clone();
    block.raw.txs[2] = block.raw.txs[1].clone();
    let Err(ContextError::DuplicateTxid(_)) = check(&h, &block) else {
        panic!("duplicate txid");
    };

    // A chain one block further along, holding the same coins: the coinbase height
    // commitment no longer matches.
    let block = prepared_block(&h);
    let backing = h.chain.base().read().coins.backing().clone();
    let mut base = Base::new(
        backing,
        FIXTURE_HEIGHT,
        h.block.header.prev_hash,
        h.block.header.time - 75,
    );
    for (outpoint, funding) in &fixture.funding {
        base.coins
            .add(outpoint.clone(), hayai_bench::chain_fixture::coin(funding))
            .unwrap();
    }
    let chain = Chain::new(base);
    let Err(ContextError::CoinbaseHeight) = contextual_check(
        &chain.view(),
        &block,
        &CheckConfig {
            network: h.cfg.network,
            rules: &h.cfg.rules,
        },
    ) else {
        panic!("height in coinbase scriptSig must match");
    };
}
