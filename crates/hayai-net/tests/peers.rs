//! Peer management over loopback: the address book file, the connection manager, the limits,
//! bans and the address messages. The clock and the DNS resolver are injected: no test reads
//! the system time for a decision or touches the public network.
//!
//! The remote nodes listen on several addresses of 127.0.0.0/8 so that they are in
//! different /16 groups (Linux routes the whole block to the loopback device).

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use hayai_consensus::HeaderRuleError;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_net::addrbook::{
    group, AddrBook, AddrBookConfig, AddrBookError, AddrState, GOSSIP_PENALTY_SECS,
};
use hayai_net::codec::{
    encode, read_message, LegacyMessage, NetAddr, Network, TimedNetAddr, VersionMessage,
};
use hayai_net::connect::{PeerConfig, PeerEnv, PeerManager, Refusal};
use hayai_net::protocol::{CompactVer, PeerProtocol, NODE_COMPACT_RELAY, NODE_NETWORK};
use hayai_net::relay::{
    BlockSink, ChainSource, HistoryRootSource, IncomingBlock, Relay, RelayConfig, RelayDeps,
    Source, TxSink,
};
use hayai_net::session::Direction;
use hayai_net::Misbehaviour;
use hayai_relay::{BlockTxn, HeaderCheck, HeaderError, Message};
use hayai_sync::score::BAN_SECS;
use hayai_wire::header::{BlockHash, BlockHeader, PowError};
use hayai_wire::{RawTx, TxLookup, WtxId};

const NET: Network = Network::Regtest;
const NOW: u64 = 1_800_000_000;
const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

struct Nothing;

impl TxLookup for Nothing {
    fn get(&self, _: &WtxId) -> Option<Arc<RawTx>> {
        None
    }
    fn for_each_id(&self, _: &mut dyn FnMut(&WtxId)) {}
    fn len(&self) -> usize {
        0
    }
}

impl TxSink for Nothing {
    fn accept_tx(&self, _: Arc<RawTx>, _: Source) -> bool {
        false
    }
}

impl BlockSink for Nothing {
    fn accept_block(&self, _: IncomingBlock) {}
}

impl HistoryRootSource for Nothing {
    fn history_root(&self, _: &BlockHash) -> Option<[u8; 32]> {
        None
    }
}

impl ChainSource for Nothing {
    fn tip_height(&self) -> u32 {
        7
    }
    fn tip_hash(&self) -> BlockHash {
        BlockHash([0x77; 32])
    }
    fn tx_branch(&self) -> Option<BranchId> {
        Some(BranchId::Nu5)
    }
    fn block_branch(&self, _: &BlockHash) -> Option<BranchId> {
        Some(BranchId::Nu5)
    }
    fn headers_after(&self, _: &[BlockHash], _: &BlockHash, _: bool) -> Vec<BlockHeader> {
        Vec::new()
    }
    fn block_bytes(&self, _: &BlockHash) -> Option<Bytes> {
        None
    }
}

impl HeaderCheck for Nothing {
    fn check(&self, _: &BlockHeader) -> Result<(), HeaderError> {
        Ok(())
    }
}

/// A header check that refuses a header by its version: 5 has no proof of work, 6 has an
/// unknown parent.
struct ByVersion;

impl HeaderCheck for ByVersion {
    fn check(&self, header: &BlockHeader) -> Result<(), HeaderError> {
        match header.version {
            5 => Err(HeaderError::Rule(HeaderRuleError::Pow(
                PowError::HashAboveTarget,
            ))),
            6 => Err(HeaderError::ParentUnknown(header.prev_hash)),
            _ => Ok(()),
        }
    }
}

fn deps() -> RelayDeps {
    RelayDeps {
        txs: Arc::new(Nothing),
        tx_sink: Arc::new(Nothing),
        block_sink: Arc::new(Nothing),
        chain: Arc::new(Nothing),
        header_check: Arc::new(ByVersion),
        history_roots: Arc::new(Nothing),
        sync: None,
    }
}

fn relay_config() -> RelayConfig {
    let mut c = RelayConfig::new(NET);
    c.tick = Duration::from_millis(20);
    c.handshake_timeout = Duration::from_secs(3);
    c.connect_timeout = Duration::from_secs(3);
    c.mempool_poll = None;
    c
}

/// A remote node with the default peer manager, listening on `ip`.
fn remote(ip: &str) -> (Arc<Relay>, SocketAddr) {
    let relay = Relay::new(relay_config(), deps());
    let addr = relay.listen(format!("{ip}:0")).unwrap();
    (relay, addr)
}

/// The node under test: a fixed clock, a resolver that answers `seeds` and counts its
/// calls, and a seeded generator.
struct Node {
    relay: Arc<Relay>,
    manager: Arc<PeerManager>,
    clock: Arc<AtomicU64>,
    resolved: Arc<AtomicUsize>,
    addr: SocketAddr,
}

impl Node {
    fn new(seeds: Vec<SocketAddr>, configure: impl FnOnce(&mut PeerConfig)) -> Self {
        let mut config = PeerConfig::new(NET);
        config.seeders = vec!["seed.example:18344".into()];
        config.book.retry_base_secs = 100;
        configure(&mut config);
        let book = AddrBook::new(config.book.clone());
        Self::with_book(config, book, seeds)
    }

    fn with_book(config: PeerConfig, book: AddrBook, seeds: Vec<SocketAddr>) -> Self {
        let clock = Arc::new(AtomicU64::new(NOW));
        let resolved = Arc::new(AtomicUsize::new(0));
        let env = PeerEnv {
            clock: {
                let clock = clock.clone();
                Arc::new(move || clock.load(Ordering::SeqCst))
            },
            resolver: {
                let resolved = resolved.clone();
                Arc::new(move |name| {
                    assert_eq!(name, "seed.example:18344");
                    resolved.fetch_add(1, Ordering::SeqCst);
                    Ok(seeds.clone())
                })
            },
            rng_seed: Some(11),
        };
        let manager = PeerManager::new(config, book, env);
        let relay = Relay::with_peer_manager(relay_config(), deps(), manager.clone());
        let addr = relay.listen("127.0.0.1:0").unwrap();
        Self {
            relay,
            manager,
            clock,
            resolved,
            addr,
        }
    }

    fn advance(&self, secs: u64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }

    fn outbound(&self) -> Vec<SocketAddr> {
        self.relay
            .peers()
            .iter()
            .filter(|p| p.direction == Direction::Outbound && p.established)
            .map(|p| p.addr)
            .collect()
    }

    fn established(&self) -> usize {
        self.relay.peers().iter().filter(|p| p.established).count()
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn version(version: u32, services: u64, peer: SocketAddr) -> LegacyMessage {
    LegacyMessage::Version(VersionMessage {
        version,
        services,
        timestamp: 0,
        addr_recv: NetAddr {
            services: 0,
            addr: peer,
        },
        addr_from: NetAddr {
            services,
            addr: ([0, 0, 0, 0], 0).into(),
        },
        nonce: 0x1234,
        user_agent: "/MagicBean:6.0.0/".into(),
        start_height: 0,
        relay: true,
    })
}

/// A scripted peer on a raw socket.
struct SimPeer {
    stream: TcpStream,
}

impl SimPeer {
    fn over(stream: TcpStream) -> Self {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self { stream }
    }

    /// Connects to `addr` and completes the handshake with protocol version `protocol`.
    fn connect(addr: SocketAddr, protocol: u32, services: u64) -> Self {
        let mut peer = Self::over(TcpStream::connect(addr).unwrap());
        peer.handshake(addr, protocol, services);
        peer
    }

    fn handshake(&mut self, addr: SocketAddr, protocol: u32, services: u64) {
        let LegacyMessage::Version(theirs) = self.recv() else {
            panic!("expected version first");
        };
        assert_eq!(theirs.version, hayai_net::protocol::protocol_version());
        self.send(&version(protocol, services, addr));
        assert_eq!(self.recv(), LegacyMessage::Verack);
        self.send(&LegacyMessage::Verack);
    }

    fn local(&self) -> SocketAddr {
        self.stream.local_addr().unwrap()
    }

    fn send(&mut self, m: &LegacyMessage) {
        self.stream.write_all(&encode(NET, m)).unwrap();
    }

    fn recv(&mut self) -> LegacyMessage {
        read_message(&mut self.stream, NET, usize::MAX).expect("message")
    }

    /// The next message that is not a ping (answered) or pong.
    fn recv_app(&mut self) -> LegacyMessage {
        loop {
            match self.recv() {
                LegacyMessage::Ping(n) => self.send(&LegacyMessage::Pong(n)),
                LegacyMessage::Pong(_) => {}
                other => return other,
            }
        }
    }

    /// A ping round trip: the pong proves that the node sent nothing else before it.
    fn quiet(&mut self, nonce: u64) {
        self.send(&LegacyMessage::Ping(nonce));
        loop {
            match self.recv() {
                LegacyMessage::Ping(n) => self.send(&LegacyMessage::Pong(n)),
                LegacyMessage::Pong(n) if n == nonce => return,
                LegacyMessage::Pong(_) => {}
                other => panic!("unexpected {other:?} before the pong"),
            }
        }
    }

    /// Whether the node closed the connection: the next read ends without a message.
    fn closed(&mut self) -> bool {
        loop {
            match read_message(&mut self.stream, NET, usize::MAX) {
                Ok(LegacyMessage::Ping(_) | LegacyMessage::Pong(_)) => {}
                Ok(other) => panic!("unexpected {other:?} on a connection that must close"),
                Err(hayai_net::codec::ReadError::Io(e)) => {
                    assert!(
                        !matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ),
                        "the node kept the connection open"
                    );
                    return true;
                }
                Err(e) => panic!("{e}"),
            }
        }
    }
}

/// Whether the node refuses a new connection to `addr`: it closes it before its `version`.
fn refused(addr: SocketAddr) -> bool {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    match read_message(&mut stream, NET, usize::MAX) {
        Ok(LegacyMessage::Version(_)) => false,
        Ok(other) => panic!("unexpected {other:?}"),
        Err(hayai_net::codec::ReadError::Io(e)) => {
            assert!(
                !matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ),
                "the node kept the connection open"
            );
            true
        }
        Err(e) => panic!("{e}"),
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("peers-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn public(n: u8) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(11, n, 0, 1)), 8233)
}

fn timed(addr: SocketAddr, time: u64) -> TimedNetAddr {
    TimedNetAddr {
        time: time as u32,
        net: NetAddr {
            services: NODE_NETWORK,
            addr,
        },
    }
}

// ----- the address book file -----

fn sample_book() -> AddrBook {
    let mut book = AddrBook::new(AddrBookConfig::public());
    book.add_local(&[public(1), public(2)], NODE_NETWORK, NOW);
    book.add_gossiped(
        &[
            timed(public(3), NOW - 50),
            timed("[2606:4700::1]:8233".parse().unwrap(), NOW),
        ],
        public(1).ip(),
        NOW,
    );
    book.mark_attempt(&public(1), NOW + 1);
    book.mark_success(&public(1), NODE_NETWORK | NODE_COMPACT_RELAY, NOW + 2);
    book.mark_failed(&public(2), NOW + 3);
    book.ban(public(9).ip(), NOW + BAN_SECS, NOW);
    book.ban("2001:4860::1".parse().unwrap(), NOW + 5, NOW);
    book
}

#[test]
fn the_address_book_file_round_trips() {
    let path = scratch("round-trip").join("peers.dat");
    let book = sample_book();
    book.save(&path).unwrap();
    assert!(!path.with_extension("dat.tmp").exists());
    let loaded = AddrBook::load(&path, AddrBookConfig::public()).unwrap();
    let before: Vec<_> = book.entries().copied().collect();
    let after: Vec<_> = loaded.entries().copied().collect();
    assert_eq!(before.len(), 4);
    assert_eq!(before, after);
    assert_eq!(
        loaded.get(&public(1)).unwrap().state(),
        AddrState::Responded
    );
    assert_eq!(loaded.get(&public(2)).unwrap().state(), AddrState::Failed);
    assert_eq!(
        loaded.get(&public(3)).unwrap().state(),
        AddrState::NeverTried
    );
    assert_eq!(loaded.get(&public(3)).unwrap().source, public(1).ip());
    assert!(loaded.is_banned(public(9).ip(), NOW + BAN_SECS - 1));
    assert!(!loaded.is_banned(public(9).ip(), NOW + BAN_SECS));
    assert!(loaded.is_banned("2001:4860::1".parse().unwrap(), NOW + 4));
    // A second save replaces the file, and the result loads again.
    let mut loaded = loaded;
    loaded.remove(&public(3));
    loaded.save(&path).unwrap();
    let again = AddrBook::load(&path, AddrBookConfig::public()).unwrap();
    assert_eq!(again.len(), 3);
    // A smaller capacity keeps what fits.
    let small = AddrBookConfig {
        capacity: 2,
        ..AddrBookConfig::public()
    };
    assert_eq!(AddrBook::load(&path, small).unwrap().len(), 2);
}

#[test]
fn a_missing_address_book_file_gives_an_empty_book() {
    let path = scratch("missing").join("peers.dat");
    let book = AddrBook::load(&path, AddrBookConfig::public()).unwrap();
    assert!(book.is_empty());
}

#[test]
fn a_damaged_address_book_file_is_an_error() {
    let dir = scratch("damaged");
    let path = dir.join("peers.dat");
    sample_book().save(&path).unwrap();
    let good = std::fs::read(&path).unwrap();
    let load = |bytes: &[u8]| {
        let path = dir.join("candidate.dat");
        std::fs::write(&path, bytes).unwrap();
        AddrBook::load(&path, AddrBookConfig::public()).map(|b| b.len())
    };
    assert!(matches!(load(&good), Ok(4)));
    // One changed bit anywhere, the hash included.
    for at in [0, 5, 9, 40, good.len() / 2, good.len() - 33, good.len() - 1] {
        let mut bad = good.clone();
        bad[at] ^= 0x01;
        assert!(
            matches!(load(&bad), Err(AddrBookError::Damaged("hash mismatch"))),
            "byte {at}"
        );
    }
    // A file cut short, and an empty file.
    assert!(matches!(
        load(&good[..good.len() - 7]),
        Err(AddrBookError::Damaged(_))
    ));
    assert!(matches!(
        load(&good[..20]),
        Err(AddrBookError::Damaged("shorter than its hash"))
    ));
    assert!(matches!(load(&[]), Err(AddrBookError::Damaged(_))));
    // A valid hash over a wrong magic, a wrong version, or counts that do not fit.
    let rehash = |mut body: Vec<u8>| {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(&body);
        body.extend_from_slice(&hash);
        body
    };
    let body = &good[..good.len() - 32];
    let mut magic = body.to_vec();
    magic[0] = b'X';
    assert!(matches!(
        load(&rehash(magic)),
        Err(AddrBookError::Damaged("wrong magic"))
    ));
    let mut version = body.to_vec();
    version[4] = 2;
    assert!(matches!(
        load(&rehash(version)),
        Err(AddrBookError::Damaged("unknown version"))
    ));
    let mut count = body.to_vec();
    count[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(
        load(&rehash(count)),
        Err(AddrBookError::Damaged("entry count exceeds the file"))
    ));
    let mut extra = body.to_vec();
    extra.push(0);
    assert!(matches!(
        load(&rehash(extra)),
        Err(AddrBookError::Damaged("ban count does not match the file"))
    ));
    assert!(matches!(
        load(&rehash(body[..body.len() - 30].to_vec())),
        Err(AddrBookError::Damaged(_))
    ));
}

// ----- the connection manager -----

#[test]
fn the_manager_reaches_the_outbound_target_and_replaces_a_dropped_peer() {
    // Two remotes share the group 127.1.0.0/16; three more have a group each.
    let remotes: Vec<(Arc<Relay>, SocketAddr)> = [
        "127.1.0.1",
        "127.1.0.2",
        "127.2.0.1",
        "127.3.0.1",
        "127.4.0.1",
    ]
    .iter()
    .map(|ip| remote(ip))
    .collect();
    let seeds: Vec<SocketAddr> = remotes.iter().map(|(_, a)| *a).collect();
    let node = Node::new(seeds.clone(), |c| {
        c.outbound_target = 3;
        c.outbound_per_group = 1;
        c.max_per_ip = 1;
    });
    // The book is empty: the step asks the seeder once, then dials three groups.
    node.manager.maintain(&node.relay);
    assert_eq!(node.resolved.load(Ordering::SeqCst), 1);
    assert_eq!(node.manager.book().len(), 5);
    wait_for("three outbound peers", || node.outbound().len() == 3);
    let first = node.outbound();
    let mut groups: Vec<_> = first.iter().map(|a| group(a.ip())).collect();
    groups.sort();
    groups.dedup();
    assert_eq!(groups.len(), 3);
    for addr in &first {
        let book = node.manager.book();
        let entry = book.get(addr).unwrap();
        assert_eq!(entry.state(), AddrState::Responded);
        assert_eq!(entry.last_seen, NOW);
        assert_eq!(entry.last_attempt, Some(NOW));
        assert_eq!(entry.services & NODE_NETWORK, NODE_NETWORK);
    }
    // At the target, a step dials nothing and asks no seeder.
    node.manager.maintain(&node.relay);
    assert_eq!(node.relay.peers().len(), 3);
    assert_eq!(node.resolved.load(Ordering::SeqCst), 1);

    // One peer goes away. The next step dials an address of a free group. The dropped
    // address is in its retry delay, so the step does not dial it again.
    let dropped = first[0];
    let (gone, _) = remotes.iter().find(|(_, a)| *a == dropped).unwrap();
    gone.shutdown();
    wait_for("the peer left", || node.relay.peers().len() == 2);
    node.manager.maintain(&node.relay);
    wait_for("three outbound peers again", || node.outbound().len() == 3);
    let second = node.outbound();
    assert!(!second.contains(&dropped));
    let mut groups: Vec<_> = second.iter().map(|a| group(a.ip())).collect();
    groups.sort();
    groups.dedup();
    assert_eq!(groups.len(), 3);
    // The seeder interval is not over: no second query.
    assert_eq!(node.resolved.load(Ordering::SeqCst), 1);

    node.relay.shutdown();
    for (relay, _) in &remotes {
        relay.shutdown();
    }
}

#[test]
fn a_failed_address_is_dialled_again_only_after_its_delay() {
    // A port where nothing listens: the connection is refused at once.
    let dead = {
        let listener = TcpListener::bind("127.5.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let node = Node::new(vec![dead], |c| c.outbound_target = 2);
    let entry = |node: &Node| node.manager.book().get(&dead).copied();
    node.manager.maintain(&node.relay);
    assert_eq!(entry(&node).unwrap().failures, 1);
    assert_eq!(entry(&node).unwrap().last_attempt, Some(NOW));
    // Base delay 100 s, doubled by one failure.
    node.advance(199);
    node.manager.maintain(&node.relay);
    assert_eq!(entry(&node).unwrap().failures, 1);
    node.advance(1);
    node.manager.maintain(&node.relay);
    assert_eq!(entry(&node).unwrap().failures, 2);
    assert_eq!(entry(&node).unwrap().last_attempt, Some(NOW + 200));
    node.advance(399);
    node.manager.maintain(&node.relay);
    assert_eq!(entry(&node).unwrap().failures, 2);
    assert_eq!(node.resolved.load(Ordering::SeqCst), 1);
    // The third failure of an address that never responded removes it. The seeder interval
    // (600 s) is over at this step: the seeder is asked again and has no new address.
    node.advance(1);
    node.manager.maintain(&node.relay);
    assert_eq!(entry(&node), None);
    assert_eq!(node.resolved.load(Ordering::SeqCst), 2);
    // Before the next interval the seeder is not asked, so the book stays empty.
    node.advance(599);
    node.manager.maintain(&node.relay);
    assert_eq!(entry(&node), None);
    assert_eq!(node.resolved.load(Ordering::SeqCst), 2);
    node.advance(1);
    node.manager.maintain(&node.relay);
    assert_eq!(node.resolved.load(Ordering::SeqCst), 3);
    assert_eq!(entry(&node).unwrap().failures, 1);
    node.relay.shutdown();
}

#[test]
fn a_seeder_that_fails_does_not_stop_the_others() {
    let (remote_relay, remote_addr) = remote("127.6.0.1");
    let clock = Arc::new(AtomicU64::new(NOW));
    let mut config = PeerConfig::new(NET);
    config.seeders = vec!["down.example:1".into(), "up.example:1".into()];
    config.outbound_target = 1;
    let env = PeerEnv {
        clock: Arc::new(move || clock.load(Ordering::SeqCst)),
        resolver: Arc::new(move |name| match name {
            "up.example:1" => Ok(vec![remote_addr]),
            _ => Err(std::io::Error::other("no such host")),
        }),
        rng_seed: Some(1),
    };
    let book = AddrBook::new(config.book.clone());
    let manager = PeerManager::new(config, book, env);
    let relay = Relay::with_peer_manager(relay_config(), deps(), manager.clone());
    manager.maintain(&relay);
    wait_for("the outbound peer", || {
        relay.peers().iter().any(|p| p.established)
    });
    assert_eq!(relay.peers()[0].addr, remote_addr);
    relay.shutdown();
    remote_relay.shutdown();
}

#[test]
fn the_manager_thread_fills_the_target_and_saves_the_book_at_the_end() {
    let (remote_relay, remote_addr) = remote("127.7.0.1");
    let path = scratch("thread").join("peers.dat");
    let mut config = PeerConfig::new(NET);
    config.seeders = vec!["seed.example:18344".into()];
    config.outbound_target = 1;
    config.maintain_interval = Duration::from_millis(10);
    config.book_path = Some(path.clone());
    let book = AddrBook::load(&path, config.book.clone()).unwrap();
    let node = Node::with_book(config.clone(), book, vec![remote_addr]);
    let thread = node.manager.spawn(&node.relay).unwrap();
    wait_for("the outbound peer", || node.outbound() == vec![remote_addr]);
    node.relay.shutdown();
    thread.join().unwrap();
    let saved = AddrBook::load(&path, config.book).unwrap();
    assert_eq!(
        saved.get(&remote_addr).unwrap().state(),
        AddrState::Responded
    );
    remote_relay.shutdown();
}

#[test]
fn the_node_never_keeps_its_own_address() {
    let node = Node::new(Vec::new(), |c| {
        c.outbound_target = 1;
        c.seeders.clear();
    });
    assert_eq!(node.manager.add_peers(&[node.addr]), 1);
    node.manager.maintain(&node.relay);
    wait_for("the self connection ended", || {
        node.relay.peers().is_empty() && node.manager.book().is_empty()
    });
    // A peer tells the address again: the book refuses it.
    let mut peer = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    peer.send(&LegacyMessage::Addr(vec![timed(node.addr, NOW)]));
    peer.quiet(1);
    assert!(node.manager.book().is_empty());
    assert_eq!(node.manager.add_peers(&[node.addr]), 1);
    node.relay.shutdown();
}

// ----- limits -----

#[test]
fn inbound_connections_above_the_limit_are_refused() {
    let node = Node::new(Vec::new(), |c| c.max_inbound = 2);
    let mut a = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    let mut b = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    wait_for("two inbound peers", || node.established() == 2);
    assert!(refused(node.addr));
    assert_eq!(node.relay.peers().len(), 2);
    // The limit is on inbound peers: an outbound connection is still possible.
    let (remote_relay, remote_addr) = remote("127.8.0.1");
    node.relay.connect(remote_addr).unwrap();
    wait_for("three peers", || node.established() == 3);
    // A place that becomes free is given again.
    drop(a.stream);
    wait_for("one peer left", || node.relay.peers().len() == 2);
    a = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    a.quiet(1);
    b.quiet(2);
    node.relay.shutdown();
    remote_relay.shutdown();
}

#[test]
fn connections_with_one_ip_address_are_bounded() {
    let node = Node::new(Vec::new(), |c| c.max_per_ip = 1);
    let mut a = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    wait_for("the inbound peer", || node.established() == 1);
    assert!(refused(node.addr));
    // The bound counts both directions: 127.0.0.1 is in use, so no outbound to it either.
    let (remote_relay, remote_addr) = remote("127.0.0.1");
    let Err(e) = node.relay.connect(remote_addr) else {
        panic!("a second connection with the IP address was accepted");
    };
    assert!(e.to_string().contains("maximum of connections"), "{e}");
    a.quiet(1);
    assert_eq!(node.relay.peers().len(), 1);
    // The refusal is not a failure of the address.
    node.manager.add_peers(&[remote_addr]);
    assert_eq!(node.manager.book().get(&remote_addr).unwrap().failures, 0);
    node.relay.shutdown();
    remote_relay.shutdown();
}

// ----- protocol version -----

#[test]
fn a_peer_below_the_minimum_protocol_version_is_refused_without_a_ban() {
    let node = Node::new(Vec::new(), |_| {});
    let mut old = SimPeer::over(TcpStream::connect(node.addr).unwrap());
    let LegacyMessage::Version(_) = old.recv() else {
        panic!("expected version first");
    };
    old.send(&version(170_149, NODE_NETWORK, node.addr));
    assert!(old.closed());
    assert_eq!(node.manager.score(LOOPBACK), 0);
    assert!(!node.manager.is_banned(LOOPBACK));

    // 170,150 passes the initial minimum. After NU6.3 the minimum is 170,160: the node
    // disconnects the established peer below it and refuses a new one.
    let mut nu62 = SimPeer::connect(node.addr, 170_150, NODE_NETWORK);
    let mut nu63 = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    wait_for("two peers", || node.established() == 2);
    let minimum = hayai_net::min_peer_version(NET, hayai_consensus::Upgrade::Nu6_3);
    assert_eq!(minimum, 170_160);
    node.relay.set_min_peer_version(minimum);
    assert!(nu62.closed());
    nu63.quiet(1);
    assert_eq!(node.relay.peers().len(), 1);
    let mut late = SimPeer::over(TcpStream::connect(node.addr).unwrap());
    let LegacyMessage::Version(_) = late.recv() else {
        panic!("expected version first");
    };
    late.send(&version(170_150, NODE_NETWORK, node.addr));
    assert!(late.closed());
    assert!(!node.manager.is_banned(LOOPBACK));
    node.relay.shutdown();
}

// ----- scores and bans -----

/// A frame with a wrong checksum.
fn malformed() -> Vec<u8> {
    let mut frame = encode(NET, &LegacyMessage::Ping(1));
    let last = frame.len() - 1;
    frame[last] ^= 0xff;
    frame
}

#[test]
fn a_malformed_frame_disconnects_and_the_second_one_bans_until_the_ban_ends() {
    let node = Node::new(Vec::new(), |_| {});
    let mut bad = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    let mut bystander = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    wait_for("two peers", || node.established() == 2);
    bad.stream.write_all(&malformed()).unwrap();
    // The first malformed frame closes the connection of the sender only. It is not a ban.
    assert!(bad.closed());
    bystander.quiet(1);
    assert_eq!(node.manager.score(LOOPBACK), 50);
    assert!(!node.manager.is_banned(LOOPBACK));
    wait_for("one peer", || node.relay.peers().len() == 1);
    // The second one, before the points decay, is a ban. The ban closes every connection
    // with the IP address.
    let mut bad = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    wait_for("two peers again", || node.established() == 2);
    bad.stream.write_all(&malformed()).unwrap();
    assert!(bad.closed());
    assert!(bystander.closed());
    wait_for("no peers", || node.relay.peers().is_empty());
    assert!(node.manager.is_banned(LOOPBACK));
    // A banned peer is refused when it connects again, and the node does not dial it.
    assert!(refused(node.addr));
    let (remote_relay, remote_addr) = remote("127.0.0.1");
    node.manager.add_peers(&[remote_addr]);
    let Err(e) = node.relay.connect(remote_addr) else {
        panic!("the node dialled a banned address");
    };
    assert!(e.to_string().contains("banned"), "{e}");
    node.manager.maintain(&node.relay);
    assert!(node.relay.peers().is_empty());
    // One second before the end the ban holds; at the end the peer is accepted again and
    // starts from zero points.
    node.advance(BAN_SECS - 1);
    assert!(refused(node.addr));
    node.advance(1);
    let mut back = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    back.quiet(1);
    assert_eq!(node.manager.score(LOOPBACK), 0);
    node.relay.shutdown();
    remote_relay.shutdown();
}

#[test]
fn a_message_before_the_handshake_disconnects_and_the_second_one_bans() {
    let node = Node::new(Vec::new(), |_| {});
    for banned in [false, true] {
        let mut peer = SimPeer::over(TcpStream::connect(node.addr).unwrap());
        let LegacyMessage::Version(_) = peer.recv() else {
            panic!("expected version first");
        };
        peer.send(&LegacyMessage::Mempool);
        assert!(peer.closed());
        assert_eq!(node.manager.is_banned(LOOPBACK), banned);
    }
    node.relay.shutdown();
}

fn source_of(node: &Node, peer: &SimPeer) -> Source {
    let me = peer.local();
    let info = node
        .relay
        .peers()
        .into_iter()
        .find(|p| p.addr == me)
        .expect("the peer is connected");
    Source::Peer {
        id: info.id,
        protocol: info.protocol,
        ip: info.addr.ip(),
    }
}

#[test]
fn reported_misbehaviour_disconnects_at_the_threshold_and_decays() {
    let node = Node::new(Vec::new(), |_| {});
    let mut peer = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    wait_for("the peer", || node.established() == 1);
    let source = source_of(&node, &peer);
    // A local source and four invalid transactions change nothing for the connection.
    node.relay
        .misbehaved(Source::Local, Misbehaviour::InvalidBlock);
    for _ in 0..4 {
        node.relay
            .misbehaved(source, Misbehaviour::InvalidTransaction);
    }
    assert_eq!(node.manager.score(LOOPBACK), 40);
    peer.quiet(1);
    // The fifth reaches the disconnection threshold. It is not a ban.
    node.relay
        .misbehaved(source, Misbehaviour::InvalidTransaction);
    assert!(peer.closed());
    assert!(!node.manager.is_banned(LOOPBACK));
    assert_eq!(node.manager.score(LOOPBACK), 50);
    // The score stays with the IP address across connections, and it decays.
    let mut peer = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    wait_for("the peer again", || node.established() == 1);
    node.advance(30 * 60);
    assert_eq!(node.manager.score(LOOPBACK), 20);
    let source = source_of(&node, &peer);
    node.relay.misbehaved(source, Misbehaviour::Unsolicited);
    assert_eq!(node.manager.score(LOOPBACK), 40);
    peer.quiet(2);
    // Two stalls disconnect and add no points.
    node.relay.misbehaved(source, Misbehaviour::Stall);
    peer.quiet(3);
    node.relay.misbehaved(source, Misbehaviour::Stall);
    assert!(peer.closed());
    assert_eq!(node.manager.score(LOOPBACK), 40);
    // The node refuses the IP address while it has two stalls. One stall decays in 10 min.
    assert_eq!(
        node.manager.admit(LOOPBACK, Direction::Inbound, 0, 0),
        Err(Refusal::Stalled)
    );
    node.advance(10 * 60);
    assert_eq!(
        node.manager.admit(LOOPBACK, Direction::Inbound, 0, 0),
        Ok(())
    );
    // An invalid block from a legacy peer is a ban, also when the peer left before the
    // node finished the validation.
    let peer = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    wait_for("the peer a third time", || node.established() == 1);
    let source = source_of(&node, &peer);
    drop(peer);
    wait_for("the peer left", || node.relay.peers().is_empty());
    node.relay.misbehaved(source, Misbehaviour::InvalidBlock);
    assert!(node.manager.is_banned(LOOPBACK));
    node.relay.shutdown();
}

#[test]
fn compact_relay_failures_that_are_not_faults_cost_nothing() {
    let node = Node::new(Vec::new(), |_| {});
    let mut peer = SimPeer::connect(node.addr, 170_160, NODE_NETWORK | NODE_COMPACT_RELAY);
    assert_eq!(
        peer.recv_app(),
        LegacyMessage::CompactVer(CompactVer::CURRENT)
    );
    peer.send(&LegacyMessage::CompactVer(CompactVer::CURRENT));
    wait_for("compact relay negotiated", || {
        matches!(
            node.relay.peers().first().map(|p| p.protocol),
            Some(PeerProtocol::CompactRelay(_))
        )
    });
    // A `BlockTxn` that answers no request of this node: a late answer after a retry.
    peer.send(&LegacyMessage::Compact(Message::BlockTxn(BlockTxn {
        block_hash: BlockHash([7; 32]),
        txs: Vec::new(),
    })));
    peer.quiet(1);
    // A block that consensus rejects, from a peer that forwards before it validates.
    node.relay
        .misbehaved(source_of(&node, &peer), Misbehaviour::InvalidBlock);
    peer.quiet(2);
    assert_eq!(node.manager.score(LOOPBACK), 0);
    assert_eq!(node.relay.peers().len(), 1);
    node.relay.shutdown();
}

/// A `block` message with one minimal v5 transaction and a header of version `version`.
fn block_message(version: u32) -> LegacyMessage {
    let mut tx = Vec::with_capacity(25);
    tx.extend_from_slice(&0x8000_0005u32.to_le_bytes());
    tx.extend_from_slice(&0x26A7_270Au32.to_le_bytes());
    tx.extend_from_slice(&0xC2D6_D0B4u32.to_le_bytes());
    tx.extend_from_slice(&[0; 13]);
    let parsed = RawTx::parse(Bytes::from(tx.clone()), BranchId::Nu5).expect("minimal v5");
    let header = BlockHeader {
        version,
        prev_hash: BlockHash([1; 32]),
        merkle_root: hayai_wire::merkle_root(&[parsed.txid]),
        block_commitments: [0x44; 32],
        time: 1_700_000_000,
        bits: 0x1f07_ffff,
        nonce: [0x55; 32],
        solution: vec![0x66; NET.pow().solution_len()],
    };
    let mut bytes = header.serialize().to_vec();
    bytes.push(1);
    bytes.extend_from_slice(&tx);
    LegacyMessage::Block(Bytes::from(bytes))
}

#[test]
fn a_header_without_proof_of_work_bans_and_an_unknown_parent_costs_nothing() {
    let node = Node::new(Vec::new(), |_| {});
    let mut peer = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    // A valid header, then a header whose parent this node does not know.
    peer.send(&block_message(4));
    peer.send(&block_message(6));
    peer.quiet(1);
    assert_eq!(node.manager.score(LOOPBACK), 0);
    peer.send(&block_message(5));
    assert!(peer.closed());
    assert!(node.manager.is_banned(LOOPBACK));
    node.relay.shutdown();
}

#[test]
fn a_peer_that_sends_the_nonce_back_cannot_remove_another_address() {
    let node = Node::new(Vec::new(), |_| {});
    node.manager.add_peers(&[public(77)]);
    let mut peer = SimPeer::over(TcpStream::connect(node.addr).unwrap());
    let LegacyMessage::Version(theirs) = peer.recv() else {
        panic!("expected version first");
    };
    // The node takes its own nonce as a connection to itself and closes. The address in
    // the message is not used.
    let LegacyMessage::Version(mut echo) = version(170_160, NODE_NETWORK, public(77)) else {
        unreachable!()
    };
    echo.nonce = theirs.nonce;
    peer.send(&LegacyMessage::Version(echo));
    assert!(peer.closed());
    assert!(node.manager.book().get(&public(77)).is_some());
    assert_eq!(node.manager.score(LOOPBACK), 0);
    // A peer can still tell that address.
    let mut other = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    node.manager.book().remove(&public(77));
    other.send(&LegacyMessage::Addr(vec![timed(public(77), NOW - 3_600)]));
    other.quiet(1);
    assert!(node.manager.book().get(&public(77)).is_some());
    node.relay.shutdown();
}

// ----- address messages -----

#[test]
fn getaddr_is_answered_once_to_an_inbound_peer() {
    let node = Node::new(Vec::new(), |_| {});
    let known: Vec<SocketAddr> = (1..=20).map(public).collect();
    node.manager.add_peers(&known);
    for addr in &known {
        node.manager.book().mark_success(addr, NODE_NETWORK, NOW);
    }
    let mut peer = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    peer.send(&LegacyMessage::GetAddr);
    let LegacyMessage::Addr(addrs) = peer.recv_app() else {
        panic!("expected addr");
    };
    // 23 % of 20, rounded up.
    assert_eq!(addrs.len(), 5);
    for a in &addrs {
        assert!(known.contains(&a.net.addr));
        assert_eq!(u64::from(a.time), NOW / 1800 * 1800);
    }
    // The second `getaddr` on the connection gets no answer.
    peer.send(&LegacyMessage::GetAddr);
    peer.quiet(1);
    // Another connection gets its own answer.
    let mut other = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    other.send(&LegacyMessage::GetAddr);
    assert!(matches!(other.recv_app(), LegacyMessage::Addr(a) if a.len() == 5));
    node.relay.shutdown();
}

#[test]
fn an_outbound_peer_is_asked_for_addresses_and_its_own_getaddr_is_ignored() {
    let listener = TcpListener::bind("127.9.0.1:0").unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let node = Node::new(Vec::new(), |c| c.book.max_per_source = 4096);
    node.manager.add_peers(&[public(200)]);
    node.manager
        .book()
        .mark_success(&public(200), NODE_NETWORK, NOW);
    node.relay.connect(remote_addr).unwrap();
    let mut peer = SimPeer::over(listener.accept().unwrap().0);
    peer.handshake(node.addr, 170_160, NODE_NETWORK);
    // The node asks its new outbound peer.
    assert_eq!(peer.recv_app(), LegacyMessage::GetAddr);
    // It does not answer the `getaddr` of an outbound peer.
    peer.send(&LegacyMessage::GetAddr);
    peer.quiet(1);
    // The answer to the node's `getaddr` can hold a full message. The book takes it with
    // the peer as the source.
    let answer: Vec<TimedNetAddr> = (0..1000u32)
        .map(|i| {
            let ip = Ipv4Addr::from(0x0c00_0000 + i);
            timed(SocketAddr::new(ip.into(), 8233), NOW - 60)
        })
        .collect();
    peer.send(&LegacyMessage::Addr(answer.clone()));
    peer.quiet(2);
    assert_eq!(node.manager.book().len(), 1001);
    let entry = *node.manager.book().get(&answer[0].net.addr).unwrap();
    assert_eq!(entry.source, remote_addr.ip());
    assert_eq!(entry.last_seen, NOW - 60 - GOSSIP_PENALTY_SECS);
    // A second full message was not asked for: the budget of the connection lets one
    // address in.
    let flood: Vec<TimedNetAddr> = (0..1000u32)
        .map(|i| {
            let ip = Ipv4Addr::from(0x0d00_0000 + i);
            timed(SocketAddr::new(ip.into(), 8233), NOW - 60)
        })
        .collect();
    peer.send(&LegacyMessage::Addr(flood.clone()));
    peer.quiet(3);
    assert_eq!(node.manager.book().len(), 1002);
    peer.send(&LegacyMessage::Addr(flood));
    peer.quiet(4);
    assert_eq!(node.manager.book().len(), 1002);
    // The budget grows again with time: one address each ten seconds.
    node.advance(30);
    let more: Vec<TimedNetAddr> = (0..10u32)
        .map(|i| {
            let ip = Ipv4Addr::from(0x0e00_0000 + i);
            timed(SocketAddr::new(ip.into(), 8233), NOW - 60)
        })
        .collect();
    peer.send(&LegacyMessage::AddrV2(more));
    peer.quiet(5);
    assert_eq!(node.manager.book().len(), 1005);
    node.relay.shutdown();
}

#[test]
fn a_fresh_unsolicited_address_goes_on_to_two_peers_once() {
    let node = Node::new(Vec::new(), |_| {});
    let mut a = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    let mut b = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    let mut c = SimPeer::connect(node.addr, 170_160, NODE_NETWORK);
    wait_for("three peers", || node.established() == 3);
    let fresh = timed(public(50), NOW - 30);
    a.send(&LegacyMessage::Addr(vec![fresh]));
    assert_eq!(b.recv_app(), LegacyMessage::Addr(vec![fresh]));
    assert_eq!(c.recv_app(), LegacyMessage::Addr(vec![fresh]));
    a.quiet(1);
    assert!(node.manager.book().get(&public(50)).is_some());
    // The same announcement from another peer is not news: it does not travel again.
    b.send(&LegacyMessage::Addr(vec![fresh]));
    b.quiet(2);
    a.quiet(3);
    c.quiet(4);
    // An old address enters the book and does not travel. `addrv2` is handled as `addr`.
    let old = timed(public(51), NOW - 3_600);
    c.send(&LegacyMessage::AddrV2(vec![old]));
    c.quiet(5);
    assert!(node.manager.book().get(&public(51)).is_some());
    a.quiet(6);
    b.quiet(7);
    node.relay.shutdown();
}
