//! The block download scheduler against simulated peers.
//!
//! The simulation has its own clock in milliseconds and no network. A simulated peer has a
//! round-trip time, a rate and a behaviour. It sends the bodies of one connection in
//! sequence. A simulated validator commits the delivered blocks in order, each after a fixed
//! time. The simulation checks the invariants of the scheduler after each event.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use hayai_consensus::{Checkpoints, HeaderRuleError, Network};
use hayai_sync::download::{Action, DownloadConfig, DownloadError, Event, Scheduler};
use hayai_sync::headers::{
    BestTipChange, ChainConfig, HeaderChain, HeaderContextView, HeaderRules, Tip,
};
use hayai_sync::score::Misbehaviour;
use hayai_wire::header::{BlockHash, BlockHeader, PowParams};
use proptest::prelude::*;

/// The Regtest limit: work 17 for each block.
const EASY: u32 = 0x200f_0f0f;
/// Work 32 for each block.
const HARD: u32 = 0x2007_ffff;
const NOW: u32 = 2_000_000_000;
const TICK_MS: u64 = 100;
/// A simulation that is not complete at this time has a fault.
const LIMIT_MS: u64 = 6 * 60 * 60 * 1_000;

struct Permissive;

impl HeaderRules for Permissive {
    fn check(&self, _: &BlockHeader, _: &HeaderContextView<'_>) -> Result<(), HeaderRuleError> {
        Ok(())
    }
}

fn genesis() -> Tip {
    Tip {
        height: 0,
        hash: Network::Regtest.params().genesis_hash,
    }
}

/// `n` headers in a row on `prev`. `salt` makes the hashes unique.
fn branch(prev: BlockHash, n: usize, bits: u32, salt: u64) -> Vec<BlockHeader> {
    let mut headers: Vec<BlockHeader> = Vec::with_capacity(n);
    let mut prev = prev;
    for i in 0..n as u64 {
        let mut merkle_root = [0u8; 32];
        merkle_root[..8].copy_from_slice(&(salt + i).to_le_bytes());
        let next = BlockHeader {
            version: 4,
            prev_hash: prev,
            merkle_root,
            block_commitments: [0; 32],
            time: (salt + i) as u32,
            bits,
            nonce: [0; 32],
            solution: vec![0; PowParams::REGTEST.solution_len()],
        };
        prev = next.hash();
        headers.push(next);
    }
    headers
}

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value from `low` to `high`, the two included.
    fn range(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low + 1)
    }
}

/// Sizes of the simulated blocks in bytes, the two bounds included.
#[derive(Clone, Copy)]
struct Sizes(u64, u64);

const SMALL: Sizes = Sizes(1_500, 20_000);
const FULL: Sizes = Sizes(1_500_000, 2_000_000);

#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    Honest,
    /// Answers this number of requests, then answers no more.
    StallAfter(u32),
    /// Sends one block that the node did not request with each answered `getdata`.
    Unsolicited,
    /// Sends a body that does not match its header for the block at this height.
    WrongBody(u32),
    /// After this number of answers the rate of the peer is 300 kB/s.
    SlowAfter(u32),
    /// Answers each request but the one for the block at this height.
    Withhold(u32),
    /// The limits of Zebra and Zakura for one `getdata` message: the peer answers the
    /// requests in order until it sent 16 blocks or 1 MB, and never answers the others.
    AnswerLimits,
}

#[derive(Clone, Copy)]
struct Profile {
    rtt_ms: u64,
    bytes_per_sec: u64,
    /// The height that the peer reports.
    reported_height: u32,
    /// The peer answers `notfound` above this height.
    has_up_to: u32,
    behaviour: Behaviour,
}

impl Profile {
    fn honest(rtt_ms: u64, bytes_per_sec: u64) -> Self {
        Self {
            rtt_ms,
            bytes_per_sec,
            reported_height: u32::MAX,
            has_up_to: u32::MAX,
            behaviour: Behaviour::Honest,
        }
    }

    fn with(mut self, behaviour: Behaviour) -> Self {
        self.behaviour = behaviour;
        self
    }
}

struct SimPeer {
    profile: Profile,
    connected: bool,
    /// The connection sends the bodies in sequence.
    busy_until: u64,
    answered: u32,
    requests: u32,
}

enum SimEvent {
    Tick,
    Arrive { peer: u32, hash: BlockHash },
    NotFound { peer: u32, hash: BlockHash },
    ValidationDone { hash: BlockHash },
}

struct Sim {
    _dir: tempfile::TempDir,
    chain: HeaderChain,
    config: DownloadConfig,
    scheduler: Scheduler<u32>,
    now: u64,
    seq: u64,
    queue: BTreeMap<(u64, u64), SimEvent>,
    rng: Rng,
    sizes: Sizes,
    /// Height and size of each generated block.
    blocks: HashMap<BlockHash, (u32, u32)>,
    peers: BTreeMap<u32, SimPeer>,
    /// The bodies that the node holds for the scheduler.
    stored: HashMap<BlockHash, u32>,
    /// Stored bodies that do not match their header.
    wrong_bodies: HashSet<BlockHash>,
    /// Blocks that the validator refuses.
    invalid_blocks: HashSet<BlockHash>,
    validate_ms: u64,
    validator_free_at: u64,
    /// Delivered blocks that are not committed, in order.
    pending: VecDeque<Tip>,
    committed: Tip,
    // Records for the assertions.
    delivered: Vec<(Tip, bool)>,
    penalties: Vec<(u32, Misbehaviour)>,
    disconnects: Vec<u32>,
    discards: usize,
    /// Bodies of answered requests that the scheduler did not store.
    late_duplicates: usize,
    /// Stored bodies that were not the answer to an active request.
    late_used: usize,
    out_of_order_arrivals: usize,
    highest_arrival: u32,
    peak_memory: u64,
    peak_held: u64,
    peak_in_flight: u32,
    committed_bytes: u64,
}

impl Sim {
    fn new(chain_config: ChainConfig, config: DownloadConfig, sizes: Sizes, seed: u64) -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("scratch dir");
        let (chain, _) = HeaderChain::open(chain_config, &dir.path().join("headers.log")).unwrap();
        let scheduler = Scheduler::new(config.clone(), &chain, genesis()).unwrap();
        let mut sim = Self {
            _dir: dir,
            chain,
            config,
            scheduler,
            now: 0,
            seq: 0,
            queue: BTreeMap::new(),
            rng: Rng(seed),
            sizes,
            blocks: HashMap::new(),
            peers: BTreeMap::new(),
            stored: HashMap::new(),
            wrong_bodies: HashSet::new(),
            invalid_blocks: HashSet::new(),
            validate_ms: 0,
            validator_free_at: 0,
            pending: VecDeque::new(),
            committed: genesis(),
            delivered: Vec::new(),
            penalties: Vec::new(),
            disconnects: Vec::new(),
            discards: 0,
            late_duplicates: 0,
            late_used: 0,
            out_of_order_arrivals: 0,
            highest_arrival: 0,
            peak_memory: 0,
            peak_held: 0,
            peak_in_flight: 0,
            committed_bytes: 0,
        };
        sim.push(TICK_MS, SimEvent::Tick);
        sim
    }

    fn regtest(config: DownloadConfig, sizes: Sizes, seed: u64) -> Self {
        Self::new(ChainConfig::new(Network::Regtest), config, sizes, seed)
    }

    fn push(&mut self, delay: u64, event: SimEvent) {
        self.seq += 1;
        self.queue.insert((self.now + delay, self.seq), event);
    }

    /// Adds headers to the header chain and tells the scheduler when the best chain changed.
    fn headers(&mut self, headers: &[BlockHeader], first_height: u32) -> Option<BestTipChange> {
        for (i, header) in headers.iter().enumerate() {
            let size = self.rng.range(self.sizes.0, self.sizes.1) as u32;
            self.blocks
                .insert(header.hash(), (first_height + i as u32, size));
        }
        let mut change: Option<BestTipChange> = None;
        for batch in headers.chunks(160) {
            let accepted = self.chain.accept_headers(batch, &Permissive, NOW).unwrap();
            if let Some(new) = accepted.tip_change {
                let fork_point = match change {
                    Some(old) if old.fork_point.height < new.fork_point.height => old.fork_point,
                    _ => new.fork_point,
                };
                change = Some(BestTipChange { fork_point, ..new });
            }
        }
        if let Some(change) = change {
            if change.fork_point.height < self.committed.height {
                // The node removes the committed blocks that left the best chain.
                self.committed = change.fork_point;
                self.pending.clear();
            }
            let committed = self.committed;
            self.event(Event::BestHeaderTipChanged { committed });
        }
        change
    }

    /// A chain of `n` generated blocks on the genesis block.
    fn main_chain(&mut self, n: usize) -> Vec<BlockHeader> {
        let headers = branch(genesis().hash, n, EASY, 1);
        self.headers(&headers, 1);
        headers
    }

    fn connect(&mut self, id: u32, profile: Profile) {
        self.peers.insert(
            id,
            SimPeer {
                profile,
                connected: true,
                busy_until: 0,
                answered: 0,
                requests: 0,
            },
        );
        self.event(Event::PeerConnected {
            peer: id,
            best_height_hint: profile.reported_height,
        });
    }

    fn disconnect(&mut self, id: u32) {
        self.peers.get_mut(&id).unwrap().connected = false;
        self.event(Event::PeerDisconnected { peer: id });
    }

    /// Gives one event to the scheduler, applies the actions and checks the invariants.
    fn event(&mut self, event: Event<u32>) {
        let received = match &event {
            Event::BlockReceived { hash, .. } => Some(*hash),
            _ => None,
        };
        let actions = self
            .scheduler
            .handle(event, self.now, &self.chain)
            .expect("the event agrees with the state");
        let mut left = Vec::new();
        for action in actions {
            if let Action::Disconnect { peer } = action {
                left.push(peer);
            }
            self.apply(action, received);
        }
        self.check();
        // The node closes the connections, and the scheduler gets the events.
        for peer in left {
            self.event(Event::PeerDisconnected { peer });
        }
    }

    fn apply(&mut self, action: Action<u32>, received: Option<BlockHash>) {
        match action {
            Action::Request { peer, hashes } => {
                assert!(!hashes.is_empty());
                assert!(hashes.len() <= 16, "one getdata has at most 16 blocks");
                let entry = self.peers.get_mut(&peer).unwrap();
                entry.requests += hashes.len() as u32;
                let limits = entry.profile.behaviour == Behaviour::AnswerLimits;
                let mut sent = 0;
                for hash in hashes {
                    if limits && sent >= 1_000_000 {
                        break;
                    }
                    sent += u64::from(self.blocks[&hash].1);
                    self.request(peer, hash);
                }
            }
            Action::Store { hash } => {
                assert_eq!(received, Some(hash), "only the received body is stored");
                let (_, size) = self.blocks[&hash];
                let None = self.stored.insert(hash, size) else {
                    panic!("a body is stored twice");
                };
            }
            Action::Deliver {
                block,
                checkpointed,
            } => {
                let next = self.committed.height + self.pending.len() as u32 + 1;
                assert_eq!(block.height, next, "delivery in height order");
                let on_chain = self.chain.best_chain_from(block.height).next();
                assert_eq!(on_chain, Some(block), "delivery on the best chain");
                assert!(self.stored.contains_key(&block.hash) || self.node_has(&block.hash));
                self.delivered.push((block, checkpointed));
                self.pending.push_back(block);
                self.validator_free_at = self.validator_free_at.max(self.now) + self.validate_ms;
                let delay = self.validator_free_at - self.now;
                self.push(delay, SimEvent::ValidationDone { hash: block.hash });
            }
            Action::Discard { hash } => {
                let Some(_) = self.stored.remove(&hash) else {
                    panic!("a discarded body is a stored body");
                };
                self.wrong_bodies.remove(&hash);
                self.discards += 1;
                if let Some(at) = self.pending.iter().position(|b| b.hash == hash) {
                    self.pending.truncate(at);
                }
            }
            Action::Penalize { peer, reason } => self.penalties.push((peer, reason)),
            Action::Disconnect { peer } => {
                self.disconnects.push(peer);
                self.peers.get_mut(&peer).unwrap().connected = false;
            }
        }
    }

    fn node_has(&self, hash: &BlockHash) -> bool {
        use hayai_sync::headers::Status;
        let status = self.chain.entry(hash).unwrap().status;
        matches!(status, Status::BodyKnown | Status::BodyValid)
    }

    /// The peer gets one hash of a `getdata` message.
    fn request(&mut self, id: u32, hash: BlockHash) {
        let (height, size) = self.blocks[&hash];
        let now = self.now;
        let peer = self.peers.get_mut(&id).unwrap();
        assert!(peer.connected, "a request goes to a connected peer");
        let profile = peer.profile;
        if let Behaviour::StallAfter(n) = profile.behaviour {
            if peer.answered >= n {
                return;
            }
        }
        if profile.behaviour == Behaviour::Withhold(height) {
            return;
        }
        peer.answered += 1;
        if height > profile.has_up_to {
            return self.push(profile.rtt_ms, SimEvent::NotFound { peer: id, hash });
        }
        let rate = match profile.behaviour {
            Behaviour::SlowAfter(n) if peer.answered > n => 300_000,
            _ => profile.bytes_per_sec,
        };
        let start = peer.busy_until.max(now + profile.rtt_ms);
        let done = start + u64::from(size) * 1_000 / rate;
        peer.busy_until = done;
        self.push(done - now, SimEvent::Arrive { peer: id, hash });
        if let Behaviour::Unsolicited = profile.behaviour {
            // The block at height 1 is never an active request at this time.
            let extra = self.chain.best_chain_from(1).next().unwrap().hash;
            self.push(
                done - now,
                SimEvent::Arrive {
                    peer: id,
                    hash: extra,
                },
            );
        }
    }

    fn step(&mut self, event: SimEvent) {
        match event {
            SimEvent::Tick => {
                self.push(TICK_MS, SimEvent::Tick);
                self.event(Event::Tick);
            }
            SimEvent::Arrive { peer, hash } => {
                let entry = &self.peers[&peer];
                if !entry.connected {
                    return;
                }
                let (height, size) = self.blocks[&hash];
                if height < self.highest_arrival {
                    self.out_of_order_arrivals += 1;
                }
                self.highest_arrival = self.highest_arrival.max(height);
                let wrong = entry.profile.behaviour == Behaviour::WrongBody(height);
                let active = self
                    .scheduler
                    .in_flight()
                    .any(|(block, asked)| block.hash == hash && asked == peer);
                let before = self.stored.len();
                self.event(Event::BlockReceived {
                    peer,
                    hash,
                    bytes_len: size,
                });
                match self.stored.len() > before {
                    true if wrong => drop(self.wrong_bodies.insert(hash)),
                    true if !active => self.late_used += 1,
                    true => {}
                    false => self.late_duplicates += 1,
                }
            }
            SimEvent::NotFound { peer, hash } => {
                if self.peers[&peer].connected {
                    self.event(Event::NotFound {
                        peer,
                        hashes: vec![hash],
                    });
                }
            }
            SimEvent::ValidationDone { hash } => self.validation_done(hash),
        }
    }

    fn validation_done(&mut self, hash: BlockHash) {
        // The node stopped the validation of a block that left the best chain.
        let Some(block) = self.pending.front().copied().filter(|b| b.hash == hash) else {
            return;
        };
        if self.invalid_blocks.contains(&hash) {
            self.chain.mark_invalid(&hash).unwrap();
            self.pending.clear();
            self.stored.remove(&hash);
            return self.event(Event::BlockInvalid { hash });
        }
        if self.wrong_bodies.remove(&hash) {
            // The header stays valid. The validator gets the blocks after it again.
            self.pending.clear();
            self.stored.remove(&hash);
            return self.event(Event::BlockInvalid { hash });
        }
        self.pending.pop_front();
        self.chain.mark_body_valid(&hash).unwrap();
        if let Some(size) = self.stored.remove(&hash) {
            self.committed_bytes += u64::from(size);
        }
        self.committed = block;
        self.event(Event::BlockCommitted { hash });
    }

    /// The invariants that hold after each event.
    fn check(&mut self) {
        let stats = self.scheduler.stats();
        let config = &self.config;
        assert!(stats.memory_bytes <= config.memory_budget_bytes, "budget");
        let stored: u64 = self.stored.values().map(|size| u64::from(*size)).sum();
        assert_eq!(
            stats.held_bytes, stored,
            "the count equals the stored bytes"
        );
        assert!(stats.window_blocks <= config.window_blocks as usize);
        assert!(stats.delivered_blocks <= config.validation_lookahead as usize);
        assert_eq!(stats.delivered_blocks, self.pending.len());
        assert_eq!(stats.committed, self.committed);
        let mut each: BTreeMap<u32, u32> = BTreeMap::new();
        let mut requested = 0;
        for (block, peer) in self.scheduler.in_flight() {
            assert!(
                !self.stored.contains_key(&block.hash),
                "request for a held block"
            );
            assert!(block.height > self.committed.height + self.pending.len() as u32);
            assert!(self.peers[&peer].connected);
            *each.entry(peer).or_default() += 1;
            requested += 1;
        }
        assert_eq!(stats.requested_blocks, requested);
        assert!(each.values().all(|n| *n <= config.peer_in_flight_blocks));
        self.peak_memory = self.peak_memory.max(stats.memory_bytes);
        self.peak_held = self.peak_held.max(stats.held_bytes);
        self.peak_in_flight = self.peak_in_flight.max(requested);
    }

    fn synced(&self) -> bool {
        self.committed == self.chain.best_tip()
    }

    /// Runs the events until `done` is true.
    fn run_until(&mut self, done: impl Fn(&Self) -> bool) {
        while !done(self) {
            let Some(((time, _), event)) = self.queue.pop_first() else {
                unreachable!("the tick is always in the queue");
            };
            assert!(
                time <= LIMIT_MS,
                "no progress: {:?}",
                self.scheduler.stats()
            );
            self.now = time;
            self.step(event);
        }
    }

    fn run(&mut self) {
        self.run_until(Self::synced);
    }

    /// Each block of the best chain was delivered one time, in height order.
    fn assert_delivered_once_in_order(&self) {
        let best: Vec<Tip> = self.chain.best_chain_from(1).collect();
        let on_best: HashSet<BlockHash> = best.iter().map(|block| block.hash).collect();
        let delivered: Vec<Tip> = self
            .delivered
            .iter()
            .map(|(block, _)| *block)
            .filter(|block| on_best.contains(&block.hash))
            .collect();
        assert_eq!(delivered, best);
    }

    fn count(&self, peer: u32, reason: Misbehaviour) -> usize {
        self.penalties
            .iter()
            .filter(|penalty| **penalty == (peer, reason))
            .count()
    }

    fn report(&self, scenario: &str) {
        let seconds = self.now as f64 / 1_000.0;
        let megabytes = self.committed_bytes as f64 / 1e6;
        println!(
            "throughput {scenario}: {} blocks, {megabytes:.0} MB in {seconds:.1} s simulated: \
             {:.0} blocks/s, {:.1} MB/s; peak held {:.1} MB, with reservations {:.1} MB, \
             peak in flight {}, \
             out-of-order arrivals {}",
            self.committed.height,
            f64::from(self.committed.height) / seconds,
            megabytes / seconds,
            self.peak_held as f64 / 1e6,
            self.peak_memory as f64 / 1e6,
            self.peak_in_flight,
            self.out_of_order_arrivals,
        );
    }
}

/// Eight peers: round-trip times from 20 ms to 300 ms, rates from 1.5 MB/s to 12.5 MB/s,
/// 50 MB/s in sum.
fn eight_peers(sim: &mut Sim) {
    let profiles = [
        (20, 12_500_000),
        (40, 10_000_000),
        (60, 8_000_000),
        (80, 6_000_000),
        (120, 5_000_000),
        (150, 4_000_000),
        (200, 3_000_000),
        (300, 1_500_000),
    ];
    for (id, (rtt_ms, rate)) in profiles.into_iter().enumerate() {
        sim.connect(id as u32, Profile::honest(rtt_ms, rate));
    }
}

#[test]
fn full_sync_of_5000_small_blocks_with_8_peers() {
    let mut sim = Sim::regtest(DownloadConfig::default(), SMALL, 1);
    sim.main_chain(5_000);
    eight_peers(&mut sim);
    sim.run();
    sim.assert_delivered_once_in_order();
    assert_eq!(sim.delivered.len(), 5_000);
    assert!(sim.penalties.is_empty(), "{:?}", sim.penalties);
    assert!(
        sim.out_of_order_arrivals > 0,
        "the bodies arrive out of order"
    );
    assert!(sim.stored.is_empty());
    // Each peer supplies blocks, and the fastest peer supplies more than the slowest.
    let requests: Vec<u32> = sim.peers.values().map(|peer| peer.requests).collect();
    assert!(requests.iter().all(|n| *n > 0), "{requests:?}");
    assert!(requests[0] > 2 * requests[7], "{requests:?}");
    assert_eq!(requests.iter().sum::<u32>(), 5_000);
    sim.report("small blocks, 8 peers");
}

#[test]
fn full_sync_of_5000_full_blocks_with_8_peers() {
    let mut sim = Sim::regtest(DownloadConfig::default(), FULL, 2);
    sim.main_chain(5_000);
    eight_peers(&mut sim);
    // The cost of a block in the checkpoint range.
    sim.validate_ms = 3;
    sim.run();
    sim.assert_delivered_once_in_order();
    assert!(sim.penalties.is_empty(), "{:?}", sim.penalties);
    assert!(sim.out_of_order_arrivals > 0);
    // The sum of the rates of the peers is 50 MB/s. The download uses most of it.
    let rate = sim.committed_bytes as f64 / (sim.now as f64 / 1_000.0);
    assert!(rate > 40e6, "{rate}");
    sim.report("full blocks, 8 peers, validation 3 ms");
}

#[test]
fn a_peer_that_stalls_is_penalized_and_replaced() {
    let config = DownloadConfig::default();
    let mut sim = Sim::regtest(config.clone(), SMALL, 3);
    sim.main_chain(5_000);
    eight_peers(&mut sim);
    // The peer answers 20 requests and then no more.
    sim.connect(
        8,
        Profile::honest(30, 10_000_000).with(Behaviour::StallAfter(20)),
    );
    sim.run();
    sim.assert_delivered_once_in_order();
    sim.report("small blocks, 8 peers and 1 peer that stalls");
    // The rescue moved the requests of the peer before the request timeout, without a
    // penalty.
    assert!(sim.now < config.request_timeout_ms, "{}", sim.now);
    assert!(sim.penalties.is_empty(), "{:?}", sim.penalties);
    assert!(sim.peers[&8].requests < 100);
    // The peer owes the answers. The request timeout gives the stall.
    let until = sim.now + 2 * config.request_timeout_ms;
    sim.run_until(|sim| sim.now >= until);
    assert_eq!(sim.penalties, [(8, Misbehaviour::Stall)]);
    assert!(sim.disconnects.is_empty());
}

#[test]
fn two_stalls_disconnect_the_peer() {
    let mut sim = Sim::regtest(DownloadConfig::default(), FULL, 3);
    sim.main_chain(1_500);
    eight_peers(&mut sim);
    sim.connect(
        8,
        Profile::honest(30, 10_000_000).with(Behaviour::StallAfter(20)),
    );
    sim.run();
    sim.assert_delivered_once_in_order();
    // The peer gets requests again one request timeout after the first stall.
    assert_eq!(sim.penalties, [(8, Misbehaviour::Stall); 2]);
    assert_eq!(sim.disconnects, [8]);
    assert_eq!(sim.scheduler.stats().peers, 8);
    sim.report("full blocks, 8 peers and 1 peer that stalls");
}

#[test]
fn a_peer_without_a_measured_rate_has_the_request_timeout() {
    let config = DownloadConfig::default();
    let mut sim = Sim::regtest(config.clone(), SMALL, 4);
    sim.main_chain(600);
    // The peer never answers, so it has no measured rate and no rescue.
    sim.connect(
        0,
        Profile::honest(20, 10_000_000).with(Behaviour::StallAfter(0)),
    );
    sim.connect(1, Profile::honest(20, 10_000_000));
    sim.run_until(|sim| !sim.penalties.is_empty());
    assert_eq!(sim.penalties, [(0, Misbehaviour::Stall)]);
    // The request timeout and one largest block at the lowest rate.
    let allowance = 2_000_000 * 1_000 / config.min_rate_bytes_per_sec;
    let expected = config.request_timeout_ms + allowance;
    assert!(
        sim.now >= expected && sim.now <= expected + TICK_MS,
        "{}",
        sim.now
    );
    // The peer had the lowest block, so no block was committed before.
    assert_eq!(sim.committed.height, 0);
    sim.run();
    sim.assert_delivered_once_in_order();
}

#[test]
fn late_bodies_after_a_rescue_are_used_and_give_no_penalty() {
    let config = DownloadConfig {
        rescue_timeout_ms: 500,
        ..DownloadConfig::default()
    };
    let mut sim = Sim::regtest(config, FULL, 20);
    sim.main_chain(60);
    // The peer 0 has a measured rate of 10 MB/s. Then a block needs 6 s, which is more than
    // the rescue time and less than the request timeout. The rescue moves its requests to
    // the peer 1. A block of the peer 1 needs 7 s, so some late bodies of the peer 0
    // arrive first.
    sim.connect(
        0,
        Profile::honest(20, 10_000_000).with(Behaviour::SlowAfter(4)),
    );
    sim.connect(1, Profile::honest(20, 280_000));
    sim.run();
    sim.assert_delivered_once_in_order();
    assert!(sim.penalties.is_empty(), "{:?}", sim.penalties);
    assert!(sim.late_used > 0);
    assert!(sim.late_duplicates > 0);
}

#[test]
fn a_peer_cannot_keep_one_block_back() {
    let config = DownloadConfig::default();
    let mut sim = Sim::regtest(config.clone(), SMALL, 21);
    sim.main_chain(3_000);
    // The fastest peer gets the request for the block 700 and answers all the others.
    sim.connect(
        0,
        Profile::honest(5, 20_000_000).with(Behaviour::Withhold(700)),
    );
    sim.connect(1, Profile::honest(40, 5_000_000));
    sim.connect(2, Profile::honest(40, 5_000_000));
    sim.run();
    sim.assert_delivered_once_in_order();
    assert_eq!(sim.peers[&0].requests - sim.peers[&0].answered, 1);
    // The bodies after the block 700 were no progress for its request, so the rescue came
    // before the request timeout and the download continued.
    assert!(sim.now < config.request_timeout_ms, "{}", sim.now);
    assert!(sim.penalties.is_empty(), "{:?}", sim.penalties);
    // The peer owes the answer.
    let until = sim.now + 2 * config.request_timeout_ms;
    sim.run_until(|sim| sim.now >= until);
    assert_eq!(sim.penalties, [(0, Misbehaviour::Stall)]);
}

#[test]
fn unsolicited_blocks_are_penalized_and_not_stored() {
    let mut sim = Sim::regtest(DownloadConfig::default(), SMALL, 5);
    sim.main_chain(2_000);
    eight_peers(&mut sim);
    sim.connect(
        8,
        Profile::honest(30, 10_000_000).with(Behaviour::Unsolicited),
    );
    sim.run();
    // The simulation refuses an `Action::Store` for a body that is not an answer.
    sim.assert_delivered_once_in_order();
    // 20 points each: the third one reaches 50 points.
    assert_eq!(sim.count(8, Misbehaviour::Unsolicited), 3);
    assert_eq!(sim.disconnects, [8]);
}

#[test]
fn notfound_moves_the_request_to_another_peer() {
    let mut sim = Sim::regtest(DownloadConfig::default(), SMALL, 6);
    sim.main_chain(3_000);
    // Four peers report the full height and have only a part of the chain.
    for id in 0..4 {
        let mut profile = Profile::honest(20, 10_000_000);
        profile.has_up_to = 500 * (id + 1);
        sim.connect(id, profile);
    }
    sim.connect(4, Profile::honest(100, 2_000_000));
    sim.run();
    sim.assert_delivered_once_in_order();
    assert!(sim.penalties.is_empty(), "{:?}", sim.penalties);
    assert!(sim.peers[&4].requests >= 1_000);
}

#[test]
fn a_peer_below_the_height_gets_no_request_when_another_peer_reports_it() {
    let mut sim = Sim::regtest(DownloadConfig::default(), SMALL, 7);
    sim.main_chain(2_000);
    let mut low = Profile::honest(10, 20_000_000);
    low.reported_height = 1_000;
    low.has_up_to = 1_000;
    sim.connect(0, low);
    sim.connect(1, Profile::honest(100, 2_000_000));
    sim.run();
    sim.assert_delivered_once_in_order();
    // The window is 1,024 blocks, so no request above the height 1,000 went to the peer 0.
    assert!(sim.peers[&0].requests <= 1_000);
    assert!(sim.peers[&0].answered == sim.peers[&0].requests);
}

#[test]
fn back_off_when_no_peer_has_the_block() {
    let config = DownloadConfig::default();
    let mut sim = Sim::regtest(config.clone(), SMALL, 8);
    sim.main_chain(400);
    for id in 0..3 {
        let mut profile = Profile::honest(20, 10_000_000);
        profile.has_up_to = 200;
        sim.connect(id, profile);
    }
    sim.run_until(|sim| sim.committed.height == 200);
    // 60 s more: the three peers answer `notfound`, and the wait doubles.
    let until = sim.now + 60_000;
    sim.run_until(|sim| sim.now >= until);
    assert_eq!(sim.committed.height, 200);
    let requests: u32 = sim.peers.values().map(|peer| peer.requests).sum();
    // Without the wait a request would go out at each answer: one each 20 ms.
    let window = 200;
    assert!(requests < 200 + 3 * window * 8, "{requests}");
    assert!(sim.penalties.is_empty());
    sim.connect(3, Profile::honest(20, 10_000_000));
    sim.run();
    sim.assert_delivered_once_in_order();
}

#[test]
fn the_memory_budget_holds_with_a_small_budget() {
    let config = DownloadConfig {
        // Eight largest blocks.
        memory_budget_bytes: 16_000_000,
        ..DownloadConfig::default()
    };
    let mut sim = Sim::regtest(config, FULL, 9);
    sim.main_chain(600);
    eight_peers(&mut sim);
    sim.validate_ms = 20;
    sim.run();
    // `Sim::check` compares the memory with the budget after each event.
    sim.assert_delivered_once_in_order();
    assert!(sim.peak_memory > 12_000_000, "{}", sim.peak_memory);
    assert!(sim.peak_in_flight <= 7);
    sim.report("full blocks, budget 16 MB");
}

#[test]
fn slow_validation_holds_the_window() {
    let config = DownloadConfig {
        memory_budget_bytes: 64_000_000,
        ..DownloadConfig::default()
    };
    let lookahead = config.validation_lookahead as usize;
    let mut sim = Sim::regtest(config, FULL, 10);
    sim.main_chain(400);
    eight_peers(&mut sim);
    // Cold validation of a full shielded block: 135 ms. The download is 4 times faster.
    sim.validate_ms = 135;
    sim.run_until(|sim| sim.committed.height == 200);
    // The validator has its lookahead, the budget is in use, and the requests stopped.
    assert_eq!(sim.pending.len(), lookahead);
    let stats = sim.scheduler.stats();
    assert!(stats.memory_bytes > 56_000_000, "{stats:?}");
    assert!(stats.requested_blocks <= 2, "{stats:?}");
    sim.run();
    sim.assert_delivered_once_in_order();
    assert!(sim.penalties.is_empty(), "{:?}", sim.penalties);
    // The validator sets the rate: 135 ms for each block.
    assert!(sim.now >= 400 * 135);
    assert!(sim.now < 400 * 135 + 5_000, "{}", sim.now);
    sim.report("full blocks, validation 135 ms, budget 64 MB");
}

/// A chain configuration without the finality rule, so that a test can fork at each height.
fn no_finality() -> ChainConfig {
    let mut config = ChainConfig::new(Network::Regtest);
    config.finality_depth = 1_000_000;
    config
}

#[test]
fn reorg_above_the_committed_tip_keeps_the_common_bodies() {
    let mut sim = Sim::new(no_finality(), DownloadConfig::default(), SMALL, 11);
    let main = sim.main_chain(3_000);
    eight_peers(&mut sim);
    sim.validate_ms = 2;
    sim.run_until(|sim| sim.committed.height >= 1_000);
    let committed = sim.committed.height;
    let delivered_before = sim.delivered.len();
    // The fork point is in the window: the blocks to it are common, the others are not.
    let fork_height = committed + 300;
    let fork = branch(
        main[fork_height as usize - 1].hash(),
        2_000,
        HARD,
        1_000_000,
    );
    let old_in_flight: Vec<(Tip, u32)> = sim.scheduler.in_flight().collect();
    assert!(old_in_flight.iter().any(|(b, _)| b.height > fork_height));
    let stored_before = sim.stored.len();
    assert!(stored_before > 0);
    let change = sim.headers(&fork, fork_height + 1).unwrap();
    assert!(change.is_reorg());
    assert_eq!(change.fork_point.height, fork_height);
    // No request stays for a block of the old branch, and its bodies are gone.
    for (block, _) in sim.scheduler.in_flight() {
        assert_eq!(sim.chain.best_chain_from(block.height).next(), Some(block));
    }
    for hash in sim.stored.keys() {
        assert!(sim.chain.entry(hash).unwrap().on_best_chain);
    }
    assert!(sim.discards > 0);
    assert!(sim.stored.len() + sim.discards == stored_before);
    sim.run();
    assert_eq!(sim.committed.height, fork_height + 2_000);
    sim.assert_delivered_once_in_order();
    // The late bodies of the old branch are not unsolicited.
    assert!(sim.penalties.is_empty(), "{:?}", sim.penalties);
    // The blocks below the fork point were not delivered again.
    assert_eq!(
        sim.delivered.len() - delivered_before,
        (fork_height + 2_000) as usize - delivered_before
    );
}

#[test]
fn reorg_below_the_committed_tip_starts_from_the_fork_point() {
    let mut sim = Sim::new(no_finality(), DownloadConfig::default(), SMALL, 12);
    let main = sim.main_chain(2_000);
    eight_peers(&mut sim);
    sim.validate_ms = 2;
    sim.run_until(|sim| sim.committed.height >= 1_000);
    let fork_height = 900;
    let fork = branch(main[fork_height - 1].hash(), 1_500, HARD, 2_000_000);
    let change = sim.headers(&fork, fork_height as u32 + 1).unwrap();
    assert_eq!(change.fork_point.height, 900);
    assert_eq!(sim.scheduler.stats().committed, change.fork_point);
    assert!(
        sim.stored.is_empty(),
        "all the bodies were on the old branch"
    );
    sim.run();
    assert_eq!(sim.committed.height, 2_400);
    sim.assert_delivered_once_in_order();
    assert!(sim.penalties.is_empty(), "{:?}", sim.penalties);
}

#[test]
fn all_peers_disconnect_and_new_peers_connect() {
    let mut sim = Sim::regtest(DownloadConfig::default(), SMALL, 13);
    sim.main_chain(3_000);
    eight_peers(&mut sim);
    sim.run_until(|sim| sim.committed.height >= 1_200);
    for id in 0..8 {
        sim.disconnect(id);
    }
    assert_eq!(sim.scheduler.in_flight().count(), 0);
    let at = sim.committed.height + sim.scheduler.stats().delivered_blocks as u32;
    let until = sim.now + 30_000;
    sim.run_until(|sim| sim.now >= until);
    assert!(sim.committed.height < 3_000);
    assert!(sim.committed.height >= at);
    for id in 8..12 {
        sim.connect(id, Profile::honest(50, 5_000_000));
    }
    sim.run();
    sim.assert_delivered_once_in_order();
    assert!(sim.penalties.is_empty(), "{:?}", sim.penalties);
}

#[test]
fn an_invalid_block_bans_its_peer_and_ends_the_chain() {
    let mut sim = Sim::regtest(DownloadConfig::default(), SMALL, 14);
    let main = sim.main_chain(1_000);
    eight_peers(&mut sim);
    let invalid = main[699].hash();
    sim.invalid_blocks.insert(invalid);
    sim.run();
    // The best chain stops before the invalid block.
    assert_eq!(sim.committed.height, 699);
    sim.assert_delivered_once_in_order();
    let [(peer, Misbehaviour::InvalidBlock)] = sim.penalties[..] else {
        panic!("{:?}", sim.penalties);
    };
    assert_eq!(sim.disconnects, [peer]);
    assert!(sim.stored.is_empty(), "the bodies after the block are gone");
    assert_eq!(sim.scheduler.in_flight().count(), 0);
    assert_eq!(sim.scheduler.stats().window_blocks, 0);
}

#[test]
fn a_wrong_body_is_requested_from_another_peer() {
    let mut sim = Sim::regtest(DownloadConfig::default(), SMALL, 15);
    sim.main_chain(1_000);
    // The only peer gives a wrong body for the block 400.
    sim.connect(
        0,
        Profile::honest(20, 10_000_000).with(Behaviour::WrongBody(400)),
    );
    sim.run_until(|sim| !sim.disconnects.is_empty());
    assert_eq!(sim.committed.height, 399);
    assert_eq!(sim.penalties, [(0, Misbehaviour::InvalidBlock)]);
    sim.connect(1, Profile::honest(20, 10_000_000));
    sim.run();
    assert_eq!(sim.committed.height, 1_000);
    // The validator got the block 400 two times.
    let twice = sim
        .delivered
        .iter()
        .filter(|(b, _)| b.height == 400)
        .count();
    assert_eq!(twice, 2);
}

#[test]
fn a_new_scheduler_continues_from_the_committed_tip() {
    let mut sim = Sim::regtest(DownloadConfig::default(), SMALL, 16);
    sim.main_chain(3_000);
    eight_peers(&mut sim);
    sim.run_until(|sim| sim.committed.height >= 1_500);
    // A stop of the node: the bodies in memory and the connections are lost. The header
    // chain and the committed tip stay.
    let committed = sim.committed;
    sim.stored.clear();
    sim.pending.clear();
    sim.queue.clear();
    sim.push(TICK_MS, SimEvent::Tick);
    for peer in sim.peers.values_mut() {
        peer.connected = false;
    }
    sim.scheduler = Scheduler::new(sim.config.clone(), &sim.chain, committed).unwrap();
    let stats = sim.scheduler.stats();
    assert_eq!(stats.committed, committed);
    assert_eq!(stats.window_blocks, 1_024);
    assert_eq!(stats.memory_bytes, 0);
    let delivered_before = sim.delivered.len();
    for id in 8..16 {
        sim.connect(id, Profile::honest(50, 5_000_000));
    }
    sim.run();
    assert_eq!(sim.committed.height, 3_000);
    // The first delivery after the start is the block after the committed tip.
    assert_eq!(
        sim.delivered[delivered_before].0.height,
        committed.height + 1
    );
}

#[test]
fn delivery_reports_the_checkpoint_range() {
    let headers = branch(genesis().hash, 300, EASY, 1);
    let mut chain_config = ChainConfig::new(Network::Regtest);
    chain_config.checkpoints =
        Checkpoints::new(vec![(100, headers[99].hash()), (200, headers[199].hash())]).unwrap();
    let mut sim = Sim::new(chain_config, DownloadConfig::default(), SMALL, 17);
    // The best chain reached only the first checkpoint.
    sim.headers(&headers[..150], 1);
    sim.connect(0, Profile::honest(20, 10_000_000));
    sim.run();
    let hints: Vec<bool> = sim.delivered.iter().map(|(_, hint)| *hint).collect();
    assert!(hints[..100].iter().all(|hint| *hint));
    assert!(hints[100..].iter().all(|hint| !*hint));
    // The second checkpoint is on the chain now.
    sim.headers(&headers[150..], 151);
    sim.run();
    let hints: Vec<bool> = sim.delivered.iter().map(|(_, hint)| *hint).collect();
    assert!(hints[150..200].iter().all(|hint| *hint));
    assert!(hints[200..].iter().all(|hint| !*hint));
}

#[test]
fn a_body_that_the_node_has_is_delivered_without_a_request() {
    let mut sim = Sim::regtest(DownloadConfig::default(), SMALL, 18);
    let main = sim.main_chain(50);
    // The node got these bodies from another source.
    for header in &main[..10] {
        sim.chain.mark_body_received(&header.hash()).unwrap();
    }
    sim.chain.mark_body_received(&main[20].hash()).unwrap();
    let committed = sim.committed;
    sim.event(Event::BestHeaderTipChanged { committed });
    assert_eq!(sim.delivered.len(), 10);
    sim.connect(0, Profile::honest(20, 10_000_000));
    sim.run();
    sim.assert_delivered_once_in_order();
    assert_eq!(sim.peers[&0].requests, 39);
}

#[test]
fn events_that_do_not_agree_with_the_state_are_errors() {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let log = dir.path().join("headers.log");
    let (mut chain, _) = HeaderChain::open(no_finality(), &log).unwrap();
    let main = branch(genesis().hash, 10, EASY, 1);
    chain.accept_headers(&main, &Permissive, NOW).unwrap();
    let config = DownloadConfig::default();
    let tip = |height: u32| Tip {
        height,
        hash: main[height as usize - 1].hash(),
    };

    let zero_window = DownloadConfig {
        window_blocks: 0,
        ..config.clone()
    };
    let small_budget = DownloadConfig {
        memory_budget_bytes: 3_999_999,
        ..config.clone()
    };
    let no_lookahead = DownloadConfig {
        validation_lookahead: 0,
        ..config.clone()
    };
    for bad in [zero_window, small_budget, no_lookahead] {
        let result = Scheduler::<u32>::new(bad, &chain, genesis());
        assert!(matches!(result, Err(DownloadError::Config(_))));
    }
    let wrong_height = Tip {
        height: 4,
        hash: main[2].hash(),
    };
    let result = Scheduler::<u32>::new(config.clone(), &chain, wrong_height);
    let expected = DownloadError::CommittedNotOnBestChain(wrong_height);
    assert_eq!(result.err(), Some(expected));

    let mut scheduler = Scheduler::<u32>::new(config, &chain, genesis()).unwrap();
    let mut handle = |event, chain: &HeaderChain| scheduler.handle(event, 0, chain);
    // The block is not delivered.
    let commit = Event::BlockCommitted { hash: tip(1).hash };
    assert_eq!(
        handle(commit, &chain),
        Err(DownloadError::UnexpectedCommit(tip(1).hash))
    );
    // The block is not held.
    let invalid = Event::BlockInvalid { hash: tip(1).hash };
    assert_eq!(
        handle(invalid, &chain),
        Err(DownloadError::NotHeld(tip(1).hash))
    );
    // The committed block is on a branch that is not the best chain.
    let fork = branch(tip(5).hash, 10, HARD, 100);
    chain.accept_headers(&fork, &Permissive, NOW).unwrap();
    let changed = Event::BestHeaderTipChanged { committed: tip(6) };
    assert_eq!(
        handle(changed, &chain),
        Err(DownloadError::CommittedNotOnBestChain(tip(6)))
    );
    // A block from a peer that is not connected has no effect.
    let received = Event::BlockReceived {
        peer: 9,
        hash: tip(1).hash,
        bytes_len: 100,
    };
    assert_eq!(handle(received, &chain), Ok(vec![]));
}

#[test]
fn a_block_above_the_size_limit_is_malformed() {
    let config = DownloadConfig::default();
    let mut sim = Sim::regtest(config, Sizes(2_000_001, 2_000_001), 19);
    sim.main_chain(1);
    sim.connect(0, Profile::honest(20, 10_000_000));
    sim.run_until(|sim| !sim.disconnects.is_empty());
    assert_eq!(sim.penalties, [(0, Misbehaviour::Malformed)]);
    assert!(sim.stored.is_empty());
}

/// A scheduler that a test drives with single events.
struct Direct {
    _dir: tempfile::TempDir,
    chain: HeaderChain,
    scheduler: Scheduler<u32>,
    main: Vec<BlockHeader>,
}

impl Direct {
    /// A chain of `known` blocks. The test adds the other headers of `main` later.
    fn new(config: DownloadConfig, blocks: usize, known: usize) -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let log = dir.path().join("headers.log");
        let (mut chain, _) = HeaderChain::open(no_finality(), &log).unwrap();
        let main = branch(genesis().hash, blocks, EASY, 1);
        chain
            .accept_headers(&main[..known], &Permissive, NOW)
            .unwrap();
        let scheduler = Scheduler::new(config, &chain, genesis()).unwrap();
        Self {
            _dir: dir,
            chain,
            scheduler,
            main,
        }
    }

    fn hash(&self, height: u32) -> BlockHash {
        self.main[height as usize - 1].hash()
    }

    fn at(&mut self, now: u64, event: Event<u32>) -> Vec<Action<u32>> {
        self.scheduler.handle(event, now, &self.chain).unwrap()
    }

    fn connect(&mut self, now: u64, peer: u32) -> Vec<Action<u32>> {
        let best_height_hint = u32::MAX;
        self.at(
            now,
            Event::PeerConnected {
                peer,
                best_height_hint,
            },
        )
    }

    /// A body of 2 MB.
    fn block(&mut self, now: u64, peer: u32, height: u32) -> Vec<Action<u32>> {
        let hash = self.hash(height);
        let bytes_len = 2_000_000;
        self.at(
            now,
            Event::BlockReceived {
                peer,
                hash,
                bytes_len,
            },
        )
    }

    fn not_found(&mut self, now: u64, peer: u32, height: u32) -> Vec<Action<u32>> {
        let hashes = vec![self.hash(height)];
        self.at(now, Event::NotFound { peer, hashes })
    }

    fn request(&self, peer: u32, heights: &[u32]) -> Action<u32> {
        let hashes = heights.iter().map(|height| self.hash(*height)).collect();
        Action::Request { peer, hashes }
    }

    /// One `getdata` message for each block: at the block size of 2 MB of these tests a
    /// message has one block, so that a peer with the answer limits answers it.
    fn requests(&self, peer: u32, heights: &[u32]) -> Vec<Action<u32>> {
        heights
            .iter()
            .map(|height| self.request(peer, &[*height]))
            .collect()
    }

    fn store(&self, height: u32) -> Action<u32> {
        let hash = self.hash(height);
        Action::Store { hash }
    }
}

#[test]
fn the_faster_peer_gets_the_next_blocks() {
    let mut direct = Direct::new(DownloadConfig::default(), 10, 8);
    // A peer without a delivery gets at most 4 requests.
    assert_eq!(direct.connect(0, 0), direct.requests(0, &[1, 2, 3, 4]));
    assert_eq!(direct.connect(0, 1), direct.requests(1, &[5, 6, 7, 8]));
    // The peer 0 needs 100 ms for a block, the peer 1 needs 10 ms.
    for i in 0..4 {
        direct.block(10 + 10 * u64::from(i), 1, 5 + i);
        direct.block(100 + 100 * u64::from(i), 0, 1 + i);
    }
    assert_eq!(direct.scheduler.in_flight().count(), 0);
    // The two peers have no request in flight. Two times 10 ms is less than 100 ms.
    let rest = direct.main[8..].to_vec();
    direct
        .chain
        .accept_headers(&rest, &Permissive, NOW)
        .unwrap();
    let committed = genesis();
    let actions = direct.at(500, Event::BestHeaderTipChanged { committed });
    assert_eq!(actions, direct.requests(1, &[9, 10]));
}

#[test]
fn bodies_of_later_requests_are_no_progress_for_an_earlier_request() {
    let mut direct = Direct::new(DownloadConfig::default(), 8, 8);
    direct.connect(0, 0);
    direct.connect(0, 1);
    // The peer 0 answers its requests for the blocks 2, 3 and 4 at intervals below the
    // rescue timeout, and does not answer the request for the block 1.
    assert_eq!(direct.block(100, 0, 2), [direct.store(2)]);
    for height in 5..=8 {
        direct.block(100, 1, height);
    }
    assert_eq!(direct.at(1_500, Event::Tick), []);
    direct.block(1_600, 0, 3);
    // 2,000 ms after the first body, plus 100 ms for one largest block at 20 MB/s. The
    // body at 1,600 ms does not move this time.
    assert_eq!(direct.at(2_100, Event::Tick), []);
    let rescue = direct.at(2_300, Event::Tick);
    assert_eq!(rescue, direct.requests(1, &[1, 4]));
    // The late bodies of the peer 0 are used.
    assert_eq!(direct.block(2_400, 0, 4), [direct.store(4)]);
    let actions = direct.block(2_500, 0, 1);
    assert_eq!(actions[0], direct.store(1));
    assert!(matches!(actions[1], Action::Deliver { block, .. } if block.height == 1));
    assert_eq!(actions.len(), 9);
}

#[test]
fn a_late_body_above_the_budget_is_dropped() {
    let megabytes = |stats: hayai_sync::download::Stats| stats.memory_bytes / 1_000_000;
    let config = DownloadConfig {
        memory_budget_bytes: 12_000_000,
        backoff_ms: 60_000,
        ..DownloadConfig::default()
    };
    let mut direct = Direct::new(config, 10, 10);
    // 10 MB for the blocks after the first one: 5 requests.
    assert_eq!(direct.connect(0, 0), direct.requests(0, &[1, 2, 3, 4]));
    assert_eq!(direct.connect(0, 1), [direct.request(1, &[5])]);
    // No peer has the block 1, so it waits 60 s. The peer 1 gets the next block.
    assert_eq!(direct.not_found(10, 0, 1), [direct.request(1, &[1])]);
    assert_eq!(direct.not_found(20, 1, 1), [direct.request(1, &[6])]);
    for height in 2..=4 {
        assert_eq!(direct.block(100, 0, height), [direct.store(height)]);
    }
    assert_eq!(direct.block(200, 1, 5), [direct.store(5)]);
    assert_eq!(megabytes(direct.scheduler.stats()), 10);
    // The peer 1 stalls with the request for the block 6. The peer 0 does not have the
    // block, so the block 6 waits too.
    let stall = direct.at(9_000, Event::Tick);
    let penalty = Action::Penalize {
        peer: 1,
        reason: Misbehaviour::Stall,
    };
    assert_eq!(stall, [penalty, direct.request(0, &[6])]);
    assert_eq!(direct.not_found(9_100, 0, 6), [direct.request(0, &[7])]);
    assert_eq!(direct.block(9_200, 0, 7), [direct.store(7)]);
    assert_eq!(megabytes(direct.scheduler.stats()), 10);
    // The wait of the block 1 ends. Its request uses the last 2 MB of the budget.
    let retry = direct.at(61_000, Event::Tick);
    assert_eq!(retry, [direct.request(0, &[1])]);
    assert_eq!(megabytes(direct.scheduler.stats()), 12);
    // The late body of the block 6 has no reservation and no room.
    assert_eq!(direct.block(61_500, 1, 6), []);
    assert_eq!(megabytes(direct.scheduler.stats()), 12);
    // The body of the block 1 has its reservation.
    let actions = direct.block(61_600, 0, 1);
    assert_eq!(actions[0], direct.store(1));
    assert_eq!(megabytes(direct.scheduler.stats()), 12);
}

/// One random scenario: the peers, the configuration, the times of the events and the
/// header chain changes come from the seed.
fn random_scenario(seed: u64) {
    let mut rng = Rng(seed);
    let max_block = 100_000;
    let config = DownloadConfig {
        window_blocks: rng.range(2, 64) as u32,
        memory_budget_bytes: max_block * rng.range(2, 12),
        max_block_bytes: max_block as u32,
        peer_in_flight_blocks: rng.range(1, 16) as u32,
        peer_in_flight_bytes: max_block * rng.range(1, 8),
        validation_lookahead: rng.range(1, 8) as u32,
        request_timeout_ms: rng.range(500, 4_000),
        rescue_timeout_ms: rng.range(200, 1_000),
        min_rate_bytes_per_sec: 50_000,
        backoff_ms: rng.range(100, 1_000),
    };
    let sizes = Sizes(100, rng.range(1_000, max_block));
    let mut sim = Sim::new(no_finality(), config, sizes, rng.next());
    sim.validate_ms = rng.range(0, 30);
    let blocks = rng.range(100, 300) as usize;
    let main = branch(genesis().hash, blocks, EASY, 1);
    // A part of the headers is known at the start. The others come later.
    let known = rng.range(1, blocks as u64) as usize;
    sim.headers(&main[..known], 1);
    let mut next_id = 0;
    let mut random_peer = |sim: &mut Sim, rng: &mut Rng| {
        let mut profile = Profile::honest(rng.range(1, 400), rng.range(20_000, 5_000_000));
        match rng.range(0, 11) {
            0 | 1 => profile.behaviour = Behaviour::StallAfter(rng.range(0, 40) as u32),
            2 => profile.behaviour = Behaviour::Unsolicited,
            3 | 4 => profile.has_up_to = rng.range(0, 300) as u32,
            5 => profile.reported_height = rng.range(0, 300) as u32,
            6 => profile.behaviour = Behaviour::Withhold(rng.range(1, 300) as u32),
            7 => profile.behaviour = Behaviour::SlowAfter(rng.range(0, 40) as u32),
            _ => {}
        }
        sim.connect(next_id, profile);
        next_id += 1;
    };
    for _ in 0..rng.range(0, 6) {
        random_peer(&mut sim, &mut rng);
    }
    // Random changes at random times.
    let mut rest = Some(&main[known..]);
    let mut fork_done = false;
    for _ in 0..rng.range(0, 12) {
        let until = sim.now + rng.range(0, 3_000);
        sim.run_until(|sim| sim.now >= until);
        match rng.range(0, 5) {
            0 | 1 => random_peer(&mut sim, &mut rng),
            2 => {
                let connected: Vec<u32> = sim
                    .peers
                    .iter()
                    .filter(|(_, peer)| peer.connected)
                    .map(|(id, _)| *id)
                    .collect();
                if !connected.is_empty() {
                    let at = rng.range(0, connected.len() as u64 - 1) as usize;
                    sim.disconnect(connected[at]);
                }
            }
            3 => {
                if let Some(headers) = rest.take() {
                    sim.headers(headers, known as u32 + 1);
                }
            }
            _ if !fork_done => {
                // A branch with more work from a random block of the best chain.
                fork_done = true;
                rest = None;
                let best = sim.chain.best_tip().height;
                let from = rng.range(0, u64::from(best)) as u32;
                let parent = sim.chain.best_chain_from(from).next().unwrap().hash;
                let length = (best - from) as usize + rng.range(1, 50) as usize;
                let fork = branch(parent, length, HARD, 5_000_000);
                let change = sim.headers(&fork, from + 1).unwrap();
                assert_eq!(change.fork_point.height, from);
            }
            _ => {}
        }
    }
    // Two honest peers make the end of the download possible.
    sim.connect(1_000, Profile::honest(30, 2_000_000));
    sim.connect(1_001, Profile::honest(60, 1_000_000));
    sim.run();
    sim.assert_delivered_once_in_order();
    assert!(sim.stored.is_empty());
    assert_eq!(sim.scheduler.in_flight().count(), 0);
    assert_eq!(sim.scheduler.stats().memory_bytes, 0);
    // An honest peer is never penalized for a block.
    for (peer, reason) in &sim.penalties {
        let behaviour = sim.peers[peer].profile.behaviour;
        match reason {
            Misbehaviour::Unsolicited => assert!(behaviour == Behaviour::Unsolicited),
            Misbehaviour::Stall => {}
            other => panic!("{other:?}"),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// Random peers, configurations and event orders. `Sim::check` holds after each event:
    /// the budget, the count of the stored bytes, no request for a held or delivered
    /// block, the limit of each peer, the lookahead. At the end each block of the best
    /// chain was delivered one time and in order.
    #[test]
    fn random_event_orders_keep_the_invariants(seed in any::<u64>()) {
        random_scenario(seed);
    }
}

/// A Zebra or Zakura peer answers at most 16 blocks and 1 MB of one `getdata` message. The
/// scheduler asks again for the other blocks at once, and the peer gets no stall.
#[test]
fn the_answer_limits_of_a_zakura_peer_give_no_stall() {
    for (sizes, blocks) in [(SMALL, 2_000), (Sizes(100_000, 400_000), 400), (FULL, 100)] {
        let config = DownloadConfig::default();
        let mut sim = Sim::regtest(config.clone(), sizes, 21);
        sim.main_chain(blocks);
        sim.connect(
            0,
            Profile::honest(20, 50_000_000).with(Behaviour::AnswerLimits),
        );
        sim.run();
        sim.assert_delivered_once_in_order();
        assert_eq!(sim.delivered.len(), blocks);
        assert!(sim.penalties.is_empty(), "{:?}", sim.penalties);
        assert!(sim.disconnects.is_empty());
        // No request waited for the request timeout: the time is the transfer time.
        let bytes: u64 = sim.blocks.values().map(|(_, size)| u64::from(*size)).sum();
        let transfer_ms = bytes * 1_000 / 50_000_000;
        assert!(
            sim.now < 2 * transfer_ms + config.request_timeout_ms / 2,
            "{} ms for {bytes} bytes",
            sim.now
        );
    }
}
