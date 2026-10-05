//! The live template: an ordered candidate set, a dependency graph, the current greedy
//! selection under the block limits, and the templates derived from it.
//!
//! The selection is the ZIP 317 block production algorithm with one difference: where the
//! ZIP picks the next candidate at random in proportion to its weight ratio, the walk takes
//! the candidates in the candidate order (highest weight ratio first). The order has the two
//! passes of the ZIP in sequence: first every candidate that pays the conventional fee,
//! then every other candidate. The walk tries each candidate one time. It takes a candidate
//! when the block stays in the size limit, the sigop limit and the per-block limits of the
//! rule set of the height, and the unpaid actions of the block stay at or below
//! [`BLOCK_UNPAID_ACTION_LIMIT`]. A candidate that pays the conventional fee has no unpaid
//! action, so the unpaid action budget only limits the second pass. The walk does not stop
//! at the first candidate that does not fit.
//!
//! Unmined parents (the ZIP does not specify them). A transaction is a candidate of the walk
//! only when the block holds all its unmined parents. A taken candidate can make a child
//! selectable whose position in the order the walk already passed. The walk then tries the
//! child immediately, so it never has to revisit the prefix.
//!
//! After every incremental update, the selection equals what this walk produces from scratch.
//! The templates are therefore independent of the event history. The block order is not the
//! selection order: a template lists the selected set in canonical order (parents first, then
//! txid), so a new high-ratio arrival does not move the transactions around it, and the same
//! set gives the same block bytes on every node (`docs/protocol-compact-relay.md`, Canonical
//! order).
//!
//! Tip events. A new block mines some candidates and conflicts with others. A mined
//! candidate leaves the set, and its children lose the dependency on it: they become
//! selectable on the new tip. A conflicting candidate leaves the set with every descendant.
//!
//! Speculative tips. A node builds the layer of a new block before its scripts and proofs
//! are verified, and moves the template to it with [`LiveTemplate::on_speculative_tip`]. The
//! template keeps the candidates that this tip event dropped, and the dependencies that it
//! released. When the block verifies, the node calls [`LiveTemplate::on_confirm`] and the
//! kept candidates are released. When the verification fails, [`LiveTemplate::on_revert`]
//! restores the parent tip, adds the kept candidates back (except the ones the source
//! removed in the meantime), gives the children their released dependencies back, and
//! emits [`TemplateUpdate::Reverted`].

use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Included, Unbounded};
use std::sync::Arc;
use std::time::Instant;

use ahash::{AHashMap, AHashSet};
use hayai_consensus::{rules_at, BlockLimits};
use hayai_crypto::zcash_primitives::transaction::TxId;
use hayai_wire::header::{BlockHash, BlockHeader, PowParams};
use hayai_wire::WtxId;

use crate::candidate::{Candidate, CandidateSource, OrderKey, SetEvent};
use crate::coinbase::{CoinbaseError, CoinbaseSpec, CoinbaseTx};
use crate::submission::TemplateStore;
use crate::zip317::BLOCK_UNPAID_ACTION_LIMIT;

/// Block version of every template that this crate produces.
pub const BLOCK_VERSION: u32 = 4;
/// Consensus block size limit.
pub const MAX_BLOCK_BYTES: usize = 2_000_000;
/// Bytes reserved for the transaction-count CompactSize. A valid block cannot need more.
const TX_COUNT_PREFIX_BYTES: usize = 5;

/// The sigop limit and the shielded limits of a block are not in the configuration: they
/// are the limits of the rule set of the template height.
#[derive(Clone, Debug)]
pub struct TemplateConfig {
    pub max_block_bytes: usize,
    pub coinbase: CoinbaseSpec,
    /// Time that a template stays available for submission.
    pub retention: std::time::Duration,
    /// The Equihash parameters of the network: they set the header length in the byte
    /// budget and the solution length that a submission must have.
    pub pow: PowParams,
}

impl TemplateConfig {
    pub fn new(coinbase: CoinbaseSpec) -> Self {
        Self {
            max_block_bytes: MAX_BLOCK_BYTES,
            coinbase,
            retention: std::time::Duration::from_secs(60),
            pow: PowParams::MAINNET,
        }
    }
}

/// The chain tip that a template builds on. `history_root` is the chain history tree root
/// after the parent block (ZIP 221): the caller takes it from the parent's layer
/// (`hayai_state::Layer::history_root`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tip {
    pub parent_hash: BlockHash,
    pub height: u32,
    pub time: u32,
    /// The median-time-past of the block at `height`
    /// (`hayai_consensus::difficulty::median_time_past` of the times up to the parent). The
    /// time rules of the header refer to it.
    pub median_time_past: u32,
    pub bits: u32,
    pub history_root: [u8; 32],
    /// The total of the chain value pools after the parent block
    /// (`hayai_state::ValuePools::total`). From the NSM reissuance height the subsidy of
    /// the coinbase depends on it (`CoinbaseTerms::after`). A tip without it at such a
    /// height is an error.
    pub issued_supply: Option<u64>,
}

/// An immutable template, as the node serves it to subscribers and keeps it for
/// submission.
#[derive(Debug)]
pub struct StoredTemplate {
    pub id: u64,
    pub tip: Tip,
    pub coinbase: CoinbaseTx,
    /// Transactions after the coinbase, in canonical block order (parents first, then
    /// txid; `hayai_wire::order`).
    pub txs: Vec<Arc<Candidate>>,
    pub fees_total: u64,
    pub merkle_root: [u8; 32],
    pub auth_data_root: [u8; 32],
    pub block_commitments: [u8; 32],
    /// The Equihash parameters of the network ([`TemplateConfig::pow`]).
    pub pow: PowParams,
    pub created: Instant,
}

impl StoredTemplate {
    /// The header that the pool completes with nonce and solution.
    pub fn header_template(&self, time: u32) -> BlockHeader {
        BlockHeader {
            version: BLOCK_VERSION,
            prev_hash: self.tip.parent_hash,
            merkle_root: self.merkle_root,
            block_commitments: self.block_commitments,
            time,
            bits: self.tip.bits,
            nonce: [0; 32],
            solution: Vec::new(),
        }
    }
}

/// ZIP 244 `hashBlockCommitments`.
pub fn block_commitments(history_root: &[u8; 32], auth_data_root: &[u8; 32]) -> [u8; 32] {
    let hash = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"ZcashBlockCommit")
        .to_state()
        .update(history_root)
        .update(auth_data_root)
        .update(&[0u8; 32])
        .finalize();
    hash.as_bytes().try_into().expect("32-byte hash")
}

/// What a tip event produces, in order.
#[derive(Clone, Debug)]
pub enum TemplateUpdate {
    /// Coinbase-only template for the new tip.
    Empty(Arc<StoredTemplate>),
    /// Full selection for the new tip.
    Full(Arc<StoredTemplate>),
    /// The selection changed under the same tip.
    Changed(Arc<StoredTemplate>),
    /// The speculative block `rejected` failed verification. `template` is the full
    /// template on its parent again.
    Reverted {
        rejected: BlockHash,
        template: Arc<StoredTemplate>,
    },
}

impl TemplateUpdate {
    pub fn template(&self) -> &Arc<StoredTemplate> {
        match self {
            Self::Empty(t) | Self::Full(t) | Self::Changed(t) => t,
            Self::Reverted { template, .. } => template,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ApplyError {
    #[error("candidate {0:?} is already known")]
    Duplicate(WtxId),
    #[error("candidate {child:?} depends on unknown parent {parent:?}")]
    UnknownParent { child: WtxId, parent: Box<WtxId> },
    #[error("repriced candidate {0:?} is unknown")]
    UnknownCandidate(WtxId),
    #[error(transparent)]
    Coinbase(#[from] CoinbaseError),
    #[error("block {0:?} is not the oldest speculative tip")]
    NotOldestSpeculative(BlockHash),
    #[error("no speculative tip follows a template on parent {0:?}")]
    NoSpeculativeChild(BlockHash),
}

struct Entry {
    candidate: Arc<Candidate>,
    children: Vec<WtxId>,
}

/// One speculative tip event: the block the template moved to, the tip before it, the
/// candidates the event dropped, and the children whose dependencies on mined candidates it
/// released (as they were before the event).
struct SpeculativeLevel {
    block: BlockHash,
    previous: Tip,
    dropped: AHashMap<WtxId, Arc<Candidate>>,
    released: Vec<Arc<Candidate>>,
}

/// What a tip event changed in the set: the candidates it dropped, and the children whose
/// dependencies on mined candidates it released, as they were before the event.
struct TipChanges {
    dropped: Vec<Arc<Candidate>>,
    released: Vec<Arc<Candidate>>,
}

/// Remaining room in the block under construction.
#[derive(Clone, Copy, Debug, Default)]
struct Budget {
    bytes: usize,
    sigops: u32,
    /// ZIP 317 unpaid actions that the block can still hold.
    unpaid_actions: u32,
    orchard_actions: u32,
    ironwood_actions: u32,
    sapling_ios: u32,
    /// ZIP 218 `GlobalShieldedBudget`: the three counts above together.
    shielded_cost: u32,
}

/// The shielded cost of `c` (ZIP 218): its Orchard actions, its Ironwood actions and its
/// Sapling spends and outputs.
fn shielded_cost(c: &Candidate) -> u32 {
    c.orchard_actions
        .saturating_add(c.ironwood_actions)
        .saturating_add(c.sapling_ios)
}

impl Budget {
    /// The room of a block that holds only `coinbase`: the block size minus the header, the
    /// transaction count and the coinbase with its script slack; the sigop limit minus the
    /// sigops of the coinbase; the ZIP 317 unpaid action limit; the shielded limits of
    /// `limits`.
    fn fresh(config: &TemplateConfig, limits: &BlockLimits, coinbase: &CoinbaseTx) -> Self {
        Self {
            bytes: config
                .max_block_bytes
                .checked_sub(config.pow.header_len() + TX_COUNT_PREFIX_BYTES)
                .and_then(|b| b.checked_sub(coinbase.bytes.len() + coinbase.script_slack))
                .expect("header and coinbase fit in a block"),
            sigops: limits
                .sigops
                .checked_sub(coinbase.sigops)
                .expect("the sigops of the coinbase fit in a block"),
            unpaid_actions: BLOCK_UNPAID_ACTION_LIMIT,
            orchard_actions: limits.orchard_actions,
            ironwood_actions: limits.ironwood_actions,
            sapling_ios: limits.sapling_ios,
            shielded_cost: limits.shielded_cost,
        }
    }

    fn fits(&self, c: &Candidate) -> bool {
        c.size_bytes() <= self.bytes
            && c.sigops <= self.sigops
            && c.unpaid_actions <= self.unpaid_actions
            && c.orchard_actions <= self.orchard_actions
            && c.ironwood_actions <= self.ironwood_actions
            && c.sapling_ios <= self.sapling_ios
            && shielded_cost(c) <= self.shielded_cost
    }

    fn take(&mut self, c: &Candidate) {
        self.bytes -= c.size_bytes();
        self.sigops -= c.sigops;
        self.unpaid_actions -= c.unpaid_actions;
        self.orchard_actions -= c.orchard_actions;
        self.ironwood_actions -= c.ironwood_actions;
        self.sapling_ios -= c.sapling_ios;
        self.shielded_cost -= shielded_cost(c);
    }

    fn give_back(&mut self, c: &Candidate) {
        self.bytes += c.size_bytes();
        self.sigops += c.sigops;
        self.unpaid_actions += c.unpaid_actions;
        self.orchard_actions += c.orchard_actions;
        self.ironwood_actions += c.ironwood_actions;
        self.sapling_ios += c.sapling_ios;
        self.shielded_cost += shielded_cost(c);
    }
}

struct Selected {
    candidate: Arc<Candidate>,
    /// Cursor position of the walk when it took this candidate: its own key for a direct
    /// pick, or the key of the ancestor for a child taken immediately after its last parent.
    /// The selection vector is non-decreasing in this key.
    picked_at: OrderKey,
}

pub struct LiveTemplate {
    config: TemplateConfig,
    entries: AHashMap<WtxId, Entry>,
    order: BTreeMap<OrderKey, WtxId>,
    selection: Vec<Selected>,
    selected: AHashMap<WtxId, OrderKey>,
    budget: Budget,
    fees_total: u64,
    tip: Option<Tip>,
    /// The room of a block on the tip that holds only the coinbase. No room before a tip is
    /// known.
    tip_budget: Budget,
    next_id: u64,
    current: Option<Arc<StoredTemplate>>,
    store: TemplateStore,
    /// Speculative tip events not yet confirmed or reverted, oldest first.
    speculative: Vec<SpeculativeLevel>,
}

impl LiveTemplate {
    pub fn new(config: TemplateConfig) -> Self {
        Self {
            store: TemplateStore::new(config.retention),
            config,
            entries: AHashMap::new(),
            order: BTreeMap::new(),
            selection: Vec::new(),
            selected: AHashMap::new(),
            budget: Budget::default(),
            fees_total: 0,
            tip: None,
            tip_budget: Budget::default(),
            next_id: 1,
            current: None,
            speculative: Vec::new(),
        }
    }

    /// The latest template, once a tip is known.
    pub fn current(&self) -> Option<&Arc<StoredTemplate>> {
        self.current.as_ref()
    }

    /// Templates kept for submission.
    pub fn store(&self) -> &TemplateStore {
        &self.store
    }

    /// Drops templates older than the configured retention. The newest always stays.
    pub fn prune_templates(&mut self, now: Instant) {
        self.store.prune(now);
    }

    /// Rebuilds the block that a submission describes. See
    /// [`crate::submission::rebuild_block`].
    pub fn rebuild_submission(
        &self,
        submit: &crate::messages::Submit,
    ) -> Result<crate::submission::RebuiltBlock, crate::submission::SubmitError> {
        crate::submission::rebuild_block(&self.store, submit)
    }

    pub fn candidate_count(&self) -> usize {
        self.entries.len()
    }

    /// Selected transactions in selection order (the greedy walk), without the coinbase. The
    /// block order of a template is the canonical order of this set.
    pub fn selection(&self) -> impl Iterator<Item = &Arc<Candidate>> {
        self.selection.iter().map(|s| &s.candidate)
    }

    /// Loads the snapshot of a source and returns its event stream. The caller feeds every
    /// event to [`LiveTemplate::apply`]. The function takes the stream before the snapshot, so
    /// that no change falls between the two.
    pub fn load_from(
        &mut self,
        source: &dyn CandidateSource,
    ) -> Result<
        (
            Option<TemplateUpdate>,
            crossbeam_channel::Receiver<SetEvent>,
        ),
        ApplyError,
    > {
        let events = source.events();
        let update = self.load(source.candidates())?;
        Ok((update, events))
    }

    /// Loads an initial snapshot in any order. Returns the new template if a tip is known.
    pub fn load(
        &mut self,
        candidates: Vec<Candidate>,
    ) -> Result<Option<TemplateUpdate>, ApplyError> {
        self.insert_many(candidates.into_iter().map(Arc::new).collect())?;
        self.rebuild_selection();
        self.emit_if_changed(TemplateUpdate::Changed)
    }

    /// Inserts candidates in any order. Every parent must be in the set or already known.
    fn insert_many(&mut self, candidates: Vec<Arc<Candidate>>) -> Result<(), ApplyError> {
        let mut ids = AHashSet::with_capacity(candidates.len());
        for c in &candidates {
            if self.entries.contains_key(&c.wtxid) || !ids.insert(c.wtxid) {
                return Err(ApplyError::Duplicate(c.wtxid));
            }
        }
        for c in &candidates {
            for parent in &c.depends_on {
                if !ids.contains(parent) && !self.entries.contains_key(parent) {
                    return Err(ApplyError::UnknownParent {
                        child: c.wtxid,
                        parent: Box::new(*parent),
                    });
                }
            }
        }
        let mut edges = Vec::new();
        for c in candidates {
            let id = c.wtxid;
            edges.extend(c.depends_on.iter().map(|parent| (*parent, id)));
            self.order.insert(c.order_key(), id);
            self.entries.insert(
                id,
                Entry {
                    candidate: c,
                    children: Vec::new(),
                },
            );
        }
        for (parent, child) in edges {
            self.entries
                .get_mut(&parent)
                .expect("checked above")
                .children
                .push(child);
        }
        Ok(())
    }

    /// Applies one set event. Returns the new template when the selection changed under a
    /// known tip.
    pub fn apply(&mut self, event: SetEvent) -> Result<Option<TemplateUpdate>, ApplyError> {
        match event {
            SetEvent::Added(candidate) => self.add(candidate)?,
            SetEvent::Removed(wtxid) => {
                self.remove(wtxid);
                // A candidate that a speculative tip event keeps is no longer in the source:
                // a revert must not add it back.
                for level in &mut self.speculative {
                    level.dropped.remove(&wtxid);
                }
            }
            SetEvent::Repriced {
                wtxid,
                fee,
                weight_ratio,
                unpaid_actions,
            } => self.reprice(wtxid, fee, weight_ratio, unpaid_actions)?,
        }
        self.emit_if_changed(TemplateUpdate::Changed)
    }

    /// Switches to a new tip. `mined` lists the candidates that the new block contains: they
    /// leave the set, and their children lose the dependency on them. `conflicting` lists the
    /// candidates that the new block invalidated (they spend an outpoint or reveal a
    /// nullifier that the block consumed): they leave the set with their descendants. `emit`
    /// receives the coinbase-only template before any selection work, and the full template
    /// after the selection rebuild.
    ///
    /// A committed tip ends every speculative tip event: the kept candidates are released.
    pub fn on_tip(
        &mut self,
        tip: Tip,
        mined: &[WtxId],
        conflicting: &[WtxId],
        emit: impl FnMut(TemplateUpdate),
    ) -> Result<(), ApplyError> {
        self.speculative.clear();
        self.switch_tip(tip, mined, conflicting, emit)?;
        Ok(())
    }

    /// [`LiveTemplate::on_tip`] for a block whose layer is speculative (built, scripts and
    /// proofs not yet verified). The template keeps the candidates that the event drops,
    /// and the dependencies that it releases, until [`LiveTemplate::on_confirm`] or
    /// [`LiveTemplate::on_revert`]. The tip's `parent_hash` is the speculative block.
    pub fn on_speculative_tip(
        &mut self,
        tip: Tip,
        mined: &[WtxId],
        conflicting: &[WtxId],
        emit: impl FnMut(TemplateUpdate),
    ) -> Result<(), ApplyError> {
        let Some(previous) = self.tip else {
            // Without a previous tip there is no template to restore on a revert.
            return Err(ApplyError::NoSpeculativeChild(tip.parent_hash));
        };
        let TipChanges { dropped, released } = self.switch_tip(tip, mined, conflicting, emit)?;
        self.speculative.push(SpeculativeLevel {
            block: tip.parent_hash,
            previous,
            dropped: dropped.into_iter().map(|c| (c.wtxid, c)).collect(),
            released,
        });
        Ok(())
    }

    /// The speculative block `block` verified and committed: the candidates its tip event
    /// dropped are released. `block` must be the oldest speculative tip, as the chain
    /// commits speculative layers oldest first.
    pub fn on_confirm(&mut self, block: BlockHash) -> Result<(), ApplyError> {
        match self.speculative.first() {
            Some(level) if level.block == block => {
                self.speculative.remove(0);
                Ok(())
            }
            _ => Err(ApplyError::NotOldestSpeculative(block)),
        }
    }

    /// The speculative block on top of `parent_tip.parent_hash` failed verification. The
    /// template returns to `parent_tip` and adds back the candidates that this tip event and
    /// every later speculative tip event dropped, except the ones that the source removed
    /// since. A kept candidate whose parent is no longer known is not added back. The
    /// children that these events released depend on their mined parents again, for each
    /// parent that is in the set. `emit` receives [`TemplateUpdate::Reverted`] with the full
    /// template on `parent_tip`.
    pub fn on_revert(
        &mut self,
        parent_tip: Tip,
        mut emit: impl FnMut(TemplateUpdate),
    ) -> Result<(), ApplyError> {
        let Some(position) = self
            .speculative
            .iter()
            .position(|level| level.previous.parent_hash == parent_tip.parent_hash)
        else {
            return Err(ApplyError::NoSpeculativeChild(parent_tip.parent_hash));
        };
        let levels: Vec<SpeculativeLevel> = self.speculative.drain(position..).collect();
        let rejected = levels[0].block;
        let mut back: AHashMap<WtxId, Arc<Candidate>> = AHashMap::new();
        let mut released = Vec::new();
        for level in levels {
            released.push(level.released);
            for (id, candidate) in level.dropped {
                if !self.entries.contains_key(&id) {
                    back.insert(id, candidate);
                }
            }
        }
        // A candidate whose parent the source removed while it was kept has no place in
        // the set: drop it, and its own children, until every parent is known.
        loop {
            let orphans: Vec<WtxId> = back
                .values()
                .filter(|c| {
                    c.depends_on
                        .iter()
                        .any(|p| !back.contains_key(p) && !self.entries.contains_key(p))
                })
                .map(|c| c.wtxid)
                .collect();
            if orphans.is_empty() {
                break;
            }
            for id in orphans {
                back.remove(&id);
            }
        }
        self.insert_many(back.into_values().collect())?;
        // The levels are oldest first. The newest level restores first, so the dependency
        // list of the oldest level is the one that stays.
        for before in released.into_iter().rev().flatten() {
            self.restore_dependencies(&before);
        }
        self.set_tip(parent_tip)?;
        self.rebuild_selection();
        let template = self.make_template()?;
        emit(TemplateUpdate::Reverted { rejected, template });
        Ok(())
    }

    /// Gives a held candidate the dependencies of `before` (the candidate before a tip event
    /// released them) on the parents that the set holds. The current fee stays, because the
    /// source can have repriced the candidate since.
    fn restore_dependencies(&mut self, before: &Candidate) {
        let id = before.wtxid;
        let Some(entry) = self.entries.get(&id) else {
            return;
        };
        let current = entry.candidate.clone();
        let depends_on: Vec<WtxId> = before
            .depends_on
            .iter()
            .filter(|p| self.entries.contains_key(p))
            .copied()
            .collect();
        for parent in &depends_on {
            if !current.depends_on.contains(parent) {
                self.entries
                    .get_mut(parent)
                    .expect("filtered above")
                    .children
                    .push(id);
            }
        }
        let mut restored = (*current).clone();
        restored.depends_on = depends_on;
        self.entries.get_mut(&id).expect("held").candidate = Arc::new(restored);
    }

    /// Moves to `tip`: emits the coinbase-only template, drops `mined` and `conflicting` with
    /// the descendants of `conflicting`, releases the children of `mined`, rebuilds the
    /// selection and emits the full template. Returns the dropped candidates and the released
    /// children as they were before.
    fn switch_tip(
        &mut self,
        tip: Tip,
        mined: &[WtxId],
        conflicting: &[WtxId],
        mut emit: impl FnMut(TemplateUpdate),
    ) -> Result<TipChanges, ApplyError> {
        self.set_tip(tip)?;
        self.clear_selection();
        let coinbase = self
            .config
            .coinbase
            .build_on(tip.height, 0, tip.issued_supply)?;
        let empty = self.finish_template(coinbase, Vec::new(), 0);
        emit(TemplateUpdate::Empty(empty));

        let mut dropped = Vec::new();
        for id in mined {
            dropped.extend(self.remove(*id));
        }
        for id in conflicting {
            self.remove_subtree(*id, &mut dropped);
        }
        // After the conflicts: a released child is one that stays in the set.
        let released = self.release_children(mined);
        self.rebuild_selection();
        let full = self.make_template()?;
        emit(TemplateUpdate::Full(full));
        Ok(TipChanges { dropped, released })
    }

    /// Removes the `mined` candidates from the dependencies of every candidate in the set.
    /// The scan covers the whole set, because the source can remove a mined parent
    /// (`SetEvent::Removed`) before the tip event, and the parent's child list leaves with
    /// it. Returns the changed candidates as they were before.
    fn release_children(&mut self, mined: &[WtxId]) -> Vec<Arc<Candidate>> {
        if mined.is_empty() {
            return Vec::new();
        }
        let mined: AHashSet<WtxId> = mined.iter().copied().collect();
        let mut before = Vec::new();
        for entry in self.entries.values_mut() {
            if !entry.candidate.depends_on.iter().any(|p| mined.contains(p)) {
                continue;
            }
            let mut released = (*entry.candidate).clone();
            released.depends_on.retain(|p| !mined.contains(p));
            before.push(std::mem::replace(&mut entry.candidate, Arc::new(released)));
        }
        before
    }

    /// Sets the tip and the room of a block of its height: the limits of the rule set of
    /// the height, minus the coinbase. A height without a rule set is an error.
    fn set_tip(&mut self, tip: Tip) -> Result<(), ApplyError> {
        let coinbase = self
            .config
            .coinbase
            .build_on(tip.height, 0, tip.issued_supply)?;
        let rules =
            rules_at(self.config.coinbase.network, tip.height).map_err(CoinbaseError::Terms)?;
        self.tip = Some(tip);
        self.tip_budget = Budget::fresh(&self.config, &rules.limits, &coinbase);
        Ok(())
    }

    fn add(&mut self, candidate: Candidate) -> Result<(), ApplyError> {
        let id = candidate.wtxid;
        if self.entries.contains_key(&id) {
            return Err(ApplyError::Duplicate(id));
        }
        for parent in &candidate.depends_on {
            if !self.entries.contains_key(parent) {
                return Err(ApplyError::UnknownParent {
                    child: id,
                    parent: Box::new(*parent),
                });
            }
        }
        let key = candidate.order_key();
        for parent in &candidate.depends_on {
            self.entries
                .get_mut(parent)
                .expect("checked above")
                .children
                .push(id);
        }
        let candidate = Arc::new(candidate);
        self.order.insert(key, id);
        self.entries.insert(
            id,
            Entry {
                candidate: candidate.clone(),
                children: Vec::new(),
            },
        );
        let Some(_) = self.tip else {
            return Ok(());
        };
        if !self.parents_selected(&candidate) {
            // The walk skips it wherever it sits.
            return Ok(());
        }
        let cutoff = self.selection.last().map(|s| s.picked_at);
        // The walk takes it at its own position only if it took every parent before it reached
        // this position. Otherwise, the walk takes it immediately after its last parent, as a
        // child pick.
        let direct_pick = candidate.depends_on.iter().all(|p| self.selected[p] < key);
        match cutoff {
            Some(cutoff) if key < cutoff => {
                if direct_pick && self.budget.fits(&candidate) {
                    // Everything taken after this position still fits after it. Each later
                    // remaining budget is at least the final one, and the final one holds this
                    // candidate.
                    let pos = self.selection.partition_point(|s| s.picked_at < key);
                    self.take(candidate, key, Some(pos));
                } else {
                    self.rollback_from(key);
                    self.walk(Included(key));
                }
            }
            _ => {
                if self.budget.fits(&candidate) {
                    self.take(candidate, key, None);
                }
            }
        }
        Ok(())
    }

    /// Removes `id` from the set and returns it, or `None` when the set does not hold it.
    fn remove(&mut self, id: WtxId) -> Option<Arc<Candidate>> {
        let entry = self.entries.remove(&id)?;
        let key = entry.candidate.order_key();
        self.order.remove(&key);
        for parent in &entry.candidate.depends_on {
            if let Some(p) = self.entries.get_mut(parent) {
                p.children.retain(|c| *c != id);
            }
        }
        if let Some(picked_at) = self.selected.get(&id).copied() {
            self.rollback_from(picked_at);
            self.walk(Included(picked_at));
        }
        Some(entry.candidate)
    }

    /// Removes `id` and every descendant, and appends them to `removed`.
    fn remove_subtree(&mut self, id: WtxId, removed: &mut Vec<Arc<Candidate>>) {
        let mut stack = vec![id];
        while let Some(next) = stack.pop() {
            if let Some(entry) = self.entries.get(&next) {
                stack.extend(entry.children.iter().copied());
                removed.extend(self.remove(next));
            }
        }
    }

    fn reprice(
        &mut self,
        id: WtxId,
        fee: u64,
        weight_ratio: crate::zip317::WeightRatio,
        unpaid_actions: u32,
    ) -> Result<(), ApplyError> {
        let Some(entry) = self.entries.get_mut(&id) else {
            return Err(ApplyError::UnknownCandidate(id));
        };
        let old_key = entry.candidate.order_key();
        let mut updated = (*entry.candidate).clone();
        updated.fee = fee;
        updated.weight_ratio = weight_ratio;
        updated.unpaid_actions = unpaid_actions;
        let new_key = updated.order_key();
        entry.candidate = Arc::new(updated);
        self.order.remove(&old_key);
        self.order.insert(new_key, id);
        let Some(_) = self.tip else {
            return Ok(());
        };
        let from = old_key.min(new_key);
        self.rollback_from(from);
        self.walk(Included(from));
        Ok(())
    }

    fn parents_selected(&self, c: &Candidate) -> bool {
        c.depends_on.iter().all(|p| self.selected.contains_key(p))
    }

    fn clear_selection(&mut self) {
        self.selection.clear();
        self.selected.clear();
        self.fees_total = 0;
        self.budget = self.tip_budget;
    }

    fn rebuild_selection(&mut self) {
        let Some(_) = self.tip else {
            return;
        };
        self.clear_selection();
        self.walk(Unbounded);
    }

    /// Drops every selected candidate taken at or after cursor position `key` and restores the
    /// budget. The walk can then resume from `key`.
    fn rollback_from(&mut self, key: OrderKey) {
        let pos = self.selection.partition_point(|s| s.picked_at < key);
        for s in self.selection.drain(pos..) {
            self.budget.give_back(&s.candidate);
            self.fees_total -= s.candidate.fee;
            self.selected.remove(&s.candidate.wtxid);
        }
    }

    /// The greedy walk over the order from `from` to the end.
    fn walk(&mut self, from: std::ops::Bound<OrderKey>) {
        let mut cursor = from;
        loop {
            let next = self
                .order
                .range((cursor, Unbounded))
                .next()
                .map(|(k, id)| (*k, *id));
            let Some((key, id)) = next else {
                break;
            };
            cursor = Excluded(key);
            self.try_select(id, key);
        }
    }

    /// Takes `id` if it is selectable at cursor position `cursor`. Then it takes any child
    /// whose own position the walk has already passed.
    fn try_select(&mut self, id: WtxId, cursor: OrderKey) -> bool {
        if self.selected.contains_key(&id) {
            return false;
        }
        let candidate = self.entries[&id].candidate.clone();
        if !self.parents_selected(&candidate) || !self.budget.fits(&candidate) {
            return false;
        }
        self.take(candidate, cursor, None);
        let mut children: Vec<(OrderKey, WtxId)> = self.entries[&id]
            .children
            .iter()
            .map(|c| (self.entries[c].candidate.order_key(), *c))
            .filter(|(k, _)| *k < cursor)
            .collect();
        children.sort_unstable();
        for (_, child) in children {
            self.try_select(child, cursor);
        }
        true
    }

    fn take(&mut self, candidate: Arc<Candidate>, picked_at: OrderKey, at: Option<usize>) {
        self.budget.take(&candidate);
        self.fees_total += candidate.fee;
        self.selected.insert(candidate.wtxid, picked_at);
        let selected = Selected {
            candidate,
            picked_at,
        };
        match at {
            Some(pos) => self.selection.insert(pos, selected),
            None => self.selection.push(selected),
        }
    }

    /// The selection in canonical block order (`hayai_wire::order`): parents first, then
    /// txid. The block bytes therefore depend on the selected set only.
    fn block_order(&self) -> Vec<Arc<Candidate>> {
        let txids: Vec<TxId> = self
            .selection
            .iter()
            .map(|s| s.candidate.wtxid.txid)
            .collect();
        let spends: Vec<Vec<TxId>> = self
            .selection
            .iter()
            .map(|s| {
                s.candidate
                    .spends
                    .iter()
                    .map(|outpoint| TxId::from_bytes(*outpoint.hash()))
                    .collect()
            })
            .collect();
        let Ok(order) = hayai_wire::canonical_order(&txids, &spends) else {
            unreachable!("a txid commits to its inputs, so spends cannot form a cycle");
        };
        order
            .into_iter()
            .map(|i| self.selection[i].candidate.clone())
            .collect()
    }

    fn emit_if_changed(
        &mut self,
        wrap: fn(Arc<StoredTemplate>) -> TemplateUpdate,
    ) -> Result<Option<TemplateUpdate>, ApplyError> {
        let Some(_) = self.tip else {
            return Ok(None);
        };
        let txs = self.block_order();
        let unchanged = matches!(&self.current, Some(current)
            if current.txs.len() == txs.len()
                && current.txs.iter().zip(&txs).all(|(a, b)| Arc::ptr_eq(a, b)));
        if unchanged {
            return Ok(None);
        }
        Ok(Some(wrap(self.template_of(txs)?)))
    }

    fn make_template(&mut self) -> Result<Arc<StoredTemplate>, ApplyError> {
        let txs = self.block_order();
        self.template_of(txs)
    }

    fn template_of(&mut self, txs: Vec<Arc<Candidate>>) -> Result<Arc<StoredTemplate>, ApplyError> {
        let tip = self.tip.expect("caller checked the tip");
        let coinbase =
            self.config
                .coinbase
                .build_on(tip.height, self.fees_total, tip.issued_supply)?;
        Ok(self.finish_template(coinbase, txs, self.fees_total))
    }

    fn finish_template(
        &mut self,
        coinbase: CoinbaseTx,
        txs: Vec<Arc<Candidate>>,
        fees_total: u64,
    ) -> Arc<StoredTemplate> {
        let tip = self.tip.expect("caller checked the tip");
        let (merkle_root, auth_data_root) = roots(&coinbase, &txs);
        let template = Arc::new(StoredTemplate {
            id: self.next_id,
            tip,
            coinbase,
            txs,
            fees_total,
            merkle_root,
            auth_data_root,
            block_commitments: block_commitments(&tip.history_root, &auth_data_root),
            pow: self.config.pow,
            created: Instant::now(),
        });
        self.next_id += 1;
        self.store.insert(template.clone());
        self.current = Some(template.clone());
        template
    }
}

/// Merkle root and ZIP 244 auth data root of the coinbase followed by `txs`.
pub fn roots(coinbase: &CoinbaseTx, txs: &[Arc<Candidate>]) -> ([u8; 32], [u8; 32]) {
    let mut txids = Vec::with_capacity(txs.len() + 1);
    let mut digests = Vec::with_capacity(txs.len() + 1);
    txids.push(coinbase.txid);
    digests.push(coinbase.auth_digest);
    for tx in txs {
        txids.push(tx.wtxid.txid);
        digests.push(tx.wtxid.auth_digest);
    }
    (
        hayai_wire::merkle_root(&txids),
        hayai_wire::auth_data_root(&digests),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{candidate, coinbase_spec, tip};
    use crate::zip317::Zip317Params;

    fn live() -> LiveTemplate {
        LiveTemplate::new(TemplateConfig::new(coinbase_spec()))
    }

    fn ids(t: &StoredTemplate) -> Vec<WtxId> {
        t.txs.iter().map(|c| c.wtxid).collect()
    }

    /// Rebuilds the selection from the same candidate set in a fresh instance.
    fn from_scratch(live: &LiveTemplate) -> Vec<WtxId> {
        let mut fresh = LiveTemplate::new(live.config.clone());
        let mut all: Vec<Candidate> = live
            .entries
            .values()
            .map(|e| (*e.candidate).clone())
            .collect();
        all.sort_by_key(|c| c.wtxid);
        fresh.load(all).unwrap();
        let tip = live.tip.unwrap();
        let mut full = None;
        fresh
            .on_tip(tip, &[], &[], |u| {
                if let TemplateUpdate::Full(t) = u {
                    full = Some(t);
                }
            })
            .unwrap();
        ids(&full.unwrap())
    }

    /// The event that gives `candidate` the fee `fee`.
    fn repriced(candidate: &Candidate, fee: u64) -> SetEvent {
        let params = Zip317Params::ZIP317;
        SetEvent::Repriced {
            wtxid: candidate.wtxid,
            fee,
            weight_ratio: params.weight_ratio(fee, candidate.conventional_fee),
            unpaid_actions: params.unpaid_actions(fee, candidate.conventional_fee),
        }
    }

    /// The ZIP 317 block production algorithm as two explicit passes over a sorted list,
    /// with the candidate order in place of the random pick. Pass 1 tries the candidates
    /// that pay the conventional fee, pass 2 tries the others. A transaction whose turn
    /// comes before the block holds all its parents waits, and the algorithm tries it
    /// immediately after the last parent.
    fn two_pass_reference(live: &LiveTemplate) -> AHashSet<WtxId> {
        struct Run<'a> {
            live: &'a LiveTemplate,
            budget: Budget,
            taken: AHashSet<WtxId>,
            waiting: AHashSet<WtxId>,
        }
        impl Run<'_> {
            fn try_take(&mut self, c: &Candidate) {
                if !self.budget.fits(c) {
                    return;
                }
                self.budget.take(c);
                self.taken.insert(c.wtxid);
                let mut children: Vec<&Candidate> = self.live.entries[&c.wtxid]
                    .children
                    .iter()
                    .map(|id| &*self.live.entries[id].candidate)
                    .collect();
                children.sort_by_key(|child| child.order_key());
                for child in children {
                    let ready = child.depends_on.iter().all(|p| self.taken.contains(p));
                    if ready && self.waiting.remove(&child.wtxid) {
                        self.try_take(child);
                    }
                }
            }
        }
        let mut sorted: Vec<&Candidate> = live.entries.values().map(|e| &*e.candidate).collect();
        sorted.sort_by_key(|c| c.order_key());
        let mut run = Run {
            live,
            budget: live.tip_budget,
            taken: AHashSet::new(),
            waiting: AHashSet::new(),
        };
        for pays_conventional_fee in [true, false] {
            for c in &sorted {
                if (c.fee >= c.conventional_fee) != pays_conventional_fee
                    || run.taken.contains(&c.wtxid)
                {
                    continue;
                }
                if c.depends_on.iter().all(|p| run.taken.contains(p)) {
                    run.try_take(c);
                } else {
                    run.waiting.insert(c.wtxid);
                }
            }
        }
        run.taken
    }

    /// The block order of the current template.
    fn current_ids(live: &LiveTemplate) -> Vec<WtxId> {
        ids(live.current().expect("a tip is known"))
    }

    /// `ids` sorted by txid: the canonical order of a set without parents in it.
    fn by_txid(mut ids: Vec<WtxId>) -> Vec<WtxId> {
        ids.sort_by(|a, b| a.txid.as_ref().cmp(b.txid.as_ref()));
        ids
    }

    #[test]
    fn tip_event_emits_empty_then_full_in_canonical_order() {
        let mut live = live();
        let low = candidate(1, 10_000, 1, &[]);
        let high = candidate(2, 40_000, 1, &[]);
        let mid = candidate(3, 20_000, 1, &[]);
        let None = live
            .load(vec![low.clone(), high.clone(), mid.clone()])
            .unwrap()
        else {
            panic!("no template before a tip");
        };
        let mut updates = Vec::new();
        live.on_tip(tip(100), &[], &[], |u| updates.push(u))
            .unwrap();
        let [TemplateUpdate::Empty(empty), TemplateUpdate::Full(full)] = updates.as_slice() else {
            panic!("expected Empty then Full, got {updates:?}");
        };
        assert!(empty.txs.is_empty());
        assert_eq!(empty.fees_total, 0);
        // Selection by weight ratio, block order by txid.
        let selected: Vec<WtxId> = live.selection().map(|c| c.wtxid).collect();
        assert_eq!(selected, vec![high.wtxid, mid.wtxid, low.wtxid]);
        assert_eq!(ids(full), by_txid(selected));
        assert_eq!(full.fees_total, 70_000);
        assert_ne!(full.merkle_root, empty.merkle_root);
        assert_eq!(full.id, empty.id + 1);
        assert_eq!(live.store().get(empty.id).unwrap().id, empty.id);
    }

    #[test]
    fn child_is_selected_only_after_its_parents() {
        let mut live = live();
        let parent = candidate(1, 10_000, 1, &[]);
        let child = candidate(2, 40_000, 1, &[parent.wtxid]);
        // The child arrives first in the snapshot. The load tolerates any order.
        live.load(vec![child.clone(), parent.clone()]).unwrap();
        live.on_tip(tip(1), &[], &[], |_| {}).unwrap();
        assert_eq!(current_ids(&live), vec![parent.wtxid, child.wtxid]);
        assert_eq!(current_ids(&live), from_scratch(&live));

        // The removal of the parent drops the child from the selection too.
        let update = live
            .apply(SetEvent::Removed(parent.wtxid))
            .unwrap()
            .unwrap();
        assert!(update.template().txs.is_empty());
        assert!(current_ids(&live).is_empty());
        // The removal of an id that is already gone is a no-op.
        let None = live.apply(SetEvent::Removed(parent.wtxid)).unwrap() else {
            panic!("removing an unknown id must not change the template");
        };
    }

    #[test]
    fn load_from_a_source_then_drain_its_events() {
        struct Source {
            snapshot: Vec<Candidate>,
            events: crossbeam_channel::Receiver<SetEvent>,
        }
        impl CandidateSource for Source {
            fn candidates(&self) -> Vec<Candidate> {
                self.snapshot.clone()
            }
            fn events(&self) -> crossbeam_channel::Receiver<SetEvent> {
                self.events.clone()
            }
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let a = candidate(1, 10_000, 1, &[]);
        let b = candidate(2, 20_000, 1, &[]);
        let source = Source {
            snapshot: vec![a.clone()],
            events: rx,
        };
        tx.send(SetEvent::Added(b.clone())).unwrap();
        let mut live = live();
        live.on_tip(tip(1), &[], &[], |_| {}).unwrap();
        let (update, events) = live.load_from(&source).unwrap();
        assert_eq!(ids(update.unwrap().template()), vec![a.wtxid]);
        while let Ok(event) = events.try_recv() {
            live.apply(event).unwrap();
        }
        assert_eq!(current_ids(&live), by_txid(vec![b.wtxid, a.wtxid]));
    }

    #[test]
    fn unknown_parent_and_duplicates_are_errors() {
        let mut live = live();
        let parent = candidate(1, 10_000, 1, &[]);
        let child = candidate(2, 40_000, 1, &[parent.wtxid]);
        assert!(matches!(
            live.apply(SetEvent::Added(child.clone())),
            Err(ApplyError::UnknownParent { .. })
        ));
        live.apply(SetEvent::Added(parent.clone())).unwrap();
        assert!(matches!(
            live.apply(SetEvent::Added(parent)),
            Err(ApplyError::Duplicate(_))
        ));
        assert!(matches!(
            live.apply(repriced(&child, 1)),
            Err(ApplyError::UnknownCandidate(_))
        ));
    }

    #[test]
    fn incremental_add_matches_from_scratch_under_a_tight_byte_limit() {
        let mut seed = 0;
        let mut next = |fee: u64, n_out: usize| {
            seed += 1;
            candidate(seed, fee, n_out, &[])
        };
        let initial = [
            next(30_000, 1),
            next(36_000, 1),
            next(27_000, 1),
            next(33_000, 2),
        ];
        // Room for the header, the coinbase and exactly the four initial transactions.
        let coinbase = coinbase_spec().build(5, 0).unwrap();
        let mut config = TemplateConfig::new(coinbase_spec());
        config.max_block_bytes = PowParams::MAINNET.header_len()
            + TX_COUNT_PREFIX_BYTES
            + coinbase.bytes.len()
            + coinbase.script_slack
            + initial.iter().map(|c| c.size_bytes()).sum::<usize>();
        let mut live = LiveTemplate::new(config);
        live.on_tip(tip(5), &[], &[], |_| {}).unwrap();
        for c in initial {
            live.apply(SetEvent::Added(c)).unwrap();
        }
        assert_eq!(live.selection().count(), 4);
        assert_eq!(current_ids(&live), from_scratch(&live));
        let before = current_ids(&live);
        // A low-fee candidate that does not fit changes nothing.
        let None = live.apply(SetEvent::Added(next(24_000, 3))).unwrap() else {
            panic!("a candidate that does not fit must not change the template");
        };
        assert_eq!(current_ids(&live), before);
        // A high-fee candidate that does not fit in the final budget displaces the tail.
        let Some(_) = live.apply(SetEvent::Added(next(150_000, 3))).unwrap() else {
            panic!("a displacing candidate must produce a template");
        };
        assert_eq!(current_ids(&live), from_scratch(&live));
        assert_ne!(current_ids(&live), before);
        // The removal of a selected one refills from the cutoff.
        let victim = current_ids(&live)[0];
        live.apply(SetEvent::Removed(victim)).unwrap().unwrap();
        assert_eq!(current_ids(&live), from_scratch(&live));
        // A reprice moves a candidate across the cutoff.
        let last = live.selection().last().unwrap().wtxid;
        let event = repriced(&live.entries[&last].candidate, 300_000);
        live.apply(event).unwrap();
        assert_eq!(live.selection().next().unwrap().wtxid, last);
        assert_eq!(current_ids(&live), from_scratch(&live));
    }

    /// The sigop limit is the limit of the rule set minus the sigops of the coinbase. A
    /// rule set before NU7 has no shielded limit.
    #[test]
    fn the_limits_are_the_limits_of_the_rule_set_of_the_height() {
        let mut spec = coinbase_spec();
        // A P2PKH script: 1 sigop.
        spec.script_pubkey = [&[0x76, 0xa9, 0x14][..], &[7; 20], &[0x88, 0xac]].concat();
        let mut live = LiveTemplate::new(TemplateConfig::new(spec));
        live.on_tip(tip(5), &[], &[], |_| {}).unwrap();
        let coinbase_sigops = live.current().unwrap().coinbase.sigops;
        assert_eq!(coinbase_sigops, 1);
        let limit = BlockLimits::PRE_NU7.sigops;
        let mut over = candidate(1, 40_000, 1, &[]);
        over.sigops = limit;
        let mut exact = candidate(2, 30_000, 1, &[]);
        exact.sigops = limit - coinbase_sigops - 1;
        let mut one = candidate(3, 20_000, 1, &[]);
        one.sigops = 1;
        let mut second = candidate(4, 10_000, 1, &[]);
        second.sigops = 1;
        let mut shielded = candidate(5, 10_000, 1, &[]);
        shielded.sigops = 0;
        shielded.orchard_actions = 5_000;
        shielded.ironwood_actions = 5_000;
        shielded.sapling_ios = 5_000;
        for c in [over, exact.clone(), one.clone(), second, shielded.clone()] {
            live.apply(SetEvent::Added(c)).unwrap();
        }
        let selected: AHashSet<WtxId> = live.selection().map(|c| c.wtxid).collect();
        assert_eq!(
            selected,
            AHashSet::from_iter([exact.wtxid, one.wtxid, shielded.wtxid])
        );
        assert_eq!(live.budget.sigops, 0);
        assert_eq!(current_ids(&live), from_scratch(&live));
    }

    /// The limits of the NU7 rule set (ZIP 218). The test gives the limits to the budget
    /// directly, so it runs on a backend without the NU7 rule set too.
    #[test]
    fn shielded_limits_bound_the_selection() {
        let mut live = live();
        live.on_tip(tip(5), &[], &[], |_| {}).unwrap();
        let coinbase = live.current().unwrap().coinbase.clone();
        live.tip_budget = Budget::fresh(&live.config, &BlockLimits::NU7, &coinbase);
        let mut heavy = candidate(1, 40_000, 1, &[]);
        heavy.orchard_actions = 331;
        let mut sapling = candidate(3, 40_000, 1, &[]);
        sapling.sapling_ios = 301;
        let mut ironwood = candidate(5, 40_000, 1, &[]);
        ironwood.ironwood_actions = 331;
        let ok = candidate(4, 10_000, 1, &[]);
        // The Ironwood budget is the total of the block: two candidates of 200 actions do
        // not fit together. The shielded cost is the total of the three pools: after 200
        // Ironwood actions, 131 Orchard actions do not fit, 130 fit, and then one Sapling
        // output does not fit.
        let mut first = candidate(6, 50_000, 1, &[]);
        first.ironwood_actions = 200;
        let mut second = candidate(7, 45_000, 1, &[]);
        second.ironwood_actions = 200;
        let mut too_much = candidate(8, 44_000, 1, &[]);
        too_much.orchard_actions = 131;
        let mut orchard = candidate(9, 43_000, 1, &[]);
        orchard.orchard_actions = 130;
        let mut one_more = candidate(10, 42_000, 1, &[]);
        one_more.sapling_ios = 1;
        live.load(vec![
            heavy,
            sapling,
            ironwood,
            ok.clone(),
            first.clone(),
            second,
            too_much,
            orchard.clone(),
            one_more,
        ])
        .unwrap();
        let selected: AHashSet<WtxId> = live.selection().map(|c| c.wtxid).collect();
        assert_eq!(
            selected,
            AHashSet::from_iter([first.wtxid, orchard.wtxid, ok.wtxid])
        );
        assert_eq!(live.budget.shielded_cost, 0);
    }

    /// ZIP 317, second pass, with the unpaid action limit of 0: a candidate with an unpaid
    /// action is in no template, at each fee below its conventional fee. A candidate that
    /// pays the conventional fee is in the template. A candidate that does not fit does
    /// not stop the pass.
    #[test]
    fn the_unpaid_action_limit_of_zero_bounds_the_second_pass() {
        assert_eq!(BLOCK_UNPAID_ACTION_LIMIT, 0);
        let unpaid =
            |live: &LiveTemplate| -> u32 { live.selection().map(|c| c.unpaid_actions).sum() };
        let mut live = live();
        live.on_tip(tip(5), &[], &[], |_| {}).unwrap();
        assert_eq!(live.budget.unpaid_actions, 0);
        // One zatoshi below the conventional fee: 1 unpaid action, not in the template.
        let one = candidate(1, 9_999, 1, &[]);
        assert_eq!((one.conventional_fee, one.unpaid_actions), (10_000, 1));
        let None = live.apply(SetEvent::Added(one.clone())).unwrap() else {
            panic!("a candidate with an unpaid action must not change the template");
        };
        // 2 unpaid actions and 3 unpaid actions: not in the template.
        let two = candidate(2, 4_000, 1, &[]);
        let three = candidate(3, 7_000, 4, &[]);
        assert_eq!((three.conventional_fee, three.unpaid_actions), (20_000, 3));
        for c in [&two, &three] {
            let None = live.apply(SetEvent::Added(c.clone())).unwrap() else {
                panic!("a candidate with unpaid actions must not change the template");
            };
        }
        assert_eq!(live.selection().count(), 0);
        // The pass continues after them: a candidate at its conventional fee has no
        // unpaid action and fits, in the first pass or in the second.
        let paid = candidate(4, 10_000, 1, &[]);
        assert_eq!(paid.unpaid_actions, 0);
        live.apply(SetEvent::Added(paid.clone())).unwrap().unwrap();
        let ids = |live: &LiveTemplate| -> AHashSet<WtxId> {
            live.selection().map(|c| c.wtxid).collect()
        };
        assert_eq!(ids(&live), AHashSet::from_iter([paid.wtxid]));
        assert_eq!(current_ids(&live), from_scratch(&live));
        // A higher fee: the candidate is in the template when it pays its conventional
        // fee, and not one zatoshi below it.
        let None = live.apply(repriced(&three, 19_999)).unwrap() else {
            panic!("one unpaid action is above the limit");
        };
        live.apply(repriced(&three, 20_000)).unwrap().unwrap();
        assert_eq!(ids(&live), AHashSet::from_iter([paid.wtxid, three.wtxid]));
        // A lower fee takes a candidate out of the template.
        live.apply(repriced(&paid, 9_999)).unwrap().unwrap();
        assert_eq!(ids(&live), AHashSet::from_iter([three.wtxid]));
        assert_eq!(unpaid(&live), BLOCK_UNPAID_ACTION_LIMIT);
        assert_eq!(ids(&live), two_pass_reference(&live));
        assert_eq!(current_ids(&live), from_scratch(&live));
    }

    /// The template across NU7 on Testnet. With the NU7 rule set: the template of the NU7
    /// height has the NU7 branch id, the limits of ZIP 218 and a coinbase with the miner
    /// share of the fees. Without it: the tip is an error, and the template does not use
    /// the rules of another upgrade.
    #[test]
    fn the_template_follows_the_rules_at_the_nu7_boundary() {
        use hayai_consensus::{ConsensusError, Network, RuleSet, Upgrade};
        use hayai_crypto::zcash_protocol::consensus::BranchId;
        let Some(nu7) = Network::Testnet.activation_height(Upgrade::Nu7) else {
            panic!("Testnet has an NU7 height on every backend");
        };
        let mut spec = coinbase_spec();
        spec.network = Network::Testnet;
        let mut live = LiveTemplate::new(TemplateConfig::new(spec.clone()));
        let paying = candidate(1, 10_000, 1, &[]);
        live.on_tip(tip(nu7 - 1), &[], &[], |_| {}).unwrap();
        live.apply(SetEvent::Added(paying.clone())).unwrap();
        let before = live.current().unwrap();
        assert_eq!(before.coinbase.branch_id, BranchId::Nu6_3);
        assert_eq!(before.coinbase, spec.build(nu7 - 1, 10_000).unwrap());
        assert_eq!(live.tip_budget.shielded_cost, u32::MAX);
        for height in [nu7, nu7 + 1] {
            let Some(rules) = RuleSet::of(Upgrade::Nu7) else {
                assert!(matches!(
                    live.on_tip(tip(height), &[], &[], |_| {}),
                    Err(ApplyError::Coinbase(CoinbaseError::Terms(
                        ConsensusError::UnsupportedUpgrade { .. }
                    )))
                ));
                continue;
            };
            live.on_tip(tip(height), &[], &[], |_| {}).unwrap();
            let template = live.current().unwrap();
            assert_eq!(template.coinbase.branch_id, rules.branch_id);
            assert_eq!(template.fees_total, 10_000);
            // The coinbase of the template is the coinbase of the fees of the template:
            // the miner gets 4,000 of the 10,000 zatoshis.
            assert_eq!(template.coinbase, spec.build(height, 10_000).unwrap());
            assert_ne!(template.coinbase, spec.build(height, 4_000).unwrap());
            assert_eq!(live.tip_budget.orchard_actions, 330);
            assert_eq!(live.tip_budget.ironwood_actions, 330);
            assert_eq!(live.tip_budget.sapling_ios, 300);
            assert_eq!(live.tip_budget.shielded_cost, 330);
        }
    }

    /// The template at the NSM reissuance height of a test Regtest (NU7 at 9, reissuance
    /// at 12), at the height before and at the height after. The tip carries the total
    /// of the chain value pools after the parent, and the coinbase of the template is the
    /// coinbase of that total and of the fees of the template. A tip without the total
    /// is an error from the reissuance height.
    #[test]
    fn the_template_has_the_reissuance_bonus_from_the_reissuance_height() {
        use hayai_consensus::{subsidy, ConsensusError, RegtestConfig, RuleSet, Upgrade};

        let network = RegtestConfig::new(&[(Upgrade::Nu7, 9)], Vec::new(), 0)
            .expect("a valid configuration")
            .with_test_reissuance_height(12)
            .network();
        let Some(_) = RuleSet::of(Upgrade::Nu7) else {
            return;
        };
        let mut spec = coinbase_spec();
        spec.network = network;
        let mut live = LiveTemplate::new(TemplateConfig::new(spec.clone()));
        let balance = 4_000_000_000u64;
        let tip_with = |height: u32, with_supply: bool| {
            let scheduled = subsidy::scheduled_issuance(network, height - 1);
            let issued = u64::try_from(scheduled).expect("fits") - balance;
            Tip {
                issued_supply: with_supply.then_some(issued),
                ..tip(height)
            }
        };
        live.on_tip(tip_with(11, true), &[], &[], |_| {}).unwrap();
        live.apply(SetEvent::Added(candidate(1, 10_000, 1, &[])))
            .unwrap();
        for height in [11, 12, 13] {
            let tip = tip_with(height, true);
            let mut updates = Vec::new();
            live.on_tip(tip, &[], &[], |update| updates.push(update))
                .unwrap();
            let template = live.current().unwrap();
            assert_eq!(template.tip, tip);
            assert_eq!(template.fees_total, 10_000);
            assert_eq!(template.coinbase.miner_fees, 4_000);
            assert_eq!(
                template.coinbase,
                spec.build_on(height, 10_000, tip.issued_supply).unwrap()
            );
            // The empty template of the tip has the bonus too.
            let Some(TemplateUpdate::Empty(empty)) = updates.first() else {
                panic!("a tip event emits the empty template first");
            };
            assert_eq!(
                empty.coinbase,
                spec.build_on(height, 0, tip.issued_supply).unwrap()
            );
            // The bonus of 550 zatoshis is in the coinbase from height 12 only.
            let without_bonus =
                spec.build_on(height, 10_000, tip.issued_supply.map(|s| s + balance));
            assert_eq!(
                without_bonus.unwrap() == template.coinbase,
                height == 11,
                "{height}"
            );
        }
        // A tip without the total: a template before the reissuance height, an error
        // from it. The template does not use a subsidy without the bonus.
        live.on_tip(tip_with(11, false), &[], &[], |_| {}).unwrap();
        for height in [12, 13] {
            assert!(matches!(
                live.on_tip(tip_with(height, false), &[], &[], |_| {}),
                Err(ApplyError::Coinbase(CoinbaseError::Terms(
                    ConsensusError::IssuedSupplyUnknown { height: h }
                ))) if h == height
            ));
        }
    }

    /// Parent `p` and child `c` in the set; a block mines `p`; the next template holds `c`,
    /// without the dependency. The source's removal of `p` can come before or after the
    /// tip event. A child that conflicts with the block leaves with its own descendants.
    #[test]
    fn children_of_mined_candidates_stay_and_become_selectable() {
        let p = candidate(1, 10_000, 1, &[]);
        let c = candidate(2, 50_000, 1, &[p.wtxid]);
        let g = candidate(3, 40_000, 1, &[c.wtxid]);
        let full_after = |t: &mut LiveTemplate, mined: &[WtxId], conflicting: &[WtxId]| {
            let mut full = None;
            t.on_tip(tip(2), mined, conflicting, |u| {
                if let TemplateUpdate::Full(t) = u {
                    full = Some(t);
                }
            })
            .unwrap();
            full.expect("a tip event emits the full template")
        };

        // The removal of `p` follows the tip event, as in hayaid.
        let mut first = live();
        first.load(vec![p.clone(), c.clone(), g.clone()]).unwrap();
        first.on_tip(tip(1), &[], &[], |_| {}).unwrap();
        assert_eq!(
            ids(first.current().unwrap()),
            vec![p.wtxid, c.wtxid, g.wtxid]
        );
        let full = full_after(&mut first, &[p.wtxid], &[]);
        assert_eq!(ids(&full), vec![c.wtxid, g.wtxid]);
        assert!(full.txs[0].depends_on.is_empty());
        assert_eq!(full.txs[1].depends_on, vec![c.wtxid]);
        assert_eq!(current_ids(&first), from_scratch(&first));
        let None = first.apply(SetEvent::Removed(p.wtxid)).unwrap() else {
            panic!("the mined parent is already gone");
        };

        // The removal of `p` comes first: `c` waits for the tip event.
        let mut second = live();
        second.load(vec![p.clone(), c.clone(), g.clone()]).unwrap();
        second.on_tip(tip(1), &[], &[], |_| {}).unwrap();
        let Some(TemplateUpdate::Changed(t)) = second.apply(SetEvent::Removed(p.wtxid)).unwrap()
        else {
            panic!("the removal changes the selection");
        };
        assert!(t.txs.is_empty(), "c cannot go in without its parent");
        let full = full_after(&mut second, &[p.wtxid], &[]);
        assert_eq!(ids(&full), vec![c.wtxid, g.wtxid]);
        assert_eq!(current_ids(&second), from_scratch(&second));

        // `c` conflicts with the block that mines `p`: `c` and `g` leave.
        let mut third = live();
        third.load(vec![p.clone(), c.clone(), g.clone()]).unwrap();
        third.on_tip(tip(1), &[], &[], |_| {}).unwrap();
        let full = full_after(&mut third, &[p.wtxid], &[c.wtxid]);
        assert!(full.txs.is_empty());
        assert_eq!(third.candidate_count(), 0);
    }

    #[test]
    fn conflicts_and_their_descendants_are_dropped_on_tip() {
        let mut live = live();
        let a = candidate(1, 10_000, 1, &[]);
        let b = candidate(2, 10_000, 1, &[a.wtxid]);
        let c = candidate(3, 10_000, 1, &[b.wtxid]);
        let d = candidate(4, 10_000, 1, &[]);
        live.load(vec![a.clone(), b.clone(), c.clone(), d.clone()])
            .unwrap();
        live.on_tip(tip(1), &[], &[], |_| {}).unwrap();
        assert_eq!(live.selection().count(), 4);
        let mut full = None;
        live.on_tip(tip(2), &[], &[a.wtxid], |u| {
            if let TemplateUpdate::Full(t) = u {
                full = Some(t);
            }
        })
        .unwrap();
        assert_eq!(ids(&full.unwrap()), vec![d.wtxid]);
        assert_eq!(live.candidate_count(), 1);
    }

    #[test]
    fn same_set_gives_the_same_bytes_whatever_the_history() {
        let cands: Vec<Candidate> = (1..=30)
            .map(|i| candidate(i, 5_000 * (i % 7 + 1), (i % 3 + 1) as usize, &[]))
            .collect();
        let mut forward = live();
        forward.load(cands.clone()).unwrap();
        forward.on_tip(tip(9), &[], &[], |_| {}).unwrap();

        let mut backward = live();
        backward.on_tip(tip(9), &[], &[], |_| {}).unwrap();
        for c in cands.iter().rev() {
            backward.apply(SetEvent::Added(c.clone())).unwrap();
        }
        // Add and remove a decoy, so that the history differs further.
        let decoy = candidate(99, 1_000_000, 1, &[]);
        backward.apply(SetEvent::Added(decoy.clone())).unwrap();
        backward.apply(SetEvent::Removed(decoy.wtxid)).unwrap();

        let f = forward.current().unwrap();
        let b = backward.current().unwrap();
        assert_eq!(ids(f), ids(b));
        assert_eq!(f.coinbase.bytes, b.coinbase.bytes);
        assert_eq!(f.merkle_root, b.merkle_root);
        assert_eq!(f.block_commitments, b.block_commitments);
    }

    #[test]
    fn random_event_histories_match_from_scratch() {
        use rand::rngs::StdRng;
        use rand::SeedableRng;
        for seed in 0..4u64 {
            // Room for about 12 small transactions and mostly conventional fees: the byte
            // limit and the sigop limit bound the selection.
            let tight = random_history(StdRng::seed_from_u64(0x5eed + seed), 12, 60, 4);
            assert!(
                tight.bytes_or_sigops,
                "the byte or sigop limit bound a step"
            );
            // Room for about 80 transactions and many fees below the conventional fee:
            // the unpaid action limit of 0 keeps these candidates out of the second pass.
            let low_fee = random_history(StdRng::seed_from_u64(0xfee + seed), 80, 30, 8);
            assert!(
                low_fee.unpaid_actions,
                "the unpaid action limit bound a step"
            );
        }
    }

    /// The limits that kept a selectable candidate out of the block in at least one step.
    #[derive(Default)]
    struct Bound {
        bytes_or_sigops: bool,
        unpaid_actions: bool,
    }

    /// 300 random events on a block with room for about `room` small transactions. A fee
    /// is below `max_fee_k` thousand zatoshis and a transaction has fewer than `max_out`
    /// outputs. After each event the incremental selection equals the from-scratch build
    /// and the two-pass reference.
    fn random_history(
        mut rng: rand::rngs::StdRng,
        room: usize,
        max_fee_k: u64,
        max_out: usize,
    ) -> Bound {
        use rand::Rng;
        let coinbase = coinbase_spec().build(7, 0).unwrap();
        let mut config = TemplateConfig::new(coinbase_spec());
        config.max_block_bytes = PowParams::MAINNET.header_len()
            + TX_COUNT_PREFIX_BYTES
            + coinbase.bytes.len()
            + coinbase.script_slack
            + room * 160;
        let mut live = LiveTemplate::new(config);
        live.on_tip(tip(7), &[], &[], |_| {}).unwrap();
        // About 6 candidates of the average sigop count reach the sigop limit.
        let max_sigops = live.tip_budget.sigops / 3;
        let mut bound = Bound::default();
        let mut seed = 0u64;
        let mut known: Vec<WtxId> = Vec::new();
        for _ in 0..300 {
            let op = if known.is_empty() {
                0
            } else {
                rng.gen_range(0..10)
            };
            match op {
                0..=5 => {
                    seed += 1;
                    let mut parents = Vec::new();
                    if !known.is_empty() && rng.gen_bool(0.4) {
                        parents.push(known[rng.gen_range(0..known.len())]);
                    }
                    if known.len() > 1 && rng.gen_bool(0.2) {
                        let p = known[rng.gen_range(0..known.len())];
                        if !parents.contains(&p) {
                            parents.push(p);
                        }
                    }
                    let fee = 1_000 * rng.gen_range(0..max_fee_k);
                    let mut c = candidate(seed, fee, rng.gen_range(1..max_out), &parents);
                    c.sigops = match rng.gen_bool(0.5) {
                        true => rng.gen_range(0..max_sigops),
                        false => rng.gen_range(0..6),
                    };
                    known.push(c.wtxid);
                    live.apply(SetEvent::Added(c)).unwrap();
                }
                6..=7 => {
                    // Remove a candidate and its descendants, as a mempool would.
                    let victim = known[rng.gen_range(0..known.len())];
                    let mut stack = vec![victim];
                    while let Some(id) = stack.pop() {
                        if let Some(entry) = live.entries.get(&id) {
                            stack.extend(entry.children.iter().copied());
                        }
                        known.retain(|k| *k != id);
                        live.apply(SetEvent::Removed(id)).unwrap();
                    }
                }
                _ => {
                    let id = known[rng.gen_range(0..known.len())];
                    let fee = 1_000 * rng.gen_range(0..max_fee_k);
                    let event = repriced(&live.entries[&id].candidate, fee);
                    live.apply(event).unwrap();
                }
            }
            assert_eq!(current_ids(&live), from_scratch(&live));
            // Parents precede their children in block order.
            let mut seen = AHashSet::new();
            for c in &live.current().unwrap().txs {
                assert!(c.depends_on.iter().all(|p| seen.contains(p)));
                seen.insert(c.wtxid);
            }
            // The template always holds the selected set.
            let selected: AHashSet<WtxId> = live.selection().map(|c| c.wtxid).collect();
            assert_eq!(seen, selected);
            let selected_fees: u64 = live.selection().map(|c| c.fee).sum();
            assert_eq!(live.current().unwrap().fees_total, selected_fees);
            // The selection is the result of the two passes of ZIP 317.
            assert_eq!(selected, two_pass_reference(&live));
            let unpaid: u32 = live.selection().map(|c| c.unpaid_actions).sum();
            assert_eq!(
                unpaid + live.budget.unpaid_actions,
                BLOCK_UNPAID_ACTION_LIMIT
            );
            let sigops: u32 = live.selection().map(|c| c.sigops).sum();
            assert_eq!(sigops + live.budget.sigops, live.tip_budget.sigops);
            // Which limit keeps a candidate out, when the block holds all its parents.
            for entry in live.entries.values() {
                let c = &entry.candidate;
                if selected.contains(&c.wtxid) || !live.parents_selected(c) {
                    continue;
                }
                let room = c.size_bytes() <= live.budget.bytes && c.sigops <= live.budget.sigops;
                bound.bytes_or_sigops |= !room;
                bound.unpaid_actions |= room && c.unpaid_actions > live.budget.unpaid_actions;
            }
        }
        assert!(live.selection().count() > 3, "the limit left room for work");
        bound
    }

    /// The tip of a template on top of the block `hash` at `height - 1`.
    fn tip_on(hash: u8, height: u32) -> Tip {
        Tip {
            parent_hash: BlockHash([hash; 32]),
            height,
            time: 1_700_000_000 + height,
            median_time_past: 1_700_000_000 + height - 1,
            bits: 0x1d00_ffff,
            history_root: [hash; 32],
            issued_supply: None,
        }
    }

    #[test]
    fn revert_restores_the_parent_template() {
        let mut live = live();
        let a = candidate(1, 30_000, 1, &[]);
        let b = candidate(2, 20_000, 1, &[a.wtxid]);
        let c = candidate(3, 10_000, 1, &[]);
        let d = candidate(4, 15_000, 1, &[]);
        live.load(vec![a.clone(), b.clone(), c.clone(), d.clone()])
            .unwrap();
        let parent = tip_on(0x10, 11);
        live.on_tip(parent, &[], &[], |_| {}).unwrap();
        let before = ids(live.current().unwrap());

        // Speculative block 0x20 mines `a` (its child `b` stays, without the dependency) and
        // conflicts with `c`. Then a second speculative block 0x30 on top of it mines `d`.
        let mut updates = Vec::new();
        live.on_speculative_tip(tip_on(0x20, 12), &[a.wtxid], &[c.wtxid], |u| {
            updates.push(u)
        })
        .unwrap();
        let [TemplateUpdate::Empty(_), TemplateUpdate::Full(full)] = updates.as_slice() else {
            panic!("a speculative tip emits Empty then Full, got {updates:?}");
        };
        assert_eq!(ids(full), by_txid(vec![b.wtxid, d.wtxid]));
        assert!(full.txs.iter().all(|t| t.depends_on.is_empty()));
        live.on_speculative_tip(tip_on(0x30, 13), &[d.wtxid], &[], |_| {})
            .unwrap();
        assert_eq!(live.candidate_count(), 1);

        // The source evicts `c` while block 0x20 waits: a revert must not bring it back.
        live.apply(SetEvent::Removed(c.wtxid)).unwrap();
        let e = candidate(5, 50_000, 1, &[]);
        live.apply(SetEvent::Added(e.clone())).unwrap();

        // Block 0x20 fails: both speculative tips go, the template is on 0x10 again.
        let mut updates = Vec::new();
        live.on_revert(parent, |u| updates.push(u)).unwrap();
        let [TemplateUpdate::Reverted { rejected, template }] = updates.as_slice() else {
            panic!("a revert emits Reverted, got {updates:?}");
        };
        assert_eq!(*rejected, BlockHash([0x20; 32]));
        assert_eq!(template.tip, parent);
        let mut held = ids(template);
        held.sort();
        let mut expected = vec![e.wtxid, a.wtxid, b.wtxid, d.wtxid];
        expected.sort();
        assert_eq!(held, expected);
        let a_at = template
            .txs
            .iter()
            .position(|t| t.wtxid == a.wtxid)
            .unwrap();
        let b_at = template
            .txs
            .iter()
            .position(|t| t.wtxid == b.wtxid)
            .unwrap();
        assert!(a_at < b_at, "a precedes its child b");
        assert_eq!(
            template.txs[b_at].depends_on,
            vec![a.wtxid],
            "b depends on a again"
        );
        assert_ne!(ids(template), before);
        assert_eq!(current_ids(&live), from_scratch(&live));
        let Err(ApplyError::NoSpeculativeChild(_)) = live.on_revert(parent, |_| {}) else {
            panic!("nothing left to revert");
        };
    }

    #[test]
    fn confirm_releases_the_kept_candidates_oldest_first() {
        let mut live = live();
        let a = candidate(1, 30_000, 1, &[]);
        let b = candidate(2, 20_000, 1, &[]);
        live.load(vec![a.clone(), b.clone()]).unwrap();
        live.on_tip(tip_on(0x10, 11), &[], &[], |_| {}).unwrap();
        live.on_speculative_tip(tip_on(0x20, 12), &[a.wtxid], &[], |_| {})
            .unwrap();
        live.on_speculative_tip(tip_on(0x30, 13), &[b.wtxid], &[], |_| {})
            .unwrap();
        let Err(ApplyError::NotOldestSpeculative(_)) = live.on_confirm(BlockHash([0x30; 32]))
        else {
            panic!("block 0x30 commits after block 0x20");
        };
        live.on_confirm(BlockHash([0x20; 32])).unwrap();
        // Block 0x30 fails: only `b` comes back; `a` was mined by the committed block.
        let mut reverted = None;
        live.on_revert(tip_on(0x20, 13), |u| reverted = Some(u))
            .unwrap();
        let Some(TemplateUpdate::Reverted { rejected, template }) = reverted else {
            panic!("a revert emits Reverted");
        };
        assert_eq!(rejected, BlockHash([0x30; 32]));
        assert_eq!(ids(&template), vec![b.wtxid]);
        // A committed tip ends every speculative tip event.
        live.on_speculative_tip(tip_on(0x40, 14), &[b.wtxid], &[], |_| {})
            .unwrap();
        live.on_tip(tip_on(0x41, 14), &[], &[], |_| {}).unwrap();
        let Err(ApplyError::NoSpeculativeChild(_)) = live.on_revert(tip_on(0x20, 13), |_| {})
        else {
            panic!("the committed tip released the speculative state");
        };
        assert_eq!(live.candidate_count(), 0);
    }

    /// A child whose txid sorts before its parent's still follows it, and a new candidate
    /// with the highest ratio takes its txid position: the transactions around it keep their
    /// relative order.
    #[test]
    fn block_order_is_parents_first_then_txid() {
        let mut live = live();
        live.on_tip(tip(3), &[], &[], |_| {}).unwrap();
        let mut seed = 0;
        let (parent, child) = loop {
            seed += 1;
            let parent = candidate(seed, 10_000, 1, &[]);
            let child = candidate(seed + 1000, 10_000, 1, &[parent.wtxid]);
            if child.wtxid.txid.as_ref() < parent.wtxid.txid.as_ref() {
                break (parent, child);
            }
        };
        let others: Vec<Candidate> = (1..=6)
            .map(|i| candidate(500 + i, 10_000, 1, &[]))
            .collect();
        live.apply(SetEvent::Added(parent.clone())).unwrap();
        live.apply(SetEvent::Added(child.clone())).unwrap();
        for c in &others {
            live.apply(SetEvent::Added(c.clone())).unwrap();
        }
        let before = current_ids(&live);
        let mut roots: Vec<WtxId> = others.iter().map(|c| c.wtxid).collect();
        roots.push(parent.wtxid);
        let mut expected = by_txid(roots);
        expected.push(child.wtxid);
        assert_eq!(before, expected);

        let rich = candidate(900, 1_000_000, 1, &[]);
        live.apply(SetEvent::Added(rich.clone())).unwrap();
        assert_eq!(live.selection().next().unwrap().wtxid, rich.wtxid);
        let after: Vec<WtxId> = current_ids(&live)
            .into_iter()
            .filter(|id| *id != rich.wtxid)
            .collect();
        assert_eq!(after, before);
    }

    #[test]
    fn block_commitments_match_zip244_layout() {
        // The personalization and the element order distinguish this from a plain hash.
        let h = block_commitments(&[1; 32], &[2; 32]);
        let expected = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(b"ZcashBlockCommit")
            .hash(&[[1u8; 32], [2; 32], [0; 32]].concat());
        assert_eq!(&h[..], expected.as_bytes());
    }
}
