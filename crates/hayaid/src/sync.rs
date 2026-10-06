//! The block synchronization of a full node: the header sync, the block download and the
//! bodies that wait for the validator.
//!
//! [`SyncInbox`] is the sink of the relay (`hayai_net::SyncSink`). It runs on the reader
//! thread of a peer: it parses a `block` message with the rule set of the height of the
//! block and checks the merkle root, then it puts the message in the queue of the driver.
//!
//! [`Sync`] belongs to the driver. It owns the download scheduler
//! (`hayai_sync::download::Scheduler`) and applies its actions. It is the only sender of
//! `getheaders` and of `getdata` for a block.
//!
//! Header sync. One peer at a time gives the headers of the best chain. The node sends
//! `getheaders` with the locator of the best header chain. A `headers` message of 160
//! headers is followed by the next `getheaders`, with the last header first in the locator.
//! When the peer has no more headers, the node asks each other peer one time. Only a peer
//! with evidence of more headers than the node has takes the role: its reported height is
//! above the best header, or it sent a full `headers` message with a new header. Such a peer
//! that does not answer in [`SyncConfig::header_timeout_ms`] is disconnected, and another
//! peer takes its place. A peer without that evidence gets `getheaders` and no role: Zakura,
//! Zebra and zcashd send no `headers` message when they have no header after the locator,
//! so silence is not a stall.
//!
//! Idle poll. While no peer has the role, the node sends `getheaders` to one peer at a
//! time, in rotation. The delay starts at [`SyncConfig::header_poll_ms`] and doubles after
//! each poll, up to [`SyncConfig::header_poll_max_ms`]. News sets the delay back to the
//! start value: a new header, an `inv` with an unknown block, a new block of the relay, a
//! new peer that reports more than the best header. A poll without an answer costs the peer
//! nothing. An `inv` with an unknown block hash is followed by `getheaders` to the
//! peer of the `inv`. A hayaid peer announces a block before its own driver has the header,
//! so the answer can lack the block: the node asks again, at most
//! [`ANNOUNCE_RETRIES`] times.
//!
//! Withheld bodies. The best header chain can have blocks that no connected peer sends:
//! a peer sent the headers and does not answer the `getdata`, or the peers that have the
//! chain left. The scheduler names the lowest such block when each peer that can have it
//! failed ([`Scheduler::withheld`]). The node then takes the block and its descendants
//! out of the fork choice (`HeaderChain::mark_unavailable`) and downloads the chain with
//! the most work among the other chains, as zcashd does: zcashd activates the chain with
//! the most work among the chains whose blocks it has. The template is always on the
//! committed tip. The headers come back into the fork choice when a peer that connected
//! after the exclusion sends a header of the chain, when the relay completes a block of
//! it, and after a back-off of [`WITHHELD_RETRY_MS`] that doubles at each exclusion in a
//! row. A peer that was connected at the exclusion did not send the block: its headers of
//! the chain are the answer to the `getheaders` of the exclusion and do not end it. The
//! mark is not on disk. `hayai_sync_bodies_withheld` is 1 while a chain is out of the fork
//! choice. The log has one warning for an excluded block, then at most one for each wait.
//!
//! Bodies. The node holds the bodies of the blocks that the scheduler stored
//! ([`Action::Store`]) and the bodies that the relay completed (compact relay, a local
//! block). A body of the relay is a body from another source: the node records it in the
//! header chain (`HeaderChain::mark_body_received`) and tells the scheduler with
//! [`Event::BestHeaderTipChanged`]. A downloaded block that is the best header tip goes on
//! to the peers through `Relay::forward_block`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use crossbeam_channel::Sender;
use hayai_consensus::header::{check_contextual, check_local_time, HeaderRuleError, HeaderVerdict};
use hayai_consensus::{ConsensusError, ParentChain};
use hayai_crypto::zcash_primitives::transaction::TxId;
use hayai_net::codec::{MAX_HEADERS, MAX_LOCATOR_HASHES};
use hayai_net::{Misbehaviour, PeerId, Relay, Source, SyncEvent, SyncSink};
use hayai_sync::download::{Action, DownloadConfig, Event, Scheduler, Stats};
use hayai_sync::headers::{
    HeaderChain, HeaderContextView, HeaderError, HeaderRules, RejectReason, Status, Tip,
};
use hayai_trace::{event, Table, Tracer};
use hayai_wire::header::{BlockHash, BlockHeader};
use hayai_wire::{merkle_root, RawBlock};
use parking_lot::{Condvar, Mutex};
use serde_json::json;

use crate::headers::HeaderIndex;
use crate::metrics::NodeMetrics;
use crate::node::{fatal, Event as DriverEvent, NodeError};
use crate::params::NetParams;

/// Bodies of the relay that the node holds for blocks that are not on the best header
/// chain, at most. Above this number the node drops them and records them as missing.
const MAX_SIDE_BODIES: usize = 64;

/// Default of [`SyncConfig::header_poll_ms`]. The announcements of the peers bring the new
/// blocks, and the poll finds a block whose announcement did not arrive. 30 s is less than
/// one half of the block spacing of Mainnet (75 s) and about one block spacing of NU7
/// (25 s). Zakura starts its tip search again each 45 s.
pub const HEADER_POLL_MS: u64 = 30_000;
/// Default of [`SyncConfig::header_poll_max_ms`]: four doublings of [`HEADER_POLL_MS`],
/// 8 min. On a chain with blocks, each block sets the delay back before it gets there.
/// The largest delay is for a chain without blocks, where a poll has no result.
pub const HEADER_POLL_MAX_MS: u64 = 480_000;

/// Times that the node asks the peer of an announced block for its header again.
const ANNOUNCE_RETRIES: u32 = 5;
/// Time between two such requests.
const ANNOUNCE_RETRY_MS: u64 = 300;
/// Announced blocks that wait for their header, at most.
const MAX_ANNOUNCED: usize = 64;
/// First wait before the headers of a chain without bodies are in the fork choice again.
/// The wait doubles at each exclusion in a row, at most [`WITHHELD_MAX_DOUBLINGS`] times.
const WITHHELD_RETRY_MS: u64 = 30_000;
const WITHHELD_MAX_DOUBLINGS: u32 = 5;

/// The contextual header rules of hayai-consensus, with the clock of the node. The header
/// chain starts at the genesis block, so the context always has the blocks that a rule
/// reads.
pub struct ConsensusHeaderRules;

impl HeaderRules for ConsensusHeaderRules {
    fn check(
        &self,
        header: &BlockHeader,
        context: &HeaderContextView<'_>,
    ) -> Result<(), HeaderRuleError> {
        let chain = ParentChain {
            height: context.height,
            times: context.times,
            bits: context.bits,
        };
        match check_contextual(context.network, header, &chain)? {
            HeaderVerdict::Checked => check_local_time(header, context.now),
            HeaderVerdict::ContextTooShort(unchecked) => {
                unreachable!("a chain from the genesis block has a short context: {unchecked:?}")
            }
        }
    }
}

/// Why the body of a `block` message is not usable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BodyError {
    #[error("the header chain does not have the header of the block")]
    UnknownHeader,
    /// The height of the block has no rule set: the node stops.
    #[error(transparent)]
    NoRules(ConsensusError),
    #[error("the block does not parse: {0}")]
    Malformed(String),
    #[error("the merkle root of the header does not match the transactions")]
    MerkleRoot,
    /// The list has a transaction twice and the merkle root of the list without the second
    /// one (CVE-2012-2459): the body is not the body of the header.
    #[error("the block has the transaction {0} twice")]
    DuplicateTxid(TxId),
}

/// A message of the block synchronization, as the driver gets it.
pub enum NetEvent {
    PeerConnected {
        peer: Source,
        start_height: u32,
    },
    PeerDisconnected {
        peer: PeerId,
    },
    Headers {
        peer: Source,
        headers: Vec<BlockHeader>,
    },
    BlockInv {
        peer: Source,
        hashes: Vec<BlockHash>,
    },
    /// A `block` message whose header parses.
    Block {
        peer: Source,
        hash: BlockHash,
        bytes_len: u32,
        body: Result<Arc<RawBlock>, BodyError>,
    },
    /// A `block` message whose header does not parse.
    Garbage {
        peer: Source,
    },
    NotFound {
        peer: Source,
        hashes: Vec<BlockHash>,
    },
}

/// Messages of one peer that wait in the queue of the driver, at most. At the bound the
/// reader thread of the peer waits, so the peer cannot send faster than the driver works,
/// and the memory of the queue has a bound for each connection.
pub(crate) const MAX_QUEUED_PER_PEER: usize = 32;

/// The messages of the block synchronization that wait in the queue of the driver.
#[derive(Default)]
pub struct Backlog {
    state: Mutex<BacklogState>,
    drained: Condvar,
}

#[derive(Default)]
struct BacklogState {
    /// Queued messages of each peer.
    counts: HashMap<PeerId, usize>,
    /// Arrival time of each queued message, in queue order.
    arrivals: VecDeque<Instant>,
    /// The driver stopped.
    closed: bool,
}

/// The peer whose queue bound counts `event`. The connection events have no bound: the
/// relay sends one of each for a connection.
fn counted_peer(event: &NetEvent) -> Option<PeerId> {
    let source = match event {
        NetEvent::PeerConnected { .. } | NetEvent::PeerDisconnected { .. } => return None,
        NetEvent::Headers { peer, .. }
        | NetEvent::BlockInv { peer, .. }
        | NetEvent::Block { peer, .. }
        | NetEvent::Garbage { peer }
        | NetEvent::NotFound { peer, .. } => peer,
    };
    match source {
        Source::Peer { id, .. } => Some(*id),
        Source::Local => None,
    }
}

impl Backlog {
    /// Puts `event`, which arrived at `at`, in the queue `events`. With `wait`, the call
    /// returns when the peer of the event has room in the queue. Without it, an event of a
    /// peer at its bound is dropped: the relay sends a block announcement also from its
    /// ticker thread, which must not wait, and the header sync finds the block again.
    pub(crate) fn send(
        &self,
        events: &Sender<DriverEvent>,
        event: NetEvent,
        at: Instant,
        wait: bool,
    ) {
        let mut state = self.state.lock();
        if let Some(peer) = counted_peer(&event) {
            loop {
                if state.closed {
                    return;
                }
                if state.counts.get(&peer).copied().unwrap_or(0) < MAX_QUEUED_PER_PEER {
                    break;
                }
                if !wait {
                    tracing::debug!(%peer, "queue of the driver full; block announcement dropped");
                    return;
                }
                self.drained.wait(&mut state);
            }
            *state.counts.entry(peer).or_default() += 1;
        }
        // The queue order is the order of `arrivals`: the send is under the lock.
        let Ok(()) = events.send(DriverEvent::Net { event, at }) else {
            tracing::debug!("driver stopped; synchronization message dropped");
            state.closed = true;
            self.drained.notify_all();
            return;
        };
        state.arrivals.push_back(at);
    }

    /// The driver took `event` from its queue.
    pub(crate) fn received(&self, event: &NetEvent) {
        let mut state = self.state.lock();
        state.arrivals.pop_front();
        let Some(peer) = counted_peer(event) else {
            return;
        };
        if let Some(count) = state.counts.get_mut(&peer) {
            *count -= 1;
            if *count == 0 {
                state.counts.remove(&peer);
            }
        }
        self.drained.notify_all();
    }

    /// The time before which the driver handled every message: the arrival time of the
    /// oldest queued message, or the current time when the queue is empty. The clock of
    /// the stall rules must not pass it: a body that waits in the queue is not late.
    pub(crate) fn horizon(&self) -> Instant {
        let state = self.state.lock();
        state.arrivals.front().copied().unwrap_or_else(Instant::now)
    }

    /// The driver stopped: no reader thread waits for room.
    fn close(&self) {
        self.state.lock().closed = true;
        self.drained.notify_all();
    }
}

/// The sink of the relay: parses each block on the reader thread of its peer and gives
/// every message to the driver with its arrival time.
pub struct SyncInbox {
    pub events: Sender<DriverEvent>,
    pub headers: Arc<Mutex<HeaderChain>>,
    pub params: NetParams,
    pub backlog: Arc<Backlog>,
}

impl SyncInbox {
    fn block(&self, peer: Source, bytes: Bytes) -> NetEvent {
        let Ok(header) = BlockHeader::parse(&bytes) else {
            return NetEvent::Garbage { peer };
        };
        let hash = header.hash();
        let bytes_len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let height = self.headers.lock().entry(&hash).map(|entry| entry.height);
        let body = match height {
            None => Err(BodyError::UnknownHeader),
            Some(height) => parse_body(self.params, height, bytes),
        };
        NetEvent::Block {
            peer,
            hash,
            bytes_len,
            body,
        }
    }
}

/// The transaction id that makes `txids` a merkle mutation of the list of the header
/// (CVE-2012-2459): an id occurs twice, and the list without the later occurrences has the
/// merkle root `root` too. A peer can make such a body from the body of a valid block, so
/// it is a wrong body. `None` for a list without a repeated id, and for a list with a
/// repeated id whose root no other list gives: the header then commits to the repeated
/// id, and the block is not valid.
pub fn merkle_mutation(txids: &[TxId], root: &[u8; 32]) -> Option<TxId> {
    let twice = hayai_wire::duplicate_txid(txids)?;
    let mut seen = HashSet::with_capacity(txids.len());
    let first: Vec<TxId> = txids
        .iter()
        .copied()
        .filter(|txid| seen.insert(*txid))
        .collect();
    (merkle_root(&first) == *root).then_some(twice)
}

/// Parses the block at `height` with the rule set of that height and checks its merkle
/// root.
pub fn parse_body(
    params: NetParams,
    height: u32,
    bytes: Bytes,
) -> Result<Arc<RawBlock>, BodyError> {
    let branch = params.branch_at(height).map_err(BodyError::NoRules)?;
    let raw = RawBlock::parse(bytes, branch).map_err(|e| BodyError::Malformed(e.to_string()))?;
    let txids = raw.txids();
    if merkle_root(&txids) != raw.header.merkle_root {
        return Err(BodyError::MerkleRoot);
    }
    if let Some(twice) = merkle_mutation(&txids, &raw.header.merkle_root) {
        return Err(BodyError::DuplicateTxid(twice));
    }
    Ok(Arc::new(raw))
}

impl SyncSink for SyncInbox {
    fn on_sync(&self, event: SyncEvent) {
        let at = Instant::now();
        // The messages of the reader thread of a peer wait for room in the queue.
        let (event, wait) = match event {
            SyncEvent::PeerConnected { peer, start_height } => {
                (NetEvent::PeerConnected { peer, start_height }, false)
            }
            SyncEvent::PeerDisconnected { peer } => (NetEvent::PeerDisconnected { peer }, false),
            SyncEvent::Headers { peer, headers } => (NetEvent::Headers { peer, headers }, true),
            SyncEvent::BlockInv { peer, hashes } => (NetEvent::BlockInv { peer, hashes }, false),
            SyncEvent::Block { peer, bytes } => (self.block(peer, bytes), true),
            SyncEvent::NotFound { peer, hashes } => (NetEvent::NotFound { peer, hashes }, true),
        };
        self.backlog.send(&self.events, event, at, wait);
    }
}

/// Configuration of [`Sync`].
#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub download: DownloadConfig,
    /// Time without a `headers` message after which the node disconnects the peer of the
    /// header sync.
    pub header_timeout_ms: u64,
    /// First delay of the idle poll of the header sync.
    pub header_poll_ms: u64,
    /// Largest delay of the idle poll of the header sync.
    pub header_poll_max_ms: u64,
}

/// A body that waits for the validator.
pub struct Body {
    pub block: Arc<RawBlock>,
    /// The peer that sent the block, or the local source.
    pub supplier: Source,
    /// The reception of the block: the arrival of the `block` message of the download
    /// before its parse, or the arrival of a block of the relay at the driver queue.
    pub received: Instant,
    /// The relay completed the body. The scheduler did not request it.
    pub relayed: bool,
    /// The block was the best header tip when its body arrived: it goes on to the peers
    /// when the driver checked the body.
    forward: bool,
}

/// A `block` message of the download.
struct Arrived {
    hash: BlockHash,
    supplier: Source,
    /// The arrival of the message, before its parse.
    received: Instant,
    body: Result<Arc<RawBlock>, BodyError>,
}

/// A block that the scheduler gave to the validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivered {
    pub block: Tip,
}

/// Why the validator refused a delivered block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The block breaks a consensus rule. The header chain records it as invalid.
    Invalid,
    /// The body is not the body that the header commits to. Another peer can have the
    /// body of the header, so the header stays valid.
    WrongBody,
}

/// The state of the chains without bodies.
#[derive(Clone, Copy)]
struct Withheld {
    /// A chain is out of the fork choice.
    excluded: bool,
    /// The time of the next step: the end of the exclusion, or, after it, the time at
    /// which the node forgets the exclusions in a row.
    until_ms: u64,
    /// Exclusions in a row.
    rounds: u32,
    /// The block of the last exclusion.
    block: BlockHash,
    /// No warning for an exclusion of `block` before this time.
    warn_at_ms: u64,
}

struct SyncPeer {
    source: Source,
    start_height: u32,
    /// The last header of the newest `headers` message of the peer that the header chain
    /// holds: a block of the chain of the peer.
    tip: Option<BlockHash>,
    /// The node sent `getheaders` to the peer since the last change of the header sync
    /// peer.
    asked: bool,
    /// The peer was connected at an exclusion of a chain without bodies: it did not send
    /// the block, so its headers of that chain do not end the exclusion.
    withheld: bool,
}

/// The peer of the header sync and its progress in the current window. The peer must add
/// [`MAX_HEADERS`] headers to the header chain in each window of
/// [`SyncConfig::header_timeout_ms`]: an answer with headers that the node has is no
/// progress.
struct HeaderPeer {
    id: PeerId,
    /// The peer sent a full `headers` message with a new header: it has more headers.
    more: bool,
    /// Start of the window.
    since_ms: u64,
    /// Headers that the peer added in the window.
    added: usize,
}

/// An announced block whose header the node does not have.
struct Announced {
    peer: PeerId,
    retry_at_ms: u64,
    retries: u32,
}

/// The values of a `sync_progress` trace row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Progress {
    headers_height: u32,
    blocks_height: u32,
    peers: usize,
    in_flight: u32,
    held_bytes: u64,
}

/// The parts of the node that [`Sync`] uses.
pub struct SyncParts {
    pub headers: Arc<Mutex<HeaderChain>>,
    pub index: Arc<HeaderIndex>,
    pub relay: Arc<Relay>,
    pub backlog: Arc<Backlog>,
    pub metrics: Arc<NodeMetrics>,
    pub tracer: Tracer,
}

/// The block synchronization of the driver. See the module documentation.
pub struct Sync {
    params: NetParams,
    headers: Arc<Mutex<HeaderChain>>,
    index: Arc<HeaderIndex>,
    relay: Arc<Relay>,
    backlog: Arc<Backlog>,
    scheduler: Scheduler<PeerId>,
    lookahead: usize,
    peers: HashMap<PeerId, SyncPeer>,
    bodies: HashMap<BlockHash, Body>,
    delivered: VecDeque<Delivered>,
    header_peer: Option<HeaderPeer>,
    header_timeout_ms: u64,
    poll_base_ms: u64,
    poll_max_ms: u64,
    /// The delay before the last idle poll, or the start value after news.
    poll_delay_ms: u64,
    /// The time of the next idle poll.
    poll_at_ms: u64,
    /// The peer of the last idle poll.
    poll_last: Option<PeerId>,
    withheld: Option<Withheld>,
    announced: HashMap<BlockHash, Announced>,
    started: Instant,
    now_ms: u64,
    progress: Option<Progress>,
    progress_at_ms: u64,
    metrics: Arc<NodeMetrics>,
    tracer: Tracer,
}

/// The largest height that a chain of `params` can have at the time `now`: one block for
/// each second since the genesis block is far above each target spacing and each
/// difficulty bound. A peer that reports more gets this value, so that a false height
/// does not give it the first place among the peers.
fn plausible_height(params: NetParams, now: u32, reported: u32) -> u32 {
    reported.min(now.saturating_sub(params.genesis().1))
}

fn now_secs() -> u32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    u32::try_from(secs).unwrap_or(u32::MAX)
}

/// The newest block of the committed chain of `index` that is on the best chain of
/// `chain`.
fn committed_on_best(index: &HeaderIndex, chain: &HeaderChain) -> Result<Tip, NodeError> {
    let (mut height, mut hash) = index.tip();
    loop {
        if matches!(chain.entry(&hash), Some(entry) if entry.on_best_chain) {
            return Ok(Tip { height, hash });
        }
        let below = height.checked_sub(1).and_then(|h| index.ancestors_at(h, 1));
        let Some([(parent, _)]) = below.as_deref() else {
            return Err(NodeError(format!(
                "the best header chain leaves the committed chain below height {height}, \
                 below the window of the node"
            )));
        };
        height -= 1;
        hash = *parent;
    }
}

impl Sync {
    pub fn new(params: NetParams, config: SyncConfig, parts: SyncParts) -> Result<Self, NodeError> {
        let SyncParts {
            headers,
            index,
            relay,
            backlog,
            metrics,
            tracer,
        } = parts;
        let lookahead = config.download.validation_lookahead as usize;
        let scheduler = {
            let chain = headers.lock();
            let committed = committed_on_best(&index, &chain)?;
            Scheduler::new(config.download, &chain, committed)
                .map_err(|e| fatal("block download", e))?
        };
        Ok(Self {
            params,
            headers,
            index,
            relay,
            backlog,
            scheduler,
            lookahead,
            peers: HashMap::new(),
            bodies: HashMap::new(),
            delivered: VecDeque::new(),
            header_peer: None,
            header_timeout_ms: config.header_timeout_ms,
            poll_base_ms: config.header_poll_ms,
            poll_max_ms: config.header_poll_max_ms,
            poll_delay_ms: config.header_poll_ms,
            poll_at_ms: config.header_poll_ms,
            poll_last: None,
            withheld: None,
            announced: HashMap::new(),
            started: Instant::now(),
            now_ms: 0,
            progress: None,
            progress_at_ms: 0,
            metrics,
            tracer,
        })
    }

    /// Moves the clock of the scheduler to `at`. The clock never goes back.
    fn clock(&mut self, at: Instant) {
        let ms = at.saturating_duration_since(self.started).as_millis();
        self.now_ms = self.now_ms.max(u64::try_from(ms).unwrap_or(u64::MAX));
    }

    /// Moves the clock to the time before which the driver handled every message
    /// ([`Backlog::horizon`]). Each step that is not a message of a peer starts with this
    /// call: a request then has the time of its send, also after a long validation.
    fn settle_clock(&mut self) {
        self.clock(self.backlog.horizon());
    }

    /// Gives `event` to the scheduler and applies its actions.
    fn step(&mut self, event: Event<PeerId>) -> Result<(), NodeError> {
        self.step_with(event, None)
    }

    /// [`Sync::step`] for a `block` message: `arrived` is its body, which the node keeps
    /// only for [`Action::Store`].
    fn step_with(
        &mut self,
        event: Event<PeerId>,
        mut arrived: Option<Arrived>,
    ) -> Result<(), NodeError> {
        let (actions, best, block_height) = {
            let chain = self.headers.lock();
            let actions = self
                .scheduler
                .handle(event, self.now_ms, &chain)
                .map_err(|e| fatal("block download", e))?;
            let height = arrived
                .as_ref()
                .and_then(|arrived| chain.entry(&arrived.hash))
                .map(|entry| entry.height);
            (actions, chain.best_tip(), height)
        };
        let mut refused = None;
        for action in actions {
            match action {
                Action::Request { peer, hashes } => {
                    // A peer that left gives `PeerDisconnected`, and the scheduler moves
                    // the request.
                    self.relay.request_blocks(peer, &hashes);
                }
                Action::Store { hash } => {
                    let Some(Arrived {
                        hash: arrived_hash,
                        supplier,
                        received,
                        body,
                    }) = arrived.take()
                    else {
                        return Err(NodeError(format!(
                            "the block download stores {hash} and no block arrived"
                        )));
                    };
                    if arrived_hash != hash {
                        return Err(NodeError(format!(
                            "the block download stores {hash} and the block {arrived_hash} arrived"
                        )));
                    }
                    match body {
                        Ok(block) => {
                            self.tracer
                                .emit(Table::BlockSync, event::BLOCK_RECEIVED, || {
                                    let peer = match supplier {
                                        Source::Local => None,
                                        Source::Peer { id, .. } => Some(id.0),
                                    };
                                    json!({
                                        "peer": peer,
                                        "height": block_height,
                                        "hash": hash.to_string(),
                                        "bytes": block.bytes.len(),
                                        "source": "download",
                                        "received_unix_us": crate::node::received_unix_micros(received),
                                    })
                                });
                            self.metrics.sync_downloaded_blocks.inc();
                            // A new block at the tip goes on to the peers before its
                            // validation, after the driver checked that the body is the
                            // body of the header ([`Sync::body_checked`]).
                            self.bodies.insert(
                                hash,
                                Body {
                                    block,
                                    supplier,
                                    received,
                                    relayed: false,
                                    forward: best.hash == hash,
                                },
                            );
                        }
                        Err(BodyError::NoRules(e)) => return Err(fatal("consensus rules", e)),
                        Err(e) => {
                            tracing::debug!(%hash, ?supplier, error = %e, "block body refused");
                            self.penalize_wrong_body(supplier);
                            refused = Some(hash);
                        }
                    }
                }
                Action::Deliver { block, .. } => self.delivered.push_back(Delivered { block }),
                Action::Discard { hash } => {
                    self.bodies.remove(&hash);
                    self.cut_delivered(&hash);
                    // The block can be in the header chain no more, or invalid.
                    let mut chain = self.headers.lock();
                    if let Some(Status::BodyKnown) = chain.entry(&hash).map(|e| e.status) {
                        chain
                            .mark_body_missing(&hash)
                            .map_err(|e| fatal("header chain", e))?;
                    }
                }
                Action::Penalize { peer, reason } => {
                    if let Some(entry) = self.peers.get(&peer) {
                        self.relay.misbehaved(entry.source, reason);
                    }
                }
                Action::Disconnect { peer } => self.relay.disconnect(peer),
            }
        }
        match refused {
            // The scheduler gives the penalty and asks another peer.
            Some(hash) => {
                self.cut_delivered(&hash);
                self.step(Event::BlockInvalid { hash })
            }
            None => Ok(()),
        }
    }

    /// Removes `hash` and the blocks after it from the blocks that wait for the validator.
    fn cut_delivered(&mut self, hash: &BlockHash) {
        if let Some(at) = self.delivered.iter().position(|d| d.block.hash == *hash) {
            self.delivered.truncate(at);
        }
    }

    /// Tells the scheduler where the chain of each peer leaves the best header chain. A
    /// peer on another branch does not have the blocks of the best chain above the fork.
    fn update_fork_points(&mut self) -> Result<(), NodeError> {
        let points: Vec<(PeerId, Option<u32>)> = {
            let chain = self.headers.lock();
            self.peers
                .iter()
                .filter_map(|(id, peer)| {
                    let ancestor = chain.best_chain_ancestor(&peer.tip?)?;
                    let on_best = ancestor.hash == peer.tip?;
                    Some((*id, (!on_best).then_some(ancestor.height)))
                })
                .collect()
        };
        for (peer, height) in points {
            self.step(Event::PeerForkPoint { peer, height })?;
        }
        Ok(())
    }

    /// The time of the wait after the exclusion number `rounds` in a row.
    fn withheld_wait_ms(rounds: u32) -> u64 {
        WITHHELD_RETRY_MS << rounds.min(WITHHELD_MAX_DOUBLINGS)
    }

    /// Takes the block that no peer sends, and its descendants, out of the fork choice.
    fn exclude_withheld(&mut self) -> Result<(), NodeError> {
        let Some(block) = self.scheduler.withheld() else {
            return Ok(());
        };
        let change = self
            .headers
            .lock()
            .mark_unavailable(&block.hash)
            .map_err(|e| fatal("header chain", e))?;
        let rounds = self.withheld.map_or(0, |withheld| withheld.rounds);
        let wait_ms = Self::withheld_wait_ms(rounds);
        // One warning for a block, then at most one for each wait.
        let warn_at_ms = match self.withheld {
            Some(last) if last.block == block.hash && self.now_ms < last.warn_at_ms => {
                tracing::debug!(height = block.height, hash = %block.hash, "no peer sends the block again");
                last.warn_at_ms
            }
            _ => {
                tracing::warn!(
                    height = block.height,
                    hash = %block.hash,
                    new_best = ?change.map(|change| change.new),
                    peers = self.peers.len(),
                    wait_ms,
                    "no peer sends the block: its header chain is out of the fork choice"
                );
                self.now_ms + wait_ms
            }
        };
        self.withheld = Some(Withheld {
            excluded: true,
            until_ms: self.now_ms + wait_ms,
            rounds: rounds + 1,
            block: block.hash,
            warn_at_ms,
        });
        for peer in self.peers.values_mut() {
            peer.withheld = true;
        }
        self.metrics.sync_withheld_chains.inc();
        self.metrics.sync_bodies_withheld.set(1.0);
        self.best_chain_changed()?;
        // Each peer gets the locator of the new best chain.
        self.restart_header_sync();
        Ok(())
    }

    /// Puts the excluded headers into the fork choice again.
    fn include_withheld(&mut self) -> Result<(), NodeError> {
        let Some(withheld) = &mut self.withheld else {
            return Ok(());
        };
        if !withheld.excluded {
            return Ok(());
        }
        withheld.excluded = false;
        let waited = self.now_ms >= withheld.until_ms;
        withheld.until_ms = self.now_ms + Self::withheld_wait_ms(withheld.rounds);
        self.metrics.sync_bodies_withheld.set(0.0);
        if self.headers.lock().clear_unavailable() {
            // The end of a wait is at most one line for each wait. A new peer or a block
            // of the relay can end the exclusion more often.
            match waited {
                true => {
                    tracing::info!("the header chains without bodies are in the fork choice again")
                }
                false => {
                    tracing::debug!("the header chains without bodies are in the fork choice again")
                }
            }
            self.best_chain_changed()?;
        }
        Ok(())
    }

    /// The steps of the withheld-body rule at a tick.
    fn check_withheld(&mut self) -> Result<(), NodeError> {
        self.exclude_withheld()?;
        match self.withheld {
            Some(withheld) if self.now_ms < withheld.until_ms => Ok(()),
            Some(Withheld { excluded: true, .. }) => self.include_withheld(),
            // No exclusion since the last retry: the next one starts the back-off again.
            Some(Withheld {
                excluded: false, ..
            }) => {
                self.withheld = None;
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Tells the scheduler that the best header chain or a body state changed.
    fn best_chain_changed(&mut self) -> Result<(), NodeError> {
        self.update_fork_points()?;
        let committed = {
            let chain = self.headers.lock();
            let committed = committed_on_best(&self.index, &chain)?;
            // The same rule as the scheduler: the delivered blocks that are still the
            // blocks of the best chain after the same committed block stay delivered.
            let keep = match self.scheduler.stats().committed == committed {
                true => {
                    let mut best = chain.best_chain_from(committed.height + 1);
                    self.delivered
                        .iter()
                        .take_while(|d| best.next() == Some(d.block))
                        .count()
                }
                false => 0,
            };
            self.delivered.truncate(keep);
            committed
        };
        self.step(Event::BestHeaderTipChanged { committed })
    }

    /// Sends `getheaders` to the peer `id`: the locator of the best header chain, after
    /// `last` when the peer continues a branch.
    fn ask_headers(&mut self, id: PeerId, last: Option<BlockHash>) {
        let mut locator: Vec<BlockHash> = last.into_iter().collect();
        locator.extend(self.headers.lock().locator());
        locator.truncate(MAX_LOCATOR_HASHES);
        if let Some(peer) = self.peers.get_mut(&id) {
            peer.asked = true;
        }
        self.relay.send_getheaders(id, locator);
    }

    /// Without a peer of the header sync: the peer with the largest reported height that
    /// the node did not ask becomes that peer when it reports more than the best header.
    /// When no such peer reports more, each of them gets one `getheaders` and no role:
    /// such a peer can have no answer.
    fn start_header_sync(&mut self) {
        let None = self.header_peer else {
            return;
        };
        let best = self.header_height();
        let next = self
            .peers
            .iter()
            .filter(|(_, peer)| !peer.asked)
            .max_by_key(|(id, peer)| (peer.start_height, std::cmp::Reverse(**id)))
            .map(|(id, peer)| (*id, peer.start_height));
        match next {
            Some((id, height)) if height > best => {
                self.header_peer = Some(HeaderPeer {
                    id,
                    more: false,
                    since_ms: self.now_ms,
                    added: 0,
                });
                self.news();
                self.ask_headers(id, None);
            }
            Some(_) => self.finish_header_sync(),
            None => {}
        }
    }

    /// The node learned of a block that it did not have: the idle poll starts again at
    /// its first delay.
    fn news(&mut self) {
        self.poll_delay_ms = self.poll_base_ms;
        self.poll_at_ms = self.now_ms + self.poll_base_ms;
    }

    /// The idle poll: without a peer of the header sync, the next peer in rotation gets
    /// one `getheaders` when the delay passed, and the delay doubles.
    fn poll_headers(&mut self) {
        let None = self.header_peer else {
            // No poll follows the end of an exchange at once.
            self.poll_at_ms = self.poll_at_ms.max(self.now_ms + self.poll_delay_ms);
            return;
        };
        if self.now_ms < self.poll_at_ms {
            return;
        }
        let after = self.poll_last;
        let next = self
            .peers
            .keys()
            .filter(|id| Some(**id) > after)
            .min()
            .or_else(|| self.peers.keys().min())
            .copied();
        let Some(id) = next else {
            return;
        };
        self.poll_last = Some(id);
        self.poll_delay_ms = self.poll_delay_ms.saturating_mul(2).min(self.poll_max_ms);
        self.poll_at_ms = self.now_ms + self.poll_delay_ms;
        self.ask_headers(id, None);
    }

    /// The peer of the header sync has no more headers: each peer that the node did not
    /// ask gets one `getheaders`.
    fn finish_header_sync(&mut self) {
        self.header_peer = None;
        let rest: Vec<PeerId> = self
            .peers
            .iter()
            .filter(|(_, peer)| !peer.asked)
            .map(|(id, _)| *id)
            .collect();
        if !rest.is_empty() {
            // The next poll does not follow these requests at once.
            self.poll_at_ms = self.poll_at_ms.max(self.now_ms + self.poll_delay_ms);
        }
        for id in rest {
            self.ask_headers(id, None);
        }
    }

    /// The peer of the header sync left or does not answer: every peer can be asked again.
    fn restart_header_sync(&mut self) {
        self.header_peer = None;
        for peer in self.peers.values_mut() {
            peer.asked = false;
        }
        self.start_header_sync();
    }

    fn is_header_peer(&self, id: PeerId) -> bool {
        matches!(&self.header_peer, Some(peer) if peer.id == id)
    }

    /// Handles one message of the relay. `at` is its arrival time.
    pub fn on_net(&mut self, event: NetEvent, at: Instant) -> Result<(), NodeError> {
        self.backlog.received(&event);
        self.clock(at);
        match event {
            NetEvent::PeerConnected { peer, start_height } => {
                let Source::Peer { id, .. } = peer else {
                    return Ok(());
                };
                // The relay can report the end of a connection before its start, from
                // another thread. A peer that left is not a peer of the synchronization.
                if !self.relay.peers().iter().any(|p| p.id == id) {
                    return Ok(());
                }
                let start_height = plausible_height(self.params, now_secs(), start_height);
                self.peers.insert(
                    id,
                    SyncPeer {
                        source: peer,
                        start_height,
                        tip: None,
                        asked: false,
                        withheld: false,
                    },
                );
                self.step(Event::PeerConnected {
                    peer: id,
                    best_height_hint: start_height,
                })?;
                self.start_header_sync();
                Ok(())
            }
            NetEvent::PeerDisconnected { peer } => {
                let Some(_) = self.peers.remove(&peer) else {
                    return Ok(());
                };
                self.step(Event::PeerDisconnected { peer })?;
                if self.is_header_peer(peer) {
                    self.restart_header_sync();
                }
                Ok(())
            }
            NetEvent::Headers { peer, headers } => self.on_headers(peer, headers),
            NetEvent::BlockInv { peer, hashes } => {
                let Source::Peer { id, .. } = peer else {
                    return Ok(());
                };
                if !self.peers.contains_key(&id) {
                    return Ok(());
                }
                let unknown: Vec<BlockHash> = {
                    let chain = self.headers.lock();
                    hashes
                        .into_iter()
                        .filter(|hash| !matches!(chain.entry(hash), Some(_entry)))
                        .collect()
                };
                if unknown.is_empty() {
                    return Ok(());
                }
                self.news();
                for hash in unknown {
                    if self.announced.len() < MAX_ANNOUNCED {
                        self.announced.insert(
                            hash,
                            Announced {
                                peer: id,
                                retry_at_ms: self.now_ms + ANNOUNCE_RETRY_MS,
                                retries: ANNOUNCE_RETRIES,
                            },
                        );
                    }
                }
                self.ask_headers(id, None);
                Ok(())
            }
            NetEvent::Block {
                peer,
                hash,
                bytes_len,
                body,
            } => {
                let Source::Peer { id, .. } = peer else {
                    return Ok(());
                };
                self.step_with(
                    Event::BlockReceived {
                        peer: id,
                        hash,
                        bytes_len,
                    },
                    Some(Arrived {
                        hash,
                        supplier: peer,
                        received: at,
                        body,
                    }),
                )
            }
            NetEvent::Garbage { peer } => {
                self.relay.misbehaved(peer, Misbehaviour::Malformed);
                Ok(())
            }
            NetEvent::NotFound { peer, hashes } => {
                let Source::Peer { id, .. } = peer else {
                    return Ok(());
                };
                self.step(Event::NotFound { peer: id, hashes })
            }
        }
    }

    fn on_headers(&mut self, peer: Source, headers: Vec<BlockHeader>) -> Result<(), NodeError> {
        let Source::Peer { id, .. } = peer else {
            return Ok(());
        };
        if !self.peers.contains_key(&id) {
            return Ok(());
        }
        let Some(last) = headers.last().map(BlockHeader::hash) else {
            if self.is_header_peer(id) {
                self.finish_header_sync();
            }
            return Ok(());
        };
        let (result, last_entry) = {
            let mut chain = self.headers.lock();
            let result = chain.accept_headers(&headers, &ConsensusHeaderRules, now_secs());
            (result, chain.entry(&last))
        };
        let last_height = last_entry.map(|entry| entry.height);
        let accepted = match result {
            Ok(accepted) => {
                // The peer has the blocks of the headers that it sends.
                if let Some(height) = last_height {
                    if let Some(entry) = self.peers.get_mut(&id) {
                        entry.tip = Some(last);
                    }
                    self.step(Event::PeerConnected {
                        peer: id,
                        best_height_hint: height,
                    })?;
                    self.update_fork_points()?;
                }
                // A peer that connected after the exclusion sends a header of a chain that
                // is out of the fork choice: it can have the blocks. A peer that was
                // connected at the exclusion sends such headers as the answer to the
                // `getheaders` of the exclusion.
                let failed = matches!(self.peers.get(&id), Some(peer) if peer.withheld);
                if matches!(last_entry, Some(entry) if entry.unavailable) && !failed {
                    self.include_withheld()?;
                }
                // More headers follow a full message that added a header. A full message
                // of headers that the node has is no reason to ask the peer again.
                let more = headers.len() == MAX_HEADERS && accepted.added > 0;
                let is_header_peer = self.is_header_peer(id);
                match (more, &mut self.header_peer) {
                    // Another peer of the header sync has priority.
                    (true, None) => {
                        self.header_peer = Some(HeaderPeer {
                            id,
                            more: true,
                            since_ms: self.now_ms,
                            added: accepted.added,
                        });
                        self.ask_headers(id, Some(last));
                    }
                    (true, Some(current)) if is_header_peer => {
                        current.more = true;
                        current.added += accepted.added;
                        self.ask_headers(id, Some(last));
                    }
                    (true, Some(_)) => {
                        if let Some(entry) = self.peers.get_mut(&id) {
                            entry.asked = false;
                        }
                    }
                    (false, _) if is_header_peer => self.finish_header_sync(),
                    (false, _) => {}
                }
                accepted
            }
            Err(error) => {
                let HeaderError {
                    hash,
                    reason,
                    accepted,
                    ..
                } = *error;
                tracing::debug!(%id, %hash, %reason, "header refused");
                match reason {
                    RejectReason::Store(e) => return Err(fatal("header log", e)),
                    // A fault that every node sees in the header alone, or against the
                    // checkpoint list. The time rules depend on the local clock or on the
                    // branch, and the `nBits` rule on the branch: no penalty (Zakura gives
                    // 0 points for them).
                    RejectReason::Rule(
                        HeaderRuleError::Version(_)
                        | HeaderRuleError::SolutionLength { .. }
                        | HeaderRuleError::Pow(_)
                        | HeaderRuleError::Equihash(_)
                        | HeaderRuleError::WorkOverflow,
                    )
                    | RejectReason::CheckpointMismatch { .. } => {
                        self.relay.misbehaved(peer, Misbehaviour::InvalidHeader);
                    }
                    RejectReason::Rule(_) => {}
                    RejectReason::Unconnected(_) => {
                        self.relay
                            .misbehaved(peer, Misbehaviour::UnconnectedHeaders);
                        if self.peers.contains_key(&id) {
                            self.ask_headers(id, None);
                        }
                    }
                    // These depend on the blocks that this node holds: no penalty.
                    RejectReason::InvalidParent(_)
                    | RejectReason::KnownInvalid
                    | RejectReason::ForkBelowFinalized { .. }
                    | RejectReason::SideHeaderLimit => {}
                }
                if self.is_header_peer(id) {
                    self.restart_header_sync();
                }
                accepted
            }
        };
        if accepted.added > 0 {
            self.news();
            self.best_chain_changed()?;
        }
        Ok(())
    }

    /// The time passed: the stall rules of the scheduler and of the header sync, then the
    /// metrics and the `sync_progress` row. The time of the rules is the arrival time of
    /// the oldest message in the queue of the driver, so the tick runs also while the
    /// queue has messages.
    pub fn tick(&mut self) -> Result<(), NodeError> {
        self.settle_clock();
        self.step(Event::Tick)?;
        // The end of a window: the peer continues when it added the minimum.
        let now = self.now_ms;
        let slow = match &mut self.header_peer {
            Some(peer) if now.saturating_sub(peer.since_ms) >= self.header_timeout_ms => {
                let slow = peer.added < MAX_HEADERS;
                peer.since_ms = now;
                peer.added = 0;
                slow.then_some((peer.id, peer.more))
            }
            _ => None,
        };
        if let Some((id, more)) = slow {
            // A stall needs evidence that the peer has more headers than the node. The
            // node can get the headers that the peer reported from another source in the
            // window: the peer then has no answer, and it leaves the role without a penalty.
            let best = self.header_height();
            let claims_more =
                matches!(self.peers.get(&id), Some(p) if more || p.start_height > best);
            match claims_more {
                true => {
                    tracing::info!(
                        %id,
                        "the peer of the header sync does not answer; disconnecting"
                    );
                    if let Some(peer) = self.peers.get(&id) {
                        self.relay.misbehaved(peer.source, Misbehaviour::Stall);
                    }
                    // `PeerDisconnected` selects the next peer.
                    self.relay.disconnect(id);
                }
                false => self.finish_header_sync(),
            }
        }
        self.poll_headers();
        self.retry_announced();
        self.check_withheld()?;
        self.report();
        Ok(())
    }

    /// Asks again for the announced blocks whose header did not arrive.
    fn retry_announced(&mut self) {
        let now = self.now_ms;
        let mut ask = Vec::new();
        {
            let chain = self.headers.lock();
            let peers = &self.peers;
            self.announced.retain(|hash, entry| {
                if matches!(chain.entry(hash), Some(_entry)) || !peers.contains_key(&entry.peer) {
                    return false;
                }
                if now < entry.retry_at_ms {
                    return true;
                }
                if !ask.contains(&entry.peer) {
                    ask.push(entry.peer);
                }
                entry.retry_at_ms = now + ANNOUNCE_RETRY_MS;
                entry.retries -= 1;
                entry.retries > 0
            });
        }
        for peer in ask {
            self.ask_headers(peer, None);
        }
    }

    fn report(&mut self) {
        let stats = self.scheduler.stats();
        let progress = Progress {
            headers_height: self.headers.lock().best_tip().height,
            blocks_height: self.index.tip().0,
            peers: stats.peers,
            in_flight: stats.requested_blocks,
            held_bytes: stats.held_bytes,
        };
        self.metrics
            .sync_header_height
            .set(f64::from(progress.headers_height));
        self.metrics.sync_peers.set(progress.peers as f64);
        self.metrics
            .sync_requests_in_flight
            .set(f64::from(progress.in_flight));
        self.metrics
            .sync_downloads_in_flight
            .set(f64::from(progress.in_flight) + self.bodies.len() as f64);
        self.metrics.sync_held_bytes.set(progress.held_bytes as f64);
        // One row for each second in which a value changed.
        let recent = match self.progress {
            Some(last) if last == progress => return,
            Some(_) => self.now_ms < self.progress_at_ms + 1_000,
            None => false,
        };
        if recent {
            return;
        }
        self.progress = Some(progress);
        self.progress_at_ms = self.now_ms;
        self.tracer
            .emit(Table::BlockSync, event::SYNC_PROGRESS, || {
                json!({
                    "headers_height": progress.headers_height,
                    "blocks_height": progress.blocks_height,
                    "peers": progress.peers,
                    "in_flight": progress.in_flight,
                    "held_bytes": progress.held_bytes,
                })
            });
    }

    /// A complete block from the relay (compact relay, or a block of this node). The inner
    /// error is the reason why the header chain refuses the header.
    pub fn relayed(
        &mut self,
        block: Arc<RawBlock>,
        source: Source,
        received: Instant,
    ) -> Result<Result<(), String>, NodeError> {
        self.settle_clock();
        let hash = block.hash();
        if self.bodies.contains_key(&hash) {
            return Ok(Ok(()));
        }
        let excluded;
        let mut news = false;
        {
            let mut chain = self.headers.lock();
            let known = chain.entry(&hash);
            excluded = matches!(known, Some(entry) if entry.unavailable);
            match known.map(|entry| entry.status) {
                Some(Status::BodyKnown | Status::BodyValid) => return Ok(Ok(())),
                Some(Status::Invalid) => return Ok(Err(RejectReason::KnownInvalid.to_string())),
                Some(Status::HeaderValid) => {}
                None => {
                    let accepted = chain.accept_headers(
                        std::slice::from_ref(&block.header),
                        &ConsensusHeaderRules,
                        now_secs(),
                    );
                    match accepted {
                        Ok(_) => news = true,
                        Err(error) => match error.reason {
                            RejectReason::Store(e) => return Err(fatal("header log", e)),
                            reason => return Ok(Err(reason.to_string())),
                        },
                    }
                }
            }
            chain
                .mark_body_received(&hash)
                .map_err(|e| fatal("header chain", e))?;
        }
        if news {
            self.news();
        }
        self.bodies.insert(
            hash,
            Body {
                block,
                supplier: source,
                received,
                relayed: true,
                forward: false,
            },
        );
        self.drop_side_bodies()?;
        // The node has a block of a chain that is out of the fork choice.
        if excluded {
            self.include_withheld()?;
        }
        self.best_chain_changed()?;
        Ok(Ok(()))
    }

    /// Drops the bodies of the relay whose block left the header chain, and, above
    /// [`MAX_SIDE_BODIES`], the ones whose block is not on the best chain.
    fn drop_side_bodies(&mut self) -> Result<(), NodeError> {
        let mut chain = self.headers.lock();
        let mut side = Vec::new();
        self.bodies.retain(|hash, body| {
            if !body.relayed {
                return true;
            }
            match chain.entry(hash) {
                None => false,
                Some(entry) if entry.status == Status::Invalid => false,
                Some(entry) => {
                    if !entry.on_best_chain {
                        side.push(*hash);
                    }
                    true
                }
            }
        });
        if side.len() > MAX_SIDE_BODIES {
            for hash in side {
                self.bodies.remove(&hash);
                chain
                    .mark_body_missing(&hash)
                    .map_err(|e| fatal("header chain", e))?;
            }
        }
        Ok(())
    }

    /// The blocks that wait for the validator, in height order.
    pub fn delivered(&self) -> impl Iterator<Item = Delivered> + '_ {
        self.delivered.iter().copied()
    }

    /// The body of `hash` that the node holds in memory.
    pub fn body(&self, hash: &BlockHash) -> Option<&Body> {
        self.bodies.get(hash)
    }

    /// Whether the header chain reached a checkpoint at or above `height` on its best
    /// chain: the block of the best chain at `height` is then an ancestor of a checkpoint.
    pub fn checkpointed(&self, height: u32) -> bool {
        matches!(self.headers.lock().last_checkpoint_reached(), Some(last) if height <= last)
    }

    /// The height of the best header tip.
    pub fn header_height(&self) -> u32 {
        self.headers.lock().best_tip().height
    }

    /// Whether `hash` is the best header tip.
    pub fn is_best_tip(&self, hash: &BlockHash) -> bool {
        self.headers.lock().best_tip().hash == *hash
    }

    /// Whether the delivered blocks are enough to leave the committed tip `tip` for their
    /// branch: the last one is the best header tip, or it has more work than `tip`, or the
    /// validator has all the blocks that the scheduler delivers before a commit.
    pub fn branch_ready(&self, tip: &BlockHash) -> bool {
        let Some(last) = self.delivered.back() else {
            return false;
        };
        if self.delivered.len() >= self.lookahead {
            return true;
        }
        let chain = self.headers.lock();
        if chain.best_tip() == last.block {
            return true;
        }
        match (chain.entry(&last.block.hash), chain.entry(tip)) {
            (Some(branch), Some(tip)) => branch.work > tip.work,
            _ => false,
        }
    }

    /// The driver checked that the body of `hash` is the body of its header: no duplicated
    /// transaction, and from NU5 the authorizing data of the header commitment. A
    /// downloaded block that was the best header tip now goes on to the peers, before its
    /// validation, as a block of the relay does. The header chain records the body, so
    /// that the node serves the header.
    pub fn body_checked(&mut self, hash: &BlockHash) -> Result<(), NodeError> {
        let Some(body) = self.bodies.get_mut(hash) else {
            return Ok(());
        };
        if !std::mem::take(&mut body.forward) {
            return Ok(());
        }
        self.headers
            .lock()
            .mark_body_received(hash)
            .map_err(|e| fatal("header chain", e))?;
        self.relay.forward_block(body.block.clone(), body.supplier);
        Ok(())
    }

    /// The penalty of a compact-relay peer for a body that is not the body of the header
    /// and that the peer sent as the answer to a `getdata`. The relay gives such a peer no
    /// penalty for an invalid block, because it forwards a block before its validation. A
    /// `getdata` answer is a body that the peer holds.
    fn penalize_wrong_body(&self, supplier: Source) {
        if let Source::Peer {
            protocol: hayai_net::PeerProtocol::CompactRelay(_),
            ..
        } = supplier
        {
            self.relay.misbehaved(supplier, Misbehaviour::Malformed);
        }
    }

    /// The validator committed the first delivered block.
    pub fn committed(&mut self, hash: &BlockHash) -> Result<(), NodeError> {
        self.settle_clock();
        let Some(Delivered { block }) = self.delivered.pop_front() else {
            return Err(NodeError(format!(
                "committed block {hash} was not delivered"
            )));
        };
        if block.hash != *hash {
            return Err(NodeError(format!(
                "committed block {hash} is not the first delivered block {}",
                block.hash
            )));
        }
        self.headers
            .lock()
            .mark_body_valid(hash)
            .map_err(|e| fatal("header chain", e))?;
        // The legacy peers get the announcement of a block only now: a peer that gets an
        // invalid block gives the penalty to this node. The header chain has the valid
        // mark first, so a peer whose `getheaders` comes after the announcement gets the
        // header.
        self.relay.block_validated(hash);
        self.bodies.remove(hash);
        self.step(Event::BlockCommitted { hash: *hash })
    }

    /// The validator refused the first delivered block. The blocks after it are no longer
    /// delivered.
    pub fn refused(&mut self, hash: &BlockHash, refusal: Refusal) -> Result<(), NodeError> {
        self.settle_clock();
        self.delivered.clear();
        let body = self.bodies.remove(hash);
        // The scheduler gives the penalty of a body that it requested. The node gives the
        // penalty of a body of the relay.
        if let Some(Body {
            supplier,
            relayed: true,
            ..
        }) = &body
        {
            self.relay.misbehaved(*supplier, Misbehaviour::InvalidBlock);
        }
        match refusal {
            Refusal::WrongBody => {
                if let Some(Body {
                    supplier,
                    relayed: false,
                    ..
                }) = &body
                {
                    self.penalize_wrong_body(*supplier);
                }
                self.headers
                    .lock()
                    .mark_body_missing(hash)
                    .map_err(|e| fatal("header chain", e))?;
                self.step(Event::BlockInvalid { hash: *hash })
            }
            Refusal::Invalid => {
                let committed_on_best = {
                    let mut chain = self.headers.lock();
                    chain
                        .mark_invalid(hash)
                        .map_err(|e| fatal("header chain", e))?;
                    let committed = self.scheduler.stats().committed;
                    matches!(chain.entry(&committed.hash), Some(entry) if entry.on_best_chain)
                };
                if committed_on_best {
                    return self.step(Event::BlockInvalid { hash: *hash });
                }
                // The committed tip left the best chain with the invalid block: the
                // scheduler gets the new committed block, and the node gives the penalty.
                if let Some(Body {
                    supplier,
                    relayed: false,
                    ..
                }) = &body
                {
                    self.relay.misbehaved(*supplier, Misbehaviour::InvalidBlock);
                }
                self.best_chain_changed()
            }
        }
    }

    /// Makes the header log durable.
    pub fn sync_headers(&self) -> Result<(), NodeError> {
        self.headers
            .lock()
            .sync()
            .map_err(|e| fatal("header log", e))
    }

    pub fn stats(&self) -> Stats {
        self.scheduler.stats()
    }

    pub fn params(&self) -> NetParams {
        self.params
    }
}

impl Drop for Sync {
    fn drop(&mut self) {
        self.backlog.close();
    }
}
