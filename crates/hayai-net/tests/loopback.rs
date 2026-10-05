//! In-process peers over 127.0.0.1: negotiation outcomes and the both-paths relay.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_net::codec::{
    encode, read_message, GetHeaders, InvItem, LegacyMessage, NetAddr, Network, VersionMessage,
    FRAME_HEADER_LEN,
};
use hayai_net::protocol::{
    features, CompactVer, Negotiated, PeerProtocol, NODE_COMPACT_RELAY, NODE_NETWORK,
};
use hayai_net::relay::{
    BlockSink, ChainSource, HistoryRootSource, IncomingBlock, Relay, RelayConfig, RelayCounters,
    RelayDeps, Source, SyncEvent, SyncSink, TxSink,
};
use hayai_relay::{
    BatchAnnounce, BatchId, BlockTxn, BlockTxnRequest, CompactBlock, FullId, HeaderCheck,
    HeaderError, IdForm, LaneStore, Message, TxRequest,
};
use hayai_wire::header::{BlockHash, BlockHeader};
use hayai_wire::{RawBlock, RawTx, TxLookup, WtxId};

const NET: Network = Network::Regtest;
const BRANCH: BranchId = BranchId::Nu5;

/// A v5 transaction with no bundles and expiry height `n`; 25 bytes, distinct txids.
fn tx_bytes(n: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(25);
    b.extend_from_slice(&0x8000_0005u32.to_le_bytes());
    b.extend_from_slice(&0x26A7_270Au32.to_le_bytes());
    b.extend_from_slice(&0xC2D6_D0B4u32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&n.to_le_bytes());
    b.extend_from_slice(&[0, 0, 0, 0, 0]);
    b
}

fn tx(n: u32) -> Arc<RawTx> {
    Arc::new(RawTx::parse(Bytes::from(tx_bytes(n)), BRANCH).expect("minimal v5 transaction"))
}

fn block(prev: u8, txs: &[Arc<RawTx>]) -> RawBlock {
    block_with_commitments(prev, txs, [0x44; 32])
}

/// A block whose `hashBlockCommitments` folds `history` with the auth data root of `txs`.
fn block_under_history(prev: u8, txs: &[Arc<RawTx>], history: &[u8; 32]) -> RawBlock {
    let digests: Vec<[u8; 32]> = txs.iter().map(|t| t.auth_digest).collect();
    let auth = hayai_wire::auth_data_root(&digests);
    block_with_commitments(prev, txs, hayai_wire::block_commitments(history, &auth))
}

fn block_with_commitments(prev: u8, txs: &[Arc<RawTx>], block_commitments: [u8; 32]) -> RawBlock {
    let txids: Vec<_> = txs.iter().map(|t| t.txid).collect();
    let header = BlockHeader {
        version: 4,
        prev_hash: BlockHash([prev; 32]),
        merkle_root: hayai_wire::merkle_root(&txids),
        block_commitments,
        time: 1_700_000_000,
        bits: 0x1f07_ffff,
        nonce: [0x55; 32],
        solution: vec![0x66; 1344],
    };
    let mut bytes = header.serialize().to_vec();
    bytes.push(u8::try_from(txs.len()).expect("small block"));
    for t in txs {
        bytes.extend_from_slice(&t.bytes);
    }
    RawBlock::parse(Bytes::from(bytes), BRANCH).expect("block parses")
}

#[derive(Default)]
struct Store {
    txs: Mutex<HashMap<WtxId, Arc<RawTx>>>,
    received: Mutex<Vec<(WtxId, Source)>>,
}

impl Store {
    fn insert(&self, t: &Arc<RawTx>) {
        self.txs.lock().unwrap().insert(t.wtxid(), t.clone());
    }
}

impl TxLookup for Store {
    fn get(&self, id: &WtxId) -> Option<Arc<RawTx>> {
        self.txs.lock().unwrap().get(id).cloned()
    }
    fn for_each_id(&self, f: &mut dyn FnMut(&WtxId)) {
        self.txs.lock().unwrap().keys().for_each(f);
    }
    fn len(&self) -> usize {
        self.txs.lock().unwrap().len()
    }
}

impl TxSink for Store {
    fn accept_tx(&self, tx: Arc<RawTx>, source: Source) -> bool {
        let id = tx.wtxid();
        let None = self.txs.lock().unwrap().insert(id, tx) else {
            return false;
        };
        self.received.lock().unwrap().push((id, source));
        true
    }
}

/// Blocks handed to the sink, with the moment each arrived, and the relay that the sink
/// tells of the validation.
#[derive(Default)]
struct Blocks(
    Mutex<Vec<IncomingBlock>>,
    Mutex<Vec<Instant>>,
    Mutex<Option<std::sync::Weak<Relay>>>,
);

impl BlockSink for Blocks {
    fn accept_block(&self, block: IncomingBlock) {
        let hash = block.block.hash();
        self.1.lock().unwrap().push(Instant::now());
        self.0.lock().unwrap().push(block);
        // The node of these tests validates a block at once.
        let relay = self
            .2
            .lock()
            .unwrap()
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        if let Some(relay) = relay {
            relay.block_validated(&hash);
        }
    }
}

/// One history root for every parent, or none.
struct Roots(Option<[u8; 32]>);

impl HistoryRootSource for Roots {
    fn history_root(&self, _: &BlockHash) -> Option<[u8; 32]> {
        self.0
    }
}

/// The tip of [`Chain`].
const TIP: BlockHash = BlockHash([0x77; 32]);

/// The locator hash after which [`Chain`] has two blocks.
const SERVED_LOCATOR: BlockHash = BlockHash([0xb0; 32]);

fn served_headers() -> Vec<BlockHeader> {
    vec![block(0xb0, &[tx(1)]).header, block(0xb1, &[tx(2)]).header]
}

struct Chain;

impl ChainSource for Chain {
    fn tip_height(&self) -> u32 {
        7
    }
    fn tip_hash(&self) -> BlockHash {
        TIP
    }
    fn tx_branch(&self) -> Option<BranchId> {
        Some(BranchId::Nu5)
    }
    fn block_branch(&self, _: &BlockHash) -> Option<BranchId> {
        Some(BranchId::Nu5)
    }
    fn headers_after(&self, locator: &[BlockHash], _: &BlockHash, _: bool) -> Vec<BlockHeader> {
        match locator.first() {
            Some(hash) if *hash == SERVED_LOCATOR => served_headers(),
            _ => Vec::new(),
        }
    }
    fn block_bytes(&self, _: &BlockHash) -> Option<Bytes> {
        None
    }
}

struct AcceptAll;

impl HeaderCheck for AcceptAll {
    fn check(&self, _: &BlockHeader) -> Result<(), HeaderError> {
        Ok(())
    }
}

struct Node {
    relay: Arc<Relay>,
    store: Arc<Store>,
    blocks: Arc<Blocks>,
    addr: SocketAddr,
}

fn config(compact: Option<CompactVer>) -> RelayConfig {
    let mut c = RelayConfig::new(NET);
    c.compact_relay = compact;
    c.tick = Duration::from_millis(20);
    c.ping_interval = Duration::from_millis(300);
    c.ping_timeout = Duration::from_secs(3);
    c.handshake_timeout = Duration::from_secs(3);
    // The tests read each message of a connection: only one test has the `mempool` poll.
    c.mempool_poll = None;
    c
}

fn node(compact: Option<CompactVer>) -> Node {
    node_with(config(compact))
}

fn node_with(config: RelayConfig) -> Node {
    node_full(config, None)
}

fn node_full(config: RelayConfig, history_root: Option<[u8; 32]>) -> Node {
    node_synced(config, history_root, None)
}

/// The messages of the block synchronization that a relay gave to its sink.
#[derive(Default)]
struct Synced(Mutex<Vec<SyncEvent>>);

impl SyncSink for Synced {
    fn on_sync(&self, event: SyncEvent) {
        self.0.lock().unwrap().push(event);
    }
}

fn node_synced(
    config: RelayConfig,
    history_root: Option<[u8; 32]>,
    sync: Option<Arc<dyn SyncSink>>,
) -> Node {
    let store = Arc::new(Store::default());
    let blocks = Arc::new(Blocks::default());
    let relay = Relay::new(
        config,
        RelayDeps {
            txs: store.clone(),
            tx_sink: store.clone(),
            block_sink: blocks.clone(),
            chain: Arc::new(Chain),
            header_check: Arc::new(AcceptAll),
            history_roots: Arc::new(Roots(history_root)),
            sync,
        },
    );
    *blocks.2.lock().unwrap() = Some(Arc::downgrade(&relay));
    let addr = relay.listen("127.0.0.1:0").unwrap();
    Node {
        relay,
        store,
        blocks,
        addr,
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_protocols(a: &Node, b: &Node, expected: PeerProtocol) {
    wait_for("both peers established", || {
        let pa = a.relay.peers();
        let pb = b.relay.peers();
        pa.len() == 1
            && pb.len() == 1
            && pa[0].established
            && pb[0].established
            && pa[0].protocol == expected
            && pb[0].protocol == expected
    });
}

fn compact(version: u16) -> PeerProtocol {
    PeerProtocol::CompactRelay(Negotiated {
        version,
        features: features::KNOWN,
    })
}

fn no_blocks(n: &Node) -> bool {
    n.blocks.0.lock().unwrap().is_empty()
}

#[test]
fn hayai_to_hayai_relays_compact_blocks_and_transactions() {
    let a = node(Some(CompactVer::CURRENT));
    let b = node(Some(CompactVer::CURRENT));
    a.relay.connect(b.addr).unwrap();
    wait_protocols(&a, &b, compact(2));

    // A transaction announced on A reaches B's store through TxAnnounce/TxRequest/Tx.
    let t1 = tx(1);
    a.store.insert(&t1);
    a.relay.announce_tx(&t1);
    wait_for("tx on b", || b.store.get(&t1.wtxid()).is_some());
    let (_, source) = b.store.received.lock().unwrap()[0];
    assert!(matches!(
        source,
        Source::Peer {
            protocol: PeerProtocol::CompactRelay(_),
            ..
        }
    ));
    // And one from B reaches A the same way.
    let t2 = tx(2);
    b.store.insert(&t2);
    b.relay.announce_tx(&t2);
    wait_for("tx on a", || a.store.get(&t2.wtxid()).is_some());

    // A block of known transactions travels as a compact block and is byte-identical.
    let blk = block(1, &[tx(100), t1.clone(), t2.clone()]);
    a.relay.block_found(blk.clone());
    wait_for("block on b", || !b.blocks.0.lock().unwrap().is_empty());
    let got = b.blocks.0.lock().unwrap()[0].clone();
    assert_eq!(got.block.bytes, blk.bytes);
    assert!(matches!(
        got.source,
        Source::Peer {
            protocol: PeerProtocol::CompactRelay(_),
            ..
        }
    ));
    // A's own sink saw it once, as local.
    let local = a.blocks.0.lock().unwrap();
    assert_eq!(local.len(), 1);
    assert_eq!(local[0].source, Source::Local);
    drop(local);

    // A block with a transaction A never announced (and does not hold) is prefilled.
    let blk2 = block(2, &[tx(101), tx(3), t1.clone()]);
    a.relay.block_found(blk2.clone());
    wait_for("second block on b", || {
        b.blocks.0.lock().unwrap().len() == 2
    });
    assert_eq!(b.blocks.0.lock().unwrap()[1].block.bytes, blk2.bytes);

    a.relay.shutdown();
    b.relay.shutdown();
}

#[test]
fn legacy_to_legacy_uses_inv_getdata_block() {
    let a = node(None);
    let b = node(Some(CompactVer::CURRENT));
    b.relay.connect(a.addr).unwrap();
    wait_protocols(&a, &b, PeerProtocol::Legacy);

    let t1 = tx(10);
    a.store.insert(&t1);
    a.relay.announce_tx(&t1);
    wait_for("tx on b", || b.store.get(&t1.wtxid()).is_some());
    let (_, source) = b.store.received.lock().unwrap()[0];
    assert!(matches!(
        source,
        Source::Peer {
            protocol: PeerProtocol::Legacy,
            ..
        }
    ));
    let t2 = tx(11);
    b.store.insert(&t2);
    b.relay.announce_tx(&t2);
    wait_for("tx on a", || a.store.get(&t2.wtxid()).is_some());

    let blk = block(3, &[tx(102), t1, t2]);
    a.relay.block_found(blk.clone());
    wait_for("block on b", || !b.blocks.0.lock().unwrap().is_empty());
    let got = b.blocks.0.lock().unwrap()[0].clone();
    assert_eq!(got.block.bytes, blk.bytes);
    assert!(matches!(
        got.source,
        Source::Peer {
            protocol: PeerProtocol::Legacy,
            ..
        }
    ));
    a.relay.shutdown();
    b.relay.shutdown();
}

#[test]
fn version_range_mismatch_stays_legacy() {
    let a = node(Some(CompactVer {
        max_version: 3,
        min_version: 3,
        features: features::KNOWN,
    }));
    let b = node(Some(CompactVer::CURRENT));
    a.relay.connect(b.addr).unwrap();
    wait_protocols(&a, &b, PeerProtocol::Legacy);
    let blk = block(4, &[tx(103)]);
    b.relay.block_found(blk.clone());
    wait_for("block on a", || !a.blocks.0.lock().unwrap().is_empty());
    assert_eq!(a.blocks.0.lock().unwrap()[0].block.bytes, blk.bytes);
    a.relay.shutdown();
    b.relay.shutdown();
}

/// A peer driven by hand through the codec: what zcashd or Zebra would see.
struct SimPeer {
    stream: TcpStream,
}

impl SimPeer {
    fn connect(addr: SocketAddr, services: u64) -> Self {
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut peer = Self { stream };
        let LegacyMessage::Version(their_version) = peer.recv() else {
            panic!("expected version first");
        };
        assert_eq!(their_version.user_agent, "/hayai:0.1.0/");
        assert_eq!(their_version.start_height, 7);
        peer.send(&LegacyMessage::Version(VersionMessage {
            version: 170_160,
            services,
            timestamp: 0,
            addr_recv: NetAddr { services: 0, addr },
            addr_from: NetAddr {
                services,
                addr: ([0, 0, 0, 0], 0).into(),
            },
            nonce: 0x1234,
            user_agent: "/MagicBean:6.0.0/".into(),
            start_height: 0,
            relay: true,
        }));
        assert_eq!(peer.recv(), LegacyMessage::Verack);
        peer.send(&LegacyMessage::Verack);
        peer
    }

    /// Connects and negotiates the compact relay extension with `n` at the current
    /// version.
    fn connect_compact(n: &Node) -> Self {
        Self::connect_compact_offering(n, CompactVer::CURRENT, 2)
    }

    fn connect_compact_offering(n: &Node, offer: CompactVer, expected: u16) -> Self {
        let mut peer = Self::connect(n.addr, NODE_NETWORK | NODE_COMPACT_RELAY);
        assert_eq!(
            peer.recv_app(),
            LegacyMessage::CompactVer(CompactVer::CURRENT)
        );
        peer.send(&LegacyMessage::CompactVer(offer));
        let me = peer.stream.local_addr().unwrap();
        wait_for("sim peer upgraded", || {
            n.relay
                .peers()
                .iter()
                .any(|p| p.addr == me && p.protocol == compact(expected))
        });
        peer
    }

    /// A ping round trip: everything the node sent before is in the stream ahead of the
    /// pong, so `recv_app` returning the pong proves nothing else was sent.
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

    fn send(&mut self, m: &LegacyMessage) {
        use std::io::Write;
        self.stream.write_all(&encode(NET, m)).unwrap();
    }

    fn recv(&mut self) -> LegacyMessage {
        read_message(&mut self.stream, NET, usize::MAX).expect("message")
    }

    /// The next message that is not a ping (answered) or pong. The pings of the node have
    /// no end, so a message that does not come is a panic after 20 s, not a wait.
    fn recv_app(&mut self) -> LegacyMessage {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "no application message in 20 s");
            match self.recv() {
                LegacyMessage::Ping(n) => self.send(&LegacyMessage::Pong(n)),
                LegacyMessage::Pong(_) => {}
                other => return other,
            }
        }
    }
}

#[test]
fn simulated_legacy_peer_with_the_bit_gets_zcmpctver_then_legacy_relay() {
    let n = node(Some(CompactVer::CURRENT));
    let mut peer = SimPeer::connect(n.addr, NODE_NETWORK | NODE_COMPACT_RELAY);
    let hello = peer.recv_app();
    assert_eq!(hello, LegacyMessage::CompactVer(CompactVer::CURRENT));
    // Never answer: the peer stays legacy and gets the legacy path.
    let t = tx(20);
    n.store.insert(&t);
    let blk = block(5, &[tx(104), t.clone()]);
    n.relay.block_found(blk.clone());
    let inv = peer.recv_app();
    assert_eq!(inv, LegacyMessage::Inv(vec![InvItem::Block(blk.hash())]));
    assert_eq!(n.relay.peers()[0].protocol, PeerProtocol::Legacy);
    peer.send(&LegacyMessage::GetData(vec![
        InvItem::Block(blk.hash()),
        InvItem::Wtx(t.wtxid()),
        InvItem::Block(BlockHash([9; 32])),
    ]));
    assert_eq!(peer.recv_app(), LegacyMessage::Block(blk.bytes.clone()));
    assert_eq!(peer.recv_app(), LegacyMessage::Tx(t.bytes.clone()));
    assert_eq!(
        peer.recv_app(),
        LegacyMessage::NotFound(vec![InvItem::Block(BlockHash([9; 32]))])
    );
    // A transaction sent as `tx` lands in the store with a legacy source.
    let t2 = tx(21);
    peer.send(&LegacyMessage::Tx(t2.bytes.clone()));
    wait_for("tx in store", || n.store.get(&t2.wtxid()).is_some());
    // A full `block` from the legacy peer goes through the sink exactly once.
    let blk2 = block(6, &[tx(105), t2.clone()]);
    peer.send(&LegacyMessage::Block(blk2.bytes.clone()));
    peer.send(&LegacyMessage::Block(blk2.bytes.clone()));
    wait_for("block in sink", || n.blocks.0.lock().unwrap().len() == 2);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(n.blocks.0.lock().unwrap().len(), 2);
    // `mempool` answers with the store's inventory.
    peer.send(&LegacyMessage::Mempool);
    let LegacyMessage::Inv(items) = peer.recv_app() else {
        panic!("expected inv");
    };
    assert_eq!(items.len(), 2);
    assert!(items.contains(&InvItem::Wtx(t.wtxid())));

    // A malformed frame disconnects the peer; nothing panics and the relay lives on.
    let mut garbage = encode(NET, &LegacyMessage::Ping(1));
    garbage[FRAME_HEADER_LEN] ^= 0xff;
    peer.send(&LegacyMessage::Unknown {
        command: *b"garbage\0\0\0\0\0",
        payload: Bytes::from(garbage),
    });
    use std::io::Write;
    let mut raw = encode(NET, &LegacyMessage::Ping(2));
    raw[22] ^= 1;
    peer.stream.write_all(&raw).unwrap();
    wait_for("peer dropped", || n.relay.peers().is_empty());
    let mut rest = Vec::new();
    use std::io::Read;
    let _ = peer.stream.read_to_end(&mut rest);
    assert!(!n.relay.peers().iter().any(|p| p.established));
    n.relay.shutdown();
}

#[test]
fn simulated_legacy_peer_without_the_bit_never_sees_the_extension() {
    let n = node(Some(CompactVer::CURRENT));
    let mut peer = SimPeer::connect(n.addr, NODE_NETWORK);
    // Wait through a keepalive round: nothing but pings.
    peer.send(&LegacyMessage::Ping(77));
    assert_eq!(peer.recv(), LegacyMessage::Pong(77));
    wait_for("established", || n.relay.peers()[0].established);
    let blk = block(7, &[tx(106)]);
    n.relay.block_found(blk.clone());
    assert_eq!(
        peer.recv_app(),
        LegacyMessage::Inv(vec![InvItem::Block(blk.hash())])
    );
    // A stray `zcmpct` from this legacy peer is ignored, not fatal.
    peer.send(&LegacyMessage::Compact(Message::TxRequest(
        hayai_relay::TxRequest { ids: vec![] },
    )));
    peer.send(&LegacyMessage::Ping(78));
    assert_eq!(peer.recv(), LegacyMessage::Pong(78));
    assert_eq!(n.relay.peers()[0].protocol, PeerProtocol::Legacy);
    n.relay.shutdown();
}

#[test]
fn simulated_peer_with_unknown_feature_bits_negotiates() {
    let n = node(Some(CompactVer::CURRENT));
    let mut peer = SimPeer::connect(n.addr, NODE_NETWORK | NODE_COMPACT_RELAY);
    assert_eq!(
        peer.recv_app(),
        LegacyMessage::CompactVer(CompactVer::CURRENT)
    );
    peer.send(&LegacyMessage::CompactVer(CompactVer {
        max_version: 9,
        min_version: 1,
        features: features::COMPACT_BLOCKS_V1 | (1 << 50),
    }));
    wait_for("upgraded", || {
        n.relay.peers()[0].protocol
            == PeerProtocol::CompactRelay(Negotiated {
                version: 2,
                features: features::COMPACT_BLOCKS_V1,
            })
    });
    // Now a block arrives as a compact block, and a lane batch is not sent (no lanes bit).
    let blk = block(8, &[tx(107)]);
    n.relay.block_found(blk.clone());
    let LegacyMessage::Compact(Message::CompactBlock(cb)) = peer.recv_app() else {
        panic!("expected a compact block");
    };
    assert_eq!(&cb.header[..], &blk.bytes[..1487]);
    assert_eq!(cb.prefilled.len(), 1, "coinbase prefilled");
    n.relay.announce_batch(hayai_relay::BatchAnnounce {
        lane_id: [1; 32],
        seq: 1,
        batch_id: hayai_relay::BatchId::compute(&[tx(107).wtxid()]),
        ids: vec![tx(107).wtxid()],
    });
    peer.send(&LegacyMessage::Ping(5));
    assert_eq!(peer.recv(), LegacyMessage::Pong(5));
    n.relay.shutdown();
}

#[test]
fn a_block_seen_on_two_paths_is_forwarded_once() {
    // A --compact--> B <--legacy-- sim peer. The same block reaches B from A and locally.
    let a = node(Some(CompactVer::CURRENT));
    let b = node(Some(CompactVer::CURRENT));
    a.relay.connect(b.addr).unwrap();
    wait_protocols(&a, &b, compact(2));
    let mut sim = SimPeer::connect(b.addr, NODE_NETWORK);
    wait_for("sim established", || {
        b.relay.peers().iter().filter(|p| p.established).count() == 2
    });
    let t = tx(30);
    a.store.insert(&t);
    b.store.insert(&t);
    let blk = block(9, &[tx(108), t]);
    a.relay.block_found(blk.clone());
    b.relay.block_found(blk.clone());
    assert_eq!(
        sim.recv_app(),
        LegacyMessage::Inv(vec![InvItem::Block(blk.hash())])
    );
    wait_for("b sink", || !b.blocks.0.lock().unwrap().is_empty());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(b.blocks.0.lock().unwrap().len(), 1, "one sink call");
    assert_eq!(a.blocks.0.lock().unwrap().len(), 1);
    // Nothing else reached the legacy peer.
    sim.send(&LegacyMessage::Ping(1));
    assert_eq!(sim.recv(), LegacyMessage::Pong(1));
    a.relay.shutdown();
    b.relay.shutdown();
}

#[test]
fn disabled_extension_advertises_no_bit() {
    let n = node(None);
    let stream = TcpStream::connect(n.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut stream = stream;
    let LegacyMessage::Version(v) = read_message(&mut stream, NET, usize::MAX).unwrap() else {
        panic!("version");
    };
    assert_eq!(v.services & NODE_COMPACT_RELAY, 0);
    assert_eq!(v.services, NODE_NETWORK);
    let enabled = node(Some(CompactVer::CURRENT));
    let mut stream = TcpStream::connect(enabled.addr).unwrap();
    let LegacyMessage::Version(v) = read_message(&mut stream, NET, usize::MAX).unwrap() else {
        panic!("version");
    };
    assert_eq!(v.services, NODE_NETWORK | NODE_COMPACT_RELAY);
    n.relay.shutdown();
    enabled.relay.shutdown();
}

/// A compact block of `blk` as a peer that assumes every transaction is known.
fn compact_of(blk: &RawBlock, nonce: u64) -> LegacyMessage {
    compact_with(blk, nonce, |_, _| IdForm::Short)
}

fn compact_with(
    blk: &RawBlock,
    nonce: u64,
    form: impl FnMut(usize, &WtxId) -> IdForm,
) -> LegacyMessage {
    LegacyMessage::Compact(Message::CompactBlock(Box::new(CompactBlock::from_block(
        blk,
        &[],
        form,
        nonce,
    ))))
}

fn tx_request(ids: Vec<WtxId>) -> LegacyMessage {
    LegacyMessage::Compact(Message::TxRequest(TxRequest { ids }))
}

fn tx_message(txs: &[&Arc<RawTx>]) -> LegacyMessage {
    LegacyMessage::Compact(Message::Tx(hayai_relay::Tx {
        txs: txs.iter().map(|t| t.bytes.clone()).collect(),
    }))
}

fn block_txn_request(blk: &RawBlock, indexes: Vec<u32>) -> LegacyMessage {
    LegacyMessage::Compact(Message::BlockTxnRequest(BlockTxnRequest::for_missing(
        blk.hash(),
        indexes,
    )))
}

/// A short-id collision (here: a stale store entry under the right WtxId) makes the
/// reconstructed body fail the merkle check. BIP 152 behaviour: the full block is requested
/// from the sender, who is not disconnected, and the block is then forwarded to other
/// compact peers re-keyed with this node's own nonce and short ids.
#[test]
fn merkle_mismatch_requests_the_full_block_and_forwards_re_keyed() {
    let n = node(Some(CompactVer::CURRENT));
    let mut sender = SimPeer::connect_compact(&n);
    let mut other = SimPeer::connect_compact(&n);
    let t = tx(40);
    let stale = tx(41);
    n.store.txs.lock().unwrap().insert(t.wtxid(), stale);
    let blk = block(10, &[tx(109), t.clone()]);
    let sent_nonce = 7;
    sender.send(&compact_of(&blk, sent_nonce));
    assert_eq!(
        sender.recv_app(),
        LegacyMessage::GetData(vec![InvItem::Block(blk.hash())])
    );
    assert_eq!(n.relay.peers().len(), 2, "the sender stays connected");
    // Nothing was forwarded on the mismatching id list.
    other.quiet(1);
    assert_eq!(
        n.relay.metrics(),
        RelayCounters {
            root_mismatches: 1,
            ..RelayCounters::default()
        }
    );
    sender.send(&LegacyMessage::Block(blk.bytes.clone()));
    wait_for("block in sink", || !n.blocks.0.lock().unwrap().is_empty());
    assert_eq!(n.blocks.0.lock().unwrap()[0].block.bytes, blk.bytes);

    let LegacyMessage::Compact(Message::CompactBlock(cb)) = other.recv_app() else {
        panic!("expected the forwarded compact block");
    };
    assert_eq!(&cb.header[..], &blk.bytes[..1487]);
    assert_ne!(cb.nonce, sent_nonce, "forwarded with a local nonce");
    let CompactBlock { short_ids, .. } =
        CompactBlock::from_block(&blk, &[], |_, _| IdForm::Short, sent_nonce);
    assert_ne!(
        cb.short_ids, short_ids,
        "short ids recomputed under the local nonce"
    );
    let good = Store::default();
    good.insert(&t);
    let rebuilt = hayai_relay::reconstruct(&cb, &good, &LaneStore::new(), BRANCH).unwrap();
    assert_eq!(rebuilt.bytes, blk.bytes);
    // Nothing else reached the sender; it is alive.
    sender.quiet(3);
    assert_eq!(n.relay.metrics().forwarded_after_body, 1);

    // A full id that names another transaction fails the same check before anything is
    // forwarded.
    let wrong = tx(45);
    let blk2 = block(14, &[tx(113), tx(46)]);
    let mut cb = CompactBlock::from_block(&blk2, &[], |_, _| IdForm::Full, 1);
    cb.full_ids[0].id = wrong.wtxid();
    sender.send(&LegacyMessage::Compact(Message::CompactBlock(Box::new(cb))));
    assert_eq!(
        sender.recv_app(),
        LegacyMessage::GetData(vec![InvItem::Block(blk2.hash())])
    );
    other.quiet(2);
    assert_eq!(n.relay.metrics().root_mismatches, 2);
    assert_eq!(n.relay.metrics().forwarded_on_ids, 0);
    // A root mismatch is not a fault of the sender: it has no score.
    assert_eq!(n.relay.peer_manager().score(n.addr.ip()), 0);
    assert_eq!(n.relay.peers().len(), 2);
    n.relay.shutdown();
}

/// A `BlockTxnRequest` nobody answers moves to the next peer that announced the block, then
/// to a full-block request, then the wait is dropped so a later announcement starts over.
#[test]
fn stalled_compact_blocks_are_retried_then_dropped() {
    let mut cfg = config(Some(CompactVer::CURRENT));
    cfg.pending_retry = Duration::from_millis(100);
    cfg.pending_max_age = Duration::from_secs(5);
    let n = node_with(cfg);
    let mut first = SimPeer::connect_compact(&n);
    let mut second = SimPeer::connect_compact(&n);
    let unknown = tx(42);
    let blk = block(11, &[tx(110), unknown.clone()]);
    first.send(&compact_of(&blk, 1));
    assert_eq!(first.recv_app(), block_txn_request(&blk, vec![1]));
    // The second announcement is only recorded; no request goes out for it yet.
    second.send(&compact_of(&blk, 2));
    // After the retry period the request moves to the second announcer, then falls back to
    // the full block from it.
    assert_eq!(second.recv_app(), block_txn_request(&blk, vec![1]));
    assert_eq!(
        second.recv_app(),
        LegacyMessage::GetData(vec![InvItem::Block(blk.hash())])
    );
    // Unanswered as well: the wait is dropped and a new announcement is processed afresh.
    std::thread::sleep(Duration::from_millis(300));
    first.send(&compact_of(&blk, 3));
    assert_eq!(first.recv_app(), block_txn_request(&blk, vec![1]));
    first.send(&LegacyMessage::Compact(Message::BlockTxn(BlockTxn {
        block_hash: blk.hash(),
        txs: vec![unknown.bytes.clone()],
    })));
    wait_for("block in sink", || !n.blocks.0.lock().unwrap().is_empty());
    assert_eq!(n.blocks.0.lock().unwrap()[0].block.bytes, blk.bytes);
    assert_eq!(n.relay.peers().len(), 2, "nobody was disconnected");
    // The completed block is forwarded to the other compact peer.
    let LegacyMessage::Compact(Message::CompactBlock(cb)) = second.recv_app() else {
        panic!("expected the forwarded compact block");
    };
    assert_eq!(&cb.header[..], &blk.bytes[..1487]);

    // A wait whose source peer leaves is cleared; another peer's announcement is served.
    let blk2 = block(12, &[tx(111), tx(43)]);
    first.send(&compact_of(&blk2, 4));
    assert_eq!(first.recv_app(), block_txn_request(&blk2, vec![1]));
    drop(first);
    wait_for("first peer gone", || n.relay.peers().len() == 1);
    std::thread::sleep(Duration::from_millis(100));
    second.send(&compact_of(&blk2, 5));
    assert_eq!(second.recv_app(), block_txn_request(&blk2, vec![1]));
    n.relay.shutdown();
}

/// A wait older than `pending_max_age` is dropped even while its peer is still connected.
#[test]
fn pending_compact_blocks_are_bounded_by_age() {
    let mut cfg = config(Some(CompactVer::CURRENT));
    cfg.pending_retry = Duration::from_secs(10);
    cfg.pending_max_age = Duration::from_millis(150);
    let n = node_with(cfg);
    let mut peer = SimPeer::connect_compact(&n);
    let blk = block(13, &[tx(112), tx(44)]);
    peer.send(&compact_of(&blk, 1));
    assert_eq!(peer.recv_app(), block_txn_request(&blk, vec![1]));
    std::thread::sleep(Duration::from_millis(300));
    // Re-announced after the age bound: a fresh wait, hence a fresh request.
    peer.send(&compact_of(&blk, 2));
    assert_eq!(peer.recv_app(), block_txn_request(&blk, vec![1]));
    n.relay.shutdown();
}

/// Sim peer A → node B → node C, with observer D on B. B lacks one transaction of the block
/// and C holds it. A sends the block with that transaction as a full id. B forwards on the
/// verified id list at once: C's sink receives the block before B holds the bytes, and D
/// sees the forwarded compact block with the missing transaction as a full id. B asks A for
/// the bytes with `TxRequest`; D's own `TxRequest` to B is answered once B holds them.
#[test]
fn v2_forwards_on_a_verified_id_list_before_the_body() {
    let b = node(Some(CompactVer::CURRENT));
    let c = node(Some(CompactVer::CURRENT));
    b.relay.connect(c.addr).unwrap();
    wait_protocols(&b, &c, compact(2));
    let mut a = SimPeer::connect_compact(&b);
    let mut d = SimPeer::connect_compact(&b);

    let x = tx(50);
    c.store.insert(&x);
    let blk = block(20, &[tx(120), x.clone()]);
    a.send(&compact_with(&blk, 1, |_, _| IdForm::Full));

    // B asked A for the bytes it lacks, and nothing else.
    assert_eq!(a.recv_app(), tx_request(vec![x.wtxid()]));
    // D got the block forwarded on ids: coinbase prefilled, X as a full id, a new nonce.
    let LegacyMessage::Compact(Message::CompactBlock(cb)) = d.recv_app() else {
        panic!("expected the forwarded compact block");
    };
    assert_eq!(&cb.header[..], &blk.bytes[..1487]);
    assert_ne!(cb.nonce, 1);
    assert_eq!(cb.prefilled.len(), 1);
    assert!(cb.short_ids.is_empty());
    assert_eq!(
        cb.full_ids,
        vec![FullId {
            index: 1,
            id: x.wtxid()
        }]
    );
    // C completed from its store: its sink has the block while B's is still empty.
    wait_for("block on c", || !no_blocks(&c));
    let c_at = c.blocks.1.lock().unwrap()[0];
    assert!(no_blocks(&b), "B has no bytes for X yet");
    assert_eq!(
        b.relay.metrics(),
        RelayCounters {
            forwarded_on_ids: 1,
            forwarded_without_auth_root: 1,
            ..RelayCounters::default()
        }
    );
    // D asks B for X before B holds it: the request waits.
    d.send(&tx_request(vec![x.wtxid()]));
    d.quiet(1);

    // A delivers the bytes: B completes, the far node was earlier, D's request is served.
    a.send(&tx_message(&[&x]));
    wait_for("block on b", || !no_blocks(&b));
    let b_at = b.blocks.1.lock().unwrap()[0];
    assert!(c_at < b_at, "far node first: {c_at:?} vs {b_at:?}");
    assert_eq!(b.blocks.0.lock().unwrap()[0].block.bytes, blk.bytes);
    assert_eq!(d.recv_app(), tx_message(&[&x]));
    // B then announces X like any transaction it accepted.
    assert_eq!(
        d.recv_app(),
        LegacyMessage::Compact(Message::TxAnnounce(hayai_relay::TxAnnounce {
            ids: vec![x.wtxid()]
        }))
    );
    // Afterwards the retained body answers the same request directly.
    d.send(&tx_request(vec![x.wtxid()]));
    assert_eq!(d.recv_app(), tx_message(&[&x]));
    // D, a version 2 peer, received the block only once; A, the source, never.
    d.quiet(2);
    a.quiet(3);
    assert_eq!(
        c.relay.metrics(),
        RelayCounters {
            forwarded_on_ids: 1,
            forwarded_without_auth_root: 1,
            ..RelayCounters::default()
        }
    );
    b.relay.shutdown();
    c.relay.shutdown();
}

/// With the parent's history root known, the auth data root is checked through
/// `hashBlockCommitments` before forwarding: a matching block forwards with the auth root
/// counted as checked; a block whose commitments disagree is fetched in full instead.
#[test]
fn auth_data_root_is_checked_when_the_history_root_is_known() {
    let history = [0x5a; 32];
    let n = node_full(config(Some(CompactVer::CURRENT)), Some(history));
    let mut sender = SimPeer::connect_compact(&n);
    let mut other = SimPeer::connect_compact(&n);
    let t = tx(60);
    n.store.insert(&t);

    let good = block_under_history(21, &[tx(121), t.clone()], &history);
    sender.send(&compact_of(&good, 1));
    let LegacyMessage::Compact(Message::CompactBlock(cb)) = other.recv_app() else {
        panic!("expected the forwarded compact block");
    };
    assert_eq!(&cb.header[..], &good.bytes[..1487]);
    wait_for("block in sink", || !no_blocks(&n));
    assert_eq!(
        n.relay.metrics(),
        RelayCounters {
            forwarded_on_ids: 1,
            forwarded_without_auth_root: 0,
            ..RelayCounters::default()
        }
    );

    let bad = block(22, &[tx(122), t.clone()]);
    sender.send(&compact_of(&bad, 2));
    assert_eq!(
        sender.recv_app(),
        LegacyMessage::GetData(vec![InvItem::Block(bad.hash())])
    );
    other.quiet(1);
    assert_eq!(n.relay.metrics().root_mismatches, 1);
    assert_eq!(n.relay.peers().len(), 2, "a mismatch is not a peer fault");
    n.relay.shutdown();
}

/// A → B → C where B offers version 1 only. A↔B and B↔C negotiate version 1: A sends no
/// full ids, B completes through `BlockTxn` and forwards after the body, and C receives
/// the block.
#[test]
fn v1_peer_in_the_middle_still_relays() {
    let a = node(Some(CompactVer::CURRENT));
    let b = node(Some(CompactVer::V1));
    let c = node(Some(CompactVer::CURRENT));
    a.relay.connect(b.addr).unwrap();
    wait_protocols(&a, &b, compact(1));
    b.relay.connect(c.addr).unwrap();
    wait_for("b-c at version 1", || {
        let pb = b.relay.peers();
        pb.len() == 2 && pb.iter().all(|p| p.protocol == compact(1))
    });

    // X is in A's store but never announced to B; a version 1 peer gets a short id.
    let x = tx(70);
    a.store.insert(&x);
    let blk = block(23, &[tx(123), x.clone()]);
    a.relay.block_found(blk.clone());
    wait_for("block on c", || !no_blocks(&c));
    assert_eq!(c.blocks.0.lock().unwrap()[0].block.bytes, blk.bytes);
    assert_eq!(b.blocks.0.lock().unwrap()[0].block.bytes, blk.bytes);
    // Everybody is still connected: no full ids crossed a version 1 connection.
    assert_eq!(a.relay.peers().len(), 1);
    assert_eq!(b.relay.peers().len(), 2);
    assert_eq!(c.relay.peers().len(), 1);
    assert_eq!(
        b.relay.metrics(),
        RelayCounters {
            forwarded_on_ids: 1,
            forwarded_without_auth_root: 1,
            ..RelayCounters::default()
        },
        "B verifies the ids but has no version 2 peer to forward to"
    );
    assert_eq!(c.relay.metrics().forwarded_on_ids, 1);
    a.relay.shutdown();
    b.relay.shutdown();
    c.relay.shutdown();
}

/// Full ids from a peer that negotiated version 1 are a protocol violation.
#[test]
fn full_ids_on_a_v1_connection_disconnect() {
    let n = node(Some(CompactVer::CURRENT));
    let mut peer = SimPeer::connect_compact_offering(&n, CompactVer::V1, 1);
    let blk = block(24, &[tx(124), tx(71)]);
    peer.send(&compact_with(&blk, 1, |_, _| IdForm::Full));
    wait_for("peer dropped", || n.relay.peers().is_empty());
    // A message that the negotiated version does not permit is a fault. The first one
    // disconnects the peer. The second one, before the points decay, bans the IP address.
    assert!(!n.relay.peer_manager().is_banned(n.addr.ip()));
    let mut peer = SimPeer::connect_compact_offering(&n, CompactVer::V1, 1);
    peer.send(&compact_with(&blk, 1, |_, _| IdForm::Full));
    wait_for("peer dropped again", || n.relay.peers().is_empty());
    assert!(n.relay.peer_manager().is_banned(n.addr.ip()));
    n.relay.shutdown();
}

/// A received batch is flooded once to lane peers other than its sender, and only after
/// this node holds all of its transactions.
#[test]
fn batch_announcements_flood_one_hop_once_complete() {
    let n = node(Some(CompactVer::CURRENT));
    let mut a = SimPeer::connect_compact(&n);
    let mut d = SimPeer::connect_compact(&n);
    let t1 = tx(80);
    n.store.insert(&t1);
    let first = BatchAnnounce {
        lane_id: [8; 32],
        seq: 1,
        batch_id: BatchId::compute(&[t1.wtxid()]),
        ids: vec![t1.wtxid()],
    };
    a.send(&LegacyMessage::Compact(Message::BatchAnnounce(
        first.clone(),
    )));
    assert_eq!(
        d.recv_app(),
        LegacyMessage::Compact(Message::BatchAnnounce(first.clone()))
    );
    // Re-announcing a known batch floods nothing.
    a.send(&LegacyMessage::Compact(Message::BatchAnnounce(first)));
    a.quiet(1);
    d.quiet(2);

    // A batch with a transaction the node lacks is requested first and flooded only once
    // the bytes are held.
    let t2 = tx(81);
    let second = BatchAnnounce {
        lane_id: [8; 32],
        seq: 2,
        batch_id: BatchId::compute(&[t1.wtxid(), t2.wtxid()]),
        ids: vec![t1.wtxid(), t2.wtxid()],
    };
    a.send(&LegacyMessage::Compact(Message::BatchAnnounce(
        second.clone(),
    )));
    assert_eq!(a.recv_app(), tx_request(vec![t2.wtxid()]));
    d.quiet(3);
    a.send(&tx_message(&[&t2]));
    // D learns the transaction (announced on accept) and then the batch.
    assert_eq!(
        d.recv_app(),
        LegacyMessage::Compact(Message::TxAnnounce(hayai_relay::TxAnnounce {
            ids: vec![t2.wtxid()]
        }))
    );
    assert_eq!(
        d.recv_app(),
        LegacyMessage::Compact(Message::BatchAnnounce(second))
    );
    // The sender is not flooded with its own batch.
    a.quiet(4);
    n.relay.shutdown();
}

/// A block in canonical order over `body` (no transaction spends another, so by txid).
fn canonical_block(prev: u8, coinbase: Arc<RawTx>, body: &[Arc<RawTx>]) -> RawBlock {
    let mut sorted = body.to_vec();
    sorted.sort_by(|a, b| a.txid.as_ref().cmp(b.txid.as_ref()));
    let mut txs = vec![coinbase];
    txs.extend(sorted);
    block(prev, &txs)
}

/// A publishes its template as a candidate and finds a block close to it. B (candidates)
/// receives the candidate form and rebuilds the block from the reference and the
/// difference; C (version 2 without the candidates bit) and D (legacy) behind B receive the
/// block on their own paths.
#[test]
fn candidate_blocks_cross_a_path_with_v2_and_legacy_peers() {
    let a = node(Some(CompactVer::CURRENT));
    let b = node(Some(CompactVer::CURRENT));
    let c = node(Some(CompactVer::V2_WITHOUT_CANDIDATES));
    let d = node(None);
    a.relay.connect(b.addr).unwrap();
    wait_protocols(&a, &b, compact(2));
    b.relay.connect(c.addr).unwrap();
    b.relay.connect(d.addr).unwrap();
    wait_for("b has three established peers", || {
        let peers = b.relay.peers();
        peers.len() == 3 && peers.iter().all(|p| p.established)
    });

    let body: Vec<Arc<RawTx>> = (0..12).map(|i| tx(900 + i)).collect();
    for t in &body {
        a.store.insert(t);
        b.store.insert(t);
        c.store.insert(t);
    }
    let extra = tx(950);
    a.store.insert(&extra);
    b.store.insert(&extra);
    c.store.insert(&extra);
    let parent = BlockHash([31; 32]);
    let mut lane = hayai_relay::LanePublisher::new([4; 32]);
    let ids: Vec<WtxId> = body.iter().map(|t| t.wtxid()).collect();
    a.relay.publish_candidate(lane.publish(parent, 5, &ids));
    wait_for("candidate on b", || {
        !b.relay.candidates_on(&parent, 4).is_empty()
    });
    let None = c.relay.candidates_on(&parent, 4).first() else {
        panic!("a peer without the candidates bit never receives a candidate");
    };

    // The block drops one transaction of the candidate and adds one.
    let mut block_body: Vec<Arc<RawTx>> = body[1..].to_vec();
    block_body.push(extra.clone());
    let blk = canonical_block(31, tx(951), &block_body);
    a.relay.block_found(blk.clone());
    for (name, n) in [("b", &b), ("c", &c), ("d", &d)] {
        wait_for(name, || !no_blocks(n));
        assert_eq!(
            n.blocks.0.lock().unwrap()[0].block.bytes,
            blk.bytes,
            "{name}"
        );
    }
    assert_eq!(a.relay.metrics().candidate_blocks_sent, 1);
    let at_b = b.relay.metrics();
    assert_eq!(at_b.candidate_blocks_resolved, 1);
    assert_eq!(at_b.candidate_fallbacks, 0);
    assert_eq!(c.relay.metrics().candidate_blocks_resolved, 0);
    for n in [&a, &b, &c, &d] {
        n.relay.shutdown();
    }
}

/// A candidate block naming a candidate the receiver never stored falls back to the full
/// block on the same connection; the sender is not penalized.
#[test]
fn an_unknown_candidate_falls_back_to_the_full_block() {
    let n = node(Some(CompactVer::CURRENT));
    let mut peer = SimPeer::connect_compact(&n);
    let blk = canonical_block(32, tx(960), &[tx(961), tx(962)]);
    let header = blk.bytes.slice(..blk.header.serialized_len());
    let cb = hayai_relay::CandidateBlock {
        header,
        nonce: 1,
        lane_id: [6; 32],
        seq: 1,
        flags: hayai_relay::CANONICAL_ORDER,
        coinbase: blk.txs[0].bytes.clone(),
        removed: vec![],
        short_ids: vec![],
        full_ids: vec![],
    };
    peer.send(&LegacyMessage::Compact(Message::CandidateBlock(Box::new(
        cb,
    ))));
    assert_eq!(
        peer.recv_app(),
        LegacyMessage::GetData(vec![InvItem::Block(blk.hash())])
    );
    assert_eq!(n.relay.metrics().candidate_fallbacks, 1);
    assert_eq!(n.relay.peers().len(), 1);
    n.relay.shutdown();
}

/// With a sink the node owns the block download: an announced block is an event and the
/// relay sends no `getdata`. The node asks for the block, the `block` message is an event,
/// and a block that the node gives back reaches the peers of the relay.
#[test]
fn a_sync_sink_owns_the_block_download() {
    let a = node(None);
    let events = Arc::new(Synced::default());
    let b = node_synced(config(None), None, Some(events.clone()));
    let c = node(None);
    let a_id = b.relay.connect(a.addr).unwrap();
    c.relay.connect(b.addr).unwrap();
    wait_for("three established peers", || {
        let established = |n: &Node, count| {
            let peers = n.relay.peers();
            peers.len() == count && peers.iter().all(|p| p.established)
        };
        established(&a, 1) && established(&b, 2) && established(&c, 1)
    });
    let seen = |pick: &dyn Fn(&SyncEvent) -> bool| events.0.lock().unwrap().iter().any(pick);
    assert!(seen(&|e| matches!(
        e,
        SyncEvent::PeerConnected {
            start_height: 7,
            ..
        }
    )));

    let blk = block(4, &[tx(103)]);
    let hash = blk.hash();
    a.relay.block_found(blk.clone());
    wait_for("the announcement", || {
        seen(&|e| matches!(e, SyncEvent::BlockInv { hashes, .. } if hashes == &[hash]))
    });
    // No request went out: A sent no block.
    std::thread::sleep(Duration::from_millis(100));
    assert!(!seen(&|e| matches!(e, SyncEvent::Block { .. })));
    assert!(no_blocks(&b));

    assert!(b.relay.request_blocks(a_id, &[hash]));
    wait_for("the block", || {
        seen(&|e| matches!(e, SyncEvent::Block { bytes, .. } if bytes == &blk.bytes))
    });
    // The block of a sink does not go to the block sink of the relay.
    assert!(no_blocks(&b));

    // A block that no peer has is a `notfound` event.
    let unknown = BlockHash([9; 32]);
    assert!(b.relay.request_blocks(a_id, &[unknown]));
    wait_for("notfound", || {
        seen(&|e| matches!(e, SyncEvent::NotFound { hashes, .. } if hashes == &[unknown]))
    });

    // The node gives the block back: C learns it from B, A (the source) gets no announcement.
    let source = b
        .relay
        .peers()
        .iter()
        .find(|p| p.id == a_id)
        .map(|p| Source::Peer {
            id: p.id,
            protocol: p.protocol,
            ip: p.addr.ip(),
        })
        .unwrap();
    b.relay.forward_block(Arc::new(blk.clone()), source);
    // The legacy peer c gets the announcement after the validation of the node.
    b.relay.block_validated(&hash);
    wait_for("block on c", || !c.blocks.0.lock().unwrap().is_empty());
    assert_eq!(c.blocks.0.lock().unwrap()[0].block.bytes, blk.bytes);
    assert!(no_blocks(&b));

    // `getheaders` of the node reaches the peer, and the answer is an event.
    assert!(b.relay.send_getheaders(a_id, vec![hash]));
    wait_for("headers", || {
        seen(&|e| matches!(e, SyncEvent::Headers { .. }))
    });

    a.relay.disconnect(a.relay.peers()[0].id);
    wait_for("the peer left", || {
        seen(&|e| matches!(e, SyncEvent::PeerDisconnected { peer } if *peer == a_id))
    });
    assert!(!b.relay.request_blocks(a_id, &[hash]));
    for n in [&a, &b, &c] {
        n.relay.shutdown();
    }
}

/// Zebra and Zakura read the chain of a peer with `getblocks`. The answer is one `inv` with
/// the hashes of the blocks after the locator. Without such a block the answer is the hash
/// of the tip, so a Zakura peer does not wait for its timeout and counts no stall.
#[test]
fn getblocks_is_answered_with_the_block_hashes_after_the_locator() {
    let n = node(None);
    let mut peer = SimPeer::connect(n.addr, NODE_NETWORK);
    let mut answer = |locator: BlockHash| {
        peer.send(&LegacyMessage::GetBlocks(GetHeaders {
            version: 170_160,
            locator: vec![locator],
            stop: BlockHash([0; 32]),
        }));
        loop {
            // The other messages are the messages of a new connection.
            if let LegacyMessage::Inv(items) = peer.recv_app() {
                return items;
            }
        }
    };
    let expected: Vec<InvItem> = served_headers()
        .iter()
        .map(|h| InvItem::Block(h.hash()))
        .collect();
    assert_eq!(answer(SERVED_LOCATOR), expected);
    assert_eq!(answer(BlockHash([0xc0; 32])), vec![InvItem::Block(TIP)]);
}

/// A legacy peer gets the announcement of a block after the validation, not before: zcashd,
/// Zebra and Zakura give the penalty for an invalid block to the peer that sent it.
#[test]
fn a_legacy_peer_gets_no_block_before_its_validation() {
    let n = node(None);
    // The node of this test validates no block by itself.
    *n.blocks.2.lock().unwrap() = None;
    let mut peer = SimPeer::connect(n.addr, NODE_NETWORK);
    // The node announces a block to the peers whose handshake it completed.
    wait_for("the session at the node", || {
        n.relay.peers().iter().any(|p| p.established)
    });
    let first = block(3, &[tx(102)]);
    let second = block(4, &[tx(103)]);
    for blk in [&first, &second] {
        n.relay.block_found(blk.clone());
    }
    wait_for("blocks at the node", || {
        n.blocks.0.lock().unwrap().len() == 2
    });
    let next_inv = |peer: &mut SimPeer| loop {
        // The other messages are the messages of a new connection.
        if let LegacyMessage::Inv(items) = peer.recv_app() {
            return items;
        }
    };
    // The node does not send the block to the legacy peer before the validation.
    peer.send(&LegacyMessage::GetData(vec![InvItem::Block(first.hash())]));
    loop {
        match peer.recv_app() {
            LegacyMessage::NotFound(items) => {
                assert_eq!(items, vec![InvItem::Block(first.hash())]);
                break;
            }
            LegacyMessage::Block(_) => panic!("a block before its validation"),
            _ => continue,
        }
    }
    // A block that the node did not forward has no announcement. The first announcement
    // is the block that the node validated first.
    n.relay.block_validated(&BlockHash([0xee; 32]));
    n.relay.block_validated(&second.hash());
    assert_eq!(next_inv(&mut peer), vec![InvItem::Block(second.hash())]);
    n.relay.block_validated(&first.hash());
    assert_eq!(next_inv(&mut peer), vec![InvItem::Block(first.hash())]);
    peer.send(&LegacyMessage::GetData(vec![InvItem::Block(first.hash())]));
    loop {
        if let LegacyMessage::Block(bytes) = peer.recv_app() {
            assert_eq!(bytes, first.bytes);
            break;
        }
    }
    let blk = first;
    // The second call has no second announcement.
    n.relay.block_validated(&blk.hash());
    peer.quiet(6);
}

/// The node asks a legacy peer for its mempool after the handshake and then at the
/// interval, and requests the transactions of the answer that it does not have.
#[test]
fn a_legacy_peer_gets_mempool_requests_and_its_answer_is_used() {
    let mut c = config(None);
    c.mempool_poll = Some(Duration::from_millis(200));
    let n = node_with(c);
    let mut peer = SimPeer::connect(n.addr, NODE_NETWORK);
    let mut polls = 0;
    let start = Instant::now();
    while polls < 2 {
        if let LegacyMessage::Mempool = peer.recv_app() {
            polls += 1;
        }
    }
    assert!(
        start.elapsed() >= Duration::from_millis(200),
        "two requests at the interval"
    );
    let t = tx(40);
    peer.send(&LegacyMessage::Inv(vec![InvItem::Tx(t.txid)]));
    loop {
        if let LegacyMessage::GetData(items) = peer.recv_app() {
            assert_eq!(items.len(), 1);
            break;
        }
    }
    peer.send(&LegacyMessage::Tx(t.bytes.clone()));
    wait_for("the transaction of the answer", || {
        n.store.get(&t.wtxid()).is_some()
    });
}
