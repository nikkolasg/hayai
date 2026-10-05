//! The prepared-transaction store.
//!
//! Reads (`get`, [`hayai_wire::TxLookup`]) go straight to the concurrent maps; every mutation
//! runs under one writer lock so that conflict detection, eviction and the event stream see a
//! consistent sequence. Mutations are mempool-rate events, reads are block-rate events.
//!
//! Conflicts. Two stored transactions never spend the same outpoint or reveal the same
//! nullifier, the two per-block uniqueness rules of the contextual check, so any subset of
//! the store is a consistent block body.
//!
//! Eviction (ZIP 401). Each transaction has a cost, `max(serialized size, 10,000)`, and an
//! eviction weight, the cost plus 40,000 when the fee is below the ZIP 317 conventional
//! fee. The total cost of the store is at most the cost limit. When an insert would exceed
//! the limit, the store selects one transaction at random, with a probability in
//! proportion to its eviction weight, and evicts it together with its unmined descendants.
//! The store does this again until the new transaction fits. The new transaction is a
//! candidate of the selection: when the store selects it, the insert fails with
//! [`InsertError::Evicted`]. The ancestors of the new transaction are not candidates: their
//! eviction removes the new transaction too. The victim search is a linear scan in
//! insertion order, so that a seeded random number generator gives a reproducible result.
//!
//! Recently evicted transactions. The store keeps the txid (not the wtxid) of each
//! selected victim for [`EVICTION_MEMORY`], at most [`EVICTION_MEMORY_ENTRIES`] of them,
//! and refuses a transaction with such a txid ([`InsertError::RecentlyEvicted`]).
//!
//! Expiry (ZIP 203). [`PreparedStore::remove_expired`] removes the transactions that the
//! next block cannot contain.
//!
//! Removal of a parent (eviction, expiry, conflict, epoch change) removes its descendants,
//! because their inputs are no longer spendable in any block. Removal of a mined
//! transaction does not: its outputs are now in the chain.
//!
//! The store has no replacement rule: Zcash has no replace-by-fee.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use dashmap::DashMap;
use hayai_coins::{OutPoint, Pool};
use hayai_crypto::zcash_primitives;
use hayai_template::{Candidate, CandidateSource, SetEvent, WeightRatio, Zip317Params};
use hayai_wire::{RawTx, TxLookup, WtxId};
use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{Rng, RngCore, SeedableRng};
use zcash_primitives::transaction::TxId;

use crate::policy::is_relayable;
use crate::{PreparedTx, RuleEpoch};

/// ZIP 401 `mempooltxcostlimit`: the default limit of the total cost of the store.
pub const MEMPOOL_TX_COST_LIMIT: usize = 80_000_000;
/// ZIP 401: the cost of a transaction is its serialized size or this value, whichever is
/// larger.
pub const MEMPOOL_COST_THRESHOLD: u64 = 10_000;
/// ZIP 401 `low_fee_penalty`: added to the eviction weight of a transaction that pays less
/// than the ZIP 317 conventional fee.
pub const LOW_FEE_PENALTY: u64 = 40_000;
/// ZIP 401 `mempoolevictionmemoryminutes`: the time for which the store refuses an evicted
/// transaction.
pub const EVICTION_MEMORY: Duration = Duration::from_secs(60 * 60);
/// ZIP 401 `eviction_memory_entries`: the largest number of evicted txids the store keeps.
pub const EVICTION_MEMORY_ENTRIES: usize = 40_000;

/// The ZIP 317 and ZIP 401 values of a stored transaction, computed once at insert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryFees {
    /// Fee in zatoshis.
    pub fee: u64,
    /// ZIP 317 conventional fee in zatoshis.
    pub conventional_fee: u64,
    /// ZIP 317 unpaid actions.
    pub unpaid_actions: u32,
    /// `min(fee / conventional_fee, cap)`.
    pub weight_ratio: WeightRatio,
    /// ZIP 401 cost.
    pub cost: u64,
    /// ZIP 401 eviction weight.
    pub eviction_weight: u64,
}

struct Entry {
    tx: Arc<PreparedTx>,
    /// Insertion order: the order of the eviction scan.
    seq: u64,
    /// The template candidate, with the ZIP 317 values. Its `depends_on` is the parent
    /// list at insert.
    candidate: Candidate,
    fees: EntryFees,
}

/// The txids of the recently evicted transactions, oldest first.
struct RecentlyEvicted {
    order: VecDeque<(TxId, Instant)>,
    ids: HashSet<TxId>,
    capacity: usize,
}

impl RecentlyEvicted {
    fn new(capacity: usize) -> Self {
        Self {
            order: VecDeque::new(),
            ids: HashSet::new(),
            capacity,
        }
    }

    /// Removes the entries older than [`EVICTION_MEMORY`] at `now`.
    fn prune(&mut self, now: Instant) {
        while let Some((txid, at)) = self.order.front() {
            if now.saturating_duration_since(*at) <= EVICTION_MEMORY {
                break;
            }
            self.ids.remove(txid);
            self.order.pop_front();
        }
    }

    fn contains(&mut self, txid: &TxId, now: Instant) -> bool {
        self.prune(now);
        self.ids.contains(txid)
    }

    /// Records an eviction at `now`. The oldest entry leaves a full list.
    fn add(&mut self, txid: TxId, now: Instant) {
        self.prune(now);
        if !self.ids.insert(txid) {
            return;
        }
        if self.order.len() >= self.capacity {
            if let Some((oldest, _)) = self.order.pop_front() {
                self.ids.remove(&oldest);
            }
        }
        self.order.push_back((txid, now));
    }
}

struct Writer {
    epoch: RuleEpoch,
    seq: u64,
    /// Wire bytes held.
    bytes: usize,
    /// ZIP 401 total cost.
    cost: u64,
    evicted: RecentlyEvicted,
    rng: Box<dyn RngCore + Send>,
    subscribers: Vec<Sender<SetEvent>>,
}

impl Writer {
    fn emit(&mut self, event: SetEvent) {
        self.subscribers
            .retain(|s| matches!(s.send(event.clone()), Ok(())));
    }
}

pub struct PreparedStore {
    txs: DashMap<WtxId, Entry, ahash::RandomState>,
    by_txid: DashMap<TxId, WtxId, ahash::RandomState>,
    spent_index: DashMap<OutPoint, WtxId, ahash::RandomState>,
    nullifier_index: DashMap<(Pool, [u8; 32]), WtxId, ahash::RandomState>,
    writer: Mutex<Writer>,
    params: Zip317Params,
    cost_limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InsertError {
    #[error("transaction {0:?} is already stored")]
    Duplicate(WtxId),
    #[error("outpoint {outpoint:?} is already spent by {by:?}")]
    Conflict { outpoint: OutPoint, by: WtxId },
    #[error("{pool} nullifier {nullifier:?} is already revealed by {by:?}")]
    NullifierConflict {
        pool: Pool,
        nullifier: [u8; 32],
        by: WtxId,
    },
    #[error("transaction scripts or shielded bundles are not verified")]
    Unverified,
    #[error("transaction epoch {tx:?} differs from the store epoch {store:?}")]
    Epoch { tx: RuleEpoch, store: RuleEpoch },
    #[error("a coinbase is never stored")]
    Coinbase,
    #[error("the store is at its cost limit and the ZIP 401 eviction selected the transaction")]
    Evicted,
    #[error("transaction {0:?} was evicted less than 60 min ago")]
    RecentlyEvicted(TxId),
}

impl PreparedStore {
    /// An empty store that accepts transactions of `epoch`, holds a total ZIP 401 cost of
    /// at most `cost_limit` ([`MEMPOOL_TX_COST_LIMIT`] on a node) and computes the ZIP 317
    /// values with `params`. The eviction uses a random number generator seeded by the
    /// operating system.
    pub fn new(epoch: RuleEpoch, cost_limit: usize, params: Zip317Params) -> Self {
        Self::with_rng(epoch, cost_limit, params, Box::new(StdRng::from_entropy()))
    }

    /// [`PreparedStore::new`] with the random number generator of the eviction.
    pub fn with_rng(
        epoch: RuleEpoch,
        cost_limit: usize,
        params: Zip317Params,
        rng: Box<dyn RngCore + Send>,
    ) -> Self {
        Self {
            txs: DashMap::default(),
            by_txid: DashMap::default(),
            spent_index: DashMap::default(),
            nullifier_index: DashMap::default(),
            writer: Mutex::new(Writer {
                epoch,
                seq: 0,
                bytes: 0,
                cost: 0,
                evicted: RecentlyEvicted::new(EVICTION_MEMORY_ENTRIES),
                rng,
                subscribers: Vec::new(),
            }),
            params,
            cost_limit: cost_limit as u64,
        }
    }

    pub fn get(&self, id: &WtxId) -> Option<Arc<PreparedTx>> {
        self.txs.get(id).map(|e| e.tx.clone())
    }

    pub fn get_by_txid(&self, txid: &TxId) -> Option<Arc<PreparedTx>> {
        let id = *self.by_txid.get(txid)?;
        self.get(&id)
    }

    /// The stored transaction spending `outpoint`, if any.
    pub fn spender(&self, outpoint: &OutPoint) -> Option<WtxId> {
        self.spent_index.get(outpoint).map(|e| *e)
    }

    /// The stored transaction revealing `nullifier` in `pool`, if any.
    pub fn revealer(&self, pool: Pool, nullifier: &[u8; 32]) -> Option<WtxId> {
        self.nullifier_index.get(&(pool, *nullifier)).map(|e| *e)
    }

    pub fn epoch(&self) -> RuleEpoch {
        self.writer.lock().epoch
    }

    /// Wire bytes held.
    pub fn cost_bytes(&self) -> usize {
        self.writer.lock().bytes
    }

    /// ZIP 401 total cost of the transactions held.
    pub fn total_cost(&self) -> u64 {
        self.writer.lock().cost
    }

    /// The ZIP 317 and ZIP 401 values of a stored transaction.
    pub fn fees(&self, id: &WtxId) -> Option<EntryFees> {
        self.txs.get(id).map(|e| e.fees)
    }

    /// Whether the store evicted a transaction with `txid` less than [`EVICTION_MEMORY`]
    /// ago. The node asks before it downloads or prepares a transaction.
    pub fn is_recently_evicted(&self, txid: &TxId) -> bool {
        self.writer.lock().evicted.contains(txid, Instant::now())
    }

    /// The transactions to announce to a peer (the answer to a `mempool` message), when the
    /// next block has height `next_height`. A transaction that expires soon is not in the
    /// list, as in zcashd.
    pub fn relay_ids(&self, next_height: u32) -> Vec<WtxId> {
        self.txs
            .iter()
            .filter(|e| is_relayable(e.tx.expiry_height, next_height))
            .map(|e| *e.key())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.txs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.txs.is_empty()
    }

    /// Unmined parents of `tx`: stored transactions whose outputs it spends.
    fn parents(&self, tx: &PreparedTx) -> Vec<WtxId> {
        let mut parents = Vec::new();
        for outpoint in tx.spent_outpoints() {
            let Some(parent) = self.by_txid.get(&TxId::from_bytes(*outpoint.hash())) else {
                continue;
            };
            if !parents.contains(&*parent) {
                parents.push(*parent);
            }
        }
        parents
    }

    /// Every stored transaction `tx` depends on, directly or through other stored ones.
    fn ancestors(&self, tx: &PreparedTx) -> Vec<WtxId> {
        let mut found = self.parents(tx);
        let mut next = 0;
        while let Some(id) = found.get(next).copied() {
            next += 1;
            let Some(entry) = self.txs.get(&id) else {
                continue;
            };
            for parent in self.parents(&entry.tx) {
                if !found.contains(&parent) {
                    found.push(parent);
                }
            }
        }
        found
    }

    /// Stores a fully verified transaction and announces it.
    pub fn insert(&self, tx: Arc<PreparedTx>) -> Result<(), InsertError> {
        self.insert_at(tx, Instant::now())
    }

    /// [`PreparedStore::insert`] at the time `now`, the clock of the recently-evicted list.
    pub fn insert_at(&self, tx: Arc<PreparedTx>, now: Instant) -> Result<(), InsertError> {
        let mut w = self.writer.lock();
        if tx.epoch != w.epoch {
            return Err(InsertError::Epoch {
                tx: tx.epoch,
                store: w.epoch,
            });
        }
        if !(tx.scripts_ok && tx.shielded_ok) {
            return Err(InsertError::Unverified);
        }
        if tx.is_coinbase {
            return Err(InsertError::Coinbase);
        }
        let id = tx.wtxid();
        if self.txs.contains_key(&id) {
            return Err(InsertError::Duplicate(id));
        }
        if w.evicted.contains(&tx.raw.txid, now) {
            return Err(InsertError::RecentlyEvicted(tx.raw.txid));
        }
        for outpoint in tx.spent_outpoints() {
            if let Some(by) = self.spent_index.get(outpoint) {
                return Err(InsertError::Conflict {
                    outpoint: outpoint.clone(),
                    by: *by,
                });
            }
        }
        for (pool, nullifier) in &tx.nullifiers {
            if let Some(by) = self.nullifier_index.get(&(*pool, *nullifier)) {
                return Err(InsertError::NullifierConflict {
                    pool: *pool,
                    nullifier: *nullifier,
                    by: *by,
                });
            }
        }
        let candidate = tx.candidate(self.parents(&tx), &self.params);
        let cost = (tx.raw.bytes.len() as u64).max(MEMPOOL_COST_THRESHOLD);
        let low_fee = tx.fee < candidate.conventional_fee;
        let fees = EntryFees {
            fee: tx.fee,
            conventional_fee: candidate.conventional_fee,
            unpaid_actions: self
                .params
                .unpaid_actions(tx.fee, candidate.conventional_fee),
            weight_ratio: candidate.weight_ratio,
            cost,
            eviction_weight: cost + if low_fee { LOW_FEE_PENALTY } else { 0 },
        };
        let protected = self.ancestors(&tx);
        while w.cost + cost > self.cost_limit {
            let Some((victim, txid)) = self.select_victim(&mut w, &protected, fees.eviction_weight)
            else {
                w.evicted.add(tx.raw.txid, now);
                return Err(InsertError::Evicted);
            };
            self.remove_entry(&mut w, &victim, true);
            w.evicted.add(txid, now);
        }
        w.seq += 1;
        w.bytes += tx.raw.bytes.len();
        w.cost += cost;
        for outpoint in tx.spent_outpoints() {
            self.spent_index.insert(outpoint.clone(), id);
        }
        for (pool, nullifier) in &tx.nullifiers {
            self.nullifier_index.insert((*pool, *nullifier), id);
        }
        self.by_txid.insert(tx.raw.txid, id);
        self.txs.insert(
            id,
            Entry {
                tx,
                seq: w.seq,
                candidate: candidate.clone(),
                fees,
            },
        );
        w.emit(SetEvent::Added(candidate));
        Ok(())
    }

    /// ZIP 401 `EvictTransaction`: selects one transaction at random, with a probability in
    /// proportion to its eviction weight. The candidates are the stored transactions
    /// outside `exclude` and the transaction being inserted, which has the weight
    /// `new_weight`. `None`: the selection is the transaction being inserted.
    fn select_victim(
        &self,
        w: &mut Writer,
        exclude: &[WtxId],
        new_weight: u64,
    ) -> Option<(WtxId, TxId)> {
        let mut candidates: Vec<(u64, WtxId, TxId, u64)> = self
            .txs
            .iter()
            .filter(|e| !exclude.contains(e.key()))
            .map(|e| (e.seq, *e.key(), e.tx.raw.txid, e.fees.eviction_weight))
            .collect();
        candidates.sort_unstable_by_key(|(seq, ..)| *seq);
        let total: u64 = candidates.iter().map(|(.., weight)| weight).sum::<u64>() + new_weight;
        let mut point = w.rng.gen_range(0..total);
        for (_, id, txid, weight) in candidates {
            if point < weight {
                return Some((id, txid));
            }
            point -= weight;
        }
        None
    }

    /// Removes one entry, its index records and (with `cascade`) its unmined descendants.
    fn remove_entry(&self, w: &mut Writer, id: &WtxId, cascade: bool) {
        let Some((_, entry)) = self.txs.remove(id) else {
            return;
        };
        let tx = &entry.tx;
        w.bytes -= tx.raw.bytes.len();
        w.cost -= entry.fees.cost;
        self.by_txid
            .remove_if(&tx.raw.txid, |_, stored| stored == id);
        for outpoint in tx.spent_outpoints() {
            self.spent_index.remove_if(outpoint, |_, by| by == id);
        }
        for (pool, nullifier) in &tx.nullifiers {
            self.nullifier_index
                .remove_if(&(*pool, *nullifier), |_, by| by == id);
        }
        w.emit(SetEvent::Removed(*id));
        if !cascade {
            return;
        }
        let outputs = tx.raw.tx.transparent_bundle().map_or(0, |b| b.vout.len());
        for n in 0..outputs {
            let child = OutPoint::new(*tx.raw.txid.as_ref(), n as u32);
            let Some(child) = self.spent_index.get(&child).map(|c| *c) else {
                continue;
            };
            self.remove_entry(w, &child, true);
        }
    }

    /// Drops transactions that were mined; their descendants stay. Returns how many were
    /// held.
    pub fn remove_mined(&self, ids: &[WtxId]) -> usize {
        let mut w = self.writer.lock();
        let mut removed = 0;
        for id in ids {
            if self.txs.contains_key(id) {
                self.remove_entry(&mut w, id, false);
                removed += 1;
            }
        }
        removed
    }

    /// Drops every transaction spending one of `outpoints` or revealing one of `nullifiers`
    /// (both spent by a new tip block: the node passes the layer's `spent` set and
    /// `nullifiers` sets after [`PreparedStore::remove_mined`]) and their descendants.
    /// Returns the ids dropped.
    pub fn remove_conflicting(
        &self,
        outpoints: &[OutPoint],
        nullifiers: &[(Pool, [u8; 32])],
    ) -> Vec<WtxId> {
        let mut w = self.writer.lock();
        let mut dropped = Vec::new();
        let by_outpoint = outpoints
            .iter()
            .filter_map(|o| self.spent_index.get(o).map(|e| *e));
        let by_nullifier = nullifiers
            .iter()
            .filter_map(|nf| self.nullifier_index.get(nf).map(|e| *e));
        let conflicting: Vec<WtxId> = by_outpoint.chain(by_nullifier).collect();
        for id in conflicting {
            let before = self.txs.len();
            self.remove_entry(&mut w, &id, true);
            if self.txs.len() < before {
                dropped.push(id);
            }
        }
        dropped
    }

    /// The transactions that a block of height `next_height` cannot contain (ZIP 203: the
    /// block height is above the expiry height).
    pub fn expired(&self, next_height: u32) -> Vec<WtxId> {
        self.txs
            .iter()
            .filter(|e| e.tx.expiry_height != 0 && next_height > e.tx.expiry_height)
            .map(|e| *e.key())
            .collect()
    }

    /// Drops every transaction that a block of height `next_height` cannot contain (ZIP 203:
    /// the block height is above the expiry height) and their descendants. The node calls
    /// it for each new tip with the tip height plus 1. Returns the expired ids dropped.
    pub fn remove_expired(&self, next_height: u32) -> Vec<WtxId> {
        let mut w = self.writer.lock();
        let expired = self.expired(next_height);
        for id in &expired {
            self.remove_entry(&mut w, id, true);
        }
        expired
    }

    /// Switches the store to `epoch`, dropping every transaction prepared under another one
    /// with its descendants. Returns the dropped transactions.
    pub fn set_epoch(&self, epoch: RuleEpoch) -> Vec<WtxId> {
        let mut w = self.writer.lock();
        if w.epoch == epoch {
            return Vec::new();
        }
        w.epoch = epoch;
        let stale: Vec<WtxId> = self
            .txs
            .iter()
            .filter(|e| e.tx.epoch != epoch)
            .map(|e| *e.key())
            .collect();
        // A descendant of a transaction of another epoch is of that epoch too.
        for id in &stale {
            self.remove_entry(&mut w, id, true);
        }
        stale
    }
}

impl TxLookup for PreparedStore {
    fn get(&self, id: &WtxId) -> Option<Arc<RawTx>> {
        self.txs.get(id).map(|e| e.tx.raw.clone())
    }

    fn for_each_id(&self, f: &mut dyn FnMut(&WtxId)) {
        for entry in self.txs.iter() {
            f(entry.key());
        }
    }

    fn len(&self) -> usize {
        self.txs.len()
    }
}

impl CandidateSource for PreparedStore {
    fn candidates(&self) -> Vec<Candidate> {
        let _w = self.writer.lock();
        self.txs
            .iter()
            .map(|e| {
                let mut candidate = e.candidate.clone();
                candidate.depends_on = self.parents(&e.tx);
                candidate
            })
            .collect()
    }

    fn events(&self) -> Receiver<SetEvent> {
        let (tx, rx) = crossbeam_channel::unbounded();
        self.writer.lock().subscribers.push(tx);
        rx
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::test_support::{p2pkh, prepared, TxSpec, BRANCH, P2PKH_SIG};

    /// The fee of [`TxSpec::paying`] that equals the conventional fee (2 grace actions).
    const CONVENTIONAL: u64 = 800;

    fn store(entries: usize, seed: u64) -> PreparedStore {
        PreparedStore::with_rng(
            RuleEpoch::consensus(BRANCH),
            entries * MEMPOOL_COST_THRESHOLD as usize,
            Zip317Params::ZAKURA,
            Box::new(StdRng::seed_from_u64(seed)),
        )
    }

    fn paying(id: u32, fee: u64) -> Arc<PreparedTx> {
        Arc::new(prepared(&TxSpec::paying(id, fee)))
    }

    /// A transaction that spends output 0 of `parent`.
    fn child_of(parent: &PreparedTx, expiry: u32) -> Arc<PreparedTx> {
        Arc::new(prepared(&TxSpec {
            inputs: vec![(P2PKH_SIG.to_vec(), p2pkh(9), 50_000)],
            outputs: vec![(p2pkh(8), 40_000)],
            spends: vec![OutPoint::new(*parent.raw.txid.as_ref(), 0)],
            expiry,
            ..TxSpec::standard()
        }))
    }

    /// A transaction of at least `size` bytes.
    fn sized(id: u32, size: usize) -> Arc<PreparedTx> {
        Arc::new(prepared(&TxSpec {
            outputs: vec![(vec![0x51; size], 50_000)],
            id,
            ..TxSpec::standard()
        }))
    }

    fn holds(store: &PreparedStore, tx: &PreparedTx) -> bool {
        store.txs.contains_key(&tx.wtxid())
    }

    fn removed(events: &Receiver<SetEvent>) -> Vec<WtxId> {
        std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                SetEvent::Removed(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_entry_holds_the_zip_317_and_zip_401_values() {
        let store = store(10, 0);
        let paid = paying(1, CONVENTIONAL);
        let low = paying(2, CONVENTIONAL - 1);
        let large = sized(3, 25_000);
        for tx in [&paid, &low, &large] {
            store.insert(tx.clone()).unwrap();
        }
        let fees = store.fees(&paid.wtxid()).unwrap();
        assert_eq!(
            fees,
            EntryFees {
                fee: CONVENTIONAL,
                conventional_fee: CONVENTIONAL,
                unpaid_actions: 0,
                weight_ratio: Zip317Params::ZAKURA.weight_ratio(CONVENTIONAL, CONVENTIONAL),
                cost: MEMPOOL_COST_THRESHOLD,
                eviction_weight: MEMPOOL_COST_THRESHOLD,
            }
        );
        // One zatoshi below the conventional fee: one unpaid action and the penalty. The
        // store does not apply the policy: with the unpaid action limit of 0 the policy
        // admits no such transaction.
        let fees = store.fees(&low.wtxid()).unwrap();
        assert_eq!(fees.unpaid_actions, 1);
        assert_eq!(fees.cost, MEMPOOL_COST_THRESHOLD);
        assert_eq!(
            fees.eviction_weight,
            MEMPOOL_COST_THRESHOLD + LOW_FEE_PENALTY
        );
        // Above the threshold the cost is the serialized size.
        let size = large.raw.bytes.len() as u64;
        assert!(size > 25_000);
        assert_eq!(store.fees(&large.wtxid()).unwrap().cost, size);
        assert_eq!(store.total_cost(), 2 * MEMPOOL_COST_THRESHOLD + size);
        assert_eq!(
            store.cost_bytes(),
            paid.raw.bytes.len() + low.raw.bytes.len() + large.raw.bytes.len()
        );
        // The candidates carry the stored values.
        let candidates = store.candidates();
        let candidate = candidates.iter().find(|c| c.wtxid == low.wtxid()).unwrap();
        assert_eq!(candidate.conventional_fee, CONVENTIONAL);
        assert_eq!(candidate.weight_ratio, fees.weight_ratio);
    }

    /// ZIP 401: the probability of an eviction is in proportion to the eviction weight.
    /// 10 stored transactions pay the conventional fee (weight 10,000 each), 10 pay less
    /// (weight 50,000 each) and the new transaction pays it (weight 10,000). The sum of the
    /// weights is 610,000.
    #[test]
    fn eviction_follows_the_eviction_weights() {
        const TRIALS: u64 = 6_100;
        let full: Vec<Arc<PreparedTx>> = (0..10).map(|i| paying(i, CONVENTIONAL)).collect();
        let low: Vec<Arc<PreparedTx>> = (10..20).map(|i| paying(i, CONVENTIONAL / 2)).collect();
        let new = paying(20, CONVENTIONAL);
        let (mut low_evicted, mut full_evicted, mut new_evicted) = (0u64, 0u64, 0u64);
        for trial in 0..TRIALS {
            let store = store(20, trial);
            // Interleave, so that the result does not depend on the scan order.
            for (a, b) in full.iter().zip(&low) {
                store.insert(a.clone()).unwrap();
                store.insert(b.clone()).unwrap();
            }
            assert_eq!(store.total_cost(), 200_000);
            match store.insert(new.clone()) {
                Err(InsertError::Evicted) => {
                    new_evicted += 1;
                    assert_eq!(store.len(), 20);
                }
                Ok(()) => {
                    assert_eq!(store.len(), 20);
                    let gone_low = low.iter().filter(|t| !holds(&store, t));
                    low_evicted += gone_low.count() as u64;
                    let gone_full = full.iter().filter(|t| !holds(&store, t));
                    full_evicted += gone_full.count() as u64;
                }
                Err(other) => panic!("unexpected {other:?}"),
            }
            assert_eq!(store.total_cost(), 200_000);
        }
        assert_eq!(low_evicted + full_evicted + new_evicted, TRIALS);
        // Expected: 5,000 (standard deviation 30), 1,000 (29) and 100 (10).
        assert!((4_850..=5_150).contains(&low_evicted), "{low_evicted}");
        assert!((855..=1_145).contains(&full_evicted), "{full_evicted}");
        assert!((50..=150).contains(&new_evicted), "{new_evicted}");
        // One low-fee transaction is evicted 5 times as often as one that pays the fee.
        let ratio = low_evicted as f64 / full_evicted as f64;
        assert!((4.3..=5.8).contains(&ratio), "{ratio}");
    }

    #[test]
    fn the_total_cost_is_never_above_the_limit() {
        let store = store(30, 7);
        let limit = 30 * MEMPOOL_COST_THRESHOLD;
        let events = store.events();
        let mut evictions = 0;
        for i in 0..400u32 {
            // Sizes from below the threshold to 3.5 times the threshold; some low fees.
            let tx = match i % 4 {
                0 => sized(i, 7_000 * (i as usize % 6)),
                1 => paying(i, CONVENTIONAL / 4),
                _ => paying(i, CONVENTIONAL),
            };
            match store.insert(tx.clone()) {
                Ok(()) => {}
                Err(InsertError::Evicted) => assert!(store.is_recently_evicted(&tx.raw.txid)),
                Err(other) => panic!("unexpected {other:?}"),
            }
            evictions += removed(&events).len();
            let held: u64 = store
                .candidates()
                .iter()
                .map(|c| store.fees(&c.wtxid).unwrap().cost)
                .sum();
            assert_eq!(store.total_cost(), held);
            assert!(store.total_cost() <= limit, "insert {i}");
        }
        assert!(evictions > 300, "the store was full for most inserts");
        // A transaction with a cost above the limit never enters: the selection continues
        // until it selects the new transaction.
        let Err(InsertError::Evicted) = store.insert(sized(1_000, 300_001)) else {
            panic!("a cost above the limit is refused");
        };
        assert!(store.total_cost() <= limit);
    }

    #[test]
    fn an_evicted_transaction_is_refused_until_the_memory_expires() {
        let store = store(1, 3);
        let events = store.events();
        let start = Instant::now();
        let (a, b) = (paying(1, CONVENTIONAL), paying(2, CONVENTIONAL));
        store.insert_at(a.clone(), start).unwrap();
        // The store has room for one: the selection evicts `a` or refuses `b`.
        let (kept, evicted) = match store.insert_at(b.clone(), start) {
            Ok(()) => (b, a),
            Err(InsertError::Evicted) => (a, b),
            Err(other) => panic!("unexpected {other:?}"),
        };
        assert_eq!(
            store.get(&kept.wtxid()).map(|t| t.wtxid()),
            Some(kept.wtxid())
        );
        assert!(store.is_recently_evicted(&evicted.raw.txid));
        assert!(!store.is_recently_evicted(&kept.raw.txid));
        // An eviction of a stored transaction emits `Removed`.
        let gone = removed(&events);
        assert!(gone.is_empty() || gone == vec![evicted.wtxid()]);
        // Room again: the list alone refuses the transaction, until 60 min passed.
        assert_eq!(store.remove_mined(&[kept.wtxid()]), 1);
        let Err(InsertError::RecentlyEvicted(txid)) =
            store.insert_at(evicted.clone(), start + EVICTION_MEMORY)
        else {
            panic!("refused at exactly 60 min");
        };
        assert_eq!(txid, evicted.raw.txid);
        store
            .insert_at(
                evicted.clone(),
                start + EVICTION_MEMORY + Duration::from_secs(1),
            )
            .unwrap();
    }

    #[test]
    fn the_recently_evicted_list_is_bounded() {
        let txid = |i: u32| {
            let mut bytes = [0u8; 32];
            bytes[..4].copy_from_slice(&i.to_le_bytes());
            TxId::from_bytes(bytes)
        };
        let start = Instant::now();
        let mut list = RecentlyEvicted::new(3);
        for i in 0..4 {
            list.add(txid(i), start + Duration::from_secs(u64::from(i)));
        }
        // The oldest entry left the full list.
        assert!(!list.contains(&txid(0), start));
        assert!((1..4).all(|i| list.contains(&txid(i), start)));
        assert_eq!((list.order.len(), list.ids.len()), (3, 3));
        // A second eviction of a listed txid keeps one entry.
        list.add(txid(3), start);
        assert_eq!((list.order.len(), list.ids.len()), (3, 3));
        // Entry 1 is from second 1: it expires after second 3,601.
        let at = |s| start + Duration::from_secs(s);
        assert!(list.contains(&txid(1), at(3_601)));
        assert!(!list.contains(&txid(1), at(3_602)));
        assert!(list.contains(&txid(2), at(3_602)));
        assert_eq!(
            PreparedStore::new(RuleEpoch::consensus(BRANCH), 1, Zip317Params::ZAKURA)
                .writer
                .lock()
                .evicted
                .capacity,
            EVICTION_MEMORY_ENTRIES
        );
    }

    #[test]
    fn expiry_at_the_height_boundary() {
        let store = store(10, 0);
        let events = store.events();
        let with = |id, expiry| {
            Arc::new(prepared(&TxSpec {
                id,
                expiry,
                ..TxSpec::standard()
            }))
        };
        let (never, at_100, at_101) = (with(1, 0), with(2, 100), with(3, 101));
        let child = child_of(&at_100, 0);
        for tx in [&never, &at_100, &at_101, &child] {
            store.insert(tx.clone()).unwrap();
        }
        // A block of height 100 can contain a transaction with expiry height 100.
        assert_eq!(store.remove_expired(100), vec![]);
        assert_eq!(removed(&events), vec![]);
        // A block of height 101 cannot. The child of the expired transaction goes too.
        assert_eq!(store.remove_expired(101), vec![at_100.wtxid()]);
        assert_eq!(removed(&events), vec![at_100.wtxid(), child.wtxid()]);
        assert_eq!(store.remove_expired(102), vec![at_101.wtxid()]);
        assert_eq!(removed(&events), vec![at_101.wtxid()]);
        assert_eq!(store.remove_expired(u32::MAX), vec![]);
        assert_eq!(store.len(), 1);
        assert_eq!(store.total_cost(), MEMPOOL_COST_THRESHOLD);
        // An expired transaction is not on the recently-evicted list.
        assert!(!store.is_recently_evicted(&at_100.raw.txid));
    }

    #[test]
    fn eviction_never_selects_an_ancestor_of_the_new_transaction() {
        let (mut child_stored, mut child_refused) = (0, 0);
        for seed in 0..200 {
            let store = store(3, seed);
            let grandparent = paying(1, CONVENTIONAL);
            let parent = child_of(&grandparent, 0);
            let other = paying(2, CONVENTIONAL);
            for tx in [&grandparent, &parent, &other] {
                store.insert(tx.clone()).unwrap();
            }
            let child = child_of(&parent, 0);
            let result = store.insert(child.clone());
            // The two ancestors stay, whatever the selection.
            for tx in [&grandparent, &parent] {
                assert_eq!(store.get(&tx.wtxid()).map(|t| t.wtxid()), Some(tx.wtxid()));
                assert!(!store.is_recently_evicted(&tx.raw.txid));
            }
            match result {
                Ok(()) => {
                    child_stored += 1;
                    assert!(!holds(&store, &other));
                    assert!(store.is_recently_evicted(&other.raw.txid));
                    let candidates = store.candidates();
                    let c = candidates
                        .iter()
                        .find(|c| c.wtxid == child.wtxid())
                        .unwrap();
                    assert_eq!(c.depends_on, vec![parent.wtxid()]);
                }
                Err(InsertError::Evicted) => {
                    child_refused += 1;
                    assert!(holds(&store, &other));
                }
                Err(other) => panic!("unexpected {other:?}"),
            }
            assert_eq!(store.len(), 3);
        }
        // Two candidates of equal weight.
        assert!(child_stored > 60 && child_refused > 60);
    }

    #[test]
    fn eviction_removes_the_descendants_of_the_victim() {
        let (mut parent_evicted, mut child_evicted) = (0, 0);
        for seed in 0..200 {
            let store = store(2, seed);
            let events = store.events();
            let parent = paying(1, CONVENTIONAL);
            let child = child_of(&parent, 0);
            store.insert(parent.clone()).unwrap();
            store.insert(child.clone()).unwrap();
            let new = paying(2, CONVENTIONAL);
            let result = store.insert(new.clone());
            let gone = removed(&events);
            let Ok(()) = result else {
                assert_eq!(store.len(), 2);
                assert_eq!(gone, vec![]);
                continue;
            };
            if store.is_recently_evicted(&parent.raw.txid) {
                parent_evicted += 1;
                // The child leaves with its parent. Only the selected victim is on the
                // recently-evicted list.
                assert_eq!(gone, vec![parent.wtxid(), child.wtxid()]);
                assert!(!store.is_recently_evicted(&child.raw.txid));
                assert_eq!(store.len(), 1);
                assert_eq!(store.total_cost(), MEMPOOL_COST_THRESHOLD);
            } else {
                child_evicted += 1;
                assert_eq!(gone, vec![child.wtxid()]);
                assert!(store.is_recently_evicted(&child.raw.txid));
                assert!(holds(&store, &parent));
                assert_eq!(store.len(), 2);
            }
            // No stored transaction spends an output of a transaction that left.
            let spender = store.spender(&OutPoint::new(*parent.raw.txid.as_ref(), 0));
            assert_eq!(spender, holds(&store, &child).then(|| child.wtxid()));
        }
        assert!(parent_evicted > 30 && child_evicted > 30);
    }

    #[test]
    fn conflicts_stay_and_no_fee_replaces_a_stored_transaction() {
        let store = store(10, 0);
        let first = paying(1, CONVENTIONAL);
        store.insert(first.clone()).unwrap();
        // The same outpoint with 100 times the fee: Zcash has no replace-by-fee.
        let mut spec = TxSpec::paying(1, 100 * CONVENTIONAL);
        spec.lock_time = 1;
        let Err(InsertError::Conflict { by, .. }) = store.insert(Arc::new(prepared(&spec))) else {
            panic!("a second spend of an outpoint is a conflict");
        };
        assert_eq!(by, first.wtxid());
        // The same nullifier.
        let mut a = prepared(&TxSpec::paying(2, CONVENTIONAL));
        let mut b = prepared(&TxSpec::paying(3, 100 * CONVENTIONAL));
        a.nullifiers = vec![(Pool::Orchard, [7; 32])];
        b.nullifiers = vec![(Pool::Orchard, [7; 32])];
        let a = Arc::new(a);
        store.insert(a.clone()).unwrap();
        let Err(InsertError::NullifierConflict { by, .. }) = store.insert(Arc::new(b)) else {
            panic!("a second reveal of a nullifier is a conflict");
        };
        assert_eq!(by, a.wtxid());
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn relay_ids_leave_out_a_transaction_that_expires_soon() {
        let store = store(10, 0);
        let with = |id, expiry| {
            Arc::new(prepared(&TxSpec {
                id,
                expiry,
                ..TxSpec::standard()
            }))
        };
        let (never, soon, later) = (with(1, 0), with(2, 102), with(3, 103));
        for tx in [&never, &soon, &later] {
            store.insert(tx.clone()).unwrap();
        }
        let mut ids = store.relay_ids(100);
        ids.sort();
        let mut expected = vec![never.wtxid(), later.wtxid()];
        expected.sort();
        assert_eq!(ids, expected);
    }
}
