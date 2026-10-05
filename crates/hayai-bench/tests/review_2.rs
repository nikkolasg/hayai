//! Review findings against hayai-state contextual rules.

use std::sync::Arc;

use hayai_bench::chain_fixture::{harness, Harness};
use hayai_bench::fixtures::{transparent_block, FIXTURE_HEIGHT};
use hayai_state::{contextual_check, CheckConfig, PreparedBlock};

fn prepared_block(h: &Harness) -> PreparedBlock {
    let view = h.chain.view();
    let mut batch = hayai_prepared::ScopedBatch::new(&h.cfg.keys);
    let coinbase =
        hayai_prepared::prepare(h.block.txs[0].clone(), h.cfg.epoch(), &view, &mut batch).unwrap();
    let mut txs = vec![Arc::new(coinbase)];
    txs.extend(h.prepare_all());
    PreparedBlock::new(h.block.clone(), txs)
}

/// zcashd `Consensus::CheckTxInputs`: "bad-txns-coinbase-spend-has-transparent-outputs"
/// (`fCoinbaseMustBeShielded`, true on mainnet and testnet); Zebra
/// `CoinbaseSpendRestriction::DisallowCoinbaseSpend` /
/// `UnshieldedTransparentCoinbaseSpend`. A transaction spending a coinbase output must have
/// no transparent outputs, however mature the coin. hayai only checks maturity, and the
/// existing `immature_coinbase_spend_is_rejected` test asserts the opposite behaviour.
#[test]
fn mature_coinbase_spent_by_a_transaction_with_transparent_outputs_is_rejected() {
    let fixture = transparent_block(2, 1);
    let h = harness(&fixture);
    {
        let (outpoint, _) = &fixture.funding[1];
        let mut base = h.chain.base().write();
        let mut coin = base.coins.spend(outpoint).unwrap();
        coin.is_coinbase = true;
        coin.height = FIXTURE_HEIGHT - 1000;
        base.coins.add(outpoint.clone(), coin).unwrap();
    }
    let block = prepared_block(&h);
    let spender = &block.txs[2];
    assert!(
        !spender.raw.tx.transparent_bundle().unwrap().vout.is_empty(),
        "the fixture spender has transparent outputs"
    );
    let result = contextual_check(
        &h.chain.view(),
        &block,
        &CheckConfig {
            network: h.cfg.network,
            rules: &h.cfg.rules,
        },
    )
    .map(|_| ());
    let Err(_) = result else {
        panic!("a coinbase output spent by a transaction with transparent outputs is invalid");
    };
}
