//! The driver of a full node: the blocks of the download scheduler and of the relay, the
//! fork choice, the reorg and the speculative tip.
//!
//! Fork choice. The header chain has the best chain: the most cumulative work among the
//! chains without an invalid block, and the first-seen chain on equal work. The scheduler
//! delivers the blocks of that chain after the newest committed block that is on it.
//!
//! Reorg. When the first delivered block does not extend the committed tip, its branch
//! left the committed chain at the parent of that block. The driver waits until the
//! delivered blocks of the branch have more work than the committed tip, or until the
//! scheduler delivers no more blocks before a commit. Then it disconnects the committed
//! blocks down to the fork point and validates the branch. A block of the branch that is
//! not valid becomes invalid in the header chain, the header chain selects the next best
//! chain, and the scheduler delivers its blocks: the blocks of the first branch come
//! back from the block store. The transactions of the disconnected blocks go back to the
//! mempool with the transactions of the store, through the admission rules, on the new tip.
//!
//! Speculative tip. The driver builds the layers of the delivered blocks in order on
//! `Chain::view_speculative` (`build_layer`, `Chain::push_speculative`), then verifies the
//! scripts and the proofs of all of them in parallel, then confirms them in order. When
//! the one delivered block is the best header tip, the template moves to the block at the
//! layer build and goes back when the verification fails.

use std::sync::Arc;
use std::time::Instant;

use hayai_net::Source;
use hayai_state::SpecId;
use hayai_validate::{build_layer, verify, Timings, ValidateConfig, Verification};
use hayai_wire::header::BlockHash;
use hayai_wire::{RawBlock, WtxId};
use rayon::prelude::*;

use super::fault::{check_body, fault_of, local_fault, Fault};
use super::{fatal, publish, CommitCtx, Driver, NodeError, TipChange};
use crate::sync::{NetEvent, Refusal};

/// Blocks between the committed tip and the best header tip above which the node builds
/// no template. A block on a tip that is this far below the chain of the network is not
/// a block of that chain, so its template has no use, and its build takes time from the
/// synchronization. The value is the distance at which Zakura refuses `getblocktemplate`
/// (`MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP`). At the tip the header chain is at
/// most some blocks before the bodies. A header chain whose blocks no peer sends leaves
/// the fork choice ([`crate::sync`], withheld bodies), and the template then follows the
/// committed tip again.
const TEMPLATE_MAX_BLOCKS_BEHIND: u32 = 100;

/// A delivered block with its body.
struct Job {
    height: u32,
    hash: BlockHash,
    raw: Arc<RawBlock>,
    /// The header chain reached a checkpoint at or above the block.
    checkpointed: bool,
    origin: &'static str,
}

/// A block whose layer is on the speculative tip.
struct Built<'k> {
    job: usize,
    id: SpecId,
    ctx: CommitCtx,
    verification: Verification<'k>,
    timings: Timings,
}

/// The refusal that the block synchronization gets for a fault that does not stop the
/// node.
fn refusal_of(fault: Fault) -> Option<Refusal> {
    match fault {
        Fault::WrongBody => Some(Refusal::WrongBody),
        Fault::Invalid => Some(Refusal::Invalid),
        Fault::Local => None,
    }
}

impl Driver {
    fn sync_mut(&mut self) -> &mut crate::sync::Sync {
        let Some(sync) = &mut self.sync else {
            unreachable!("a full node has the block synchronization");
        };
        sync
    }

    /// A message of the block synchronization.
    pub(super) fn on_net(&mut self, event: NetEvent, at: Instant) -> Result<(), NodeError> {
        let Some(sync) = &mut self.sync else {
            return Err(NodeError(
                "a synchronization message reached a node without synchronization".into(),
            ));
        };
        sync.on_net(event, at)?;
        self.process_delivered()
    }

    /// A complete block of the relay: a compact block, or a block of this node.
    pub(super) fn on_relayed(
        &mut self,
        block: Arc<RawBlock>,
        source: Source,
    ) -> Result<(), NodeError> {
        let hash = block.hash();
        if let Err(reason) = self.sync_mut().relayed(block, source)? {
            tracing::info!(%hash, %reason, "block of the relay refused by the header chain");
            self.remember_rejection(hash, reason);
        }
        self.process_delivered()
    }

    /// Validates and commits one batch of delivered blocks (at most the validation
    /// lookahead of the scheduler), then gives the transactions of the disconnected blocks
    /// back to the mempool and rebuilds the template. With more delivered blocks the
    /// function sets `more_delivered`, and the event loop calls it again after the
    /// messages that wait: a tick and a message never wait for more than one batch.
    ///
    /// The node builds no template while its committed tip is more than
    /// [`TEMPLATE_MAX_BLOCKS_BEHIND`] blocks below the best header tip.
    pub(super) fn process_delivered(&mut self) -> Result<(), NodeError> {
        let mut changes = std::mem::take(&mut self.deferred_changes);
        // Whether the template is not on the committed tip.
        let mut stale = std::mem::take(&mut self.template_deferred);
        let before = self.chain.tip().hash;
        self.more_delivered = match self.next_batch()? {
            Some(jobs) => {
                let (_, template_moved) = self.commit_batch(&jobs, &mut changes)?;
                if self.chain.tip().hash != before {
                    stale = !template_moved;
                }
                // A refused block ends its batch, and the scheduler delivers again.
                true
            }
            None => false,
        };
        if !self.disconnected.is_empty() && self.reorg_done() {
            let returned = std::mem::take(&mut self.disconnected);
            // The store is empty for a moment, then holds what is valid on the new tip.
            changes.dropped.extend(self.mempool.readmit(returned));
            stale = true;
        }
        if !stale {
            return Ok(());
        }
        if self.far_behind() {
            self.deferred_changes = changes;
            self.template_deferred = true;
            return Ok(());
        }
        self.on_tip(&changes)
    }

    /// Whether the committed tip is more than [`TEMPLATE_MAX_BLOCKS_BEHIND`] blocks below
    /// the best header tip.
    fn far_behind(&self) -> bool {
        let Some(sync) = &self.sync else {
            return false;
        };
        let behind = sync.header_height().saturating_sub(self.chain.tip().height);
        behind > TEMPLATE_MAX_BLOCKS_BEHIND
    }

    /// Whether no delivered block waits for a branch that leaves the committed tip.
    fn reorg_done(&self) -> bool {
        let Some(sync) = &self.sync else {
            return true;
        };
        let None = sync.delivered().next() else {
            return false;
        };
        true
    }

    /// The delivered blocks that the driver can validate now, with their bodies. `None`:
    /// no block is delivered, or the first one must wait.
    fn next_batch(&mut self) -> Result<Option<Vec<Job>>, NodeError> {
        let Some(sync) = &self.sync else {
            return Ok(None);
        };
        let tip = self.chain.tip();
        let mandatory = self.params.kind.mandatory_checkpoint_height();
        let mut jobs = Vec::new();
        for delivered in sync.delivered() {
            let (height, hash) = (delivered.block.height, delivered.block.hash);
            let checkpointed = sync.checkpointed(height);
            // A block at or below the mandatory checkpoint has only the checkpoint path:
            // it waits until the header chain reaches a checkpoint above it.
            if !checkpointed && height <= mandatory {
                break;
            }
            let (raw, origin) = match sync.body(&hash) {
                Some(body) => (
                    body.block.clone(),
                    match body.relayed {
                        true => super::origin(&body.supplier),
                        false => "download",
                    },
                ),
                // A block that the node committed before and that is on the best chain
                // again.
                None => (Arc::new(self.stored_block(height, &hash)?), "stored"),
            };
            jobs.push(Job {
                height,
                hash,
                raw,
                checkpointed,
                origin,
            });
        }
        let Some(first) = jobs.first() else {
            return Ok(None);
        };
        let fork = first.raw.header.prev_hash;
        if fork != tip.hash {
            if !sync.branch_ready(&tip.hash) {
                return Ok(None);
            }
            tracing::info!(%fork, tip = %tip.hash, branch = %first.hash, "reorg: the best chain leaves the committed tip");
            self.disconnect_to(&fork)?;
        }
        Ok(Some(jobs))
    }

    /// Commits `jobs` in order and stops at the first refused block. Returns the number of
    /// committed blocks, and whether the template already moved to the new tip.
    fn commit_batch(
        &mut self,
        jobs: &[Job],
        changes: &mut TipChange,
    ) -> Result<(usize, bool), NodeError> {
        let mut done = 0;
        while done < jobs.len() {
            let job = &jobs[done];
            // A checkpointed block and a block with a prebuilt body commit in one step.
            if job.checkpointed || self.prebuilt.class_of(&job.raw) != "full" {
                // The checkpoint path gets the hash that the header chain has at the height.
                let checkpoint = job.checkpointed.then_some(job.hash);
                match self.commit_on_tip(&job.raw, job.origin, checkpoint)? {
                    Ok(change) => {
                        changes.mined.extend(change.mined);
                        changes.dropped.extend(change.dropped);
                        self.sync_mut().committed(&job.hash)?;
                        done += 1;
                        continue;
                    }
                    Err(error) => {
                        let refusal = match (job.checkpointed, fault_of(&error)) {
                            (_, Fault::WrongBody) => Refusal::WrongBody,
                            (false, Fault::Invalid) => Refusal::Invalid,
                            // The header chain fixed the hash of this block with a
                            // checkpoint. A peer cannot make it invalid.
                            (true, Fault::Invalid) => {
                                return Err(NodeError(format!(
                                    "checkpointed block {} at height {}: {error}",
                                    job.hash, job.height
                                )))
                            }
                            (_, Fault::Local) => {
                                return Err(local_fault(self.mode, job.height, &job.hash, &error))
                            }
                        };
                        self.sync_mut().refused(&job.hash, refusal)?;
                        return Ok((done, false));
                    }
                }
            }
            let run = jobs[done..]
                .iter()
                .take_while(|job| !job.checkpointed)
                .count();
            let (committed, template_moved) =
                self.commit_speculative(&jobs[done..done + run], changes)?;
            done += committed;
            if committed < run {
                return Ok((done, false));
            }
            if template_moved {
                return Ok((done, true));
            }
        }
        Ok((done, false))
    }

    /// The speculative path for `jobs`, which extend the committed tip in order: the layer
    /// of each block on the speculative tip, the verification of all blocks in parallel,
    /// then the commits in order. Stops at the first refused block.
    fn commit_speculative(
        &mut self,
        jobs: &[Job],
        changes: &mut TipChange,
    ) -> Result<(usize, bool), NodeError> {
        // The verification runs on the rayon pool, which never builds a key: the driver
        // waits here for the key of each rule set of the batch.
        for job in jobs {
            super::await_keys(self.params, self.mode, &self.keys, job.height)
                .map_err(super::no_rules)?;
        }
        let cfgs: Vec<ValidateConfig> = jobs
            .iter()
            .map(|job| self.validate_config(job.height))
            .collect::<Result<_, _>>()?;
        let mut built: Vec<Built<'_>> = Vec::with_capacity(jobs.len());
        // The block that failed before its verification, with the reason.
        let mut failed: Option<(CommitCtx, Refusal, String)> = None;
        // The error of a block that this node cannot validate. The node stops after the
        // commits of the blocks before it.
        let mut local: Option<NodeError> = None;
        for (k, job) in jobs.iter().enumerate() {
            let view = self.chain.view_speculative();
            let ctx = self.begin_commit(&job.raw, job.height, job.origin, "full", &view);
            let checked = check_body(&job.raw, &view, &cfgs[k], true);
            if let Ok(()) = checked {
                self.sync_mut().body_checked(&job.hash)?;
            }
            let layer = checked
                .and_then(|()| build_layer((*job.raw).clone(), &self.store, &view, &cfgs[k]));
            match layer {
                Ok((layer, verification, timings)) => {
                    let id = self
                        .chain
                        .push_speculative(layer)
                        .map_err(|e| fatal("speculative push", e))?;
                    built.push(Built {
                        job: k,
                        id,
                        ctx,
                        verification,
                        timings,
                    });
                }
                Err(error) => {
                    let fault = fault_of(&error);
                    match refusal_of(fault) {
                        Some(refusal) => failed = Some((ctx, refusal, error.to_string())),
                        None => {
                            self.reject_commit(&ctx, &error.to_string(), "full", fault);
                            local = Some(local_fault(self.mode, job.height, &job.hash, &error));
                        }
                    }
                    break;
                }
            }
        }

        // At the tip the template moves to the block before its verification.
        let at_tip = match (jobs, &failed) {
            ([job], None) => self.sync_mut().is_best_tip(&job.hash),
            _ => false,
        };
        let previous_tip = self.template_tip;
        if at_tip {
            self.speculative_template()?;
        }

        let mut ids = Vec::with_capacity(built.len());
        let mut pending = Vec::with_capacity(built.len());
        let mut verifications = Vec::with_capacity(built.len());
        for b in built {
            ids.push(b.id);
            pending.push((b.job, b.ctx, b.timings));
            verifications.push(b.verification);
        }
        let verdicts: Vec<_> = verifications.into_par_iter().map(verify).collect();

        let mut committed = 0;
        let mut rejected: Option<BlockHash> = None;
        for ((id, (k, ctx, mut timings)), verdict) in ids.into_iter().zip(pending).zip(verdicts) {
            let job = &jobs[k];
            if let Some(ancestor) = rejected {
                // The layer left the chain with its ancestor.
                self.reject_commit(
                    &ctx,
                    &format!("the ancestor {ancestor} is invalid"),
                    "full",
                    Fault::Invalid,
                );
                continue;
            }
            match verdict {
                Ok(verified) => {
                    timings.scripts = verified.scripts;
                    timings.shielded = verified.shielded;
                    timings.total += verified.total;
                    let layers = self
                        .chain
                        .confirm(id)
                        .map_err(|e| fatal("speculative confirm", e))?;
                    let [layer] = &layers[..] else {
                        return Err(NodeError(format!(
                            "the confirmation of {} committed {} layers",
                            job.hash,
                            layers.len()
                        )));
                    };
                    self.finish_commit(&ctx, &job.raw, layer, &timings, "full", changes)?;
                    if at_tip {
                        self.live
                            .on_confirm(job.hash)
                            .map_err(|e| fatal("template", e))?;
                    }
                    self.sync_mut().committed(&job.hash)?;
                    committed += 1;
                }
                Err(error) => {
                    self.chain
                        .reject(id)
                        .map_err(|e| fatal("speculative reject", e))?;
                    let fault = fault_of(&error);
                    self.reject_commit(&ctx, &error.to_string(), "full", fault);
                    if let (true, Some(tip)) = (at_tip, previous_tip) {
                        self.revert_template(tip)?;
                    }
                    let Some(refusal) = refusal_of(fault) else {
                        return Err(local_fault(self.mode, job.height, &job.hash, &error));
                    };
                    self.sync_mut().refused(&job.hash, refusal)?;
                    rejected = Some(job.hash);
                }
            }
        }
        if let (None, Some((ctx, refusal, reason))) = (rejected, failed) {
            let hash = ctx.hash;
            let fault = match refusal {
                Refusal::WrongBody => Fault::WrongBody,
                Refusal::Invalid => Fault::Invalid,
            };
            self.reject_commit(&ctx, &reason, "full", fault);
            self.sync_mut().refused(&hash, refusal)?;
        }
        if let Some(error) = local {
            return Err(error);
        }
        Ok((committed, at_tip && committed == 1))
    }

    /// The candidates that conflict with the newest speculative block, and the block's own
    /// transactions: what the template drops for it.
    fn speculative_template(&mut self) -> Result<(), NodeError> {
        let Some((_, layer)) = self.chain.speculative().last() else {
            unreachable!("the caller pushed a speculative layer");
        };
        let mined: Vec<WtxId> = layer.wtxids.clone();
        let mut conflicting: Vec<WtxId> = layer
            .spent
            .iter()
            .filter_map(|outpoint| self.store.spender(outpoint))
            .chain(
                super::layer_nullifiers(layer)
                    .iter()
                    .filter_map(|(pool, nullifier)| self.store.revealer(*pool, nullifier)),
            )
            // A transaction that the block after the speculative block cannot contain
            // (ZIP 203) leaves the template with the conflicts.
            .chain(self.store.expired(layer.height + 1))
            .filter(|id| !mined.contains(id))
            .collect();
        conflicting.sort_unstable_by_key(|id| (*id.txid.as_ref(), id.auth_digest));
        conflicting.dedup();
        let tip = self.template_tip(&self.chain.view_speculative())?;
        let started = Instant::now();
        let (feed, tracer, metrics) = (&self.feed, &self.tracer, &self.metrics);
        let mut updates = Vec::new();
        self.live
            .on_speculative_tip(tip, &mined, &conflicting, |update| {
                publish(feed, tracer, metrics, &update, Some(started));
                updates.push(update);
            })
            .map_err(|e| fatal("template", e))?;
        self.template_tip = Some(tip);
        for update in &updates {
            self.after_update(update);
        }
        Ok(())
    }

    /// The speculative block failed its verification: the template goes back to `tip`.
    fn revert_template(&mut self, tip: hayai_template::Tip) -> Result<(), NodeError> {
        let (feed, tracer, metrics) = (&self.feed, &self.tracer, &self.metrics);
        let mut updates = Vec::new();
        self.live
            .on_revert(tip, |update| {
                publish(feed, tracer, metrics, &update, None);
                updates.push(update);
            })
            .map_err(|e| fatal("template", e))?;
        self.template_tip = Some(tip);
        for update in &updates {
            self.after_update(update);
        }
        Ok(())
    }
}

/// Opens the header chain of the node and adds the headers of the committed blocks that
/// the header log does not hold: the log is not made durable for each header, and a data
/// directory of an earlier version has no log. The committed tip and its ancestors are
/// then valid bodies in the header chain.
pub(super) fn open_header_chain(
    path: &std::path::Path,
    params: crate::params::NetParams,
    tip: hayai_state::Tip,
    blocks: &hayai_blockstore::BlockStore,
) -> Result<hayai_sync::headers::HeaderChain, NodeError> {
    use hayai_sync::headers::{ChainConfig, HeaderChain};
    use hayai_wire::header::BlockHeader;

    let (mut chain, report) = HeaderChain::open(ChainConfig::new(params.kind), path)
        .map_err(|e| fatal("header log", e))?;
    tracing::info!(?report, best = ?chain.best_tip(), "header chain opened");
    let mut missing: Vec<BlockHeader> = Vec::new();
    let (mut height, mut hash) = (tip.height, tip.hash);
    while !matches!(chain.entry(&hash), Some(_entry)) {
        let bytes = blocks
            .get_by_hash(&hash)
            .map_err(|e| fatal("block store", e))?
            .ok_or_else(|| {
                NodeError(format!(
                    "the header chain and the block store do not hold the committed block \
                     {hash} of height {height}"
                ))
            })?;
        let header = BlockHeader::parse(&bytes).map_err(|e| fatal("stored header", e))?;
        hash = header.prev_hash;
        height = height.checked_sub(1).ok_or_else(|| {
            NodeError("the committed chain does not start at the genesis block".into())
        })?;
        missing.push(header);
    }
    missing.reverse();
    // The node committed these blocks: the rule of the local clock does not apply to them
    // again, so the clock of the rule is the largest time.
    chain
        .accept_headers(&missing, &crate::sync::ConsensusHeaderRules, u32::MAX)
        .map_err(|e| fatal("header of a committed block", e))?;
    chain
        .mark_body_valid(&tip.hash)
        .map_err(|e| fatal("header chain", e))?;
    Ok(chain)
}
