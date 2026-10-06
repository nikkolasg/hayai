//! Node assembly and the driver loop.
//!
//! Threads: the hayai-net relay (one reader per peer, a ticker), the RPC and metrics
//! servers (one thread per connection), the driver, the node ticker, the trace writer and,
//! in shadow mode, the follower. CPU-heavy work runs on the one global rayon pool.
//!
//! The driver owns the chain state. Every block reaches it as an [`Event`]: from the relay
//! (`BlockSink`, after the header check and the forward), from the producer through the
//! relay, from the block download, or from the shadow follower. For each block it emits
//! `commit_start`, validates with hayai-validate, pushes the layer, appends the wire bytes
//! to the block store, finalizes and flushes per policy, updates the prepared store and
//! the live template, and emits `commit_finish`.
//!
//! Full mode. The node synchronizes from its peers (`crate::sync`, `full`): the header
//! chain selects the best chain by cumulative work, the download scheduler gives the
//! blocks of that chain in height order, and the driver validates them on the speculative
//! tip (`build_layer`, `Chain::push_speculative`, `verify`, `Chain::confirm`). A block of
//! the relay is a body from another source of the same path.
//!
//! Template as lane. In full mode with the compact relay, every template change goes to the
//! relay as a batch of its additions and a candidate (`hayai_relay::LanePublisher`,
//! `Relay::publish_candidate`), so peers rebuild this node's blocks from a reference.
//! `mining.lane_publication` is the choice of the miner: the whole template, the template
//! without the private transactions (`Mempool::admit_private`), or nothing.
//!
//! Prebuilt bodies. While idle, at most once per [`PREBUILD_INTERVAL`], the driver
//! prebuilds the body of the newest template (`mining.prebuild_own`) and of up to
//! `network.prebuilt_candidates` candidates of peers' lanes on the tip
//! (`hayai_validate::prebuild`). A block whose body is one of them commits with
//! `hayai_validate::commit_prebuilt`: the header and coinbase rules, then the prebuilt
//! layer. Any other block takes `validate_block`.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{select, unbounded, Receiver, Sender};
use hayai_blockstore::BlockStore;
use hayai_coins::{
    BestBlock, Coin, CoinsBacking, MemBacking, MemConfig, OutPoint, Pool, RocksBacking,
};
use hayai_consensus::difficulty::expected_bits;
use hayai_consensus::{ParentChain, DIFFICULTY_CONTEXT_BLOCKS};
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_index::{BlockJob, IndexWriter, TreesBefore, WalletIndex};
use hayai_net::{
    AddrBook, BlockSink, ChainSource, CompactVer, Direction, HistoryRootSource, IncomingBlock,
    PeerConfig, PeerEnv, PeerManager, PeerProtocol, Relay, RelayConfig, RelayDeps, Source,
    SyncSink,
};
use hayai_prepared::{PreparedStore, VerifyingKeys, MEMPOOL_TX_COST_LIMIT};
use hayai_relay::{LaneId, LanePublisher};
use hayai_rpc::{
    BlockSubmitSink, Cookie, HttpServer, MetricsServer, Registry, Rpc, RpcConfig, SubmitOutcome,
    SubmittedBlock, TemplateFeed, TipSource,
};
use hayai_state::history::HistoryState;
use hayai_state::PrebuiltBody;
use hayai_state::{Anchors, Base, Chain, ChainView, Frontiers, Layer, LAYER_WINDOW};
use hayai_sync::download::DownloadConfig;
use hayai_sync::headers::{HeaderChain, Status};
use hayai_template::{
    CoinbaseSpec, LiveTemplate, SetEvent, StoredTemplate, TemplateConfig, TemplateUpdate, Tip,
    Zip317Params,
};
use hayai_trace::{event, Table, Tracer};
use hayai_validate::{
    apply_checkpointed, commit_prebuilt, prebuild, validate_block, BlockError, CommitError,
    HeaderPolicy, Timings, ValidateConfig,
};
use hayai_wire::header::{BlockHash, BlockHeader};
use hayai_wire::{RawBlock, RawTx, WtxId};
use parking_lot::{Condvar, Mutex, RwLock};
use serde_json::json;

mod fault;
mod full;

use self::fault::{check_body, fault_of, Fault};
use crate::backing::{SpentLog, UpstreamBacking};
use crate::config::{Backend, Config, LanePublication, Mode};
use crate::headers::{HeaderIndex, NodeHeaderCheck, SeedBlock};
use crate::mempool::{Mempool, PublicTxs};
use crate::metrics::{
    hash_suffix, micros, register_build_info, stage_durations, LastBlock, NodeMetrics,
    TemplateKind, STAGES,
};
use crate::mining::{miner_script, Producer};
use crate::params::{NetParams, NetworkKind, REGTEST_POW_LIMIT_BITS};
use crate::persist::{StateLog, StateRecord};
use crate::process;
use crate::shadow::{self, UpstreamBlock};
use crate::sync::{Backlog, NetEvent, Sync, SyncConfig, SyncInbox, SyncParts};
use crate::upstream::Upstream;

/// Coinbase scriptSig bytes after the height.
const MINER_DATA: &[u8] = b"hayai";
/// Rejected block hashes remembered for `generate`, `submitblock` and the shadow verdicts.
const REMEMBERED_REJECTIONS: usize = 1024;
/// Blocks waiting on a pending parent, at most.
const MAX_WAITING: usize = 256;
/// Shortest time between two prebuild rounds: a template changes at most this often
/// towards pools as well (`docs/protocol-template-push.md`, Set event).
pub const PREBUILD_INTERVAL: Duration = Duration::from_millis(200);
/// Period of the clock of the block synchronization (stall rules, metrics).
const SYNC_TICK: Duration = Duration::from_millis(250);
/// Full mode: blocks of the committed chain that the header index holds. The window of
/// the layers is the largest reorg, and a state record names the 28 blocks that end at
/// the base.
const INDEX_BLOCKS: usize = LAYER_WINDOW + 2 * DIFFICULTY_CONTEXT_BLOCKS;
/// Full mode: the header log of the header chain, in `data_dir`.
const HEADER_LOG: &str = "headers.log";
/// Full mode: the address book of the peer manager, in `data_dir`.
const PEERS_FILE: &str = "peers.dat";
/// Full mode with `[state] wallet_index`: the wallet index, in `data_dir`.
const WALLET_INDEX_DIR: &str = "wallet-index";
/// Full mode: blocks before the end of the checkpoint range at which the node starts the
/// build of the Orchard keys. The checkpoint path reads no key, and a build takes about
/// 2 s, so the keys are ready before the first block with full validation.
const KEY_LEAD_BLOCKS: u32 = 1_000;
/// The wait of an idle driver with no prebuild due.
const IDLE_WAIT: Duration = Duration::from_secs(3600);

/// A fatal node error: the node stops and reports it.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct NodeError(pub String);

pub(crate) fn fatal(context: &str, e: impl std::fmt::Display) -> NodeError {
    NodeError(format!("{context}: {e}"))
}

/// The error of a height whose rule set this build does not have: the node stops.
fn no_rules(e: hayai_consensus::ConsensusError) -> NodeError {
    fatal("consensus rules", e)
}

/// Inputs of the driver.
pub enum Event {
    /// A header-checked block from the relay. `origin` is `local`, `legacy` or `compact`.
    Block {
        block: Arc<RawBlock>,
        origin: &'static str,
        source: Source,
        /// The arrival of the complete block at the queue of the driver.
        received: Instant,
    },
    /// Full mode: a message of the block synchronization and its arrival time.
    Net {
        event: NetEvent,
        at: Instant,
    },
    /// Shadow mode: the upstream best chain after `fork`, oldest first.
    Upstream {
        fork: BlockHash,
        blocks: Vec<UpstreamBlock>,
    },
    Shutdown,
    /// Stops the driver as a crash would: no final flush, snapshot or sync
    /// ([`Node::abandon`]).
    Abandon,
}

fn now_secs() -> u32 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    u32::try_from(secs).unwrap_or(u32::MAX)
}

/// The wall clock of `received` in microseconds since the Unix epoch: the field
/// `received_unix_us` of a `block_received` row.
pub(crate) fn received_unix_micros(received: Instant) -> u64 {
    hayai_trace::unix_micros().saturating_sub(micros(received.elapsed()))
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

// ----- tip watch -----

struct TipState {
    height: u32,
    hash: BlockHash,
    committed: VecDeque<BlockHash>,
    rejected: VecDeque<(BlockHash, String)>,
    stopped: Option<String>,
}

/// The committed tip, for the RPC and for callers that wait on a block's commit.
pub struct TipWatch {
    state: Mutex<TipState>,
    changed: Condvar,
}

impl TipWatch {
    fn new(height: u32, hash: BlockHash) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(TipState {
                height,
                hash,
                committed: VecDeque::new(),
                rejected: VecDeque::new(),
                stopped: None,
            }),
            changed: Condvar::new(),
        })
    }

    pub fn tip(&self) -> (u32, BlockHash) {
        let s = self.state.lock();
        (s.height, s.hash)
    }

    fn set(&self, height: u32, hash: BlockHash) {
        let mut s = self.state.lock();
        s.height = height;
        s.hash = hash;
        s.committed.push_back(hash);
        if s.committed.len() > REMEMBERED_REJECTIONS {
            s.committed.pop_front();
        }
        drop(s);
        self.changed.notify_all();
    }

    fn reject(&self, hash: BlockHash, reason: String) {
        let mut s = self.state.lock();
        s.rejected.push_back((hash, reason));
        if s.rejected.len() > REMEMBERED_REJECTIONS {
            s.rejected.pop_front();
        }
        drop(s);
        self.changed.notify_all();
    }

    fn stop(&self, reason: String) {
        self.state.lock().stopped = Some(reason);
        self.changed.notify_all();
    }

    /// Waits until `hash` is committed. Fails when the block is rejected, when the node
    /// stops, or after `timeout`.
    pub fn wait_for(&self, hash: &BlockHash, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut s = self.state.lock();
        loop {
            if s.committed.contains(hash) {
                return Ok(());
            }
            if let Some((_, reason)) = s.rejected.iter().find(|(h, _)| h == hash) {
                return Err(format!("block {hash} rejected: {reason}"));
            }
            if let Some(reason) = &s.stopped {
                return Err(format!("node stopped: {reason}"));
            }
            if self.changed.wait_until(&mut s, deadline).timed_out() {
                return Err(format!("block {hash} not committed after {timeout:?}"));
            }
        }
    }

    /// Waits until the tip reaches `height`; returns whether it did within `timeout`.
    pub fn wait_height(&self, height: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut s = self.state.lock();
        while s.height < height {
            if self.changed.wait_until(&mut s, deadline).timed_out() {
                return s.height >= height;
            }
        }
        true
    }
}

impl TipSource for TipWatch {
    fn tip(&self) -> (u32, BlockHash) {
        TipWatch::tip(self)
    }
}

// ----- relay and RPC sinks -----

fn origin(source: &Source) -> &'static str {
    match source {
        Source::Local => "local",
        Source::Peer {
            protocol: PeerProtocol::Legacy,
            ..
        } => "legacy",
        Source::Peer {
            protocol: PeerProtocol::CompactRelay(_),
            ..
        } => "compact",
    }
}

/// The relay's block sink: a `block_received` row, then the driver's queue.
struct BlockInbox {
    events: Sender<Event>,
    index: Arc<HeaderIndex>,
    tracer: Tracer,
}

impl BlockSink for BlockInbox {
    fn accept_block(&self, incoming: IncomingBlock) {
        let IncomingBlock { block, source } = incoming;
        let origin = origin(&source);
        let received = Instant::now();
        self.tracer
            .emit(Table::BlockSync, event::BLOCK_RECEIVED, || {
                let peer = match source {
                    Source::Local => None,
                    Source::Peer { id, .. } => Some(id.0),
                };
                json!({
                    "peer": peer,
                    "height": self.index.parent_context(&block.header.prev_hash).map(|p| p.height + 1),
                    "hash": block.hash().to_string(),
                    "bytes": block.bytes.len(),
                    "source": origin,
                    "received_unix_us": received_unix_micros(received),
                })
            });
        let event = Event::Block {
            block,
            origin,
            source,
            received,
        };
        if self.events.send(event).is_err() {
            tracing::debug!("driver stopped; block dropped");
        }
    }
}

/// What legacy peers read: headers and stored blocks.
struct ChainServe {
    params: NetParams,
    index: Arc<HeaderIndex>,
    blocks: Arc<BlockStore>,
    /// Full mode: the header chain, which holds every header from the genesis block.
    headers: Option<Arc<parking_lot::Mutex<HeaderChain>>>,
}

impl ChainServe {
    /// The consensus branch of the rule set of `height`. A height without a rule set is an
    /// error of the log here: the driver stops when its chain reaches that height.
    fn branch_at(&self, height: u32) -> Option<BranchId> {
        match self.params.branch_at(height) {
            Ok(branch) => Some(branch),
            Err(e) => {
                tracing::error!(height, error = %e, "no consensus branch for a relayed message");
                None
            }
        }
    }
}

impl ChainSource for ChainServe {
    fn tip_height(&self) -> u32 {
        self.index.tip().0
    }

    fn tip_hash(&self) -> BlockHash {
        self.index.tip().1
    }

    fn tx_branch(&self) -> Option<BranchId> {
        self.branch_at(self.index.tip().0 + 1)
    }

    fn block_branch(&self, parent: &BlockHash) -> Option<BranchId> {
        // A pending header is a parent too, as in the header check.
        self.branch_at(self.index.parent_context(parent)?.height + 1)
    }

    fn headers_after(
        &self,
        locator: &[BlockHash],
        stop: &BlockHash,
        validated_only: bool,
    ) -> Vec<hayai_wire::header::BlockHeader> {
        // ZIP 204: at most 160 headers in the answer to `getheaders`.
        let limit = hayai_net::codec::MAX_HEADERS;
        let Some(headers) = &self.headers else {
            return self.index.headers_after(locator, stop, limit);
        };
        // The node serves the headers of the blocks that it can send: the best header
        // chain up to the first block whose body the node does not have. A block that the
        // node sends to a compact-relay peer before its validation is one of them. A
        // legacy peer gets the headers of the validated blocks only.
        let chain = headers.lock();
        let mut found = match chain.headers_after(locator, stop, limit) {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(error = %e, "header log read failed");
                return Vec::new();
            }
        };
        let held = found
            .iter()
            .take_while(
                |header| match chain.entry(&header.hash()).map(|e| e.status) {
                    Some(Status::BodyValid) => true,
                    Some(Status::BodyKnown) => !validated_only,
                    Some(Status::HeaderValid | Status::Invalid) | None => false,
                },
            )
            .count();
        found.truncate(held);
        found
    }

    fn block_bytes(&self, hash: &BlockHash) -> Option<bytes::Bytes> {
        match self.blocks.get_by_hash(hash) {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(%hash, error = %e, "block store read failed");
                None
            }
        }
    }
}

/// ZIP 221 roots after recent blocks, for the relay's id-list check.
#[derive(Default)]
struct HistoryRoots {
    roots: Mutex<RecentRoots>,
}

#[derive(Default)]
struct RecentRoots {
    by_hash: HashMap<BlockHash, [u8; 32]>,
    order: VecDeque<BlockHash>,
}

impl HistoryRoots {
    fn insert(&self, hash: BlockHash, root: [u8; 32]) {
        let mut roots = self.roots.lock();
        roots.by_hash.insert(hash, root);
        roots.order.push_back(hash);
        if roots.order.len() > 2 * LAYER_WINDOW {
            if let Some(old) = roots.order.pop_front() {
                roots.by_hash.remove(&old);
            }
        }
    }
}

impl HistoryRootSource for HistoryRoots {
    fn history_root(&self, parent: &BlockHash) -> Option<[u8; 32]> {
        self.roots.lock().by_hash.get(parent).copied()
    }
}

/// `submitblock`: the header check, then the relay path, then the wait for the commit.
struct Submitter {
    params: NetParams,
    relay: Arc<Relay>,
    tip: Arc<TipWatch>,
    index: Arc<HeaderIndex>,
    header_check: Arc<NodeHeaderCheck>,
}

impl BlockSubmitSink for Submitter {
    fn submit(&self, block: SubmittedBlock) -> SubmitOutcome {
        let raw = match block {
            SubmittedBlock::Full(raw) => raw,
            SubmittedBlock::FromTemplate(rebuilt) => {
                let height = self.index.tip().0 + 1;
                let branch = match self.params.branch_at(height) {
                    Ok(branch) => branch,
                    Err(e) => return SubmitOutcome::Rejected(e.to_string()),
                };
                match RawBlock::parse(rebuilt.bytes, branch) {
                    Ok(raw) => raw,
                    Err(e) => return SubmitOutcome::Rejected(e.to_string()),
                }
            }
        };
        let hash = raw.hash();
        if self.index.contains(&hash) {
            return SubmitOutcome::Duplicate;
        }
        if let Err(e) = self.header_check.verify(&raw.header) {
            return SubmitOutcome::Rejected(e.to_string());
        }
        self.relay.block_found(raw);
        match self.tip.wait_for(&hash, Duration::from_secs(30)) {
            Ok(()) => SubmitOutcome::Accepted,
            Err(reason) => SubmitOutcome::Rejected(reason),
        }
    }
}

// ----- the driver -----

struct Driver {
    params: NetParams,
    mode: Mode,
    chain: Chain,
    index: Arc<HeaderIndex>,
    view: Arc<RwLock<ChainView>>,
    store: Arc<PreparedStore>,
    keys: Arc<VerifyingKeys>,
    live: LiveTemplate,
    set_events: Receiver<SetEvent>,
    feed: Arc<TemplateFeed>,
    blocks: Arc<BlockStore>,
    header_check: Arc<NodeHeaderCheck>,
    history_roots: Arc<HistoryRoots>,
    tip: Arc<TipWatch>,
    metrics: Arc<NodeMetrics>,
    tracer: Tracer,
    flush_interval: u32,
    since_flush: u32,
    /// The state records that go with the coins flushes.
    state_log: StateLog,
    /// The memory backend, for its snapshots; `None` with RocksDB.
    mem: Option<Arc<MemBacking>>,
    /// Finalized blocks between two snapshots.
    snapshot_interval: u32,
    since_snapshot: u32,
    rejected: HashMap<BlockHash, String>,
    rejected_order: VecDeque<BlockHash>,
    /// Blocks whose parent is a pending header, by parent hash.
    waiting: HashMap<BlockHash, Vec<Waiting>>,
    relay: Arc<Relay>,
    /// Full mode: the block synchronization.
    sync: Option<Sync>,
    mempool: Arc<Mempool>,
    /// Full mode: the transactions of the blocks that a reorg disconnected, in block order,
    /// until they go back to the mempool.
    disconnected: Vec<Arc<RawTx>>,
    /// The tip of the newest template, for the revert of a speculative template.
    template_tip: Option<Tip>,
    /// This node's lane: full mode with the compact relay.
    lane: Option<LanePublisher>,
    prebuilt: Prebuilt,
    last_tick: Instant,
    /// Full mode: delivered blocks can wait for the next call of `process_delivered`.
    more_delivered: bool,
    /// Full mode: the template is not on the committed tip, because the node is far below
    /// the best header tip. `deferred_changes` has the transactions that left the store
    /// since the last template.
    template_deferred: bool,
    deferred_changes: TipChange,
    /// The reception of the newest block that the driver validated, for the time from
    /// the reception to the first template on that block.
    tip_received: Option<TipReceived>,
    /// The writer of the wallet index (`[state] wallet_index`).
    wallet: Option<IndexWriter>,
    /// The base of the last coins flush: the restart never goes below it, so the wallet
    /// index needs no undo record at or below it.
    durable_base: u32,
}

/// What the wallet index takes of a committed block besides the block: the coins that its
/// inputs spent, which the validation read, and the note commitment trees before it.
pub(crate) struct IndexInput {
    spent_coins: Vec<Vec<Coin>>,
    before: Frontiers,
}

impl IndexInput {
    /// Takes the spent coins out of `layer`, whose parent is the tip of `view`. The chain
    /// does not keep them.
    pub(crate) fn take(layer: &mut Layer, view: &ChainView) -> Self {
        Self {
            spent_coins: std::mem::take(&mut layer.spent_coins),
            before: view.frontiers(),
        }
    }

    fn job(self, height: u32, raw: &Arc<RawBlock>) -> BlockJob {
        BlockJob {
            height,
            hash: raw.hash().0,
            parent: raw.header.prev_hash.0,
            raw: raw.clone(),
            spent_coins: self.spent_coins,
            trees_before: TreesBefore {
                sapling: self.before.sapling,
                orchard: self.before.orchard,
                ironwood: self.before.ironwood,
            },
        }
    }
}

/// The reception time of a block, until the first template on that block has it.
struct TipReceived {
    hash: BlockHash,
    at: Instant,
    /// A template on the block took the time.
    used: bool,
}

/// A block of shadow mode that waits for its parent: the block, its origin and its
/// reception.
type Waiting = (Arc<RawBlock>, &'static str, Instant);

/// What the trace rows of one commit share.
struct CommitCtx {
    /// The reception of the block (`metrics::LastBlock`).
    received: Instant,
    origin: &'static str,
    /// The time of the push of the layer on the chain. The commit path sets it.
    push: Duration,
    started: Instant,
    height: u32,
    hash: BlockHash,
    hash_hex: String,
    commitments_checked: bool,
    trusted_anchors: u64,
    coins_before: u64,
    nullifiers_before: u64,
    /// What the wallet index takes of the block. The commit path sets it before the push.
    index: Option<IndexInput>,
}

/// The transactions that left the prepared store with the new blocks of the tip.
#[derive(Default)]
struct TipChange {
    /// The transactions of the blocks.
    mined: Vec<WtxId>,
    /// The transactions that conflict with a block or that expired, with their
    /// descendants.
    dropped: Vec<WtxId>,
}

/// The nullifiers that the block of `layer` revealed, with their pools.
fn layer_nullifiers(layer: &Layer) -> Vec<(Pool, [u8; 32])> {
    Pool::ALL
        .iter()
        .flat_map(|pool| {
            layer.nullifiers[pool.index()]
                .iter()
                .map(move |nf| (*pool, *nf))
        })
        .collect()
}

/// How the event loop ended.
enum Exit {
    Clean,
    Abandon,
}

/// The prebuilt bodies on the tip and what waits for a prebuild.
struct Prebuilt {
    own_enabled: bool,
    /// Candidates of peers' lanes prebuilt per tip, at most.
    candidates_max: usize,
    /// The newest template, when its body is not prebuilt yet.
    pending_own: Option<Arc<StoredTemplate>>,
    own: Option<PrebuiltBody>,
    /// Prebuilt candidates of peers' lanes, oldest first.
    candidates: VecDeque<((LaneId, u64), PrebuiltBody)>,
    last_round: Instant,
}

impl Prebuilt {
    fn new(own_enabled: bool, candidates_max: usize) -> Self {
        Self {
            own_enabled,
            candidates_max,
            pending_own: None,
            own: None,
            candidates: VecDeque::new(),
            last_round: Instant::now(),
        }
    }

    /// Whether a round has work: a template to prebuild, or candidates to look for.
    fn has_work(&self) -> bool {
        matches!(
            (&self.pending_own, self.candidates_max),
            (Some(_), _) | (None, 1..)
        )
    }

    /// The time until the next round, when one has work.
    fn wait(&self, now: Instant) -> Duration {
        match self.has_work() {
            true => (self.last_round + PREBUILD_INTERVAL).saturating_duration_since(now),
            false => IDLE_WAIT,
        }
    }

    /// Takes the prebuilt body of `raw`, with its origin, if one matches.
    fn take(&mut self, raw: &RawBlock) -> Option<(PrebuiltBody, &'static str)> {
        if let Some(own) = self.own.take_if(|own| own.matches(raw)) {
            return Some((own, "own"));
        }
        let position = self.candidates.iter().position(|(_, b)| b.matches(raw))?;
        let (_, body) = self.candidates.remove(position)?;
        Some((body, "candidate"))
    }

    /// The `apply_class` that `raw` is expected to take: `commit_finish` records the one it
    /// took (a prebuilt body can still turn out not to apply).
    fn class_of(&self, raw: &RawBlock) -> &'static str {
        if matches!(&self.own, Some(own) if own.matches(raw)) {
            return "prebuilt_own";
        }
        if self.candidates.iter().any(|(_, body)| body.matches(raw)) {
            return "prebuilt_candidate";
        }
        "full"
    }

    /// Drops every body: they extend the previous tip.
    fn clear(&mut self) {
        self.own = None;
        self.candidates.clear();
    }
}

impl Driver {
    fn run(mut self, events: Receiver<Event>) -> Result<(), NodeError> {
        let result = self.event_loop(&events);
        if let Ok(Exit::Abandon) = result {
            if let Some(wallet) = self.wallet.take() {
                wallet.abandon();
            }
            return Ok(());
        }
        let closed = self.close();
        result.map(|_| ()).and(closed)
    }

    fn event_loop(&mut self, events: &Receiver<Event>) -> Result<Exit, NodeError> {
        loop {
            // The tick runs on elapsed time, also while messages wait. Its clock is the
            // arrival time of the oldest message in the queue: a block that waits in the
            // queue arrived before the tick and is not late.
            if self.last_tick.elapsed() >= SYNC_TICK {
                self.last_tick = Instant::now();
                if let Some(sync) = &mut self.sync {
                    sync.tick()?;
                    self.process_delivered()?;
                }
            }
            // A busy driver still prebuilds once per interval.
            let mut wait = self.prebuilt.wait(Instant::now());
            if wait.is_zero() {
                self.prebuild_round()?;
                continue;
            }
            if let Mode::Full = self.mode {
                wait = wait.min(SYNC_TICK);
            }
            // Delivered blocks wait: the loop takes a message that is ready, else the
            // next batch.
            if self.more_delivered {
                wait = Duration::ZERO;
            }
            select! {
                recv(events) -> ev => match ev {
                    Ok(Event::Block { block, origin, source, received }) => match self.mode {
                        Mode::Full => self.on_relayed(block, source, received)?,
                        Mode::Shadow => self.on_block(block, origin, received)?,
                    },
                    Ok(Event::Net { event, at }) => self.on_net(event, at)?,
                    Ok(Event::Upstream { fork, blocks }) => self.on_upstream(fork, blocks)?,
                    Ok(Event::Shutdown) | Err(_) => return Ok(Exit::Clean),
                    Ok(Event::Abandon) => return Ok(Exit::Abandon),
                },
                recv(self.set_events) -> ev => match ev {
                    Ok(ev) => self.on_set_event(ev),
                    Err(_) => return Err(NodeError("prepared store event stream closed".into())),
                },
                default(wait) => {
                    if self.more_delivered {
                        self.process_delivered()?;
                    } else if self.prebuilt.wait(Instant::now()).is_zero() {
                        self.prebuild_round()?;
                    }
                }
            }
        }
    }

    /// The validation configuration of a block at `height`, with the rule set of that
    /// height.
    fn validate_config(&self, height: u32) -> Result<ValidateConfig, NodeError> {
        validate_config(self.params, self.mode, height, &self.keys).map_err(no_rules)
    }

    /// Prebuilds the newest template's body, then the candidates of peers' lanes on the tip
    /// that are not prebuilt yet, up to the configured number.
    fn prebuild_round(&mut self) -> Result<(), NodeError> {
        self.prebuilt.last_round = Instant::now();
        let (height, tip) = self.index.tip();
        let view = self.chain.view();
        let cfg = self.validate_config(height + 1)?;
        if let Some(template) = self.prebuilt.pending_own.take() {
            if template.tip.parent_hash == tip {
                let ids: Vec<WtxId> = template.txs.iter().map(|c| c.wtxid).collect();
                let started = Instant::now();
                match prebuild(&ids, &self.store, &view, &cfg) {
                    Ok(body) => self.prebuilt.own = Some(body),
                    Err(e) => {
                        self.prebuilt.own = None;
                        tracing::debug!(template = template.id, error = %e, "template body not prebuilt");
                    }
                }
                self.metrics
                    .prebuild_duration
                    .observe_duration(started.elapsed());
            }
        }
        if self.prebuilt.candidates_max == 0 {
            return Ok(());
        }
        for candidate in self.relay.candidates_on(&tip, self.prebuilt.candidates_max) {
            let key = (candidate.lane, candidate.seq);
            if self.prebuilt.candidates.iter().any(|(k, _)| *k == key) {
                continue;
            }
            let Some(ids) = canonical_body(&self.store, &candidate.ids) else {
                continue;
            };
            let started = Instant::now();
            let body = prebuild(&ids, &self.store, &view, &cfg);
            self.metrics
                .prebuild_duration
                .observe_duration(started.elapsed());
            match body {
                Ok(body) => {
                    self.prebuilt.candidates.push_back((key, body));
                    while self.prebuilt.candidates.len() > self.prebuilt.candidates_max {
                        self.prebuilt.candidates.pop_front();
                    }
                }
                Err(e) => {
                    tracing::debug!(lane = ?key.0, seq = key.1, error = %e, "candidate body not prebuilt")
                }
            }
        }
        Ok(())
    }

    /// The template update's side effects besides the feed: the lane publication and the
    /// prebuild of its body. A coinbase-only template is followed by a full one at once.
    fn after_update(&mut self, update: &TemplateUpdate) {
        let template = match update {
            TemplateUpdate::Empty(_) => return,
            TemplateUpdate::Full(t) | TemplateUpdate::Changed(t) => t,
            TemplateUpdate::Reverted { template, .. } => template,
        };
        if let Some(lane) = &mut self.lane {
            // A private transaction is in the template and not in the lane.
            let ids: Vec<WtxId> = template
                .txs
                .iter()
                .map(|c| c.wtxid)
                .filter(|id| !self.mempool.is_private(id))
                .collect();
            self.relay
                .publish_candidate(lane.publish(template.tip.parent_hash, template.id, &ids));
        }
        if self.prebuilt.own_enabled {
            self.prebuilt.pending_own = Some(template.clone());
        }
    }

    /// Writes the finalized coins and the best block record, writes a snapshot with the
    /// memory backend, and syncs the block files.
    fn close(&mut self) -> Result<(), NodeError> {
        self.flush_coins("final coins flush")?;
        self.snapshot()?;
        self.blocks
            .sync()
            .map_err(|e| fatal("final block store sync", e))?;
        if let Some(wallet) = self.wallet.take() {
            let stats = wallet.stats.clone();
            wallet.close().map_err(|e| fatal("wallet index", e))?;
            self.metrics.record_wallet_index(&stats);
            log_wallet_index(&stats);
        }
        Ok(())
    }

    /// Writes the state record of the base, then flushes the finalized coins with the best
    /// block record of the same base. The record comes first: a crash between the two writes
    /// leaves a record that the restart drops, never a best block without a record.
    fn flush_coins(&mut self, context: &str) -> Result<(), NodeError> {
        let height = self.chain.base().read().height;
        // The wallet index is durable with the base block before the coins store names the
        // base: a restart then only undoes index blocks above the base, from their undo
        // records. `persist` returns when a sync holds the base. That is usually the sync
        // that the flush before started, with the tip of that time: with a flush interval
        // below the finality depth (1,000 blocks), that tip is above this base. Else
        // `persist` waits for a new sync. The next sync runs in the background during the writes below, and during the blocks
        // up to the next flush. It removes only the undo records at or below the durable
        // base, which no restart goes below.
        if let Some(wallet) = &mut self.wallet {
            wallet
                .persist(height, self.durable_base)
                .map_err(|e| fatal("wallet index", e))?;
        }
        let ancestors = self
            .index
            .ancestors_at(height, DIFFICULTY_CONTEXT_BLOCKS)
            .ok_or_else(|| NodeError(format!("the header index does not hold height {height}")))?;
        let record = state_record(
            self.params,
            self.mode,
            &mut self.chain.base().write(),
            ancestors,
        );
        self.state_log
            .write(&record)
            .map_err(|e| fatal("state log", e))?;
        // The header log holds the header of the best block before the coins store names
        // that block.
        if let Some(sync) = &self.sync {
            sync.sync_headers()?;
        }
        // The block files hold each block up to the base before the coins store names the
        // base: the replay of the next start reads the blocks above the base from them.
        self.blocks
            .sync()
            .map_err(|e| fatal("block store sync", e))?;
        self.chain.flush().map_err(|e| fatal(context, e))?;
        self.durable_base = height;
        self.metrics.finalized_height.set(f64::from(height));
        Ok(())
    }

    /// Memory backend: writes a snapshot of the flushed coin set and truncates its log.
    fn snapshot(&mut self) -> Result<(), NodeError> {
        let Some(mem) = &self.mem else {
            return Ok(());
        };
        let started = Instant::now();
        let bytes = mem.snapshot().map_err(|e| fatal("coins snapshot", e))?;
        tracing::info!(
            bytes,
            coins = mem.coin_count(),
            elapsed_ms = millis(started.elapsed()),
            "coins snapshot written"
        );
        self.since_snapshot = 0;
        Ok(())
    }

    fn on_set_event(&mut self, ev: SetEvent) {
        match self.live.apply(ev) {
            Ok(Some(update)) => {
                publish(&self.feed, &self.tracer, &self.metrics, &update, None);
                self.after_update(&update);
            }
            Ok(None) => {}
            Err(e) => tracing::error!(error = %e, "live template rejected a store event"),
        }
    }

    /// Shadow mode: validates a block of the relay on the tip. A block whose parent is a
    /// pending header (still in the queue, or still validating on another path) waits for
    /// its parent's commit.
    fn on_block(
        &mut self,
        block: Arc<RawBlock>,
        origin: &'static str,
        received: Instant,
    ) -> Result<(), NodeError> {
        let mut queue = vec![(block, origin, received)];
        while let Some((block, origin, received)) = queue.pop() {
            let hash = block.hash();
            if self.index.contains(&hash) {
                continue;
            }
            let parent = block.header.prev_hash;
            let (_, tip) = self.index.tip();
            if parent != tip {
                let waiting: usize = self.waiting.values().map(Vec::len).sum();
                if self.index.is_pending(&parent) && waiting < MAX_WAITING {
                    self.waiting
                        .entry(parent)
                        .or_default()
                        .push((block, origin, received));
                } else {
                    tracing::info!(%hash, %parent, %tip, "block does not extend the tip; not validated");
                }
                continue;
            }
            match self.commit(block, origin, received)? {
                Ok(()) => {
                    if let Some(children) = self.waiting.remove(&hash) {
                        queue.extend(children);
                    }
                }
                // `commit` records the rejection: traces, metrics and the rejected set. The
                // blocks that wait on it cannot connect.
                Err(_) => {
                    self.waiting.remove(&hash);
                }
            }
        }
        Ok(())
    }

    fn remember_rejection(&mut self, hash: BlockHash, reason: String) {
        self.index.remove_pending(&hash);
        self.tip.reject(hash, reason.clone());
        self.rejected.insert(hash, reason);
        self.rejected_order.push_back(hash);
        if self.rejected_order.len() > REMEMBERED_REJECTIONS {
            if let Some(old) = self.rejected_order.pop_front() {
                self.rejected.remove(&old);
            }
        }
    }

    /// Shadow mode: accepts the anchors of `raw` that hayai does not hold as anchors from
    /// before the start height (see `shadow`), and returns how many it accepted.
    fn trust_anchors(&self, raw: &RawBlock, view: &ChainView) -> u64 {
        let mut accepted = 0;
        for tx in &raw.txs {
            let mut roots: Vec<(Pool, [u8; 32])> = Vec::new();
            if let Some(bundle) = tx.tx.sapling_bundle() {
                for spend in bundle.shielded_spends() {
                    roots.push((Pool::Sapling, spend.anchor().to_bytes()));
                }
            }
            if let Some(bundle) = tx.tx.orchard_bundle() {
                roots.push((Pool::Orchard, bundle.anchor().to_bytes()));
            }
            if let Some(bundle) = tx.tx.ironwood_bundle() {
                roots.push((Pool::Ironwood, bundle.anchor().to_bytes()));
            }
            for (pool, root) in roots {
                if !view.has_anchor(pool, &root) {
                    self.chain.base().write().insert_anchor(pool, root);
                    accepted += 1;
                }
            }
        }
        self.metrics.trusted_anchors.add(accepted);
        accepted
    }

    /// Starts the commit of `raw` at `height` on `view`: the `commit_start` row and the
    /// counters that the rows of the commit compare.
    fn begin_commit(
        &self,
        raw: &RawBlock,
        height: u32,
        origin: &'static str,
        received: Instant,
        expected_class: &str,
        view: &ChainView,
    ) -> CommitCtx {
        let started = Instant::now();
        let hash = raw.hash();
        let hash_hex = hash.to_string();
        self.tracer
            .emit(Table::CommitState, event::COMMIT_START, || {
                json!({
                    "source": "hayaid",
                    "apply_class": expected_class,
                    "origin": origin,
                    "height": height,
                    "hash": hash_hex,
                })
            });
        let trusted_anchors = match self.mode {
            Mode::Shadow => self.trust_anchors(raw, view),
            Mode::Full => 0,
        };
        let commitments_checked = match view.history() {
            Some(_) => true,
            None => {
                self.metrics.commitments_unchecked.inc();
                false
            }
        };
        CommitCtx {
            received,
            origin,
            push: Duration::ZERO,
            started,
            height,
            hash,
            hash_hex,
            commitments_checked,
            trusted_anchors,
            coins_before: self.metrics.trusted_coins.get(),
            nullifiers_before: self.metrics.trusted_nullifiers.get(),
            index: None,
        }
    }

    /// Records the rejection of the block of `ctx`: metrics, trace rows and the rejected
    /// set.
    fn reject_commit(&mut self, ctx: &CommitCtx, reason: &str, apply_class: &str, fault: Fault) {
        let elapsed = ctx.started.elapsed();
        let (height, hash) = (ctx.height, ctx.hash);
        // A block that the node cannot validate is not a rejected block.
        let (validated, finish) = match fault {
            Fault::Invalid => ("invalid", "rejected"),
            Fault::WrongBody => ("wrong_body", "rejected"),
            Fault::Local => ("not_validated", "stopped"),
        };
        if fault != Fault::Local {
            self.metrics
                .verify_failure
                .observe_duration(ctx.received.elapsed());
            self.metrics.blocks_rejected.inc();
        }
        tracing::warn!(%hash, height, error = %reason, result = validated, "block not committed");
        self.tracer
            .emit(Table::CommitState, event::BLOCK_VALIDATED, || {
                json!({
                    "height": height,
                    "hash": ctx.hash_hex,
                    "result": validated,
                    "reason": reason,
                    "block_commitments_checked": ctx.commitments_checked,
                    "trusted_coins": self.metrics.trusted_coins.get() - ctx.coins_before,
                    "trusted_nullifiers": self.metrics.trusted_nullifiers.get() - ctx.nullifiers_before,
                    "trusted_anchors": ctx.trusted_anchors,
                })
            });
        self.tracer
            .emit(Table::CommitState, event::COMMIT_FINISH, || {
                json!({
                    "source": "hayaid",
                    "apply_class": apply_class,
                    "height": height,
                    "hash": ctx.hash_hex,
                    "result": finish,
                    "reason": reason,
                    "elapsed_ms": millis(elapsed),
                })
            });
        self.remember_rejection(hash, reason.to_string());
    }

    /// Finishes the commit of `raw`, whose valid `layer` is the committed tip of the chain:
    /// the header index, the block store, the finalization and the flush, the prepared
    /// store, the metrics and the trace rows. `changes` gets the transactions that left the
    /// store.
    fn finish_commit(
        &mut self,
        ctx: &mut CommitCtx,
        raw: &Arc<RawBlock>,
        layer: &Layer,
        timings: &Timings,
        apply_class: &str,
        changes: &mut TipChange,
    ) -> Result<(), NodeError> {
        let (height, hash) = (ctx.height, ctx.hash);
        self.metrics.record_stages(timings);
        let contextual_commit_us =
            micros(timings.context + timings.trees + timings.history + ctx.push);
        self.metrics.store_hits.add(timings.known as u64);
        self.metrics.store_misses.add(timings.unknown as u64);
        let validated = Instant::now();
        let received_to_validated_us = micros(validated.duration_since(ctx.received));
        let validation_us = micros(validated.duration_since(ctx.started));
        self.tracer
            .emit(Table::CommitState, event::BLOCK_VALIDATED, || {
                let mut row = json!({
                    "height": height,
                    "hash": ctx.hash_hex,
                    "result": "valid",
                    "since_received_us": received_to_validated_us,
                    "validation_us": validation_us,
                    "contextual_commit_us": contextual_commit_us,
                    "bytes": raw.bytes.len(),
                    "class": block_class(raw),
                    "txs": raw.txs.len(),
                    "known": timings.known,
                    "unknown": timings.unknown,
                    "block_commitments_checked": ctx.commitments_checked,
                    "trusted_coins": self.metrics.trusted_coins.get() - ctx.coins_before,
                    "trusted_nullifiers": self.metrics.trusted_nullifiers.get() - ctx.nullifiers_before,
                    "trusted_anchors": ctx.trusted_anchors,
                });
                for (stage, d) in STAGES.iter().zip(stage_durations(timings)) {
                    row[format!("{stage}_us")] = json!(micros(d));
                }
                row
            });

        let spent: Vec<OutPoint> = layer.spent.iter().cloned().collect();
        let nullifiers = layer_nullifiers(layer);
        self.prebuilt.clear();
        self.index.push(raw.header.clone());
        if let Mode::Full = self.mode {
            self.index.prune(INDEX_BLOCKS);
        }
        self.blocks
            .append(height, raw)
            .map_err(|e| fatal("block store append", e))?;
        if let (Some(wallet), Some(input)) = (&self.wallet, ctx.index.take()) {
            wallet
                .apply(input.job(height, raw))
                .map_err(|e| fatal("wallet index", e))?;
        }
        if let Some(root) = layer.history_root() {
            self.history_roots.insert(hash, root);
        }
        let finalized = self
            .chain
            .finalize_excess(LAYER_WINDOW)
            .map_err(|e| fatal("finalize", e))?;
        self.since_snapshot += finalized as u32;
        self.metrics
            .base_height
            .set(f64::from(self.chain.base().read().height));
        self.since_flush += 1;
        if self.since_flush >= self.flush_interval {
            self.flush_coins("coins flush")?;
            self.since_flush = 0;
            if self.since_snapshot >= self.snapshot_interval {
                self.snapshot()?;
            }
        }
        request_keys(self.params, self.mode, &self.keys, height + 1).map_err(no_rules)?;
        // The store changes its epoch before the mempool sees the new tip: a transaction
        // of the old epoch is not valid in the next block, so the store drops it, and an
        // admission on the old tip cannot store one after the change.
        let epoch = self.params.epoch_at(height + 1).map_err(no_rules)?;
        // An admission that ran its checks on the old tip does not insert after this
        // point: it sees the tip change and runs again.
        let mempool = self.mempool.clone();
        let tip_change = mempool.tip_change();
        changes.dropped.extend(self.store.set_epoch(epoch));
        *self.view.write() = self.chain.view();
        self.store.remove_mined(&layer.wtxids);
        changes.mined.extend_from_slice(&layer.wtxids);
        changes
            .dropped
            .extend(self.store.remove_conflicting(&spent, &nullifiers));
        changes
            .dropped
            .extend(self.store.remove_expired(height + 1));
        mempool.forget_private(&layer.wtxids);
        mempool.forget_private(&changes.dropped);
        drop(tip_change);
        self.relay
            .set_min_peer_version(min_peer_version_at(self.params, height));
        self.tip.set(height, hash);
        self.note_received(hash, ctx.received);

        let committed = Instant::now();
        let elapsed = committed.duration_since(ctx.started);
        let received_to_committed_us = micros(committed.duration_since(ctx.received));
        let commit_us = micros(committed.duration_since(validated));
        self.metrics.record_last_block(
            &LastBlock {
                height,
                hash_suffix: hash_suffix(&ctx.hash_hex),
                source: ctx.origin,
                size_bytes: raw.bytes.len(),
                transactions: raw.txs.len(),
                received_to_validated_us,
                validation_us,
                commit_us,
                received_to_committed_us,
                contextual_commit_us,
            },
            timings,
        );
        self.metrics.verified_height.set(f64::from(height));
        self.metrics.committed_height.set(f64::from(height));
        self.metrics.verified_blocks.inc();
        self.metrics.commit_duration.observe_duration(elapsed);
        self.metrics
            .mempool_transactions
            .set(self.store.len() as f64);
        self.metrics
            .mempool_bytes
            .set(self.store.cost_bytes() as f64);
        self.tracer
            .emit(Table::CommitState, event::COMMIT_FINISH, || {
                json!({
                    "source": "hayaid",
                    "apply_class": apply_class,
                    "height": height,
                    "hash": ctx.hash_hex,
                    "result": "committed",
                    "elapsed_ms": millis(elapsed),
                    "received_to_commit_us": received_to_committed_us,
                    "commit_us": commit_us,
                })
            });
        Ok(())
    }

    /// Records the reception time of the block `hash` for the first template on it. A
    /// block that already has the record keeps it: the template of a speculative block
    /// comes before its commit.
    pub(super) fn note_received(&mut self, hash: BlockHash, at: Instant) {
        if !matches!(&self.tip_received, Some(known) if known.hash == hash) {
            self.tip_received = Some(TipReceived {
                hash,
                at,
                used: false,
            });
        }
    }

    /// The reception time of the block `parent` for a template on it, one time for each
    /// block: a later template on the same block is not the first one.
    pub(super) fn take_received(&mut self, parent: &BlockHash) -> Option<Instant> {
        match &mut self.tip_received {
            Some(known) if known.hash == *parent && !known.used => {
                known.used = true;
                Some(known.at)
            }
            _ => None,
        }
    }

    /// Validates `raw` on the committed tip and commits it, in one step: a prebuilt body
    /// when one matches, the checkpoint path for a block with a `checkpoint`, else the full
    /// validation. `checkpoint` is the hash that the header chain has at the height of the
    /// block, on a branch that reached a checkpoint at or above it. The outer error is
    /// fatal. The inner error is the rejection of the block, which this function records
    /// with its [`Fault`].
    fn commit_on_tip(
        &mut self,
        raw: &Arc<RawBlock>,
        origin: &'static str,
        received: Instant,
        checkpoint: Option<BlockHash>,
    ) -> Result<Result<TipChange, BlockError>, NodeError> {
        let view = self.chain.view();
        let height = view.tip_height() + 1;
        let expected_class = match checkpoint {
            Some(_) => "checkpoint",
            None => self.prebuilt.class_of(raw),
        };
        let mut ctx = self.begin_commit(raw, height, origin, received, expected_class, &view);
        let cfg = self.validate_config(height)?;
        // A body that the header does not commit to never reaches a commit path.
        let checked = match checkpoint {
            Some(_) => check_body(raw, &view, &cfg, false),
            None => check_body(raw, &view, &cfg, true),
        };
        if let (Ok(()), None, Some(sync)) = (&checked, checkpoint, &mut self.sync) {
            sync.body_checked(&raw.hash())?;
        }
        let (verdict, apply_class) = match (checked, checkpoint) {
            (Err(e), _) => (Err(e), expected_class),
            (Ok(()), Some(expected)) => (
                apply_checkpointed(raw, expected, &view, &cfg, self.params.kind.checkpoints()),
                "checkpoint",
            ),
            (Ok(()), None) => {
                await_keys(self.params, self.mode, &self.keys, height).map_err(no_rules)?;
                self.validate_or_swap(raw, &view, &cfg)
            }
        };
        let (mut layer, timings) = match verdict {
            Ok(done) => done,
            Err(e) => {
                self.reject_commit(&ctx, &e.to_string(), apply_class, fault_of(&e));
                return Ok(Err(e));
            }
        };
        ctx.index = Some(IndexInput::take(&mut layer, &view));
        let pushing = Instant::now();
        let layer = self.chain.push(layer).map_err(|e| fatal("chain push", e))?;
        ctx.push = pushing.elapsed();
        let mut changes = TipChange::default();
        self.finish_commit(&mut ctx, raw, &layer, &timings, apply_class, &mut changes)?;
        Ok(Ok(changes))
    }

    /// Shadow mode: validates `raw` on the tip, commits it and rebuilds the template. The
    /// outer error is fatal; the inner one is the block's rejection.
    fn commit(
        &mut self,
        raw: Arc<RawBlock>,
        origin: &'static str,
        received: Instant,
    ) -> Result<Result<(), String>, NodeError> {
        let (height, hash) = (self.chain.tip().height + 1, raw.hash());
        match self.commit_on_tip(&raw, origin, received, None)? {
            Ok(changes) => {
                self.on_tip(&changes)?;
                Ok(Ok(()))
            }
            Err(e) => match fault_of(&e) {
                Fault::WrongBody | Fault::Invalid => Ok(Err(e.to_string())),
                // hayai has no verdict for the block: the row says so, and the node stops.
                Fault::Local => {
                    let reason = e.to_string();
                    self.tracer
                        .emit(Table::CommitState, event::UPSTREAM_VERDICT, || {
                            json!({
                                "level": "error",
                                "height": height,
                                "hash": hash.to_string(),
                                "hayai": "not_validated",
                                "upstream": "accepted",
                                "agree": null,
                                "reason": reason,
                            })
                        });
                    Err(fault::local_fault(self.mode, height, &hash, &e))
                }
            },
        }
    }

    /// Validates `raw` on `view`: from a prebuilt body when one matches, else in full.
    /// Returns the verdict and the `apply_class` of the trace rows (`prebuilt_own`,
    /// `prebuilt_candidate` or `full`).
    fn validate_or_swap(
        &mut self,
        raw: &Arc<RawBlock>,
        view: &ChainView,
        cfg: &ValidateConfig,
    ) -> (Result<(Layer, Timings), BlockError>, &'static str) {
        if let Some((body, origin)) = self.prebuilt.take(raw) {
            match commit_prebuilt(raw, body, view, cfg) {
                Ok(done) => {
                    let (counter, class) = match origin {
                        "own" => (&self.metrics.prebuilt_commits_own, "prebuilt_own"),
                        _ => (
                            &self.metrics.prebuilt_commits_candidate,
                            "prebuilt_candidate",
                        ),
                    };
                    counter.inc();
                    return (Ok(done), class);
                }
                Err(CommitError::Invalid(e)) => return (Err(e), "prebuilt"),
                Err(CommitError::Mismatch(reason)) => {
                    tracing::debug!(hash = %raw.hash(), reason, "prebuilt body does not apply; validating in full");
                }
            }
        }
        (
            validate_block((**raw).clone(), &self.store, view, cfg),
            "full",
        )
    }

    /// The template tip on top of `view`: the block that a miner builds on the tip of
    /// `view`, with the time and the `nBits` that the header rules require of it.
    fn template_tip(&self, view: &ChainView) -> Result<Tip, NodeError> {
        let hayai_state::Tip { height, hash } = view.tip();
        let next = height + 1;
        let history_root = match (view.history(), self.mode) {
            (Some(history), _) => history.root(),
            // The shadow template is never served: a placeholder root keeps its timing.
            (None, Mode::Shadow) => [0; 32],
            (None, Mode::Full) => {
                return Err(NodeError(format!(
                    "the history tree after {hash} is unknown in full mode"
                )))
            }
        };
        let times = view.recent_times();
        let Some(mtp) = hayai_consensus::difficulty::median_time_past(&times) else {
            unreachable!("the view holds the time of its tip");
        };
        let time = template_time(self.params.kind, next, mtp, now_secs());
        // The `nBits` that the header rules require of a block at `next` with the time of
        // the template. On Testnet the value depends on that time (the minimum-difficulty
        // rule), so it is the value of this template time only.
        let bits = match self.params.kind {
            // Regtest has no expected `nBits` (`NetworkParams::disable_pow`).
            NetworkKind::Regtest | NetworkKind::ConfiguredRegtest(_) => REGTEST_POW_LIMIT_BITS,
            NetworkKind::Testnet | NetworkKind::Mainnet => {
                let bits: Vec<u32> = view
                    .difficulty_context()
                    .into_iter()
                    .map(|(_, bits)| bits)
                    .collect();
                let chain = ParentChain {
                    height: next,
                    times: &times,
                    bits: &bits,
                };
                expected_bits(self.params.kind, time, &chain)
                    .map_err(|e| fatal("template bits", e))?
            }
        };
        Ok(Tip {
            parent_hash: hash,
            height: next,
            time,
            median_time_past: mtp,
            bits,
            history_root,
            issued_supply: Some(view.value_pools().total()),
        })
    }

    /// Rebuilds the template on the committed tip: the coinbase-only template first, then
    /// the full one. `changes` has the candidates that the new blocks mined and the ones
    /// that left the store with them (`LiveTemplate::on_tip`).
    fn on_tip(&mut self, changes: &TipChange) -> Result<(), NodeError> {
        let tip = self.template_tip(&self.chain.view())?;
        let started = Instant::now();
        let timed = Some(TipEvent {
            started,
            received: self.take_received(&tip.parent_hash),
        });
        let (feed, tracer, metrics) = (&self.feed, &self.tracer, &self.metrics);
        let mut updates = Vec::new();
        self.live
            .on_tip(tip, &changes.mined, &changes.dropped, |update| {
                publish(feed, tracer, metrics, &update, timed);
                updates.push(update);
            })
            .map_err(|e| fatal("template", e))?;
        self.template_tip = Some(tip);
        for update in &updates {
            self.after_update(update);
        }
        Ok(())
    }

    /// Disconnects the committed blocks above `fork`. In full mode their transactions wait
    /// in `disconnected` for the mempool.
    fn disconnect_to(&mut self, fork: &BlockHash) -> Result<(), NodeError> {
        let mut returned = Vec::new();
        while self.chain.tip().hash != *fork {
            let hayai_state::Tip { height, hash } = self.chain.tip();
            let Some(_) = self.chain.pop() else {
                return Err(NodeError(format!(
                    "fork point {fork} is below hayai's {LAYER_WINDOW}-block window"
                )));
            };
            if let Some(wallet) = &mut self.wallet {
                wallet
                    .undo(height, hash.0)
                    .map_err(|e| fatal("wallet index", e))?;
            }
            let Some(_) = self.index.pop() else {
                return Err(NodeError(format!("header index cannot pop {hash}")));
            };
            if let Mode::Full = self.mode {
                let raw = self.stored_block(height, &hash)?;
                // The loop goes from the tip down: the older block goes first.
                returned.splice(0..0, raw.txs.iter().skip(1).cloned().map(Arc::new));
            }
            self.metrics.blocks_disconnected.inc();
            tracing::warn!(height, %hash, "block disconnected by a reorg");
            self.tracer.emit(
                Table::CommitState,
                event::BLOCK_DISCONNECTED,
                || json!({ "height": height, "hash": hash.to_string() }),
            );
            let (h, tip) = self.index.tip();
            self.relay
                .set_min_peer_version(min_peer_version_at(self.params, h));
            self.tip.set(h, tip);
        }
        returned.append(&mut self.disconnected);
        self.disconnected = returned;
        // A reorg across an activation height: the store takes the transactions of the
        // rule set of the next block again.
        let next = self.chain.tip().height + 1;
        let epoch = self.params.epoch_at(next).map_err(no_rules)?;
        let mempool = self.mempool.clone();
        let tip_change = mempool.tip_change();
        self.store.set_epoch(epoch);
        *self.view.write() = self.chain.view();
        drop(tip_change);
        self.prebuilt.clear();
        Ok(())
    }

    /// The committed block `hash` of `height` from the block store.
    fn stored_block(&self, height: u32, hash: &BlockHash) -> Result<RawBlock, NodeError> {
        let bytes = self
            .blocks
            .get_by_hash(hash)
            .map_err(|e| fatal("block store", e))?
            .ok_or_else(|| NodeError(format!("the block store does not hold the block {hash}")))?;
        RawBlock::parse(bytes, self.params.branch_at(height).map_err(no_rules)?)
            .map_err(|e| fatal(&format!("stored block {hash}"), e))
    }

    /// hayai's tree roots after `hash` against upstream's.
    fn compare_roots(&self, block: &UpstreamBlock) -> Result<(), String> {
        let hash = block.raw.hash();
        let Some(layer) = self.chain.layers().find(|l| l.hash == hash) else {
            return Err(format!(
                "the layer of {hash} left the window before the comparison"
            ));
        };
        let ours = layer.anchors;
        let theirs = Anchors {
            sapling: block.sapling_root,
            orchard: block.orchard_root,
            ironwood: block.ironwood_root,
        };
        if ours != theirs {
            return Err(format!(
                "tree roots differ: hayai {ours:?}, upstream {theirs:?}"
            ));
        }
        Ok(())
    }

    fn record_verdict(&self, block: &UpstreamBlock, verdict: &Result<(), String>) {
        let hash = block.raw.hash().to_string();
        match verdict {
            Ok(()) => {
                self.metrics.upstream_agreements.inc();
                self.tracer
                    .emit(Table::CommitState, event::UPSTREAM_VERDICT, || {
                        json!({
                            "height": block.height,
                            "hash": hash,
                            "hayai": "valid",
                            "upstream": "accepted",
                            "agree": true,
                        })
                    });
            }
            Err(reason) => {
                self.metrics.upstream_disagreements.inc();
                tracing::error!(height = block.height, %hash, %reason, "hayai disagrees with upstream");
                self.tracer
                    .emit(Table::CommitState, event::UPSTREAM_VERDICT, || {
                        json!({
                            "level": "error",
                            "height": block.height,
                            "hash": hash,
                            "hayai": "invalid",
                            "upstream": "accepted",
                            "agree": false,
                            "reason": reason,
                        })
                    });
            }
        }
    }

    /// Shadow mode: follows the upstream best chain. A block upstream accepted and hayai
    /// rejects is a disagreement: the node records it and stops, because it cannot build
    /// on a block it does not hold.
    fn on_upstream(
        &mut self,
        fork: BlockHash,
        blocks: Vec<UpstreamBlock>,
    ) -> Result<(), NodeError> {
        let mut first_new = 0;
        while let Some(b) = blocks.get(first_new) {
            if !self.index.contains(&b.raw.hash()) {
                break;
            }
            let verdict = self.compare_roots(b);
            self.record_verdict(b, &verdict);
            if let Err(reason) = verdict {
                return Err(NodeError(format!(
                    "disagreement at {}: {reason}",
                    b.raw.hash()
                )));
            }
            first_new += 1;
        }
        let fork = match first_new {
            0 => fork,
            n => blocks[n - 1].raw.hash(),
        };
        if !self.index.contains(&fork) {
            return Err(NodeError(format!(
                "upstream fork point {fork} is not on hayai's chain"
            )));
        }
        let rest = &blocks[first_new..];
        if rest.is_empty() && self.index.tip().1 == fork {
            return Ok(());
        }
        self.disconnect_to(&fork)?;
        if rest.is_empty() {
            return self.on_tip(&TipChange::default());
        }
        for b in rest {
            let hash = b.raw.hash();
            let verdict = if let Some(reason) = self.rejected.get(&hash) {
                Err(reason.clone())
            } else {
                match self.header_check.verify(&b.raw.header) {
                    Err(e) => Err(format!("header: {e}")),
                    Ok(_) => {
                        self.tracer
                            .emit(Table::BlockSync, event::BLOCK_RECEIVED, || {
                                json!({
                                    "peer": null,
                                    "height": b.height,
                                    "hash": hash.to_string(),
                                    "bytes": b.raw.bytes.len(),
                                    "source": "upstream_rpc",
                                    "received_unix_us": hayai_trace::unix_micros(),
                                })
                            });
                        self.commit(b.raw.clone(), "upstream_rpc", Instant::now())?
                    }
                }
            };
            let verdict = verdict.and_then(|()| self.compare_roots(b));
            self.record_verdict(b, &verdict);
            if let Err(reason) = verdict {
                return Err(NodeError(format!("disagreement at {hash}: {reason}")));
            }
        }
        Ok(())
    }
}

/// The clock of a template update that follows a tip change.
#[derive(Clone, Copy)]
struct TipEvent {
    /// The start of the template build on the new tip.
    started: Instant,
    /// The reception of the tip block, for the first template on that block only.
    received: Option<Instant>,
}

/// Gives `update` to the template feed, which serves `getblocktemplate`, the long poll
/// and the push protocol, then writes the trace row and the metrics. The stop point of
/// "template ready" is the reading of the clock after `TemplateFeed::publish` returns.
fn publish(
    feed: &TemplateFeed,
    tracer: &Tracer,
    metrics: &NodeMetrics,
    update: &TemplateUpdate,
    tip_event: Option<TipEvent>,
) {
    feed.publish(update);
    let ready = Instant::now();
    let t = update.template();
    let since_tip = tip_event.map(|e| ready.duration_since(e.started));
    let since_received_us = tip_event
        .and_then(|e| e.received)
        .map(|at| micros(ready.duration_since(at)));
    let fields = || {
        json!({
            "height": t.tip.height,
            "parent": t.tip.parent_hash.to_string(),
            "template_id": t.id,
            "txs": t.txs.len(),
            "fees": t.fees_total,
            "since_tip_us": since_tip.map(micros),
            "since_received_us": since_received_us,
        })
    };
    let record = |kind| {
        if let Some(us) = since_received_us {
            metrics.record_last_template(kind, t.tip.height.saturating_sub(1), us);
        }
    };
    match update {
        TemplateUpdate::Empty(_) => {
            if let Some(elapsed) = since_tip {
                metrics.template_empty_latency.observe_duration(elapsed);
            }
            record(TemplateKind::Empty);
            tracer.emit(Table::Template, event::TEMPLATE_EMPTY, fields)
        }
        // A revert is the full template on the parent of a rejected speculative block.
        TemplateUpdate::Full(_) | TemplateUpdate::Reverted { .. } => {
            if let Some(elapsed) = since_tip {
                metrics.template_full_latency.observe_duration(elapsed);
            }
            record(TemplateKind::Full);
            metrics.template_rebuilt.inc();
            tracer.emit(Table::Template, event::TEMPLATE_FULL, fields);
        }
        TemplateUpdate::Changed(_) => metrics.template_rebuilt.inc(),
    }
}

/// The canonical block order of the candidate set `ids`, when the store holds every one.
fn canonical_body(store: &PreparedStore, ids: &[WtxId]) -> Option<Vec<WtxId>> {
    let mut txs = Vec::with_capacity(ids.len());
    for id in ids {
        txs.push(store.get(id)?);
    }
    let raws: Vec<&RawTx> = txs.iter().map(|t| t.raw.as_ref()).collect();
    let order = hayai_wire::canonical_order_of(&raws).ok()?;
    Some(order.into_iter().map(|i| ids[i]).collect())
}

/// The block class of the benchmark plan: `empty` (coinbase only), `shielded` (a Sapling
/// or Orchard bundle outside the coinbase), else `transparent`.
pub fn block_class(raw: &RawBlock) -> &'static str {
    let rest = match raw.txs.get(1..) {
        None | Some([]) => return "empty",
        Some(rest) => rest,
    };
    let shielded = rest
        .iter()
        .any(|t| !matches!((t.tx.sapling_bundle(), t.tx.orchard_bundle()), (None, None)));
    if shielded {
        "shielded"
    } else {
        "transparent"
    }
}

/// The validation configuration of a block at `height` on the network of `params`: the
/// rule set comes from the height, never from a fixed upgrade.
fn validate_config(
    params: NetParams,
    mode: Mode,
    height: u32,
    keys: &Arc<VerifyingKeys>,
) -> Result<ValidateConfig, hayai_consensus::ConsensusError> {
    Ok(ValidateConfig {
        network: params.kind,
        rules: *params.rules_at(height)?,
        keys: keys.clone(),
        // A full node starts at the genesis block, so its context is never too short for a
        // header rule. A shadow node starts from upstream's state. Its seed holds the whole
        // context, so no rule waits for context. A shorter context is counted and trusted,
        // never taken as a pass (`hayai_shadow_trusted_bits_total`).
        header: match mode {
            Mode::Full => HeaderPolicy::Enforce,
            Mode::Shadow => HeaderPolicy::TrustShortContext,
        },
    })
}

/// The time of the template of the block at `height` whose median-time-past is `mtp`: the
/// clock of the node, inside the limits of the header rules. The time is above the
/// median-time-past, and at most the median-time-past plus 90 min from the start height of
/// that rule. A node whose tip is far behind its clock therefore gives the largest time
/// that the rules permit, not its clock.
fn template_time(network: NetworkKind, height: u32, mtp: u32, now: u32) -> u32 {
    now.clamp(mtp + 1, hayai_rpc::template::max_time(network, height, mtp))
}

/// The first height at or above `height` whose block has full validation: a full node
/// applies the blocks at or below the last checkpoint of the network with the checkpoint
/// path.
/// ZIP 204, ZIP 201: the oldest peer protocol version that the node accepts when its tip
/// is at `height`: the version of the upgrade of the tip, as Zakura
/// (`Version::min_remote_for_height`, `zakura-network/src/protocol/external/types.rs:32-48`).
fn min_peer_version_at(params: NetParams, height: u32) -> u32 {
    hayai_net::min_peer_version(params.wire(), params.kind.upgrade_at(height))
}

fn first_validated(params: NetParams, mode: Mode, height: u32) -> u32 {
    match (mode, params.kind.checkpoints().last_height()) {
        (Mode::Full, Some(last)) => height.max(last + 1),
        (Mode::Full, None) | (Mode::Shadow, _) => height,
    }
}

/// Starts the background build of the Orchard keys that the blocks from `height` on need:
/// the key of the rule set of the first block with full validation, and the key of the
/// next upgrade after it. The node calls this function at its start and after each commit,
/// so the key of an upgrade is in work from the activation of the upgrade before it. A
/// full node that is more than [`KEY_LEAD_BLOCKS`] below the end of the checkpoint range
/// builds no key.
fn request_keys(
    params: NetParams,
    mode: Mode,
    keys: &Arc<VerifyingKeys>,
    height: u32,
) -> Result<(), hayai_consensus::ConsensusError> {
    let from = first_validated(params, mode, height);
    if from - height > KEY_LEAD_BLOCKS {
        return Ok(());
    }
    let branches: Vec<_> = [Some(params.branch_at(from)?), params.next_branch(from)]
        .into_iter()
        .flatten()
        .collect();
    keys.prebuild_more(&branches);
    Ok(())
}

/// Makes the Orchard key of the rule set of `height` ready before the full validation of a
/// block at `height`. The caller is not a rayon worker: it waits for the build thread.
fn await_keys(
    params: NetParams,
    mode: Mode,
    keys: &Arc<VerifyingKeys>,
    height: u32,
) -> Result<(), hayai_consensus::ConsensusError> {
    if keys.has_orchard_key(params.branch_at(height)?) {
        return Ok(());
    }
    request_keys(params, mode, keys, height)?;
    keys.ready();
    Ok(())
}

// ----- assembly -----

/// A running node.
pub struct Node {
    pub p2p_addr: Option<SocketAddr>,
    pub rpc_addr: Option<SocketAddr>,
    /// The cookie file of the RPC server. `None`: no RPC server, or no authentication.
    pub rpc_cookie: Option<PathBuf>,
    pub metrics_addr: Option<SocketAddr>,
    pub relay: Arc<Relay>,
    pub tip: Arc<TipWatch>,
    pub producer: Option<Arc<Producer>>,
    pub mempool: Arc<Mempool>,
    events: Sender<Event>,
    done: Receiver<Result<(), String>>,
    /// The sender of `stop_requests`: the channel stays open without an RPC server.
    _stop_tx: Sender<()>,
    stop_requests: Receiver<()>,
    driver: JoinHandle<()>,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
    rpc: Option<Arc<HttpServer>>,
    metrics_server: Option<Arc<MetricsServer>>,
    tracer: Tracer,
}

/// The data of the coinbase input after the height: the marker of hayai, then `: ` and
/// `[mining] extra_coinbase_data` when the file has that key.
fn miner_data(extra: Option<&str>) -> Vec<u8> {
    match extra {
        Some(extra) => [MINER_DATA, b": ", extra.as_bytes()].concat(),
        None => MINER_DATA.to_vec(),
    }
}

/// The peer manager of a full node: the limits of `[network]`, and the address book in
/// the directory of `[network] cache_dir`.
fn peer_manager(
    network: &crate::config::NetworkSection,
    params: NetParams,
    data_dir: &Path,
) -> Result<Arc<PeerManager>, NodeError> {
    let mut config = PeerConfig::new(params.wire());
    let limits = network.peer_limits();
    if let Some(outbound) = limits.outbound {
        config.outbound_target = outbound;
    }
    if let Some(inbound) = limits.inbound {
        config.max_inbound = inbound;
    }
    if let Some(per_ip) = network.max_connections_per_ip {
        config.max_per_ip = per_ip;
    }
    if let Some(seeders) = network.initial_peers() {
        config.seeders = seeders.clone();
    }
    if let Some(ban_secs) = network.ban_secs {
        config.ban_secs = ban_secs;
    }
    let book = match network.peer_cache_dir(data_dir) {
        Some(dir) => {
            fs::create_dir_all(dir).map_err(|e| fatal("address book directory", e))?;
            let path = dir.join(PEERS_FILE);
            let book =
                AddrBook::load(&path, config.book.clone()).map_err(|e| fatal("address book", e))?;
            config.book_path = Some(path);
            book
        }
        None => AddrBook::new(config.book.clone()),
    };
    Ok(PeerManager::new(config, book, PeerEnv::system()))
}

fn require_empty(dir: &Path) -> Result<(), NodeError> {
    match fs::read_dir(dir) {
        Ok(mut entries) => match entries.next() {
            None => Ok(()),
            Some(_) => Err(NodeError(format!(
                "{} is not empty and cache_dir has no {}: it is not the data directory of \
                 a hayaid node; use an empty cache_dir",
                dir.display(),
                StateLog::FILE
            ))),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(fatal(&dir.display().to_string(), e)),
    }
}

/// Removes what a first start created in the data directory unless it is disarmed. The first
/// start disarms it once the start record of the state log is on disk: from then on the
/// directory is a valid, restartable one.
struct FreshDir {
    data_dir: PathBuf,
    existed: bool,
    armed: bool,
}

impl FreshDir {
    fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            existed: data_dir.exists(),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for FreshDir {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for dir in ["coins", "blocks", WALLET_INDEX_DIR] {
            let path = self.data_dir.join(dir);
            if let Err(e) = fs::remove_dir_all(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %path.display(), error = %e, "failed first start left a directory");
                }
            }
        }
        for file in [StateLog::FILE, "spent.log", HEADER_LOG, PEERS_FILE] {
            let path = self.data_dir.join(file);
            if let Err(e) = fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %path.display(), error = %e, "failed first start left a file");
                }
            }
        }
        if !self.existed {
            if let Err(e) = fs::remove_dir(&self.data_dir) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %self.data_dir.display(), error = %e, "failed first start left cache_dir");
                }
            }
        }
    }
}

/// The state record of the base at `ancestors`, taking the base's anchors.
fn state_record(
    params: NetParams,
    mode: Mode,
    base: &mut Base,
    ancestors: Vec<(BlockHash, u32)>,
) -> StateRecord {
    StateRecord {
        network: params.kind,
        mode,
        base: base.state(),
        ancestors,
        new_anchors: base.take_new_anchors(),
        new_sprout_trees: base.take_new_sprout_trees(),
    }
}

/// Creates the state log with the start record of a first start.
fn start_state_log(
    data_dir: &Path,
    params: NetParams,
    mode: Mode,
    base: &mut Base,
    ancestors: &[(BlockHash, u32)],
) -> Result<StateLog, NodeError> {
    let record = state_record(params, mode, base, ancestors.to_vec());
    StateLog::create(data_dir, &record).map_err(|e| fatal("state log", e))
}

/// What [`replay`] reads and writes besides the chain.
struct Replay<'a> {
    index: &'a HeaderIndex,
    blocks: &'a BlockStore,
    params: NetParams,
    mode: Mode,
    keys: &'a Arc<VerifyingKeys>,
    history_roots: &'a HistoryRoots,
    /// Height of the start record: the blocks above it are in the block store.
    start_height: u32,
    /// The writer of the wallet index, whose tip is the base of the chain.
    wallet: Option<&'a IndexWriter>,
}

/// Height of the last block that [`replay`] reads for a base at `base_height`: the end of
/// the block store. `None` when the store is empty and the base is the start record.
fn replay_end(
    blocks: &BlockStore,
    base_height: u32,
    start_height: u32,
) -> Result<Option<u32>, NodeError> {
    match blocks.tip_height() {
        None if base_height == start_height => Ok(None),
        Some(last) if last >= base_height => Ok(Some(last)),
        stored => Err(NodeError(format!(
            "the block store ends at {stored:?}, below the state at height {base_height}: \
             the block files do not match the coins store"
        ))),
    }
}

/// The block that the block store holds at `height`, with its header, when it extends the
/// block `tip`. `None` ends the replay.
fn stored_child(
    blocks: &BlockStore,
    height: u32,
    tip: BlockHash,
) -> Result<Option<(bytes::Bytes, BlockHeader)>, NodeError> {
    let bytes = blocks
        .get_bytes(height)
        .map_err(|e| fatal("block store", e))?
        .ok_or_else(|| NodeError(format!("the block store has no block at height {height}")))?;
    let header =
        BlockHeader::parse(&bytes).map_err(|e| fatal(&format!("replay of block {height}"), e))?;
    if header.prev_hash != tip {
        tracing::warn!(
            height,
            hash = %header.hash(),
            %tip,
            "stored block does not extend the replayed chain; the replay ends"
        );
        return Ok(None);
    }
    Ok(Some((bytes, header)))
}

/// The network of the data directory `data_dir` and the height of the tip that a restart
/// of the node on it resumes at. The function starts no node and writes nothing.
///
/// It applies the rule of the restart with the same code: the best block of the coins
/// store selects the record of the state log ([`StateLog::resume_point`]), and the blocks
/// of the block store above that base count while each one extends the block before
/// ([`replay_end`], [`stored_child`]). The restart also validates each of these blocks. A
/// block that fails there stops the start of the node, so it gives no other tip.
///
/// A node can run on `data_dir` during the call. The result is then a tip that the
/// directory held during the read, or an error when the node changed a file under it.
pub fn stored_tip(data_dir: &Path) -> Result<(NetworkKind, u32), NodeError> {
    if !StateLog::exists(data_dir) {
        return Err(NodeError(format!(
            "State directory doesn't have a chain tip block: {} has no {}",
            data_dir.display(),
            StateLog::FILE
        )));
    }
    let best = hayai_coins::stored_best_block(&data_dir.join("coins"))
        .map_err(|e| fatal("coins store", e))?;
    let resume = StateLog::resume_point(data_dir, best).map_err(|e| fatal("state log", e))?;
    let blocks =
        BlockStore::open_read_only(data_dir.join("blocks")).map_err(|e| fatal("block store", e))?;
    let (mut height, mut hash) = (resume.height, resume.hash);
    if let Some(last) = replay_end(&blocks, height, resume.start_height)? {
        for next in height + 1..=last {
            let Some((_, header)) = stored_child(&blocks, next, hash)? else {
                break;
            };
            (height, hash) = (next, header.hash());
        }
    }
    Ok((resume.network, height))
}

/// Pushes the blocks of the block store above the chain's tip, so that the layer window and
/// the tip are what they were before the stop. A block above the last checkpoint of the
/// network is validated in full. A block at or below it takes the checkpoint path of a
/// full node. A block that does not extend the tip ends the replay: after a reorg, the
/// index by height can name a block of the old branch above the tip of the new branch.
/// Returns the number of layers that were merged into the base while the blocks were
/// pushed.
fn replay(chain: &mut Chain, r: &Replay) -> Result<usize, NodeError> {
    let base_height = chain.tip().height;
    let Some(last) = replay_end(r.blocks, base_height, r.start_height)? else {
        return Ok(0);
    };
    let store = PreparedStore::new(
        r.params.epoch_at(base_height + 1).map_err(no_rules)?,
        MEMPOOL_TX_COST_LIMIT,
        Zip317Params::ZAKURA,
    );
    let started = Instant::now();
    let mut finalized = 0;
    let mut replayed = 0u32;
    for height in base_height + 1..=last {
        let Some((bytes, _)) = stored_child(r.blocks, height, chain.tip().hash)? else {
            break;
        };
        let raw = RawBlock::parse(bytes, r.params.branch_at(height).map_err(no_rules)?)
            .map_err(|e| fatal(&format!("replay of block {height}"), e))?;
        // The contextual header rules run in `validate_block`. The proof of work runs here,
        // and the rule against the clock of the node does not run on a stored block.
        hayai_consensus::header::check_proof_of_work(r.params.kind, &raw.header)
            .map_err(|e| fatal(&format!("replay of block {height}"), e))?;
        let cfg = validate_config(r.params, r.mode, height, r.keys).map_err(no_rules)?;
        // A stored block at or below the last checkpoint takes the checkpoint path, as it
        // does in the synchronization: the node validated it before it stored it.
        let checkpoints = r.params.kind.checkpoints();
        let checkpointed = matches!(checkpoints.last_height(), Some(last) if height <= last);
        let (mut layer, _) = match (checkpointed, r.mode) {
            // The header chain is not open during the replay, so the expected hash is the
            // hash of the stored block: the node compared the block with the header chain
            // before it stored it. The checkpoint list and the parent are checked again.
            (true, Mode::Full) => {
                apply_checkpointed(&raw, raw.hash(), &chain.view(), &cfg, checkpoints)
            }
            (false, Mode::Full) | (_, Mode::Shadow) => {
                await_keys(r.params, r.mode, r.keys, height).map_err(no_rules)?;
                validate_block(raw.clone(), &store, &chain.view(), &cfg)
            }
        }
        .map_err(|e| fatal(&format!("replay of block {height}"), e))?;
        if let Some(root) = layer.history_root() {
            r.history_roots.insert(raw.hash(), root);
        }
        let input = IndexInput::take(&mut layer, &chain.view());
        chain
            .push(layer)
            .map_err(|e| fatal("replay chain push", e))?;
        r.index.push(raw.header.clone());
        if let Some(wallet) = r.wallet {
            wallet
                .apply(input.job(height, &Arc::new(raw)))
                .map_err(|e| fatal("wallet index", e))?;
        }
        finalized += chain
            .finalize_excess(LAYER_WINDOW)
            .map_err(|e| fatal("replay finalize", e))?;
        replayed += 1;
    }
    tracing::info!(
        blocks = replayed,
        from = base_height + 1,
        elapsed_ms = millis(started.elapsed()),
        "blocks replayed from the block store"
    );
    Ok(finalized)
}

impl Node {
    /// Builds every component from `config`, starts the threads and returns once the node
    /// listens.
    pub fn start(config: &Config) -> Result<Node, NodeError> {
        let params = NetParams::new(
            config
                .consensus_network()
                .map_err(|e| fatal("configuration", e))?,
        );
        let mode = config.network.mode;
        let data_dir = &config.state.cache_dir;
        let coins_dir = data_dir.join("coins");
        let blocks_dir = data_dir.join("blocks");
        let resuming = StateLog::exists(data_dir);
        let wallet_dir = data_dir.join(WALLET_INDEX_DIR);
        if !resuming {
            require_empty(&coins_dir)?;
            require_empty(&blocks_dir)?;
            require_empty(&wallet_dir)?;
        }

        let tracer = match &config.network.zakura.trace_dir {
            Some(dir) => {
                Tracer::open(dir, &config.trace.node).map_err(|e| fatal("trace dir", e))?
            }
            None => Tracer::disabled(),
        };
        let registry = Registry::new();
        let metrics = Arc::new(NodeMetrics::new(&registry));
        register_build_info(&registry, config);

        // The upstream node and, at the first start, the seed: read before any file of the
        // data directory exists, so a failed seed leaves the directory as it was.
        let upstream = config
            .shadow
            .as_ref()
            .map(|shadow| Arc::new(Upstream::new(shadow.rpc_addr)));
        let seed = match (&upstream, &config.shadow, resuming) {
            (Some(upstream), Some(shadow), false) => {
                let seed = shadow::seed(upstream, params, shadow.start_height)
                    .map_err(|e| fatal("shadow seed", e))?;
                tracing::info!(height = seed.height, hash = %seed.hash, "shadow start state read from upstream");
                Some(seed)
            }
            _ => None,
        };

        // From here on a failed first start removes what it created, up to the write of the
        // start record of the state log.
        let mut fresh_dir = (!resuming).then(|| FreshDir::new(data_dir));
        fs::create_dir_all(data_dir).map_err(|e| fatal("cache_dir", e))?;
        let (store_backing, mem, best): (
            Arc<dyn CoinsBacking>,
            Option<Arc<MemBacking>>,
            Option<BestBlock>,
        ) = match config.state.backend {
            Backend::Rocksdb => {
                let rocks = RocksBacking::open(&coins_dir, &hayai_coins::Config::default())
                    .map_err(|e| fatal("coins store", e))?;
                let best = rocks.best_block().map_err(|e| fatal("coins store", e))?;
                (Arc::new(rocks), None, best)
            }
            Backend::Memory => {
                let (mem, recovery) = MemBacking::open(&coins_dir, &MemConfig::default())
                    .map_err(|e| fatal("coins store", e))?;
                tracing::info!(?recovery, "coins store opened");
                let best = mem.best_block().map_err(|e| fatal("coins store", e))?;
                let mem = Arc::new(mem);
                (mem.clone(), Some(mem), best)
            }
        };
        let blocks = Arc::new(BlockStore::open(&blocks_dir).map_err(|e| fatal("block store", e))?);

        // The base: restored from the state log, or the genesis block or the shadow seed.
        let spent_path = data_dir.join("spent.log");
        let (base, index, state_log, start_height) = match (resuming, seed) {
            (true, _) => {
                let (state_log, recovered) = StateLog::open(data_dir, params.kind, mode, best)
                    .map_err(|e| fatal("state log", e))?;
                let backing: Arc<dyn CoinsBacking> = match &upstream {
                    None => store_backing,
                    Some(upstream) => Arc::new(UpstreamBacking::new(
                        store_backing,
                        upstream.clone(),
                        recovered.start_height,
                        params
                            .branch_at(recovered.start_height + 1)
                            .map_err(no_rules)?,
                        metrics.clone(),
                        SpentLog::open(&spent_path, best.map(|b| b.height))
                            .map_err(|e| fatal("spent log", e))?,
                    )),
                };
                tracing::info!(
                    height = recovered.base.height,
                    hash = %recovered.base.hash,
                    "state restored from the state log"
                );
                // The base has the `nBits` of its newest blocks. With the ancestors of the
                // record they are the context of the header rules, as before the stop.
                let index = HeaderIndex::new(
                    recovered.base.height,
                    &SeedBlock::from_lists(&recovered.ancestors, &recovered.base.bits),
                );
                let start_height = recovered.start_height;
                let base = Base::restore(
                    backing,
                    recovered.base,
                    recovered.anchors,
                    recovered.sprout_trees,
                );
                (base, index, state_log, start_height)
            }
            (false, None) => {
                let (genesis, genesis_time) = params.genesis();
                let mut base = Base::new(store_backing, 0, genesis, genesis_time);
                base.history = Some(Arc::new(HistoryState::empty(
                    params.branch_at(0).map_err(no_rules)?,
                )));
                let ancestors = [(genesis, genesis_time)];
                let state_log = start_state_log(data_dir, params, mode, &mut base, &ancestors)?;
                // The node does not parse the genesis header: no rule reads its `nBits`.
                let index = HeaderIndex::new(0, &SeedBlock::from_lists(&ancestors, &[]));
                (base, index, state_log, 0)
            }
            (false, Some(seed)) => {
                let Some(upstream) = &upstream else {
                    unreachable!("a seed is read from the upstream node");
                };
                let backing: Arc<dyn CoinsBacking> = Arc::new(UpstreamBacking::new(
                    store_backing,
                    upstream.clone(),
                    seed.height,
                    params.branch_at(seed.height + 1).map_err(no_rules)?,
                    metrics.clone(),
                    SpentLog::open(&spent_path, None).map_err(|e| fatal("spent log", e))?,
                ));
                let times: Vec<u32> = seed.ancestors.iter().map(|block| block.time).collect();
                let bits: Vec<u32> = seed
                    .ancestors
                    .iter()
                    .filter_map(|block| block.bits)
                    .collect();
                let Some(time) = times.last() else {
                    unreachable!("a seed holds its start block");
                };
                let mut base = Base::new(backing, seed.height, seed.hash, *time);
                // The context of the header rules of the first block after the start: block
                // validation then checks the times and the `nBits` with no wait.
                base.set_header_context(&times, &bits);
                let anchors = Anchors {
                    sapling: seed.sapling.root().to_bytes(),
                    orchard: seed.orchard.root().to_bytes(),
                    ironwood: seed.ironwood.root().to_bytes(),
                };
                base.insert_anchor(Pool::Sapling, anchors.sapling);
                base.insert_anchor(Pool::Orchard, anchors.orchard);
                base.insert_anchor(Pool::Ironwood, anchors.ironwood);
                base.anchors = anchors;
                base.sapling_frontier = Arc::new(seed.sapling);
                base.orchard_frontier = Arc::new(seed.orchard);
                base.ironwood_frontier = Arc::new(seed.ironwood);
                // Upstream gives no Sprout treestate: a block with a JoinSplit is an error.
                base.set_sprout_unknown();
                base.value_pools = seed.value_pools;
                let ancestors: Vec<(BlockHash, u32)> = seed
                    .ancestors
                    .iter()
                    .map(|block| (block.hash, block.time))
                    .collect();
                let state_log = start_state_log(data_dir, params, mode, &mut base, &ancestors)?;
                let index = HeaderIndex::new(seed.height, &seed.ancestors);
                (base, index, state_log, seed.height)
            }
        };
        // The wallet index: at its first start its tip is the genesis block. At a restart it
        // undoes its blocks above the base, and the replay below indexes them again.
        let wallet_index = match (config.state.wallet_index, resuming) {
            (true, _) => {
                let wallet =
                    WalletIndex::open(&wallet_dir).map_err(|e| fatal("wallet index", e))?;
                if !resuming {
                    wallet
                        .start_at_genesis(&params.genesis().0 .0)
                        .map_err(|e| fatal("wallet index", e))?;
                }
                let undone = wallet
                    .rewind_to(base.height, &base.hash.0)
                    .map_err(|e| fatal("wallet index", e))?;
                tracing::info!(
                    undone,
                    base = base.height,
                    "wallet index rewound to the base"
                );
                Some(Arc::new(wallet))
            }
            (false, true) if wallet_dir.exists() => {
                tracing::warn!(
                    dir = %wallet_dir.display(),
                    "[state] wallet_index is off: the wallet index of cache_dir falls behind, \
                     and a start with the index on refuses it"
                );
                None
            }
            (false, _) => None,
        };
        let wallet = match &wallet_index {
            Some(index) => {
                Some(IndexWriter::spawn(index.clone()).map_err(|e| fatal("wallet index", e))?)
            }
            None => None,
        };
        let durable_base = base.height;
        if let Some(guard) = &mut fresh_dir {
            guard.disarm();
        }

        let mut chain = Chain::new(base);
        let chain_base = chain.base().clone();
        let index = Arc::new(index);
        let base_next = chain.tip().height + 1;
        let keys = Arc::new(VerifyingKeys::new());
        request_keys(params, mode, &keys, base_next).map_err(no_rules)?;
        // The Orchard keys must exist before the first batch: no batch builds a key. This
        // thread is not a rayon worker, so it can wait for the build on the pool.
        keys.ready();
        let history_roots = Arc::new(HistoryRoots::default());
        let replayed = replay(
            &mut chain,
            &Replay {
                index: &index,
                blocks: &blocks,
                params,
                mode,
                keys: &keys,
                history_roots: &history_roots,
                start_height,
                wallet: wallet.as_ref(),
            },
        )?;
        let (tip_height, tip_hash) = index.tip();
        // Full mode: the header chain, with the committed blocks as valid bodies.
        let headers = match mode {
            Mode::Full => Some(Arc::new(parking_lot::Mutex::new(full::open_header_chain(
                &data_dir.join(HEADER_LOG),
                params,
                chain.tip(),
                &blocks,
            )?))),
            Mode::Shadow => None,
        };
        let view = Arc::new(RwLock::new(chain.view()));
        let next = tip_height + 1;
        let store = Arc::new(PreparedStore::new(
            params.epoch_at(next).map_err(no_rules)?,
            config.mempool.tx_cost_limit as usize,
            Zip317Params::ZAKURA,
        ));
        let feed = TemplateFeed::new(Duration::from_secs(60));
        let script_pubkey = miner_script(&config.mining, params.kind).map_err(NodeError)?;
        let mut template_config = TemplateConfig::new(CoinbaseSpec {
            script_pubkey,
            miner_data: miner_data(config.mining.extra_coinbase_data.as_deref()),
            network: params.kind,
        });
        template_config.pow = params.pow();
        let mut live = LiveTemplate::new(template_config);
        let (initial, set_events) = live
            .load_from(store.as_ref())
            .map_err(|e| fatal("template load", e))?;
        if let Some(update) = initial {
            feed.publish(&update);
        }

        let (events_tx, events_rx) = unbounded();
        let header_check = Arc::new(NodeHeaderCheck {
            params,
            index: index.clone(),
            tracer: tracer.clone(),
            metrics: metrics.clone(),
            trust_short_context: mode == Mode::Shadow,
        });
        let mut relay_config = RelayConfig::new(params.wire());
        relay_config.min_peer_version = min_peer_version_at(params, tip_height);
        relay_config.compact_relay = match config.network.compact_relay {
            true => Some(CompactVer::CURRENT),
            false => None,
        };
        let mempool = Arc::new(Mempool::new(
            store.clone(),
            view.clone(),
            params,
            keys.clone(),
            metrics.clone(),
        ));
        let backlog = Arc::new(Backlog::default());
        let relay_deps = RelayDeps {
            txs: Arc::new(PublicTxs {
                store: store.clone(),
                mempool: mempool.clone(),
            }),
            tx_sink: mempool.clone(),
            block_sink: Arc::new(BlockInbox {
                events: events_tx.clone(),
                index: index.clone(),
                tracer: tracer.clone(),
            }),
            chain: Arc::new(ChainServe {
                params,
                index: index.clone(),
                blocks: blocks.clone(),
                headers: headers.clone(),
            }),
            header_check: header_check.clone(),
            history_roots: history_roots.clone(),
            sync: headers.clone().map(|headers| {
                Arc::new(SyncInbox {
                    events: events_tx.clone(),
                    headers,
                    params,
                    backlog: backlog.clone(),
                }) as Arc<dyn SyncSink>
            }),
        };
        // Full mode: the peer manager finds and keeps the outbound peers. A shadow node
        // has the peers of its configuration only.
        let relay = match mode {
            Mode::Full => Relay::with_peer_manager(
                relay_config,
                relay_deps,
                peer_manager(&config.network, params, data_dir)?,
            ),
            Mode::Shadow => Relay::new(relay_config, relay_deps),
        };
        mempool.set_relay(&relay);
        let p2p_addr = match config.network.listen_addr {
            Some(addr) => Some(relay.listen(addr).map_err(|e| fatal("P2P listen", e))?),
            None => None,
        };
        let tip = TipWatch::new(tip_height, tip_hash);

        let producer = match (config.mining.regtest_produce, params.kind.is_regtest()) {
            (true, true) => Some(Arc::new(Producer::new(
                params,
                feed.clone(),
                relay.clone(),
                tip.clone(),
            ))),
            _ => None,
        };
        let (stop_tx, stop_requests) = crossbeam_channel::bounded(1);
        let mut rpc_cookie = None;
        let rpc = match (config.rpc.listen_addr, &headers) {
            (Some(addr), Some(headers)) => {
                let submit = Arc::new(Submitter {
                    params,
                    relay: relay.clone(),
                    tip: tip.clone(),
                    index: index.clone(),
                    header_check: header_check.clone(),
                });
                let query = Arc::new(crate::query::Query {
                    params,
                    blocks: blocks.clone(),
                    view: view.clone(),
                    store: store.clone(),
                    mempool: mempool.clone(),
                    relay: relay.clone(),
                    base: chain_base.clone(),
                    headers: headers.clone(),
                    private: config.mining.lane_publication != LanePublication::All,
                    stop: stop_tx.clone(),
                    wallet: wallet_index.clone(),
                });
                let rpc = Rpc::with_parts(
                    RpcConfig {
                        metrics: Some(registry.clone()),
                        ..RpcConfig::new(params.kind)
                    },
                    feed.clone(),
                    submit,
                    tip.clone(),
                    producer
                        .clone()
                        .map(|p| p as Arc<dyn hayai_rpc::BlockGenerator>),
                    Some(query),
                );
                let cookie = match config.rpc.enable_cookie_auth {
                    true => {
                        let dir = config.rpc.cookie_dir.as_deref().unwrap_or(data_dir);
                        Some(Cookie::create(dir).map_err(|e| fatal("RPC cookie file", e))?)
                    }
                    false => {
                        if let Some(addr) = config.rpc.open_addr() {
                            tracing::warn!(
                                %addr,
                                "the RPC server has no authentication and its address is not a \
                                 loopback address: each host that reaches it can call each method"
                            );
                        }
                        None
                    }
                };
                rpc_cookie = cookie.as_ref().map(|c| c.path().to_path_buf());
                Some(HttpServer::serve(addr, rpc, cookie).map_err(|e| fatal("RPC listen", e))?)
            }
            // The configuration check refuses an RPC address in shadow mode, which has no
            // header chain.
            (Some(_), None) | (None, _) => None,
        };
        let metrics_server = match config.metrics.endpoint_addr {
            Some(addr) => Some(
                MetricsServer::serve(addr, registry.clone())
                    .map_err(|e| fatal("metrics listen", e))?,
            ),
            None => None,
        };
        metrics.verified_height.set(f64::from(tip_height));
        metrics.committed_height.set(f64::from(tip_height));
        // The base of a start is the base of the last flush.
        metrics
            .finalized_height
            .set(f64::from(chain_base.read().height));
        metrics.base_height.set(f64::from(chain_base.read().height));

        let mut driver = Driver {
            params,
            mode: config.network.mode,
            chain,
            index: index.clone(),
            view,
            store: store.clone(),
            keys,
            live,
            set_events,
            feed: feed.clone(),
            blocks,
            header_check,
            history_roots,
            tip: tip.clone(),
            metrics: metrics.clone(),
            tracer: tracer.clone(),
            flush_interval: config.state.flush_interval_blocks,
            since_flush: 0,
            state_log,
            mem: mem.clone(),
            snapshot_interval: config.state.snapshot_interval_blocks,
            since_snapshot: replayed as u32,
            rejected: HashMap::new(),
            rejected_order: VecDeque::new(),
            waiting: HashMap::new(),
            relay: relay.clone(),
            sync: match headers {
                Some(headers) => Some(Sync::new(
                    params,
                    SyncConfig {
                        download: DownloadConfig {
                            memory_budget_bytes: config.sync.memory_budget_bytes,
                            request_timeout_ms: config.sync.request_timeout_ms,
                            ..DownloadConfig::default()
                        },
                        header_timeout_ms: config.sync.header_timeout_ms,
                        header_poll_ms: config.sync.header_poll_ms,
                        header_poll_max_ms: config.sync.header_poll_max_ms,
                    },
                    SyncParts {
                        headers,
                        index: index.clone(),
                        relay: relay.clone(),
                        backlog,
                        metrics: metrics.clone(),
                        tracer: tracer.clone(),
                    },
                )?),
                None => None,
            },
            mempool: mempool.clone(),
            disconnected: Vec::new(),
            template_tip: None,
            last_tick: Instant::now(),
            more_delivered: false,
            template_deferred: false,
            tip_received: None,
            deferred_changes: TipChange::default(),
            wallet,
            durable_base,
            lane: match (
                config.network.mode,
                config.network.compact_relay,
                config.mining.lane_publication,
            ) {
                (Mode::Full, true, LanePublication::All | LanePublication::Public) => {
                    Some(LanePublisher::new(rand::random()))
                }
                (Mode::Full, true, LanePublication::None)
                | (Mode::Full, false, _)
                | (Mode::Shadow, ..) => None,
            },
            prebuilt: Prebuilt::new(
                config.mining.prebuild_own && config.network.mode == Mode::Full,
                config.network.prebuilt_candidates,
            ),
        };
        driver.on_tip(&TipChange::default())?;
        let (done_tx, done) = crossbeam_channel::bounded(1);
        let driver_tip = tip.clone();
        let driver = thread::Builder::new()
            .name("driver".into())
            .spawn(move || {
                let result = driver.run(events_rx).map_err(|e| e.to_string());
                if let Err(e) = &result {
                    tracing::error!(error = %e, "driver stopped");
                    driver_tip.stop(e.clone());
                }
                let _ = done_tx.send(result);
            })
            .map_err(|e| fatal("driver thread", e))?;

        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::new();
        if let (Some(upstream), Some(shadow)) = (upstream, &config.shadow) {
            workers.push(
                shadow::spawn_follower(
                    upstream,
                    params,
                    index.clone(),
                    events_tx.clone(),
                    stop.clone(),
                    Duration::from_millis(shadow.poll_interval_ms),
                )
                .map_err(|e| fatal("follower thread", e))?,
            );
        }
        // Full mode: the connection thread dials the addresses of the book and asks the DNS
        // seeders until the node has its outbound peers. Without it the node has only the
        // peers of `[network] peers`.
        if let Mode::Full = mode {
            workers.push(
                relay
                    .peer_manager()
                    .spawn(&relay)
                    .map_err(|e| fatal("connection thread", e))?,
            );
        }
        workers.push(spawn_ticker(
            Ticker {
                relay: relay.clone(),
                store,
                base: chain_base,
                mem,
                metrics,
                tracer: tracer.clone(),
                peers: config.network.peers.clone(),
                max_peers: config.network.peer_limits().total,
            },
            stop.clone(),
        )?);
        Ok(Node {
            p2p_addr,
            rpc_addr: rpc.as_ref().map(|s| s.addr()),
            rpc_cookie,
            metrics_addr: metrics_server.as_ref().map(|s| s.addr()),
            relay,
            tip,
            producer,
            mempool,
            events: events_tx,
            done,
            _stop_tx: stop_tx,
            stop_requests,
            driver,
            stop,
            workers,
            rpc,
            metrics_server,
            tracer,
        })
    }

    /// Admits a transaction of this node into the mempool and announces it to the peers.
    pub fn submit_tx(&self, tx: Arc<RawTx>) -> Result<(), crate::mempool::Reject> {
        self.mempool.admit(tx.clone())?;
        self.relay.announce_tx(&tx);
        Ok(())
    }

    /// Admits a private transaction of this node into the mempool. The node does not show
    /// it to a peer before a block contains it.
    pub fn submit_private_tx(&self, tx: Arc<RawTx>) -> Result<(), crate::mempool::Reject> {
        self.mempool.admit_private(tx)
    }

    /// Receives the driver's result when it stops on its own (a fatal error).
    pub fn done(&self) -> &Receiver<Result<(), String>> {
        &self.done
    }

    /// Receives a message when the `stop` method of the RPC server asks the node to stop.
    /// The owner of the node then calls [`Node::shutdown`].
    pub fn stop_requested(&self) -> &Receiver<()> {
        &self.stop_requests
    }

    /// Stops the network and the servers, lets the driver flush the coins and sync the
    /// block files, and closes the trace files.
    pub fn shutdown(self) -> Result<(), NodeError> {
        self.stop_with(Event::Shutdown)
    }

    /// Fault injection for tests: stops the node as a crash would. The driver does not
    /// flush the coins, write a snapshot or sync the block files. What earlier flushes and
    /// block appends wrote stays in `data_dir`.
    pub fn abandon(self) -> Result<(), NodeError> {
        self.stop_with(Event::Abandon)
    }

    fn stop_with(self, last: Event) -> Result<(), NodeError> {
        self.stop.store(true, Ordering::Release);
        self.relay.shutdown();
        if let Some(s) = &self.rpc {
            s.shutdown();
        }
        if let Some(s) = &self.metrics_server {
            s.shutdown();
        }
        let _ = self.events.send(last);
        for w in self.workers {
            let _ = w.join();
        }
        let _ = self.driver.join();
        let driver_result = match self.done.try_recv() {
            Ok(result) => result.map_err(NodeError),
            // The driver already reported through `done()`.
            Err(_) => Ok(()),
        };
        let traces = self.tracer.close().map_err(|e| fatal("trace close", e));
        driver_result.and(traces)
    }
}

/// Logs the totals of the writer of the wallet index.
fn log_wallet_index(stats: &hayai_index::WriterStats) {
    use std::sync::atomic::Ordering::Relaxed;
    tracing::info!(
        blocks = stats.blocks.load(Relaxed),
        batches = stats.batches.load(Relaxed),
        bytes = stats.bytes.load(Relaxed),
        build_ms = stats.build_us.load(Relaxed) / 1_000,
        write_ms = stats.write_us.load(Relaxed) / 1_000,
        queue_wait_ms = stats.queue_wait_us.load(Relaxed) / 1_000,
        persist_wait_ms = stats.persist_wait_us.load(Relaxed) / 1_000,
        sync_ms = stats.sync_us.load(Relaxed) / 1_000,
        persist_stalls = stats.persist_stalls.load(Relaxed),
        "wallet index writer closed"
    );
}

/// What the node ticker reads once per second.
struct Ticker {
    relay: Arc<Relay>,
    store: Arc<PreparedStore>,
    /// The base of the chain, for the coins cache size.
    base: Arc<RwLock<Base>>,
    /// The memory backend, for its coin count.
    mem: Option<Arc<MemBacking>>,
    metrics: Arc<NodeMetrics>,
    tracer: Tracer,
    peers: Vec<SocketAddr>,
    max_peers: usize,
}

/// Once per second: metrics from the process, the relay, the store and the coins cache,
/// trace drop counts, the peer limit, and a redial of configured peers every ten seconds.
fn spawn_ticker(t: Ticker, stop: Arc<AtomicBool>) -> Result<JoinHandle<()>, NodeError> {
    let Ticker {
        relay,
        store,
        base,
        mem,
        metrics,
        tracer,
        peers,
        max_peers,
    } = t;
    thread::Builder::new()
        .name("node-ticker".into())
        .spawn(move || {
            let mut tick: u64 = 0;
            let mut process_error_logged = false;
            while !stop.load(Ordering::Acquire) {
                if tick.is_multiple_of(10) {
                    let connected: Vec<SocketAddr> = relay
                        .peers()
                        .iter()
                        .filter(|p| p.direction == Direction::Outbound)
                        .map(|p| p.addr)
                        .collect();
                    for addr in &peers {
                        if connected.contains(addr) {
                            continue;
                        }
                        if let Err(e) = relay.connect(*addr) {
                            tracing::debug!(%addr, error = %e, "peer dial failed");
                        }
                    }
                }
                let current = relay.peers();
                if current.len() > max_peers {
                    let mut inbound: Vec<_> = current
                        .iter()
                        .filter(|p| p.direction == Direction::Inbound)
                        .collect();
                    inbound.sort_by_key(|p| std::cmp::Reverse(p.id));
                    for p in inbound.into_iter().take(current.len() - max_peers) {
                        relay.disconnect(p.id);
                    }
                }
                let current = relay.peers();
                metrics.peers.set(current.len() as f64);
                metrics
                    .net_peers
                    .set(current.iter().filter(|p| p.established).count() as f64);
                metrics.record_relay(relay.metrics());
                let (received, sent) = relay.bytes();
                metrics.net_in_bytes.set(received);
                metrics.net_out_bytes.set(sent);
                metrics.mempool_transactions.set(store.len() as f64);
                metrics.mempool_bytes.set(store.cost_bytes() as f64);
                metrics.record_trace_drops(&tracer);
                match process::read() {
                    Ok(stats) => metrics.record_process(stats),
                    Err(e) if !process_error_logged => {
                        tracing::warn!(error = %e, "process statistics are not exported");
                        process_error_logged = true;
                    }
                    Err(_) => {}
                }
                {
                    let base = base.read();
                    metrics.coins_cache_entries.set(base.coins.len() as f64);
                    metrics
                        .coins_cache_bytes
                        .set(base.coins.memory_bytes() as f64);
                }
                if let Some(mem) = &mem {
                    metrics.coins_store_coins.set(mem.coin_count() as f64);
                }
                tick += 1;
                for _ in 0..10 {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        })
        .map_err(|e| fatal("ticker thread", e))
}

#[cfg(test)]
mod tests {
    use hayai_consensus::{rules_at, BlockLimits, ConsensusError, RuleSet, Upgrade};

    use super::*;

    /// The driver takes the rule set, and with it the block limits, from the height of the
    /// block. It stops at a height whose upgrade has no rule set.
    #[test]
    fn the_validation_config_takes_the_rule_set_of_the_height() {
        let keys = Arc::new(VerifyingKeys::new());
        for kind in [NetworkKind::Mainnet, NetworkKind::Testnet] {
            let params = NetParams::new(kind);
            for upgrade in Upgrade::ALL {
                let (Some(height), Some(rules)) =
                    (kind.activation_height(upgrade), RuleSet::of(upgrade))
                else {
                    continue;
                };
                for h in [height.saturating_sub(1), height] {
                    let cfg = validate_config(params, Mode::Full, h, &keys).expect("a rule set");
                    assert_eq!(Ok(&cfg.rules), rules_at(kind, h));
                    let limits = match kind.upgrade_at(h) {
                        Upgrade::Nu7 => BlockLimits::NU7,
                        _ => BlockLimits::PRE_NU7,
                    };
                    assert_eq!(cfg.rules.limits, limits);
                    assert_eq!(Ok(cfg.epoch()), params.epoch_at(h));
                }
                let cfg = validate_config(params, Mode::Full, height, &keys).expect("a rule set");
                assert_eq!(&cfg.rules, rules);
            }
            // A build without the NU7 rule set stops at the NU7 height.
            if let (Some(nu7), None) = (
                kind.activation_height(Upgrade::Nu7),
                RuleSet::of(Upgrade::Nu7),
            ) {
                let refused = validate_config(params, Mode::Full, nu7, &keys).map(|_| ());
                assert_eq!(
                    refused,
                    Err(ConsensusError::UnsupportedUpgrade {
                        upgrade: Upgrade::Nu7,
                        height: nu7,
                    })
                );
                let message = no_rules(refused.expect_err("refused")).to_string();
                assert!(message.contains("consensus rules"), "{message}");
                assert!(message.contains("Nu7"), "{message}");
            }
        }
    }

    /// The template time is the clock inside the limits of the header rules: a header with
    /// that time passes `check_contextual` when the clock is far after the tip, at the
    /// tip, and before the median-time-past.
    /// The longest `extra_coinbase_data` of the configuration fits in the coinbase input
    /// at a height of 4 bytes, and one byte more does not.
    #[test]
    fn the_longest_extra_coinbase_data_fits_in_the_coinbase_input() {
        use crate::config::MAX_EXTRA_COINBASE_DATA;

        assert_eq!(miner_data(None), b"hayai");
        assert_eq!(miner_data(Some("pool")), b"hayai: pool");
        let spec = |extra: usize| CoinbaseSpec {
            script_pubkey: vec![0x51],
            miner_data: miner_data(Some(&"x".repeat(extra))),
            network: NetworkKind::Regtest,
        };
        let height = 0x0100_0000;
        let built = spec(MAX_EXTRA_COINBASE_DATA)
            .build(height, 0)
            .expect("the longest text");
        let tag = [b"hayai: ".as_slice(), &[b'x'; MAX_EXTRA_COINBASE_DATA]].concat();
        assert!(built.bytes.windows(tag.len()).any(|w| w == tag));
        let Err(_) = spec(MAX_EXTRA_COINBASE_DATA + 1).build(height, 0) else {
            panic!("one byte more fits");
        };
    }

    #[test]
    fn the_template_time_is_inside_the_limits_of_the_header_rules() {
        use hayai_consensus::header::{check_contextual, MAX_FUTURE_BLOCK_TIME_MTP};
        use hayai_wire::header::{BlockHeader, PowParams};

        // Newest first. The median of the 11 times is 1,000,050.
        let times: Vec<u32> = (0..11).map(|i| 1_000_100 - 10 * i).collect();
        let mtp = 1_000_050;
        let limit = mtp + MAX_FUTURE_BLOCK_TIME_MTP;
        let network = NetworkKind::Regtest;
        for (now, expected) in [
            (2_000_000_000, limit),
            (limit + 1, limit),
            (limit, limit),
            (1_000_200, 1_000_200),
            (mtp, mtp + 1),
            (0, mtp + 1),
        ] {
            let time = template_time(network, 20, mtp, now);
            assert_eq!(time, expected, "clock {now}");
            let header = BlockHeader {
                version: 4,
                prev_hash: BlockHash([0; 32]),
                merkle_root: [0; 32],
                block_commitments: [0; 32],
                time,
                bits: REGTEST_POW_LIMIT_BITS,
                nonce: [0; 32],
                solution: vec![0; PowParams::REGTEST.solution_len()],
            };
            let chain = ParentChain {
                height: 20,
                times: &times,
                bits: &[],
            };
            assert_eq!(
                check_contextual(network, &header, &chain),
                Ok(hayai_consensus::HeaderVerdict::Checked),
                "clock {now}"
            );
        }
        // Below the start height of the rule the clock has no upper limit.
        assert_eq!(template_time(network, 1, mtp, 2_000_000_000), 2_000_000_000);
        let testnet = NetworkKind::Testnet;
        assert_eq!(
            template_time(testnet, 653_605, mtp, 2_000_000_000),
            2_000_000_000
        );
        assert_eq!(template_time(testnet, 653_606, mtp, 2_000_000_000), limit);
    }

    /// A full node in the checkpoint range builds no Orchard key until it is
    /// [`KEY_LEAD_BLOCKS`] below the end of the range. The first block with full validation
    /// is the block after the last checkpoint.
    #[test]
    fn no_key_is_requested_in_the_checkpoint_range() {
        let params = NetParams::new(NetworkKind::Mainnet);
        let last = NetworkKind::Mainnet
            .checkpoints()
            .last_height()
            .expect("a checkpoint list");
        assert_eq!(first_validated(params, Mode::Full, 1), last + 1);
        assert_eq!(first_validated(params, Mode::Full, last + 7), last + 7);
        assert_eq!(first_validated(params, Mode::Shadow, 1), 1);
        let keys = Arc::new(VerifyingKeys::new());
        for height in [1, last - KEY_LEAD_BLOCKS] {
            request_keys(params, Mode::Full, &keys, height).expect("a rule set");
            keys.ready();
            let branch = params.branch_at(last + 1).expect("a rule set");
            assert!(!keys.has_orchard_key(branch), "height {height}");
        }
    }

    /// Mainnet block 1 of Zebra's vectors, with one bit of the nonce changed when `broken`.
    fn stored_block_1(broken: bool) -> RawBlock {
        let name = format!(
            "{}/../hayai-bench/tests/vectors/block-main-0-000-001.hex",
            env!("CARGO_MANIFEST_DIR")
        );
        let hex = std::fs::read_to_string(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut bytes = hex::decode(hex.trim()).expect("hex");
        if broken {
            // The nonce is the 32 bytes at offset 108 of the header.
            bytes[108] ^= 1;
        }
        let params = NetParams::new(NetworkKind::Mainnet);
        RawBlock::parse(bytes.into(), params.branch_at(1).expect("a branch")).expect("a block")
    }

    /// The replay of a block store that holds `block` at height 1, on a Mainnet chain whose
    /// genesis block has the time `genesis_time`.
    fn replay_block_1(block: &RawBlock, genesis_time: u32) -> Result<usize, NodeError> {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        fs::create_dir_all(&base).expect("scratch base");
        let dir = tempfile::tempdir_in(base).expect("scratch dir");
        let params = NetParams::new(NetworkKind::Mainnet);
        let (genesis, _) = params.genesis();
        let (backing, _) =
            MemBacking::open(&dir.path().join("coins"), &MemConfig::default()).expect("coins");
        let mut base = Base::new(Arc::new(backing), 0, genesis, genesis_time);
        base.history = Some(Arc::new(HistoryState::empty(
            params.branch_at(0).expect("a branch"),
        )));
        let mut chain = Chain::new(base);
        let blocks = BlockStore::open(dir.path().join("blocks")).expect("block store");
        blocks.append(1, block).expect("append");
        let index = HeaderIndex::new(0, &SeedBlock::from_lists(&[(genesis, genesis_time)], &[]));
        replay(
            &mut chain,
            &Replay {
                index: &index,
                blocks: &blocks,
                params,
                mode: Mode::Full,
                keys: &Arc::new(VerifyingKeys::new()),
                history_roots: &HistoryRoots::default(),
                start_height: 0,
                wallet: None,
            },
        )
    }

    /// ZIP 204: when an upgrade activates, the node disconnects the peers below the protocol
    /// version of the upgrade. A Regtest chain has NU6.3 at height 3. A peer with the NU6.2
    /// version 170,150 stays connected at the tip 2 and leaves at the tip 3.
    #[test]
    fn a_peer_below_the_version_of_the_upgrade_of_the_tip_is_disconnected() {
        use std::io::Write;
        use std::net::TcpStream;

        use hayai_net::codec::{encode, read_message, LegacyMessage, NetAddr, VersionMessage};

        use crate::sync_tests::{config_with, generate, scratch, wait_for, wait_tip};

        let dir = scratch();
        let config = config_with(
            dir.path(),
            "a",
            true,
            false,
            "[regtest]\nactivation_heights = { nu6_3 = 3 }\n",
        );
        let node = Node::start(&config).expect("the node starts");
        let net = hayai_net::Network::Regtest;
        let mut stream =
            TcpStream::connect(node.p2p_addr.expect("the node listens")).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("a read timeout");
        let version = LegacyMessage::Version(VersionMessage {
            version: 170_150,
            services: 1,
            timestamp: 0,
            addr_recv: NetAddr {
                services: 0,
                addr: ([127, 0, 0, 1], 0).into(),
            },
            addr_from: NetAddr {
                services: 1,
                addr: ([0, 0, 0, 0], 0).into(),
            },
            nonce: 0x5eed,
            user_agent: "/nu6.2-peer:1/".into(),
            start_height: 0,
            relay: true,
        });
        stream.write_all(&encode(net, &version)).expect("version");
        let mut handshake = 0;
        while handshake < 2 {
            match read_message(&mut stream, net, usize::MAX).expect("a message") {
                LegacyMessage::Version(_) => {
                    stream
                        .write_all(&encode(net, &LegacyMessage::Verack))
                        .expect("verack");
                    handshake += 1;
                }
                LegacyMessage::Verack => handshake += 1,
                _ => {}
            }
        }
        let connected = |node: &Node| {
            node.relay
                .peers()
                .iter()
                .any(|p| p.established && p.version == Some(170_150))
        };
        wait_for("the handshake of the peer", || connected(&node));

        let hashes = generate(&node, 2);
        wait_tip(&node, (2, hashes[1]));
        assert!(connected(&node), "the NU6.2 version passes before NU6.3");

        let hashes = generate(&node, 1);
        wait_tip(&node, (3, hashes[0]));
        // The driver raises the minimum before it publishes the tip.
        assert!(!connected(&node), "the NU6.2 version fails from NU6.3");
        node.shutdown().expect("clean shutdown");
    }

    /// A replay checks the proof of work of each stored block. The store holds Mainnet
    /// block 1 of Zebra's vectors, which is below the last checkpoint: the replay applies it
    /// with the checkpoint path. The contextual header rules of a block above the last
    /// checkpoint run in `validate_block` (the Regtest tests of `sync_tests`).
    #[test]
    fn a_replay_applies_the_proof_of_work_to_a_stored_block() {
        let (_, genesis_time) = NetParams::new(NetworkKind::Mainnet).genesis();
        assert_eq!(
            replay_block_1(&stored_block_1(false), genesis_time).expect("block 1 replays"),
            0
        );

        // A stored header that changed on disk: the proof of work fails.
        let message = replay_block_1(&stored_block_1(true), genesis_time)
            .expect_err("a broken header")
            .to_string();
        assert!(message.contains("replay of block 1"), "{message}");
        assert!(
            message.contains("equihash") || message.contains("target"),
            "{message}"
        );
    }
}
