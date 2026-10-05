//! The node's peer set and relay policy: both paths, always.
//!
//! Every block and transaction travels to legacy peers over the legacy protocol and to
//! compact-relay peers over the extension, at the same moment, so that a miner that does
//! not run the extension exchanges blocks with this node exactly as with any other node.
//!
//! - Transactions: `inv` (`MSG_WTX`, `MSG_TX` for v4) to legacy peers, `TxAnnounce` to
//!   compact-relay peers; `getdata`/`TxRequest` served from the [`TxLookup`] wire bytes;
//!   a transaction received on either path goes to one [`TxSink`].
//! - Blocks: every block, found locally, received in full from a legacy peer or
//!   reconstructed from a compact block, enters through one path ([`Relay::ingest_block`]):
//!   deduplicated by hash, header-checked, forwarded as `CompactBlock` to compact-relay
//!   peers and as `inv MSG_BLOCK` to legacy peers (served by `getdata` from the retained
//!   wire bytes), then handed to the [`BlockSink`]. Forwarding precedes full validation,
//!   as BIP 152 high-bandwidth peers do; proof of work bounds what can be forwarded.
//! - A received compact block is resolved against the local stores. Once every position
//!   has a WtxId and the merkle root (and, when the parent's history root is known, the
//!   auth data root) matches the header, the block is forwarded to version 2 peers at
//!   once: short ids re-keyed with this node's nonce, batch references and full ids
//!   unchanged, positions whose bytes this node lacks as full ids. The bytes are then
//!   requested with `TxRequest` from every peer that announced the block. Version 1 peers
//!   and legacy peers receive the block once its body is complete, as before.
//! - Per compact peer, the node remembers which transactions it announced and when, and
//!   which the peer announced back. A transaction announced less than
//!   [`RelayConfig::fresh_window`] ago and not announced back, or never announced, is a
//!   full id for a version 2 peer; a transaction outside the local store is prefilled.
//! - A `TxRequest` for a transaction of a block this node is still completing is answered
//!   when the bytes arrive; one for a retained block is answered from the retained body.
//! - A compact block waiting on a peer (`BlockTxn`, `Tx`, a batch, or the full block after
//!   a root mismatch) is retried on another announcing peer after
//!   [`RelayConfig::pending_retry`], falls back to `getdata MSG_BLOCK`, and is dropped
//!   after [`RelayConfig::pending_max_age`] or when no announcing peer is left.
//! - Lanes: `BatchAnnounce` only to compact-relay peers that negotiated the lanes feature.
//!   A received batch is flooded once, to those peers except its sender, after this node
//!   holds all of its transactions.
//! - Candidates (feature bit 2): [`Relay::publish_candidate`] sends a template change as a
//!   batch and a `CandidateAnnounce`. A received candidate is flooded once, to the peers
//!   with the feature except its sender, after this node holds all of its batches and their
//!   transactions. A block in canonical order whose set is close to a known candidate on
//!   its parent goes to those peers as a `CandidateBlock`. A received `CandidateBlock`
//!   whose candidate, batches or short ids do not resolve falls back to the full block; one
//!   whose additions lack bytes waits for them with `TxRequest`. Its canonical order needs
//!   the inputs of every transaction, so it is forwarded once its bytes are held.
//! - `getheaders` is answered from the [`ChainSource`], so legacy nodes sync past this node.
//! - Block synchronization belongs to the node. With a [`SyncSink`], the relay gives it the
//!   `headers`, `block` and `notfound` messages, the block hashes of `inv`, and the peers
//!   that connect and leave, and the relay sends no `getdata` for a block: the node sends
//!   `getheaders` and `getdata` through [`Relay::send_getheaders`] and
//!   [`Relay::request_blocks`], and gives a downloaded block back with
//!   [`Relay::forward_block`]. A compact block that the relay cannot complete is then an
//!   announcement for the sink, and the node downloads the block. Without a sink the relay
//!   asks for each announced block itself.
//! - Peer management goes through the [`PeerManager`]: it admits or refuses each connection
//!   (ban, inbound limit, limit per IP address), takes the addresses of `addr` and `addrv2`,
//!   and gives the `getaddr` answer (inbound peers only, once per connection, as zcashd).
//!   The node sends `getaddr` to each new outbound peer. An unsolicited `addr` of at most 10
//!   fresh addresses goes on to two other peers.
//! - Misbehaviour goes through the score of the peer's IP address (`hayai_sync::score`): a
//!   frame that does not decode, a message that the negotiated protocol does not permit, a
//!   header without valid proof of work, and what [`Relay::misbehaved`] reports. A failure
//!   of the compact relay that is not a fault (a short-id collision, an unknown batch or
//!   candidate, a late answer) costs nothing.

use std::collections::hash_map::Entry as MapEntry;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use hayai_consensus::HeaderRuleError;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_crypto::zcash_protocol::TxId;
use hayai_relay::HeaderError;
use hayai_relay::{
    Batch, BatchAnnounce, BatchRequest, BlockTxn, BlockTxnRequest, CandidateAnnounce,
    CandidateBlock, CandidatePartial, CandidateStore, CompactBlock, CompactBuilder, HeaderCheck,
    IdCheck, IdForm, LaneStore, Message, Partial, Publication, ReconstructError, ResolvedCandidate,
    Tx, TxAnnounce, TxRequest,
};
use hayai_sync::score::{Misbehaviour, Verdict};
use hayai_wire::header::{BlockHash, BlockHeader};
use hayai_wire::{RawBlock, RawTx, TxLookup, WtxId, PRE_V5_AUTH_DIGEST};
use rand::seq::SliceRandom;

use crate::addrbook::{ip_key, relayable, AddrBudget};
use crate::codec::{
    GetHeaders, InvItem, LegacyMessage, Network, TimedNetAddr, MAX_ADDR_ENTRIES, MAX_HEADERS,
    MAX_INV_ENTRIES,
};
use crate::connect::{PeerManager, Refusal};
use crate::protocol::{
    features, protocol_version, CompactVer, Negotiated, PeerProtocol, INITIAL_MIN_PEER_VERSION,
    NODE_COMPACT_RELAY, NODE_NETWORK, USER_AGENT,
};
use crate::session::{tx_inv_item, Action, Direction, PeerSession, SessionConfig, SessionError};
use crate::transport::{Control, Incoming, TcpTransport, Transport};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct PeerId(pub u64);

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "peer#{}", self.0)
    }
}

#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub network: Network,
    /// `None` runs a plain legacy node: no service bit, no `zcmpctver`, no `zcmpct`.
    pub compact_relay: Option<CompactVer>,
    pub user_agent: String,
    pub protocol_version: u32,
    /// Oldest peer protocol version accepted at the start
    /// (`crate::protocol::min_peer_version`). [`Relay::set_min_peer_version`] changes it.
    pub min_peer_version: u32,
    pub handshake_timeout: Duration,
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
    pub connect_timeout: Duration,
    /// Period of the keepalive and timeout sweep.
    pub tick: Duration,
    /// Messages queued per peer before it is considered too slow and dropped.
    pub outbound_queue: usize,
    /// Full blocks kept for `getdata`/`BlockTxnRequest`/`TxRequest` after forwarding.
    pub retained_blocks: usize,
    /// A compact block waiting on one peer this long is re-requested from another peer
    /// that announced it, or as a full block.
    pub pending_retry: Duration,
    /// A compact block still incomplete this long after its announcement is dropped.
    pub pending_max_age: Duration,
    /// A transaction announced to a version 2 peer less than this long ago, and not
    /// announced back, travels as a full id in a compact block.
    pub fresh_window: Duration,
    /// Time between two `mempool` requests to a legacy peer. The first request follows the
    /// handshake. `None`: no request. Zebra and Zakura announce a transaction one time, to
    /// a part of their peers that are ready, and each node reads the mempools of its peers
    /// for the rest (Zakura `mempool/crawler.rs`, 73 s).
    pub mempool_poll: Option<Duration>,
}

impl RelayConfig {
    pub fn new(network: Network) -> Self {
        Self {
            network,
            compact_relay: Some(CompactVer::CURRENT),
            user_agent: USER_AGENT.to_string(),
            protocol_version: protocol_version(),
            min_peer_version: INITIAL_MIN_PEER_VERSION,
            handshake_timeout: Duration::from_secs(10),
            ping_interval: Duration::from_secs(60),
            ping_timeout: Duration::from_secs(20 * 60),
            connect_timeout: Duration::from_secs(10),
            tick: Duration::from_secs(1),
            outbound_queue: 1024,
            retained_blocks: 32,
            pending_retry: Duration::from_secs(2),
            pending_max_age: Duration::from_secs(20),
            fresh_window: Duration::from_secs(3),
            mempool_poll: Some(Duration::from_secs(60)),
        }
    }

    /// Service bits advertised in `version`.
    pub fn services(&self) -> u64 {
        match self.compact_relay {
            Some(_) => NODE_NETWORK | NODE_COMPACT_RELAY,
            None => NODE_NETWORK,
        }
    }
}

/// Where a transaction or block came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Local,
    Peer {
        id: PeerId,
        protocol: PeerProtocol,
        /// The IP address of the peer: the score of a peer that left is still recorded.
        ip: IpAddr,
    },
}

impl Source {
    fn peer_id(&self) -> Option<PeerId> {
        match self {
            Source::Local => None,
            Source::Peer { id, .. } => Some(*id),
        }
    }
}

/// A header-checked block on its way to validation; every block passes here exactly once.
#[derive(Clone, Debug)]
pub struct IncomingBlock {
    pub block: Arc<RawBlock>,
    pub source: Source,
}

/// Receives transactions from both paths (the prepared store / mempool).
pub trait TxSink: Send + Sync {
    /// Returns `true` when the transaction is new to the node, which is what makes the
    /// relay announce it onwards.
    fn accept_tx(&self, tx: Arc<RawTx>, source: Source) -> bool;
}

/// Receives header-checked, deduplicated blocks (the validator).
pub trait BlockSink: Send + Sync {
    fn accept_block(&self, block: IncomingBlock);
}

/// What the block synchronization of the node reads from the network.
#[derive(Clone, Debug)]
pub enum SyncEvent {
    /// The handshake with `peer` is complete. `start_height` is the height of its `version`
    /// message.
    PeerConnected { peer: Source, start_height: u32 },
    /// The connection of a peer that had a complete handshake is closed.
    PeerDisconnected { peer: PeerId },
    /// A `headers` message.
    Headers {
        peer: Source,
        headers: Vec<BlockHeader>,
    },
    /// The block hashes of an `inv` message that the relay did not see before, or the hash
    /// of a compact block that the relay cannot use: the header check does not know its
    /// parent, or the compact path failed and the node must download the block.
    BlockInv {
        peer: Source,
        hashes: Vec<BlockHash>,
    },
    /// The payload of a `block` message.
    Block { peer: Source, bytes: Bytes },
    /// The block hashes of a `notfound` message.
    NotFound {
        peer: Source,
        hashes: Vec<BlockHash>,
    },
}

/// Receives the messages of the block synchronization. The calls come from the reader
/// thread of the peer, in the order of its messages.
pub trait SyncSink: Send + Sync {
    fn on_sync(&self, event: SyncEvent);
}

/// The chain as legacy peers see it.
pub trait ChainSource: Send + Sync {
    fn tip_height(&self) -> u32;
    /// The hash of the committed tip.
    fn tip_hash(&self) -> BlockHash;
    /// The consensus branch of the block after the committed tip: the relay parses a
    /// received transaction under it. `None`: the node has no rule set for that height,
    /// and the relay drops the transaction.
    fn tx_branch(&self) -> Option<BranchId>;
    /// The consensus branch of a block whose parent is `parent`: the relay parses the
    /// transactions of that block under it. `None`: the chain does not know the parent,
    /// or the node has no rule set for the height of the block.
    fn block_branch(&self, parent: &BlockHash) -> Option<BranchId>;
    /// Headers after the first locator hash the chain knows, up to `stop` (all-zero: none)
    /// and at most [`MAX_HEADERS`]. Fewer may be returned; more are truncated. With
    /// `validated_only` the answer ends before the first block that the node did not
    /// validate: a legacy peer requests each block whose header it gets, and gives the
    /// penalty for an invalid block to this node.
    fn headers_after(
        &self,
        locator: &[BlockHash],
        stop: &BlockHash,
        validated_only: bool,
    ) -> Vec<BlockHeader>;
    /// Wire bytes of a stored block, for `getdata` of blocks no longer retained here.
    fn block_bytes(&self, hash: &BlockHash) -> Option<Bytes>;
}

/// ZIP 221 chain history roots, for checking a compact block's auth data root through
/// `hashBlockCommitments` before the body is held.
pub trait HistoryRootSource: Send + Sync {
    /// The history tree root after `parent`, when the node knows it; `None` lets the block
    /// forward on its merkle root alone.
    fn history_root(&self, parent: &BlockHash) -> Option<[u8; 32]>;
}

/// Counters of the compact block paths.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RelayCounters {
    /// Compact blocks forwarded on a verified id list before the body was held.
    pub forwarded_on_ids: u64,
    /// Subset of `forwarded_on_ids` whose auth data root could not be checked because the
    /// parent's history root was unknown.
    pub forwarded_without_auth_root: u64,
    /// Blocks whose first compact forwarding carried a complete body.
    pub forwarded_after_body: u64,
    /// Id lists that failed the merkle or commitments check (full block requested).
    pub root_mismatches: u64,
    /// Candidate blocks sent to peers.
    pub candidate_blocks_sent: u64,
    /// Received candidate blocks that resolved against a stored candidate.
    pub candidate_blocks_resolved: u64,
    /// Received candidate blocks that fell back to the full block.
    pub candidate_fallbacks: u64,
}

#[derive(Default)]
struct Metrics {
    forwarded_on_ids: AtomicU64,
    forwarded_without_auth_root: AtomicU64,
    forwarded_after_body: AtomicU64,
    root_mismatches: AtomicU64,
    candidate_blocks_sent: AtomicU64,
    candidate_blocks_resolved: AtomicU64,
    candidate_fallbacks: AtomicU64,
}

impl Metrics {
    fn snapshot(&self) -> RelayCounters {
        RelayCounters {
            forwarded_on_ids: self.forwarded_on_ids.load(Ordering::Relaxed),
            forwarded_without_auth_root: self.forwarded_without_auth_root.load(Ordering::Relaxed),
            forwarded_after_body: self.forwarded_after_body.load(Ordering::Relaxed),
            root_mismatches: self.root_mismatches.load(Ordering::Relaxed),
            candidate_blocks_sent: self.candidate_blocks_sent.load(Ordering::Relaxed),
            candidate_blocks_resolved: self.candidate_blocks_resolved.load(Ordering::Relaxed),
            candidate_fallbacks: self.candidate_fallbacks.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerInfo {
    pub id: PeerId,
    pub addr: SocketAddr,
    pub direction: Direction,
    pub established: bool,
    pub protocol: PeerProtocol,
}

struct Peer {
    id: PeerId,
    addr: SocketAddr,
    direction: Direction,
    /// The nonce of this node's `version` on this connection.
    nonce: u64,
    transport: Arc<dyn Transport>,
    session: Mutex<PeerSession>,
    known: Mutex<PeerKnowledge>,
    established: AtomicBool,
    /// The node sent `getaddr` and the answer is not complete yet (zcashd `fGetAddr`).
    getaddr_sent: AtomicBool,
    /// The node answered a `getaddr` of this peer (zcashd `fSentAddr`).
    getaddr_answered: AtomicBool,
    addr_budget: Mutex<AddrBudget>,
    /// The `getdata` items that the node did not answer yet.
    getdata: Mutex<VecDeque<InvItem>>,
}

impl Peer {
    fn protocol(&self) -> PeerProtocol {
        self.session().protocol()
    }

    fn negotiated(&self) -> Option<Negotiated> {
        self.protocol().compact_relay().copied()
    }

    fn session(&self) -> MutexGuard<'_, PeerSession> {
        // A poisoned session belongs to a thread that panicked mid-message; the peer is
        // dropped on the next error either way, so the state is still usable.
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn known(&self) -> MutexGuard<'_, PeerKnowledge> {
        lock(&self.known)
    }
}

/// What this node knows about a compact peer's transaction set.
#[derive(Default)]
struct PeerKnowledge {
    map: HashMap<WtxId, Known>,
    /// Insertion order, for eviction.
    order: VecDeque<WtxId>,
}

#[derive(Clone, Copy)]
enum Known {
    /// This node announced the transaction to the peer at that time.
    AnnouncedAt(Instant),
    /// The peer announced or sent the transaction to this node.
    Held,
}

/// Ids remembered per peer. 64-byte keys: about 1.3 MiB per peer at the bound.
const KNOWN_PER_PEER: usize = 16_384;

impl PeerKnowledge {
    fn record(&mut self, id: WtxId, known: Known) {
        match self.map.entry(id) {
            MapEntry::Occupied(mut e) => {
                if let Known::Held = known {
                    e.insert(Known::Held);
                }
            }
            MapEntry::Vacant(e) => {
                e.insert(known);
                self.order.push_back(id);
            }
        }
        while self.order.len() > KNOWN_PER_PEER {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            self.map.remove(&old);
        }
    }

    /// The form of one block transaction for a version 2 peer.
    fn form(&self, id: &WtxId, in_store: bool, now: Instant, fresh_window: Duration) -> IdForm {
        match self.map.get(id) {
            Some(Known::Held) => IdForm::Short,
            Some(Known::AnnouncedAt(at)) if now.duration_since(*at) >= fresh_window => {
                IdForm::Short
            }
            Some(Known::AnnouncedAt(_)) => IdForm::Full,
            None if in_store => IdForm::Full,
            None => IdForm::Prefilled,
        }
    }
}

enum BlockState {
    /// Header checked; ids or bytes still being completed. `forwarded_on_ids` says whether
    /// version 2 peers already received the block on its verified id list.
    HeaderChecked { forwarded_on_ids: bool },
    /// Body complete, forwarded and handed to the sink.
    Complete,
}

/// Bounded memory of recent blocks: a hash set for deduplication and the last few bodies
/// for serving, with their transactions indexed by id.
struct RecentBlocks {
    states: HashMap<BlockHash, BlockState>,
    order: VecDeque<BlockHash>,
    bodies: HashMap<BlockHash, Arc<RawBlock>>,
    body_order: VecDeque<BlockHash>,
    tx_index: HashMap<WtxId, (BlockHash, u32)>,
    retained_bodies: usize,
}

const RECENT_HASHES: usize = 4096;

impl RecentBlocks {
    fn new(retained_bodies: usize) -> Self {
        Self {
            states: HashMap::new(),
            order: VecDeque::new(),
            bodies: HashMap::new(),
            body_order: VecDeque::new(),
            tx_index: HashMap::new(),
            retained_bodies,
        }
    }

    fn seen(&self, hash: &BlockHash) -> bool {
        self.states.contains_key(hash)
    }

    fn insert_state(&mut self, hash: BlockHash, state: BlockState) {
        let None = self.states.insert(hash, state) else {
            return;
        };
        self.order.push_back(hash);
        while self.order.len() > RECENT_HASHES {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            self.states.remove(&old);
        }
    }

    fn forwarded_on_ids(&self, hash: &BlockHash) -> bool {
        matches!(
            self.states.get(hash),
            Some(BlockState::HeaderChecked {
                forwarded_on_ids: true
            })
        )
    }

    fn retain_body(&mut self, block: Arc<RawBlock>) {
        let hash = block.hash();
        if self.bodies.contains_key(&hash) {
            return;
        }
        for (index, tx) in block.txs.iter().enumerate() {
            self.tx_index
                .entry(tx.wtxid())
                .or_insert((hash, index as u32));
        }
        self.bodies.insert(hash, block);
        self.body_order.push_back(hash);
        while self.body_order.len() > self.retained_bodies {
            let Some(old) = self.body_order.pop_front() else {
                break;
            };
            let Some(block) = self.bodies.remove(&old) else {
                continue;
            };
            for tx in &block.txs {
                if matches!(self.tx_index.get(&tx.wtxid()), Some((h, _)) if *h == old) {
                    self.tx_index.remove(&tx.wtxid());
                }
            }
        }
    }

    fn body(&self, hash: &BlockHash) -> Option<Arc<RawBlock>> {
        self.bodies.get(hash).cloned()
    }

    /// Wire bytes of a transaction of a retained block.
    fn tx_bytes(&self, id: &WtxId) -> Option<Bytes> {
        let (hash, index) = self.tx_index.get(id)?;
        let block = self.bodies.get(hash)?;
        Some(block.txs.get(*index as usize)?.bytes.clone())
    }

    /// Drops a header-checked hash whose body never arrived, so that the next announcement
    /// of the block starts over.
    fn forget(&mut self, hash: &BlockHash) {
        let Some(BlockState::HeaderChecked { .. }) = self.states.remove(hash) else {
            return;
        };
        self.order.retain(|h| h != hash);
    }
}

/// A compact block whose completion is waiting on a peer.
struct Pending {
    wait: Wait,
    /// Peer the current request went to.
    from: PeerId,
    /// The peer whose announcement this node is completing, as the block's source.
    source: Source,
    /// When the current request went out.
    since: Instant,
    /// When this wait started.
    started: Instant,
    /// Other peers that sent this compact block, asked in turn when `from` stalls or leaves.
    announcers: Vec<PeerId>,
    /// `TxRequest`s for transactions of this block that arrived before the bytes.
    deferred: Vec<(PeerId, WtxId)>,
}

enum Wait {
    /// `BlockTxn` for the positions `indexes`, whose short ids did not resolve.
    Ids {
        partial: Box<Partial>,
        indexes: Vec<u32>,
    },
    /// Ids complete and forwarded; `Tx` for the bytes this node lacks, from every announcer.
    Bytes(Box<Partial>),
    /// A `BatchAnnounce` for every unknown batch the compact block references.
    Batches(Box<CompactBlock>),
    /// The full block over `getdata MSG_BLOCK`, after the compact path failed (BIP 152:
    /// a root mismatch is a short-id collision, not a peer fault).
    FullBlock,
    /// A candidate block whose set is known; `Tx` for the bytes this node lacks, from every
    /// announcer.
    Candidate(Box<CandidatePartial>),
}

impl Pending {
    fn wants(&self, id: &WtxId) -> bool {
        match &self.wait {
            Wait::Ids { partial, .. } | Wait::Bytes(partial) => partial.wants(id),
            Wait::Candidate(partial) => partial.wants(id),
            Wait::Batches(_) | Wait::FullBlock => false,
        }
    }
}

const MAX_PENDING: usize = 64;
/// Bytes in the outbound queue of a peer above which the node stops the answers to its
/// `getdata` items until the queue drains (zcashd stops at its send buffer size).
const GETDATA_PAUSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_OWN_BATCHES: usize = 256;
/// Deferred `TxRequest` entries kept per pending block.
const MAX_DEFERRED: usize = 4096;
/// Received batches not yet complete, remembered for flooding once they are.
const MAX_INCOMPLETE_BATCHES: usize = 256;
/// Upper bound on the wire bytes carried by one `Tx` or `BlockTxn` answer.
const MAX_TX_ANSWER_BYTES: usize = 4 * 1024 * 1024;
/// Own candidates kept for the candidate form, as many as a receiver keeps per lane.
const MAX_OWN_CANDIDATES: usize = CandidateStore::PER_LANE;
/// Received candidates not yet complete, remembered for flooding once they are.
const MAX_INCOMPLETE_CANDIDATES: usize = 256;
/// Candidates a sender compares with one block.
const MAX_CANDIDATES_SEARCHED: usize = 32;

pub struct Relay {
    config: RelayConfig,
    session_config: SessionConfig,
    txs: Arc<dyn TxLookup>,
    tx_sink: Arc<dyn TxSink>,
    block_sink: Arc<dyn BlockSink>,
    chain: Arc<dyn ChainSource>,
    header_check: Arc<dyn HeaderCheck + Send + Sync>,
    history_roots: Arc<dyn HistoryRootSource>,
    sync: Option<Arc<dyn SyncSink>>,
    peers: Mutex<HashMap<PeerId, Arc<Peer>>>,
    /// The `version` nonces of the live connections, with the dialled address for an
    /// outbound connection. A `version` from a peer with one of them comes from this node
    /// (Bitcoin Core checks the nonces of its outbound connections in the same way).
    nonces: Mutex<HashMap<u64, Option<SocketAddr>>>,
    next_peer: AtomicU64,
    lanes: Mutex<LaneStore>,
    own_batches: Mutex<VecDeque<Batch>>,
    /// Received batches whose transactions are not all held yet, with their sender.
    incomplete_batches: Mutex<VecDeque<(hayai_relay::BatchId, PeerId)>>,
    candidates: Mutex<CandidateStore>,
    own_candidates: Mutex<VecDeque<ResolvedCandidate>>,
    /// Received candidates whose batches are not all held and complete yet, with their sender.
    incomplete_candidates: Mutex<VecDeque<(CandidateAnnounce, PeerId)>>,
    blocks: Mutex<RecentBlocks>,
    pending: Mutex<HashMap<BlockHash, Pending>>,
    /// The newest forwarded blocks that the legacy peers do not know yet, each with the
    /// peer that sent it. See [`Relay::block_validated`].
    unannounced: Mutex<VecDeque<(BlockHash, Option<PeerId>)>>,
    /// The time of the last `mempool` request to each legacy peer.
    mempool_polls: Mutex<HashMap<PeerId, Instant>>,
    metrics: Metrics,
    peer_manager: Arc<PeerManager>,
    min_peer_version: AtomicU32,
    listen_addr: Mutex<Option<SocketAddr>>,
    stopped: AtomicBool,
}

/// Everything the relay talks to.
pub struct RelayDeps {
    pub txs: Arc<dyn TxLookup>,
    pub tx_sink: Arc<dyn TxSink>,
    pub block_sink: Arc<dyn BlockSink>,
    pub chain: Arc<dyn ChainSource>,
    pub header_check: Arc<dyn HeaderCheck + Send + Sync>,
    pub history_roots: Arc<dyn HistoryRootSource>,
    /// The block synchronization of the node. `None`: the relay asks for each announced
    /// block itself and gives it to the [`BlockSink`].
    pub sync: Option<Arc<dyn SyncSink>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Which compact peers a block goes to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Audience {
    /// Every compact peer: the block's first compact forwarding.
    All,
    /// Version 2 peers only: forwarding on a verified id list.
    FullIdPeers,
    /// Version 1 peers only: the body of a block already forwarded on its ids.
    BodyOnlyPeers,
}

impl Audience {
    fn includes(self, negotiated: &Negotiated) -> bool {
        match self {
            Audience::All => true,
            Audience::FullIdPeers => negotiated.full_ids(),
            Audience::BodyOnlyPeers => !negotiated.full_ids(),
        }
    }
}

impl Relay {
    /// Builds the relay with a [`PeerManager`] that has the defaults of the network: an
    /// empty address book that is not saved, the system clock and the system resolver.
    pub fn new(config: RelayConfig, deps: RelayDeps) -> Arc<Self> {
        let peer_manager = PeerManager::with_defaults(config.network);
        Self::with_peer_manager(config, deps, peer_manager)
    }

    /// Builds the relay and starts its keepalive sweep. Nothing listens or connects until
    /// [`Relay::listen`], [`Relay::connect`] or [`PeerManager::maintain`] is called.
    pub fn with_peer_manager(
        config: RelayConfig,
        deps: RelayDeps,
        peer_manager: Arc<PeerManager>,
    ) -> Arc<Self> {
        let session_config = SessionConfig {
            protocol_version: config.protocol_version,
            min_peer_version: config.min_peer_version,
            services: config.services(),
            user_agent: config.user_agent.clone(),
            compact_relay: config.compact_relay,
            handshake_timeout: config.handshake_timeout,
            ping_interval: config.ping_interval,
            ping_timeout: config.ping_timeout,
        };
        let relay = Arc::new(Self {
            blocks: Mutex::new(RecentBlocks::new(config.retained_blocks)),
            min_peer_version: AtomicU32::new(config.min_peer_version),
            peer_manager,
            config,
            session_config,
            txs: deps.txs,
            tx_sink: deps.tx_sink,
            block_sink: deps.block_sink,
            chain: deps.chain,
            header_check: deps.header_check,
            history_roots: deps.history_roots,
            sync: deps.sync,
            peers: Mutex::new(HashMap::new()),
            nonces: Mutex::new(HashMap::new()),
            next_peer: AtomicU64::new(1),
            lanes: Mutex::new(LaneStore::new()),
            own_batches: Mutex::new(VecDeque::new()),
            incomplete_batches: Mutex::new(VecDeque::new()),
            candidates: Mutex::new(CandidateStore::new()),
            own_candidates: Mutex::new(VecDeque::new()),
            incomplete_candidates: Mutex::new(VecDeque::new()),
            pending: Mutex::new(HashMap::new()),
            unannounced: Mutex::new(VecDeque::new()),
            mempool_polls: Mutex::new(HashMap::new()),
            metrics: Metrics::default(),
            listen_addr: Mutex::new(None),
            stopped: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&relay);
        let tick = relay.config.tick;
        thread::Builder::new()
            .name("net-ticker".into())
            .spawn(move || loop {
                thread::sleep(tick);
                let Some(relay) = weak.upgrade() else {
                    break;
                };
                if relay.stopped.load(Ordering::Acquire) {
                    break;
                }
                relay.tick();
            })
            .expect("spawn ticker thread");
        relay
    }

    pub fn config(&self) -> &RelayConfig {
        &self.config
    }

    pub fn metrics(&self) -> RelayCounters {
        self.metrics.snapshot()
    }

    pub fn peer_manager(&self) -> &Arc<PeerManager> {
        &self.peer_manager
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// Sets the oldest peer protocol version accepted and disconnects the established peers
    /// below it. The node calls this when a network upgrade activates
    /// (`crate::protocol::min_peer_version`).
    pub fn set_min_peer_version(&self, version: u32) {
        self.min_peer_version.store(version, Ordering::Release);
        let peers: Vec<Arc<Peer>> = lock(&self.peers).values().cloned().collect();
        for peer in peers {
            let old = matches!(peer.session().peer_version(), Some(v) if v.version < version);
            if old {
                self.remove_peer(peer.id, "protocol version below the new minimum");
            }
        }
    }

    /// Binds `addr` and accepts peers on a background thread. Returns the bound address.
    pub fn listen(self: &Arc<Self>, addr: impl ToSocketAddrs) -> io::Result<SocketAddr> {
        let listener = TcpListener::bind(addr)?;
        let local = listener.local_addr()?;
        *lock(&self.listen_addr) = Some(local);
        let weak = Arc::downgrade(self);
        thread::Builder::new()
            .name(format!("net-accept-{local}"))
            .spawn(move || {
                for stream in listener.incoming() {
                    let Some(relay) = weak.upgrade() else {
                        break;
                    };
                    if relay.stopped.load(Ordering::Acquire) {
                        break;
                    }
                    match stream {
                        Ok(stream) => {
                            if let Err(e) = relay.add_peer(stream, Direction::Inbound) {
                                tracing::debug!(error = %e, "inbound peer setup failed");
                            }
                        }
                        Err(e) => tracing::debug!(error = %e, "accept failed"),
                    }
                }
            })?;
        Ok(local)
    }

    pub fn listen_addr(&self) -> Option<SocketAddr> {
        *lock(&self.listen_addr)
    }

    /// Connects to a peer and starts its handshake. The address book records the attempt,
    /// and a failure before the handshake completes. A banned address is refused.
    pub fn connect(self: &Arc<Self>, addr: SocketAddr) -> io::Result<PeerId> {
        if self.peer_manager.is_banned(addr.ip()) {
            return Err(io::Error::other(Refusal::Banned));
        }
        let stream = match TcpStream::connect_timeout(&addr, self.config.connect_timeout) {
            Ok(stream) => stream,
            Err(e) => {
                self.peer_manager.on_failed(&addr);
                return Err(e);
            }
        };
        self.add_peer(stream, Direction::Outbound)
    }

    fn add_peer(self: &Arc<Self>, stream: TcpStream, direction: Direction) -> io::Result<PeerId> {
        let id = PeerId(self.next_peer.fetch_add(1, Ordering::Relaxed));
        let transport = TcpTransport::new(stream, self.config.network, self.config.outbound_queue)?;
        let addr = transport.peer_addr();
        let mut session_config = self.session_config.clone();
        session_config.min_peer_version = self.min_peer_version.load(Ordering::Acquire);
        let (session, version) = PeerSession::new(
            session_config,
            direction,
            addr,
            self.chain.tip_height(),
            Instant::now(),
            unix_time(),
        );
        let nonce = session.nonce();
        let peer = Arc::new(Peer {
            id,
            addr,
            direction,
            nonce,
            transport: transport.clone(),
            session: Mutex::new(session),
            known: Mutex::new(PeerKnowledge::default()),
            established: AtomicBool::new(false),
            getaddr_sent: AtomicBool::new(false),
            getaddr_answered: AtomicBool::new(false),
            addr_budget: Mutex::new(AddrBudget::new(self.peer_manager.now())),
            getdata: Mutex::new(VecDeque::new()),
        });
        // The limits and the insertion are under one lock, so that two connections at the
        // same time cannot both pass.
        let admitted = {
            let mut peers = lock(&self.peers);
            let key = ip_key(addr.ip());
            let same_ip = peers
                .values()
                .filter(|p| ip_key(p.addr.ip()) == key)
                .count();
            let inbound = peers
                .values()
                .filter(|p| p.direction == Direction::Inbound)
                .count();
            let admitted = self
                .peer_manager
                .admit(addr.ip(), direction, same_ip, inbound);
            if let Ok(()) = admitted {
                peers.insert(id, peer.clone());
                let dialled = match direction {
                    Direction::Outbound => Some(addr),
                    Direction::Inbound => None,
                };
                lock(&self.nonces).insert(nonce, dialled);
            }
            admitted
        };
        if let Err(refusal) = admitted {
            transport.close();
            tracing::debug!(%addr, ?direction, %refusal, "connection refused");
            return Err(io::Error::other(refusal));
        }
        if direction == Direction::Outbound {
            self.peer_manager.on_attempt(&addr);
        }
        if let Err(e) = peer.transport.send(&version) {
            self.remove_peer(id, &e.to_string());
            return Err(io::Error::other(e.to_string()));
        }
        let relay = Arc::clone(self);
        let handler_peer = peer.clone();
        let started = transport.run_reader(Box::new(move |incoming| match incoming {
            Incoming::Message(m) => relay.on_message(&handler_peer, m),
            Incoming::Closed(reason) => {
                relay.remove_peer(handler_peer.id, &reason);
                Control::Close
            }
            Incoming::Malformed(reason) => {
                relay.penalize(&handler_peer, Misbehaviour::Malformed, &reason);
                relay.remove_peer(handler_peer.id, &reason);
                Control::Close
            }
        }));
        if let Err(e) = started {
            self.remove_peer(id, &e.to_string());
            return Err(e);
        }
        tracing::debug!(%id, %addr, ?direction, "peer added");
        Ok(id)
    }

    fn remove_peer(&self, id: PeerId, reason: &str) {
        let Some(peer) = lock(&self.peers).remove(&id) else {
            return;
        };
        peer.transport.close();
        lock(&self.nonces).remove(&peer.nonce);
        let established = peer.established.load(Ordering::Acquire);
        if peer.direction == Direction::Outbound && !established {
            self.peer_manager.on_failed(&peer.addr);
        }
        if let (Some(sync), true) = (&self.sync, established) {
            sync.on_sync(SyncEvent::PeerDisconnected { peer: id });
        }
        tracing::debug!(%id, addr = %peer.addr, reason, "peer removed");
    }

    /// Records a misbehaviour of `peer` and applies the verdict.
    fn penalize(&self, peer: &Peer, reason: Misbehaviour, detail: &str) {
        self.penalize_ip(peer.id, peer.addr.ip(), reason, detail);
    }

    /// Records a misbehaviour of the peer `id` at `ip` and applies the verdict: nothing, a
    /// disconnection, or a ban, which closes every connection with the same
    /// [`ip_key`]. The peer does not need to be connected.
    fn penalize_ip(&self, id: PeerId, ip: IpAddr, reason: Misbehaviour, detail: &str) {
        let verdict = self.peer_manager.record(ip, reason);
        tracing::debug!(%id, %ip, ?reason, ?verdict, detail, "peer misbehaved");
        match verdict {
            Verdict::Keep => {}
            Verdict::Disconnect => self.remove_peer(id, detail),
            Verdict::Ban => {
                tracing::info!(%ip, ?reason, detail, "peer banned");
                let key = ip_key(ip);
                let banned: Vec<PeerId> = lock(&self.peers)
                    .values()
                    .filter(|p| ip_key(p.addr.ip()) == key)
                    .map(|p| p.id)
                    .collect();
                for id in banned {
                    self.remove_peer(id, "banned");
                }
            }
        }
    }

    /// Reports a misbehaviour that the node found outside the relay: an invalid transaction
    /// from the [`TxSink`], an invalid block from the validator, a stall or unsolicited data
    /// from the block download.
    ///
    /// A local source has no score. [`Misbehaviour::InvalidBlock`] from a compact-relay peer
    /// has no score either: that peer forwards a block on its proof of work, before it
    /// validates the block (`docs/protocol-compact-relay.md`, Both paths, always). The
    /// score is recorded also when the peer left in the meantime.
    pub fn misbehaved(&self, source: Source, reason: Misbehaviour) {
        let Source::Peer { id, protocol, ip } = source else {
            return;
        };
        if let (Misbehaviour::InvalidBlock, PeerProtocol::CompactRelay(_)) = (reason, protocol) {
            return;
        }
        self.penalize_ip(id, ip, reason, "reported by the node");
    }

    /// Disconnects a peer after a compact-relay error. A structural error is a fault of the
    /// sender and costs score; an error that depends on the local stores does not.
    fn fail_compact(&self, peer: &Peer, error: &ReconstructError) {
        let detail = error.to_string();
        if compact_fault(error) {
            self.penalize(peer, Misbehaviour::Malformed, &detail);
        }
        self.remove_peer(peer.id, &detail);
    }

    /// Scores a header that fails a rule that needs no chain context: version, solution
    /// length, proof of work, Equihash. Every other failure depends on what this node knows
    /// (an unknown parent, a block already in the chain, a short context) or on its view of
    /// the chain (difficulty, time), and costs nothing here.
    fn penalize_header(&self, peer: &Peer, error: &HeaderError) {
        let context_free = matches!(
            error,
            HeaderError::Rule(
                HeaderRuleError::Version(_)
                    | HeaderRuleError::SolutionLength { .. }
                    | HeaderRuleError::Pow(_)
                    | HeaderRuleError::Equihash(_)
                    | HeaderRuleError::WorkOverflow
            )
        );
        if context_free {
            self.penalize(peer, Misbehaviour::InvalidHeader, &error.to_string());
        }
    }

    pub fn disconnect(&self, id: PeerId) {
        self.remove_peer(id, "disconnected locally");
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        let peers: Vec<Arc<Peer>> = lock(&self.peers).values().cloned().collect();
        let mut out: Vec<PeerInfo> = peers
            .iter()
            .map(|p| {
                let s = p.session();
                PeerInfo {
                    id: p.id,
                    addr: p.addr,
                    direction: s.direction(),
                    established: s.is_established(),
                    protocol: s.protocol(),
                }
            })
            .collect();
        out.sort_by_key(|p| p.id);
        out
    }

    /// Closes every connection and stops the background threads.
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        let peers: Vec<Arc<Peer>> = lock(&self.peers).drain().map(|(_, p)| p).collect();
        for p in peers {
            p.transport.close();
        }
        // Wakes the acceptor so that it observes the stop flag.
        if let Some(addr) = self.listen_addr() {
            let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(200));
        }
    }

    fn tick(&self) {
        let now = Instant::now();
        let peers: Vec<Arc<Peer>> = lock(&self.peers).values().cloned().collect();
        for peer in peers {
            let result = peer.session().on_tick(now);
            match result {
                Ok(actions) => self.perform(&peer, actions),
                Err(e) => self.remove_peer(peer.id, &e.to_string()),
            }
            self.serve_getdata(&peer);
        }
        self.poll_mempools(now);
        self.sweep_pending(now);
    }

    /// Sends the `mempool` requests that are due ([`RelayConfig::mempool_poll`]). The
    /// answer is an `inv`, and the relay requests the transactions that it does not have.
    fn poll_mempools(&self, now: Instant) {
        let Some(interval) = self.config.mempool_poll else {
            return;
        };
        let peers: Vec<Arc<Peer>> = self
            .established_peers()
            .into_iter()
            .filter(|peer| matches!(peer.protocol(), PeerProtocol::Legacy))
            .collect();
        let due: Vec<Arc<Peer>> = {
            let mut polls = lock(&self.mempool_polls);
            polls.retain(|id, _| peers.iter().any(|peer| peer.id == *id));
            peers
                .into_iter()
                .filter(|peer| match polls.get(&peer.id) {
                    Some(last) if now.duration_since(*last) < interval => false,
                    _ => {
                        polls.insert(peer.id, now);
                        true
                    }
                })
                .collect()
        };
        for peer in due {
            self.send(&peer, &LegacyMessage::Mempool);
        }
    }

    /// Moves stalled waits to the next announcing peer, falls back to the full block when no
    /// announcer is left, and drops waits whose peers all left or that exceeded the age bound.
    fn sweep_pending(&self, now: Instant) {
        let connected: HashMap<PeerId, Arc<Peer>> = lock(&self.peers).clone();
        let mut sends: Vec<(Arc<Peer>, LegacyMessage)> = Vec::new();
        let mut dropped: Vec<BlockHash> = Vec::new();
        let mut announced: Vec<(Arc<Peer>, BlockHash)> = Vec::new();
        {
            let lanes = lock(&self.lanes);
            let mut pending = lock(&self.pending);
            pending.retain(|hash, p| {
                if now.duration_since(p.started) >= self.config.pending_max_age {
                    tracing::debug!(%hash, "compact block incomplete past the age bound; dropping");
                    dropped.push(*hash);
                    return false;
                }
                let gone = !connected.contains_key(&p.from);
                if !gone && now.duration_since(p.since) < self.config.pending_retry {
                    return true;
                }
                let from = p.from;
                p.announcers
                    .retain(|id| *id != from && connected.contains_key(id));
                let next = if p.announcers.is_empty() {
                    if gone || matches!(p.wait, Wait::FullBlock) {
                        tracing::debug!(%hash, "no peer left to complete the compact block; dropping");
                        dropped.push(*hash);
                        return false;
                    }
                    if let (Some(_), Some(peer)) = (&self.sync, connected.get(&from)) {
                        // The node downloads the block.
                        announced.push((peer.clone(), *hash));
                        dropped.push(*hash);
                        return false;
                    }
                    p.wait = Wait::FullBlock;
                    from
                } else {
                    p.announcers.remove(0)
                };
                p.from = next;
                p.since = now;
                tracing::debug!(%hash, peer = %next, "re-requesting a stalled compact block");
                sends.push((connected[&next].clone(), request_for(*hash, &p.wait, &lanes)));
                true
            });
        }
        if !dropped.is_empty() {
            let mut blocks = lock(&self.blocks);
            for hash in &dropped {
                blocks.forget(hash);
            }
        }
        for (peer, message) in sends {
            self.send(&peer, &message);
        }
        if let Some(sync) = &self.sync {
            for (peer, hash) in announced {
                sync.on_sync(SyncEvent::BlockInv {
                    peer: self.peer_source(&peer),
                    hashes: vec![hash],
                });
            }
        }
    }

    /// The compact path of the block `hash` failed. Without a sink the relay asks `peer` for
    /// the full block. With a sink the relay forgets the block and reports it as announced
    /// by `peer`: the node downloads it. The `TxRequest`s that wait for the block get no
    /// answer.
    fn wait_for_full_block(
        &self,
        peer: &Arc<Peer>,
        hash: BlockHash,
        announcers: Vec<PeerId>,
        deferred: Vec<(PeerId, WtxId)>,
    ) {
        let Some(sync) = &self.sync else {
            return self.wait_on(peer, hash, Wait::FullBlock, announcers, deferred);
        };
        lock(&self.blocks).forget(&hash);
        sync.on_sync(SyncEvent::BlockInv {
            peer: self.peer_source(peer),
            hashes: vec![hash],
        });
    }

    /// A compact block whose header failed the check. A context-free failure costs score. An
    /// unknown parent is not a fault: with a sink the block is an announcement, and the node
    /// asks the peer for the headers.
    fn header_check_failed(&self, peer: &Arc<Peer>, hash: BlockHash, error: &HeaderError) {
        self.penalize_header(peer, error);
        if let (Some(sync), HeaderError::ParentUnknown(_)) = (&self.sync, error) {
            sync.on_sync(SyncEvent::BlockInv {
                peer: self.peer_source(peer),
                hashes: vec![hash],
            });
        }
    }

    fn on_message(&self, peer: &Arc<Peer>, message: LegacyMessage) -> Control {
        if let LegacyMessage::Version(v) = &message {
            let own_nonce = lock(&self.nonces).get(&v.nonce).copied();
            if let Some(dialled) = own_nonce {
                // The dialled address is this node. The nonce of an outbound connection
                // names it. For the nonce of an inbound connection, the reader is the
                // outbound side and the peer address is the dialled address. The address
                // never comes from the message, so that a peer that sends a nonce back
                // cannot remove the address of another node.
                let own = match (dialled, peer.direction) {
                    (Some(dialled), _) => Some(dialled),
                    (None, Direction::Outbound) => Some(peer.addr),
                    (None, Direction::Inbound) => None,
                };
                if let Some(own) = own {
                    self.peer_manager.on_self_connection(&own);
                }
                self.remove_peer(peer.id, "connected to self");
                return Control::Close;
            }
        }
        let result = peer.session().on_message(message);
        match result {
            Ok(actions) => self.perform(peer, actions),
            Err(e) => {
                let detail = e.to_string();
                match e {
                    SessionError::DuplicateVersion | SessionError::BeforeHandshake(_) => {
                        self.penalize(peer, Misbehaviour::Malformed, &detail);
                    }
                    SessionError::SelfConnection
                    | SessionError::VersionTooOld { .. }
                    | SessionError::HandshakeTimeout(_)
                    | SessionError::PingTimeout(_) => {}
                }
                self.remove_peer(peer.id, &detail);
                return Control::Close;
            }
        }
        if lock(&self.peers).contains_key(&peer.id) {
            Control::Continue {
                max_body: peer.session().max_body_len(),
            }
        } else {
            Control::Close
        }
    }

    fn perform(&self, peer: &Arc<Peer>, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Send(m) => self.send(peer, &m),
                Action::Established => {
                    tracing::info!(id = %peer.id, addr = %peer.addr, "handshake complete (legacy)");
                    let (version, services, start_height) = peer
                        .session()
                        .peer_version()
                        .map_or((0, 0, 0), |v| (v.version, v.services, v.start_height));
                    // The minimum can rise between the start of a session and its handshake.
                    if version < self.min_peer_version.load(Ordering::Acquire) {
                        self.remove_peer(peer.id, "protocol version below the new minimum");
                        return;
                    }
                    peer.established.store(true, Ordering::Release);
                    self.peer_manager
                        .on_established(&peer.addr, peer.direction, services);
                    if let Some(sync) = &self.sync {
                        sync.on_sync(SyncEvent::PeerConnected {
                            peer: self.peer_source(peer),
                            start_height,
                        });
                    }
                    // zcashd asks each new outbound peer for addresses.
                    if peer.direction == Direction::Outbound {
                        peer.getaddr_sent.store(true, Ordering::Release);
                        lock(&peer.addr_budget).grant_getaddr();
                        self.send(peer, &LegacyMessage::GetAddr);
                    }
                }
                Action::Upgraded(protocol) => {
                    tracing::info!(id = %peer.id, addr = %peer.addr, ?protocol, "compact relay negotiated");
                }
                Action::Deliver(m) => self.deliver(peer, m),
            }
        }
    }

    fn send(&self, peer: &Peer, message: &LegacyMessage) {
        self.send_or_remove(peer, message);
    }

    /// Queues `message` for `peer`. A failure removes the peer, and the result is false.
    fn send_or_remove(&self, peer: &Peer, message: &LegacyMessage) -> bool {
        match peer.transport.send(message) {
            Ok(()) => true,
            Err(e) => {
                self.remove_peer(peer.id, &e.to_string());
                false
            }
        }
    }

    fn send_compact(&self, peer: &Peer, message: Message) {
        self.send(peer, &LegacyMessage::Compact(message));
    }

    fn established_peers(&self) -> Vec<Arc<Peer>> {
        lock(&self.peers)
            .values()
            .filter(|p| p.session().is_established())
            .cloned()
            .collect()
    }

    /// Established compact peers with the lanes feature.
    fn lane_peers(&self) -> Vec<Arc<Peer>> {
        self.established_peers()
            .into_iter()
            .filter(|p| matches!(p.negotiated(), Some(n) if n.has(features::LANES_V1)))
            .collect()
    }

    /// Established compact peers with the candidates feature.
    fn candidate_peers(&self) -> Vec<Arc<Peer>> {
        self.established_peers()
            .into_iter()
            .filter(|p| matches!(p.negotiated(), Some(n) if n.candidates()))
            .collect()
    }

    fn peer_source(&self, peer: &Peer) -> Source {
        Source::Peer {
            id: peer.id,
            protocol: peer.protocol(),
            ip: peer.addr.ip(),
        }
    }

    // ----- application messages -----

    fn deliver(&self, peer: &Arc<Peer>, message: LegacyMessage) {
        match message {
            LegacyMessage::Inv(items) => self.on_inv(peer, items),
            LegacyMessage::GetData(items) => self.on_getdata(peer, items),
            LegacyMessage::Tx(bytes) => self.on_tx_bytes(peer, bytes),
            LegacyMessage::Block(bytes) => self.on_block_bytes(peer, bytes),
            LegacyMessage::GetHeaders(g) => {
                let legacy = matches!(peer.protocol(), PeerProtocol::Legacy);
                let mut headers = self.chain.headers_after(&g.locator, &g.stop, legacy);
                headers.truncate(MAX_HEADERS);
                self.send(peer, &LegacyMessage::Headers(headers));
            }
            // Zebra and Zakura read the chain of a peer with `getblocks`. The answer is the
            // hashes of the blocks that `getheaders` gives. Without such a block the answer
            // is the hash of the tip, which the peer has or can request. zcashd and Zakura
            // send no message then, but the connection of a Zakura peer takes no other
            // request while it waits for an answer (Zakura `sync.rs`,
            // `TIPS_RESPONSE_TIMEOUT`, 6 s): that peer announces its next block late. An
            // empty `inv` is not an answer: Zakura counts it as a stall and disconnects
            // the peer after 3 (`peer_set/stall_tracker.rs`).
            LegacyMessage::GetBlocks(g) => {
                let mut headers = self.chain.headers_after(&g.locator, &g.stop, true);
                headers.truncate(MAX_HEADERS);
                let mut hashes: Vec<InvItem> =
                    headers.iter().map(|h| InvItem::Block(h.hash())).collect();
                if hashes.is_empty() {
                    hashes.push(InvItem::Block(self.chain.tip_hash()));
                }
                self.send(peer, &LegacyMessage::Inv(hashes));
            }
            LegacyMessage::Mempool => self.on_mempool(peer),
            LegacyMessage::Compact(m) => self.on_compact(peer, m),
            LegacyMessage::NotFound(items) => {
                tracing::debug!(id = %peer.id, count = items.len(), "notfound");
                let hashes: Vec<BlockHash> = items
                    .iter()
                    .filter_map(|item| match item {
                        InvItem::Block(hash) => Some(*hash),
                        _ => None,
                    })
                    .collect();
                if let (Some(sync), false) = (&self.sync, hashes.is_empty()) {
                    sync.on_sync(SyncEvent::NotFound {
                        peer: self.peer_source(peer),
                        hashes,
                    });
                }
            }
            LegacyMessage::Addr(addrs) | LegacyMessage::AddrV2(addrs) => self.on_addr(peer, addrs),
            LegacyMessage::GetAddr => self.on_getaddr(peer),
            // The header sync belongs to the node: without a sink, `headers` is dropped.
            LegacyMessage::Headers(headers) => {
                if let Some(sync) = &self.sync {
                    sync.on_sync(SyncEvent::Headers {
                        peer: self.peer_source(peer),
                        headers,
                    });
                }
            }
            // Filter commands are decode-only.
            LegacyMessage::FilterLoad(_)
            | LegacyMessage::FilterAdd(_)
            | LegacyMessage::FilterClear => {}
            // Handled inside the session; never delivered.
            LegacyMessage::Version(_)
            | LegacyMessage::Verack
            | LegacyMessage::Ping(_)
            | LegacyMessage::Pong(_)
            | LegacyMessage::Reject(_)
            | LegacyMessage::SendAddrV2
            | LegacyMessage::CompactVer(_)
            | LegacyMessage::Unknown { .. } => {}
        }
    }

    /// Addresses from a peer: the budget of the connection bounds them, the address book
    /// takes them, and the fresh ones of a small unsolicited message go on to two other
    /// peers (zcashd `RelayAddress`). An address goes on only when it is news to the book, so
    /// that the same announcement does not travel twice through this node.
    fn on_addr(&self, peer: &Arc<Peer>, mut addrs: Vec<TimedNetAddr>) {
        let now = self.peer_manager.now();
        let solicited = peer.getaddr_sent.load(Ordering::Acquire);
        // zcashd: an answer of fewer than 1,000 addresses ends the `getaddr` exchange.
        if addrs.len() < MAX_ADDR_ENTRIES {
            peer.getaddr_sent.store(false, Ordering::Release);
        }
        let message_len = addrs.len();
        let allowed = lock(&peer.addr_budget).take(message_len, now);
        addrs.truncate(allowed);
        let news = self.peer_manager.on_addrs(peer.addr.ip(), &addrs);
        tracing::debug!(id = %peer.id, received = addrs.len(), news = news.len(), "addresses");
        if solicited {
            return;
        }
        let fresh = relayable(&news, message_len, now);
        if fresh.is_empty() {
            return;
        }
        let mut others: Vec<Arc<Peer>> = self
            .established_peers()
            .into_iter()
            .filter(|p| p.id != peer.id)
            .collect();
        others.shuffle(&mut rand::thread_rng());
        for other in others.iter().take(2) {
            self.send(other, &LegacyMessage::Addr(fresh.clone()));
        }
    }

    /// Answers `getaddr` as zcashd does: only to an inbound peer (an outbound peer could
    /// use the answer to recognise this node), and once per connection.
    fn on_getaddr(&self, peer: &Arc<Peer>) {
        if peer.direction != Direction::Inbound {
            return;
        }
        if peer.getaddr_answered.swap(true, Ordering::AcqRel) {
            return;
        }
        let addrs = self.peer_manager.getaddr_answer();
        if !addrs.is_empty() {
            self.send(peer, &LegacyMessage::Addr(addrs));
        }
    }

    fn on_inv(&self, peer: &Arc<Peer>, items: Vec<InvItem>) {
        let mut wanted = Vec::new();
        let mut announced = Vec::new();
        for item in items {
            let unknown = match &item {
                InvItem::Block(h) => !lock(&self.blocks).seen(h),
                InvItem::Wtx(id) => !self.has_tx(id),
                InvItem::Tx(txid) => !self.has_txid(txid),
                InvItem::Error(_) | InvItem::FilteredBlock(_) => false,
            };
            match (item, unknown, &self.sync) {
                (_, false, _) => {}
                // The node asks for the header, then for the block.
                (InvItem::Block(hash), true, Some(_)) => announced.push(hash),
                (item, true, _) => wanted.push(item),
            }
        }
        if !wanted.is_empty() {
            self.send(peer, &LegacyMessage::GetData(wanted));
        }
        if let (Some(sync), false) = (&self.sync, announced.is_empty()) {
            sync.on_sync(SyncEvent::BlockInv {
                peer: self.peer_source(peer),
                hashes: announced,
            });
        }
    }

    /// Queues the items of a `getdata` message of `peer` and serves what the outbound
    /// queue takes now. zcashd keeps the items too and stops while its send buffer is full.
    fn on_getdata(&self, peer: &Arc<Peer>, items: Vec<InvItem>) {
        {
            let mut backlog = lock(&peer.getdata);
            let room = MAX_INV_ENTRIES.saturating_sub(backlog.len());
            backlog.extend(items.into_iter().take(room));
        }
        self.serve_getdata(peer);
    }

    /// Answers the queued `getdata` items of `peer`, in order, while the outbound queue of
    /// the peer holds less than [`GETDATA_PAUSE_BYTES`]. The rest waits for the next
    /// message of the peer or the next tick. A failed send ends the answers: the peer left.
    fn serve_getdata(&self, peer: &Arc<Peer>) {
        let mut not_found = Vec::new();
        loop {
            if peer.transport.queued_bytes() >= GETDATA_PAUSE_BYTES {
                break;
            }
            let Some(item) = lock(&peer.getdata).pop_front() else {
                break;
            };
            let found = match item {
                // A legacy peer gets no block before its validation
                // ([`Relay::block_validated`]).
                InvItem::Block(h) => {
                    let unvalidated = matches!(peer.protocol(), PeerProtocol::Legacy)
                        && lock(&self.unannounced).iter().any(|(hash, _)| *hash == h);
                    match unvalidated {
                        true => None,
                        false => self.block_bytes(&h).map(LegacyMessage::Block),
                    }
                }
                InvItem::Wtx(id) => self
                    .txs
                    .get(&id)
                    .map(|tx| LegacyMessage::Tx(tx.bytes.clone())),
                InvItem::Tx(txid) => self
                    .lookup_txid(&txid)
                    .map(|tx| LegacyMessage::Tx(tx.bytes.clone())),
                InvItem::FilteredBlock(_) => None,
                InvItem::Error(_) => continue,
            };
            match found {
                Some(message) => {
                    if !self.send_or_remove(peer, &message) {
                        return;
                    }
                }
                None => not_found.push(item),
            }
        }
        if !not_found.is_empty() {
            self.send(peer, &LegacyMessage::NotFound(not_found));
        }
    }

    fn on_mempool(&self, peer: &Arc<Peer>) {
        let mut ids = Vec::with_capacity(self.txs.len());
        self.txs.for_each_id(&mut |id| ids.push(tx_inv_item(id)));
        for chunk in ids.chunks(MAX_INV_ENTRIES) {
            self.send(peer, &LegacyMessage::Inv(chunk.to_vec()));
        }
    }

    /// A transaction's bytes from a peer, on either path: pending blocks that wait for it
    /// take it whatever the sink decides, then the sink sees it.
    fn on_tx_bytes(&self, peer: &Arc<Peer>, bytes: Bytes) {
        let Some(branch) = self.chain.tx_branch() else {
            tracing::error!(id = %peer.id, "no rule set for the next block; transaction dropped");
            return;
        };
        let tx = match RawTx::parse(bytes, branch) {
            Ok(tx) => Arc::new(tx),
            Err(e) => {
                tracing::debug!(id = %peer.id, error = %e, "unparsable transaction");
                return;
            }
        };
        if let PeerProtocol::CompactRelay(_) = peer.protocol() {
            peer.known().record(tx.wtxid(), Known::Held);
        }
        self.offer_to_pending(&tx);
        self.ingest_tx(tx, self.peer_source(peer));
    }

    fn on_block_bytes(&self, peer: &Arc<Peer>, bytes: Bytes) {
        if let Some(sync) = &self.sync {
            sync.on_sync(SyncEvent::Block {
                peer: self.peer_source(peer),
                bytes,
            });
            return;
        }
        let branch = BlockHeader::parse(&bytes)
            .ok()
            .and_then(|header| self.chain.block_branch(&header.prev_hash));
        let Some(branch) = branch else {
            tracing::debug!(id = %peer.id, "block without a header or a known parent");
            return;
        };
        match RawBlock::parse(bytes, branch) {
            Ok(block) => self.ingest_block(Arc::new(block), self.peer_source(peer)),
            Err(e) => tracing::debug!(id = %peer.id, error = %e, "unparsable block"),
        }
    }

    fn on_compact(&self, peer: &Arc<Peer>, message: Message) {
        match message {
            Message::TxAnnounce(TxAnnounce { ids }) => {
                {
                    let mut known = peer.known();
                    for id in &ids {
                        known.record(*id, Known::Held);
                    }
                }
                let unknown: Vec<WtxId> = ids.into_iter().filter(|id| !self.has_tx(id)).collect();
                if !unknown.is_empty() {
                    self.send_compact(peer, Message::TxRequest(TxRequest { ids: unknown }));
                }
            }
            Message::TxRequest(TxRequest { ids }) => self.on_tx_request(peer, ids),
            Message::Tx(Tx { txs }) => {
                for bytes in txs {
                    self.on_tx_bytes(peer, bytes);
                }
            }
            Message::BatchAnnounce(announce) => self.on_batch_announce(peer, announce),
            Message::BatchRequest(BatchRequest { ids }) => {
                for id in ids {
                    let announce = {
                        let lanes = lock(&self.lanes);
                        let own = lock(&self.own_batches);
                        lanes
                            .get(&id)
                            .or_else(|| own.iter().find(|b| b.id == id))
                            .map(batch_announce)
                    };
                    if let Some(a) = announce {
                        self.send_compact(peer, Message::BatchAnnounce(a));
                    }
                }
            }
            Message::CompactBlock(cb) => self.ingest_compact(peer, cb),
            Message::BlockTxnRequest(BlockTxnRequest {
                block_hash,
                indexes,
            }) => {
                let Some(block) = lock(&self.blocks).body(&block_hash) else {
                    return;
                };
                let mut txs = Vec::with_capacity(indexes.len());
                for index in indexes {
                    let Some(tx) = block.txs.get(index as usize) else {
                        tracing::debug!(id = %peer.id, index, "BlockTxnRequest index out of range");
                        return;
                    };
                    txs.push(tx.bytes.clone());
                }
                self.send_compact(peer, Message::BlockTxn(BlockTxn { block_hash, txs }));
            }
            Message::BlockTxn(txn) => self.on_block_txn(peer, txn),
            Message::Block(b) => self.on_block_bytes(peer, b.bytes),
            Message::CandidateAnnounce(announce) => {
                if !matches!(peer.negotiated(), Some(n) if n.candidates()) {
                    let detail = "candidate announcement without the feature";
                    self.penalize(peer, Misbehaviour::Malformed, detail);
                    self.remove_peer(peer.id, detail);
                    return;
                }
                self.on_candidate_announce(peer, announce);
            }
            Message::CandidateBlock(cb) => {
                if !matches!(peer.negotiated(), Some(n) if n.candidates()) {
                    let detail = "candidate block without the feature";
                    self.penalize(peer, Misbehaviour::Malformed, detail);
                    self.remove_peer(peer.id, detail);
                    return;
                }
                self.ingest_candidate_block(peer, cb);
            }
        }
    }

    /// Stores a received candidate, asks the sender for the batches this node lacks, and
    /// floods the candidate once its batches are held and complete.
    fn on_candidate_announce(&self, peer: &Arc<Peer>, announce: CandidateAnnounce) {
        let inserted = lock(&self.candidates).insert(announce.clone(), Instant::now());
        match inserted {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                tracing::debug!(id = %peer.id, error = %e, "candidate announcement rejected");
                return;
            }
        }
        let unknown: Vec<hayai_relay::BatchId> = {
            let lanes = lock(&self.lanes);
            announce
                .batches
                .iter()
                .filter(|b| !known_batch(&lanes, b))
                .copied()
                .collect()
        };
        if !unknown.is_empty() {
            self.send_compact(peer, Message::BatchRequest(BatchRequest { ids: unknown }));
        }
        {
            let mut incomplete = lock(&self.incomplete_candidates);
            incomplete.push_back((announce, peer.id));
            while incomplete.len() > MAX_INCOMPLETE_CANDIDATES {
                incomplete.pop_front();
            }
        }
        self.flood_ready_candidates();
    }

    /// Floods the received candidates whose batches are now held and complete.
    fn flood_ready_candidates(&self) {
        let ready: Vec<(CandidateAnnounce, PeerId)> = {
            let lanes = lock(&self.lanes);
            let mut incomplete = lock(&self.incomplete_candidates);
            let mut ready = Vec::new();
            incomplete.retain(|(announce, from)| {
                let complete = announce.batches.iter().all(
                    |id| matches!(lanes.get(id), Some(batch) if batch.status(&*self.txs).complete),
                );
                if complete {
                    ready.push((announce.clone(), *from));
                }
                !complete
            });
            ready
        };
        for (announce, from) in ready {
            for peer in self.candidate_peers() {
                if peer.id != from {
                    self.send_compact(&peer, Message::CandidateAnnounce(announce.clone()));
                }
            }
        }
    }

    /// Publishes a template change of this node's lane: the batch of additions to lane
    /// peers, and the candidate to peers with the candidates feature. Keeps the candidate
    /// for the candidate form of this node's blocks.
    pub fn publish_candidate(&self, publication: Publication) {
        let Publication {
            batch,
            candidate,
            resolved,
        } = publication;
        if let Some(batch) = batch {
            self.announce_batch(batch);
        }
        {
            let mut own = lock(&self.own_candidates);
            own.push_back(resolved);
            while own.len() > MAX_OWN_CANDIDATES {
                own.pop_front();
            }
        }
        for peer in self.candidate_peers() {
            self.send_compact(&peer, Message::CandidateAnnounce(candidate.clone()));
        }
    }

    /// The received candidates on `parent` whose batches this node holds, newest first
    /// within each lane, at most `limit`.
    pub fn candidates_on(&self, parent: &BlockHash, limit: usize) -> Vec<ResolvedCandidate> {
        let lanes = lock(&self.lanes);
        let candidates = lock(&self.candidates);
        candidates
            .on_parent(parent)
            .into_iter()
            .filter_map(|c| hayai_relay::expand(c, &lanes).ok())
            .take(limit)
            .collect()
    }

    /// Own and received candidates on `parent`, for the candidate form of a block.
    fn candidates_for(&self, parent: &BlockHash) -> Vec<ResolvedCandidate> {
        let mut found: Vec<ResolvedCandidate> = lock(&self.own_candidates)
            .iter()
            .rev()
            .filter(|c| c.parent == *parent)
            .cloned()
            .collect();
        found.extend(self.candidates_on(parent, MAX_CANDIDATES_SEARCHED));
        found.truncate(MAX_CANDIDATES_SEARCHED);
        found
    }

    /// A candidate block: the header gate of every compact block, then resolution against
    /// the candidate store. Anything that does not resolve falls back to the full block.
    fn ingest_candidate_block(&self, peer: &Arc<Peer>, cb: Box<CandidateBlock>) {
        let header = match cb.parse_header() {
            Ok(h) => h,
            Err(e) => {
                let detail = format!("candidate block header: {e}");
                self.penalize(peer, Misbehaviour::Malformed, &detail);
                self.remove_peer(peer.id, &detail);
                return;
            }
        };
        let hash = header.hash();
        if self.note_announcer(&hash, peer.id) {
            return;
        }
        if let Err(e) = self.header_check.check(&header) {
            tracing::warn!(%hash, id = %peer.id, error = %e, "candidate block failed the header check");
            self.header_check_failed(peer, hash, &e);
            return;
        }
        if self.note_announcer(&hash, peer.id) {
            return;
        }
        lock(&self.blocks).insert_state(
            hash,
            BlockState::HeaderChecked {
                forwarded_on_ids: false,
            },
        );
        let Some(branch) = self.chain.block_branch(&header.prev_hash) else {
            tracing::debug!(%hash, id = %peer.id, "no consensus branch for the candidate block; requesting the full block");
            self.wait_for_full_block(peer, hash, Vec::new(), Vec::new());
            return;
        };
        let result = {
            let lanes = lock(&self.lanes);
            let candidates = lock(&self.candidates);
            hayai_relay::resolve_candidate(&cb, &*self.txs, &candidates, &lanes, branch)
        };
        let resolved = match result {
            Ok(resolved) => resolved,
            Err(e @ ReconstructError::CandidateFlags(_)) => {
                self.fail_compact(peer, &e);
                return;
            }
            Err(e) => {
                tracing::debug!(%hash, id = %peer.id, error = %e, "candidate block does not resolve; requesting the full block");
                self.metrics
                    .candidate_fallbacks
                    .fetch_add(1, Ordering::Relaxed);
                self.wait_for_full_block(peer, hash, Vec::new(), Vec::new());
                return;
            }
        };
        self.metrics
            .candidate_blocks_resolved
            .fetch_add(1, Ordering::Relaxed);
        tracing::info!(%hash, id = %peer.id, missing = resolved.missing_ids().len(), "candidate block header-valid; set resolved");
        if resolved.is_complete() {
            self.order_candidate(peer, resolved, Vec::new(), Vec::new());
        } else {
            self.wait_on(
                peer,
                hash,
                Wait::Candidate(Box::new(resolved)),
                Vec::new(),
                Vec::new(),
            );
        }
    }

    /// Puts a complete candidate block in canonical order and drives it as any partial.
    fn order_candidate(
        &self,
        peer: &Arc<Peer>,
        resolved: CandidatePartial,
        announcers: Vec<PeerId>,
        deferred: Vec<(PeerId, WtxId)>,
    ) {
        let hash = resolved.block_hash();
        match resolved.into_partial() {
            Ok(partial) => self.advance(peer, partial, announcers, deferred),
            Err(e) => {
                tracing::debug!(%hash, id = %peer.id, error = %e, "candidate block has no canonical order; requesting the full block");
                self.metrics
                    .candidate_fallbacks
                    .fetch_add(1, Ordering::Relaxed);
                self.wait_for_full_block(peer, hash, announcers, deferred);
            }
        }
    }

    /// Serves from the store, then from retained bodies; an id of a block still being
    /// completed is answered when its bytes arrive.
    fn on_tx_request(&self, peer: &Arc<Peer>, ids: Vec<WtxId>) {
        let mut found = Vec::with_capacity(ids.len());
        let mut rest = Vec::new();
        for id in ids {
            match self.txs.get(&id) {
                Some(tx) => found.push(tx.bytes.clone()),
                None => rest.push(id),
            }
        }
        if !rest.is_empty() {
            let blocks = lock(&self.blocks);
            let mut pending = lock(&self.pending);
            for id in rest {
                if let Some(bytes) = blocks.tx_bytes(&id) {
                    found.push(bytes);
                    continue;
                }
                let Some(p) = pending.values_mut().find(|p| p.wants(&id)) else {
                    continue;
                };
                if p.deferred.len() < MAX_DEFERRED {
                    p.deferred.push((peer.id, id));
                }
            }
        }
        for chunk in chunk_by_size(found.into_iter()) {
            self.send_compact(peer, Message::Tx(Tx { txs: chunk }));
        }
    }

    fn on_batch_announce(&self, peer: &Arc<Peer>, announce: BatchAnnounce) {
        let (status, was_known) = {
            let mut lanes = lock(&self.lanes);
            let was_known = known_batch(&lanes, &announce.batch_id);
            (
                lanes.insert(&announce, &*self.txs, Instant::now()),
                was_known,
            )
        };
        let status = match status {
            Ok(status) => status,
            Err(e) => {
                tracing::debug!(id = %peer.id, error = %e, "batch announcement rejected");
                return;
            }
        };
        if !status.missing.is_empty() {
            self.send_compact(
                peer,
                Message::TxRequest(TxRequest {
                    ids: status.missing,
                }),
            );
        }
        if !was_known {
            if status.complete {
                self.flood_batch(&announce, Some(peer.id));
            } else {
                let mut incomplete = lock(&self.incomplete_batches);
                incomplete.push_back((announce.batch_id, peer.id));
                while incomplete.len() > MAX_INCOMPLETE_BATCHES {
                    incomplete.pop_front();
                }
            }
        }
        self.flood_ready_candidates();
        // Compact blocks that were waiting on this batch.
        let ready: Vec<(BlockHash, Pending)> = {
            let lanes = lock(&self.lanes);
            let mut pending = lock(&self.pending);
            let hashes: Vec<BlockHash> = pending
                .iter()
                .filter(|(_, p)| match &p.wait {
                    Wait::Batches(compact) => {
                        compact.batch_refs.iter().all(|id| known_batch(&lanes, id))
                    }
                    Wait::Ids { .. } | Wait::Bytes(_) | Wait::FullBlock | Wait::Candidate(_) => {
                        false
                    }
                })
                .map(|(h, _)| *h)
                .collect();
            hashes
                .into_iter()
                .filter_map(|h| pending.remove(&h).map(|p| (h, p)))
                .collect()
        };
        for (hash, p) in ready {
            let Wait::Batches(compact) = p.wait else {
                unreachable!("filtered on the batch wait");
            };
            // The sender may have left meanwhile; any announcer still connected serves.
            let sender = {
                let peers = lock(&self.peers);
                std::iter::once(p.from)
                    .chain(p.announcers.iter().copied())
                    .find_map(|id| peers.get(&id).cloned())
            };
            let Some(sender) = sender else {
                lock(&self.blocks).forget(&hash);
                continue;
            };
            let announcers = p
                .announcers
                .into_iter()
                .filter(|id| *id != sender.id)
                .collect();
            self.resolve(&sender, compact, announcers, p.deferred);
        }
    }

    /// Sends a batch to every lane peer but `exclude`.
    fn flood_batch(&self, announce: &BatchAnnounce, exclude: Option<PeerId>) {
        for peer in self.lane_peers() {
            if Some(peer.id) == exclude {
                continue;
            }
            self.send_compact(&peer, Message::BatchAnnounce(announce.clone()));
        }
    }

    /// Floods received batches that the store now completes.
    fn flood_completed_batches(&self) {
        let ready: Vec<(BatchAnnounce, PeerId)> = {
            let lanes = lock(&self.lanes);
            let mut incomplete = lock(&self.incomplete_batches);
            let mut ready = Vec::new();
            incomplete.retain(|(id, from)| {
                let Some(batch) = lanes.get(id) else {
                    return false;
                };
                if !batch.status(&*self.txs).complete {
                    return true;
                }
                ready.push((batch_announce(batch), *from));
                false
            });
            ready
        };
        for (announce, from) in ready {
            self.flood_batch(&announce, Some(from));
        }
        self.flood_ready_candidates();
    }

    fn on_block_txn(&self, peer: &Arc<Peer>, txn: BlockTxn) {
        let taken = {
            let mut pending = lock(&self.pending);
            let expected = matches!(
                pending.get(&txn.block_hash),
                Some(Pending { wait: Wait::Ids { .. }, from, .. }) if *from == peer.id
            );
            if !expected {
                tracing::debug!(id = %peer.id, "unexpected BlockTxn");
                return;
            }
            pending.remove(&txn.block_hash)
        };
        let Some(Pending {
            wait: Wait::Ids {
                mut partial,
                indexes,
            },
            announcers,
            deferred,
            ..
        }) = taken
        else {
            unreachable!("checked above under the same lock");
        };
        let hash = partial.block_hash();
        let Some(branch) = self.chain.block_branch(&partial.header().prev_hash) else {
            tracing::debug!(%hash, id = %peer.id, "no consensus branch for the block; requesting the full block");
            self.wait_for_full_block(peer, hash, announcers, deferred);
            return;
        };
        if let Err(e) = partial.apply_block_txn(&txn, &indexes, branch) {
            tracing::debug!(id = %peer.id, error = %e, "BlockTxn does not complete the block");
            self.fail_compact(peer, &e);
            return;
        }
        self.advance(peer, *partial, announcers, deferred);
    }

    // ----- the two ingest paths -----

    /// Announces a locally accepted transaction on both paths.
    pub fn announce_tx(&self, tx: &RawTx) {
        self.broadcast_tx(&tx.wtxid(), None);
    }

    fn ingest_tx(&self, tx: Arc<RawTx>, source: Source) {
        let id = tx.wtxid();
        if self.tx_sink.accept_tx(tx, source) {
            self.broadcast_tx(&id, source.peer_id());
            self.flood_completed_batches();
        }
    }

    fn broadcast_tx(&self, id: &WtxId, exclude: Option<PeerId>) {
        let inv = LegacyMessage::Inv(vec![tx_inv_item(id)]);
        let now = Instant::now();
        for peer in self.established_peers() {
            if Some(peer.id) == exclude {
                continue;
            }
            match peer.protocol() {
                PeerProtocol::Legacy => self.send(&peer, &inv),
                PeerProtocol::CompactRelay(_) => {
                    peer.known().record(*id, Known::AnnouncedAt(now));
                    self.send_compact(&peer, Message::TxAnnounce(TxAnnounce { ids: vec![*id] }))
                }
            }
        }
    }

    /// Publishes a batch of this node's lane to peers that negotiated lanes, and keeps it
    /// so that compact blocks can reference it.
    pub fn announce_batch(&self, announce: BatchAnnounce) {
        {
            let mut own = lock(&self.own_batches);
            own.push_back(Batch {
                id: announce.batch_id,
                lane: announce.lane_id,
                seq: announce.seq,
                ids: announce.ids.clone(),
            });
            while own.len() > MAX_OWN_BATCHES {
                own.pop_front();
            }
        }
        self.flood_batch(&announce, None);
    }

    /// A block this node found.
    pub fn block_found(&self, block: RawBlock) {
        self.ingest_block(Arc::new(block), Source::Local);
    }

    /// The one path every complete block takes: dedup, header check, forward on both
    /// paths, hand to the sink.
    fn ingest_block(&self, block: Arc<RawBlock>, source: Source) {
        let hash = block.hash();
        let audience = {
            let mut blocks = lock(&self.blocks);
            match blocks.states.get(&hash) {
                Some(BlockState::Complete) => return,
                Some(BlockState::HeaderChecked { forwarded_on_ids }) => {
                    let audience = if *forwarded_on_ids {
                        Audience::BodyOnlyPeers
                    } else {
                        Audience::All
                    };
                    blocks.insert_state(hash, BlockState::Complete);
                    blocks.retain_body(block.clone());
                    audience
                }
                None => {
                    drop(blocks);
                    if let Err(e) = self.header_check.check(&block.header) {
                        tracing::warn!(%hash, ?source, error = %e, "block failed the header check");
                        let peer = source
                            .peer_id()
                            .and_then(|id| lock(&self.peers).get(&id).cloned());
                        if let Some(peer) = peer {
                            self.penalize_header(&peer, &e);
                        }
                        return;
                    }
                    let mut blocks = lock(&self.blocks);
                    if blocks.seen(&hash) {
                        // Raced with the same block on another connection.
                        return;
                    }
                    blocks.insert_state(hash, BlockState::Complete);
                    blocks.retain_body(block.clone());
                    Audience::All
                }
            }
        };
        self.forward(&block, source, audience);
        self.block_sink
            .accept_block(IncomingBlock { block, source });
    }

    /// A block that the node downloaded and whose header is in its header chain: the relay
    /// records and retains it, and forwards it on both paths. The block does not go to the
    /// [`BlockSink`]. A block that the relay already forwarded is ignored.
    pub fn forward_block(&self, block: Arc<RawBlock>, source: Source) {
        let hash = block.hash();
        let audience = {
            let mut blocks = lock(&self.blocks);
            let audience = match blocks.states.get(&hash) {
                Some(BlockState::Complete) => return,
                Some(BlockState::HeaderChecked {
                    forwarded_on_ids: true,
                }) => Audience::BodyOnlyPeers,
                Some(BlockState::HeaderChecked {
                    forwarded_on_ids: false,
                })
                | None => Audience::All,
            };
            blocks.insert_state(hash, BlockState::Complete);
            blocks.retain_body(block.clone());
            audience
        };
        self.forward(&block, source, audience);
    }

    /// The node validated the block `hash`: the relay announces it to the legacy peers,
    /// except to its source. A block that the relay did not forward has no announcement.
    ///
    /// A legacy peer gets no block before its validation. zcashd, Zebra and Zakura give
    /// a penalty for an invalid block to the peer that sent it, and Zakura bans that peer.
    /// A peer of the compact-relay extension gets the block after the header check
    /// (`docs/protocol-compact-relay.md`).
    pub fn block_validated(&self, hash: &BlockHash) {
        let exclude = {
            let mut unannounced = lock(&self.unannounced);
            let Some(at) = unannounced.iter().position(|(h, _)| h == hash) else {
                return;
            };
            unannounced.remove(at).and_then(|(_, source)| source)
        };
        let inv = LegacyMessage::Inv(vec![InvItem::Block(*hash)]);
        for peer in self.established_peers() {
            if Some(peer.id) == exclude {
                continue;
            }
            if let PeerProtocol::Legacy = peer.protocol() {
                self.send(&peer, &inv);
            }
        }
    }

    /// Sends a complete block to `audience` on the compact path, except to its source. The
    /// legacy peers get the announcement after the validation ([`Relay::block_validated`]).
    fn forward(&self, block: &Arc<RawBlock>, source: Source, audience: Audience) {
        let hash = block.hash();
        // A full block ends any wait for it (compact completion or `getdata`).
        let ended = lock(&self.pending).remove(&hash);
        tracing::info!(%hash, ?source, txs = block.txs.len(), "block header-valid; forwarding to compact-relay peers before validation");
        if let Audience::All = audience {
            self.metrics
                .forwarded_after_body
                .fetch_add(1, Ordering::Relaxed);
        }
        let exclude = source.peer_id();
        let builder = CompactBuilder::from_block(block, rand::random::<u64>());
        self.send_compact_block(&builder, exclude, audience);
        {
            let mut unannounced = lock(&self.unannounced);
            unannounced.push_back((hash, exclude));
            while unannounced.len() > RECENT_HASHES {
                unannounced.pop_front();
            }
        }
        if let Some(p) = ended {
            self.serve_deferred(block, p.deferred);
        }
    }

    /// Sends `getheaders` with `locator` to the peer `id`. Returns `false` when the peer is
    /// not connected.
    pub fn send_getheaders(&self, id: PeerId, locator: Vec<BlockHash>) -> bool {
        self.send_to(
            id,
            &LegacyMessage::GetHeaders(GetHeaders {
                version: self.config.protocol_version,
                locator,
                stop: BlockHash([0; 32]),
            }),
        )
    }

    /// Sends one `getdata` for the blocks `hashes` to the peer `id`. Returns `false` when
    /// the peer is not connected.
    pub fn request_blocks(&self, id: PeerId, hashes: &[BlockHash]) -> bool {
        let items = hashes.iter().map(|hash| InvItem::Block(*hash)).collect();
        self.send_to(id, &LegacyMessage::GetData(items))
    }

    fn send_to(&self, id: PeerId, message: &LegacyMessage) -> bool {
        let Some(peer) = lock(&self.peers).get(&id).cloned() else {
            return false;
        };
        self.send(&peer, message);
        true
    }

    /// Builds one compact block per peer in `audience` and sends it. Version 1 peers get
    /// today's forms (short id when in the store, prefilled otherwise); version 2 peers
    /// get the per-peer announcement history applied.
    fn send_compact_block(
        &self,
        builder: &CompactBuilder,
        exclude: Option<PeerId>,
        audience: Audience,
    ) {
        let peers: Vec<(Arc<Peer>, Negotiated)> = self
            .established_peers()
            .into_iter()
            .filter(|p| Some(p.id) != exclude)
            .filter_map(|p| p.negotiated().map(|n| (p, n)))
            .filter(|(_, n)| audience.includes(n))
            .collect();
        if peers.is_empty() {
            return;
        }
        let in_store: Vec<bool> = builder
            .entries()
            .iter()
            .map(|e| self.has_tx(&e.id))
            .collect();
        let candidates = match peers.iter().any(|(_, n)| n.candidates()) {
            true => self.candidates_for(&builder.parent()),
            false => Vec::new(),
        };
        let candidates: Vec<&ResolvedCandidate> = candidates.iter().collect();
        let now = Instant::now();
        // Lock order: `lanes`, then `own_batches`, as in the answer to `BatchRequest`. The
        // messages are built under the two locks and sent after their release, so a slow
        // send never holds a lock.
        let lanes = lock(&self.lanes);
        let own = lock(&self.own_batches);
        let batches: Vec<&Batch> = own.iter().chain(lanes.batches()).collect();
        let mut messages = Vec::with_capacity(peers.len());
        for (peer, negotiated) in peers {
            let batches: &[&Batch] = if negotiated.has(features::LANES_V1) {
                &batches
            } else {
                &[]
            };
            let message = {
                let known = peer.known();
                let form = |i: usize, id: &WtxId| match negotiated.full_ids() {
                    true => known.form(id, in_store[i], now, self.config.fresh_window),
                    false if in_store[i] => IdForm::Short,
                    false => IdForm::Prefilled,
                };
                let candidate_form = match negotiated.candidates() {
                    true => builder.build_candidate(&candidates, form),
                    false => None,
                };
                match candidate_form {
                    Some(cb) => {
                        self.metrics
                            .candidate_blocks_sent
                            .fetch_add(1, Ordering::Relaxed);
                        Message::CandidateBlock(Box::new(cb))
                    }
                    None => Message::CompactBlock(Box::new(builder.build(batches, form))),
                }
            };
            messages.push((peer, message));
        }
        drop(batches);
        drop(own);
        drop(lanes);
        for (peer, message) in messages {
            self.send_compact(&peer, message);
        }
    }

    fn ingest_compact(&self, peer: &Arc<Peer>, compact: Box<CompactBlock>) {
        let header = match compact.parse_header() {
            Ok(h) => h,
            Err(e) => {
                let detail = format!("compact block header: {e}");
                self.penalize(peer, Misbehaviour::Malformed, &detail);
                self.remove_peer(peer.id, &detail);
                return;
            }
        };
        if !compact.full_ids.is_empty() && !matches!(peer.negotiated(), Some(n) if n.full_ids()) {
            let detail = "full ids on a version 1 connection";
            self.penalize(peer, Misbehaviour::Malformed, detail);
            self.remove_peer(peer.id, detail);
            return;
        }
        let hash = header.hash();
        if self.note_announcer(&hash, peer.id) {
            return;
        }
        if let Err(e) = self.header_check.check(&header) {
            tracing::warn!(%hash, id = %peer.id, error = %e, "compact block failed the header check");
            self.header_check_failed(peer, hash, &e);
            return;
        }
        if self.note_announcer(&hash, peer.id) {
            // Raced with the same block on another connection.
            return;
        }
        lock(&self.blocks).insert_state(
            hash,
            BlockState::HeaderChecked {
                forwarded_on_ids: false,
            },
        );
        tracing::info!(%hash, id = %peer.id, "compact block header-valid; resolving");
        self.resolve(peer, compact, Vec::new(), Vec::new());
    }

    /// Whether `hash` was already seen; if its body is still being completed, `peer` is
    /// recorded as a fallback source.
    fn note_announcer(&self, hash: &BlockHash, peer: PeerId) -> bool {
        if !lock(&self.blocks).seen(hash) {
            return false;
        }
        if let Some(p) = lock(&self.pending).get_mut(hash) {
            if p.from != peer && !p.announcers.contains(&peer) {
                p.announcers.push(peer);
            }
        }
        true
    }

    fn resolve(
        &self,
        peer: &Arc<Peer>,
        compact: Box<CompactBlock>,
        announcers: Vec<PeerId>,
        deferred: Vec<(PeerId, WtxId)>,
    ) {
        let Ok(header) = compact.parse_header() else {
            unreachable!("the header was parsed before resolution");
        };
        let hash = header.hash();
        let Some(branch) = self.chain.block_branch(&header.prev_hash) else {
            tracing::debug!(%hash, id = %peer.id, "no consensus branch for the compact block; requesting the full block");
            self.wait_for_full_block(peer, hash, announcers, deferred);
            return;
        };
        let result = {
            let lanes = lock(&self.lanes);
            hayai_relay::resolve(&compact, &*self.txs, &lanes, branch)
        };
        match result {
            Ok(partial) => self.advance(peer, partial, announcers, deferred),
            Err(ReconstructError::UnknownBatch(id)) => {
                tracing::debug!(%hash, batch = ?id, "compact block references unknown batches");
                self.wait_on(peer, hash, Wait::Batches(compact), announcers, deferred);
            }
            Err(e) => {
                tracing::warn!(id = %peer.id, error = %e, "compact block rejected");
                self.fail_compact(peer, &e);
            }
        }
    }

    /// Drives a partial block after new information from `peer`: requests unknown ids,
    /// forwards once the id list verifies, then completes the body or requests its bytes.
    fn advance(
        &self,
        peer: &Arc<Peer>,
        partial: Partial,
        announcers: Vec<PeerId>,
        deferred: Vec<(PeerId, WtxId)>,
    ) {
        let hash = partial.block_hash();
        let unknown = partial.unknown();
        if !unknown.is_empty() {
            let wait = Wait::Ids {
                partial: Box::new(partial),
                indexes: unknown,
            };
            self.wait_on(peer, hash, wait, announcers, deferred);
            return;
        }
        if !lock(&self.blocks).forwarded_on_ids(&hash) {
            let history = self.history_roots.history_root(&partial.header().prev_hash);
            match partial.verify_ids(history.as_ref()) {
                Ok(check) => self.forward_on_ids(&partial, peer.id, check),
                Err(
                    e @ (ReconstructError::MerkleMismatch { .. }
                    | ReconstructError::CommitmentsMismatch),
                ) => {
                    tracing::debug!(id = %peer.id, %hash, error = %e, "id list does not match the header; requesting the full block");
                    self.metrics.root_mismatches.fetch_add(1, Ordering::Relaxed);
                    self.wait_for_full_block(peer, hash, announcers, deferred);
                    return;
                }
                Err(e) => {
                    tracing::error!(id = %peer.id, %hash, error = %e, "id check on an incomplete id list");
                    self.remove_peer(peer.id, &e.to_string());
                    return;
                }
            }
        }
        if partial.is_complete() {
            self.complete(
                peer.id,
                self.peer_source(peer),
                partial,
                announcers,
                deferred,
            );
            return;
        }
        self.wait_on(
            peer,
            hash,
            Wait::Bytes(Box::new(partial)),
            announcers,
            deferred,
        );
    }

    /// Forwards a block whose id list verified to version 2 peers, before its body is held.
    fn forward_on_ids(&self, partial: &Partial, exclude: PeerId, check: IdCheck) {
        let hash = partial.block_hash();
        lock(&self.blocks).insert_state(
            hash,
            BlockState::HeaderChecked {
                forwarded_on_ids: true,
            },
        );
        self.metrics
            .forwarded_on_ids
            .fetch_add(1, Ordering::Relaxed);
        if !check.auth_root_checked {
            self.metrics
                .forwarded_without_auth_root
                .fetch_add(1, Ordering::Relaxed);
        }
        tracing::info!(%hash, auth_root_checked = check.auth_root_checked, missing = partial.missing_ids().len(), "compact block id-complete; forwarding before the body");
        let builder = match partial.builder(rand::random::<u64>()) {
            Ok(b) => b,
            Err(e) => unreachable!("ids verified before forwarding: {e}"),
        };
        self.send_compact_block(&builder, Some(exclude), Audience::FullIdPeers);
    }

    /// Assembles a partial whose bytes are all held and runs it through the block path.
    fn complete(
        &self,
        from: PeerId,
        source: Source,
        partial: Partial,
        announcers: Vec<PeerId>,
        deferred: Vec<(PeerId, WtxId)>,
    ) {
        let hash = partial.block_hash();
        match partial.assemble() {
            Ok(block) => {
                let block = Arc::new(block);
                self.ingest_block(block.clone(), source);
                self.serve_deferred(&block, deferred);
            }
            Err(e @ ReconstructError::MerkleMismatch { .. }) => {
                let Some(peer) = lock(&self.peers).get(&from).cloned() else {
                    lock(&self.blocks).forget(&hash);
                    return;
                };
                tracing::debug!(id = %from, %hash, error = %e, "assembled body does not match the header; requesting the full block");
                self.metrics.root_mismatches.fetch_add(1, Ordering::Relaxed);
                self.wait_for_full_block(&peer, hash, announcers, deferred);
            }
            Err(e) => {
                tracing::error!(id = %from, %hash, error = %e, "assembly of a complete partial failed");
                self.remove_peer(from, &e.to_string());
            }
        }
    }

    /// Fills pending blocks that wait for `tx`'s bytes and completes those now whole.
    fn offer_to_pending(&self, tx: &Arc<RawTx>) {
        let whole: Vec<(BlockHash, Pending)> = {
            let mut pending = lock(&self.pending);
            let mut whole = Vec::new();
            for (hash, p) in pending.iter_mut() {
                let complete = match &mut p.wait {
                    Wait::Ids { partial, .. } => {
                        partial.supply(tx);
                        false
                    }
                    Wait::Bytes(partial) => partial.supply(tx) && partial.is_complete(),
                    Wait::Candidate(partial) => partial.supply(tx) && partial.is_complete(),
                    Wait::Batches(_) | Wait::FullBlock => false,
                };
                if complete {
                    whole.push(*hash);
                }
            }
            whole
                .into_iter()
                .filter_map(|h| pending.remove(&h).map(|p| (h, p)))
                .collect()
        };
        for (_, p) in whole {
            match p.wait {
                Wait::Bytes(partial) => {
                    self.complete(p.from, p.source, *partial, p.announcers, p.deferred)
                }
                Wait::Candidate(resolved) => {
                    let Some(peer) = lock(&self.peers).get(&p.from).cloned() else {
                        lock(&self.blocks).forget(&resolved.block_hash());
                        continue;
                    };
                    self.order_candidate(&peer, *resolved, p.announcers, p.deferred);
                }
                Wait::Ids { .. } | Wait::Batches(_) | Wait::FullBlock => {
                    unreachable!("filtered on the waits for bytes")
                }
            }
        }
    }

    /// Answers `TxRequest`s that waited for the block's bytes.
    fn serve_deferred(&self, block: &RawBlock, deferred: Vec<(PeerId, WtxId)>) {
        if deferred.is_empty() {
            return;
        }
        let mut per_peer: HashMap<PeerId, Vec<Bytes>> = HashMap::new();
        for (peer, id) in deferred {
            let Some(tx) = block.txs.iter().find(|t| t.wtxid() == id) else {
                continue;
            };
            per_peer.entry(peer).or_default().push(tx.bytes.clone());
        }
        let peers = lock(&self.peers).clone();
        for (id, txs) in per_peer {
            let Some(peer) = peers.get(&id) else {
                continue;
            };
            for chunk in chunk_by_size(txs.into_iter()) {
                self.send_compact(peer, Message::Tx(Tx { txs: chunk }));
            }
        }
    }

    /// Records that `hash` waits on `peer` and sends it the request; a wait for bytes also
    /// asks every other announcer at once. A full pending table drops the block instead;
    /// its next announcement starts over.
    fn wait_on(
        &self,
        peer: &Arc<Peer>,
        hash: BlockHash,
        wait: Wait,
        announcers: Vec<PeerId>,
        deferred: Vec<(PeerId, WtxId)>,
    ) {
        let request = {
            let lanes = lock(&self.lanes);
            request_for(hash, &wait, &lanes)
        };
        let fan_out = matches!(wait, Wait::Bytes(_) | Wait::Candidate(_));
        let now = Instant::now();
        let inserted = {
            let mut map = lock(&self.pending);
            if map.len() >= MAX_PENDING {
                false
            } else {
                map.insert(
                    hash,
                    Pending {
                        wait,
                        from: peer.id,
                        source: self.peer_source(peer),
                        since: now,
                        started: now,
                        announcers: announcers.clone(),
                        deferred,
                    },
                );
                true
            }
        };
        if !inserted {
            tracing::debug!(%hash, "pending compact block limit reached; dropping");
            lock(&self.blocks).forget(&hash);
            return;
        }
        self.send(peer, &request);
        if fan_out {
            let peers = lock(&self.peers).clone();
            for id in announcers {
                if let Some(other) = peers.get(&id) {
                    self.send(other, &request);
                }
            }
        }
    }

    // ----- lookups -----

    fn block_bytes(&self, hash: &BlockHash) -> Option<Bytes> {
        if let Some(block) = lock(&self.blocks).body(hash) {
            return Some(block.bytes.clone());
        }
        self.chain.block_bytes(hash)
    }

    /// `MSG_TX` lookup: a v4 transaction's WtxId directly, else a scan for the txid.
    fn lookup_txid(&self, txid: &TxId) -> Option<Arc<RawTx>> {
        let v4 = WtxId {
            txid: *txid,
            auth_digest: PRE_V5_AUTH_DIGEST,
        };
        if let Some(tx) = self.txs.get(&v4) {
            return Some(tx);
        }
        let mut found = None;
        self.txs.for_each_id(&mut |id| {
            if let (None, true) = (found, id.txid == *txid) {
                found = Some(*id);
            }
        });
        found.and_then(|id| self.txs.get(&id))
    }

    fn has_tx(&self, id: &WtxId) -> bool {
        let Some(_) = self.txs.get(id) else {
            return false;
        };
        true
    }

    fn has_txid(&self, txid: &TxId) -> bool {
        let Some(_) = self.lookup_txid(txid) else {
            return false;
        };
        true
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

/// Whether a compact-relay error is a fault of the sender: the message breaks the frame
/// rules whatever the receiver holds. Every other error depends on the local stores, on the
/// local parser, or on an internal state, and is not a fault.
fn compact_fault(error: &ReconstructError) -> bool {
    match error {
        ReconstructError::TooManyTransactions(_)
        | ReconstructError::IndexOrder { .. }
        | ReconstructError::IndexOutOfRange { .. }
        | ReconstructError::IndexTwice(_)
        | ReconstructError::WrongBlock { .. }
        | ReconstructError::TxnCountMismatch { .. }
        | ReconstructError::AlreadyHeld(_)
        | ReconstructError::CandidateFlags(_) => true,
        ReconstructError::UnknownBatch(_)
        | ReconstructError::Header(_)
        | ReconstructError::Transaction { .. }
        | ReconstructError::IdsIncomplete(_)
        | ReconstructError::BytesIncomplete(_)
        | ReconstructError::MerkleMismatch { .. }
        | ReconstructError::CommitmentsMismatch
        | ReconstructError::UnknownCandidate { .. }
        | ReconstructError::CandidateParent { .. }
        | ReconstructError::Candidate(_)
        | ReconstructError::UnresolvedShortIds(_)
        | ReconstructError::DuplicateId(_)
        | ReconstructError::Order(_) => false,
    }
}

fn known_batch(lanes: &LaneStore, id: &hayai_relay::BatchId) -> bool {
    let Some(_) = lanes.get(id) else {
        return false;
    };
    true
}

/// The message that asks a peer for what a wait needs.
fn request_for(hash: BlockHash, wait: &Wait, lanes: &LaneStore) -> LegacyMessage {
    match wait {
        Wait::Ids { indexes, .. } => LegacyMessage::Compact(Message::BlockTxnRequest(
            BlockTxnRequest::for_missing(hash, indexes.clone()),
        )),
        Wait::Bytes(partial) => LegacyMessage::Compact(Message::TxRequest(TxRequest {
            ids: partial.missing_ids(),
        })),
        Wait::Batches(compact) => {
            let ids = compact
                .batch_refs
                .iter()
                .filter(|b| !known_batch(lanes, b))
                .copied()
                .collect();
            LegacyMessage::Compact(Message::BatchRequest(BatchRequest { ids }))
        }
        Wait::FullBlock => LegacyMessage::GetData(vec![InvItem::Block(hash)]),
        Wait::Candidate(partial) => LegacyMessage::Compact(Message::TxRequest(TxRequest {
            ids: partial.missing_ids(),
        })),
    }
}

fn batch_announce(batch: &Batch) -> BatchAnnounce {
    BatchAnnounce {
        lane_id: batch.lane,
        seq: batch.seq,
        batch_id: batch.id,
        ids: batch.ids.clone(),
    }
}

/// Groups transaction bytes into answers of at most [`MAX_TX_ANSWER_BYTES`] each; a single
/// transaction larger than that forms its own group.
fn chunk_by_size(txs: impl Iterator<Item = Bytes>) -> Vec<Vec<Bytes>> {
    let mut out: Vec<Vec<Bytes>> = Vec::new();
    let mut current = Vec::new();
    let mut size = 0usize;
    for tx in txs {
        if !current.is_empty() && size + tx.len() > MAX_TX_ANSWER_BYTES {
            out.push(std::mem::take(&mut current));
            size = 0;
        }
        size += tx.len();
        current.push(tx);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_respects_the_size_bound() {
        let big = Bytes::from(vec![0u8; MAX_TX_ANSWER_BYTES - 10]);
        let small = Bytes::from(vec![1u8; 20]);
        let chunks = chunk_by_size(vec![small.clone(), big.clone(), small.clone()].into_iter());
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], vec![small.clone()]);
        assert_eq!(chunks[1], vec![big]);
        assert_eq!(chunks[2], vec![small]);
        assert!(chunk_by_size(std::iter::empty()).is_empty());
    }

    #[test]
    fn recent_blocks_bound_hashes_and_bodies() {
        let mut recent = RecentBlocks::new(2);
        for i in 0..(RECENT_HASHES + 5) {
            let mut h = [0u8; 32];
            h[..8].copy_from_slice(&(i as u64).to_le_bytes());
            recent.insert_state(BlockHash(h), BlockState::Complete);
        }
        assert_eq!(recent.states.len(), RECENT_HASHES);
        assert!(!recent.seen(&BlockHash([0u8; 32])));
    }

    fn id(n: u8) -> WtxId {
        WtxId {
            txid: TxId::from_bytes([n; 32]),
            auth_digest: [n; 32],
        }
    }

    #[test]
    fn peer_knowledge_decides_the_id_form() {
        let window = Duration::from_secs(3);
        let t0 = Instant::now();
        let mut known = PeerKnowledge::default();
        // Never announced: full when held, prefilled when outside the store.
        assert_eq!(known.form(&id(1), true, t0, window), IdForm::Full);
        assert_eq!(known.form(&id(1), false, t0, window), IdForm::Prefilled);
        // Announced recently: full until the window passes.
        known.record(id(1), Known::AnnouncedAt(t0));
        assert_eq!(
            known.form(&id(1), true, t0 + window / 2, window),
            IdForm::Full
        );
        assert_eq!(known.form(&id(1), true, t0 + window, window), IdForm::Short);
        // Announced back by the peer: short at once, and a later announcement by this
        // node does not demote it.
        known.record(id(2), Known::AnnouncedAt(t0));
        known.record(id(2), Known::Held);
        known.record(id(2), Known::AnnouncedAt(t0));
        assert_eq!(known.form(&id(2), true, t0, window), IdForm::Short);
    }

    #[test]
    fn peer_knowledge_is_bounded() {
        let mut known = PeerKnowledge::default();
        let t0 = Instant::now();
        for i in 0..(KNOWN_PER_PEER + 10) {
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&(i as u64).to_le_bytes());
            let id = WtxId {
                txid: TxId::from_bytes(bytes),
                auth_digest: bytes,
            };
            known.record(id, Known::Held);
        }
        assert_eq!(known.map.len(), KNOWN_PER_PEER);
        assert_eq!(known.order.len(), KNOWN_PER_PEER);
        let mut first = [0u8; 32];
        first[..8].copy_from_slice(&0u64.to_le_bytes());
        let evicted = WtxId {
            txid: TxId::from_bytes(first),
            auth_digest: first,
        };
        assert_eq!(
            known.form(&evicted, true, t0, Duration::from_secs(3)),
            IdForm::Full
        );
    }
}
