//! Full mode on an in-process Regtest network: hayaid nodes over loopback, and scripted
//! legacy peers that serve a chain of generated blocks.
//!
//! Every block comes from the Regtest producer of a hayaid node. A scripted peer first
//! downloads the chain of a node over the legacy protocol, then serves it, or a changed
//! copy of it, to the node under test.

use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_net::codec::{
    encode, read_message, GetHeaders, InvItem, LegacyMessage, NetAddr, Network, VersionMessage,
    MAX_HEADERS,
};
use hayai_net::PeerProtocol;
use hayai_rpc::BlockGenerator;
use hayai_wire::header::{BlockHash, BlockHeader};
use hayai_wire::{merkle_root, RawBlock};
use serde_json::Value;

use crate::config::Config;
use crate::node::Node;
use crate::params::{NetParams, NetworkKind};

const NET: Network = Network::Regtest;
const WAIT: Duration = Duration::from_secs(60);

fn scratch() -> tempfile::TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
    std::fs::create_dir_all(&base).expect("scratch base");
    tempfile::tempdir_in(base).expect("scratch dir")
}

/// The configuration of a Regtest full node. The peer manager dials no peer: each test
/// makes its connections. `extra` is TOML text with more sections.
fn config_with(dir: &Path, name: &str, produce: bool, compact: bool, extra: &str) -> Config {
    let text = format!(
        r#"
[network]
network = "regtest"
listen_addr = "127.0.0.1:0"
compact_relay = {compact}
outbound_peers = 0

[state]
data_dir = "{data}"
flush_interval_blocks = 8

[sync]
request_timeout_ms = 1500
header_timeout_ms = 2000

[trace]
dir = "{trace}"
node = "{name}"

[mining]
miner_script = "51"
regtest_produce = {produce}

{extra}
"#,
        data = dir.join(format!("{name}-data")).display(),
        trace = dir.join(format!("{name}-trace")).display(),
    );
    Config::parse(&text).expect("test config")
}

fn start(dir: &Path, name: &str, produce: bool, compact: bool) -> Node {
    start_with(dir, name, produce, compact, "")
}

fn start_with(dir: &Path, name: &str, produce: bool, compact: bool, extra: &str) -> Node {
    Node::start(&config_with(dir, name, produce, compact, extra))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn addr(node: &Node) -> SocketAddr {
    node.p2p_addr.expect("the node listens")
}

fn generate(node: &Node, n: u32) -> Vec<BlockHash> {
    let Some(producer) = &node.producer else {
        panic!("the node is not a producer");
    };
    producer.generate(n).expect("generate")
}

fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_tip(node: &Node, tip: (u32, BlockHash)) {
    wait_for(&format!("the tip {tip:?}"), || node.tip.tip() == tip);
}

fn rows(dir: &Path, name: &str, file: &str) -> Vec<Value> {
    std::fs::read_to_string(dir.join(format!("{name}-trace")).join(file))
        .expect("trace table")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON row"))
        .collect()
}

fn parse(bytes: &Bytes) -> RawBlock {
    RawBlock::parse(bytes.clone(), BranchId::Nu5).expect("a block")
}

fn send(stream: &mut TcpStream, message: &LegacyMessage) -> std::io::Result<()> {
    stream.write_all(&encode(NET, message))
}

fn version(start_height: u32) -> LegacyMessage {
    LegacyMessage::Version(VersionMessage {
        version: 170_160,
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
        nonce: rand::random(),
        user_agent: "/scripted:1/".into(),
        start_height,
        relay: true,
    })
}

/// The next message that is not part of the keepalive or of the address exchange.
fn recv(stream: &mut TcpStream) -> Option<LegacyMessage> {
    loop {
        match read_message(stream, NET, usize::MAX).ok()? {
            LegacyMessage::Ping(nonce) => send(stream, &LegacyMessage::Pong(nonce)).ok()?,
            LegacyMessage::Pong(_)
            | LegacyMessage::GetAddr
            | LegacyMessage::Addr(_)
            | LegacyMessage::CompactVer(_) => {}
            other => return Some(other),
        }
    }
}

/// Downloads the best chain of `node` over the legacy protocol: the blocks of heights 1
/// to its tip, as wire bytes.
fn fetch_chain(node: &Node) -> Vec<Bytes> {
    let (tip, _) = node.tip.tip();
    let (genesis, _) = NetParams::new(NetworkKind::Regtest).genesis();
    let mut stream = TcpStream::connect(addr(node)).expect("connect");
    send(&mut stream, &version(0)).expect("version");
    let mut handshake = 0;
    while handshake < 2 {
        match recv(&mut stream).expect("handshake") {
            LegacyMessage::Version(_) => {
                send(&mut stream, &LegacyMessage::Verack).expect("verack");
                handshake += 1;
            }
            LegacyMessage::Verack => handshake += 1,
            other => panic!("unexpected {other:?}"),
        }
    }
    let mut hashes: Vec<BlockHash> = Vec::new();
    while hashes.len() < tip as usize {
        let last = hashes.last().copied().unwrap_or(genesis);
        let request = LegacyMessage::GetHeaders(GetHeaders {
            version: 170_160,
            locator: vec![last],
            stop: BlockHash([0; 32]),
        });
        send(&mut stream, &request).expect("getheaders");
        let headers = loop {
            if let LegacyMessage::Headers(headers) = recv(&mut stream).expect("headers") {
                break headers;
            }
        };
        assert!(!headers.is_empty(), "the node serves its whole chain");
        hashes.extend(headers.iter().map(BlockHeader::hash));
    }
    let mut blocks = Vec::with_capacity(hashes.len());
    for chunk in hashes.chunks(64) {
        let items = chunk.iter().map(|hash| InvItem::Block(*hash)).collect();
        send(&mut stream, &LegacyMessage::GetData(items)).expect("getdata");
        for hash in chunk {
            let bytes = loop {
                if let LegacyMessage::Block(bytes) = recv(&mut stream).expect("block") {
                    break bytes;
                }
            };
            assert_eq!(BlockHeader::parse(&bytes).expect("a header").hash(), *hash);
            blocks.push(bytes);
        }
    }
    blocks
}

/// What a scripted peer does with the requests of the node.
#[derive(Clone, Copy)]
struct Script {
    /// Answers `getheaders`.
    headers: bool,
    /// Blocks that the peer sends before it stops to answer `getdata`.
    blocks: usize,
    /// Answers each `getheaders` with the first headers of its chain, whatever the
    /// locator is.
    repeat_headers: bool,
    /// The height of the `version` message. `None`: the length of the chain.
    start_height: Option<u32>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            headers: true,
            blocks: usize::MAX,
            repeat_headers: false,
            start_height: None,
        }
    }
}

/// A legacy peer that serves `chain` (headers, with the body when it has one) as `script`
/// says. It listens on its own loopback address, so that its score and its ban do not
/// touch the other peers of the test.
struct ScriptedPeer {
    addr: SocketAddr,
    /// `getdata` block requests that the peer received.
    requested: Arc<AtomicUsize>,
    /// Connections that the node closed.
    closed: Arc<AtomicUsize>,
}

impl ScriptedPeer {
    fn serve(ip: [u8; 4], chain: Vec<(BlockHeader, Option<Bytes>)>, script: Script) -> Self {
        let listener = TcpListener::bind((IpAddr::from(ip), 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requested = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let (counter, closes) = (requested.clone(), closed.clone());
        thread::spawn(move || {
            let position: HashMap<BlockHash, usize> = chain
                .iter()
                .enumerate()
                .map(|(at, (header, _))| (header.hash(), at))
                .collect();
            let mut sent = 0;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let start_height = script.start_height.unwrap_or(chain.len() as u32);
                let Ok(()) = send(&mut stream, &version(start_height)) else {
                    continue;
                };
                while let Some(message) = recv(&mut stream) {
                    let answer = match message {
                        LegacyMessage::Version(_) => Some(LegacyMessage::Verack),
                        LegacyMessage::GetHeaders(request) if script.headers => {
                            let from = match script.repeat_headers {
                                true => 0,
                                false => request
                                    .locator
                                    .iter()
                                    .find_map(|hash| position.get(hash))
                                    .map_or(0, |at| at + 1),
                            };
                            let headers = chain
                                .iter()
                                .skip(from)
                                .take(MAX_HEADERS)
                                .map(|(header, _)| header.clone())
                                .collect();
                            Some(LegacyMessage::Headers(headers))
                        }
                        LegacyMessage::GetData(items) => {
                            for item in items {
                                let InvItem::Block(hash) = item else { continue };
                                counter.fetch_add(1, Ordering::Relaxed);
                                let body = position
                                    .get(&hash)
                                    .and_then(|at| chain[*at].1.clone())
                                    .filter(|_| sent < script.blocks);
                                let Some(bytes) = body else { continue };
                                sent += 1;
                                let Ok(()) = send(&mut stream, &LegacyMessage::Block(bytes)) else {
                                    break;
                                };
                            }
                            None
                        }
                        _ => None,
                    };
                    let Some(answer) = answer else { continue };
                    let Ok(()) = send(&mut stream, &answer) else {
                        break;
                    };
                }
                closes.fetch_add(1, Ordering::Relaxed);
            }
        });
        Self {
            addr,
            requested,
            closed,
        }
    }
}

fn with_bodies(blocks: &[Bytes]) -> Vec<(BlockHeader, Option<Bytes>)> {
    blocks
        .iter()
        .map(|bytes| (parse(bytes).header, Some(bytes.clone())))
        .collect()
}

/// A node synchronizes 300 blocks from the genesis block from three peers, and uses more
/// than one of them.
#[test]
fn a_node_synchronizes_from_three_peers_from_the_genesis_block() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, true);
    generate(&a, 300);
    let tip = a.tip.tip();
    assert_eq!(tip.0, 300);
    // Two more nodes with the chain: each one synchronizes from the first node.
    let b = start(dir.path(), "b", false, true);
    let c = start(dir.path(), "c", false, false);
    b.relay.connect(addr(&a)).expect("b dials a");
    c.relay.connect(addr(&a)).expect("c dials a");
    wait_tip(&b, tip);
    wait_tip(&c, tip);

    let x = start(dir.path(), "x", false, true);
    for peer in [&a, &b, &c] {
        x.relay.connect(addr(peer)).expect("x dials a peer");
    }
    wait_tip(&x, tip);
    // The node follows the tip after the synchronization.
    generate(&a, 2);
    let tip = a.tip.tip();
    for node in [&b, &c, &x] {
        wait_tip(node, tip);
    }
    for node in [a, b, c, x] {
        node.shutdown().expect("clean shutdown");
    }

    let received = rows(dir.path(), "x", "block_sync.jsonl");
    let downloads: Vec<&Value> = received
        .iter()
        .filter(|row| row["event"] == "block_received" && row["source"] == "download")
        .collect();
    assert!(
        downloads.len() >= 300,
        "{} blocks downloaded",
        downloads.len()
    );
    let mut peers: Vec<u64> = downloads
        .iter()
        .map(|row| row["peer"].as_u64().expect("a peer"))
        .collect();
    peers.sort_unstable();
    peers.dedup();
    assert!(peers.len() >= 2, "blocks came from the peers {peers:?}");
    assert!(received.iter().any(|row| row["event"] == "sync_progress"));
    let commits = rows(dir.path(), "x", "commit_state.jsonl");
    let committed = commits
        .iter()
        .filter(|row| row["event"] == "commit_finish" && row["result"] == "committed")
        .count();
    assert_eq!(committed, 302);
}

/// A copy of `block` whose coinbase pays one zatoshi more: the header matches the body,
/// and the block breaks the subsidy rule.
fn overpaying(block: &Bytes) -> Bytes {
    let raw = parse(block);
    let coinbase = &raw.txs[0];
    let value = coinbase
        .tx
        .transparent_bundle()
        .expect("a transparent coinbase")
        .vout[0]
        .value()
        .into_u64();
    let mut tx = coinbase.bytes.to_vec();
    let needle = value.to_le_bytes();
    let positions: Vec<usize> = tx
        .windows(8)
        .enumerate()
        .filter(|(_, window)| *window == needle)
        .map(|(at, _)| at)
        .collect();
    let [at] = positions[..] else {
        panic!("the output value is at one position, found {positions:?}");
    };
    tx[at..at + 8].copy_from_slice(&(value + 1).to_le_bytes());
    let changed = hayai_wire::RawTx::parse(Bytes::from(tx.clone()), BranchId::Nu5).expect("a tx");
    assert_eq!(changed.auth_digest, coinbase.auth_digest);
    let mut txids = raw.txids();
    txids[0] = changed.txid;
    let mut header = raw.header.clone();
    header.merkle_root = merkle_root(&txids);
    let mut bytes = header.serialize();
    let header_len = raw.header.serialize().len();
    let rest = &block[header_len..];
    // The transaction count, then the changed coinbase, then the other transactions.
    let count_len = rest.len() - raw.txs.iter().map(|tx| tx.bytes.len()).sum::<usize>();
    bytes.extend_from_slice(&rest[..count_len]);
    bytes.extend_from_slice(&tx);
    bytes.extend_from_slice(&rest[count_len + coinbase.bytes.len()..]);
    Bytes::from(bytes)
}

/// A peer has a chain with more work whose block 60 is not valid. The node downloads the
/// block, refuses it, bans the peer and follows the chain of the honest peer.
#[test]
fn a_block_that_is_not_valid_moves_the_node_to_the_next_best_chain() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 60);
    let tip = a.tip.tip();
    let honest = fetch_chain(&a);
    let bad = overpaying(&honest[59]);
    let bad_header = parse(&bad).header;
    assert_ne!(bad_header.hash(), tip.1);
    let mut child = bad_header.clone();
    child.prev_hash = bad_header.hash();
    child.time += 1;
    child.merkle_root = [3; 32];
    let mut chain = with_bodies(&honest[..59]);
    chain.push((bad_header.clone(), Some(bad)));
    chain.push((child, None));
    let evil = ScriptedPeer::serve(
        [127, 0, 0, 4],
        chain,
        Script {
            headers: true,
            blocks: usize::MAX,
            ..Script::default()
        },
    );

    let x = start(dir.path(), "x", false, false);
    x.relay.connect(evil.addr).expect("x dials the evil peer");
    // The node is on the chain of the evil peer before it knows the honest block 60.
    wait_for("the 59 common blocks", || x.tip.tip().0 == 59);
    wait_for("the ban of the evil peer", || {
        x.relay.peer_manager().is_banned(evil.addr.ip())
    });
    assert_eq!(x.tip.tip().0, 59);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, tip);
    let Err(_) = x.relay.connect(evil.addr) else {
        panic!("the node dials a banned peer");
    };
    x.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");

    let commits = rows(dir.path(), "x", "commit_state.jsonl");
    let rejected: Vec<&Value> = commits
        .iter()
        .filter(|row| row["event"] == "commit_finish" && row["result"] == "rejected")
        .collect();
    let [row] = rejected[..] else {
        panic!("one rejected block, found {rejected:?}");
    };
    assert_eq!(row["hash"], bad_header.hash().to_string());
    assert_eq!(row["height"], 60);
}

/// A branch with more work has one valid block and then a block that is not valid. The
/// node leaves its tip for the branch, refuses the second block, and goes back to its
/// first chain with the block that it stored.
#[test]
fn a_reorg_to_a_branch_with_an_invalid_block_goes_back() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    let b = start(dir.path(), "b", true, false);
    generate(&a, 59);
    b.relay.connect(addr(&a)).expect("b dials a");
    wait_tip(&b, a.tip.tip());
    disconnect_all(&a);
    disconnect_all(&b);
    wait_for("the partition", || {
        a.relay.peers().is_empty() && b.relay.peers().is_empty()
    });
    generate(&a, 1);
    let tip = a.tip.tip();
    generate(&b, 2);
    let branch = fetch_chain(&b);
    b.shutdown().expect("clean shutdown");
    let valid = parse(&branch[59]).header;
    let bad = overpaying(&branch[60]);
    let bad_header = parse(&bad).header;
    let mut child = bad_header.clone();
    child.prev_hash = bad_header.hash();
    child.time += 1;
    child.merkle_root = [3; 32];
    let mut chain = with_bodies(&branch[..60]);
    chain.push((bad_header.clone(), Some(bad)));
    chain.push((child, None));
    let evil = ScriptedPeer::serve(
        [127, 0, 0, 5],
        chain,
        Script {
            headers: true,
            blocks: usize::MAX,
            ..Script::default()
        },
    );

    let x = start(dir.path(), "x", false, false);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, tip);
    x.relay.connect(evil.addr).expect("x dials the evil peer");
    wait_for("the ban of the evil peer", || {
        x.relay.peer_manager().is_banned(evil.addr.ip())
    });
    wait_tip(&x, tip);
    // The node still follows its peer.
    generate(&a, 1);
    wait_tip(&x, a.tip.tip());
    x.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");

    let commits = rows(dir.path(), "x", "commit_state.jsonl");
    let events: Vec<(String, String)> = commits
        .iter()
        .filter(|row| row["event"] == "block_disconnected" || row["event"] == "commit_finish")
        .skip_while(|row| row["hash"] != tip.1.to_string())
        .map(|row| {
            let what = match row["event"] == "block_disconnected" {
                true => "disconnected",
                false => row["result"].as_str().expect("a result"),
            };
            (
                what.to_string(),
                row["hash"].as_str().expect("hash").to_string(),
            )
        })
        .take(6)
        .collect();
    let expected = [
        ("committed", tip.1),
        ("disconnected", tip.1),
        ("committed", valid.hash()),
        ("rejected", bad_header.hash()),
        ("disconnected", valid.hash()),
        ("committed", tip.1),
    ]
    .map(|(what, hash)| (what.to_string(), hash.to_string()));
    assert_eq!(events, expected);
}

/// A node that stops as a crash does during the synchronization resumes from its committed
/// tip and its stored headers, and reaches the tip of its peer.
#[test]
fn a_node_resumes_the_synchronization_after_a_stop() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 400);
    let tip = a.tip.tip();

    let x = start(dir.path(), "x", false, false);
    x.relay.connect(addr(&a)).expect("x dials a");
    let deadline = Instant::now() + WAIT;
    while x.tip.tip().0 < 40 {
        assert!(Instant::now() < deadline, "no synchronization");
        thread::sleep(Duration::from_millis(1));
    }
    let stopped_at = x.tip.tip().0;
    x.abandon().expect("abandon");
    assert!(
        stopped_at < 400,
        "the node stopped after the synchronization"
    );

    // The header log has the headers that the node received before the stop.
    let x = start(dir.path(), "x", false, false);
    let resumed_at = x.tip.tip().0;
    assert!(resumed_at >= 40 - 8, "resumed at {resumed_at}");
    assert!(resumed_at < 400);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, tip);

    // A clean stop and a second start: no block is requested again.
    x.shutdown().expect("clean shutdown");
    let x = start(dir.path(), "x", false, false);
    assert_eq!(x.tip.tip(), tip);
    x.relay.connect(addr(&a)).expect("x dials a");
    generate(&a, 1);
    wait_tip(&x, a.tip.tip());
    x.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");
}

/// At the tip, a hayaid peer gets each new block over the compact relay, and a legacy
/// node gets it through the header chain and the block download, in the same network.
#[test]
fn compact_relay_and_a_legacy_peer_follow_the_tip_together() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, true);
    let b = start(dir.path(), "b", false, true);
    let c = start(dir.path(), "c", false, false);
    b.relay.connect(addr(&a)).expect("b dials a");
    c.relay.connect(addr(&b)).expect("c dials b");
    wait_for("the sessions", || {
        let compact = |node: &Node| {
            node.relay
                .peers()
                .iter()
                .any(|p| p.established && matches!(p.protocol, PeerProtocol::CompactRelay(_)))
        };
        let legacy = c
            .relay
            .peers()
            .iter()
            .any(|p| p.established && p.protocol == PeerProtocol::Legacy);
        compact(&a) && compact(&b) && legacy
    });
    for _ in 0..5 {
        generate(&a, 1);
        let tip = a.tip.tip();
        wait_tip(&b, tip);
        wait_tip(&c, tip);
    }
    for node in [a, b, c] {
        node.shutdown().expect("clean shutdown");
    }
    let sources = |name: &str| -> Vec<String> {
        rows(dir.path(), name, "block_sync.jsonl")
            .iter()
            .filter(|row| row["event"] == "block_received")
            .map(|row| row["source"].as_str().expect("a source").to_string())
            .collect()
    };
    assert_eq!(sources("b"), ["compact"; 5]);
    assert_eq!(sources("c"), ["download"; 5]);
}

/// The outpoint and the value of the coinbase output of `block`.
fn coinbase_coin(block: &Bytes) -> (hayai_coins::OutPoint, u64) {
    let raw = parse(block);
    let coinbase = &raw.txs[0];
    let value = coinbase
        .tx
        .transparent_bundle()
        .expect("a transparent coinbase")
        .vout[0]
        .value()
        .into_u64();
    (
        hayai_coins::OutPoint::new(*coinbase.txid.as_ref(), 0),
        value,
    )
}

fn shielding(coin: &(hayai_coins::OutPoint, u64), fee: u64, expiry: u32) -> Arc<hayai_wire::RawTx> {
    shielding_in(BranchId::Nu5, coin, fee, expiry)
}

/// A shielding transaction of the epoch `branch`.
fn shielding_in(
    branch: BranchId,
    coin: &(hayai_coins::OutPoint, u64),
    fee: u64,
    expiry: u32,
) -> Arc<hayai_wire::RawTx> {
    let bytes = crate::test_support::shielding_tx(coin.0.clone(), coin.1, fee, expiry, branch);
    Arc::new(hayai_wire::RawTx::parse(bytes, branch).expect("a transaction"))
}

/// Waits until the template of `node` has `count` transactions after the coinbase.
fn wait_template(node: &Node, count: usize) {
    let Some(producer) = &node.producer else {
        panic!("the node is not a producer");
    };
    wait_for(
        "the template",
        || matches!(producer.feed.current(), Some(template) if template.txs.len() == count),
    );
}

fn disconnect_all(node: &Node) {
    for peer in node.relay.peers() {
        node.relay.disconnect(peer.id);
    }
}

/// Two nodes build two branches from block 105: three blocks with a transaction on the
/// first node, four blocks on the second node. When they connect, the first node moves to
/// the branch with more work, and the transaction of its disconnected block is in its
/// mempool again. The next block of the first node contains it, and the second node
/// accepts that block.
#[test]
fn a_reorg_of_depth_three_returns_the_transactions_to_the_mempool() {
    use crate::mempool::Reject;

    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    let b = start(dir.path(), "b", true, false);
    generate(&a, 105);
    b.relay.connect(addr(&a)).expect("b dials a");
    wait_tip(&b, a.tip.tip());
    let coin = coinbase_coin(&fetch_chain(&a)[0]);
    disconnect_all(&a);
    disconnect_all(&b);
    wait_for("the partition", || {
        a.relay.peers().is_empty() && b.relay.peers().is_empty()
    });

    let tx = shielding(&coin, 15_000, 0);
    let id = tx.wtxid();
    a.submit_tx(tx.clone()).expect("the transaction is valid");
    assert!(a.mempool.contains(&id));
    let Err(Reject::Known) = a.submit_tx(tx.clone()) else {
        panic!("a second admission");
    };
    wait_template(&a, 1);
    generate(&a, 3);
    // The first block of the branch has the transaction.
    assert!(!a.mempool.contains(&id));
    assert_eq!(parse(&fetch_chain(&a)[105]).txs.len(), 2);
    let Err(Reject::Prepare(_)) = a.submit_tx(tx.clone()) else {
        panic!("the chain spent the input of the transaction");
    };
    generate(&b, 4);
    let old_tip = a.tip.tip();
    let tip = b.tip.tip();
    assert_eq!((old_tip.0, tip.0), (108, 109));

    b.relay.connect(addr(&a)).expect("b dials a");
    wait_tip(&a, tip);
    wait_for("the transaction in the mempool", || a.mempool.contains(&id));
    assert_eq!(b.tip.tip(), tip);

    wait_template(&a, 1);
    generate(&a, 1);
    let tip = a.tip.tip();
    assert_eq!(tip.0, 110);
    assert!(!a.mempool.contains(&id));
    wait_tip(&b, tip);
    assert_eq!(parse(&fetch_chain(&b)[109]).txs.len(), 2);
    a.shutdown().expect("clean shutdown");
    b.shutdown().expect("clean shutdown");

    let commits = rows(dir.path(), "a", "commit_state.jsonl");
    let disconnected: Vec<u64> = commits
        .iter()
        .filter(|row| row["event"] == "block_disconnected")
        .map(|row| row["height"].as_u64().expect("height"))
        .collect();
    assert_eq!(disconnected, [108, 107, 106]);
}

/// The admission applies the policy and the tip state to each transaction.
#[test]
fn the_mempool_refuses_what_the_policy_and_the_tip_do_not_permit() {
    use crate::mempool::Reject;
    use hayai_prepared::{InsertError, PolicyReject};

    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 105);
    let chain = fetch_chain(&a);
    let mature = coinbase_coin(&chain[0]);
    let young = coinbase_coin(&chain[50]);

    // The transaction has 3 logical actions and pays for none, or for 2 of them: the
    // unpaid action limit is 0.
    for (fee, unpaid) in [(0, 3), (1_199, 1)] {
        let reason = a
            .submit_tx(shielding(&mature, fee, 0))
            .expect_err("unpaid actions");
        assert!(
            matches!(
                reason,
                Reject::Policy(PolicyReject::UnpaidActions { unpaid: found, limit: 0 })
                    if found == unpaid
            ),
            "{reason}"
        );
    }
    // The transaction expires in less than 3 blocks.
    let reason = a
        .submit_tx(shielding(&mature, 15_000, 107))
        .expect_err("expires soon");
    assert!(
        matches!(reason, Reject::Policy(PolicyReject::ExpiringSoon { .. })),
        "{reason}"
    );
    // The coinbase output has fewer than 100 confirmations.
    let reason = a
        .submit_tx(shielding(&young, 15_000, 0))
        .expect_err("immature");
    assert!(
        matches!(
            reason,
            Reject::Policy(PolicyReject::ImmatureCoinbase { .. })
        ),
        "{reason}"
    );
    // A valid transaction, then a second spend of its input.
    let first = shielding(&mature, 15_000, 0);
    a.submit_tx(first.clone()).expect("valid");
    let reason = a
        .submit_tx(shielding(&mature, 20_000, 0))
        .expect_err("a second spend");
    assert!(
        matches!(reason, Reject::Store(InsertError::Conflict { .. })),
        "{reason}"
    );
    assert!(a.mempool.contains(&first.wtxid()));
    // A transaction leaves the store at the tip after which it cannot be mined: the next
    // height is above its expiry height. The template is empty for the blocks, so that
    // no block mines it.
    let expiring = shielding(&coinbase_coin(&chain[1]), 15_000, 110);
    wait_template(&a, 1);
    generate(&a, 1);
    assert!(!a.mempool.contains(&first.wtxid()));
    a.submit_tx(expiring.clone()).expect("valid at height 106");
    assert!(a.mempool.contains(&expiring.wtxid()));
    let b = start(dir.path(), "b", true, false);
    b.relay.connect(addr(&a)).expect("b dials a");
    wait_tip(&b, a.tip.tip());
    generate(&b, 5);
    wait_tip(&a, b.tip.tip());
    assert_eq!(a.tip.tip().0, 111);
    assert!(!a.mempool.contains(&expiring.wtxid()));
    b.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");
}

/// The admission applies the Sprout anchor rule to a transaction with a JoinSplit, before
/// the proofs: an anchor that is no Sprout treestate is refused for the anchor, and the
/// anchor of the empty tree passes the rule (the transaction then fails for its proof).
#[test]
fn the_mempool_applies_the_sprout_anchor_rule() {
    use crate::mempool::Reject;
    use hayai_coins::Pool;
    use hayai_crypto::zcash_primitives::transaction::components::sprout::{Bundle, JsDescription};
    use hayai_crypto::zcash_primitives::transaction::{Authorized, TransactionData, TxVersion};
    use hayai_crypto::zcash_protocol::consensus::BlockHeight;

    // A v4 transaction with one JoinSplit that takes 20,000 zatoshis out of the Sprout
    // pool as the fee. The proof and the signature are not valid.
    let joinsplit_tx = |anchor: [u8; 32]| {
        let mut joinsplit = Vec::new();
        joinsplit.extend_from_slice(&0u64.to_le_bytes());
        joinsplit.extend_from_slice(&20_000u64.to_le_bytes());
        joinsplit.extend_from_slice(&anchor);
        for part in 1..=4u8 {
            joinsplit.extend_from_slice(&[part; 32]);
        }
        joinsplit.resize(1698, 0);
        let sprout = Bundle {
            joinsplits: vec![JsDescription::read(&joinsplit[..], true).expect("a JoinSplit")],
            joinsplit_pubkey: [3; 32],
            joinsplit_sig: [4; 64],
        };
        let tx = TransactionData::<Authorized>::from_parts(
            TxVersion::V4,
            BranchId::Nu5,
            0,
            BlockHeight::from_u32(0),
            None,
            Some(sprout),
            None,
            None,
        )
        .freeze()
        .expect("a v4 transaction");
        let mut bytes = Vec::new();
        tx.write(&mut bytes).expect("vec write");
        Arc::new(hayai_wire::RawTx::parse(bytes.into(), BranchId::Nu5).expect("a transaction"))
    };

    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 1);
    let reason = a
        .submit_tx(joinsplit_tx([9; 32]))
        .expect_err("no treestate");
    assert_eq!(reason, Reject::UnknownAnchor(Pool::Sprout));
    let empty = hayai_trees::SproutFrontier::empty().root();
    let reason = a.submit_tx(joinsplit_tx(empty)).expect_err("no proof");
    assert_eq!(reason, Reject::Proof);
    a.shutdown().expect("clean shutdown");
}

/// The activation heights of the upgrade tests: NU6 at 108 and NU6.2 at 116. NU6.2 has
/// another Orchard circuit, so a node needs a second verifying key at height 116.
const UPGRADES: &str = "[regtest]\nactivation_heights = { nu6 = 108, nu6_2 = 116 }\n";

fn upgrade_branch(height: u32) -> BranchId {
    match height {
        ..108 => BranchId::Nu5,
        108..116 => BranchId::Nu6,
        116.. => BranchId::Nu6_2,
    }
}

/// The number of transactions of the block at `height` of `chain` (the blocks from height
/// 1 on), parsed under the rule set of the height.
fn tx_count(chain: &[Bytes], height: u32) -> usize {
    RawBlock::parse(chain[height as usize - 1].clone(), upgrade_branch(height))
        .expect("a block")
        .txs
        .len()
}

/// Admits `tx` into the mempool of `node`. The Orchard key of the next upgrade is built
/// in the background, so the admission waits for it.
fn submit_when_keys_are_ready(node: &Node, tx: &Arc<hayai_wire::RawTx>) {
    use crate::mempool::Reject;
    use hayai_prepared::PrepareError;

    wait_for("the verifying key", || match node.submit_tx(tx.clone()) {
        Ok(()) => true,
        Err(Reject::Prepare(PrepareError::Unsupported(_))) => false,
        Err(reason) => panic!("the transaction is refused: {reason}"),
    });
}

/// A producer on a Regtest network with configured funding streams and a configured
/// lockbox disbursement mines across the NU6.1 height: each block is a block of its
/// template. A second node synchronizes the chain and validates each block. The coinbase
/// has the funding stream output in the range of the streams and the disbursement output
/// in the NU6.1 activation block, and the deferred pool pays the disbursement.
#[test]
fn a_chain_with_configured_funding_streams_crosses_nu6_1() {
    const ADDRESS: &str = "t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu";
    // The deferred pool gets 75,000,000 zatoshis in each of the blocks 11 and 12.
    let extra = format!(
        r#"[regtest]
activation_heights = {{ nu6 = 5, nu6_1 = 13 }}
lockbox_disbursements = [{{ address = "{ADDRESS}", amount = 150000000 }}]

[[regtest.funding_streams]]
height_range = {{ start = 11, end = 17 }}
recipients = [
    {{ receiver = "Deferred", numerator = 12 }},
    {{ receiver = "MajorGrants", numerator = 8, addresses = ["{ADDRESS}"] }},
]
"#
    );
    let dir = scratch();
    let a = start_with(dir.path(), "a", true, true, &extra);
    let x = start_with(dir.path(), "x", false, true, &extra);
    generate(&a, 18);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, a.tip.tip());
    let chain = fetch_chain(&a);
    let outputs = |height: u32| -> Vec<u64> {
        let branch = match height {
            ..5 => BranchId::Nu5,
            5..13 => BranchId::Nu6,
            13.. => BranchId::Nu6_1,
        };
        let block = RawBlock::parse(chain[height as usize - 1].clone(), branch).expect("a block");
        let coinbase = block.txs[0].tx.transparent_bundle().expect("a coinbase");
        coinbase
            .vout
            .iter()
            .map(|output| output.value().into_u64())
            .collect()
    };
    assert_eq!(outputs(10), [625_000_000]);
    assert_eq!(outputs(11), [500_000_000, 50_000_000]);
    assert_eq!(outputs(13), [500_000_000, 50_000_000, 150_000_000]);
    assert_eq!(outputs(16), [500_000_000, 50_000_000]);
    assert_eq!(outputs(17), [625_000_000]);
}

/// A chain crosses two upgrades, NU6 at height 108 and NU6.2 at height 116, with a
/// transaction in the last block before NU6, in the first block of NU6 and in the first
/// block of NU6.2. One node follows the producer at the tip across both heights: it parses
/// each relayed transaction under the rule set of the next block, and its mempool drops a
/// transaction of the old rule set at the activation. A second node synchronizes the
/// whole chain from the genesis block. No node restarts.
#[test]
fn a_chain_crosses_two_upgrades_at_the_tip_and_during_the_synchronization() {
    use crate::mempool::Reject;
    use hayai_prepared::PrepareError;

    let dir = scratch();
    let a = start_with(dir.path(), "a", true, true, UPGRADES);
    let x = start_with(dir.path(), "x", false, true, UPGRADES);
    generate(&a, 104);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, a.tip.tip());
    let chain = fetch_chain(&a);
    let coin = |height: usize| coinbase_coin(&chain[height - 1]);

    // NU5, the last rule set before NU6: the transaction goes over the relay and is in
    // block 105.
    let nu5 = shielding_in(BranchId::Nu5, &coin(1), 15_000, 0);
    a.submit_tx(nu5.clone()).expect("valid under NU5");
    wait_for("the relayed transaction", || {
        x.mempool.contains(&nu5.wtxid())
    });
    wait_template(&a, 1);
    generate(&a, 1);
    wait_tip(&x, a.tip.tip());

    // A transaction of NU5 that no block mines: the mempool of the second node holds it
    // until the next block is the first block of NU6.
    disconnect_all(&a);
    disconnect_all(&x);
    wait_for("the partition", || {
        a.relay.peers().is_empty() && x.relay.peers().is_empty()
    });
    let stale = shielding_in(BranchId::Nu5, &coin(2), 15_000, 0);
    x.submit_tx(stale.clone()).expect("valid under NU5");
    generate(&a, 2);
    assert_eq!(a.tip.tip().0, 107);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, a.tip.tip());
    wait_for("the drop of the NU5 transaction", || {
        !x.mempool.contains(&stale.wtxid())
    });
    let reason = x.submit_tx(stale.clone()).expect_err("an NU5 transaction");
    assert!(
        matches!(
            reason,
            Reject::Prepare(PrepareError::BranchId {
                tx: BranchId::Nu5,
                epoch: BranchId::Nu6
            })
        ),
        "{reason}"
    );

    // NU6: the transaction of the first NU6 block goes over the relay before the block.
    let nu6 = shielding_in(BranchId::Nu6, &coin(3), 15_000, 0);
    a.submit_tx(nu6.clone()).expect("valid under NU6");
    wait_for("the relayed NU6 transaction", || {
        x.mempool.contains(&nu6.wtxid())
    });
    wait_template(&a, 1);
    generate(&a, 1);
    wait_tip(&x, a.tip.tip());
    generate(&a, 7);
    assert_eq!(a.tip.tip().0, 115);
    wait_tip(&x, a.tip.tip());

    // NU6.2: the first block has a transaction with a proof of the new circuit.
    let nu6_2 = shielding_in(BranchId::Nu6_2, &coin(4), 15_000, 0);
    submit_when_keys_are_ready(&a, &nu6_2);
    wait_template(&a, 1);
    generate(&a, 3);
    let tip = a.tip.tip();
    assert_eq!(tip.0, 118);
    wait_tip(&x, tip);
    let chain = fetch_chain(&a);
    for (height, count) in [(105, 2), (106, 1), (107, 1), (108, 2), (115, 1), (116, 2)] {
        assert_eq!(tx_count(&chain, height), count, "block {height}");
    }

    // A new node synchronizes the chain across both activation heights.
    let y = start_with(dir.path(), "y", false, false, UPGRADES);
    y.relay.connect(addr(&x)).expect("y dials x");
    wait_tip(&y, tip);
    // The mempool of the new node takes a transaction of the rule set of the tip.
    let late = shielding_in(BranchId::Nu6_2, &coin(5), 15_000, 0);
    y.submit_tx(late.clone()).expect("valid under NU6.2");
    wait_for("the relayed NU6.2 transaction", || {
        a.mempool.contains(&late.wtxid())
    });
    for node in [a, x, y] {
        node.shutdown().expect("clean shutdown");
    }

    for name in ["x", "y"] {
        let commits = rows(dir.path(), name, "commit_state.jsonl");
        let results: Vec<&str> = commits
            .iter()
            .filter(|row| row["event"] == "commit_finish")
            .map(|row| row["result"].as_str().expect("a result"))
            .collect();
        assert_eq!(results, ["committed"; 118], "{name}");
    }
    // The blocks at the activation heights reached the first node over the compact relay.
    let received = rows(dir.path(), "x", "block_sync.jsonl");
    for height in [108, 116] {
        let sources: Vec<&Value> = received
            .iter()
            .filter(|row| row["event"] == "block_received" && row["height"] == height)
            .map(|row| &row["source"])
            .collect();
        assert_eq!(sources, ["compact"], "block {height}");
    }
}

/// A chain crosses NU7 at height 108, after NU6.3 at height 104. The producer builds each
/// block from its template, so the coinbase of the template passes the validation on both
/// sides of the height: block 107 pays the subsidy of 6.25 ZEC and all the fees, and block
/// 108 pays a third of that subsidy and the miner share of the fees (ZIP 218, NSM). One
/// node follows the producer at the tip, and a second node synchronizes the whole chain.
/// No node restarts. The NSM reissuance starts at height 110 (a value of the test
/// configuration): the template of the blocks 110 and 111 pays the subsidy plus the bonus
/// of the NSM value balance after the parent, and the validation accepts them. The chain
/// value pools of each node hold the scheduled issuance minus the NSM value balance. A node of a build without the NU7 rule set
/// stops with an error when its next block is block 108, and the test ends there.
#[test]
fn a_chain_crosses_nu7_at_the_tip_and_during_the_synchronization() {
    use crate::mempool::Reject;
    use hayai_consensus::{nsm, subsidy, RuleSet, Upgrade};
    use hayai_prepared::PrepareError;

    const NU7: &str = "[regtest]\nactivation_heights = { nu6_3 = 104, nu7 = 108 }\n";
    let dir = scratch();
    let start = |name: &str, produce: bool, compact: bool| {
        let mut config = config_with(dir.path(), name, produce, compact, NU7);
        let Some(regtest) = &mut config.regtest else {
            panic!("the configuration has a [regtest] section");
        };
        regtest.test_reissuance_height = Some(110);
        let network = config.consensus_network().expect("a network");
        let node = Node::start(&config).unwrap_or_else(|e| panic!("{name}: {e}"));
        (node, network)
    };
    let (a, regtest) = start("a", true, true);
    let (x, _) = start("x", false, true);
    generate(&a, 106);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, a.tip.tip());
    let chain = fetch_chain(&a);
    let coin = |height: usize| coinbase_coin(&chain[height - 1]);
    let miner_value = |block: &Bytes, branch: BranchId| {
        let raw = RawBlock::parse(block.clone(), branch).expect("a block");
        let coinbase = raw.txs[0].tx.transparent_bundle().expect("a coinbase");
        (raw.txs.len(), coinbase.vout[0].value().into_u64())
    };

    // NU6.3, the last block before NU7: the miner gets the fees of 15,001 zatoshis.
    let last = shielding_in(BranchId::Nu6_3, &coin(1), 15_001, 0);
    submit_when_keys_are_ready(&a, &last);
    wait_template(&a, 1);
    let Some(rules) = RuleSet::of(Upgrade::Nu7) else {
        // The node stops when its next block is the first block of NU7.
        let producer = a.producer.as_ref().expect("a producer");
        let Err(stopped) = producer.generate(1) else {
            panic!("a build without the NU7 rule set stops before the NU7 height");
        };
        let stopped = format!("{stopped:?}");
        assert!(stopped.contains("Nu7 is active at height 108"), "{stopped}");
        return;
    };
    let nu7 = rules.branch_id;
    generate(&a, 1);
    assert_eq!(a.tip.tip().0, 107);
    wait_tip(&x, a.tip.tip());
    let chain = fetch_chain(&a);
    assert_eq!(
        miner_value(&chain[106], BranchId::Nu6_3),
        (2, 625_000_000 + 15_001)
    );

    // The next block is the first block of NU7: a transaction of NU6.3 is refused, and a
    // transaction of NU7 goes over the relay before the block.
    let stale = shielding_in(BranchId::Nu6_3, &coin(2), 15_001, 0);
    let reason = x.submit_tx(stale).expect_err("an NU6.3 transaction");
    assert!(
        matches!(
            reason,
            Reject::Prepare(PrepareError::BranchId { tx: BranchId::Nu6_3, epoch }) if epoch == nu7
        ),
        "{reason}"
    );
    let first = shielding_in(nu7, &coin(2), 15_001, 0);
    submit_when_keys_are_ready(&a, &first);
    wait_for("the relayed NU7 transaction", || {
        x.mempool.contains(&first.wtxid())
    });
    wait_template(&a, 1);
    generate(&a, 1);
    wait_tip(&x, a.tip.tip());
    generate(&a, 3);
    let tip = a.tip.tip();
    assert_eq!(tip.0, 111);
    wait_tip(&x, tip);
    let chain = fetch_chain(&a);
    // 9,000 of the 15,001 zatoshis of fees stay out of the coinbase of block 108.
    assert_eq!(miner_value(&chain[107], nu7), (2, 208_333_333 + 6_001));
    assert_eq!(miner_value(&chain[108], nu7), (1, 208_333_333));
    // The reissuance: the balance of 9,000 zatoshis gives a bonus of 1 zatoshi to block
    // 110, and the balance of 8,999 gives 1 zatoshi to block 111.
    assert_eq!(nsm::reissuance_height(regtest), Some(110));
    assert_eq!(nsm::reissuance_bonus(9_000), 1);
    assert_eq!(miner_value(&chain[109], nu7), (1, 208_333_334));
    assert_eq!(miner_value(&chain[110], nu7), (1, 208_333_334));

    // A new node synchronizes the chain across the NU7 height.
    let (y, _) = start("y", false, false);
    y.relay.connect(addr(&x)).expect("y dials x");
    wait_tip(&y, tip);
    let scheduled = 107 * 625_000_000 + 4 * 208_333_333;
    for node in [&a, &x, &y] {
        let total = node.mempool.view().value_pools().total();
        assert_eq!(total, scheduled - 8_998);
    }
    assert_eq!(
        subsidy::scheduled_issuance(regtest, 111),
        u128::from(scheduled)
    );
    assert_eq!(nsm::balance(regtest, 111, scheduled - 8_998), Ok(8_998));
    // The mempool of the new node takes a transaction of NU7.
    let late = shielding_in(nu7, &coin(3), 15_000, 0);
    y.submit_tx(late.clone()).expect("valid under NU7");
    wait_for("the relayed NU7 transaction", || {
        a.mempool.contains(&late.wtxid())
    });
    for node in [a, x, y] {
        node.shutdown().expect("clean shutdown");
    }
    for name in ["x", "y"] {
        let commits = rows(dir.path(), name, "commit_state.jsonl");
        let results: Vec<&str> = commits
            .iter()
            .filter(|row| row["event"] == "commit_finish")
            .map(|row| row["result"].as_str().expect("a result"))
            .collect();
        assert_eq!(results, ["committed"; 111], "{name}");
    }
}

/// The `[regtest]` section that gives the chain `blocks` (heights from 1) the checkpoints
/// at `heights` and the mandatory checkpoint height `mandatory`.
fn checkpoints_toml(blocks: &[Bytes], heights: &[u32], mandatory: u32) -> String {
    let entries: Vec<String> = heights
        .iter()
        .map(|height| {
            let header = BlockHeader::parse(&blocks[*height as usize - 1]).expect("a header");
            format!("[{height}, \"{}\"]", header.hash())
        })
        .collect();
    format!(
        "[regtest]\ncheckpoints = [{}]\nmandatory_checkpoint_height = {mandatory}\n",
        entries.join(", ")
    )
}

/// The `apply_class` of each committed block of the node `name`, by height: `checkpoint`
/// for the checkpoint path, `full` for each class of the full validation (a block at the
/// tip can commit from the prebuilt body of the template).
fn commit_classes(dir: &Path, name: &str) -> Vec<(u64, String)> {
    rows(dir, name, "commit_state.jsonl")
        .iter()
        .filter(|row| row["event"] == "commit_finish" && row["result"] == "committed")
        .map(|row| {
            (
                row["height"].as_u64().expect("a height"),
                match row["apply_class"].as_str().expect("a class") {
                    "checkpoint" => "checkpoint",
                    "full" | "prebuilt_own" => "full",
                    other => panic!("class {other}"),
                }
                .to_string(),
            )
        })
        .collect()
}

/// `count` headers without a body after `parent`: each one has the next time and a merkle
/// root that no body has.
fn header_chain_after(parent: &BlockHeader, count: usize) -> Vec<(BlockHeader, Option<Bytes>)> {
    let mut chain = Vec::with_capacity(count);
    let mut last = parent.clone();
    for _ in 0..count {
        let mut next = last.clone();
        next.prev_hash = last.hash();
        next.time += 1;
        next.merkle_root = [3; 32];
        chain.push((next.clone(), None));
        last = next;
    }
    chain
}

/// A node with the checkpoints 20 and 40 and the mandatory checkpoint 30 applies the
/// blocks up to 40 with the checkpoint path and the blocks above 40 with full validation.
/// While its header chain ends at 35, the blocks 21 to 35 wait: a block at or below the
/// mandatory checkpoint has no full validation, and no checkpoint above it is reached.
#[test]
fn the_checkpoint_path_ends_at_the_last_checkpoint_and_blocks_wait_below_the_mandatory_one() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 60);
    let tip = a.tip.tip();
    let blocks = fetch_chain(&a);
    let short = ScriptedPeer::serve(
        [127, 0, 0, 6],
        with_bodies(&blocks[..35]),
        Script {
            headers: true,
            blocks: usize::MAX,
            ..Script::default()
        },
    );

    let extra = checkpoints_toml(&blocks, &[20, 40], 30);
    let x = start_with(dir.path(), "x", false, false, &extra);
    x.relay.connect(short.addr).expect("x dials the short peer");
    wait_for("the checkpoint 20", || x.tip.tip().0 == 20);
    // The node has the bodies of the blocks 21 to 35 and commits none of them.
    wait_for("the requests for the blocks", || {
        short.requested.load(Ordering::Relaxed) >= 35
    });
    thread::sleep(Duration::from_millis(500));
    assert_eq!(x.tip.tip().0, 20);

    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, tip);
    generate(&a, 1);
    wait_tip(&x, a.tip.tip());
    x.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");

    let classes = commit_classes(dir.path(), "x");
    let expected: Vec<(u64, String)> = (1..=61)
        .map(|height| {
            let class = match height <= 40 {
                true => "checkpoint",
                false => "full",
            };
            (height, class.to_string())
        })
        .collect();
    assert_eq!(classes, expected);
}

/// A peer has a header chain with more work that leaves the chain of the checkpoints at
/// height 10. A node that reached the checkpoint 40 refuses the first header of the fork
/// and stays on its chain. A new node accepts the fork up to height 19, refuses the header
/// at the checkpoint height 20, bans the peer and commits no block of the fork.
#[test]
fn a_fork_below_a_checkpoint_is_refused() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 60);
    let tip = a.tip.tip();
    let blocks = fetch_chain(&a);
    let mut fork = with_bodies(&blocks[..10]);
    let fork_point = fork[9].0.clone();
    fork.extend(header_chain_after(&fork_point, 70));
    let script = Script {
        headers: true,
        blocks: usize::MAX,
        ..Script::default()
    };
    let extra = checkpoints_toml(&blocks, &[20, 40], 30);

    // A node that reached the last checkpoint.
    let x = start_with(dir.path(), "x", false, false, &extra);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, tip);
    let evil = ScriptedPeer::serve([127, 0, 0, 7], fork.clone(), script);
    x.relay.connect(evil.addr).expect("x dials the fork peer");
    wait_for("the session with the fork peer", || {
        x.relay.peers().iter().filter(|p| p.established).count() == 2
    });
    // The fork has more headers than the chain. The node still follows the chain.
    generate(&a, 1);
    let tip = a.tip.tip();
    wait_tip(&x, tip);
    assert!(!x.relay.peer_manager().is_banned(evil.addr.ip()));
    x.shutdown().expect("clean shutdown");

    // A node that reached no checkpoint.
    let evil = ScriptedPeer::serve([127, 0, 0, 8], fork, script);
    let y = start_with(dir.path(), "y", false, false, &extra);
    y.relay.connect(evil.addr).expect("y dials the fork peer");
    wait_for("the ban of the fork peer", || {
        y.relay.peer_manager().is_banned(evil.addr.ip())
    });
    assert_eq!(y.tip.tip().0, 0);
    y.relay.connect(addr(&a)).expect("y dials a");
    wait_tip(&y, tip);
    y.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");
    let classes = commit_classes(dir.path(), "y");
    assert_eq!(classes.len(), 61);
    assert!(classes[..40].iter().all(|(_, class)| class == "checkpoint"));
}

/// A copy of `block` with the header of `block` and the transactions of `body`.
fn with_header_of(block: &Bytes, body: &Bytes) -> Bytes {
    let header_len = BlockHeader::parse(block)
        .expect("a header")
        .serialize()
        .len();
    let mut bytes = block[..header_len].to_vec();
    bytes.extend_from_slice(&body[header_len..]);
    Bytes::from(bytes)
}

/// A copy of `block` with its last transaction twice. With an odd number of transactions
/// the merkle root is the root of the block.
fn with_last_transaction_twice(block: &Bytes) -> Bytes {
    let raw = parse(block);
    assert_eq!(raw.txs.len(), 3, "a block with three transactions");
    let header_len = raw.header.serialize().len();
    assert_eq!(block[header_len], 3, "the transaction count");
    let mut bytes = block.to_vec();
    bytes[header_len] = 4;
    bytes.extend_from_slice(&raw.txs[2].bytes);
    let changed = Bytes::from(bytes);
    let doubled = parse(&changed);
    assert_eq!(doubled.txs.len(), 4);
    assert_eq!(merkle_root(&doubled.txids()), raw.header.merkle_root);
    changed
}

/// In the checkpoint range, a peer sends a body whose transactions are not the
/// transactions of the header, and a second peer sends a body with its last transaction
/// twice, which has the merkle root of the header. Each peer gets the ban, the node does
/// not stop and writes no invalid mark, and the honest peer gives the blocks.
#[test]
fn a_wrong_body_in_the_checkpoint_range_costs_its_peer_only() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 104);
    let early = fetch_chain(&a);
    for height in [1, 2] {
        let coin = coinbase_coin(&early[height - 1]);
        a.submit_tx(shielding(&coin, 15_000, 0)).expect("valid");
    }
    wait_template(&a, 2);
    generate(&a, 6);
    let tip = a.tip.tip();
    assert_eq!(tip.0, 110);
    let blocks = fetch_chain(&a);
    let script = Script {
        headers: true,
        blocks: usize::MAX,
        ..Script::default()
    };
    // Block 30 with the transactions of a block that pays one zatoshi more.
    let mut changed = with_bodies(&blocks);
    changed[29].1 = Some(with_header_of(&blocks[29], &overpaying(&blocks[29])));
    let changed = ScriptedPeer::serve([127, 0, 0, 9], changed, script);
    // Block 105 with its last transaction twice.
    let mut doubled = with_bodies(&blocks);
    doubled[104].1 = Some(with_last_transaction_twice(&blocks[104]));
    let doubled = ScriptedPeer::serve([127, 0, 0, 10], doubled, script);

    let extra = checkpoints_toml(&blocks, &[50, 108], 60);
    let x = start_with(dir.path(), "x", false, false, &extra);
    x.relay
        .connect(changed.addr)
        .expect("x dials the first peer");
    wait_for("the ban of the first peer", || {
        x.relay.peer_manager().is_banned(changed.addr.ip())
    });
    assert_eq!(x.tip.tip().0, 29);
    x.relay
        .connect(doubled.addr)
        .expect("x dials the second peer");
    wait_for("the ban of the second peer", || {
        x.relay.peer_manager().is_banned(doubled.addr.ip())
    });
    assert_eq!(x.tip.tip().0, 104);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, tip);
    // No block has an invalid mark: a restart gives the same tip, and the node follows.
    x.shutdown().expect("clean shutdown");
    let x = start_with(dir.path(), "x", false, false, &extra);
    assert_eq!(x.tip.tip(), tip);
    x.relay.connect(addr(&a)).expect("x dials a");
    generate(&a, 1);
    wait_tip(&x, a.tip.tip());
    x.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");
    let rejected = rows(dir.path(), "x", "commit_state.jsonl")
        .iter()
        .filter(|row| row["event"] == "commit_finish" && row["result"] != "committed")
        .count();
    assert_eq!(rejected, 0, "the wrong bodies do not reach the validator");
}

/// A node stops as a crash does inside the checkpoint range. The start applies the stored
/// blocks with the checkpoint path, and the node continues the synchronization through
/// the last checkpoint.
#[test]
fn a_node_restarts_inside_the_checkpoint_range() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 400);
    let tip = a.tip.tip();
    let blocks = fetch_chain(&a);
    let extra = checkpoints_toml(&blocks, &[200, 380], 300);

    let x = start_with(dir.path(), "x", false, false, &extra);
    x.relay.connect(addr(&a)).expect("x dials a");
    let deadline = Instant::now() + WAIT;
    while x.tip.tip().0 < 40 {
        assert!(Instant::now() < deadline, "no synchronization");
        thread::sleep(Duration::from_millis(1));
    }
    let stopped_at = x.tip.tip().0;
    x.abandon().expect("abandon");
    assert!(stopped_at < 380, "the node stopped at {stopped_at}");

    let x = start_with(dir.path(), "x", false, false, &extra);
    let resumed_at = x.tip.tip().0;
    assert!(resumed_at >= 40 - 8, "resumed at {resumed_at}");
    assert!(resumed_at < 380, "resumed at {resumed_at}");
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, tip);
    x.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");

    // Each height has the class of its range, before and after the stop.
    let classes = commit_classes(dir.path(), "x");
    assert!(classes.len() >= 400);
    for (height, class) in &classes {
        let expected = match *height <= 380 {
            true => "checkpoint",
            false => "full",
        };
        assert_eq!(class, expected, "block {height}");
    }
    assert_eq!(classes.last().map(|(height, _)| *height), Some(400));
}

/// A header with a time above the limit of the time rules is refused, and its peer gets
/// no penalty: the rule depends on the branch and on the local clock, not on the header
/// alone. The node takes the blocks of the peer.
#[test]
fn a_header_that_fails_a_time_rule_costs_its_peer_nothing() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 30);
    let tip = a.tip.tip();
    let blocks = fetch_chain(&a);
    let mut chain = with_bodies(&blocks);
    let mut late = header_chain_after(&chain[29].0, 1);
    // More than 90 min above the median-time-past and more than 2 h above the clock.
    late[0].0.time += 3 * 60 * 60;
    chain.extend(late);
    let peer = ScriptedPeer::serve([127, 0, 0, 12], chain, Script::default());
    let x = start(dir.path(), "x", false, false);
    x.relay.connect(peer.addr).expect("x dials the peer");
    wait_tip(&x, tip);
    assert!(!x.relay.peer_manager().is_banned(peer.addr.ip()));
    assert_eq!(x.relay.peer_manager().score(peer.addr.ip()), 0);
    // The next ticks of the block synchronization write a `sync_progress` row.
    thread::sleep(Duration::from_millis(600));
    x.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");
    let headers = rows(dir.path(), "x", "block_sync.jsonl")
        .iter()
        .filter(|row| row["event"] == "sync_progress")
        .map(|row| row["headers_height"].as_u64().expect("a height"))
        .max();
    assert_eq!(
        headers,
        Some(30),
        "the late header is not in the header chain"
    );
}

/// A copy of `block` with one changed byte in the scriptSig of its v5 coinbase. The
/// transaction id, the merkle root and the block hash stay: only the authorizing data
/// change.
fn with_changed_coinbase_script(block: &Bytes) -> Bytes {
    let raw = parse(block);
    let needle = b"hayai";
    let at = block
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("the miner data of the coinbase");
    let mut bytes = block.to_vec();
    bytes[at] ^= 1;
    let changed = Bytes::from(bytes);
    let other = parse(&changed);
    assert_eq!(other.hash(), raw.hash());
    assert_eq!(other.txids(), raw.txids());
    assert_ne!(other.auth_digests(), raw.auth_digests());
    changed
}

/// The node has the prebuilt body of its template, which has no transaction. A peer
/// sends the next block, whose body after the coinbase equals the prebuilt body, with a
/// changed coinbase scriptSig. The body is a wrong body: the peer gets the ban, the header
/// stays valid, and the node takes the block of the honest peer.
#[test]
fn a_changed_coinbase_script_on_the_prebuilt_path_is_a_wrong_body() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 12);
    let tip = a.tip.tip();
    let blocks = fetch_chain(&a);
    let first = ScriptedPeer::serve(
        [127, 0, 0, 13],
        with_bodies(&blocks[..11]),
        Script::default(),
    );
    let mut changed = with_bodies(&blocks);
    changed[11].1 = Some(with_changed_coinbase_script(&blocks[11]));
    let evil = ScriptedPeer::serve([127, 0, 0, 14], changed, Script::default());

    let x = start(dir.path(), "x", false, false);
    x.relay.connect(first.addr).expect("x dials the first peer");
    wait_for("block 11", || x.tip.tip().0 == 11);
    // The node prebuilds the body of its template on block 11 while it is idle.
    thread::sleep(3 * crate::PREBUILD_INTERVAL);
    x.relay.connect(evil.addr).expect("x dials the evil peer");
    wait_for("the ban of the evil peer", || {
        x.relay.peer_manager().is_banned(evil.addr.ip())
    });
    assert_eq!(x.tip.tip().0, 11);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, tip);
    x.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");

    let commits = rows(dir.path(), "x", "commit_state.jsonl");
    let block_12: Vec<(&str, &str)> = commits
        .iter()
        .filter(|row| row["event"] == "commit_finish" && row["height"] == 12)
        .map(|row| {
            (
                row["result"].as_str().expect("a result"),
                row["apply_class"].as_str().expect("a class"),
            )
        })
        .collect();
    assert_eq!(
        block_12,
        [("rejected", "prebuilt_own"), ("committed", "prebuilt_own")]
    );
}

/// A transaction passes the checks of the admission on the tip N. Before its insert, the
/// node commits the block N + 1, which spends the same coin. The insert sees the tip
/// change, the admission runs again on the new tip, and the store does not take the
/// transaction.
#[test]
fn an_admission_on_an_old_tip_does_not_insert_after_the_commit() {
    use crate::mempool::Reject;
    use hayai_prepared::PrepareError;

    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    let b = start(dir.path(), "b", true, false);
    generate(&a, 104);
    b.relay.connect(addr(&a)).expect("b dials a");
    wait_tip(&b, a.tip.tip());
    let coin = coinbase_coin(&fetch_chain(&a)[0]);
    disconnect_all(&a);
    disconnect_all(&b);
    wait_for("the partition", || {
        a.relay.peers().is_empty() && b.relay.peers().is_empty()
    });

    // The second node mines a spend of the coin in the block 105.
    b.submit_tx(shielding(&coin, 20_000, 0)).expect("valid");
    wait_template(&b, 1);
    generate(&b, 1);
    let tip = b.tip.tip();

    // The first node admits another spend of the coin. Between its checks and its insert
    // the node commits the block 105.
    let (a, b) = (Arc::new(a), Arc::new(b));
    let (a_hook, b_hook) = (a.clone(), b.clone());
    *a.mempool.before_insert.lock() = Some(Box::new(move || {
        b_hook.relay.connect(addr(&a_hook)).expect("b dials a");
        wait_tip(&a_hook, tip);
    }));
    let tx = shielding(&coin, 15_000, 0);
    let reason = a.submit_tx(tx.clone()).expect_err("the coin is spent");
    assert!(
        matches!(reason, Reject::Prepare(PrepareError::MissingInput(_))),
        "{reason}"
    );
    assert!(!a.mempool.contains(&tx.wtxid()));
    for node in [a, b] {
        let Ok(node) = Arc::try_unwrap(node) else {
            panic!("the hook left a reference to the node");
        };
        node.shutdown().expect("clean shutdown");
    }
}

/// The queue of the driver takes a bounded number of messages for each peer, and the
/// time of the stall rules is the arrival time of the oldest queued message.
#[test]
fn the_driver_queue_has_a_bound_for_each_peer() {
    use crate::node::Event;
    use crate::sync::{Backlog, NetEvent};
    use hayai_net::{PeerId, Source};

    let backlog = Backlog::default();
    let (events, queue) = crossbeam_channel::unbounded();
    let peer = Source::Peer {
        id: PeerId(7),
        protocol: PeerProtocol::Legacy,
        ip: [127, 0, 0, 1].into(),
    };
    let announce = || NetEvent::BlockInv {
        peer,
        hashes: Vec::new(),
    };
    let first = Instant::now();
    backlog.send(&events, announce(), first, false);
    for _ in 0..100 {
        backlog.send(&events, announce(), Instant::now(), false);
    }
    assert_eq!(queue.len(), crate::sync::MAX_QUEUED_PER_PEER);
    assert_eq!(backlog.horizon(), first);
    // The driver takes one message: one more has room.
    let Ok(Event::Net { event, .. }) = queue.try_recv() else {
        panic!("a queued message");
    };
    backlog.received(&event);
    assert!(backlog.horizon() > first);
    backlog.send(&events, announce(), Instant::now(), false);
    assert_eq!(queue.len(), crate::sync::MAX_QUEUED_PER_PEER);
    // A connection message has no bound.
    backlog.send(
        &events,
        NetEvent::PeerDisconnected { peer: PeerId(7) },
        Instant::now(),
        false,
    );
    assert_eq!(queue.len(), crate::sync::MAX_QUEUED_PER_PEER + 1);
}

/// The value of the metric `name` of `node`.
fn metric(node: &Node, name: &str) -> f64 {
    use std::io::Read;

    let addr = node.metrics_addr.expect("the node serves metrics");
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .expect("request");
    let mut text = String::new();
    stream.read_to_string(&mut text).expect("answer");
    text.lines()
        .find_map(|line| line.strip_prefix(name)?.trim().parse().ok())
        .unwrap_or_else(|| panic!("no metric {name}"))
}

/// A block with a transaction twice whose merkle root is the root of its own list is
/// not a merkle mutation: the header commits to the repeated transaction. The block is
/// invalid, and its header gets the invalid mark.
#[test]
fn a_repeated_transaction_that_the_header_commits_to_is_an_invalid_block() {
    use crate::sync::merkle_mutation;

    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 104);
    let early = fetch_chain(&a);
    for height in [1, 2] {
        let coin = coinbase_coin(&early[height - 1]);
        a.submit_tx(shielding(&coin, 15_000, 0)).expect("valid");
    }
    wait_template(&a, 2);
    // The history root after block 104, to which the header of block 105 commits.
    let Some(producer) = &a.producer else {
        panic!("the node is a producer");
    };
    let history_root = producer
        .feed
        .current()
        .expect("a template")
        .tip
        .history_root;
    generate(&a, 1);
    let tip = a.tip.tip();
    let blocks = fetch_chain(&a);
    // The transactions [cb, t1, t2, t1] with the root of this list.
    let raw = parse(&blocks[104]);
    let header_len = raw.header.serialize().len();
    let mut txids = raw.txids();
    txids.push(txids[1]);
    let mut header = raw.header.clone();
    header.merkle_root = merkle_root(&txids);
    let mut digests = raw.auth_digests();
    digests.push(digests[1]);
    header.block_commitments =
        hayai_wire::block_commitments(&history_root, &hayai_wire::auth_data_root(&digests));
    assert_eq!(merkle_mutation(&txids, &header.merkle_root), None);
    let mut bytes = header.serialize();
    bytes.push(4);
    bytes.extend_from_slice(&blocks[104][header_len + 1..]);
    bytes.extend_from_slice(&raw.txs[1].bytes);
    let bad = Bytes::from(bytes);
    // The mutation form of the honest block is a wrong body.
    let mutated = parse(&with_last_transaction_twice(&blocks[104]));
    assert_eq!(
        merkle_mutation(&mutated.txids(), &mutated.header.merkle_root),
        Some(mutated.txs[2].txid)
    );

    let mut chain = with_bodies(&blocks[..104]);
    chain.push((header.clone(), Some(bad)));
    chain.extend(header_chain_after(&header, 1));
    let evil = ScriptedPeer::serve([127, 0, 0, 16], chain, Script::default());
    let x = start(dir.path(), "x", false, false);
    x.relay.connect(evil.addr).expect("x dials the evil peer");
    wait_for("the ban of the evil peer", || {
        x.relay.peer_manager().is_banned(evil.addr.ip())
    });
    assert_eq!(x.tip.tip().0, 104);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, tip);
    x.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");
    let rejected: Vec<Value> = rows(dir.path(), "x", "commit_state.jsonl")
        .into_iter()
        .filter(|row| row["event"] == "block_validated" && row["result"] != "valid")
        .collect();
    let [row] = &rejected[..] else {
        panic!("one rejected block, found {rejected:?}");
    };
    assert_eq!(row["result"], "invalid");
    assert_eq!(row["hash"], header.hash().to_string());
    assert!(
        row["reason"]
            .as_str()
            .expect("a reason")
            .contains("duplicate txid"),
        "{row}"
    );
}

/// A copy of `tx` with `needle` replaced by `with` at its one position.
fn replaced(tx: &hayai_wire::RawTx, needle: &[u8], with: &[u8]) -> Arc<hayai_wire::RawTx> {
    let positions: Vec<usize> = tx
        .bytes
        .windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle)
        .map(|(at, _)| at)
        .collect();
    let [at] = positions[..] else {
        panic!("the bytes are at one position, found {positions:?}");
    };
    let mut bytes = tx.bytes.to_vec();
    bytes[at..at + with.len()].copy_from_slice(with);
    Arc::new(hayai_wire::RawTx::parse(Bytes::from(bytes), BranchId::Nu5).expect("a transaction"))
}

/// The admission refuses a transaction with a nullifier that the chain holds, and a
/// transaction with an anchor that is no tree state of the chain, before it reads a proof.
#[test]
fn the_mempool_refuses_a_nullifier_of_the_chain_and_an_unknown_anchor() {
    use crate::mempool::Reject;
    use hayai_coins::Pool;

    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 105);
    let chain = fetch_chain(&a);
    let (first, second) = (coinbase_coin(&chain[0]), coinbase_coin(&chain[1]));
    assert_eq!(first.1, second.1);
    let tx = shielding(&first, 15_000, 0);
    a.submit_tx(tx.clone()).expect("valid");
    wait_template(&a, 1);
    generate(&a, 1);
    assert!(!a.mempool.contains(&tx.wtxid()));

    // The Orchard bundle of the mined transaction with another transparent input: its
    // nullifiers are in the chain.
    let again = replaced(&tx, first.0.hash(), second.0.hash());
    assert_ne!(again.txid, tx.txid);
    assert_eq!(
        a.submit_tx(again).expect_err("a nullifier of the chain"),
        Reject::NullifierInChain(Pool::Orchard)
    );

    // A new transaction with one changed bit in its Orchard anchor.
    let fresh = shielding(&second, 15_000, 0);
    let anchor = hayai_crypto::orchard::Anchor::empty_tree().to_bytes();
    let mut other = anchor;
    other[0] ^= 1;
    let changed = replaced(&fresh, &anchor, &other);
    assert_eq!(
        a.submit_tx(changed).expect_err("an unknown anchor"),
        Reject::UnknownAnchor(Pool::Orchard)
    );
    a.submit_tx(fresh)
        .expect("the transaction with its own anchor is valid");
    a.shutdown().expect("clean shutdown");
}

/// A node at height 104 and the block 105 of `a`, which has one shielding transaction.
/// Returns the chain of `a` and the history root after block 104.
fn chain_with_a_transaction_in_block_105(a: &Node) -> (Vec<Bytes>, [u8; 32]) {
    generate(a, 104);
    let coin = coinbase_coin(&fetch_chain(a)[0]);
    a.submit_tx(shielding(&coin, 15_000, 0)).expect("valid");
    wait_template(a, 1);
    let Some(producer) = &a.producer else {
        panic!("the node is a producer");
    };
    let history_root = producer
        .feed
        .current()
        .expect("a template")
        .tip
        .history_root;
    generate(a, 1);
    (fetch_chain(a), history_root)
}

/// A copy of `block` with one changed bit in the last byte of its last transaction: the
/// binding signature of the Orchard bundle, which is authorizing data.
fn with_changed_signature(block: &Bytes) -> Bytes {
    let mut bytes = block.to_vec();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    Bytes::from(bytes)
}

/// A peer sends block 105 with a changed signature: the authorizing data are not the data
/// of the header commitment. The block takes the full validation path. The peer gets the
/// ban, the header gets no invalid mark (also after a restart), and the node takes the
/// block of the honest peer.
#[test]
fn changed_authorizing_data_on_the_full_path_are_a_wrong_body() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    let (blocks, _) = chain_with_a_transaction_in_block_105(&a);
    let tip = a.tip.tip();
    let mut changed = with_bodies(&blocks);
    changed[104].1 = Some(with_changed_signature(&blocks[104]));
    assert_eq!(parse(&blocks[104]).hash(), changed[104].0.hash());
    let evil = ScriptedPeer::serve([127, 0, 0, 17], changed, Script::default());

    let x = start(dir.path(), "x", false, false);
    x.relay.connect(evil.addr).expect("x dials the evil peer");
    wait_for("the ban of the evil peer", || {
        x.relay.peer_manager().is_banned(evil.addr.ip())
    });
    assert_eq!(x.tip.tip().0, 104);
    // A restart before the honest body: the header log has no invalid mark.
    x.shutdown().expect("clean shutdown");
    let x = start(dir.path(), "x", false, false);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, tip);
    x.shutdown().expect("clean shutdown");
    a.shutdown().expect("clean shutdown");

    let results: Vec<(String, String)> = rows(dir.path(), "x", "commit_state.jsonl")
        .iter()
        .filter(|row| row["event"] == "block_validated" && row["height"] == 105)
        .map(|row| {
            (
                row["result"].as_str().expect("a result").to_string(),
                row["hash"].as_str().expect("a hash").to_string(),
            )
        })
        .collect();
    let hash = tip.1.to_string();
    assert_eq!(
        results,
        [
            ("wrong_body".to_string(), hash.clone()),
            ("valid".to_string(), hash)
        ]
    );
}

/// The one delivered block is the best header tip, and its layer builds: the template
/// moves to the block before the verification. A signature of the block is not valid, so
/// the verification fails, and the template goes back to the committed tip.
#[test]
fn the_template_goes_back_after_a_failed_verification_at_the_tip() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    let (blocks, history_root) = chain_with_a_transaction_in_block_105(&a);
    a.shutdown().expect("clean shutdown");
    // Block 105 with a changed signature and a header that commits to the changed data.
    let bad = with_changed_signature(&blocks[104]);
    let raw = parse(&bad);
    let mut header = raw.header.clone();
    header.block_commitments = hayai_wire::block_commitments(
        &history_root,
        &hayai_wire::auth_data_root(&raw.auth_digests()),
    );
    let header_len = header.serialize().len();
    let mut bytes = header.serialize();
    bytes.extend_from_slice(&bad[header_len..]);
    let parent = parse(&blocks[103]).hash();
    let mut chain = with_bodies(&blocks[..104]);
    chain.push((header.clone(), Some(Bytes::from(bytes))));
    let first = ScriptedPeer::serve(
        [127, 0, 0, 18],
        with_bodies(&blocks[..104]),
        Script::default(),
    );
    let evil = ScriptedPeer::serve([127, 0, 0, 19], chain, Script::default());

    let x = start(dir.path(), "x", true, false);
    x.relay.connect(first.addr).expect("x dials the first peer");
    wait_for("block 104", || x.tip.tip().0 == 104);
    x.relay.connect(evil.addr).expect("x dials the evil peer");
    wait_for("the ban of the evil peer", || {
        x.relay.peer_manager().is_banned(evil.addr.ip())
    });
    assert_eq!(x.tip.tip(), (104, parent));
    // The template that the node serves is on the committed tip, and its block commits.
    let Some(producer) = &x.producer else {
        panic!("the node is a producer");
    };
    let template = producer.feed.current().expect("a template");
    assert_eq!(
        (template.tip.height, template.tip.parent_hash),
        (105, parent)
    );
    generate(&x, 1);
    assert_eq!(x.tip.tip().0, 105);
    x.shutdown().expect("clean shutdown");

    // The templates from the speculative one on: height 106 on the bad block, then
    // height 105 on the committed tip.
    let templates: Vec<(u64, String)> = rows(dir.path(), "x", "template.jsonl")
        .iter()
        .map(|row| {
            (
                row["height"].as_u64().expect("a height"),
                row["parent"].as_str().expect("a parent").to_string(),
            )
        })
        .skip_while(|(height, _)| *height != 106)
        .take_while(|(height, _)| *height <= 106)
        .collect();
    let bad_hash = header.hash().to_string();
    assert!(templates.len() >= 2, "{templates:?}");
    assert_eq!(templates[0], (106, bad_hash.clone()));
    let back = templates
        .iter()
        .find(|(_, hash)| *hash != bad_hash)
        .expect("a template after the speculative one");
    assert_eq!(*back, (105, parent.to_string()));
    let rejected = rows(dir.path(), "x", "commit_state.jsonl")
        .iter()
        .filter(|row| row["event"] == "block_validated" && row["result"] == "invalid")
        .count();
    assert_eq!(rejected, 1);
}

/// A compact-relay peer that records each message of the node. It asks for nothing by
/// itself: a test sends its requests with [`Tap::send`].
struct Tap {
    stream: TcpStream,
    /// The messages of the node, in the order of the wire, without `ping`.
    seen: Arc<parking_lot::Mutex<Vec<LegacyMessage>>>,
}

impl Tap {
    /// Connects to `node`, offers the compact relay with the lanes and the candidates, and
    /// waits until the node has the session.
    fn connect(node: &Node) -> Self {
        let mut stream = TcpStream::connect(addr(node)).expect("connect");
        // A reader that gets no message for this time ends: no test hangs on the socket.
        stream.set_read_timeout(Some(WAIT)).expect("a read timeout");
        let LegacyMessage::Version(mut hello) = version(0) else {
            unreachable!("version() gives a version message");
        };
        hello.services = hayai_net::RelayConfig::new(NET).services();
        send(&mut stream, &LegacyMessage::Version(hello)).expect("version");
        let sessions = |node: &Node| {
            let lanes = |peer: &hayai_net::PeerInfo| matches!(peer.protocol, PeerProtocol::CompactRelay(n) if n.candidates());
            node.relay.peers().into_iter().filter(lanes).count()
        };
        let before = sessions(node);
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (record, mut reader) = (seen.clone(), stream.try_clone().expect("clone"));
        thread::spawn(move || {
            while let Ok(message) = read_message(&mut reader, NET, usize::MAX) {
                let answer = match &message {
                    LegacyMessage::Version(_) => Some(LegacyMessage::Verack),
                    LegacyMessage::Ping(nonce) => Some(LegacyMessage::Pong(*nonce)),
                    // The tap has no header for the header sync of the node.
                    LegacyMessage::GetHeaders(_) => Some(LegacyMessage::Headers(Vec::new())),
                    LegacyMessage::CompactVer(_) => {
                        Some(LegacyMessage::CompactVer(hayai_net::CompactVer::CURRENT))
                    }
                    _ => None,
                };
                if let Some(answer) = answer {
                    let Ok(()) = send(&mut reader, &answer) else {
                        return;
                    };
                }
                if !matches!(message, LegacyMessage::Ping(_)) {
                    record.lock().push(message);
                }
            }
        });
        wait_for("the compact session of the tap", || sessions(node) > before);
        Self { stream, seen }
    }

    fn send(&mut self, message: LegacyMessage) {
        send(&mut self.stream, &message).expect("the node reads the tap");
    }

    /// The messages that the node sent before it answered a `ping` of this call: each
    /// message that the node queued before the call is in the result.
    fn messages(&mut self) -> Vec<LegacyMessage> {
        let nonce = rand::random();
        self.send(LegacyMessage::Ping(nonce));
        let seen = self.seen.clone();
        wait_for("the pong", || {
            seen.lock().contains(&LegacyMessage::Pong(nonce))
        });
        let all = self.seen.lock().clone();
        let end = all
            .iter()
            .position(|message| *message == LegacyMessage::Pong(nonce))
            .expect("the pong");
        all[..end].to_vec()
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// A node with the compact relay and `[mining] lane_publication = publication`.
fn start_publishing(
    dir: &Path,
    name: &str,
    produce: bool,
    publication: crate::config::LanePublication,
) -> Node {
    let mut config = config_with(dir, name, produce, true, "");
    config.mining.lane_publication = publication;
    Node::start(&config).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The node of the lane tests: a producer with 105 blocks. Returns the node and two
/// mature coins.
fn lane_node(
    dir: &Path,
    publication: crate::config::LanePublication,
) -> (Node, [(hayai_coins::OutPoint, u64); 2]) {
    let a = start_publishing(dir, "a", true, publication);
    generate(&a, 105);
    let chain = fetch_chain(&a);
    (a, [coinbase_coin(&chain[0]), coinbase_coin(&chain[1])])
}

/// What the lane tests read on the wire of the tap.
#[derive(Default)]
struct Wire {
    batches: Vec<hayai_relay::BatchAnnounce>,
    candidates: usize,
    candidate_blocks: usize,
    compact_blocks: usize,
    announced: Vec<hayai_wire::WtxId>,
}

fn wire(messages: &[LegacyMessage]) -> Wire {
    use hayai_relay::Message;

    let mut wire = Wire::default();
    for message in messages {
        match message {
            LegacyMessage::Compact(Message::BatchAnnounce(batch)) => {
                wire.batches.push(batch.clone())
            }
            LegacyMessage::Compact(Message::CandidateAnnounce(_)) => wire.candidates += 1,
            LegacyMessage::Compact(Message::CandidateBlock(_)) => wire.candidate_blocks += 1,
            LegacyMessage::Compact(Message::CompactBlock(_)) => wire.compact_blocks += 1,
            LegacyMessage::Compact(Message::TxAnnounce(announce)) => {
                wire.announced.extend_from_slice(&announce.ids)
            }
            _ => {}
        }
    }
    wire
}

/// `lane_publication = "all"`: the node publishes its template to a hayai peer as batches
/// and candidates. This is the control of the two tests below: the tap reads the lane.
#[test]
fn a_miner_publishes_its_template_as_a_lane() {
    let dir = scratch();
    let (a, [coin, _]) = lane_node(dir.path(), crate::config::LanePublication::All);
    let mut tap = Tap::connect(&a);
    let tx = shielding(&coin, 15_000, 0);
    a.submit_tx(tx.clone()).expect("valid");
    wait_template(&a, 1);
    generate(&a, 1);
    let wire = wire(&tap.messages());
    assert!(wire.candidates >= 1, "no candidate on the wire");
    assert!(
        wire.batches.iter().any(|batch| batch.ids == [tx.wtxid()]),
        "no batch with the transaction: {:?}",
        wire.batches
    );
    assert_eq!(wire.announced, [tx.wtxid()]);
    assert_eq!(wire.candidate_blocks + wire.compact_blocks, 1);
    a.shutdown().expect("clean shutdown");
}

/// `lane_publication = "none"`: no batch and no candidate leaves the node, and its block
/// still goes out over the compact relay. The node still takes the lane of another miner
/// and sends it on.
#[test]
fn a_miner_without_publication_sends_no_batch_and_no_candidate() {
    let dir = scratch();
    let (a, [first, second]) = lane_node(dir.path(), crate::config::LanePublication::None);
    let mut tap = Tap::connect(&a);
    let tx = shielding(&first, 15_000, 0);
    a.submit_tx(tx.clone()).expect("valid");
    wait_template(&a, 1);
    generate(&a, 1);
    let alone = wire(&tap.messages());
    assert!(alone.batches.is_empty(), "{:?}", alone.batches);
    assert_eq!((alone.candidates, alone.candidate_blocks), (0, 0));
    // The transaction and the block went out as before.
    assert_eq!(alone.announced, [tx.wtxid()]);
    assert_eq!(alone.compact_blocks, 1);

    // A second node publishes its template. Its lane reaches the tap through the first
    // node, and it follows the blocks of the first node.
    let b = start(dir.path(), "b", false, true);
    b.relay.connect(addr(&a)).expect("b dials a");
    wait_tip(&b, a.tip.tip());
    let tx = shielding(&second, 15_000, 0);
    a.submit_tx(tx.clone()).expect("valid");
    wait_for("the lane of the second node on the tap", || {
        let seen = wire(&tap.messages());
        seen.candidates >= 1 && seen.batches.iter().any(|batch| batch.ids == [tx.wtxid()])
    });
    generate(&a, 1);
    wait_tip(&b, a.tip.tip());
    let chain = fetch_chain(&b);
    assert_eq!((tx_count(&chain, 106), tx_count(&chain, 107)), (2, 2));
    for node in [a, b] {
        node.shutdown().expect("clean shutdown");
    }
}

/// A private transaction is in the template and in the block. Before the block, no
/// message of the node has it: no announcement, no batch, and no answer to `mempool`,
/// `getdata` and `TxRequest`. `publication` is `public` or `none`.
fn a_private_transaction_stays_off_the_wire(publication: crate::config::LanePublication) {
    use hayai_relay::{Message, TxRequest};

    let dir = scratch();
    let (a, [public_coin, private_coin]) = lane_node(dir.path(), publication);
    // The second node publishes no lane: each batch on the tap is a batch of the first.
    let b = start_publishing(dir.path(), "b", false, crate::config::LanePublication::None);
    b.relay.connect(addr(&a)).expect("b dials a");
    wait_tip(&b, a.tip.tip());
    let mut tap = Tap::connect(&a);

    let public = shielding(&public_coin, 15_000, 0);
    let private = shielding(&private_coin, 15_000, 0);
    a.submit_tx(public.clone()).expect("valid");
    a.submit_private_tx(private.clone()).expect("valid");
    assert!(a.mempool.is_private(&private.wtxid()));
    // A second private admission does not change the mark.
    let Err(crate::mempool::Reject::Known) = a.submit_private_tx(private.clone()) else {
        panic!("the store has the transaction");
    };
    assert!(a.mempool.is_private(&private.wtxid()));
    wait_template(&a, 2);

    // The tap asks for the transaction on each path.
    let id = private.wtxid();
    tap.send(LegacyMessage::Mempool);
    tap.send(LegacyMessage::GetData(vec![
        InvItem::Wtx(id),
        InvItem::Tx(id.txid),
    ]));
    tap.send(LegacyMessage::Compact(Message::TxRequest(TxRequest {
        ids: vec![id],
    })));
    let before = tap.messages();
    let txid: &[u8; 32] = private.txid.as_ref();
    // The answer to `getdata` names the two items as not found, and the answer to
    // `mempool` has the public transaction only. No other message has the id or the
    // bytes of the private transaction.
    let not_found = LegacyMessage::NotFound(vec![InvItem::Wtx(id), InvItem::Tx(id.txid)]);
    assert!(before.contains(&not_found), "{before:?}");
    assert!(before.contains(&LegacyMessage::Inv(vec![InvItem::Wtx(public.wtxid())])));
    assert_eq!(wire(&before).announced, [public.wtxid()]);
    // The mempool of the other node has the public transaction only.
    wait_for("the public transaction on b", || {
        b.mempool.contains(&public.wtxid())
    });
    assert!(!b.mempool.contains(&id));

    generate(&a, 1);
    wait_tip(&b, a.tip.tip());
    // Each message that the node sent up to the commit of the block.
    let after = tap.messages();
    let at = after
        .iter()
        .position(|message| {
            matches!(
                message,
                LegacyMessage::Compact(Message::CompactBlock(_) | Message::CandidateBlock(_))
            )
        })
        .expect("the block on the wire");
    for message in after[..at].iter().filter(|message| **message != not_found) {
        let bytes = encode(NET, message);
        assert!(
            !contains(&bytes, txid) && !contains(&bytes, &private.bytes[..64]),
            "the private transaction is on the wire before its block: {message:?}"
        );
    }
    let seen = wire(&after);
    assert_eq!(seen.announced, [public.wtxid()]);
    match publication {
        crate::config::LanePublication::None => {
            assert!(seen.batches.is_empty());
            assert_eq!(seen.candidates, 0);
        }
        _ => {
            // The lane has the public transaction only.
            assert!(seen.candidates >= 1);
            let ids: Vec<_> = seen.batches.iter().flat_map(|b| b.ids.clone()).collect();
            assert_eq!(ids, [public.wtxid()]);
        }
    }
    // The block has the bytes of the private transaction for a peer that does not have
    // them, and the mark ends with the block.
    let block = &after[at];
    assert!(contains(&encode(NET, block), &private.bytes));
    assert!(!a.mempool.is_private(&id));
    let chain = fetch_chain(&b);
    assert_eq!(tx_count(&chain, 106), 3);
    assert!(parse(&chain[105]).txs.iter().any(|tx| tx.wtxid() == id));
    for node in [a, b] {
        node.shutdown().expect("clean shutdown");
    }
}

#[test]
fn a_private_transaction_is_not_in_the_published_lane() {
    a_private_transaction_stays_off_the_wire(crate::config::LanePublication::Public);
}

#[test]
fn a_private_transaction_of_a_miner_without_publication_stays_off_the_wire() {
    a_private_transaction_stays_off_the_wire(crate::config::LanePublication::None);
}

/// One JSON-RPC call over HTTP to `node`, with the credentials of its cookie file. The
/// read has a bound of 60 s.
fn rpc_call(node: &Node, method: &str, params: Value) -> Value {
    use std::io::Read;

    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
        .to_string();
    let cookie = node.rpc_cookie.as_ref().expect("the node has a cookie");
    let authorization = hayai_rpc::cookie::authorization(cookie).expect("cookie file");
    let mut stream =
        TcpStream::connect(node.rpc_addr.expect("the node serves RPC")).expect("connect");
    stream.set_read_timeout(Some(WAIT)).expect("a read timeout");
    write!(
        stream,
        "POST / HTTP/1.1\r\nHost: x\r\nAuthorization: {authorization}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("write");
    let mut answer = String::new();
    stream.read_to_string(&mut answer).expect("answer");
    let (_, json) = answer.split_once("\r\n\r\n").expect("an HTTP body");
    serde_json::from_str(json).expect("JSON")
}

/// `sendprivatetransaction` over the RPC server: a node with `lane_publication = "all"`
/// refuses it, and each other node stores the transaction with the private mark.
/// `getrawmempool` and `getmempoolinfo` show the transaction of the mempool.
#[test]
fn sendprivatetransaction_needs_a_node_that_takes_private_transactions() {
    use crate::config::LanePublication;

    for (publication, takes) in [
        (LanePublication::All, false),
        (LanePublication::Public, true),
        (LanePublication::None, true),
    ] {
        let dir = scratch();
        let rpc = "[rpc]\nlisten_addr = \"127.0.0.1:0\"\n";
        let mut config = config_with(dir.path(), "a", true, true, rpc);
        config.mining.lane_publication = publication;
        let a = Node::start(&config).expect("node a");
        generate(&a, 105);
        let tx = shielding(&coinbase_coin(&fetch_chain(&a)[0]), 15_000, 0);
        let hexdata = serde_json::json!([hex::encode(&tx.bytes)]);
        let mut txid: [u8; 32] = *tx.txid.as_ref();
        txid.reverse();
        let txid = hex::encode(txid);

        let answer = rpc_call(&a, "sendprivatetransaction", hexdata.clone());
        let empty = serde_json::json!({ "size": 0, "bytes": 0, "usage": 0 });
        if !takes {
            assert_eq!(answer["error"]["code"], -26, "{answer}");
            let message = answer["error"]["message"].as_str().expect("a message");
            assert!(message.contains("lane_publication"), "{message}");
            assert_eq!(
                rpc_call(&a, "getmempoolinfo", serde_json::json!([]))["result"],
                empty
            );
            // The same transaction is a public transaction for this node.
            let answer = rpc_call(&a, "sendrawtransaction", hexdata);
            assert_eq!(answer["result"], txid, "{answer}");
        } else {
            assert_eq!(answer["result"], txid, "{answer}");
        }
        assert_eq!(a.mempool.is_private(&tx.wtxid()), takes);
        assert_eq!(
            rpc_call(&a, "getrawmempool", serde_json::json!([]))["result"],
            serde_json::json!([txid])
        );
        let size = tx.bytes.len();
        assert_eq!(
            rpc_call(&a, "getmempoolinfo", serde_json::json!([]))["result"],
            serde_json::json!({ "size": 1, "bytes": size, "usage": size })
        );
        a.shutdown().expect("clean shutdown");
    }
}

/// What two nodes with the same chain have in common: the tip, the tree roots, the
/// history root, the value pools and the coin of each output of the chain `blocks`.
fn state_digest(node: &Node, blocks: &[Bytes]) -> String {
    use hayai_coins::CoinsView;

    let view = node.mempool.view();
    let outpoints: Vec<hayai_coins::OutPoint> = blocks
        .iter()
        .flat_map(|block| parse(block).txs)
        .flat_map(|tx| {
            let outputs = tx.tx.transparent_bundle().map_or(0, |b| b.vout.len());
            (0..outputs as u32).map(move |n| hayai_coins::OutPoint::new(*tx.txid.as_ref(), n))
        })
        .collect();
    let coins = view.get_coins(&outpoints);
    let unspent = coins.iter().flatten().count();
    let frontiers = view.frontiers();
    format!(
        "{:?} {:?} {:?} {:?} {:?} {unspent} {coins:?}",
        view.tip(),
        frontiers.anchors,
        frontiers.sprout.root(),
        view.history().map(|history| history.root()),
        view.value_pools(),
    )
}

/// Measurement, not a check: the rate of the synchronization of 2,000 generated blocks
/// from one peer, and the rate with three transactions in the mempool of the node. Run it
/// with `cargo test -p hayaid --release --lib measure_the_sync_rate -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement; it prints the rate"]
fn measure_the_sync_rate_of_2000_blocks() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, false);
    generate(&a, 2_000);
    let tip = a.tip.tip();
    for run in 0..3 {
        let name = format!("x{run}");
        // The flush interval of the default configuration.
        let mut config = config_with(dir.path(), &name, false, false, "");
        config.state.flush_interval_blocks = 100;
        let x = Node::start(&config).expect("the node starts");
        let started = Instant::now();
        x.relay.connect(addr(&a)).expect("x dials a");
        wait_tip(&x, tip);
        let elapsed = started.elapsed();
        x.shutdown().expect("clean shutdown");
        let templates = rows(dir.path(), &name, "template.jsonl").len();
        println!(
            "run {run}: 2000 blocks in {:.3} s, {:.0} blocks/s, {templates} template rows",
            elapsed.as_secs_f64(),
            2_000.0 / elapsed.as_secs_f64()
        );
    }
    a.shutdown().expect("clean shutdown");
}

/// The tests that the fast CI does not run (`--skip slow::`): the synchronization with
/// stops at random points, and the scenarios that wait for the timeout of a peer.
mod slow {
    use super::*;

    /// A peer that sends the headers and four blocks, then no block: the node gets the chain
    /// from the other peer. A peer that does not answer `getheaders`: the node disconnects it
    /// and takes another peer for the header sync.
    #[test]
    fn a_peer_that_stalls_does_not_stop_the_synchronization() {
        let dir = scratch();
        let a = start(dir.path(), "a", true, false);
        generate(&a, 120);
        let tip = a.tip.tip();
        let chain = with_bodies(&fetch_chain(&a));
        // The peer of the header sync is the peer with the largest height: the silent one.
        let mut longer = chain.clone();
        let mut extra = chain[119].0.clone();
        extra.prev_hash = chain[119].0.hash();
        extra.time += 1;
        longer.push((extra, None));
        let silent = ScriptedPeer::serve(
            [127, 0, 0, 2],
            longer,
            Script {
                headers: false,
                blocks: 0,
                ..Script::default()
            },
        );
        let slow = ScriptedPeer::serve(
            [127, 0, 0, 3],
            chain,
            Script {
                headers: true,
                blocks: 4,
                ..Script::default()
            },
        );

        let x = start(dir.path(), "x", false, false);
        x.relay
            .connect(silent.addr)
            .expect("x dials the silent peer");
        wait_for("the handshake with the silent peer", || {
            x.relay.peers().iter().any(|p| p.established)
        });
        x.relay.connect(slow.addr).expect("x dials the slow peer");
        x.relay.connect(addr(&a)).expect("x dials a");
        wait_tip(&x, tip);
        // The header sync left the silent peer after the timeout.
        wait_for("the disconnection of the silent peer", || {
            silent.closed.load(Ordering::Relaxed) >= 1
        });
        // The slow peer got requests and sent four blocks.
        assert!(slow.requested.load(Ordering::Relaxed) > 4);
        x.shutdown().expect("clean shutdown");
        a.shutdown().expect("clean shutdown");
    }

    /// A peer reports the largest height and answers each `getheaders` with the same 160
    /// headers. The node asks it one time more, sees no new header, and takes its other peer
    /// for the header sync: it reaches the tip.
    #[test]
    fn a_peer_that_repeats_known_headers_does_not_hold_the_header_sync() {
        let dir = scratch();
        let a = start(dir.path(), "a", true, false);
        generate(&a, 400);
        let tip = a.tip.tip();
        let blocks = fetch_chain(&a);
        let looper = ScriptedPeer::serve(
            [127, 0, 0, 11],
            with_bodies(&blocks[..MAX_HEADERS]),
            Script {
                repeat_headers: true,
                start_height: Some(u32::MAX),
                ..Script::default()
            },
        );
        let x = start(dir.path(), "x", false, false);
        x.relay.connect(looper.addr).expect("x dials the looper");
        wait_for("the handshake with the looper", || {
            x.relay.peers().iter().any(|p| p.established)
        });
        x.relay.connect(addr(&a)).expect("x dials a");
        wait_tip(&x, tip);
        x.shutdown().expect("clean shutdown");
        a.shutdown().expect("clean shutdown");
    }

    /// A peer sends a header chain of 130 blocks and no body. A second peer has a chain of
    /// 120 blocks that leaves the first chain at height 100. The node takes the header chain
    /// without bodies out of the fork choice, synchronizes the chain of the second peer, and
    /// its template and its next block are on that chain. The second peer gets no request
    /// for a block that it does not have, and no penalty.
    #[test]
    fn a_header_chain_without_bodies_does_not_stop_the_node() {
        let dir = scratch();
        let a = start(dir.path(), "a", true, false);
        let b = start(dir.path(), "b", true, false);
        generate(&a, 100);
        b.relay.connect(addr(&a)).expect("b dials a");
        wait_tip(&b, a.tip.tip());
        disconnect_all(&a);
        disconnect_all(&b);
        wait_for("the partition", || {
            a.relay.peers().is_empty() && b.relay.peers().is_empty()
        });
        generate(&a, 20);
        generate(&b, 30);
        let tip = a.tip.tip();
        let withheld = fetch_chain(&b);
        b.shutdown().expect("clean shutdown");
        let silent = ScriptedPeer::serve(
            [127, 0, 0, 15],
            with_bodies(&withheld),
            Script {
                blocks: 0,
                ..Script::default()
            },
        );

        let x = start_with(
            dir.path(),
            "x",
            true,
            false,
            "[metrics]\nlisten_addr = \"127.0.0.1:0\"\n",
        );
        x.relay
            .connect(silent.addr)
            .expect("x dials the silent peer");
        wait_for("the handshake with the silent peer", || {
            x.relay.peers().iter().any(|p| p.established)
        });
        x.relay.connect(addr(&a)).expect("x dials a");
        wait_tip(&x, tip);
        assert_eq!(metric(&x, "hayai_sync_bodies_withheld"), 1.0);
        assert_eq!(metric(&x, "hayai_sync_withheld_chains_total"), 1.0);
        assert_eq!(metric(&x, "hayai_sync_header_height"), 120.0);
        // The honest peer has no score and no stall.
        let honest = x
            .relay
            .peers()
            .into_iter()
            .find(|p| p.addr == addr(&a))
            .expect("the honest peer is connected");
        assert_eq!(x.relay.peer_manager().score(honest.addr.ip()), 0);
        // The template is on the validated tip, and the next block of the node extends it.
        let Some(producer) = &x.producer else {
            panic!("the node is a producer");
        };
        wait_for(
            "the template on the tip",
            || matches!(producer.feed.current(), Some(t) if t.tip.parent_hash == tip.1),
        );
        generate(&x, 1);
        assert_eq!(x.tip.tip().0, 121);
        wait_tip(&a, x.tip.tip());
        x.shutdown().expect("clean shutdown");
        a.shutdown().expect("clean shutdown");
    }

    /// A node synchronizes 1,200 blocks and stops as a crash does at more than 20 random
    /// heights (the seed is fixed). The layer window is 1,000 blocks, so the stops are before and after
    /// the first flush of finalized coins. Its final state equals the state of a node that
    /// synchronizes without a stop: tip, tree roots, history root, value pools and coins.
    #[test]
    fn stops_at_random_points_give_the_state_of_an_uninterrupted_synchronization() {
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};

        let dir = scratch();
        let a = start(dir.path(), "a", true, false);
        generate(&a, 104);
        let early = fetch_chain(&a);
        for height in 1..=3 {
            let coin = coinbase_coin(&early[height - 1]);
            a.submit_tx(shielding(&coin, 15_000, 0)).expect("valid");
            wait_template(&a, 1);
            generate(&a, 1);
        }
        generate(&a, 1_093);
        let tip = a.tip.tip();
        assert_eq!(tip.0, 1_200);
        let blocks = fetch_chain(&a);

        let mut rng = StdRng::seed_from_u64(0x0068_6179_6169);
        // Each stop is a random number of blocks after the height of the start before it.
        let mut stops = Vec::new();
        loop {
            let x = start(dir.path(), "x", false, false);
            let resumed = x.tip.tip().0;
            let point = resumed + rng.gen_range(5..=30);
            if point >= 1_200 {
                x.abandon().expect("abandon");
                break;
            }
            x.relay.connect(addr(&a)).expect("x dials a");
            let deadline = Instant::now() + WAIT;
            while x.tip.tip().0 < point {
                assert!(Instant::now() < deadline, "no synchronization to {point}");
                thread::sleep(Duration::from_micros(200));
            }
            let stopped = x.tip.tip().0;
            x.abandon().expect("abandon");
            stops.push((resumed, stopped));
        }
        assert!(stops.len() >= 20, "{} stops: {stops:?}", stops.len());
        assert!(
            stops.iter().any(|(_, stopped)| *stopped > 1_010),
            "{stops:?}"
        );

        let x = start(dir.path(), "x", false, false);
        x.relay.connect(addr(&a)).expect("x dials a");
        wait_tip(&x, tip);
        let y = start(dir.path(), "y", false, false);
        y.relay.connect(addr(&a)).expect("y dials a");
        wait_tip(&y, tip);
        let expected = state_digest(&y, &blocks);
        assert_eq!(state_digest(&x, &blocks), expected);
        assert_eq!(state_digest(&a, &blocks), expected);
        // The state on disk gives the same state after a clean stop.
        x.shutdown().expect("clean shutdown");
        let x = start(dir.path(), "x", false, false);
        assert_eq!(state_digest(&x, &blocks), expected);
        for node in [a, x, y] {
            node.shutdown().expect("clean shutdown");
        }
    }
}
