//! The state update of a checkpointed block.
//!
//! A block at or below the last checkpoint, whose header is on the checkpointed chain, is
//! valid by its hash (Zebra and Zakura commit such a block without their contextual
//! rules). [`checkpoint_layer`] reads the transactions as the wire parser gives them and
//! builds the layer that [`super::contextual_check`] builds for the same block.
//!
//! The function keeps the checks that guard the state:
//!
//! - the parent is the tip of the view;
//! - each transparent input spends a coin that exists, and no outpoint is spent twice in
//!   the block;
//! - no nullifier is revealed twice in the block;
//! - no chain value pool is negative after the block;
//! - the header commits to the history tree of the parent, and from NU5 to the
//!   authorizing data of the block.
//!
//! The function does not apply these rules, because the checkpoint hash fixes the block:
//! the coinbase rules and terms, coinbase maturity, the order of a parent and its child in
//! the block, nullifiers against the earlier blocks, anchors, expiry, lock time, the pools
//! of the height and the block limits.

use std::time::Instant;

use hayai_coins::Pool;
use hayai_crypto::orchard::tree::MerkleHashOrchard;
use hayai_crypto::sapling_crypto::Node;
use hayai_prepared::Commitments;
use hayai_wire::RawBlock;

use super::{
    append_leaves, block_outputs, block_pools_after, check_history, check_parent, checked_sum,
    coinbase_terms, resolve_inputs, sprout_balance, CheckConfig, Checked, ContextError,
    ContextTimings, Totals,
};
use crate::{ChainView, Frontiers, Layer, Set};

/// Builds the layer of the checkpointed block `raw` on top of `view`. `auth_data_root` is
/// the ZIP 244 root of the authorizing data of `raw`.
///
/// The caller must know that the header of `raw` is on the checkpointed chain, that the
/// merkle root of the header matches the transactions, and that no txid is in the block
/// twice (`hayai_validate::apply_checkpointed` checks the three).
pub fn checkpoint_layer(
    view: &ChainView,
    raw: &RawBlock,
    auth_data_root: &[u8; 32],
    cfg: &CheckConfig<'_>,
) -> Result<Checked, ContextError> {
    let started = Instant::now();
    let height = check_parent(view, raw)?;
    let created = block_outputs(raw, height);
    let inputs = resolve_inputs(view, raw, &created)?;

    let input_count = inputs.iter().map(Vec::len).sum();
    let mut spent = Set::with_capacity_and_hasher(input_count, Default::default());
    let mut nullifiers: [Set<[u8; 32]>; 4] = Default::default();
    let mut leaves = Commitments::default();
    let mut totals = Totals::default();
    for (i, (raw_tx, coins)) in raw.txs.iter().zip(&inputs).enumerate() {
        let tx = &raw_tx.tx;
        let mut reveal =
            |pool: Pool, nullifier: [u8; 32]| match nullifiers[pool.index()].insert(nullifier) {
                true => Ok(()),
                false => Err(ContextError::DuplicateNullifier { pool, tx: i }),
            };
        if let Some(bundle) = tx.transparent_bundle() {
            if !bundle.is_coinbase() {
                for (j, txin) in bundle.vin.iter().enumerate() {
                    if !spent.insert(txin.prevout().clone()) {
                        return Err(ContextError::DoubleSpend { tx: i, input: j });
                    }
                }
            }
            let created_value = checked_sum(bundle.vout.iter().map(|o| o.value().into_u64()))?;
            let spent_value: i128 = coins.iter().map(|coin| i128::from(coin.value)).sum();
            totals.transparent_change += i128::from(created_value) - spent_value;
        }
        // The Sprout state update: the nullifiers, the note commitments and the value of
        // each JoinSplit of `tx`. No proof and no signature is read, so a JoinSplit with a
        // BCTV14 proof has the same update as one with a Groth16 proof.
        if let Some(bundle) = tx.sprout_bundle() {
            if !view.sprout_known() {
                return Err(ContextError::SproutStateUnknown { tx: i });
            }
            for joinsplit in &bundle.joinsplits {
                for nullifier in joinsplit.nullifiers() {
                    reveal(Pool::Sprout, *nullifier)?;
                }
                leaves.sprout.extend_from_slice(joinsplit.commitments());
            }
            totals.sprout_balance += sprout_balance(bundle);
        }
        if let Some(bundle) = tx.sapling_bundle() {
            for spend in bundle.shielded_spends() {
                reveal(Pool::Sapling, spend.nullifier().0)?;
            }
            let outputs = bundle.shielded_outputs();
            leaves
                .sapling
                .extend(outputs.iter().map(|o| Node::from_cmu(o.cmu())));
            totals.sapling_balance += i128::from(i64::from(*bundle.value_balance()));
        }
        // The Orchard and the Ironwood bundle have one form.
        for (bundle, pool) in [
            (tx.orchard_bundle(), Pool::Orchard),
            (tx.ironwood_bundle(), Pool::Ironwood),
        ] {
            let Some(bundle) = bundle else {
                continue;
            };
            let (tree, balance) = match pool {
                Pool::Orchard => (&mut leaves.orchard, &mut totals.orchard_balance),
                Pool::Ironwood => (&mut leaves.ironwood, &mut totals.ironwood_balance),
                Pool::Sprout | Pool::Sapling => unreachable!("the loop names two pools"),
            };
            for action in bundle.actions() {
                reveal(pool, action.nullifier().to_bytes())?;
                tree.push(MerkleHashOrchard::from_cmx(action.cmx()));
            }
            *balance += i128::from(i64::from(*bundle.value_balance()));
        }
    }
    let terms = coinbase_terms(cfg, height, view.value_pools())?;
    let value_pools = block_pools_after(cfg, height, view.value_pools(), &totals, &terms)?;
    let context = started.elapsed();

    let trees_started = Instant::now();
    let Frontiers {
        orchard: orchard_frontier,
        sapling: sapling_frontier,
        ironwood: ironwood_frontier,
        sprout: sprout_frontier,
        anchors,
    } = append_leaves(view, &leaves)?;
    let trees = trees_started.elapsed();

    let history_started = Instant::now();
    let history = check_history(
        view,
        raw,
        height,
        cfg.rules.branch_id,
        auth_data_root,
        &anchors,
    )?;
    let history_took = history_started.elapsed();

    let layer = Layer {
        height,
        hash: raw.hash(),
        parent: raw.header.prev_hash,
        time: raw.header.time,
        bits: raw.header.bits,
        wtxids: raw.txs.iter().map(|t| t.wtxid()).collect(),
        created,
        spent,
        spent_coins: inputs,
        nullifiers,
        orchard_frontier,
        sapling_frontier,
        ironwood_frontier,
        sprout_frontier,
        anchors,
        value_pools,
        history,
    };
    Ok(Checked {
        layer,
        timings: ContextTimings {
            context,
            trees,
            history: history_took,
        },
    })
}
