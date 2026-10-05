//! Faithful port of Zakura's ZIP 317 block-production selection, the baseline for the
//! template benchmarks. It runs over [`hayai_template::Candidate`] so both sides see the same
//! input.
//!
//! Source: `zakura-rpc/src/methods/types/get_block_template/zip317.rs` (Zakura 9.x):
//!
//! - `select_mempool_transactions`, lines 83–140: partition candidates into independent and
//!   dependent (any unmined parent), split the independent ones by whether they pay the
//!   conventional fee, then draw from the conventional-fee list until it is empty and from the
//!   low-fee list until it is empty.
//! - `setup_fee_weighted_index`, lines 145–154: a `WeightedIndex<f32>` over every remaining
//!   candidate, rebuilt from scratch after each draw (`choose_transaction_weighted_random`,
//!   lines 440–451, which `swap_remove`s the pick and rebuilds).
//! - `checked_add_transaction_weighted_random`, lines 221–296: one draw, `try_add` against the
//!   limits, then a breadth-first pass over the pick's dependents; a dependent is added when
//!   `has_direct_dependencies` (lines 159–187, a linear scan of the selected list) holds, it
//!   pays the conventional fee, and it fits.
//! - `BlockTemplateLimits::try_add`, lines 397–424: bytes, sigops, unpaid actions and the
//!   ZIP 218 per-pool limits. The Ironwood, Sprout and global-budget counters are not
//!   represented in `Candidate` and are omitted; hayai applies the same four limits, so the
//!   comparison is like for like.
//!
//! Zakura draws from `thread_rng()`; the port takes the generator as a parameter so a run is
//! reproducible. The per-draw cost is identical.

use std::collections::{HashMap, HashSet};

use hayai_template::{Candidate, Zip317Params};
use hayai_wire::WtxId;
use rand::distributions::{Distribution, WeightedIndex};
use rand::Rng;

/// Remaining capacity, as `BlockTemplateLimits`.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub remaining_bytes: usize,
    pub remaining_sigops: u32,
    pub remaining_unpaid_actions: u32,
    pub remaining_orchard_actions: u32,
    pub remaining_sapling_ios: u32,
}

impl Limits {
    fn try_add(&mut self, tx: &Candidate, unpaid_actions: u32) -> bool {
        if tx.size_bytes() > self.remaining_bytes
            || tx.sigops > self.remaining_sigops
            || unpaid_actions > self.remaining_unpaid_actions
            || tx.orchard_actions > self.remaining_orchard_actions
            || tx.sapling_ios > self.remaining_sapling_ios
        {
            return false;
        }
        self.remaining_bytes -= tx.size_bytes();
        self.remaining_sigops -= tx.sigops;
        self.remaining_unpaid_actions -= unpaid_actions;
        self.remaining_orchard_actions -= tx.orchard_actions;
        self.remaining_sapling_ios -= tx.sapling_ios;
        true
    }
}

/// `select_mempool_transactions`.
pub fn select_mempool_transactions(
    mempool_txs: Vec<Candidate>,
    params: &Zip317Params,
    mut limits: Limits,
    rng: &mut impl Rng,
) -> Vec<Candidate> {
    // TransactionDependencies: direct dependencies and direct dependents.
    let tx_dependencies: HashMap<WtxId, HashSet<WtxId>> = mempool_txs
        .iter()
        .filter(|tx| !tx.depends_on.is_empty())
        .map(|tx| (tx.wtxid, tx.depends_on.iter().copied().collect()))
        .collect();
    let mut direct_dependents: HashMap<WtxId, HashSet<WtxId>> = HashMap::new();
    for tx in &mempool_txs {
        for parent in &tx.depends_on {
            direct_dependents
                .entry(*parent)
                .or_default()
                .insert(tx.wtxid);
        }
    }

    let (independent_mempool_txs, mut dependent_mempool_txs): (HashMap<_, _>, HashMap<_, _>) =
        mempool_txs
            .into_iter()
            .map(|tx| (tx.wtxid, tx))
            .partition(|(tx_id, _tx)| !tx_dependencies.contains_key(tx_id));

    let (mut conventional_fee_txs, mut low_fee_txs): (Vec<_>, Vec<_>) = independent_mempool_txs
        .into_values()
        .partition(pays_conventional_fee);

    let mut selected_txs = Vec::new();
    let deps = Deps {
        dependencies: &tx_dependencies,
        dependents: &direct_dependents,
    };

    let mut conventional_fee_tx_weights = setup_fee_weighted_index(&conventional_fee_txs);
    while let Some(tx_weights) = conventional_fee_tx_weights {
        conventional_fee_tx_weights = checked_add_transaction_weighted_random(
            &mut conventional_fee_txs,
            &mut dependent_mempool_txs,
            tx_weights,
            &mut selected_txs,
            &deps,
            &mut limits,
            params,
            rng,
        );
    }

    let mut low_fee_tx_weights = setup_fee_weighted_index(&low_fee_txs);
    while let Some(tx_weights) = low_fee_tx_weights {
        low_fee_tx_weights = checked_add_transaction_weighted_random(
            &mut low_fee_txs,
            &mut dependent_mempool_txs,
            tx_weights,
            &mut selected_txs,
            &deps,
            &mut limits,
            params,
            rng,
        );
    }

    selected_txs
}

struct Deps<'a> {
    dependencies: &'a HashMap<WtxId, HashSet<WtxId>>,
    dependents: &'a HashMap<WtxId, HashSet<WtxId>>,
}

fn pays_conventional_fee(tx: &Candidate) -> bool {
    tx.fee >= tx.conventional_fee
}

/// `setup_fee_weighted_index`: `None` when there are no transactions.
fn setup_fee_weighted_index(transactions: &[Candidate]) -> Option<WeightedIndex<f32>> {
    if transactions.is_empty() {
        return None;
    }
    // Zakura substitutes a minimum fee so that every weight is positive; the clamp has the
    // same effect for a zero-fee candidate.
    let tx_weights: Vec<f32> = transactions
        .iter()
        .map(|tx| (tx.weight_ratio.to_f64() as f32).max(f32::MIN_POSITIVE))
        .collect();
    WeightedIndex::new(tx_weights).ok()
}

/// `has_direct_dependencies`: every dependency is in `selected_txs`, by linear scan.
fn has_direct_dependencies(
    candidate_tx_deps: Option<&HashSet<WtxId>>,
    selected_txs: &[Candidate],
) -> bool {
    let Some(deps) = candidate_tx_deps else {
        return true;
    };
    if selected_txs.len() < deps.len() {
        return false;
    }
    let mut num_available_deps = 0;
    for tx in selected_txs {
        if deps.contains(&tx.wtxid) {
            num_available_deps += 1;
        } else {
            continue;
        }
        if num_available_deps == deps.len() {
            return true;
        }
    }
    false
}

/// `checked_add_transaction_weighted_random`.
#[allow(clippy::too_many_arguments)]
fn checked_add_transaction_weighted_random(
    candidate_txs: &mut Vec<Candidate>,
    dependent_txs: &mut HashMap<WtxId, Candidate>,
    tx_weights: WeightedIndex<f32>,
    selected_txs: &mut Vec<Candidate>,
    deps: &Deps<'_>,
    limits: &mut Limits,
    params: &Zip317Params,
    rng: &mut impl Rng,
) -> Option<WeightedIndex<f32>> {
    let (new_tx_weights, candidate_tx) =
        choose_transaction_weighted_random(candidate_txs, tx_weights, rng);

    let unpaid = params.unpaid_actions(candidate_tx.fee, candidate_tx.conventional_fee);
    if !limits.try_add(&candidate_tx, unpaid) {
        return new_tx_weights;
    }

    let selected_tx_id = candidate_tx.wtxid;
    selected_txs.push(candidate_tx);

    let mut current_level_dependents: HashSet<WtxId> = deps
        .dependents
        .get(&selected_tx_id)
        .cloned()
        .unwrap_or_default();
    while !current_level_dependents.is_empty() {
        let mut next_level_dependents = HashSet::new();
        for dependent_tx_id in &current_level_dependents {
            if has_direct_dependencies(deps.dependencies.get(dependent_tx_id), selected_txs) {
                let Some(candidate_tx) = dependent_txs.remove(dependent_tx_id) else {
                    continue;
                };
                if !pays_conventional_fee(&candidate_tx) {
                    continue;
                }
                let unpaid = params.unpaid_actions(candidate_tx.fee, candidate_tx.conventional_fee);
                if !limits.try_add(&candidate_tx, unpaid) {
                    continue;
                }
                selected_txs.push(candidate_tx);
                if let Some(next) = deps.dependents.get(dependent_tx_id) {
                    next_level_dependents.extend(next.iter().copied());
                }
            }
        }
        current_level_dependents = next_level_dependents;
    }

    new_tx_weights
}

/// `choose_transaction_weighted_random`: sample, `swap_remove`, rebuild the index.
fn choose_transaction_weighted_random(
    candidate_txs: &mut Vec<Candidate>,
    weighted_index: WeightedIndex<f32>,
    rng: &mut impl Rng,
) -> (Option<WeightedIndex<f32>>, Candidate) {
    let candidate_position = weighted_index.sample(rng);
    let candidate_tx = candidate_txs.swap_remove(candidate_position);
    (setup_fee_weighted_index(candidate_txs), candidate_tx)
}
