//! Bulk block validation: wire bytes in, state layer out.
//!
//! Contract: `docs/architecture.md`, sections hayai-validate and Data flow. A validation has
//! two halves:
//!
//! - [`build_layer`]: the merkle root and the ZIP 244 authorizing-data root (hashed in
//!   parallel); the split of the transactions into known (prepared store hit by [`WtxId`],
//!   same epoch) and unknown; the parent check and one coin round for every input of the
//!   block that is not created in the block (`hayai_state::resolve_inputs`); the drafts of
//!   the unknown transactions in parallel; then the contextual rules with batched nullifier
//!   and anchor rounds, the tree appends, the header commitment to the parent's ZIP 221
//!   history tree and the history append (hayai-state). It returns the layer and the
//!   [`Verification`] of the block.
//! - [`verify`]: every script of the unknown transactions as one flat parallel array, and
//!   the block-scoped shielded batch (one task per bundle group, with bisection), as two
//!   concurrent tasks.
//!
//! A node publishes the layer of [`build_layer`] as a speculative tip
//! (`hayai_state::Chain::push_speculative`) and runs [`verify`] concurrently.
//! [`validate_block`] is both halves for a caller that wants one call: it runs the
//! contextual rules concurrently with the scripts and the shielded batch, and the block
//! fails with the first error in stage order (scripts, shielded, context). [`build_layer`]
//! alone reports a contextual error before any script runs.
//!
//! A node can also do the contextual work of an expected block before the block exists:
//! [`prebuild`] runs it on a body of prepared transactions (the own template, or a
//! candidate of a peer's lane), and [`commit_prebuilt`] commits a block with that body
//! after the header and coinbase rules only (`hayai_state::PrebuiltBody`).
//!
//! A block at or below the last checkpoint has its own path, [`apply_checkpointed`]: the
//! hash of the block against the checkpointed chain, the two header roots, then the state
//! update (`hayai_state::checkpoint_layer`). It runs no script, no proof, no signature and
//! no contextual header rule. The other functions refuse a block at or below the mandatory
//! checkpoint of the network ([`BlockError::BelowMandatoryCheckpoint`]): hayai has no full
//! validation for these heights, as Zebra and Zakura.
//!
//! Every full validation starts with the contextual header rules of
//! `hayai_consensus::header::check_contextual` on the context of the view
//! ([`check_block_header`], selected by [`HeaderPolicy`]): the version, the target limit,
//! the time rules and the expected `nBits`. The hash filter and Equihash
//! (`hayai_consensus::header::check_proof_of_work`) are rules that a node runs before it
//! relays a header, and a replay runs them on each stored block. They are not repeated
//! here. The rule against the clock of the node is not a consensus rule and never runs
//! here.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use hayai_coins::{Coin, OutPoint};
use hayai_consensus::header::{check_contextual, HeaderRuleError, HeaderVerdict, Unchecked};
use hayai_consensus::{Checkpoints, Network, ParentChain, RuleSet};
use hayai_prepared::{
    check_scripts, draft, Draft, PrepareError, PreparedTx, RuleEpoch, ScopedBatch, VerifyingKeys,
};
use hayai_state::{
    block_outputs, check_parent, checkpoint_layer, contextual_check_with_outputs, prebuild_body,
    resolve_inputs, ChainView, CheckConfig, Checked, ContextError, Layer, Map, PrebuiltBody,
    PreparedBlock, SwapError,
};
use hayai_wire::header::BlockHash;
use hayai_wire::{auth_data_root, duplicate_txid, merkle_root, ParseError, RawBlock, WtxId};
use rayon::prelude::*;

pub use hayai_prepared::PreparedStore;

/// Everything a validation needs besides the block, the store and the view.
#[derive(Clone)]
pub struct ValidateConfig {
    /// The network: the header rules and the coinbase terms of a height come from it.
    pub network: Network,
    /// The rule set of the block's height (`hayai_consensus::rules_at`).
    pub rules: RuleSet,
    pub keys: Arc<VerifyingKeys>,
    /// How the validation applies the header rules.
    pub header: HeaderPolicy,
}

impl ValidateConfig {
    /// The epoch under which the transactions of the block are prepared.
    pub fn epoch(&self) -> RuleEpoch {
        RuleEpoch::of(&self.rules)
    }

    fn check_config(&self) -> CheckConfig<'_> {
        CheckConfig {
            network: self.network,
            rules: &self.rules,
        }
    }
}

/// How a validation applies the contextual header rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderPolicy {
    /// The rules of the network run on the context of the view. A context that is too
    /// short for a rule is [`BlockError::HeaderContext`]. A node that validates from the
    /// genesis block uses this value: its context is never too short.
    Enforce,
    /// The rules of the network run on the context of the view. A rule whose context is
    /// too short does not run and the block passes that rule: the caller trusts the source
    /// of the block for it. A node that starts from a trusted state above the genesis
    /// block uses this value, and counts the trusted headers at its header check.
    TrustShortContext,
    /// No header rule runs. Only for generated blocks, whose headers have no proof of work
    /// and no chain of real headers before them (benchmarks and their tests). A node never
    /// uses this value.
    GeneratedBlocks,
}

/// The contextual header rules of `raw` on `view` for `network`, as `policy` selects them.
/// The parent rule runs first, because the context of another tip is not the context of
/// the block.
pub fn check_block_header(
    raw: &RawBlock,
    view: &ChainView,
    network: Network,
    policy: HeaderPolicy,
) -> Result<(), BlockError> {
    let trust_short_context = match policy {
        HeaderPolicy::Enforce => false,
        HeaderPolicy::TrustShortContext => true,
        HeaderPolicy::GeneratedBlocks => return Ok(()),
    };
    let height = check_parent(view, raw)?;
    let times = view.recent_times();
    let bits: Vec<u32> = view
        .difficulty_context()
        .into_iter()
        .map(|(_, bits)| bits)
        .collect();
    let chain = ParentChain {
        height,
        times: &times,
        bits: &bits,
    };
    match check_contextual(network, &raw.header, &chain)? {
        HeaderVerdict::Checked => Ok(()),
        HeaderVerdict::ContextTooShort(_) if trust_short_context => Ok(()),
        HeaderVerdict::ContextTooShort(unchecked) => Err(BlockError::HeaderContext(unchecked)),
    }
}

/// Wall-clock time of each stage.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timings {
    pub parse: Duration,
    pub roots: Duration,
    pub lookup: Duration,
    pub prepare_unknown: Duration,
    pub scripts: Duration,
    pub shielded: Duration,
    pub context: Duration,
    pub trees: Duration,
    /// The header commitment rule and the history tree append.
    pub history: Duration,
    pub total: Duration,
    /// Transactions served by the prepared store.
    pub known: usize,
    /// Transactions prepared during validation.
    pub unknown: usize,
}

/// Wall-clock time of the two stages of [`verify`].
#[derive(Clone, Copy, Debug, Default)]
pub struct VerifyTimings {
    pub scripts: Duration,
    pub shielded: Duration,
    pub total: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum BlockError {
    #[error("parse: {0}")]
    Parse(#[from] ParseError),
    #[error("header: {0}")]
    Header(#[from] HeaderRuleError),
    /// The view holds fewer blocks than a header rule reads, and the validation does not
    /// trust the source of the block ([`HeaderPolicy::Enforce`]).
    #[error("header: the context is too short for a rule: {}", .0.context)]
    HeaderContext(Unchecked),
    #[error("merkle root does not match the transactions")]
    MerkleRoot,
    #[error("transaction {tx}: {error}")]
    Prepare { tx: usize, error: PrepareError },
    #[error("shielded bundles of {0:?} are invalid")]
    Shielded(Vec<WtxId>),
    #[error("{0}")]
    Context(#[from] ContextError),
    /// Full validation of a block that only the checkpointed chain can give.
    #[error(
        "height {height} is at or below the mandatory checkpoint {mandatory}: the block has \
         no full validation, only the checkpoint path"
    )]
    BelowMandatoryCheckpoint { height: u32, mandatory: u32 },
    /// The checkpoint path for a block above the last checkpoint. `last` is `None` when
    /// the list is empty.
    #[error("height {height} is above the last checkpoint {last:?}")]
    AboveLastCheckpoint { height: u32, last: Option<u32> },
    #[error(
        "block {found} at height {height} is not the block {expected} of the checkpointed chain"
    )]
    NotOnCheckpointedChain {
        height: u32,
        expected: BlockHash,
        found: BlockHash,
    },
}

pub use hayai_wire::block_commitments;

/// The verification half of a block: the scripts and the shielded bundles of the
/// transactions that the prepared store did not hold. It borrows the verifying keys of the
/// [`ValidateConfig`] that built it.
pub struct Verification<'k> {
    drafts: Vec<Draft>,
    /// Block position of each draft.
    unknown: Vec<usize>,
    batch: ScopedBatch<'k>,
}

impl Verification<'_> {
    /// Transactions whose scripts and bundles this verification checks.
    pub fn transactions(&self) -> usize {
        self.drafts.len()
    }
}

/// The first half of a validation, up to the drafts: what both [`build_layer`] and
/// [`validate_block`] need before the contextual rules.
struct Drafted<'k> {
    block: PreparedBlock,
    created: Map<OutPoint, Coin>,
    inputs: Vec<Vec<Coin>>,
    verification: Verification<'k>,
    timings: Timings,
    started: Instant,
}

/// Parses then validates; `Timings::parse` covers the parse.
pub fn validate_bytes(
    bytes: Bytes,
    store: &PreparedStore,
    view: &ChainView,
    cfg: &ValidateConfig,
) -> Result<(Layer, Timings), BlockError> {
    let started = Instant::now();
    let raw = RawBlock::parse(bytes, cfg.rules.branch_id)?;
    let parse = started.elapsed();
    let (layer, mut timings) = validate_block(raw, store, view, cfg)?;
    timings.parse = parse;
    timings.total += parse;
    Ok((layer, timings))
}

/// Validates a parsed block on top of `view`, reusing the store's prepared transactions,
/// and returns the layer to push: [`build_layer`] and [`verify`] in one call, with the
/// contextual rules concurrent with the scripts and the shielded batch.
pub fn validate_block(
    raw: RawBlock,
    store: &PreparedStore,
    view: &ChainView,
    cfg: &ValidateConfig,
) -> Result<(Layer, Timings), BlockError> {
    let Drafted {
        block,
        created,
        inputs,
        verification,
        mut timings,
        started,
    } = draft_block(raw, store, view, cfg)?;
    let (verified, checked) = rayon::join(
        || verify(verification),
        || check(view, &block, created, inputs, cfg),
    );
    let verified = verified?;
    let checked = checked?;
    timings.scripts = verified.scripts;
    timings.shielded = verified.shielded;
    record_context(&mut timings, &checked);
    timings.total = started.elapsed();
    Ok((checked.layer, timings))
}

/// The merkle root rule of `raw`, on the full path and on the checkpoint path: the merkle
/// root of the header matches the txids, and no txid is in the block twice. Returns the
/// ZIP 244 root of the authorizing data.
///
/// A body with a txid twice can have the merkle root of a valid block
/// (`hayai_wire::duplicate_txid`, CVE-2012-2459). The error is
/// [`ContextError::DuplicateTxid`]: the body is at fault, and the header can be the header
/// of a valid block. Zakura has the same two checks in this order
/// (`zakura-consensus/src/block/check.rs:523-559`).
fn check_roots(raw: &RawBlock) -> Result<[u8; 32], BlockError> {
    let txids = raw.txids();
    let digests = raw.auth_digests();
    let (merkle, auth) = rayon::join(|| merkle_root(&txids), || auth_data_root(&digests));
    if merkle != raw.header.merkle_root {
        return Err(BlockError::MerkleRoot);
    }
    if let Some(txid) = duplicate_txid(&txids) {
        return Err(ContextError::DuplicateTxid(txid).into());
    }
    Ok(auth)
}

/// The rule of the mandatory checkpoint: a block at or below it has no full validation
/// (Zakura `assert_block_can_be_validated`, `zakura-state/src/service.rs:1545`). Before
/// Canopy, hayai has no verifier for some proofs and no complete rule set.
fn check_above_mandatory_checkpoint(network: Network, height: u32) -> Result<(), BlockError> {
    let mandatory = network.mandatory_checkpoint_height();
    if height <= mandatory {
        return Err(BlockError::BelowMandatoryCheckpoint { height, mandatory });
    }
    Ok(())
}

/// Applies the checkpointed block `raw` on top of `view` and returns the layer to push.
///
/// `expected` is the hash that the header chain has at the height of the block, on the
/// branch that reached a checkpoint at or above that height (the download scheduler
/// reports such a block with `checkpointed = true`). The header chain checked the headers
/// from this block to that checkpoint: the proof of work, the header rules and the
/// checkpoint hash. The function does not read the header chain: the caller must take
/// `expected` from it. The hash of `raw` itself as `expected` removes the comparison, and
/// a height between two checkpoints then has no bond to the checkpointed chain.
///
/// The function checks:
///
/// - the parent of the block is the tip of `view`, and the height is at or below the last
///   checkpoint of `checkpoints`;
/// - the hash of the block is `expected`, and it is the checkpoint hash when the height
///   has a checkpoint;
/// - the merkle root of the header matches the transactions, and no txid is in the block
///   twice ([`ContextError::DuplicateTxid`]);
/// - the state rules of `hayai_state::checkpoint_layer`, with the header commitment to the
///   history tree of the parent and to the authorizing data.
///
/// The function does not check scripts, proofs, signatures, Equihash or the contextual
/// header rules. The layer is the layer that [`validate_block`] returns for the same
/// valid block. `cfg` gives the network and the rule set of the height. The function does
/// not read the verifying keys and the header policy of `cfg`.
pub fn apply_checkpointed(
    raw: &RawBlock,
    expected: BlockHash,
    view: &ChainView,
    cfg: &ValidateConfig,
    checkpoints: &Checkpoints,
) -> Result<(Layer, Timings), BlockError> {
    let started = Instant::now();
    let mut timings = Timings::default();
    let height = check_parent(view, raw)?;
    let last = checkpoints.last_height();
    if !matches!(last, Some(last) if height <= last) {
        return Err(BlockError::AboveLastCheckpoint { height, last });
    }
    let found = raw.hash();
    for expected in [Some(expected), checkpoints.hash_at(height)]
        .into_iter()
        .flatten()
    {
        if found != expected {
            return Err(BlockError::NotOnCheckpointedChain {
                height,
                expected,
                found,
            });
        }
    }
    let auth = check_roots(raw)?;
    timings.roots = started.elapsed();
    let checked = checkpoint_layer(view, raw, &auth, &cfg.check_config())?;
    record_context(&mut timings, &checked);
    timings.total = started.elapsed();
    Ok((checked.layer, timings))
}

/// The first half of a validation: everything except the scripts and the proofs of the
/// unknown transactions. Returns the layer to publish as a speculative tip, the
/// [`Verification`] to run with [`verify`] before the layer commits, and the timings of the
/// stages that ran (`scripts` and `shielded` stay zero).
pub fn build_layer<'k>(
    raw: RawBlock,
    store: &PreparedStore,
    view: &ChainView,
    cfg: &'k ValidateConfig,
) -> Result<(Layer, Verification<'k>, Timings), BlockError> {
    let Drafted {
        block,
        created,
        inputs,
        verification,
        mut timings,
        started,
    } = draft_block(raw, store, view, cfg)?;
    let checked = check(view, &block, created, inputs, cfg)?;
    record_context(&mut timings, &checked);
    timings.total = started.elapsed();
    Ok((checked.layer, verification, timings))
}

/// The second half of a validation: the scripts and the shielded batch, as two concurrent
/// tasks. Each stops at its first broken rule; the result is the first error in stage
/// order (scripts, shielded).
pub fn verify(verification: Verification<'_>) -> Result<VerifyTimings, BlockError> {
    let started = Instant::now();
    let Verification {
        drafts,
        unknown,
        batch,
    } = verification;
    let scripts = || {
        let stage_started = Instant::now();
        let verdict = check_scripts(&drafts).map_err(|(k, error)| BlockError::Prepare {
            tx: unknown[k],
            error,
        });
        (verdict, stage_started.elapsed())
    };
    let shielded = || {
        let stage_started = Instant::now();
        let outcome = batch.finalize();
        let verdict = if outcome.failed.is_empty() {
            Ok(())
        } else {
            Err(BlockError::Shielded(outcome.failed))
        };
        (verdict, stage_started.elapsed())
    };
    let ((scripts, scripts_took), (shielded, shielded_took)) = rayon::join(scripts, shielded);
    scripts?;
    shielded?;
    Ok(VerifyTimings {
        scripts: scripts_took,
        shielded: shielded_took,
        total: started.elapsed(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum PrebuildError {
    /// A transaction of the body is not in the prepared store, or not verified under the
    /// epoch of the next block.
    #[error("transaction {0:?} is not prepared and verified under this epoch")]
    NotPrepared(WtxId),
    #[error("{0}")]
    Context(#[from] ContextError),
}

/// Prebuilds the body `ids` (block order, without a coinbase) on top of `view`. Every
/// transaction must be in `store`, prepared under the epoch of `cfg`, with its scripts and proofs
/// verified: the prebuilt body then holds the whole verification of the body.
pub fn prebuild(
    ids: &[WtxId],
    store: &PreparedStore,
    view: &ChainView,
    cfg: &ValidateConfig,
) -> Result<PrebuiltBody, PrebuildError> {
    let mut txs = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(tx) = store
            .get(id)
            .filter(|p| p.epoch == cfg.epoch() && p.scripts_ok && p.shielded_ok)
        else {
            return Err(PrebuildError::NotPrepared(*id));
        };
        txs.push(tx);
    }
    let check_cfg = cfg.check_config();
    Ok(prebuild_body(view, &txs, &check_cfg)?)
}

/// Why [`commit_prebuilt`] did not commit a block.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    /// The block is not the prebuilt one (another parent, another body, a shielded
    /// coinbase): validate it with [`validate_block`].
    #[error("block does not match the prebuilt body: {0}")]
    Mismatch(&'static str),
    /// The block is invalid; [`validate_block`] would reject it as well.
    #[error("{0}")]
    Invalid(#[from] BlockError),
}

impl From<SwapError> for CommitError {
    fn from(e: SwapError) -> Self {
        match e {
            SwapError::Mismatch(reason) => CommitError::Mismatch(reason),
            SwapError::MerkleRoot => CommitError::Invalid(BlockError::MerkleRoot),
            SwapError::Context(e) => CommitError::Invalid(BlockError::Context(e)),
        }
    }
}

/// Commits `raw` with a body prebuilt by [`prebuild`]: the first transaction must be a
/// coinbase ([`ContextError::NoCoinbase`]), the coinbase is drafted and its
/// contextual rules checked (a coinbase has no input, so it has no script to run), the
/// header roots come from the coinbase and the prebuilt branches, and the layer is the
/// prebuilt state plus the coinbase outputs.
pub fn commit_prebuilt(
    raw: &RawBlock,
    prebuilt: PrebuiltBody,
    view: &ChainView,
    cfg: &ValidateConfig,
) -> Result<(Layer, Timings), CommitError> {
    let started = Instant::now();
    if !prebuilt.matches(raw) {
        return Err(CommitError::Mismatch("another parent or another body"));
    }
    check_above_mandatory_checkpoint(cfg.network, prebuilt.height)?;
    check_block_header(raw, view, cfg.network, cfg.header)?;
    // The first transaction is a transaction of a peer: only a coinbase has no coin to
    // spend, so only a coinbase has a draft without coins.
    let Some(first) = raw
        .txs
        .first()
        .filter(|t| matches!(t.tx.transparent_bundle(), Some(b) if b.is_coinbase()))
    else {
        return Err(BlockError::Context(ContextError::NoCoinbase).into());
    };
    let coinbase = draft(first.clone(), cfg.epoch(), Vec::new())
        .map_err(|error| BlockError::Prepare { tx: 0, error })?;
    let check_cfg = cfg.check_config();
    let known = prebuilt.wtxids.len();
    let checked = prebuilt.commit(view, raw, coinbase.shared(), &check_cfg)?;
    let mut timings = Timings::default();
    record_context(&mut timings, &checked);
    timings.known = known;
    timings.unknown = 1;
    timings.total = started.elapsed();
    Ok((checked.layer, timings))
}

fn check(
    view: &ChainView,
    block: &PreparedBlock,
    created: Map<OutPoint, Coin>,
    inputs: Vec<Vec<Coin>>,
    cfg: &ValidateConfig,
) -> Result<Checked, BlockError> {
    let check_cfg = cfg.check_config();
    contextual_check_with_outputs(view, block, created, inputs, &check_cfg)
        .map_err(BlockError::Context)
}

fn record_context(timings: &mut Timings, checked: &Checked) {
    timings.context = checked.timings.context;
    timings.trees = checked.timings.trees;
    timings.history = checked.timings.history;
}

fn draft_block<'k>(
    raw: RawBlock,
    store: &PreparedStore,
    view: &ChainView,
    cfg: &'k ValidateConfig,
) -> Result<Drafted<'k>, BlockError> {
    let started = Instant::now();
    let mut timings = Timings::default();
    let height = check_parent(view, &raw)?;
    check_above_mandatory_checkpoint(cfg.network, height)?;
    check_block_header(&raw, view, cfg.network, cfg.header)?;

    // 1. Roots. The auth data root is an input of the header commitment rule, which the
    // contextual check applies once it knows the parent's history tree.
    let auth = check_roots(&raw)?;
    timings.roots = started.elapsed();

    // 2. Known / unknown. A store hit is reused only if it was prepared under this epoch
    // and fully verified; the coinbase is never stored.
    let mut slots: Vec<Option<Arc<PreparedTx>>> = raw
        .txs
        .iter()
        .map(|t| {
            store
                .get(&t.wtxid())
                .filter(|p| p.epoch == cfg.epoch() && p.scripts_ok && p.shielded_ok)
        })
        .collect();
    let unknown: Vec<usize> = slots
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| match slot {
            None => Some(i),
            Some(_) => None,
        })
        .collect();
    timings.known = raw.txs.len() - unknown.len();
    timings.unknown = unknown.len();

    // 3. One coin round for every input of the block that is not created in the block. The
    // block's own outputs are built once here and handed to the contextual check, which
    // makes them the layer's `created` map; the resolved inputs serve the drafts and the
    // check.
    let lookup_started = Instant::now();
    let created = block_outputs(&raw, height);
    let inputs = resolve_inputs(view, &raw, &created)?;
    timings.lookup = lookup_started.elapsed();

    // Drafts in parallel. The drafts (3.4 KiB each: the retyped transaction and its digests
    // for the sighashes) land in one exactly sized vector rather than rayon's per-task
    // vectors concatenated at the end.
    let prepare_started = Instant::now();
    let mut drafted: Vec<Result<Draft, BlockError>> = Vec::with_capacity(unknown.len());
    unknown
        .par_iter()
        .map(|&i| {
            draft(raw.txs[i].clone(), cfg.epoch(), inputs[i].clone())
                .map_err(|error| BlockError::Prepare { tx: i, error })
        })
        .collect_into_vec(&mut drafted);
    let mut drafts: Vec<Draft> = Vec::with_capacity(drafted.len());
    for d in drafted {
        drafts.push(d?);
    }
    for (k, d) in drafts.iter().enumerate() {
        slots[unknown[k]] = Some(d.shared().clone());
    }
    let mut batch = ScopedBatch::new(&cfg.keys);
    for (k, d) in drafts.iter().enumerate() {
        d.add_shielded(&mut batch)
            .map_err(|error| BlockError::Prepare {
                tx: unknown[k],
                error,
            })?;
    }
    timings.prepare_unknown = prepare_started.elapsed();

    let txs: Vec<Arc<PreparedTx>> = slots
        .into_iter()
        .map(|s| s.expect("every slot is filled"))
        .collect();
    Ok(Drafted {
        block: PreparedBlock {
            raw,
            txs,
            auth_data_root: auth,
        },
        created,
        inputs,
        verification: Verification {
            drafts,
            unknown,
            batch,
        },
        timings,
        started,
    })
}
