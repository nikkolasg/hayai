//! Batched verification of Orchard, Ironwood and Sapling bundles with failure isolation,
//! and the verification of Sprout JoinSplits.
//!
//! A [`ScopedBatch`] collects the bundles of one scope (one block, or one mempool admission
//! round) and verifies them with the upstream batch validators, which amortize the pairing
//! and multi-scalar work over the whole batch. The upstream validators answer only "all
//! valid" or "not all valid", so a failing batch is split in halves and each half re-verified
//! (the two halves in parallel) until the failing bundles are isolated. Bundles are kept as
//! `Arc<Transaction>`, so re-adding them costs a borrow.
//!
//! Orchard verifying keys depend on the circuit version, so bundles are grouped per version
//! and verified as one task per group. An Ironwood bundle is a bundle of the Orchard
//! protocol under the NU6.3 circuit. It goes to the group of that circuit version, with the
//! Orchard bundles of NU6.3. `orchard.rs` and `sapling.rs` hold the keys and the
//! batch verification of each pool. Only [`VerifyingKeys::prebuild`] and
//! [`VerifyingKeys::prebuild_more`] build a key, on a thread of their own (0.6 s on 32
//! threads and 1.7 s on one in a release build; far more in a debug build), and
//! [`VerifyingKeys::ready`] waits for it. Nothing builds a key on first
//! use: the keygen runs on the rayon pool, and a pool worker that waits for a build which
//! needs the pool can deadlock the process. A bundle whose key is not built fails to queue
//! with [`PrepareError::Unsupported`].
//! The Sapling verifying keys are in the binary ([`SaplingKeys::embedded`]), so a Sapling
//! bundle always has its keys.
//!
//! The JoinSplits of a transaction have no batch (Zakura verifies them one by one too):
//! each transaction with JoinSplits is one task (`sprout.rs`). The key is in the binary
//! ([`SproutKey::embedded`]).

use std::collections::HashSet;
use std::sync::Arc;
use std::thread::JoinHandle;

use hayai_crypto::orchard::circuit::OrchardCircuitVersion;
use hayai_crypto::{zcash_primitives, zcash_protocol};
use hayai_wire::WtxId;
use parking_lot::Mutex;
use rayon::prelude::*;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;

use crate::orchard::{circuit_version, slot, verify_orchard, OrchardKeys, ORCHARD_VERSIONS};
use crate::sapling::{verify_sapling, SaplingKeys};
use crate::sprout::{verify_sprout, SproutKey};
use crate::{PrepareError, RuleEpoch};

/// Every verifying key a batch may need. Shared by all batches of a process.
pub struct VerifyingKeys {
    pub orchard: OrchardKeys,
    pub sapling: SaplingKeys,
    pub sprout: SproutKey,
    build: Mutex<Build>,
}

/// The background builds of the Orchard keys.
#[derive(Default)]
struct Build {
    /// The newest build thread, until [`VerifyingKeys::ready`] joins it. Each thread joins
    /// the thread before it, so the builds run one at a time.
    handle: Option<JoinHandle<()>>,
    /// The circuit versions that a build thread has or had as its work, by slot.
    requested: [bool; 3],
}

impl Default for VerifyingKeys {
    fn default() -> Self {
        Self::new()
    }
}

impl VerifyingKeys {
    /// Keys with no Orchard key: a batch rejects every Orchard bundle with
    /// [`PrepareError::Unsupported`]. A node uses [`VerifyingKeys::prebuild`].
    pub fn new() -> Self {
        Self {
            orchard: OrchardKeys::new(),
            sapling: SaplingKeys::embedded(),
            sprout: SproutKey::embedded(),
            build: Mutex::new(Build::default()),
        }
    }

    /// Keys whose Orchard key of `epoch` (and of `next`, when given: the branch of the next upgrade)
    /// is built on a background thread from construction. The build overlaps the rest of
    /// the startup. The caller must call [`VerifyingKeys::ready`], on a thread that is not a
    /// rayon worker, before the first batch: until the build is done, a batch rejects the
    /// bundles of the key under construction. The keys of other circuit versions are built
    /// only by [`VerifyingKeys::prebuild_more`].
    pub fn prebuild(epoch: RuleEpoch, next: Option<BranchId>) -> Arc<Self> {
        let keys = Arc::new(Self::new());
        let branches: Vec<BranchId> = [Some(epoch.branch_id), next]
            .into_iter()
            .flatten()
            .collect();
        keys.prebuild_more(&branches);
        keys
    }

    /// Starts the build of the Orchard keys of `branches` on a background thread. A key
    /// that is built, or that an earlier call started, is not built again: the call then
    /// starts no thread. A node calls this function before its chain reaches an upgrade,
    /// and [`VerifyingKeys::ready`] before the first batch that needs the key.
    pub fn prebuild_more(self: &Arc<Self>, branches: &[BranchId]) {
        let mut build = self.build.lock();
        let versions: Vec<OrchardCircuitVersion> = branches
            .iter()
            .filter_map(|branch| circuit_version(*branch))
            .filter(|version| !std::mem::replace(&mut build.requested[slot(*version)], true))
            .collect();
        if versions.is_empty() {
            return;
        }
        let before = build.handle.take();
        let worker = self.clone();
        let handle = std::thread::Builder::new()
            .name("orchard-keys".into())
            .spawn(move || {
                if let Some(before) = before {
                    before.join().expect("the key build thread does not panic");
                }
                for version in versions {
                    worker.orchard.build(version);
                }
            })
            .expect("spawn the key build thread");
        build.handle = Some(handle);
    }

    /// Whether a batch has the Orchard key that the bundles of `branch` need. Before NU5
    /// no bundle needs an Orchard key.
    pub fn has_orchard_key(&self, branch: BranchId) -> bool {
        match circuit_version(branch) {
            Some(version) => matches!(self.orchard.built(version), Some(_key)),
            None => true,
        }
    }

    /// Waits for the background builds of [`VerifyingKeys::prebuild`] and
    /// [`VerifyingKeys::prebuild_more`]; returns at once when there is none or they are
    /// done. The build uses the rayon pool, so the caller must not be a rayon worker: a
    /// pool whose workers all wait here cannot finish the build.
    pub fn ready(&self) {
        let handle = self.build.lock().handle.take();
        if let Some(handle) = handle {
            handle.join().expect("the key build thread does not panic");
        }
    }
}

/// Factory of [`ScopedBatch`]es over one shared key set.
pub struct ShieldedBatcher {
    keys: Arc<VerifyingKeys>,
}

impl ShieldedBatcher {
    pub fn new(keys: Arc<VerifyingKeys>) -> Self {
        Self { keys }
    }

    pub fn keys(&self) -> &Arc<VerifyingKeys> {
        &self.keys
    }

    /// A new empty batch for one block or one admission round.
    pub fn batch(&self) -> ScopedBatch<'_> {
        ScopedBatch::new(&self.keys)
    }
}

/// The bundle of a transaction that an [`Item`] queues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Queued {
    Sprout,
    Sapling,
    Orchard,
    Ironwood,
}

/// One queued bundle of a transaction with the shielded sighash of the transaction.
pub(crate) struct Item {
    pub(crate) wtxid: WtxId,
    pub(crate) tx: Arc<Transaction>,
    pub(crate) sighash: [u8; 32],
    pub(crate) bundle: Queued,
}

/// The bundles of one scope, verified together by [`ScopedBatch::finalize`].
pub struct ScopedBatch<'k> {
    keys: &'k VerifyingKeys,
    orchard: [Vec<Item>; 3],
    sapling: Vec<Item>,
    sprout: Vec<Item>,
}

/// Verdict of a batch: every queued transaction is in exactly one list.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchOutcome {
    pub ok: Vec<WtxId>,
    pub failed: Vec<WtxId>,
}

impl<'k> ScopedBatch<'k> {
    pub fn new(keys: &'k VerifyingKeys) -> Self {
        Self {
            keys,
            orchard: [Vec::new(), Vec::new(), Vec::new()],
            sapling: Vec::new(),
            sprout: Vec::new(),
        }
    }

    /// Queues the shielded bundles of `tx` under its shielded `sighash`. It fails, and
    /// queues nothing, when a verifying key that the bundles need is absent: the Orchard
    /// key of the bundle's circuit version is not built ([`VerifyingKeys::prebuild`], then
    /// [`VerifyingKeys::ready`]), or a JoinSplit has a BCTV14 proof, for which hayai has
    /// no verifier.
    pub fn add(
        &mut self,
        wtxid: WtxId,
        tx: Arc<Transaction>,
        sighash: [u8; 32],
    ) -> Result<(), PrepareError> {
        // The Orchard bundle and the Ironwood bundle of the transaction, each with the slot
        // of the circuit version of its bundle version. The same sighash signs both
        // (Zakura `verify_v6_transaction`, `zakura-consensus/src/transaction.rs:1219-1228`).
        let mut queued: Vec<(usize, Queued)> = Vec::with_capacity(2);
        for (bundle, kind) in [
            (tx.orchard_bundle(), Queued::Orchard),
            (tx.ironwood_bundle(), Queued::Ironwood),
        ] {
            let Some(bundle) = bundle else {
                continue;
            };
            let version = bundle.bundle_version().circuit_version();
            let Some(_) = self.keys.orchard.built(version) else {
                return Err(PrepareError::Unsupported("orchard verifying key not built"));
            };
            queued.push((slot(version), kind));
        }
        if let Some(bundle) = tx.sprout_bundle() {
            for joinsplit in &bundle.joinsplits {
                let Some(_) = joinsplit.groth_proof_bytes() else {
                    return Err(PrepareError::Unsupported(
                        "BCTV14 JoinSplit proof: only the checkpoint path applies it",
                    ));
                };
            }
            self.sprout.push(Item {
                wtxid,
                tx: tx.clone(),
                sighash,
                bundle: Queued::Sprout,
            });
        }
        if let Some(_bundle) = tx.sapling_bundle() {
            self.sapling.push(Item {
                wtxid,
                tx: tx.clone(),
                sighash,
                bundle: Queued::Sapling,
            });
        }
        for (slot, bundle) in queued {
            self.orchard[slot].push(Item {
                wtxid,
                tx: tx.clone(),
                sighash,
                bundle,
            });
        }
        Ok(())
    }

    /// Number of bundles queued.
    pub fn len(&self) -> usize {
        self.orchard.iter().map(Vec::len).sum::<usize>() + self.sapling.len() + self.sprout.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Verifies everything queued, every bundle group as one concurrent task, and isolates
    /// the failing transactions by bisection. The JoinSplits of each transaction are one
    /// more task.
    pub fn finalize(self) -> BatchOutcome {
        let mut all: Vec<WtxId> = Vec::with_capacity(self.len());
        let mut groups: Vec<Group<'_>> = Vec::new();
        for (items, version) in self.orchard.iter().zip(ORCHARD_VERSIONS) {
            if items.is_empty() {
                continue;
            }
            all.extend(items.iter().map(|i| i.wtxid));
            groups.push(Group::Orchard { version, items });
        }
        if !self.sapling.is_empty() {
            all.extend(self.sapling.iter().map(|i| i.wtxid));
            groups.push(Group::Sapling {
                items: &self.sapling,
            });
        }
        all.extend(self.sprout.iter().map(|i| i.wtxid));
        let keys = self.keys;
        let sprout_failed = self
            .sprout
            .par_iter()
            .filter(|item| !verify_sprout(&keys.sprout, item))
            .map(|item| item.wtxid);
        let failed: HashSet<WtxId, ahash::RandomState> = groups
            .par_iter()
            .map(|group| {
                let refs: Vec<&Item> = group.items().iter().collect();
                match group {
                    Group::Orchard { version, .. } => {
                        let Some(vk) = keys.orchard.built(*version) else {
                            unreachable!("Orchard bundles are only queued when the key is built");
                        };
                        bisect(&refs, &|items: &[&Item]| verify_orchard(vk, items))
                    }
                    Group::Sapling { .. } => bisect(&refs, &|items: &[&Item]| {
                        verify_sapling(&keys.sapling, items)
                    }),
                }
            })
            .flatten()
            .chain(sprout_failed)
            .collect();
        let mut seen: HashSet<WtxId, ahash::RandomState> = HashSet::default();
        let mut outcome = BatchOutcome::default();
        for id in all {
            if !seen.insert(id) {
                continue;
            }
            if failed.contains(&id) {
                outcome.failed.push(id);
            } else {
                outcome.ok.push(id);
            }
        }
        outcome
    }
}

/// One batch of bundles that share a verifying key.
enum Group<'a> {
    Orchard {
        version: OrchardCircuitVersion,
        items: &'a [Item],
    },
    Sapling {
        items: &'a [Item],
    },
}

impl Group<'_> {
    fn items(&self) -> &[Item] {
        match self {
            Group::Orchard { items, .. } | Group::Sapling { items } => items,
        }
    }
}

/// Returns the ids of the failing items: verifies the whole slice, and on failure splits it
/// in halves verified in parallel, down to single items.
fn bisect(items: &[&Item], verify: &(dyn Fn(&[&Item]) -> bool + Sync)) -> Vec<WtxId> {
    if verify(items) {
        return Vec::new();
    }
    if let [single] = items {
        return vec![single.wtxid];
    }
    let (left, right) = items.split_at(items.len() / 2);
    let (mut l, r) = rayon::join(|| bisect(left, verify), || bisect(right, verify));
    l.extend(r);
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prebuild_builds_the_epoch_keys_in_the_background() {
        let epoch = RuleEpoch::consensus(BranchId::Nu6_2);
        let keys = VerifyingKeys::prebuild(epoch, Some(BranchId::Nu6_2));
        keys.ready();
        let Some(_) = keys.orchard.built(OrchardCircuitVersion::FixedPostNu6_2) else {
            panic!("the epoch key is built after ready()");
        };
        let None = keys.orchard.built(OrchardCircuitVersion::PostNu6_3) else {
            panic!("other versions are not built");
        };
        // A second wait returns at once; keys without a prebuild have no Orchard key.
        keys.ready();
        // A later upgrade: the key of its circuit is built, and the built key is kept.
        assert!(keys.has_orchard_key(BranchId::Nu6_2));
        assert!(!keys.has_orchard_key(BranchId::Nu6_3));
        assert!(keys.has_orchard_key(BranchId::Canopy));
        keys.prebuild_more(&[BranchId::Nu6_2, BranchId::Nu6_3]);
        keys.prebuild_more(&[BranchId::Nu6_3]);
        keys.ready();
        assert!(keys.has_orchard_key(BranchId::Nu6_3));
        let None = keys.orchard.built(OrchardCircuitVersion::InsecurePreNu6_2) else {
            panic!("other versions are not built");
        };
        let cold = VerifyingKeys::new();
        cold.ready();
        let None = cold.orchard.built(OrchardCircuitVersion::FixedPostNu6_2) else {
            panic!("no prebuild, no key");
        };
        // Before NU5 there is no Orchard key to build.
        let none = VerifyingKeys::prebuild(RuleEpoch::consensus(BranchId::Canopy), None);
        none.ready();
        let None = none.orchard.built(OrchardCircuitVersion::InsecurePreNu6_2) else {
            panic!("no Orchard before NU5");
        };
    }

    /// A JoinSplit with a BCTV14 proof is not queued: hayai has no verifier for it. A
    /// JoinSplit with a Groth16 proof is queued, and a wrong proof or a wrong signature
    /// fails the transaction.
    #[test]
    fn a_bctv14_joinsplit_is_an_error_and_a_wrong_groth16_joinsplit_fails() {
        use bytes::Bytes;
        use hayai_wire::RawTx;
        use zcash_primitives::transaction::TxVersion;

        use crate::sprout::tests::joinsplit_tx;

        let keys = VerifyingKeys::new();
        for (version, branch) in [
            (TxVersion::Sprout(2), BranchId::Sprout),
            (TxVersion::V3, BranchId::Overwinter),
        ] {
            let bytes = joinsplit_tx(version, branch, Vec::new(), Vec::new(), &[(7, 0, 0)]);
            let raw = RawTx::parse(Bytes::from(bytes), branch).expect("parses");
            let mut batch = ScopedBatch::new(&keys);
            let Err(PrepareError::Unsupported(reason)) =
                batch.add(raw.wtxid(), raw.tx.clone(), [0; 32])
            else {
                panic!("a BCTV14 proof is not queued");
            };
            assert!(reason.contains("BCTV14"), "{reason}");
            assert!(batch.is_empty());
        }
        let branch = BranchId::Canopy;
        let bytes = joinsplit_tx(TxVersion::V4, branch, Vec::new(), Vec::new(), &[(7, 0, 0)]);
        let raw = RawTx::parse(Bytes::from(bytes), branch).expect("parses");
        let mut batch = ScopedBatch::new(&keys);
        batch
            .add(raw.wtxid(), raw.tx.clone(), [0; 32])
            .expect("a Groth16 JoinSplit is queued");
        assert_eq!(batch.len(), 1);
        let outcome = batch.finalize();
        assert_eq!(outcome.failed, vec![raw.wtxid()]);
        assert_eq!(outcome.ok, Vec::new());
    }
}
