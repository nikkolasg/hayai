//! Port of upstream Zebra's ZIP 317 block-production selection, the Zebra baseline for the
//! template benchmarks. It runs over [`hayai_template::Candidate`], as the Zakura port does.
//!
//! Source: `zebra-rpc` 18.0.0, `src/methods/types/get_block_template/zip317.rs`. The
//! selection after the coinbase step is the same code as Zakura's, line for line:
//!
//! - `select_mempool_transactions`, lines 93–176: the partition into independent and
//!   dependent candidates, the conventional-fee and low-fee lists, and the two draw loops
//!   (Zakura: `zakura-rpc/.../zip317.rs` lines 94–139).
//! - `setup_fee_weighted_index` (lines 201–210), `has_direct_dependencies` (215–243),
//!   `checked_add_transaction_weighted_random` (277–365) and
//!   `choose_transaction_weighted_random` (471–482): the same as Zakura's. A `WeightedIndex`
//!   is built again after each draw and the dependents get a breadth-first pass.
//! - The limits are bytes, sigops, unpaid actions (`try_update_block_template_limits`, lines
//!   428–465) and, from NU7, the ZIP 218 per-pool budget (`ShieldedBudget`, lines 377–411).
//!   Zakura applies the same limits through `BlockTemplateLimits::try_add`.
//!
//! So this port calls [`crate::zakura_zip317::select_mempool_transactions`] for that part
//! and adds the step where Zebra differs. Zebra sizes the coinbase with a fake coinbase
//! transaction (lines 65–137): it takes the zero-fee coinbase from a per-block
//! `CoinbaseCache` (a mutex and a clone of the cached `TransactionTemplate`,
//! `get_block_template.rs` lines 648–662), parses that coinbase into a `Transaction`
//! (lines 128–132), and charges its bytes, sigops and shielded actions to the limits.
//! Zakura removed the fake coinbase (Zakura PR #1035) and computes the coinbase resource
//! usage without a transaction (`TransactionTemplate::coinbase_resource_usage`).
//!
//! The port measures the cache hit: the template is built again at the same height, which is
//! what a mempool change causes (`precompute.rs` line 413 and `methods.rs` line 2888). The
//! first template at a new height also builds the coinbase
//! (`TransactionTemplate::new_coinbase_with_parent_pools`); the port does not include that
//! build, so it is a lower bound for Zebra. The cached template already holds the coinbase
//! sigop count, so the port charges no sigops for the coinbase. The synthetic candidates
//! never reach the sigop limit.

use std::sync::Mutex;

use hayai_template::{Candidate, Zip317Params};
use rand::Rng;
use zb_chain::serialization::ZcashDeserialize;
use zb_chain::transaction::Transaction;

use crate::zakura_zip317::{self, Limits};

/// Zebra's `CoinbaseCache` for one height, with the zero-fee coinbase as its only entry.
pub struct CoinbaseCache {
    zero_fee: Mutex<Vec<u8>>,
}

impl CoinbaseCache {
    /// A cache that holds `coinbase`, the serialized zero-fee coinbase transaction.
    pub fn new(coinbase: Vec<u8>) -> Self {
        Self {
            zero_fee: Mutex::new(coinbase),
        }
    }

    /// `CoinbaseCache::get`: lock, then clone the cached template.
    fn get(&self) -> Vec<u8> {
        self.zero_fee.lock().expect("cache lock").clone()
    }
}

/// `select_mempool_transactions` on a cache hit. `limits` holds the block budget without
/// the coinbase: the header and the transaction count are already removed.
pub fn select_mempool_transactions(
    mempool_txs: Vec<Candidate>,
    params: &Zip317Params,
    mut limits: Limits,
    cache: &CoinbaseCache,
    rng: &mut impl Rng,
) -> Vec<Candidate> {
    let fake_coinbase = cache.get();
    let coinbase =
        Transaction::zcash_deserialize(&fake_coinbase[..]).expect("cached coinbase parses");
    let orchard_actions =
        u32::try_from(coinbase.orchard_actions().count()).expect("action count fits u32");
    let sapling_ios =
        u32::try_from(coinbase.sapling_spends_count() + coinbase.sapling_outputs().count())
            .expect("sapling count fits u32");
    let Some(bytes) = limits.remaining_bytes.checked_sub(fake_coinbase.len()) else {
        return Vec::new();
    };
    let (Some(orchard), Some(sapling)) = (
        limits
            .remaining_orchard_actions
            .checked_sub(orchard_actions),
        limits.remaining_sapling_ios.checked_sub(sapling_ios),
    ) else {
        return Vec::new();
    };
    limits.remaining_bytes = bytes;
    limits.remaining_orchard_actions = orchard;
    limits.remaining_sapling_ios = sapling;
    zakura_zip317::select_mempool_transactions(mempool_txs, params, limits, rng)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::template::{candidates, coinbase_spec, zakura_limits, PARAMS};
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    /// The fake coinbase parses with zebra-chain and its bytes are charged to the block
    /// budget: the selection fills at most the budget less the coinbase, and a coinbase
    /// larger than the budget leaves the template empty.
    #[test]
    fn the_coinbase_is_charged_to_the_block_budget() {
        let coinbase = coinbase_spec().build(1, 0).unwrap().bytes.to_vec();
        let budget = zakura_limits(0).remaining_bytes;
        let cands = candidates(8_000, 7);
        let total: usize = cands.iter().map(|c| c.size_bytes()).sum();
        assert!(total > budget, "the candidates must exceed one block");

        let cache = CoinbaseCache::new(coinbase.clone());
        let mut rng = StdRng::seed_from_u64(3);
        let selected =
            select_mempool_transactions(cands.clone(), &PARAMS, zakura_limits(0), &cache, &mut rng);
        let used: usize = selected.iter().map(|c| c.size_bytes()).sum();
        assert!(!selected.is_empty());
        assert!(
            used <= budget - coinbase.len(),
            "{used} > {budget} - {}",
            coinbase.len()
        );

        let mut limits = zakura_limits(0);
        limits.remaining_bytes = coinbase.len() - 1;
        let None = select_mempool_transactions(cands, &PARAMS, limits, &cache, &mut rng).first()
        else {
            panic!("a coinbase larger than the budget leaves no room");
        };
    }
}
