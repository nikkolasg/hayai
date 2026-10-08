//! The mempool of the node: the admission of `hayai-mempool` with the relay and the metrics.
//!
//! [`hayai_mempool::Mempool`] holds the order of the admission and the protocol of the tip
//! change with the driver. This module adds what only this node does:
//!
//! - the gauges of the store after each admission and after a reorg;
//! - the peer score: a transaction with an invalid script or an invalid proof costs the
//!   score of its peer (`hayai_sync::score`). A refusal that depends on the tip or on the
//!   policy costs nothing;
//! - the private transactions, and the transaction sink of the relay.
//!
//! A private transaction ([`Mempool::admit_private`]) is in the store and thus in the
//! template, and the node does not show it to a peer before a block contains it: the relay
//! reads the store through [`PublicTxs`], which does not have the private transactions.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock, Weak};

use hayai_mempool::{PreparedStore, Reject};
use hayai_net::{Misbehaviour, Relay, Source, TxSink};
use hayai_prepared::{PrepareError, VerifyingKeys};
use hayai_state::ChainView;
use hayai_wire::{RawTx, TxLookup, WtxId};
use parking_lot::{Mutex, MutexGuard, RwLock};

use crate::metrics::NodeMetrics;
use crate::params::NetParams;

/// Test hook: a call between the checks of an admission and its insert.
#[cfg(test)]
type BeforeInsert = Box<dyn FnOnce() + Send>;

/// The mempool of `hayai-mempool` as the transaction sink of the relay.
pub struct Mempool {
    core: hayai_mempool::Mempool,
    metrics: Arc<NodeMetrics>,
    /// The relay, for the score of a peer. The relay is built after its sinks.
    relay: OnceLock<Weak<Relay>>,
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
            core: hayai_mempool::Mempool::new(store, view, params.kind, keys),
            metrics,
            relay: OnceLock::new(),
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
        self.core.view()
    }

    /// Whether the store holds the transaction `id`.
    pub fn contains(&self, id: &WtxId) -> bool {
        self.core.contains(id)
    }

    /// The driver starts a change of the tip. It holds the result while it writes the view
    /// and removes the transactions of the store that the new tip makes invalid.
    pub(crate) fn tip_change(&self) -> MutexGuard<'_, u64> {
        self.core.tip_change()
    }

    /// Applies every admission rule to `tx` on the committed tip and stores it.
    pub fn admit(&self, tx: Arc<RawTx>) -> Result<(), Reject> {
        self.core.admit_with_hook(tx, || {
            #[cfg(test)]
            if let Some(hook) = self.before_insert.lock().take() {
                hook();
            }
        })?;
        self.record_store();
        Ok(())
    }

    /// As [`Mempool::admit`], for a transaction that the node must not show to a peer
    /// before a block contains it. The mark is set before the insert, so the relay never
    /// reads the transaction from the store.
    pub fn admit_private(&self, tx: Arc<RawTx>) -> Result<(), Reject> {
        // One private admission at a time: the mark of a transaction has one owner.
        let _one = self.private_admission.lock();
        let wtxid = tx.wtxid();
        // A transaction of the store is public: the peers can know it already.
        let None = self.core.store().get(&wtxid) else {
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

    /// After a reorg: [`hayai_mempool::Mempool::readmit`], then the gauges of the store.
    /// Returns the ids that the store held before.
    pub fn readmit(&self, disconnected: Vec<Arc<RawTx>>) -> Vec<WtxId> {
        let ids = self.core.readmit(disconnected);
        self.record_store();
        ids
    }

    /// Sets the gauges of the store.
    fn record_store(&self) {
        let store = self.core.store();
        self.metrics.mempool_transactions.set(store.len() as f64);
        self.metrics.mempool_bytes.set(store.cost_bytes() as f64);
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

    fn for_each_relay_id(&self, next_height: u32, f: &mut dyn FnMut(&WtxId)) {
        let private = self.mempool.private.read();
        self.store.for_each_relay_id(next_height, &mut |id| {
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
