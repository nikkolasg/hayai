//! The mempool of the node: the admission of a transaction into the prepared store.
//!
//! The order of the admission:
//!
//! 1. The store does not hold the transaction, and did not evict it in the last 60 min
//!    (ZIP 401).
//! 2. `prepare` on the committed tip: the context-free consensus rules and the scripts.
//! 3. `MempoolPolicy::admit`: the rules of `docs/mempool-policy.md`.
//! 4. The tip state: no Orchard bundle in the Orchard-disabled range of the network, no
//!    nullifier that the chain holds, each anchor is the tree state of an earlier block.
//! 5. The proofs and the signatures of the shielded bundles.
//! 6. The insert into the store, with the ZIP 401 eviction.
//!
//! The admission and the commit of a block run on two threads. The driver counts each
//! change of the tip ([`Mempool::tip_change`]) and cleans the store under the lock of the
//! count. The insert takes the same lock and checks that the tip is the tip of the checks:
//! on another tip the admission runs again. A transaction that conflicts with a new block
//! therefore never enters the store after the driver cleaned it.
//!
//! The cheap rules run before the proofs. A transaction with an invalid script or an
//! invalid proof costs its peer score (`hayai_sync::score`). A refusal that depends on the
//! tip or on the policy costs nothing.
//!
//! A private transaction ([`Mempool::admit_private`]) is in the store and thus in the
//! template, and the node does not show it to a peer before a block contains it: the relay
//! reads the store through [`PublicTxs`], which does not have the private transactions.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock, Weak};

use hayai_coins::{CoinsView, OutPoint, Pool};
use hayai_consensus::ConsensusError;
use hayai_net::{Misbehaviour, Relay, Source, TxSink};
use hayai_prepared::{
    prepare, InsertError, PrepareError, PreparedStore, PreparedTx, RuleEpoch, ScopedBatch,
    VerifyingKeys,
};
use hayai_prepared::{MempoolPolicy, PolicyContext, PolicyReject};
use hayai_state::ChainView;
use hayai_wire::{RawTx, TxLookup, WtxId};
use parking_lot::{Mutex, MutexGuard, RwLock};

use crate::metrics::NodeMetrics;
use crate::params::NetParams;

/// Why the mempool refuses a transaction.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Reject {
    #[error("the transaction is in the store")]
    Known,
    #[error(transparent)]
    Rules(#[from] ConsensusError),
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    #[error(transparent)]
    Policy(#[from] PolicyReject),
    #[error("the Orchard pool is disabled at height {0}")]
    OrchardDisabled(u32),
    #[error("the chain holds a {0} nullifier of the transaction")]
    NullifierInChain(Pool),
    #[error("a {0} anchor of the transaction is not the tree state of an earlier block")]
    UnknownAnchor(Pool),
    #[error("a shielded bundle of the transaction is not valid")]
    Proof,
    #[error(transparent)]
    Store(#[from] InsertError),
    /// The tip changed during each of [`ADMISSION_TRIES`] admissions. No penalty.
    #[error("the tip changed during the admission")]
    TipChanged,
}

/// Bytes of the transactions of disconnected blocks that go through the admission again
/// after a reorg, at most: two blocks.
const READMIT_BYTES: usize = 2 * hayai_wire::MAX_BLOCK_BYTES;

/// Times that the admission runs for one transaction when the tip changes under it.
const ADMISSION_TRIES: usize = 3;

/// Test hook: a call between the checks of an admission and its insert.
#[cfg(test)]
type BeforeInsert = Box<dyn FnOnce() + Send>;

/// The prepared store as the transaction sink of the relay.
pub struct Mempool {
    store: Arc<PreparedStore>,
    view: Arc<RwLock<ChainView>>,
    params: NetParams,
    policy: MempoolPolicy,
    keys: Arc<VerifyingKeys>,
    metrics: Arc<NodeMetrics>,
    /// The relay, for the score of a peer. The relay is built after its sinks.
    relay: OnceLock<Weak<Relay>>,
    /// The number of tip changes. The driver holds the lock while it changes the view and
    /// cleans the store, and the insert of an admission holds it too.
    tip_changes: Mutex<u64>,
    /// The private transactions that no block contains yet.
    private: RwLock<HashSet<WtxId>>,
    private_admission: Mutex<()>,
    #[cfg(test)]
    pub(crate) before_insert: Mutex<Option<BeforeInsert>>,
}

impl Mempool {
    pub fn new(
        store: Arc<PreparedStore>,
        view: Arc<RwLock<ChainView>>,
        params: NetParams,
        keys: Arc<VerifyingKeys>,
        metrics: Arc<NodeMetrics>,
    ) -> Self {
        Self {
            store,
            view,
            params,
            policy: MempoolPolicy::of(params.kind),
            keys,
            metrics,
            relay: OnceLock::new(),
            tip_changes: Mutex::new(0),
            private: RwLock::new(HashSet::new()),
            private_admission: Mutex::new(()),
            #[cfg(test)]
            before_insert: Mutex::new(None),
        }
    }

    /// Gives the relay that records the score of a peer.
    pub fn set_relay(&self, relay: &Arc<Relay>) {
        let Ok(()) = self.relay.set(Arc::downgrade(relay)) else {
            unreachable!("the node sets the relay of the mempool one time");
        };
    }

    /// The chain state on the committed tip, for the state comparisons of the tests.
    #[cfg(test)]
    pub(crate) fn view(&self) -> ChainView {
        self.view.read().clone()
    }

    /// Whether the store holds the transaction `id`.
    pub fn contains(&self, id: &WtxId) -> bool {
        let Some(_) = self.store.get(id) else {
            return false;
        };
        true
    }

    /// The driver starts a change of the tip. It holds the result while it writes the view
    /// and removes the transactions of the store that the new tip makes invalid.
    pub(crate) fn tip_change(&self) -> MutexGuard<'_, u64> {
        let mut changes = self.tip_changes.lock();
        *changes += 1;
        changes
    }

    /// Applies every admission rule to `tx` on the committed tip and stores it.
    pub fn admit(&self, tx: Arc<RawTx>) -> Result<(), Reject> {
        for _ in 0..ADMISSION_TRIES {
            if let Some(result) = self.admit_on_tip(&tx) {
                return result;
            }
        }
        Err(Reject::TipChanged)
    }

    /// As [`Mempool::admit`], for a transaction that the node must not show to a peer
    /// before a block contains it. The mark is set before the insert, so the relay never
    /// reads the transaction from the store.
    pub fn admit_private(&self, tx: Arc<RawTx>) -> Result<(), Reject> {
        // One private admission at a time: the mark of a transaction has one owner.
        let _one = self.private_admission.lock();
        let wtxid = tx.wtxid();
        // A transaction of the store is public: the peers can know it already.
        let None = self.store.get(&wtxid) else {
            return Err(Reject::Known);
        };
        self.private.write().insert(wtxid);
        self.admit(tx).inspect_err(|_| {
            self.private.write().remove(&wtxid);
        })
    }

    /// Whether `id` is a private transaction that no block contains yet.
    pub fn is_private(&self, id: &WtxId) -> bool {
        self.private.read().contains(id)
    }

    /// Ends the private marks of `ids`: the transactions that a block contains, the
    /// transactions that a block removed from the store, and a transaction that a peer
    /// sent.
    pub(crate) fn forget_private(&self, ids: &[WtxId]) {
        if self.private.read().is_empty() {
            return;
        }
        let mut private = self.private.write();
        for id in ids {
            private.remove(id);
        }
    }

    /// One admission of `tx`. `None`: the tip changed between the checks and the insert.
    fn admit_on_tip(&self, tx: &Arc<RawTx>) -> Option<Result<(), Reject>> {
        let tip = *self.tip_changes.lock();
        let prepared = match self.check(tx) {
            Ok(prepared) => prepared,
            Err(reason) => return Some(Err(reason)),
        };
        #[cfg(test)]
        if let Some(hook) = self.before_insert.lock().take() {
            hook();
        }
        let changes = self.tip_changes.lock();
        if *changes != tip {
            return None;
        }
        if let Err(reason) = self.store.insert(Arc::new(prepared)) {
            return Some(Err(reason.into()));
        }
        drop(changes);
        self.metrics
            .mempool_transactions
            .set(self.store.len() as f64);
        self.metrics
            .mempool_bytes
            .set(self.store.cost_bytes() as f64);
        Some(Ok(()))
    }

    /// The admission rules of `tx` on the committed tip, before the insert.
    fn check(&self, tx: &Arc<RawTx>) -> Result<PreparedTx, Reject> {
        let wtxid = tx.wtxid();
        let None = self.store.get(&wtxid) else {
            return Err(Reject::Known);
        };
        if self.store.is_recently_evicted(&tx.txid) {
            return Err(InsertError::RecentlyEvicted(tx.txid).into());
        }
        let view = self.view.read().clone();
        let next_height = view.tip_height() + 1;
        let rules = self.params.rules_at(next_height)?;
        let mut batch = ScopedBatch::new(&self.keys);
        let mut prepared = prepare((**tx).clone(), RuleEpoch::of(rules), &view, &mut batch)?;
        self.check_on_tip(&prepared, &view)?;
        if !batch.finalize().failed.is_empty() {
            return Err(Reject::Proof);
        }
        if prepared.has_shielded() {
            prepared.set_shielded_ok();
        }
        Ok(prepared)
    }

    /// The rules of step 3 and step 4 for `prepared` on the tip of `view`: the policy and
    /// the tip state. They read no proof and no script.
    fn check_on_tip(&self, prepared: &PreparedTx, view: &ChainView) -> Result<(), Reject> {
        let next_height = view.tip_height() + 1;
        let rules = self.params.rules_at(next_height)?;
        self.policy.admit(
            prepared,
            &PolicyContext {
                next_height,
                median_time_past: view.median_time_past(),
                rules,
            },
        )?;
        if let (true, Some(_)) = (
            self.params.kind.orchard_disabled(next_height),
            prepared.raw.tx.orchard_bundle(),
        ) {
            return Err(Reject::OrchardDisabled(next_height));
        }
        for pool in Pool::ALL {
            let nullifiers: Vec<[u8; 32]> = prepared
                .nullifiers
                .iter()
                .filter(|(p, _)| *p == pool)
                .map(|(_, nullifier)| *nullifier)
                .collect();
            if view
                .contains_nullifier_many(pool, &nullifiers)
                .contains(&true)
            {
                return Err(Reject::NullifierInChain(pool));
            }
        }
        if let Some((pool, _)) = prepared
            .anchors
            .iter()
            .find(|(pool, root)| !view.has_anchor(*pool, root))
        {
            return Err(Reject::UnknownAnchor(*pool));
        }
        // A Sprout anchor can be a treestate inside the transaction, so it is not in
        // `prepared.anchors`. A node that does not know the Sprout state refuses every
        // JoinSplit here.
        let Ok(()) = hayai_state::check_sprout_anchors(view, prepared, 0) else {
            return Err(Reject::UnknownAnchor(Pool::Sprout));
        };
        Ok(())
    }

    /// Whether the scripts and the proofs of `prepared` are valid on the tip of `view` as
    /// they were at its admission: the rule set is the same, and each input spends the
    /// coin that the scripts ran against.
    fn still_prepared(&self, prepared: &PreparedTx, view: &ChainView) -> bool {
        let Ok(epoch) = self.params.epoch_at(view.tip_height() + 1) else {
            return false;
        };
        let outpoints: Vec<OutPoint> = prepared.spent_outpoints().cloned().collect();
        let coins = view.get_coins(&outpoints);
        prepared.epoch == epoch
            && coins.len() == prepared.spent.len()
            && coins
                .iter()
                .zip(&prepared.spent)
                .all(|(found, spent)| found.as_ref() == Some(spent))
    }

    /// After a reorg: removes every transaction of the store and admits `disconnected`
    /// (the transactions of the disconnected blocks, in block order) and the removed
    /// transactions again on the new tip. Returns the ids that the store held before.
    ///
    /// The call is on the driver thread, so its work has a bound. A transaction of the
    /// store keeps its scripts and its proofs when its inputs are the same coins: only
    /// the policy and the tip state run again. A transaction of a disconnected block has
    /// no prepared form and takes the whole admission: the newest [`READMIT_BYTES`] of
    /// them are admitted, the older ones are dropped.
    pub fn readmit(&self, disconnected: Vec<Arc<RawTx>>) -> Vec<WtxId> {
        let mut ids = Vec::new();
        hayai_wire::TxLookup::for_each_id(self.store.as_ref(), &mut |id| ids.push(*id));
        let held: Vec<Arc<PreparedTx>> = ids.iter().filter_map(|id| self.store.get(id)).collect();
        self.store.remove_mined(&ids);

        let mut budget = READMIT_BYTES;
        let first = disconnected
            .iter()
            .rposition(|tx| match budget.checked_sub(tx.bytes.len()) {
                Some(rest) => {
                    budget = rest;
                    false
                }
                None => true,
            })
            .map_or(0, |last_dropped| last_dropped + 1);
        if first > 0 {
            tracing::warn!(
                dropped = first,
                "transactions of disconnected blocks not admitted again: above the bound"
            );
        }
        let mut pending: Vec<Arc<RawTx>> = disconnected[first..].to_vec();
        // A transaction whose parent is later in the list passes in a later round.
        loop {
            let before = pending.len();
            pending.retain(|tx| match self.admit(tx.clone()) {
                Ok(()) | Err(Reject::Known) => false,
                Err(Reject::Prepare(PrepareError::MissingInput(_))) => true,
                Err(reason) => {
                    tracing::debug!(wtxid = ?tx.wtxid(), %reason, "transaction not admitted after the reorg");
                    false
                }
            });
            if pending.is_empty() || pending.len() == before {
                break;
            }
        }

        let view = self.view.read().clone();
        for prepared in held {
            let wtxid = prepared.raw.wtxid();
            if !self.still_prepared(&prepared, &view) {
                tracing::debug!(
                    ?wtxid,
                    "transaction not kept after the reorg: its inputs or its rule set changed"
                );
                continue;
            }
            let kept = self
                .check_on_tip(&prepared, &view)
                .and_then(|()| Ok(self.store.insert(prepared)?));
            if let Err(reason) = kept {
                tracing::debug!(?wtxid, %reason, "transaction not kept after the reorg");
            }
        }
        ids
    }

    /// The score of the peer of a refused transaction: an invalid script or an invalid
    /// proof. Every other refusal depends on the tip, on the policy or on this build.
    fn penalize(&self, source: Source, reason: &Reject) {
        let misbehaviour = match reason {
            Reject::Proof => Misbehaviour::InvalidProof,
            Reject::Prepare(PrepareError::Script(..)) => Misbehaviour::InvalidTransaction,
            _ => return,
        };
        if let Some(relay) = self.relay.get().and_then(Weak::upgrade) {
            relay.misbehaved(source, misbehaviour);
        }
    }
}

/// The store without the private transactions: what the relay announces and serves.
pub struct PublicTxs {
    pub store: Arc<PreparedStore>,
    pub mempool: Arc<Mempool>,
}

impl TxLookup for PublicTxs {
    fn get(&self, id: &WtxId) -> Option<Arc<RawTx>> {
        match self.mempool.is_private(id) {
            true => None,
            false => TxLookup::get(self.store.as_ref(), id),
        }
    }

    fn for_each_id(&self, f: &mut dyn FnMut(&WtxId)) {
        let private = self.mempool.private.read();
        self.store.for_each_id(&mut |id| {
            if !private.contains(id) {
                f(id)
            }
        });
    }

    fn len(&self) -> usize {
        let private = self.mempool.private.read();
        let hidden = private
            .iter()
            .filter(|id| self.mempool.contains(id))
            .count();
        TxLookup::len(self.store.as_ref()).saturating_sub(hidden)
    }
}

impl TxSink for Mempool {
    fn accept_tx(&self, tx: Arc<RawTx>, source: Source) -> bool {
        let wtxid = tx.wtxid();
        match self.admit(tx) {
            Ok(()) => {
                // A transaction from a peer is public. A mark of an earlier private
                // admission of the same transaction, which left the store, ends here.
                self.forget_private(&[wtxid]);
                true
            }
            Err(Reject::Known) => false,
            Err(reason) => {
                let counter = match &reason {
                    Reject::Policy(_) | Reject::Store(_) | Reject::TipChanged => {
                        &self.metrics.mempool_rejected_policy
                    }
                    _ => &self.metrics.mempool_rejected_invalid,
                };
                counter.inc();
                match &reason {
                    Reject::Rules(e) => {
                        tracing::error!(?wtxid, error = %e, "transaction refused: no rule set")
                    }
                    _ => tracing::debug!(?wtxid, %reason, "transaction refused"),
                }
                self.penalize(source, &reason);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use bytes::Bytes;
    use hayai_coins::{Coin, MemBacking, MemConfig};
    use hayai_crypto::zcash_protocol::consensus::BranchId;
    use hayai_rpc::Registry;
    use hayai_state::{Base, Chain};
    use hayai_template::Zip317Params;
    use hayai_wire::header::BlockHash;
    use hayai_wire::MAX_BLOCK_BYTES;

    use super::*;
    use crate::params::NetworkKind;

    const VALUE: u64 = 625_000_000;

    /// A view with one coin: a mature coinbase coin with the script `OP_TRUE`.
    struct OneCoin(OutPoint);

    impl CoinsView for OneCoin {
        fn get_coins(&self, outpoints: &[OutPoint]) -> Vec<Option<Coin>> {
            outpoints
                .iter()
                .map(|outpoint| {
                    (*outpoint == self.0).then(|| Coin {
                        value: VALUE,
                        script_pubkey: Bytes::from_static(&[0x51]),
                        height: 1,
                        is_coinbase: true,
                    })
                })
                .collect()
        }
    }

    /// On Testnet the Orchard pool is off from height 4,048,500 until NU6.2. The admission
    /// refuses a transaction with an Orchard bundle for a block of that range, and takes
    /// the same transaction for the block before the range.
    #[test]
    fn the_admission_refuses_an_orchard_bundle_while_the_pool_is_off() {
        let params = NetParams::new(NetworkKind::Testnet);
        let start = 4_048_500;
        assert!(!params.kind.orchard_disabled(start - 1));
        assert!(params.kind.orchard_disabled(start));
        let epoch = params.epoch_at(start).expect("a rule set");
        assert_eq!(epoch.branch_id, BranchId::Nu6_1);
        let keys = VerifyingKeys::prebuild(epoch, None);
        keys.ready();

        let outpoint = OutPoint::new([3; 32], 0);
        let bytes =
            crate::test_support::shielding_tx(outpoint.clone(), VALUE, 15_000, 0, BranchId::Nu6_1);
        let raw = RawTx::parse(bytes, BranchId::Nu6_1).expect("a transaction");
        let mut batch = ScopedBatch::new(&keys);
        let prepared = prepare(raw, epoch, &OneCoin(outpoint), &mut batch).expect("prepare");
        assert!(batch.finalize().failed.is_empty());

        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&scratch).expect("scratch base");
        let dir = tempfile::tempdir_in(scratch).expect("scratch dir");
        let view_at = |height: u32| {
            let coins = dir.path().join(format!("coins-{height}"));
            let (backing, _) = MemBacking::open(&coins, &MemConfig::default()).expect("coins");
            let base = Base::new(Arc::new(backing), height, BlockHash([7; 32]), 1_700_000_000);
            Chain::new(base).view()
        };
        let mempool = |view: ChainView| {
            let store = Arc::new(PreparedStore::new(
                epoch,
                MAX_BLOCK_BYTES,
                Zip317Params::ZAKURA,
            ));
            let mut mempool = Mempool::new(
                store,
                Arc::new(RwLock::new(view)),
                params,
                keys.clone(),
                Arc::new(NodeMetrics::new(&Registry::new())),
            );
            // The spent script is `OP_TRUE`, which is not a standard script.
            mempool.policy.require_standard = false;
            mempool
        };

        let before = view_at(start - 2);
        assert_eq!(
            mempool(before.clone()).check_on_tip(&prepared, &before),
            Ok(())
        );
        let inside = view_at(start - 1);
        assert_eq!(
            mempool(inside.clone()).check_on_tip(&prepared, &inside),
            Err(Reject::OrchardDisabled(start))
        );
    }
}
