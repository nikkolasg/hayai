//! The JSON-RPC shim end to end over HTTP: template shape, long polling, submission,
//! malformed requests, cookie authentication. The servers of the tests whose subject is
//! not the authentication have no cookie.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use hayai_consensus::Network;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_rpc::cookie::{self, COOKIE_FILE};
use hayai_rpc::http::MAX_HEAD;
use hayai_rpc::{
    AddressUtxo, BlockGenerator, BlockInfo, BlockSubmitSink, ChainTip, Cookie, HttpServer,
    IndexError, MetricsServer, NodeQuery, NodeState, PeerRow, Registry, Rpc, RpcConfig,
    SubmitOutcome, SubmittedBlock, SubtreePool, SubtreeRow, TemplateFeed, TipSource, TipState,
    TransparentAddress, TxOutInfo,
};
use hayai_template::messages::{Hash32, HexBytes, Submit};
use hayai_template::submission::rebuild_block;
use hayai_template::{
    Candidate, CoinbaseSpec, LiveTemplate, SetEvent, TemplateConfig, TemplateUpdate, Tip,
    Zip317Params,
};
use hayai_wire::header::BlockHash;
use hayai_wire::{RawTx, WtxId};
use serde_json::{json, Value};

const BRANCH: BranchId = BranchId::Nu5;

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

/// `getblocktemplate` from NU7 and from the NSM reissuance height, on a test Regtest (NU7
/// at 9, reissuance at 12): `coinbasetxn.fee` is the negated miner share of the fees
/// (Zakura `TransactionTemplate::new_coinbase`), each transaction reports its whole fee,
/// and `coinbasetxn.data` is the coinbase with the subsidy, the reissuance bonus and the
/// miner share.
#[test]
fn getblocktemplate_gives_the_coinbase_of_the_nsm_rules() {
    use hayai_consensus::{subsidy, RegtestConfig, RuleSet, Upgrade};
    use hayai_crypto::zcash_primitives::transaction::Transaction;

    let network = RegtestConfig::new(&[(Upgrade::Nu7, 9)], Vec::new(), 0)
        .expect("a valid configuration")
        .with_test_reissuance_height(12)
        .network();
    let Some(_) = RuleSet::of(Upgrade::Nu7) else {
        return;
    };
    let mut spec = coinbase_spec(b"");
    spec.network = network;
    let mut live = LiveTemplate::new(TemplateConfig::new(spec));
    let balance = 2_000_000_000u64;
    for (height, bonus) in [(11u32, 0u64), (12, 275), (13, 275)] {
        let scheduled = subsidy::scheduled_issuance(network, height - 1);
        let issued = u64::try_from(scheduled).expect("fits") - balance;
        let tip = Tip {
            issued_supply: Some(issued),
            ..tip(height)
        };
        live.on_tip(tip, &[], &[], |_| {}).unwrap();
        if height == 11 {
            live.apply(SetEvent::Added(candidate(1, 70_001, Vec::new())))
                .unwrap();
        }
        let template = live.current().unwrap();
        let result = hayai_rpc::block_template(template, network, 0, None).unwrap();
        let v = serde_json::to_value(&result).unwrap();
        // 42,000 of the 70,001 zatoshis of fees stay out of the coinbase.
        assert_eq!(v["coinbasetxn"]["fee"], -28_001, "{height}");
        assert_eq!(v["transactions"][0]["fee"], 70_001);
        let data = hex::decode(v["coinbasetxn"]["data"].as_str().unwrap()).unwrap();
        let coinbase = Transaction::read(&data[..], BranchId::Sprout).unwrap();
        let miner = coinbase.transparent_bundle().unwrap().vout[0]
            .value()
            .into_u64();
        assert_eq!(miner, 208_333_333 + bonus + 28_001, "{height}");
    }
}

fn candidate(n: u32, fee: u64, depends_on: Vec<WtxId>) -> Candidate {
    let raw = RawTx::parse(Bytes::from(tx_bytes(n)), BRANCH).unwrap();
    let conventional_fee = 10_000;
    Candidate {
        wtxid: raw.wtxid(),
        bytes: raw.bytes.clone(),
        fee,
        conventional_fee,
        weight_ratio: Zip317Params::ZIP317.weight_ratio(fee, conventional_fee),
        unpaid_actions: Zip317Params::ZIP317.unpaid_actions(fee, conventional_fee),
        sigops: 1,
        orchard_actions: 0,
        ironwood_actions: 0,
        sapling_ios: 0,
        depends_on,
        spends: Vec::new(),
    }
}

fn coinbase_spec(miner_data: &[u8]) -> CoinbaseSpec {
    let mut p2pkh = vec![0x76, 0xa9, 0x14];
    p2pkh.extend_from_slice(&[0x42; 20]);
    p2pkh.extend_from_slice(&[0x88, 0xac]);
    CoinbaseSpec {
        script_pubkey: p2pkh,
        miner_data: miner_data.to_vec(),
        network: Network::Regtest,
    }
}

fn tip(height: u32) -> Tip {
    Tip {
        parent_hash: BlockHash([height as u8; 32]),
        height,
        time: 1_700_000_000 + height,
        median_time_past: 1_699_999_000 + height,
        bits: 0x1f07_ffff,
        history_root: [0x11; 32],
        issued_supply: None,
    }
}

#[derive(Default)]
struct Sink {
    got: Mutex<Vec<String>>,
}

impl BlockSubmitSink for Sink {
    fn submit(&self, block: SubmittedBlock) -> SubmitOutcome {
        let (label, nonce) = match &block {
            SubmittedBlock::FromTemplate(b) => {
                (format!("template:{}", b.template.id), b.header.nonce)
            }
            SubmittedBlock::Full(b) => ("full".to_string(), b.header.nonce),
        };
        self.got.lock().unwrap().push(label);
        match nonce[0] {
            9 => SubmitOutcome::Rejected("bad".into()),
            8 => SubmitOutcome::Duplicate,
            _ => SubmitOutcome::Accepted,
        }
    }
}

struct Chain;

impl TipSource for Chain {
    fn tip(&self) -> (u32, BlockHash) {
        (99, BlockHash([0xcc; 32]))
    }
}

struct Harness {
    live: Mutex<LiveTemplate>,
    feed: Arc<TemplateFeed>,
    sink: Arc<Sink>,
    server: Arc<HttpServer>,
}

fn harness() -> Harness {
    let feed = TemplateFeed::new(Duration::from_secs(60));
    let mut live = LiveTemplate::new(TemplateConfig::new(coinbase_spec(b"hayai")));
    live.on_tip(tip(100), &[], &[], |u| feed.publish(&u))
        .unwrap();
    let c1 = candidate(1, 20_000, vec![]);
    let c2 = candidate(2, 50_000, vec![c1.wtxid]);
    for c in [c1, c2] {
        if let Some(u) = live.apply(SetEvent::Added(c)).unwrap() {
            feed.publish(&u);
        }
    }
    let sink = Arc::new(Sink::default());
    let mut config = RpcConfig::new(Network::Regtest);
    config.long_poll_set_delay = Duration::from_millis(150);
    config.long_poll_max = Duration::from_millis(600);
    let rpc = Rpc::new(config, feed.clone(), sink.clone(), Arc::new(Chain));
    let server = HttpServer::serve("127.0.0.1:0", rpc, None).unwrap();
    Harness {
        live: Mutex::new(live),
        feed,
        sink,
        server,
    }
}

struct Client {
    stream: BufReader<TcpStream>,
}

impl Client {
    fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Self {
            stream: BufReader::new(stream),
        }
    }

    fn raw(&mut self, request: &str) -> (u16, Vec<u8>) {
        self.stream.get_mut().write_all(request.as_bytes()).unwrap();
        let mut line = String::new();
        self.stream.read_line(&mut line).unwrap();
        let status: u16 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut len = 0;
        loop {
            let mut h = String::new();
            self.stream.read_line(&mut h).unwrap();
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some(v) = h.strip_prefix("Content-Length: ") {
                len = v.parse().unwrap();
            }
        }
        let mut body = vec![0u8; len];
        self.stream.read_exact(&mut body).unwrap();
        (status, body)
    }

    fn post(&mut self, body: &str) -> (u16, Value) {
        let request = format!(
            "POST / HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (status, body) = self.raw(&request);
        (status, serde_json::from_slice(&body).unwrap())
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (status, v) =
            self.post(&json!({"id": 1, "method": method, "params": params}).to_string());
        assert_eq!(status, 200);
        v
    }
}

#[test]
fn getblocktemplate_has_zcashd_shape() {
    let h = harness();
    let mut c = Client::connect(h.server.addr());
    let v = c.call("getblocktemplate", json!([]));
    assert_eq!(v["error"], Value::Null, "{v}");
    let r = &v["result"];
    let template = h.feed.current().unwrap();
    assert_eq!(r["height"], 100);
    assert_eq!(r["version"], 4);
    assert_eq!(r["previousblockhash"], tip(100).parent_hash.to_string());
    assert_eq!(r["bits"], "1f07ffff");
    assert_eq!(
        r["target"],
        "0007ffff00000000000000000000000000000000000000000000000000000000"
    );
    assert_eq!(r["longpollid"], template.id.to_string());
    assert_eq!(r["workid"], template.id.to_string());
    // The median-time-past of the tip plus 1 s and plus 90 min: the limits of the header
    // rules. The clock of the test is above `maxtime`, and `curtime` stays at the limit.
    assert_eq!(r["mintime"], 1_699_999_100 + 1);
    assert_eq!(r["maxtime"], 1_699_999_100 + 90 * 60);
    assert_eq!(r["curtime"], r["maxtime"]);
    assert_eq!(r["sizelimit"], 2_000_000);
    assert_eq!(r["sigoplimit"], 20_000);
    assert_eq!(r["noncerange"], "00000000ffffffff");
    assert_eq!(r["mutable"], json!(["time", "transactions", "prevblock"]));
    assert!(r.get("submitold").is_none());
    let commitments = BlockHash(template.block_commitments).to_string();
    assert_eq!(r["blockcommitmentshash"], commitments);
    assert_eq!(r["lightclientroothash"], commitments);
    assert_eq!(r["finalsaplingroothash"], commitments);
    assert_eq!(r["defaultroots"]["blockcommitmentshash"], commitments);
    assert_eq!(
        r["defaultroots"]["merkleroot"],
        BlockHash(template.merkle_root).to_string()
    );
    assert_eq!(
        r["defaultroots"]["authdataroot"],
        BlockHash(template.auth_data_root).to_string()
    );
    assert_eq!(
        r["defaultroots"]["chainhistoryroot"],
        BlockHash([0x11; 32]).to_string()
    );

    let txs = r["transactions"].as_array().unwrap();
    assert_eq!(txs.len(), 2);
    // The block order is the order of the template: the parent precedes its child.
    for (i, t) in template.txs.iter().enumerate() {
        assert_eq!(txs[i]["data"], hex::encode(&t.bytes));
        assert_eq!(
            txs[i]["hash"],
            BlockHash(*t.wtxid.txid.as_ref()).to_string()
        );
        assert_eq!(
            txs[i]["authdigest"],
            BlockHash(t.wtxid.auth_digest).to_string()
        );
        assert_eq!(txs[i]["fee"], t.fee);
        assert_eq!(txs[i]["sigops"], 1);
        assert_eq!(txs[i]["required"], false);
    }
    let parent_index = template
        .txs
        .iter()
        .position(|t| t.depends_on.is_empty())
        .unwrap();
    let child_index = 1 - parent_index;
    assert_eq!(txs[parent_index]["depends"], json!([]));
    assert_eq!(txs[child_index]["depends"], json!([parent_index + 1]));

    let cb = &r["coinbasetxn"];
    assert_eq!(cb["data"], hex::encode(&template.coinbase.bytes));
    assert_eq!(
        cb["hash"],
        BlockHash(*template.coinbase.txid.as_ref()).to_string()
    );
    assert_eq!(cb["fee"], -70_000);
    assert_eq!(template.coinbase.miner_fees, 70_000);
    assert_eq!(cb["sigops"], 1, "one P2PKH output");
    assert_eq!(cb["required"], true);
    assert_eq!(cb["depends"], json!([]));

    // Trivial chain queries.
    assert_eq!(c.call("getblockcount", json!([]))["result"], 99);
    assert_eq!(
        c.call("getbestblockhash", json!([]))["result"],
        BlockHash([0xcc; 32]).to_string()
    );
    h.server.shutdown();
}

#[test]
fn long_poll_wakes_on_tip_set_and_timeout() {
    let h = Arc::new(harness());
    let current = h.feed.current().unwrap().id;

    // Without the capability, longpollid returns at once.
    let mut c = Client::connect(h.server.addr());
    let start = Instant::now();
    let v = c.call(
        "getblocktemplate",
        json!([{ "longpollid": current.to_string() }]),
    );
    assert!(start.elapsed() < Duration::from_millis(100));
    assert_eq!(v["result"]["longpollid"], current.to_string());

    // A tip event wakes the poll immediately with submitold false.
    let (tx, rx) = mpsc::channel();
    let addr = h.server.addr();
    std::thread::spawn(move || {
        let mut c = Client::connect(addr);
        let v = c.call(
            "getblocktemplate",
            json!([{ "longpollid": current.to_string(), "capabilities": ["longpoll"] }]),
        );
        tx.send(v).unwrap();
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    h.live
        .lock()
        .unwrap()
        .on_tip(tip(101), &[], &[], |u| h.feed.publish(&u))
        .unwrap();
    let v = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(v["result"]["height"], 101);
    assert_eq!(v["result"]["submitold"], false);
    // A tip publishes the coinbase-only template, then the full one at once. The waiter
    // wakes on the first publication that it observes. It therefore gets either of the two,
    // as the thread scheduling decides. Both are correct: a long poll returns the newest
    // template.
    let full_id = h.feed.current().unwrap().id;
    let got_id = v["result"]["longpollid"].as_str().unwrap().to_string();
    let got_txs = v["result"]["transactions"].as_array().unwrap().len();
    if got_id == full_id.to_string() {
        assert_eq!(got_txs, 2);
    } else {
        assert_eq!(got_id, (full_id - 1).to_string());
        assert_eq!(got_txs, 0);
        let start = Instant::now();
        let v = c.call(
            "getblocktemplate",
            json!([{ "longpollid": got_id, "capabilities": ["longpoll"] }]),
        );
        assert!(start.elapsed() < Duration::from_millis(100));
        assert_eq!(v["result"]["longpollid"], full_id.to_string());
        assert_eq!(v["result"]["submitold"], true);
        assert_eq!(v["result"]["transactions"].as_array().unwrap().len(), 2);
    }

    // A set change wakes it after the configured delay with submitold true.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut c = Client::connect(addr);
        let start = Instant::now();
        let v = c.call(
            "getblocktemplate",
            json!([{ "longpollid": full_id.to_string(), "capabilities": ["longpoll"] }]),
        );
        tx.send((v, start.elapsed())).unwrap();
    });
    std::thread::sleep(Duration::from_millis(50));
    let update = h
        .live
        .lock()
        .unwrap()
        .apply(SetEvent::Added(candidate(3, 90_000, vec![])))
        .unwrap()
        .expect("selection changed");
    h.feed.publish(&update);
    let (v, elapsed) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
    assert_eq!(v["result"]["submitold"], true);
    assert_eq!(v["result"]["longpollid"], update.template().id.to_string());
    assert_eq!(v["result"]["transactions"].as_array().unwrap().len(), 3);

    // Nothing changes: the maximum wait returns the same template.
    let latest = update.template().id;
    let start = Instant::now();
    let v = c.call(
        "getblocktemplate",
        json!([{ "longpollid": latest.to_string(), "capabilities": ["longpoll"] }]),
    );
    assert!(start.elapsed() >= Duration::from_millis(600));
    assert_eq!(v["result"]["longpollid"], latest.to_string());
    assert_eq!(v["result"]["submitold"], true);

    // A stale id from a previous tip never waits.
    let start = Instant::now();
    let v = c.call(
        "getblocktemplate",
        json!([{ "longpollid": current.to_string(), "capabilities": ["longpoll"] }]),
    );
    assert!(start.elapsed() < Duration::from_millis(100));
    assert_eq!(v["result"]["submitold"], false);
    h.server.shutdown();
}

#[test]
fn submitblock_round_trip() {
    let h = harness();
    let template = h.feed.current().unwrap();
    let submit = |coinbase: Option<Bytes>, nonce: u8| Submit {
        template_id: template.id,
        time: 1_700_000_200,
        nonce: Hash32([nonce; 32]),
        solution: HexBytes(Bytes::from(vec![0u8; 1344])),
        coinbase: coinbase.map(HexBytes),
    };
    let block = h
        .feed
        .with_store(|s| rebuild_block(s, &submit(None, 1)))
        .unwrap();
    let mut c = Client::connect(h.server.addr());
    let v = c.call(
        "submitblock",
        json!([hex::encode(&block.bytes), { "workid": template.id.to_string() }]),
    );
    assert_eq!(v["result"], Value::Null);
    assert_eq!(v["error"], Value::Null);

    // A pool that rewrote the coinbase (extra nonce) still hits the template path.
    let override_cb = coinbase_spec(b"hayai-extranonce-0001")
        .build(100, template.fees_total)
        .unwrap();
    let block2 = h
        .feed
        .with_store(|s| rebuild_block(s, &submit(Some(override_cb.bytes.clone()), 2)))
        .unwrap();
    let v = c.call(
        "submitblock",
        json!([hex::encode(&block2.bytes), { "workid": template.id.to_string() }]),
    );
    assert_eq!(v["result"], Value::Null);

    // Without workid, or with a different transaction set, the node takes the full path.
    let v = c.call("submitblock", json!([hex::encode(&block.bytes)]));
    assert_eq!(v["result"], Value::Null);
    let mut other = block.bytes.to_vec();
    let coinbase_end = 1487 + 1 + template.coinbase.bytes.len();
    other.truncate(coinbase_end);
    other[1487] = 1;
    let v = c.call(
        "submitblock",
        json!([hex::encode(&other), { "workid": template.id.to_string() }]),
    );
    assert_eq!(v["result"], Value::Null);

    // Outcomes map to zcashd's strings.
    let rejected = h
        .feed
        .with_store(|s| rebuild_block(s, &submit(None, 9)))
        .unwrap();
    assert_eq!(
        c.call("submitblock", json!([hex::encode(&rejected.bytes)]))["result"],
        "rejected"
    );
    let duplicate = h
        .feed
        .with_store(|s| rebuild_block(s, &submit(None, 8)))
        .unwrap();
    assert_eq!(
        c.call("submitblock", json!([hex::encode(&duplicate.bytes)]))["result"],
        "duplicate"
    );

    // Undecodable blocks are a deserialization error. They are not a submission.
    let v = c.call("submitblock", json!(["zz"]));
    assert_eq!(v["error"]["code"], -22);
    let v = c.call("submitblock", json!([hex::encode([0u8; 40])]));
    assert_eq!(v["error"]["code"], -22);
    let v = c.call("submitblock", json!([]));
    assert_eq!(v["error"]["code"], -8);

    assert_eq!(
        *h.sink.got.lock().unwrap(),
        vec![
            format!("template:{}", template.id),
            format!("template:{}", template.id),
            "full".to_string(),
            "full".to_string(),
            "full".to_string(),
            "full".to_string(),
        ]
    );
    h.server.shutdown();
}

#[test]
fn malformed_requests_get_json_rpc_errors() {
    let h = harness();
    let mut c = Client::connect(h.server.addr());
    let (status, v) = c.post("{not json");
    assert_eq!(status, 200);
    assert_eq!(v["error"]["code"], -32700);
    assert_eq!(v["result"], Value::Null);
    assert_eq!(v["id"], Value::Null);

    let (_, v) = c.post("[1,2]");
    assert_eq!(v["error"]["code"], -32600);

    let (_, v) = c.post(r#"{"id": 7, "method": "getinfo", "params": []}"#);
    assert_eq!(v["error"]["code"], -32601);
    assert_eq!(v["id"], 7);
    assert!(v.get("result").is_some(), "1.0 responses carry both keys");

    // JSON-RPC 2.0 shapes.
    let (_, v) = c.post(r#"{"jsonrpc": "2.0", "id": "a", "method": "getblockcount"}"#);
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["result"], 99);
    assert!(v.get("error").is_none());
    let (_, v) = c.post(r#"{"jsonrpc": "2.0", "id": "a", "method": "nope"}"#);
    assert!(v.get("result").is_none());
    assert_eq!(v["error"]["code"], -32601);

    let (_, v) =
        c.post(r#"{"id": 1, "method": "getblocktemplate", "params": [{"mode": "proposal"}]}"#);
    assert_eq!(v["error"]["code"], -8);
    let (_, v) =
        c.post(r#"{"id": 1, "method": "getblocktemplate", "params": [{"longpollid": "x"}]}"#);
    assert_eq!(v["error"]["code"], -8);
    let (_, v) = c.post(r#"{"id": 1, "method": "getblocktemplate", "params": {"a": 1}}"#);
    assert_eq!(v["error"]["code"], -32600);

    // HTTP-level errors.
    let (status, _) = c.raw("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(status, 405);
    let (status, _) = c.raw("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n");
    assert_eq!(status, 400);

    // An HTTP/1.0 request closes after the response.
    let mut c = Client::connect(h.server.addr());
    let (status, body) = c.raw(&format!(
        "POST / HTTP/1.0\r\nContent-Length: {}\r\n\r\n{}",
        r#"{"id":1,"method":"getblockcount"}"#.len(),
        r#"{"id":1,"method":"getblockcount"}"#
    ));
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["result"],
        99
    );
    let mut rest = Vec::new();
    c.stream.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty());
    h.server.shutdown();
}

#[test]
fn no_template_yet_is_a_warmup_error() {
    let feed = TemplateFeed::new(Duration::from_secs(60));
    let rpc = Rpc::new(
        RpcConfig::new(Network::Regtest),
        feed,
        Arc::new(Sink::default()),
        Arc::new(Chain),
    );
    let v: Value =
        serde_json::from_slice(&rpc.handle(br#"{"id":1,"method":"getblocktemplate"}"#)).unwrap();
    assert_eq!(v["error"]["code"], -10);
}

/// Records the consensus branch id of the last transaction of each submitted block.
#[derive(Default)]
struct BranchSink {
    got: Mutex<Vec<BranchId>>,
}

impl BlockSubmitSink for BranchSink {
    fn submit(&self, block: SubmittedBlock) -> SubmitOutcome {
        let SubmittedBlock::Full(block) = block else {
            panic!("no template is stored");
        };
        let last = block.txs.last().unwrap();
        self.got.lock().unwrap().push(last.tx.consensus_branch_id());
        SubmitOutcome::Accepted
    }
}

/// A v4 transaction does not state its consensus branch id: the parser sets it. The node
/// parses a submitted block with the branch id of the height that its coinbase states,
/// also when that height is not the one after the tip.
#[test]
fn submitblock_parses_with_the_branch_id_of_the_block_height() {
    use hayai_consensus::Upgrade;
    use hayai_wire::header::{BlockHeader, PowParams};

    // A v4 transaction without inputs and outputs.
    let mut v4 = Vec::new();
    v4.extend_from_slice(&0x8000_0004u32.to_le_bytes());
    v4.extend_from_slice(&0x892F_2085u32.to_le_bytes());
    v4.extend_from_slice(&[0; 2 + 4 + 4 + 8 + 3]);
    let block_at = |height: u32| {
        let spec = CoinbaseSpec {
            network: Network::Mainnet,
            ..coinbase_spec(b"hayai")
        };
        let header = BlockHeader {
            version: 4,
            prev_hash: BlockHash([1; 32]),
            merkle_root: [2; 32],
            block_commitments: [3; 32],
            time: 1_700_000_000,
            bits: 0x1f07_ffff,
            nonce: [4; 32],
            solution: vec![0; PowParams::MAINNET.solution_len()],
        };
        let mut bytes = header.serialize();
        bytes.push(2);
        bytes.extend_from_slice(&spec.build(height, 0).unwrap().bytes);
        bytes.extend_from_slice(&v4);
        hex::encode(bytes)
    };
    let sink = Arc::new(BranchSink::default());
    // The tip of `Chain` is at height 99: the block after it is a Sprout block.
    let rpc = Rpc::new(
        RpcConfig::new(Network::Mainnet),
        TemplateFeed::new(Duration::from_secs(60)),
        sink.clone(),
        Arc::new(Chain),
    );
    let submit = |hex: String| -> Value {
        let request = json!({"id": 1, "method": "submitblock", "params": [hex]});
        serde_json::from_slice(&rpc.handle(request.to_string().as_bytes())).unwrap()
    };
    let nu6 = Network::Mainnet.activation_height(Upgrade::Nu6).unwrap();
    for height in [100, nu6 - 1, nu6] {
        let v = submit(block_at(height));
        assert_eq!((&v["result"], &v["error"]), (&Value::Null, &Value::Null));
    }
    assert_eq!(
        *sink.got.lock().unwrap(),
        vec![BranchId::Sprout, BranchId::Nu5, BranchId::Nu6]
    );
    // A block whose first transaction states no height does not decode.
    let coinbase_len = block_at(100).len() / 2 - 1487 - 1 - v4.len();
    let mut bytes = hex::decode(block_at(100)).unwrap();
    bytes[1487] = 1;
    bytes.drain(1488..1488 + coinbase_len);
    let v = submit(hex::encode(bytes));
    assert_eq!(v["error"]["code"], -22);
    assert_eq!(sink.got.lock().unwrap().len(), 3);
}

struct Producer {
    asked: Mutex<Vec<u32>>,
}

impl BlockGenerator for Producer {
    fn generate(&self, n: u32) -> Result<Vec<BlockHash>, String> {
        self.asked.lock().unwrap().push(n);
        if n > 5 {
            return Err("stopped at the second block".into());
        }
        Ok((0..n).map(|i| BlockHash([i as u8; 32])).collect())
    }
}

#[test]
fn generate_is_served_only_with_a_generator() {
    let h = harness();
    let mut c = Client::connect(h.server.addr());
    let v = c.call("generate", json!([2]));
    assert_eq!(v["error"]["code"], -32601);

    let producer = Arc::new(Producer {
        asked: Mutex::new(Vec::new()),
    });
    let rpc = Rpc::with_generator(
        RpcConfig::new(Network::Regtest),
        h.feed.clone(),
        h.sink.clone(),
        Arc::new(Chain),
        producer.clone(),
    );
    let server = HttpServer::serve("127.0.0.1:0", rpc, None).unwrap();
    let mut c = Client::connect(server.addr());
    let v = c.call("generate", json!([2]));
    assert_eq!(
        v["result"],
        json!([
            BlockHash([0; 32]).to_string(),
            BlockHash([1; 32]).to_string()
        ])
    );
    let v = c.call("generate", json!([9]));
    assert_eq!(v["error"]["code"], -32603);
    assert_eq!(v["error"]["message"], "stopped at the second block");
    let v = c.call("generate", json!(["two"]));
    assert_eq!(v["error"]["code"], -8);
    assert_eq!(*producer.asked.lock().unwrap(), vec![2, 9]);
}

/// A chain of the blocks 0 to 99 with the hash `[height; 32]`, and a mempool that takes a
/// transaction of one byte. `calls` has the calls that change the node.
#[derive(Default)]
struct State {
    calls: Mutex<Vec<String>>,
}

impl NodeQuery for State {
    fn block_hash(&self, height: u32) -> Option<BlockHash> {
        (height < 100).then_some(BlockHash([height as u8; 32]))
    }
    fn block_bytes(&self, hash: &BlockHash) -> Option<Bytes> {
        (hash.0[0] < 100).then(|| Bytes::from(vec![hash.0[0], 0xb1]))
    }
    fn tip_state(&self) -> TipState {
        let mut sapling_root = [0; 32];
        sapling_root[0] = 0x5a;
        TipState {
            height: 99,
            hash: BlockHash([99; 32]),
            time: 1_700_000_000,
            sapling_root,
            orchard_root: [0x0c; 32],
            ironwood_root: [0x1d; 32],
            value_pools: [
                ("transparent", 1),
                ("sprout", 2),
                ("sapling", 3),
                ("orchard", 4),
                ("ironwood", 5),
                ("lockbox", 6),
            ],
        }
    }
    fn mempool_txids(&self) -> Vec<[u8; 32]> {
        let mut id = [0; 32];
        id[0] = 0xaa;
        vec![id]
    }
    fn send_transaction(&self, bytes: Bytes, private: bool) -> Result<[u8; 32], String> {
        self.calls.lock().unwrap().push(format!("send {private}"));
        match bytes[..] {
            [first] => Ok([first; 32]),
            _ => Err("the policy refuses the transaction".into()),
        }
    }
    fn block_info(&self, _hash: &BlockHash) -> Option<BlockInfo> {
        None
    }
    /// One block for each 2 s, with the `nBits` of the Regtest limit: a work of 17.
    fn header_context(&self, height: u32) -> Option<(u32, u32)> {
        (height < 100).then_some((1_000 + 2 * height, 0x200f_0f0f))
    }
    fn chain_tips(&self) -> Vec<ChainTip> {
        vec![
            ChainTip {
                height: 99,
                hash: BlockHash([99; 32]),
                branch_len: 0,
                status: "active",
            },
            ChainTip {
                height: 98,
                hash: BlockHash([0xf0; 32]),
                branch_len: 2,
                status: "valid-fork",
            },
        ]
    }
    fn node_state(&self) -> NodeState {
        NodeState {
            version: (1, 2, 3),
            user_agent: "/mock:1.2.3/".into(),
            protocol_version: 170_160,
            services: (1 << 26) | 1,
            connections: 2,
            relay_fee_rate: 100,
        }
    }
    fn peers(&self) -> Vec<PeerRow> {
        vec![
            PeerRow {
                addr: "127.0.0.1:18344".parse().unwrap(),
                user_agent: Some("/peer:1/".into()),
                version: Some(170_190),
                inbound: true,
                ping_time: Some(0.25),
                ping_wait: None,
            },
            PeerRow {
                addr: "[::1]:18344".parse().unwrap(),
                user_agent: None,
                version: None,
                inbound: false,
                ping_time: None,
                ping_wait: Some(1.5),
            },
        ]
    }
    fn mempool_size(&self) -> (usize, usize) {
        (1, 250)
    }
    fn add_node(&self, addr: SocketAddr) -> bool {
        let mut calls = self.calls.lock().unwrap();
        let call = format!("addnode {addr}");
        let new = !calls.contains(&call);
        calls.push(call);
        new
    }
    fn ping(&self) {
        self.calls.lock().unwrap().push("ping".into());
    }
    fn stop(&self) {
        self.calls.lock().unwrap().push("stop".into());
    }
    fn mempool_transaction(&self, _txid: &[u8; 32]) -> Option<Bytes> {
        None
    }
    /// One coin: output 1 of the txid `[7; 32]`, at height 90, to an OP_TRUE script.
    fn tx_out(&self, txid: &[u8; 32], index: u32, _include_mempool: bool) -> Option<TxOutInfo> {
        (*txid == [7; 32] && index == 1).then(|| TxOutInfo {
            value: 150_000_000,
            script: Bytes::from_static(&[0x51]),
            height: Some(90),
            coinbase: true,
        })
    }
    // The node of this double runs without the wallet index.
    fn index_tip(&self) -> Result<(u32, BlockHash), IndexError> {
        Err(IndexError::Off)
    }
    fn transaction_location(&self, _txid: &[u8; 32]) -> Result<Option<(u32, u16)>, IndexError> {
        Err(IndexError::Off)
    }
    fn address_balance(&self, _a: &[TransparentAddress]) -> Result<(u64, u64), IndexError> {
        Err(IndexError::Off)
    }
    fn address_txids(
        &self,
        _a: &[TransparentAddress],
        _start: u32,
        _end: u32,
    ) -> Result<Vec<[u8; 32]>, IndexError> {
        Err(IndexError::Off)
    }
    fn address_utxos(
        &self,
        _a: &[TransparentAddress],
    ) -> Result<(Vec<AddressUtxo>, (u32, BlockHash)), IndexError> {
        Err(IndexError::Off)
    }
    fn subtrees(
        &self,
        _pool: SubtreePool,
        _start: u16,
        _limit: Option<u16>,
    ) -> Result<Vec<SubtreeRow>, IndexError> {
        Err(IndexError::Off)
    }
}

/// Without the wallet index, the methods of the index answer error -1 with the setting that
/// turns it on. `gettxout` reads the coin set and needs no index.
#[test]
fn the_methods_of_the_wallet_index_refuse_without_the_index() {
    let h = harness();
    let rpc = Rpc::with_parts(
        RpcConfig::new(Network::Regtest),
        h.feed.clone(),
        h.sink.clone(),
        Arc::new(Chain),
        None,
        Some(Arc::new(State::default())),
    );
    let server = HttpServer::serve("127.0.0.1:0", rpc, None).unwrap();
    let mut c = Client::connect(server.addr());
    use hayai_crypto::zcash_address::{ToAddress, ZcashAddress};
    use hayai_crypto::zcash_protocol::consensus::NetworkType;
    let address = ZcashAddress::from_transparent_p2sh(NetworkType::Test, [1; 20]).encode();
    let txid = hex::encode([7; 32]);
    for (method, params) in [
        ("getrawtransaction", json!([txid])),
        ("getaddressbalance", json!([address])),
        ("getaddresstxids", json!([{ "addresses": [address] }])),
        (
            "getaddressutxos",
            json!([{ "addresses": [address], "chainInfo": true }]),
        ),
        ("z_getsubtreesbyindex", json!(["sapling", 0])),
    ] {
        let v = c.call(method, params);
        assert_eq!(v["error"]["code"], -1, "{method}: {v}");
        let message = v["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("wallet_index = true"),
            "{method}: {message}"
        );
    }
    assert_eq!(
        c.call("getaddressbalance", json!(["not an address"]))["error"]["code"],
        -5
    );
    assert_eq!(
        c.call("z_getsubtreesbyindex", json!(["sprout", 0]))["error"]["code"],
        -1
    );
    let out = c.call("gettxout", json!([txid, 1]))["result"].clone();
    assert_eq!(out["bestblock"], BlockHash([0xcc; 32]).to_string());
    assert_eq!(out["confirmations"], 10);
    assert_eq!(out["value"], 1.5);
    assert_eq!(out["coinbase"], true);
    assert_eq!(
        out["scriptPubKey"],
        json!({ "hex": "51", "type": "nonstandard" })
    );
    assert_eq!(c.call("gettxout", json!([txid, 0]))["result"], Value::Null);
}

/// The methods of the module `info`.
const INFO_METHODS: [&str; 18] = [
    "getinfo",
    "getmininginfo",
    "getblocksubsidy",
    "getnetworksolps",
    "getnetworkhashps",
    "getdifficulty",
    "getnetworkinfo",
    "getpeerinfo",
    "getmempoolinfo",
    "getblockheader",
    "getchaintips",
    "validateaddress",
    "z_validateaddress",
    "addnode",
    "ping",
    "stop",
    "getbestblockheightandhash",
    "getdeprecationinfo",
];

#[test]
fn the_query_methods_are_served_only_with_a_node_query() {
    let h = harness();
    let mut c = Client::connect(h.server.addr());
    let query_methods = [
        "getblockhash",
        "getblock",
        "getblockchaininfo",
        "z_gettreestate",
        "getrawmempool",
        "sendrawtransaction",
        "sendprivatetransaction",
    ];
    for method in query_methods.into_iter().chain(INFO_METHODS) {
        assert_eq!(
            c.call(method, json!([]))["error"]["code"],
            -32601,
            "{method}"
        );
    }

    let rpc = Rpc::with_parts(
        RpcConfig::new(Network::Regtest),
        h.feed.clone(),
        h.sink.clone(),
        Arc::new(Chain),
        None,
        Some(Arc::new(State::default())),
    );
    let server = HttpServer::serve("127.0.0.1:0", rpc, None).unwrap();
    let mut c = Client::connect(server.addr());
    let hash = |height: u8| BlockHash([height; 32]).to_string();

    assert_eq!(c.call("getblockhash", json!([7]))["result"], hash(7));
    assert_eq!(c.call("getblockhash", json!([100]))["error"]["code"], -8);
    // A block is named by its height, by its height as a string, or by its hash.
    for block in [json!(7), json!("7"), json!(hash(7))] {
        assert_eq!(c.call("getblock", json!([block, 0]))["result"], "07b1");
    }
    // The verbosity 1 is the default. It needs the place of the block in the chain, which
    // this node does not give. No other verbosity exists.
    assert_eq!(c.call("getblock", json!([7, 1]))["error"]["code"], -5);
    assert_eq!(c.call("getblock", json!([7]))["error"]["code"], -5);
    assert_eq!(c.call("getblock", json!([7, 2]))["error"]["code"], -8);

    let info = c.call("getblockchaininfo", json!([]))["result"].clone();
    assert_eq!(info["chain"], "regtest");
    assert_eq!(info["blocks"], 99);
    assert_eq!(info["bestblockhash"], hash(99));
    assert_eq!(
        info["valuePools"][0],
        json!({"id": "transparent", "chainValueZat": 1})
    );
    assert_eq!(
        info["valuePools"][5],
        json!({"id": "lockbox", "chainValueZat": 6})
    );

    let trees = c.call("z_gettreestate", json!([99]))["result"].clone();
    assert_eq!(trees["hash"], hash(99));
    assert_eq!(trees["height"], 99);
    assert_eq!(trees["time"], 1_700_000_000);
    // The Sapling root has the byte order of a block hash.
    let sapling = trees["sapling"]["commitments"]["finalRoot"]
        .as_str()
        .unwrap();
    assert!(
        sapling.ends_with("5a") && sapling.starts_with("00"),
        "{sapling}"
    );
    assert_eq!(
        trees["orchard"]["commitments"]["finalRoot"],
        "0c".repeat(32)
    );
    assert_eq!(
        trees["ironwood"]["commitments"]["finalRoot"],
        "1d".repeat(32)
    );
    // The node has the tree state of the tip only.
    assert_eq!(c.call("z_gettreestate", json!([98]))["error"]["code"], -8);

    let mempool = c.call("getrawmempool", json!([]))["result"].clone();
    assert_eq!(mempool, json!([format!("{}aa", "00".repeat(31))]));

    assert_eq!(
        c.call("sendrawtransaction", json!(["07"]))["result"],
        "07".repeat(32)
    );
    let refused = c.call("sendrawtransaction", json!(["0707"]));
    assert_eq!(refused["error"]["code"], -26);
    assert_eq!(
        refused["error"]["message"],
        "the policy refuses the transaction"
    );
    assert_eq!(
        c.call("sendrawtransaction", json!(["zz"]))["error"]["code"],
        -22
    );
}

/// The methods of the module `info` on the node of [`State`]: what the server makes of the
/// answers of the node, and which calls reach the node.
#[test]
fn the_info_methods_have_the_fields_of_zakura() {
    let h = harness();
    let serve = |network: Network| {
        let state = Arc::new(State::default());
        let rpc = Rpc::with_parts(
            RpcConfig::new(network),
            h.feed.clone(),
            h.sink.clone(),
            Arc::new(Chain),
            None,
            Some(state.clone()),
        );
        (HttpServer::serve("127.0.0.1:0", rpc, None).unwrap(), state)
    };
    let (server, state) = serve(Network::Regtest);
    let mut c = Client::connect(server.addr());
    let (tip_height, tip_hash) = Chain.tip();

    assert_eq!(
        c.call("getbestblockheightandhash", json!([]))["result"],
        json!({ "height": tip_height, "hash": tip_hash.0 })
    );
    assert_eq!(c.call("getdeprecationinfo", json!([]))["result"], json!({}));
    assert_eq!(
        c.call("getmempoolinfo", json!([]))["result"],
        json!({ "size": 1, "bytes": 250, "usage": 250 })
    );
    assert_eq!(
        c.call("getpeerinfo", json!([]))["result"],
        json!([
            { "addr": "127.0.0.1:18344", "subver": "/peer:1/", "version": 170_190,
              "inbound": true, "pingtime": 0.25 },
            { "addr": "[::1]:18344", "inbound": false, "pingwait": 1.5 },
        ])
    );
    assert_eq!(
        c.call("getchaintips", json!([]))["result"],
        json!([
            { "height": 99, "hash": BlockHash([99; 32]).to_string(), "branchlen": 0,
              "status": "active" },
            { "height": 98, "hash": BlockHash([0xf0; 32]).to_string(), "branchlen": 2,
              "status": "valid-fork" },
        ])
    );
    let network_info = c.call("getnetworkinfo", json!([]))["result"].clone();
    assert_eq!(
        network_info,
        json!({
            "version": 1_020_300,
            "subversion": "/mock:1.2.3/",
            "protocolversion": 170_160,
            "localservices": "0000000004000001",
            "timeoffset": 0,
            "connections": 2,
            "networks": [
                { "name": "ipv4", "limited": false, "reachable": true, "proxy": "",
                  "proxy_randomize_credentials": false },
                { "name": "ipv6", "limited": false, "reachable": true, "proxy": "",
                  "proxy_randomize_credentials": false },
                { "name": "onion", "limited": false, "reachable": false, "proxy": "",
                  "proxy_randomize_credentials": false },
            ],
            "relayfee": 1e-6,
            "localaddresses": [],
            "warnings": "",
        })
    );

    // The work of a Regtest block is 17, and the blocks of the node have 2 s each: 8
    // solutions for each second over each window. One block has no rate.
    for params in [
        json!([]),
        json!([120]),
        json!([10, 50]),
        json!([0, 99]),
        json!([5, -1]),
    ] {
        assert_eq!(
            c.call("getnetworksolps", params.clone())["result"],
            8,
            "{params}"
        );
        assert_eq!(c.call("getnetworkhashps", params)["result"], 8);
    }
    assert_eq!(c.call("getnetworksolps", json!([120, 0]))["result"], 0);
    // A parameter error has the code -1, as in zcashd and Zakura.
    assert_eq!(c.call("getnetworksolps", json!(["x"]))["error"]["code"], -1);

    // The template of the harness is on another block than the tip of the node, so the
    // difficulty is the difficulty of the bits of the tip block: the Regtest limit.
    assert_eq!(c.call("getdifficulty", json!([]))["result"], 1.0);
    let info = c.call("getinfo", json!([]))["result"].clone();
    assert_eq!(
        info,
        json!({
            "version": 1_020_300,
            "build": "v1.2.3",
            "subversion": "/mock:1.2.3/",
            "protocolversion": 170_160,
            "blocks": 99,
            "connections": 2,
            "difficulty": 1.0,
            "testnet": true,
            "paytxfee": 0.0,
            "relayfee": 1e-6,
        })
    );
    // The subsidy of Regtest, and the Mainnet streams of the first NU6 block and of the
    // first NU6.1 block (Zakura names the streams of NU6 only with the NU6 names).
    assert_eq!(
        c.call("getblocksubsidy", json!([]))["result"],
        json!({
            "miner": 6.25,
            "founders": 0.0,
            "fundingstreamstotal": 0.0,
            "lockboxtotal": 0.0,
            "totalblocksubsidy": 6.25,
        })
    );
    assert_eq!(c.call("getblocksubsidy", json!([-1]))["error"]["code"], -1);

    // `ping` reaches the node. `stop` and `addnode` do so on Regtest.
    assert_eq!(c.call("ping", json!([]))["result"], Value::Null);
    assert_eq!(c.call("ping", json!([]))["error"], Value::Null);
    assert_eq!(
        c.call("addnode", json!(["127.0.0.1:18344", "add"]))["result"],
        Value::Null
    );
    let again = c.call("addnode", json!(["127.0.0.1:18344", "add"]));
    assert_eq!(again["error"]["code"], -23, "{again}");
    for params in [
        json!(["127.0.0.1:18344", "remove"]),
        json!(["node.example", "add"]),
        json!(["127.0.0.1:18344"]),
    ] {
        assert_eq!(c.call("addnode", params)["error"]["code"], -1);
    }
    assert_eq!(
        c.call("sendprivatetransaction", json!(["07"]))["result"],
        "07".repeat(32)
    );
    assert_eq!(
        c.call("sendrawtransaction", json!(["07"]))["result"],
        "07".repeat(32)
    );
    assert_eq!(
        c.call("stop", json!([]))["result"],
        "hayaid server stopping"
    );
    assert_eq!(
        *state.calls.lock().unwrap(),
        [
            "ping",
            "ping",
            "addnode 127.0.0.1:18344",
            "addnode 127.0.0.1:18344",
            "send true",
            "send false",
            "stop"
        ]
    );

    // On another network `stop` and `addnode` do not reach the node.
    for network in [Network::Testnet, Network::Mainnet] {
        let (server, state) = serve(network);
        let mut c = Client::connect(server.addr());
        let stop = c.call("stop", json!([]));
        assert_eq!(stop["error"]["code"], -32601, "{stop}");
        assert_eq!(
            c.call("addnode", json!(["127.0.0.1:18344", "add"]))["error"]["code"],
            -1
        );
        assert_eq!(*state.calls.lock().unwrap(), Vec::<String>::new());
        // The difficulty of a network without a template on the tip comes from the bits
        // of the tip block: the Regtest bits of this node are above the limit of the
        // other networks.
        let difficulty = c.call("getdifficulty", json!([]))["result"].clone();
        assert!(
            matches!(difficulty.as_f64(), Some(d) if d > 0.0 && d < 1.0),
            "{difficulty}"
        );
        if network == Network::Mainnet {
            let nu6 = c.call("getblocksubsidy", json!([2_726_400]))["result"].clone();
            assert_eq!(
                nu6,
                json!({
                    "miner": 1.25,
                    "founders": 0.0,
                    "fundingstreamstotal": 0.125,
                    "lockboxtotal": 0.1875,
                    "totalblocksubsidy": 1.5625,
                    "fundingstreams": [{
                        "recipient": "Zcash Community Grants NU6",
                        "specification": "https://zips.z.cash/zip-1015",
                        "value": 0.125,
                        "valueZat": 12_500_000,
                        "address": "t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow",
                    }],
                    "lockboxstreams": [{
                        "recipient": "Lockbox NU6",
                        "specification": "https://zips.z.cash/zip-1015",
                        "value": 0.1875,
                        "valueZat": 18_750_000,
                    }],
                })
            );
            let nu6_1 = c.call("getblocksubsidy", json!([3_146_400]))["result"].clone();
            assert_eq!(nu6_1["fundingstreams"][0]["recipient"], "Major Grants");
            assert_eq!(
                nu6_1["lockboxstreams"][0]["specification"],
                "https://zips.z.cash/zip-0214"
            );
            // The founders' reward of the first block after the slow start.
            let early = c.call("getblocksubsidy", json!([20_000]))["result"].clone();
            assert_eq!(
                early,
                json!({
                    "miner": 10.0,
                    "founders": 2.5,
                    "fundingstreamstotal": 0.0,
                    "lockboxtotal": 0.0,
                    "totalblocksubsidy": 12.5,
                })
            );
        }
        let info = c.call("getmininginfo", json!([]))["result"].clone();
        assert_eq!(info["testnet"], network != Network::Mainnet);
        assert_eq!(
            info["chain"],
            if network == Network::Mainnet {
                "main"
            } else {
                "test"
            }
        );
    }
}

#[test]
fn metrics_endpoint_serves_the_exposition_over_http_get() {
    let registry = Registry::new();
    registry
        .gauge(
            "zcash_chain_verified_block_height",
            "Height of the best verified block.",
            &[],
        )
        .set(10.0);
    registry
        .counter("mining_template_rebuilt", "Templates rebuilt.", &[])
        .add(3);
    let server = MetricsServer::serve("127.0.0.1:0", registry.clone()).unwrap();
    let mut c = Client::connect(server.addr());
    let (status, body) = c.raw("GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(status, 200);
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("# TYPE zcash_chain_verified_block_height gauge\n"));
    assert!(text.contains("\nzcash_chain_verified_block_height 10\n"));
    assert!(text.contains("\nmining_template_rebuilt 3\n"));
    // Keep-alive: a second scrape on the same connection sees the new value.
    registry
        .gauge("zcash_chain_verified_block_height", "", &[])
        .set(11.0);
    let (status, body) = c.raw("GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(status, 200);
    assert!(String::from_utf8(body)
        .unwrap()
        .contains("\nzcash_chain_verified_block_height 11\n"));
    let (status, _) = c.raw("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(status, 404);
    let (status, _) = c.raw("POST /metrics HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(status, 405);
    server.shutdown();
}

#[test]
fn a_revert_wakes_long_polls_as_a_tip_event() {
    let h = harness();
    let full_id = h.feed.current().unwrap().id;
    let update = h
        .live
        .lock()
        .unwrap()
        .apply(SetEvent::Added(candidate(4, 70_000, vec![])))
        .unwrap()
        .expect("selection changed");
    let reverted = TemplateUpdate::Reverted {
        rejected: BlockHash([7; 32]),
        template: update.template().clone(),
    };
    h.feed.publish(&reverted);
    let start = Instant::now();
    let (template, _) = h
        .feed
        .wait_for_newer(full_id, Duration::from_secs(10), Duration::from_secs(10))
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(template.id, update.template().id);
}

// ----- cookie authentication -----

/// An empty directory in the temporary directory of the target directory.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("hayai-rpc-tests")
        .join(name);
    if let Err(e) = fs::remove_dir_all(&dir) {
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}");
    }
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A server with the cookie file in `dir` and a generator, which shows that a method ran.
fn cookie_server(h: &Harness, dir: &Path) -> (Arc<HttpServer>, Arc<Producer>) {
    let producer = Arc::new(Producer {
        asked: Mutex::new(Vec::new()),
    });
    let rpc = Rpc::with_generator(
        RpcConfig::new(Network::Regtest),
        h.feed.clone(),
        h.sink.clone(),
        Arc::new(Chain),
        producer.clone(),
    );
    let cookie = Cookie::create(dir).unwrap();
    let server = HttpServer::serve("127.0.0.1:0", rpc, Some(cookie)).unwrap();
    (server, producer)
}

/// `generate 2` on a new connection, with the given `Authorization` header.
fn generate_with(addr: SocketAddr, authorization: Option<&str>) -> (u16, Vec<u8>) {
    let body = r#"{"id": 1, "method": "generate", "params": [2]}"#;
    let authorization = match authorization {
        Some(value) => format!("Authorization: {value}\r\n"),
        None => String::new(),
    };
    Client::connect(addr).raw(&format!(
        "POST / HTTP/1.1\r\nHost: x\r\n{authorization}Content-Length: {}\r\n\r\n{body}",
        body.len()
    ))
}

#[test]
fn a_request_needs_the_credentials_of_the_cookie_file() {
    let h = harness();
    let dir = scratch("credentials");
    let (server, producer) = cookie_server(&h, &dir);
    let file = dir.join(COOKIE_FILE);

    // No header, and a secret of the correct length with one different character.
    let content = fs::read_to_string(&file).unwrap();
    let last = match content.ends_with('A') {
        true => 'B',
        false => 'A',
    };
    let wrong = format!("{}{last}", &content[..content.len() - 1]);
    fs::write(dir.join("wrong"), wrong).unwrap();
    let wrong = cookie::authorization(&dir.join("wrong")).unwrap();
    for authorization in [
        None,
        Some(wrong.as_str()),
        Some("Basic"),
        Some("Basic AAAA"),
    ] {
        let (status, body) = generate_with(server.addr(), authorization);
        assert_eq!(status, 401, "{authorization:?}");
        assert_eq!(body, b"");
    }
    assert_eq!(*producer.asked.lock().unwrap(), Vec::<u32>::new());

    let right = cookie::authorization(&file).unwrap();
    let (status, body) = generate_with(server.addr(), Some(&right));
    assert_eq!(status, 200);
    let answer: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(answer["result"].as_array().map(Vec::len), Some(2));
    assert_eq!(*producer.asked.lock().unwrap(), vec![2]);
    server.shutdown();
}

#[cfg(unix)]
#[test]
fn the_cookie_file_is_private_replaces_a_stale_file_and_ends_with_the_server() {
    use std::os::unix::fs::PermissionsExt;

    let h = harness();
    let dir = scratch("file");
    let file = dir.join(COOKIE_FILE);
    assert_eq!(file.file_name().unwrap(), ".cookie");
    // A file that a run without a clean shutdown left, with a mode that is too wide.
    fs::write(&file, "__cookie__:stale").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

    let (server, _) = cookie_server(&h, &dir);
    let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{mode:o}");
    let content = fs::read_to_string(&file).unwrap();
    let secret = content.strip_prefix("__cookie__:").expect("the user name");
    // 32 bytes in base64, as Zakura.
    assert_eq!(secret.len(), 44);
    assert!(secret.ends_with('='));
    assert!(secret
        .trim_end_matches('=')
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/'));
    // The directory has the cookie file only: no temporary file stays.
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);

    server.shutdown();
    assert!(!file.exists());

    // A cookie directory that the node cannot make is an error.
    fs::write(dir.join("plain"), "").unwrap();
    let Err(_) = Cookie::create(&dir.join("plain").join("sub")) else {
        panic!("a file is not a directory");
    };
}

#[test]
fn a_head_above_the_limit_is_refused() {
    let h = harness();
    let body = r#"{"id": 1, "method": "getblockcount", "params": []}"#;
    let request = |fill: usize| {
        format!(
            "POST / HTTP/1.1\r\nHost: x\r\nAuthorization: Basic {}\r\nContent-Length: {}\r\n\r\n{body}",
            "A".repeat(fill),
            body.len()
        )
    };
    let (status, _) = Client::connect(h.server.addr()).raw(&request(MAX_HEAD / 2));
    assert_eq!(status, 200);
    let (status, _) = Client::connect(h.server.addr()).raw(&request(MAX_HEAD));
    assert_eq!(status, 431);
    // The headers together have the bound: no single line is above it.
    let many = format!("X-Fill: {}\r\n", "a".repeat(1000)).repeat(17);
    let (status, _) = Client::connect(h.server.addr()).raw(&format!(
        "POST / HTTP/1.1\r\n{many}Content-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    assert_eq!(status, 431);
}
