//! The scheduler of the block download. See `docs/plan-consensus-and-sync.md`, item B2.
//!
//! The scheduler is pure logic. It has no sockets, no threads and no clock: the caller gives
//! the time in milliseconds with each event. It holds no block body: the node holds the
//! bodies, and the scheduler counts their bytes.
//!
//! # Events and actions
//!
//! The node gives an [`Event`] to [`Scheduler::handle`] and applies the [`Action`]s that the
//! function returns, in order. The node sends a `getdata` message for
//! [`Action::Request`], keeps or drops a body for [`Action::Store`] and
//! [`Action::Discard`], and gives a body to the validator for [`Action::Deliver`].
//!
//! # Window
//!
//! The window holds the blocks of the best header chain after the committed tip, in height
//! order, at most [`DownloadConfig::window_blocks`]. Each block of the window is in one of
//! three states:
//!
//! - missing: no request is active;
//! - requested: one peer has the active request;
//! - held: the node has the body.
//!
//! A block leaves the window when the validator commits it. The header chain is the source
//! of the window ([`HeaderChain::best_chain_from`]). A block whose state in the header chain
//! is not [`Status::HeaderValid`] is held by the node from the start, with zero bytes in the
//! count of the scheduler.
//!
//! # Memory bound
//!
//! `held bytes + requested blocks x max_block_bytes <= memory_budget_bytes` at all times
//! ([`Stats::memory_bytes`]). A request reserves the largest block, because the size of a
//! block is not known before its body arrives. The scheduler admits a request while the sum
//! stays at or below the budget minus one largest block. The first block of the window is
//! the exception: its request can use the last largest block of the budget. Thus the block
//! that the validator needs next always has room, and the bound holds.
//!
//! # Order
//!
//! Bodies arrive in any order. [`Action::Deliver`] goes out strictly in height order. At
//! most [`DownloadConfig::validation_lookahead`] blocks are delivered and not committed.
//! Each block of the best chain is delivered one time. The exception is a body that does not
//! match its header ([`Event::BlockInvalid`]): the validator gets that block and the
//! delivered blocks after it again.
//!
//! # Peers
//!
//! The scheduler assigns the missing blocks in height order. For each block it takes the
//! peer with the smallest expected time to delivery: the moving average of the delivery time
//! of the peer, multiplied by its requests in flight plus one. Thus the load spreads across
//! the peers in proportion to their speed. A peer has a limit of requests in flight, in
//! blocks and in estimated bytes. A peer that reported a height below a block gets the
//! request only when no other connected peer reported the height.
//!
//! # Liveness
//!
//! The scheduler measures the progress of a peer: the time since its last body, or since
//! its oldest request when that is later. The allowance of a peer is the time of one largest
//! block at its measured rate.
//!
//! - Rescue. The lowest block that is not held stops the validator. When its peer has a
//!   measured rate and makes no progress for [`DownloadConfig::rescue_timeout_ms`] plus the
//!   allowance, the scheduler moves all the requests of the peer to other peers. The peer
//!   gets no penalty and no new request until it sends a body or one request timeout
//!   passes. A body that arrives later is used.
//! - Stall. When a peer makes no progress for [`DownloadConfig::request_timeout_ms`] plus
//!   the allowance and a request is not answered, the peer gets one [`Misbehaviour::Stall`]
//!   and its requests move to other peers. Two stalls disconnect the peer. A request that
//!   moved in a rescue stays a request without an answer for this rule.
//! - A peer that answers a later request before an earlier one makes no progress for the
//!   earlier request from that time. Thus a peer cannot keep one block back while it sends
//!   the others.
//! - The answer limits of a peer. Zebra and Zakura answer at most 16 blocks and 1 MB for
//!   one `getdata` message and are silent for the others. zcashd answers each request.
//!   One message has at most 16 blocks, and fewer when the blocks before the last one
//!   reach 1 MB at two times the mean size of the recent blocks. When the answers to one
//!   message reached 1 MB and the peer then sends no body for the request timeout, the
//!   other requests of the message are free again without a penalty.
//! - `notfound` moves the request to another peer without a penalty. When no connected peer
//!   supplied a block, the scheduler waits before it requests the block again.
//! - A body that is not an answer to a request gives [`Misbehaviour::Unsolicited`]. The
//!   node drops the body.
//!
//! # Bodies that no peer sends
//!
//! The node tells the scheduler when the chain of a peer leaves the best header chain
//! ([`Event::PeerForkPoint`]). The scheduler does not ask that peer for a block above the
//! fork point: the peer does not have the block, and a request without an answer is a
//! stall of an honest peer. When each connected peer failed to supply the lowest missing
//! block, or no connected peer can have it, [`Scheduler::withheld`] names the block. The
//! node then takes the block out of the choice of the best header chain
//! (`HeaderChain::mark_unavailable`) and downloads the chain with the most work among the
//! other chains.
//!
//! # Start
//!
//! [`Scheduler::new`] builds the window from the header chain and the committed tip. The
//! scheduler has no state on disk.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Debug;

use hayai_wire::header::BlockHash;
use hayai_wire::MAX_BLOCK_BYTES;

use crate::headers::{HeaderChain, Status, Tip};
use crate::score::{Misbehaviour, PeerScore, Verdict};

/// Requests in flight that a peer without a delivery can have.
const UNMEASURED_PEER_BLOCKS: u32 = 4;
/// Blocks in one `getdata` message, at most. zcashd, Zebra and Zakura answer at most 16
/// blocks of one message (zcashd `MAX_BLOCKS_IN_TRANSIT_PER_PEER`, Zakura
/// `inbound.rs`, `GETDATA_MAX_BLOCK_COUNT`), and Zebra and Zakura never answer the others.
const GETDATA_MAX_BLOCKS: usize = 16;
/// Bytes of the answers to one `getdata` message after which a Zebra or Zakura peer answers
/// no other request of the message (Zakura `inbound.rs`, `GETDATA_SENT_BYTES_LIMIT`). The
/// block that reaches the limit is the last answer.
const GETDATA_ANSWER_BYTES: u64 = 1_000_000;
/// A released request stays known for this number of request timeouts. A body that arrives
/// in this time is late, not unsolicited.
const LATE_REQUEST_TIMEOUTS: u64 = 8;
/// The back-off doubles at most this number of times.
const BACKOFF_MAX_DOUBLINGS: u32 = 5;

/// Configuration of a [`Scheduler`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadConfig {
    /// Blocks in the window after the committed tip.
    pub window_blocks: u32,
    /// Bound of the held bytes plus the reservations of the requests in flight.
    pub memory_budget_bytes: u64,
    /// Size of the largest block. A request reserves this number of bytes.
    pub max_block_bytes: u32,
    /// Requests in flight for each peer, in blocks.
    pub peer_in_flight_blocks: u32,
    /// Requests in flight for each peer, in bytes at the mean size of the recent blocks.
    pub peer_in_flight_bytes: u64,
    /// Blocks that the validator can have before it commits the first of them.
    pub validation_lookahead: u32,
    /// Time without progress after which a peer with a request in flight stalls.
    pub request_timeout_ms: u64,
    /// Time without progress after which the requests of the peer that has the lowest
    /// block move to other peers, when the peer has a measured rate.
    pub rescue_timeout_ms: u64,
    /// Lower bound of the rate of a peer in the time of one largest block.
    pub min_rate_bytes_per_sec: u64,
    /// First wait before a new request for a block that no connected peer supplied. The
    /// wait doubles up to 32 times this value.
    pub backoff_ms: u64,
}

impl Default for DownloadConfig {
    /// The defaults are for blocks of 2 MB. `docs/architecture.md` gives the reasons.
    fn default() -> Self {
        Self {
            window_blocks: 1_024,
            memory_budget_bytes: 1 << 30,
            max_block_bytes: MAX_BLOCK_BYTES as u32,
            peer_in_flight_blocks: 64,
            peer_in_flight_bytes: 8_000_000,
            validation_lookahead: 16,
            request_timeout_ms: 8_000,
            rescue_timeout_ms: 2_000,
            min_rate_bytes_per_sec: 256 * 1024,
            backoff_ms: 1_000,
        }
    }
}

/// What the node tells the scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<P> {
    /// A peer that can supply blocks. `best_height_hint` is the height that the peer
    /// reported in its `version` message.
    PeerConnected {
        peer: P,
        best_height_hint: u32,
    },
    PeerDisconnected {
        peer: P,
    },
    /// A `block` message. `bytes_len` is the length of the serialized block.
    BlockReceived {
        peer: P,
        hash: BlockHash,
        bytes_len: u32,
    },
    /// The block hashes of a `notfound` message.
    NotFound {
        peer: P,
        hashes: Vec<BlockHash>,
    },
    /// The validator committed a delivered block. The block is the first of the window.
    BlockCommitted {
        hash: BlockHash,
    },
    /// The validator refused a held block. When the block is not valid, the node calls
    /// [`HeaderChain::mark_invalid`] before this event. When only the body is wrong (it
    /// does not match the header), the node does not call it, and the scheduler requests
    /// the block from another peer.
    BlockInvalid {
        hash: BlockHash,
    },
    /// The chain of the peer leaves the best header chain after `height`, so the peer
    /// does not have a block of the best chain above it. `None`: the chain of the peer is
    /// on the best chain, as far as the node knows. The event changes no request. After a
    /// change of the best chain the node sends it for each peer before
    /// [`Event::BestHeaderTipChanged`].
    PeerForkPoint {
        peer: P,
        height: Option<u32>,
    },
    /// The time passed. The node sends this event at a regular interval.
    Tick,
    /// The best header chain or a body state of the header chain changed. `committed` is
    /// the newest committed block on the best header chain.
    BestHeaderTipChanged {
        committed: Tip,
    },
}

/// What the node must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action<P> {
    /// Send one `getdata` message for these blocks to the peer.
    Request { peer: P, hashes: Vec<BlockHash> },
    /// Keep the body of the [`Event::BlockReceived`]. Without this action the node drops
    /// the body.
    Store { hash: BlockHash },
    /// Give the body to the validator. The body is a stored body, or a body that the node
    /// had before (the state of the block in the header chain was not
    /// [`Status::HeaderValid`]). `checkpointed`: the block is at or below the last
    /// checkpoint that the best header chain reached.
    Deliver { block: Tip, checkpointed: bool },
    /// Drop a stored body. The block left the best header chain. When the validator has
    /// the block, the node stops its validation.
    Discard { hash: BlockHash },
    /// Record the misbehaviour of the peer.
    Penalize { peer: P, reason: Misbehaviour },
    /// Close the connection of the peer. The scheduler does not use the peer again.
    Disconnect { peer: P },
}

/// An event that does not agree with the state of the scheduler, or a configuration that
/// has no valid window.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadError {
    #[error("configuration: {0}")]
    Config(&'static str),
    #[error("committed block {} at height {} is not on the best header chain", .0.hash, .0.height)]
    CommittedNotOnBestChain(Tip),
    #[error("block {0} is not the delivered block at the start of the window")]
    UnexpectedCommit(BlockHash),
    #[error("block {0} is not a held block of the window")]
    NotHeld(BlockHash),
}

/// Counters of a [`Scheduler`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub committed: Tip,
    /// Blocks in the window.
    pub window_blocks: usize,
    /// Blocks that the validator has and did not commit.
    pub delivered_blocks: usize,
    /// Bytes of the bodies that the node stored for the scheduler.
    pub held_bytes: u64,
    /// Requests in flight.
    pub requested_blocks: u32,
    /// `held_bytes` plus the reservations of the requests in flight. The budget is its
    /// bound.
    pub memory_bytes: u64,
    pub peers: usize,
}

enum SlotState<P> {
    Missing,
    Requested {
        peer: P,
        sent_at: u64,
        /// The time at which the peer answered a later request before this one. From this
        /// time the other bodies of the peer are not progress for this request.
        overtaken_at: Option<u64>,
        /// The `getdata` message of the request.
        message: u64,
    },
    /// `from`: the peer that supplied the body. `None`: the node had the body.
    Held {
        bytes: u32,
        from: Option<P>,
    },
}

struct Slot<P> {
    block: Tip,
    state: SlotState<P>,
    /// Connected peers that did not supply this block.
    tried: Vec<P>,
    /// No request before this time.
    retry_at: u64,
    /// Times that every connected peer failed to supply the block.
    rounds: u32,
}

struct Peer {
    best_height: u32,
    /// The peer has no block of the best chain above this height.
    fork_height: Option<u32>,
    in_flight: u32,
    /// Moving average of the time from the start of a transfer to its end.
    delivery_ms: Option<u64>,
    /// Moving average of the rate of the transfers.
    rate_bytes_per_sec: Option<u64>,
    last_arrival: u64,
    /// No request before this time.
    paused_until: u64,
    /// Requests that the scheduler released and that the peer can still answer.
    late: HashMap<BlockHash, Late>,
    score: PeerScore,
}

/// A released request.
#[derive(Clone, Copy)]
struct Late {
    /// The scheduler forgets the request at this time.
    forget_at: u64,
    /// The time of the request, when the peer stalls without an answer to it.
    owed_since: Option<u64>,
}

/// Result of the search for a peer.
enum Pick<P> {
    Peer(P),
    /// A peer that can supply the block has no free capacity, or no peer is connected.
    Wait,
    /// Each connected peer failed to supply the block.
    Exhausted,
}

/// The block download of one node. `P` identifies a peer. See the module documentation.
pub struct Scheduler<P> {
    config: DownloadConfig,
    committed: Tip,
    window: VecDeque<Slot<P>>,
    /// Hash to height, for the blocks of the window.
    heights: HashMap<BlockHash, u32>,
    /// The validator has the first `delivered` blocks of the window.
    delivered: usize,
    /// No block before this position is missing.
    scan_from: usize,
    held_bytes: u64,
    requested: u32,
    /// The number of `getdata` messages that the scheduler made.
    messages: u64,
    /// For each `getdata` message with a request in flight and an answer: its peer and
    /// the bytes of its answers.
    answered_bytes: HashMap<u64, (P, u64)>,
    /// Moving average of the size of the received blocks.
    mean_block_bytes: u64,
    peers: BTreeMap<P, Peer>,
}

impl<P: Copy + Ord + Debug> Scheduler<P> {
    /// A scheduler for the blocks of the best chain of `chain` after `committed`. This is
    /// also the function for a start after a stop.
    pub fn new(
        config: DownloadConfig,
        chain: &HeaderChain,
        committed: Tip,
    ) -> Result<Self, DownloadError> {
        if config.window_blocks == 0 {
            return Err(DownloadError::Config("window_blocks is zero"));
        }
        if config.validation_lookahead == 0 {
            return Err(DownloadError::Config("validation_lookahead is zero"));
        }
        if config.max_block_bytes == 0 || config.min_rate_bytes_per_sec == 0 {
            return Err(DownloadError::Config(
                "max_block_bytes or min_rate_bytes_per_sec is zero",
            ));
        }
        if config.peer_in_flight_blocks == 0 {
            return Err(DownloadError::Config("peer_in_flight_blocks is zero"));
        }
        if config.memory_budget_bytes < 2 * u64::from(config.max_block_bytes) {
            return Err(DownloadError::Config(
                "memory_budget_bytes is less than two blocks of max_block_bytes",
            ));
        }
        check_committed(chain, committed)?;
        let mut scheduler = Self {
            mean_block_bytes: u64::from(config.max_block_bytes),
            config,
            committed,
            window: VecDeque::new(),
            heights: HashMap::new(),
            delivered: 0,
            scan_from: 0,
            held_bytes: 0,
            requested: 0,
            messages: 0,
            answered_bytes: HashMap::new(),
            peers: BTreeMap::new(),
        };
        scheduler.refill(chain);
        Ok(scheduler)
    }

    /// Applies one event at the time `now_ms` and returns the actions in the order in which
    /// the node must apply them. `now_ms` is a clock in milliseconds that never goes back.
    ///
    /// `chain` is the header chain. After a change of its best chain the node must send
    /// [`Event::BestHeaderTipChanged`] before another event. [`Event::BlockInvalid`] reads
    /// the new best chain itself.
    pub fn handle(
        &mut self,
        event: Event<P>,
        now_ms: u64,
        chain: &HeaderChain,
    ) -> Result<Vec<Action<P>>, DownloadError> {
        let mut actions = Vec::new();
        match event {
            Event::PeerConnected {
                peer,
                best_height_hint,
            } => {
                let entry = self.peers.entry(peer).or_insert_with(|| Peer {
                    best_height: 0,
                    fork_height: None,
                    in_flight: 0,
                    delivery_ms: None,
                    rate_bytes_per_sec: None,
                    last_arrival: now_ms,
                    paused_until: 0,
                    late: HashMap::new(),
                    score: PeerScore::new(now_ms / 1_000),
                });
                entry.best_height = entry.best_height.max(best_height_hint);
            }
            Event::PeerDisconnected { peer } => self.drop_peer(peer),
            Event::BlockReceived {
                peer,
                hash,
                bytes_len,
            } => self.on_block(peer, hash, bytes_len, now_ms, &mut actions),
            Event::NotFound { peer, hashes } => {
                for hash in hashes {
                    self.on_not_found(peer, hash);
                }
            }
            Event::BlockCommitted { hash } => self.on_committed(hash, chain)?,
            Event::BlockInvalid { hash } => self.on_invalid(hash, chain, now_ms, &mut actions)?,
            Event::PeerForkPoint { peer, height } => {
                if let Some(entry) = self.peers.get_mut(&peer) {
                    entry.fork_height = height;
                }
                return Ok(actions);
            }
            Event::Tick => self.expire(now_ms, &mut actions),
            Event::BestHeaderTipChanged { committed } => {
                self.resync(committed, chain, now_ms, None, &mut actions)?;
            }
        }
        self.deliver(chain, &mut actions);
        self.schedule(now_ms, &mut actions);
        Ok(actions)
    }

    /// The lowest block of the window that the node does not have, when each connected
    /// peer that can have it failed to supply it since the last call of
    /// [`Event::BestHeaderTipChanged`] or the last body of the block.
    pub fn withheld(&self) -> Option<Tip> {
        let slot = self
            .window
            .iter()
            .find(|slot| !matches!(slot.state, SlotState::Held { .. }))?;
        match (&slot.state, slot.rounds) {
            (SlotState::Missing, 1..) => Some(slot.block),
            _ => None,
        }
    }

    pub fn stats(&self) -> Stats {
        Stats {
            committed: self.committed,
            window_blocks: self.window.len(),
            delivered_blocks: self.delivered,
            held_bytes: self.held_bytes,
            requested_blocks: self.requested,
            memory_bytes: self.memory_bytes(),
            peers: self.peers.len(),
        }
    }

    /// The active requests: the block and the peer, in height order.
    pub fn in_flight(&self) -> impl Iterator<Item = (Tip, P)> + '_ {
        self.window.iter().filter_map(|slot| match slot.state {
            SlotState::Requested { peer, .. } => Some((slot.block, peer)),
            _ => None,
        })
    }

    fn memory_bytes(&self) -> u64 {
        self.held_bytes + u64::from(self.requested) * u64::from(self.config.max_block_bytes)
    }

    fn position(&self, hash: &BlockHash) -> Option<usize> {
        let height = *self.heights.get(hash)?;
        Some((height - self.committed.height - 1) as usize)
    }

    /// Adds the next blocks of the best chain at the end of the window.
    fn refill(&mut self, chain: &HeaderChain) {
        let next = self.committed.height + 1 + self.window.len() as u32;
        let room = (self.config.window_blocks as usize).saturating_sub(self.window.len());
        for block in chain.best_chain_from(next).take(room) {
            let state = match node_has_body(chain, &block.hash) {
                true => SlotState::Held {
                    bytes: 0,
                    from: None,
                },
                false => SlotState::Missing,
            };
            self.heights.insert(block.hash, block.height);
            self.window.push_back(Slot {
                block,
                state,
                tried: Vec::new(),
                retry_at: 0,
                rounds: 0,
            });
        }
    }

    /// Removes the blocks of the window from the position `keep`. They left the best chain.
    /// The function gives no [`Action::Discard`] for the block `skip`.
    fn truncate(
        &mut self,
        keep: usize,
        now: u64,
        skip: Option<BlockHash>,
        actions: &mut Vec<Action<P>>,
    ) {
        let late = self.late(now);
        for slot in self.window.drain(keep..) {
            self.heights.remove(&slot.block.hash);
            match slot.state {
                SlotState::Missing => {}
                SlotState::Requested { peer, .. } => {
                    // A `getdata` message has no cancel. The body can still arrive.
                    self.requested -= 1;
                    if let Some(entry) = self.peers.get_mut(&peer) {
                        entry.in_flight -= 1;
                        entry.late.insert(slot.block.hash, late);
                    }
                }
                SlotState::Held { bytes, from } => {
                    self.held_bytes -= u64::from(bytes);
                    if let (Some(_), true) = (from, skip != Some(slot.block.hash)) {
                        actions.push(Action::Discard {
                            hash: slot.block.hash,
                        });
                    }
                }
            }
        }
        self.delivered = self.delivered.min(keep);
        self.scan_from = self.scan_from.min(keep);
    }

    /// Makes the window equal to the best chain after `committed`. The blocks that are on
    /// the best chain keep their state, so the node uses their bodies again.
    fn resync(
        &mut self,
        committed: Tip,
        chain: &HeaderChain,
        now: u64,
        skip: Option<BlockHash>,
        actions: &mut Vec<Action<P>>,
    ) -> Result<(), DownloadError> {
        check_committed(chain, committed)?;
        let keep = if committed == self.committed {
            let mut best = chain.best_chain_from(committed.height + 1);
            self.window
                .iter()
                .take_while(|slot| best.next() == Some(slot.block))
                .count()
        } else {
            // The window was on a committed block that left the best chain.
            0
        };
        self.truncate(keep, now, skip, actions);
        self.committed = committed;
        self.refill(chain);
        // The node can get a body from another source, for example the relay of new blocks.
        let late = self.late(now);
        for slot in self.window.iter_mut() {
            let peer = match slot.state {
                SlotState::Held { .. } => continue,
                SlotState::Missing => None,
                SlotState::Requested { peer, .. } => Some(peer),
            };
            if !node_has_body(chain, &slot.block.hash) {
                continue;
            }
            if let Some(peer) = peer {
                self.requested -= 1;
                if let Some(entry) = self.peers.get_mut(&peer) {
                    entry.in_flight -= 1;
                    entry.late.insert(slot.block.hash, late);
                }
            }
            slot.state = SlotState::Held {
                bytes: 0,
                from: None,
            };
        }
        Ok(())
    }

    /// The record of a request that the scheduler releases at `now` and that the peer does
    /// not have to answer.
    fn late(&self, now: u64) -> Late {
        Late {
            forget_at: now + LATE_REQUEST_TIMEOUTS * self.config.request_timeout_ms,
            owed_since: None,
        }
    }

    fn on_committed(&mut self, hash: BlockHash, chain: &HeaderChain) -> Result<(), DownloadError> {
        let bytes = match self.window.front() {
            Some(Slot {
                block,
                state: SlotState::Held { bytes, .. },
                ..
            }) if block.hash == hash && self.delivered > 0 => *bytes,
            _ => return Err(DownloadError::UnexpectedCommit(hash)),
        };
        let Some(slot) = self.window.pop_front() else {
            unreachable!("the window has the committed block");
        };
        self.heights.remove(&hash);
        self.held_bytes -= u64::from(bytes);
        self.committed = slot.block;
        self.delivered -= 1;
        self.scan_from = self.scan_from.saturating_sub(1);
        self.refill(chain);
        Ok(())
    }

    fn on_invalid(
        &mut self,
        hash: BlockHash,
        chain: &HeaderChain,
        now: u64,
        actions: &mut Vec<Action<P>>,
    ) -> Result<(), DownloadError> {
        let Some(at) = self.position(&hash) else {
            return Err(DownloadError::NotHeld(hash));
        };
        let SlotState::Held { bytes, from } = self.window[at].state else {
            return Err(DownloadError::NotHeld(hash));
        };
        if let Some(peer) = from {
            self.penalize(peer, Misbehaviour::InvalidBlock, now, actions);
        }
        match chain.entry(&hash).map(|entry| entry.status) {
            // The block is not valid. The best chain does not have it.
            None | Some(Status::Invalid) => {
                self.resync(self.committed, chain, now, Some(hash), actions)
            }
            // The body is wrong and the header stays. The validator gets this block and the
            // blocks after it again.
            Some(_) => {
                self.held_bytes -= u64::from(bytes);
                let slot = &mut self.window[at];
                slot.state = SlotState::Missing;
                slot.tried.extend(from);
                self.delivered = self.delivered.min(at);
                self.scan_from = self.scan_from.min(at);
                Ok(())
            }
        }
    }

    fn on_block(
        &mut self,
        peer: P,
        hash: BlockHash,
        bytes_len: u32,
        now: u64,
        actions: &mut Vec<Action<P>>,
    ) {
        // The peer left before the node handled its message. The node drops the body.
        let Some(entry) = self.peers.get_mut(&peer) else {
            return;
        };
        let late = entry.late.remove(&hash);
        let at = self.position(&hash);
        let sent_at = match at.map(|at| &self.window[at].state) {
            Some(SlotState::Requested {
                peer: asked,
                sent_at,
                ..
            }) if *asked == peer => Some(*sent_at),
            _ => None,
        };
        let (None, None) = (sent_at, late) else {
            return self.on_answer(peer, at, sent_at, bytes_len, now, actions);
        };
        self.penalize(peer, Misbehaviour::Unsolicited, now, actions);
    }

    /// A body that answers an active request (`sent_at`) or a released request.
    fn on_answer(
        &mut self,
        peer: P,
        at: Option<usize>,
        sent_at: Option<u64>,
        bytes_len: u32,
        now: u64,
        actions: &mut Vec<Action<P>>,
    ) {
        let max_block_bytes = self.config.max_block_bytes;
        let budget = self.config.memory_budget_bytes;
        let late = self.late(now);
        let memory = self.memory_bytes();
        let Some(entry) = self.peers.get_mut(&peer) else {
            unreachable!("the caller found the peer");
        };
        // The peer answers, so it gets requests again.
        entry.paused_until = 0;
        let start = entry.last_arrival;
        entry.last_arrival = now;
        let answer = match sent_at {
            Some(sent_at) => {
                entry.in_flight -= 1;
                // The transfers of one peer are in sequence on one connection.
                let transfer_ms = (now - start.max(sent_at)).max(1);
                let rate = u64::from(bytes_len) * 1_000 / transfer_ms;
                entry.delivery_ms = Some(average(entry.delivery_ms, transfer_ms));
                entry.rate_bytes_per_sec = Some(average(entry.rate_bytes_per_sec, rate));
                true
            }
            None => false,
        };
        let Some(at) = at else {
            // A late body of a block that left the window.
            return;
        };
        if let Some(answered) = sent_at {
            let SlotState::Requested { message, .. } = self.window[at].state else {
                unreachable!("the caller found the request");
            };
            self.requested -= 1;
            self.window[at].state = SlotState::Missing;
            self.scan_from = self.scan_from.min(at);
            self.mark_overtaken(peer, at, answered, now);
            self.count_answer(peer, message, bytes_len);
        }
        let slot = &mut self.window[at];
        if bytes_len > max_block_bytes {
            slot.tried.push(peer);
            return self.penalize(peer, Misbehaviour::Malformed, now, actions);
        }
        match slot.state {
            // A late body of a block that the node has.
            SlotState::Held { .. } => return,
            // A late body takes the place of the request to another peer. That request
            // becomes a released request.
            SlotState::Requested { peer: other, .. } => {
                self.requested -= 1;
                if let Some(other) = self.peers.get_mut(&other) {
                    other.in_flight -= 1;
                    other.late.insert(slot.block.hash, late);
                }
            }
            // A late body has no reservation. An answer had its reservation until now.
            SlotState::Missing => {
                if !answer && memory + u64::from(bytes_len) > budget {
                    return;
                }
            }
        }
        slot.state = SlotState::Held {
            bytes: bytes_len,
            from: Some(peer),
        };
        let (hash, height) = (slot.block.hash, slot.block.height);
        self.held_bytes += u64::from(bytes_len);
        self.mean_block_bytes = (self.mean_block_bytes * 15 + u64::from(bytes_len)) / 16;
        if let Some(entry) = self.peers.get_mut(&peer) {
            entry.best_height = entry.best_height.max(height);
        }
        actions.push(Action::Store { hash });
    }

    /// The peer sent `bytes_len` bytes as an answer to a request of the `getdata` message
    /// `message`.
    fn count_answer(&mut self, peer: P, message: u64, bytes_len: u32) {
        let open = self.window.iter().any(
            |slot| matches!(slot.state, SlotState::Requested { message: m, .. } if m == message),
        );
        if !open {
            self.answered_bytes.remove(&message);
            return;
        }
        let (_, total) = self.answered_bytes.entry(message).or_insert((peer, 0));
        *total += u64::from(bytes_len);
    }

    /// Blocks of the next `getdata` message, at most: the blocks before the last one must
    /// stay below [`GETDATA_ANSWER_BYTES`] at two times the mean size of the recent
    /// blocks. Then a peer with the answer limits answers each request of the message.
    fn message_blocks(&self) -> usize {
        let before_last = (GETDATA_ANSWER_BYTES - 1) / (2 * self.mean_block_bytes).max(1);
        (1 + before_last).min(GETDATA_MAX_BLOCKS as u64) as usize
    }

    /// Frees the requests that a peer with the answer limits does not answer: the answers
    /// to their `getdata` message reached [`GETDATA_ANSWER_BYTES`], and the peer sent no
    /// body for the request timeout. The peer gets no penalty, and the scheduler sends the
    /// requests in a new message.
    fn free_unanswered(&mut self, now: u64) {
        if self.answered_bytes.is_empty() {
            return;
        }
        // A stall, a rescue or a reorg can end the requests of a message.
        let open: std::collections::HashSet<u64> = self
            .window
            .iter()
            .filter_map(|slot| match slot.state {
                SlotState::Requested { message, .. } => Some(message),
                _ => None,
            })
            .collect();
        self.answered_bytes
            .retain(|message, _| open.contains(message));
        let late = self.late(now);
        let timeout = self.config.request_timeout_ms;
        let peers = &self.peers;
        let silent: Vec<u64> = self
            .answered_bytes
            .iter()
            .filter(|(_, (peer, total))| {
                *total >= GETDATA_ANSWER_BYTES
                    && matches!(peers.get(peer), Some(entry) if now >= entry.last_arrival + timeout)
            })
            .map(|(message, _)| *message)
            .collect();
        for message in silent {
            let Some((peer, _)) = self.answered_bytes.remove(&message) else {
                unreachable!("the message is in the map");
            };
            let Some(entry) = self.peers.get_mut(&peer) else {
                unreachable!("the filter found the peer");
            };
            for (at, slot) in self.window.iter_mut().enumerate() {
                if !matches!(slot.state, SlotState::Requested { message: m, .. } if m == message) {
                    continue;
                }
                slot.state = SlotState::Missing;
                entry.in_flight -= 1;
                entry.late.insert(slot.block.hash, late);
                self.requested -= 1;
                self.scan_from = self.scan_from.min(at);
            }
        }
    }

    /// The peer answered the request of the time `answered` for the block at the position
    /// `at`. Its requests that are before this one in the order of an honest peer (an
    /// earlier message, or a lower height in the same message) are overtaken.
    fn mark_overtaken(&mut self, peer: P, at: usize, answered: u64, now: u64) {
        for (other, slot) in self.window.iter_mut().enumerate() {
            match &mut slot.state {
                SlotState::Requested {
                    peer: asked,
                    sent_at,
                    overtaken_at: overtaken_at @ None,
                    ..
                } if *asked == peer
                    && (*sent_at < answered || (*sent_at == answered && other < at)) =>
                {
                    *overtaken_at = Some(now);
                }
                _ => {}
            }
        }
    }

    fn on_not_found(&mut self, peer: P, hash: BlockHash) {
        let at = self.position(&hash);
        let Some(entry) = self.peers.get_mut(&peer) else {
            return;
        };
        entry.late.remove(&hash);
        let Some(at) = at else {
            return;
        };
        let slot = &mut self.window[at];
        match slot.state {
            SlotState::Requested { peer: asked, .. } if asked == peer => {}
            _ => return,
        }
        entry.in_flight -= 1;
        self.requested -= 1;
        slot.state = SlotState::Missing;
        slot.tried.push(peer);
        self.scan_from = self.scan_from.min(at);
    }

    /// Removes the peer. Its requests become missing blocks.
    fn drop_peer(&mut self, peer: P) {
        let Some(_) = self.peers.remove(&peer) else {
            return;
        };
        self.release(peer, false);
        for slot in self.window.iter_mut() {
            slot.tried.retain(|tried| *tried != peer);
        }
    }

    /// Makes the requests of the peer missing blocks. Returns the hash and the time of each
    /// request.
    fn release(&mut self, peer: P, tried: bool) -> Vec<(BlockHash, u64)> {
        let mut released = Vec::new();
        for (at, slot) in self.window.iter_mut().enumerate() {
            let sent_at = match slot.state {
                SlotState::Requested {
                    peer: asked,
                    sent_at,
                    ..
                } if asked == peer => sent_at,
                _ => continue,
            };
            slot.state = SlotState::Missing;
            if tried {
                slot.tried.push(peer);
            }
            self.requested -= 1;
            self.scan_from = self.scan_from.min(at);
            released.push((slot.block.hash, sent_at));
        }
        released
    }

    /// Records the misbehaviour. A verdict to disconnect or to ban removes the peer.
    fn penalize(&mut self, peer: P, reason: Misbehaviour, now: u64, actions: &mut Vec<Action<P>>) {
        actions.push(Action::Penalize { peer, reason });
        // A peer that left has no score here. The node records the reason.
        let Some(entry) = self.peers.get_mut(&peer) else {
            return;
        };
        match entry.score.record(reason, now / 1_000) {
            Verdict::Keep => {}
            Verdict::Disconnect | Verdict::Ban => {
                actions.push(Action::Disconnect { peer });
                self.drop_peer(peer);
            }
        }
    }

    /// Time of one largest block at the rate of the peer.
    fn largest_block_ms(&self, peer: &Peer) -> u64 {
        let rate = peer
            .rate_bytes_per_sec
            .map_or(self.config.min_rate_bytes_per_sec, |rate| {
                rate.max(self.config.min_rate_bytes_per_sec)
            });
        u64::from(self.config.max_block_bytes) * 1_000 / rate
    }

    /// Finds the peers that make no progress and moves their requests.
    fn expire(&mut self, now: u64, actions: &mut Vec<Action<P>>) {
        self.free_unanswered(now);
        // For each peer: the oldest request without an answer, and whether a request is
        // after its last time.
        let mut owed: BTreeMap<P, (u64, bool)> = BTreeMap::new();
        let mut note = |peer: P, sent_at: u64, overdue: bool| {
            let entry = owed.entry(peer).or_insert((sent_at, false));
            entry.0 = entry.0.min(sent_at);
            entry.1 |= overdue;
        };
        for (key, peer) in self.peers.iter_mut() {
            for late in peer.late.values() {
                if let Some(sent_at) = late.owed_since {
                    note(*key, sent_at, late.forget_at <= now);
                }
            }
            peer.late.retain(|_, late| late.forget_at > now);
        }
        let lowest = self
            .window
            .iter()
            .position(|slot| !matches!(slot.state, SlotState::Held { .. }));
        let mut lowest_request = None;
        for (at, slot) in self.window.iter().enumerate() {
            let SlotState::Requested {
                peer,
                sent_at,
                overtaken_at,
                ..
            } = slot.state
            else {
                continue;
            };
            let entry = &self.peers[&peer];
            let limit = self.config.request_timeout_ms + self.largest_block_ms(entry);
            let overdue = matches!(overtaken_at, Some(since) if now >= since + limit);
            note(peer, sent_at, overdue);
            if Some(at) == lowest {
                let progress = overtaken_at.unwrap_or(sent_at.max(entry.last_arrival));
                lowest_request = Some((peer, progress, &slot.tried));
            }
        }
        let mut stalled = Vec::new();
        for (peer, (sent_at, overdue)) in owed {
            let entry = &self.peers[&peer];
            let limit = self.config.request_timeout_ms + self.largest_block_ms(entry);
            if overdue || now >= sent_at.max(entry.last_arrival) + limit {
                stalled.push(peer);
            }
        }
        let rescued = lowest_request.and_then(|(peer, progress, tried)| {
            let entry = &self.peers[&peer];
            let _ = entry.rate_bytes_per_sec?;
            let limit = self.config.rescue_timeout_ms + self.largest_block_ms(entry);
            let other = self.peers.iter().any(|(key, other)| {
                *key != peer && other.paused_until <= now && !tried.contains(key)
            });
            let rescue = other && !stalled.contains(&peer) && now >= progress + limit;
            rescue.then_some(peer)
        });
        let late = self.late(now);
        for (peer, stalls) in stalled
            .into_iter()
            .map(|peer| (peer, true))
            .chain(rescued.map(|peer| (peer, false)))
        {
            let released = self.release(peer, true);
            let Some(entry) = self.peers.get_mut(&peer) else {
                unreachable!("a request names a connected peer");
            };
            entry.in_flight = 0;
            entry.paused_until = now + self.config.request_timeout_ms;
            if stalls {
                // The penalty covers each request that the peer did not answer.
                for late in entry.late.values_mut() {
                    late.owed_since = None;
                }
            }
            for (hash, sent_at) in released {
                let owed_since = (!stalls).then_some(sent_at);
                entry.late.insert(hash, Late { owed_since, ..late });
            }
            if stalls {
                self.penalize(peer, Misbehaviour::Stall, now, actions);
            }
        }
    }

    /// Gives the held blocks at the start of the window to the validator, in height order.
    fn deliver(&mut self, chain: &HeaderChain, actions: &mut Vec<Action<P>>) {
        let checkpoint = chain.last_checkpoint_reached();
        while self.delivered < self.config.validation_lookahead as usize {
            let Some(Slot {
                block,
                state: SlotState::Held { .. },
                ..
            }) = self.window.get(self.delivered)
            else {
                break;
            };
            actions.push(Action::Deliver {
                block: *block,
                checkpointed: matches!(checkpoint, Some(height) if block.height <= height),
            });
            self.delivered += 1;
        }
    }

    /// Requests in flight that the peer can have.
    fn capacity(&self, peer: &Peer) -> u32 {
        let by_bytes = self.config.peer_in_flight_bytes / self.mean_block_bytes.max(1);
        let limit = by_bytes.clamp(1, u64::from(self.config.peer_in_flight_blocks)) as u32;
        match peer.delivery_ms {
            Some(_) => limit,
            None => limit.min(UNMEASURED_PEER_BLOCKS),
        }
    }

    fn is_free(&self, peer: &Peer, now: u64) -> bool {
        peer.paused_until <= now && peer.in_flight < self.capacity(peer)
    }

    /// The median delivery time of the peers that delivered a block. 0: no peer did.
    fn median_delivery_ms(&self) -> u64 {
        let mut measured: Vec<u64> = self
            .peers
            .values()
            .filter_map(|peer| peer.delivery_ms)
            .collect();
        measured.sort_unstable();
        measured.get(measured.len() / 2).copied().unwrap_or(0)
    }

    /// The peer for the block at the position `at`.
    fn pick(&self, at: usize, now: u64) -> Pick<P> {
        let slot = &self.window[at];
        let height = slot.block.height;
        // A peer whose chain left the best chain below the block does not have it.
        let untried = || {
            self.peers.iter().filter(|(key, peer)| {
                !slot.tried.contains(key)
                    && !matches!(peer.fork_height, Some(fork) if fork < height)
            })
        };
        if untried().count() == 0 {
            return match self.peers.is_empty() {
                true => Pick::Wait,
                false => Pick::Exhausted,
            };
        }
        // A peer whose reported height is below the block gets the request only when no
        // other peer reported the height.
        let reported = untried().any(|(_, peer)| peer.best_height >= height);
        // A peer without a delivery has the median time of the measured peers. With the
        // time 0 a new peer gets the lowest blocks before each measured peer.
        let unmeasured = self.median_delivery_ms();
        let best = untried()
            .filter(|(_, peer)| !reported || peer.best_height >= height)
            .filter(|(_, peer)| self.is_free(peer, now))
            .min_by_key(|(_, peer)| {
                let queue = u64::from(peer.in_flight) + 1;
                (
                    queue * peer.delivery_ms.unwrap_or(unmeasured),
                    peer.in_flight,
                )
            });
        match best {
            Some((key, _)) => Pick::Peer(*key),
            None => Pick::Wait,
        }
    }

    /// Assigns the missing blocks to the peers, in height order.
    fn schedule(&mut self, now: u64, actions: &mut Vec<Action<P>>) {
        let max_block_bytes = u64::from(self.config.max_block_bytes);
        let mut requests: BTreeMap<P, (u64, Vec<BlockHash>)> = BTreeMap::new();
        let message_blocks = self.message_blocks();
        let mut no_missing_before = true;
        for at in self.scan_from..self.window.len() {
            let SlotState::Missing = self.window[at].state else {
                if no_missing_before {
                    self.scan_from = at + 1;
                }
                continue;
            };
            no_missing_before = false;
            if !self.peers.values().any(|peer| self.is_free(peer, now)) {
                break;
            }
            // The first block of the window can use the last largest block of the budget.
            let limit = match at {
                0 => self.config.memory_budget_bytes,
                _ => self.config.memory_budget_bytes - max_block_bytes,
            };
            if self.memory_bytes() + max_block_bytes > limit {
                break;
            }
            if self.window[at].retry_at > now {
                continue;
            }
            match self.pick(at, now) {
                Pick::Wait => {}
                Pick::Exhausted => {
                    let slot = &mut self.window[at];
                    slot.tried.clear();
                    slot.retry_at =
                        now + (self.config.backoff_ms << slot.rounds.min(BACKOFF_MAX_DOUBLINGS));
                    slot.rounds += 1;
                }
                Pick::Peer(peer) => {
                    let Some(entry) = self.peers.get_mut(&peer) else {
                        unreachable!("the search returns a connected peer");
                    };
                    entry.in_flight += 1;
                    let slot = &mut self.window[at];
                    let message = match requests.get_mut(&peer) {
                        Some((_, hashes)) if hashes.len() >= message_blocks => {
                            let hashes = std::mem::take(hashes);
                            actions.push(Action::Request { peer, hashes });
                            None
                        }
                        Some((message, _)) => Some(*message),
                        None => None,
                    };
                    let message = message.unwrap_or_else(|| {
                        self.messages += 1;
                        requests.insert(peer, (self.messages, Vec::new()));
                        self.messages
                    });
                    slot.state = SlotState::Requested {
                        peer,
                        sent_at: now,
                        overtaken_at: None,
                        message,
                    };
                    if let Some((_, hashes)) = requests.get_mut(&peer) {
                        hashes.push(slot.block.hash);
                    }
                    self.requested += 1;
                }
            }
        }
        actions.extend(
            requests
                .into_iter()
                .map(|(peer, (_, hashes))| Action::Request { peer, hashes }),
        );
    }
}

/// The committed block must be on the best chain.
fn check_committed(chain: &HeaderChain, committed: Tip) -> Result<(), DownloadError> {
    match chain.entry(&committed.hash) {
        Some(entry) if entry.on_best_chain && entry.height == committed.height => Ok(()),
        _ => Err(DownloadError::CommittedNotOnBestChain(committed)),
    }
}

/// Whether the node has the body of a block of the best chain.
fn node_has_body(chain: &HeaderChain, hash: &BlockHash) -> bool {
    match chain.entry(hash).map(|entry| entry.status) {
        Some(Status::HeaderValid) => false,
        Some(Status::BodyKnown | Status::BodyValid) => true,
        Some(Status::Invalid) | None => {
            unreachable!("a block of the best chain is in the header chain and is not invalid")
        }
    }
}

/// Moving average with the weight 1/4 for the new sample.
fn average(old: Option<u64>, sample: u64) -> u64 {
    match old {
        Some(old) => (old * 3 + sample) / 4,
        None => sample,
    }
}
