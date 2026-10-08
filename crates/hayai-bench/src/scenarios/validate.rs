//! Bodies of the validation benchmark (`benches/validate.rs`) and of the
//! `validate_block_{cold,warm}` sysbench scenarios: one full block validation per iteration
//! (parsed block in, layer out) on a chain seeded with the fixture's funding set.

use std::sync::mpsc;
use std::sync::Arc;

use hayai_coins::CoinsView;
use hayai_prepared::{draft, Draft, PrepareError};
use hayai_state::{contextual_check, CheckConfig, Layer, PreparedBlock};
use hayai_validate::validate_block;
use hayai_wire::RawBlock;

use super::{Built, Impl};
use crate::chain_fixture::{harness, Harness};
use hayai_fixtures::standard_set;

/// Zakura's shape of block verification, from the audit of its sources:
///
/// - the block verifier admits every transaction (`ready().await.call(..)` on a `Buffer`,
///   `zakura-consensus/src/block.rs:589-603`) and pushes the response futures into a
///   `FuturesUnordered` (`:556`, `:604`). The `Buffer` worker only builds the future
///   (`tower` `buffer/worker.rs:170-177`); the transaction body runs when the block task
///   polls `async_checks.next()` (`:615`). The body of one transaction does not wait for
///   the scripts of another;
/// - on that block task, per transaction: the spent-output lookups
///   (`zakura-consensus/src/transaction.rs:787-940`), `CachedFfiTransaction::new` (`:603`),
///   the sighash (`:1116`) and the creation of the script futures (`:1259-1267`). This work
///   is serial across transactions, because one task polls all the futures;
/// - each input's script runs in its own `rayon::spawn_fifo`
///   (`zakura-consensus/src/script.rs:71-76`, `primitives.rs:230`), started when the block
///   task polls that script future, and overlaps with the work of the block task on the
///   next transactions. Only the awaits of the transactions overlap;
/// - after the scripts of a transaction, its fee and sigops are summed on the block task
///   (`transaction.rs:672-675`); the model leaves them to the contextual stage;
/// - the spent outputs are read three times over the pipeline: verifier, contextual check
///   and finalization (`check/utxo.rs:53-71`);
/// - nothing is reused between the mempool and the block.
///
/// The model keeps this structure: the model thread does the lookups and the preparation
/// of each transaction in order, starts the scripts of the transaction on the rayon pool
/// without joining them, and joins all the scripts after the last transaction.
///
/// The contextual stage is hayai's: the comparison is about the per-transaction crypto
/// scheduling, which dominates a transparent block. It is a model built from hayai's
/// primitives, not Zakura's code: Zakura's verifier is a tower service graph that cannot be
/// driven as a library without its state service. Transparent blocks only.
pub fn zakura_model(block: &RawBlock, h: &Harness) -> Layer {
    transparent_model(block, h, 3)
}

/// Upstream Zebra's shape of block verification, built as [`zakura_model`] is, from
/// `zebra-consensus` 16.0.0 and `zebra-state` 14.0.0 (the releases on `zebra-chain` 13.0.1):
///
/// - the block verifier pushes one transaction verification future per transaction into a
///   `FuturesUnordered` and polls them on its task (`zebra-consensus/src/block.rs:314-346`),
///   as Zakura does (`zakura-consensus/src/block.rs:556`, `:589-615`); the `Buffer` of the
///   transaction verifier only builds the future, so the body runs on the block task;
/// - each transaction builds its `CachedFfiTransaction` (the sighash digests) on that task
///   (`zebra-consensus/src/transaction.rs:330-331`), and creates its script futures there
///   (`:337-344`); the work is serial across transactions;
/// - the transaction verifier awaits the spent output of each input one after the other
///   (`block_spent_utxos`, `transaction.rs:388-433`); Zakura overlaps up to 64 lookups per
///   transaction (`zakura-consensus/src/transaction.rs:69`, `:887`). The model reads an
///   in-memory view, where the overlap changes nothing, so both models read serially;
/// - each spent coin is read three times: verifier, contextual check
///   (`zebra-state/src/service/check/utxo.rs:161`) and finalization
///   (`zebra_db/block.rs:488-491`). Zebra has no `CheckParentInputs` round before the
///   verifier (Zakura: `zakura-consensus/src/block.rs:569-577`);
/// - each input's script is verified in its own `spawn_fifo` (`zebra-consensus/src/script.rs:61`),
///   as in Zakura. It starts when the block task polls the script future and overlaps with
///   the preparation of the next transactions, so the model does not join the scripts of
///   a transaction before it starts the next one;
/// - nothing is reused between the mempool and the block for transparent inputs.
///
/// The contextual stage is hayai's, as in [`zakura_model`]. Transparent blocks only.
pub fn zebra_model(block: &RawBlock, h: &Harness) -> Layer {
    transparent_model(block, h, 3)
}

/// The check of input `j` of transaction `t` of the block, run on the rayon pool.
type ScriptCheck = dyn Fn(usize, &Draft, usize) -> Result<(), PrepareError> + Send + Sync;

/// The steps of [`zakura_model`] and [`zebra_model`], with `read_rounds` reads of every
/// spent coin.
fn transparent_model(block: &RawBlock, h: &Harness, read_rounds: usize) -> Layer {
    run_model(block, h, read_rounds, Arc::new(|_, d, j| d.check_input(j)))
}

/// [`transparent_model`] with `check` in place of the script evaluation. The model thread
/// plays the block task: it prepares the transactions in order and starts every script on
/// the rayon pool at once (`spawn_fifo`, as `zakura-consensus/src/script.rs:71-76`). It
/// joins the scripts after the last transaction.
fn run_model(block: &RawBlock, h: &Harness, read_rounds: usize, check: Arc<ScriptCheck>) -> Layer {
    let view = h.chain.view();
    let (results_tx, results_rx) = mpsc::channel();
    let mut drafts: Vec<Arc<Draft>> = Vec::with_capacity(block.txs.len());
    for (t, raw) in block.txs.iter().enumerate() {
        let coins = match raw.tx.transparent_bundle() {
            Some(b) if !b.is_coinbase() => b
                .vin
                .iter()
                .map(|txin| {
                    let mut coin = None;
                    for _round in 0..read_rounds {
                        coin = view.get_coin(txin.prevout());
                    }
                    coin.expect("funded")
                })
                .collect(),
            _ => Vec::new(),
        };
        let d = Arc::new(draft(raw.clone(), h.cfg.epoch(), coins).expect("valid"));
        for j in 0..d.input_count() {
            let d = d.clone();
            let check = check.clone();
            let results_tx = results_tx.clone();
            rayon::spawn_fifo(move || {
                let result = check(t, &d, j);
                // The draft must be unshared once the receiver has every result.
                drop(d);
                results_tx.send(result).expect("receiver alive");
            });
        }
        drafts.push(d);
    }
    drop(results_tx);
    for result in results_rx {
        result.expect("valid script");
    }
    let txs = drafts
        .into_iter()
        .map(|d| {
            let Ok(d) = Arc::try_unwrap(d) else {
                unreachable!("every script task dropped its draft before it sent its result");
            };
            Arc::new(d.finish())
        })
        .collect();
    let prepared = PreparedBlock::new(block.clone(), txs);
    contextual_check(
        &view,
        &prepared,
        &CheckConfig {
            network: h.cfg.network,
            rules: &h.cfg.rules,
        },
    )
    .expect("valid")
    .layer
}

pub fn has_shielded(block: &RawBlock) -> bool {
    block.txs.iter().any(|t| {
        matches!(
            (t.tx.orchard_bundle(), t.tx.sapling_bundle()),
            (Some(_), _) | (_, Some(_))
        )
    })
}

/// `validate_block_cold` / `validate_block_warm` on the named fixture of the standard set.
/// The Zakura and Zebra models exist for cold validation of transparent fixtures only.
pub fn build(fixture: &str, warm: bool, imp: Impl) -> Result<Built, String> {
    let Some(fixture) = standard_set().into_iter().find(|f| f.name == fixture) else {
        return Err(format!("no fixture named {fixture}"));
    };
    if let (Impl::Zakura | Impl::Zebra, true) = (imp, warm || has_shielded(&fixture.parse())) {
        return Err(format!(
            "the {imp} model covers cold validation of transparent blocks only, not {} {}",
            fixture.name,
            if warm { "warm" } else { "cold" }
        ));
    }
    let h = harness(&fixture);
    if warm {
        h.fill_store();
    }
    let view = h.chain.view();
    Ok(match imp {
        Impl::Hayai => Built::new(move |m| {
            let height = m.timed(|| {
                validate_block(h.block.clone(), &h.store, &view, &h.cfg)
                    .expect("valid")
                    .0
                    .height
            });
            assert_eq!(height, fixture.height);
        }),
        Impl::Zakura => Built::new(move |m| {
            let height = m.timed(|| zakura_model(&h.block, &h).height);
            assert_eq!(height, fixture.height);
        }),
        Impl::Zebra => Built::new(move |m| {
            let height = m.timed(|| zebra_model(&h.block, &h).height);
            assert_eq!(height, fixture.height);
        }),
    })
}

// A property of the Zakura and Zebra models.
#[cfg(all(test, feature = "baselines"))]
mod tests {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::*;

    /// The scripts of a later transaction start while the scripts of an earlier one still
    /// run: the model does not join the scripts of a transaction before it prepares the
    /// next one, as the block task of Zakura and Zebra does not.
    #[test]
    fn scripts_of_the_next_transaction_start_before_the_previous_ones_finish() {
        let Some(fixture) = standard_set()
            .into_iter()
            .find(|f| f.name == "transparent-1000x2")
        else {
            panic!("standard set has transparent-1000x2");
        };
        let h = harness(&fixture);
        // (transaction index, start, end) of every script.
        let spans: Arc<Mutex<Vec<(usize, Instant, Instant)>>> = Arc::default();
        let recorder = spans.clone();
        let check: Arc<ScriptCheck> = Arc::new(move |t, d, j| {
            let start = Instant::now();
            if t == 1 {
                std::thread::sleep(Duration::from_millis(300));
            }
            let result = d.check_input(j);
            recorder
                .lock()
                .expect("not poisoned")
                .push((t, start, Instant::now()));
            result
        });
        run_model(&h.block, &h, 3, check);
        let spans = spans.lock().expect("not poisoned");
        let first_end = spans
            .iter()
            .filter(|(t, ..)| *t == 1)
            .map(|(_, _, end)| *end)
            .min()
            .expect("transaction 1 has scripts");
        let later_start = spans
            .iter()
            .filter(|(t, ..)| *t > 1)
            .map(|(_, start, _)| *start)
            .min()
            .expect("later transactions have scripts");
        assert!(
            later_start < first_end,
            "the scripts of transaction 2 or later start only after the scripts of transaction 1 end"
        );
        let expected = 2 * (fixture.parse().txs.len() - 1);
        assert_eq!(spans.len(), expected, "every input is checked once");
    }
}
