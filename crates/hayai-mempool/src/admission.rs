//! The admission of a transaction into the prepared store.
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
//! The admission and the commit of a block run on two threads. The driver of the node counts
//! each change of the tip ([`Mempool::tip_change`]) and cleans the store under the lock of
//! the count. The insert takes the same lock and checks that the tip is the tip of the
//! checks: on another tip the admission runs again. A transaction that conflicts with a new
//! block therefore never enters the store after the driver cleaned it.
//!
//! The cheap rules run before the proofs. [`Reject`] tells the caller which refusal is a
//! fault of the sender (an invalid script or an invalid proof) and which one depends on the
//! tip or on the policy.

use std::sync::Arc;

use hayai_coins::{CoinsView, OutPoint, Pool};
use hayai_consensus::{rules_at, ConsensusError, Network};
use hayai_prepared::{prepare, PrepareError, PreparedTx, RuleEpoch, ScopedBatch, VerifyingKeys};
use hayai_state::ChainView;
use hayai_wire::{RawTx, TxLookup, WtxId};
use parking_lot::{Mutex, MutexGuard, RwLock};

use crate::policy::{MempoolPolicy, PolicyContext, PolicyReject};
use crate::store::{InsertError, PreparedStore};

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

/// The prepared store with the admission rules on the committed tip.
pub struct Mempool {
    store: Arc<PreparedStore>,
    view: Arc<RwLock<ChainView>>,
    network: Network,
    policy: MempoolPolicy,
    keys: Arc<VerifyingKeys>,
    /// The number of tip changes. The driver holds the lock while it changes the view and
    /// cleans the store, and the insert of an admission holds it too.
    tip_changes: Mutex<u64>,
}

impl Mempool {
    /// The mempool of `network` over `store`. `view` is the chain state on the committed
    /// tip: the driver of the node writes it under [`Mempool::tip_change`].
    pub fn new(
        store: Arc<PreparedStore>,
        view: Arc<RwLock<ChainView>>,
        network: Network,
        keys: Arc<VerifyingKeys>,
    ) -> Self {
        Self {
            store,
            view,
            network,
            policy: MempoolPolicy::of(network),
            keys,
            tip_changes: Mutex::new(0),
        }
    }

    /// The store of the admitted transactions.
    pub fn store(&self) -> &Arc<PreparedStore> {
        &self.store
    }

    /// The chain state on the committed tip.
    pub fn view(&self) -> ChainView {
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
    pub fn tip_change(&self) -> MutexGuard<'_, u64> {
        let mut changes = self.tip_changes.lock();
        *changes += 1;
        changes
    }

    /// Applies every admission rule to `tx` on the committed tip and stores it.
    pub fn admit(&self, tx: Arc<RawTx>) -> Result<(), Reject> {
        self.admit_with_hook(tx, || {})
    }

    /// [`Mempool::admit`] with a call between the checks and the insert of each try. A test
    /// changes the tip in this call.
    pub fn admit_with_hook(
        &self,
        tx: Arc<RawTx>,
        mut before_insert: impl FnMut(),
    ) -> Result<(), Reject> {
        for _ in 0..ADMISSION_TRIES {
            if let Some(result) = self.admit_on_tip(&tx, &mut before_insert) {
                return result;
            }
        }
        Err(Reject::TipChanged)
    }

    /// One admission of `tx`. `None`: the tip changed between the checks and the insert.
    fn admit_on_tip(
        &self,
        tx: &Arc<RawTx>,
        before_insert: &mut impl FnMut(),
    ) -> Option<Result<(), Reject>> {
        let tip = *self.tip_changes.lock();
        let prepared = match self.check(tx) {
            Ok(prepared) => prepared,
            Err(reason) => return Some(Err(reason)),
        };
        before_insert();
        let changes = self.tip_changes.lock();
        if *changes != tip {
            return None;
        }
        if let Err(reason) = self.store.insert(Arc::new(prepared)) {
            return Some(Err(reason.into()));
        }
        drop(changes);
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
        let rules = rules_at(self.network, next_height)?;
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
        let rules = rules_at(self.network, next_height)?;
        self.policy.admit(
            prepared,
            &PolicyContext {
                next_height,
                median_time_past: view.median_time_past(),
                rules,
            },
        )?;
        if let (true, Some(_)) = (
            self.network.orchard_disabled(next_height),
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
        let Ok(rules) = rules_at(self.network, view.tip_height() + 1) else {
            return false;
        };
        let epoch = RuleEpoch::of(rules);
        let outpoints: Vec<OutPoint> = prepared.spent_outpoints().cloned().collect();
        let coins = view.get_coins(&outpoints);
        // ZIP 200: a transaction of another branch leaves the pool at an activation.
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
        TxLookup::for_each_id(self.store.as_ref(), &mut |id| ids.push(*id));
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
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use bytes::Bytes;
    use hayai_coins::{Coin, MemBacking, MemConfig};
    use hayai_crypto::zcash_protocol::consensus::BranchId;
    use hayai_fixtures::regtest::shielding_tx;
    use hayai_state::{Base, Chain};
    use hayai_template::Zip317Params;
    use hayai_wire::header::BlockHash;
    use hayai_wire::MAX_BLOCK_BYTES;

    use super::*;

    const VALUE: u64 = 625_000_000;
    const NEXT_HEIGHT: u32 = 150;

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
        let network = Network::Testnet;
        let start = 4_048_500;
        assert!(!network.orchard_disabled(start - 1));
        assert!(network.orchard_disabled(start));
        let epoch = RuleEpoch::of(rules_at(network, start).expect("a rule set"));
        assert_eq!(epoch.branch_id, BranchId::Nu6_1);
        let keys = VerifyingKeys::prebuild(epoch, None);
        keys.ready();

        let outpoint = OutPoint::new([3; 32], 0);
        let bytes = shielding_tx(outpoint.clone(), VALUE, 15_000, 0, BranchId::Nu6_1);
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
            let mut mempool =
                Mempool::new(store, Arc::new(RwLock::new(view)), network, keys.clone());
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

    #[test]
    fn shielding_tx_is_valid_and_follows_the_policy() {
        let rules = rules_at(Network::Regtest, NEXT_HEIGHT).expect("Regtest rules");
        let epoch = RuleEpoch::of(rules);
        let keys = VerifyingKeys::prebuild(epoch, None);
        keys.ready();
        let outpoint = OutPoint::new([3; 32], 0);
        let view = OneCoin(outpoint.clone());

        let prepared = |fee: u64, expiry_height: u32| -> PreparedTx {
            let bytes = shielding_tx(outpoint.clone(), VALUE, fee, expiry_height, BranchId::Nu5);
            let raw = RawTx::parse(bytes, BranchId::Nu5).expect("the transaction parses");
            let mut batch = ScopedBatch::new(&keys);
            let prepared = prepare(raw, epoch, &view, &mut batch).expect("prepare passes");
            assert!(batch.finalize().failed.is_empty());
            assert_eq!(prepared.fee, fee);
            assert_eq!(prepared.expiry_height, expiry_height);
            prepared
        };
        let policy = MempoolPolicy::of(Network::Regtest);
        let admit = |tx: &PreparedTx| {
            policy.admit(
                tx,
                &PolicyContext {
                    next_height: NEXT_HEIGHT,
                    median_time_past: 0,
                    rules,
                },
            )
        };

        // One transparent input and two Orchard actions: 3 logical actions, 1,200
        // zatoshis. The unpaid action limit is 0.
        assert_eq!(admit(&prepared(1_200, 0)), Ok(()));
        assert_eq!(
            admit(&prepared(1_199, 0)),
            Err(PolicyReject::UnpaidActions {
                unpaid: 1,
                limit: 0
            })
        );
        assert_eq!(
            admit(&prepared(15_000, NEXT_HEIGHT + 1)),
            Err(PolicyReject::ExpiringSoon {
                expiry: NEXT_HEIGHT + 1,
                next_height: NEXT_HEIGHT
            })
        );
    }
}
