//! Two hayaid nodes on Regtest over loopback: node A produces ten blocks through the
//! `generate` RPC, node B follows over the compact-relay extension, both reach the same
//! tip, the traces hold `commit_start` and `commit_finish` for every block, and A's
//! `/metrics` reports height 10.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hayai_net::PeerProtocol;
use hayaid::{Config, Node};
use serde_json::{json, Value};

fn scratch() -> tempfile::TempDir {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("hayaid-tests");
    fs::create_dir_all(&base).expect("scratch base");
    tempfile::tempdir_in(base).expect("scratch dir")
}

fn config(dir: &Path, name: &str, peers: &[SocketAddr], produce: bool) -> Config {
    config_with(dir, name, peers, produce, "memory")
}

fn config_with(
    dir: &Path,
    name: &str,
    peers: &[SocketAddr],
    produce: bool,
    backend: &str,
) -> Config {
    let peers: Vec<String> = peers.iter().map(|p| format!("\"{p}\"")).collect();
    let text = format!(
        r#"
[network]
network = "regtest"
listen_addr = "127.0.0.1:0"
peers = [{peers}]

[state]
data_dir = "{data}"
backend = "{backend}"
flush_interval_blocks = 4

[rpc]
listen_addr = "127.0.0.1:0"

[metrics]
listen_addr = "127.0.0.1:0"

[trace]
dir = "{trace}"
node = "{name}"

[mining]
miner_address = "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"
regtest_produce = {produce}
"#,
        peers = peers.join(", "),
        data = dir.join(format!("{name}-data")).display(),
        trace = dir.join(format!("{name}-trace")).display(),
    );
    Config::parse(&text).expect("test config")
}

fn http(addr: SocketAddr, request: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .expect("timeout");
    stream.write_all(request.as_bytes()).expect("write");
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("status line");
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status");
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).expect("header");
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some(v) = h.strip_prefix("Content-Length: ") {
            len = v.parse().expect("length");
        }
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).expect("body");
    (status, body)
}

fn rpc(addr: SocketAddr, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = format!(
        "POST / HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (status, body) = http(addr, &request);
    assert_eq!(status, 200);
    serde_json::from_slice(&body).expect("JSON-RPC response")
}

fn rows(dir: &Path, file: &str) -> Vec<Value> {
    fs::read_to_string(dir.join(file))
        .expect("trace table")
        .lines()
        .map(|l| serde_json::from_str(l).expect("JSON row"))
        .collect()
}

fn heights(rows: &[Value], event: &str, result: Option<&str>) -> Vec<u64> {
    let mut h: Vec<u64> = rows
        .iter()
        .filter(|r| r["event"] == event)
        .filter(|r| match result {
            Some(want) => r["result"] == want,
            None => true,
        })
        .map(|r| r["height"].as_u64().expect("height"))
        .collect();
    h.sort_unstable();
    h
}

#[test]
fn two_nodes_follow_ten_produced_blocks() {
    let dir = scratch();
    let a = Node::start(&config(dir.path(), "a", &[], true)).expect("node a");
    let a_p2p = a.p2p_addr.expect("a listens");
    let b = Node::start(&config(dir.path(), "b", &[a_p2p], false)).expect("node b");

    // B dials A at once; wait until both ends negotiated the compact-relay extension.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let compact = |n: &Node| {
            n.relay
                .peers()
                .iter()
                .any(|p| p.established && matches!(p.protocol, PeerProtocol::CompactRelay(_)))
        };
        if compact(&a) && compact(&b) {
            break;
        }
        assert!(Instant::now() < deadline, "no compact-relay session");
        std::thread::sleep(Duration::from_millis(20));
    }

    let rpc_a = a.rpc_addr.expect("a serves RPC");
    let generated = rpc(rpc_a, "generate", json!([10]));
    let hashes = generated["result"]
        .as_array()
        .unwrap_or_else(|| panic!("generate failed: {generated}"));
    assert_eq!(hashes.len(), 10);
    assert_eq!(rpc(rpc_a, "getblockcount", json!([]))["result"], 10);

    assert!(
        b.tip.wait_height(10, Duration::from_secs(60)),
        "node b stopped at {:?}",
        b.tip.tip()
    );
    assert_eq!(a.tip.tip(), b.tip.tip());
    assert_eq!(a.tip.tip().1.to_string(), hashes[9].as_str().expect("hash"));
    // A publishes each template as a candidate of its lane; B holds the one on the tip.
    let tip = a.tip.tip().1;
    let deadline = Instant::now() + Duration::from_secs(10);
    while b.relay.candidates_on(&tip, 4).is_empty() {
        assert!(Instant::now() < deadline, "no candidate of A on the tip");
        std::thread::sleep(Duration::from_millis(20));
    }
    let rpc_b = b.rpc_addr.expect("b serves RPC");
    assert_eq!(
        rpc(rpc_b, "getbestblockhash", json!([]))["result"],
        hashes[9]
    );
    let Value::Object(gbt) = rpc(rpc_b, "getblocktemplate", json!([]))["result"].clone() else {
        panic!("node b serves a template");
    };
    assert_eq!(gbt["height"], 11);

    let (status, body) = http(
        a.metrics_addr.expect("a serves metrics"),
        "GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, 200);
    let text = String::from_utf8(body).expect("utf-8");
    assert!(
        text.contains("\nzcash_chain_verified_block_height 10\n"),
        "{text}"
    );
    assert!(text.contains("\nstate_memory_best_committed_block_height 10\n"));
    assert!(text.contains("sync_block_verify_duration_seconds_count{result=\"success\"} 10\n"));
    assert!(text.contains("hayai_validate_stage_duration_seconds_count{stage=\"total\"} 10\n"));
    for name in [
        "\nprocess_resident_memory_bytes ",
        "\nprocess_cpu_seconds_total ",
        "\nhayai_coins_cache_entries ",
        "\nhayai_coins_cache_bytes ",
        "\nhayai_coins_store_coins ",
        "hayai_template_latency_seconds_count{template=\"full\"} 11\n",
        "hayai_template_latency_seconds_count{template=\"empty\"} 11\n",
    ] {
        assert!(text.contains(name), "{name} is missing from {text}");
    }
    assert!(
        text.contains(&format!(
            "\nhayai_build_info{{version=\"{}\",chain=\"regtest\",mode=\"full\",crypto_backend=\"{}\",coins_backend=\"memory\"}} 1\n",
            env!("CARGO_PKG_VERSION"),
            hayai_crypto::BACKEND
        )),
        "{text}"
    );
    // Regtest has a history tree from genesis: every commitment was checked.
    assert!(text.contains("\nhayai_block_commitments_unchecked_total 0\n"));

    a.shutdown().expect("a shuts down");
    b.shutdown().expect("b shuts down");
    // The memory backend writes a snapshot at a clean shutdown.
    for name in ["a", "b"] {
        let snapshot = dir.path().join(format!("{name}-data/coins/coins.snapshot"));
        assert!(snapshot.is_file(), "{}", snapshot.display());
    }

    let all: Vec<u64> = (1..=10).collect();
    for name in ["a", "b"] {
        let trace = dir.path().join(format!("{name}-trace"));
        let commits = rows(&trace, "commit_state.jsonl");
        assert_eq!(heights(&commits, "commit_start", None), all, "{name}");
        assert_eq!(
            heights(&commits, "commit_finish", Some("committed")),
            all,
            "{name}"
        );
        assert_eq!(heights(&commits, "block_validated", Some("valid")), all);
        for r in &commits {
            assert_eq!(r["node"], name);
            let Some(_) = r["unix_us"].as_u64() else {
                panic!("unix_us on every row");
            };
        }
        let templates = rows(&trace, "template.jsonl");
        // One empty and one full template per tip: genesis and the ten blocks.
        assert_eq!(heights(&templates, "template_full", None).len(), 11);
        assert_eq!(heights(&templates, "template_empty", None).len(), 11);
    }
    let received = rows(&dir.path().join("b-trace"), "block_sync.jsonl");
    let compact: Vec<u64> = {
        let mut h: Vec<u64> = received
            .iter()
            .filter(|r| r["event"] == "block_received" && r["source"] == "compact")
            .map(|r| r["height"].as_u64().expect("height"))
            .collect();
        h.sort_unstable();
        h
    };
    assert_eq!(compact, all);
    let local = rows(&dir.path().join("a-trace"), "block_sync.jsonl")
        .iter()
        .filter(|r| r["event"] == "block_received" && r["source"] == "local")
        .count();
    assert_eq!(local, 10);
}

/// Generates `n` blocks on node `a` and returns its tip.
fn generate(node: &Node, n: u64) -> (u32, hayai_wire::header::BlockHash) {
    let generated = rpc(node.rpc_addr.expect("RPC"), "generate", json!([n]));
    assert_eq!(
        generated["result"].as_array().map(Vec::len),
        Some(n as usize),
        "{generated}"
    );
    node.tip.tip()
}

/// A node stops cleanly, restarts and continues from the same tip; a node that is
/// abandoned without the final flush recovers by replaying the blocks after the best block
/// of the coins store. 1,030 blocks put the finalized base at height 30 (window of 1,000
/// layers, the finality depth), so both restarts replay the window.
#[test]
fn a_node_resumes_after_a_clean_stop_and_after_a_crash_on_both_backends() {
    for backend in ["rocksdb", "memory"] {
        let dir = scratch();
        let cfg = config_with(dir.path(), "a", &[], true, backend);
        let node = Node::start(&cfg).expect("first start");
        let tip = generate(&node, 1_030);
        assert_eq!(tip.0, 1_030);
        node.shutdown().expect("clean stop");

        let node = Node::start(&cfg).expect("restart");
        assert_eq!(node.tip.tip(), tip, "{backend}: clean restart");
        let tip = generate(&node, 5);
        assert_eq!(
            tip.0, 1_035,
            "{backend}: the restarted node produces blocks"
        );
        // The producer built on the restored history tree: the node accepts its blocks.
        assert_eq!(
            rpc(node.rpc_addr.expect("RPC"), "getblockcount", json!([]))["result"],
            1_035
        );
        let tip = generate(&node, 7);
        node.abandon().expect("crash");

        let node = Node::start(&cfg).expect("restart after the crash");
        assert_eq!(node.tip.tip(), tip, "{backend}: restart after a crash");
        let tip = generate(&node, 3);
        assert_eq!(tip.0, 1_045, "{backend}");
        node.shutdown().expect("clean stop");
    }
}

#[test]
fn a_data_dir_of_another_network_or_mode_and_foreign_files_are_refused() {
    let dir = scratch();
    let cfg = config_with(dir.path(), "a", &[], false, "memory");
    Node::start(&cfg)
        .expect("first start")
        .shutdown()
        .expect("stop");
    assert!(dir.path().join("a-data/state.log").is_file());
    // Mainnet full mode on the Regtest data_dir.
    let other = Config::parse(&format!(
        "[network]\nnetwork = \"mainnet\"\n[state]\ndata_dir = \"{}\"\n[mining]\nminer_script = \"51\"\n",
        dir.path().join("a-data").display()
    ))
    .expect("config");
    let Err(e) = Node::start(&other) else {
        panic!("a Mainnet node must not open a Regtest data_dir");
    };
    assert!(e.to_string().contains("regtest/full"), "{e}");

    // Files of something else in coins/ and no state log: refused, not removed.
    let foreign = dir.path().join("b-data");
    std::fs::create_dir_all(foreign.join("coins")).expect("dir");
    std::fs::write(foreign.join("coins/other"), b"x").expect("file");
    let cfg = config_with(dir.path(), "b", &[], false, "memory");
    let Err(e) = Node::start(&cfg) else {
        panic!("a foreign data_dir must be refused");
    };
    assert!(e.to_string().contains("is not empty"), "{e}");
    assert!(foreign.join("coins/other").is_file());
}

/// Mainnet in full mode starts at the Mainnet genesis block.
#[test]
fn a_mainnet_full_node_starts_at_the_mainnet_genesis_block() {
    let dir = scratch();
    let cfg = Config::parse(&format!(
        "[network]\nnetwork = \"mainnet\"\n[state]\ndata_dir = \"{}\"\n[mining]\nminer_address = \"t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs\"\n",
        dir.path().join("data").display()
    ))
    .expect("config");
    let node = Node::start(&cfg).expect("mainnet node");
    let (height, hash) = node.tip.tip();
    assert_eq!(height, 0);
    assert_eq!(
        hash.to_string(),
        "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08"
    );
    node.shutdown().expect("stop");
}

fn metrics_text(node: &Node) -> String {
    let (status, body) = http(
        node.metrics_addr.expect("metrics"),
        "GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status, 200);
    String::from_utf8(body).expect("utf-8")
}

/// An own block found after the driver prebuilt the template's body commits from the
/// prebuilt layer; the trace rows name the path.
#[test]
fn own_blocks_commit_from_the_prebuilt_template() {
    let dir = scratch();
    let a = Node::start(&config(dir.path(), "a", &[], true)).expect("node a");
    let rpc_a = a.rpc_addr.expect("a serves RPC");
    for height in 1..=3u64 {
        // Longer than PREBUILD_INTERVAL: the driver prebuilds the template meanwhile.
        std::thread::sleep(hayaid::PREBUILD_INTERVAL * 3);
        let generated = rpc(rpc_a, "generate", json!([1]));
        assert_eq!(
            generated["result"].as_array().map(Vec::len),
            Some(1),
            "{generated}"
        );
        assert_eq!(rpc(rpc_a, "getblockcount", json!([]))["result"], height);
    }
    let text = metrics_text(&a);
    assert!(
        text.contains("hayai_prebuilt_commits_total{origin=\"own\"} 3\n"),
        "{text}"
    );
    assert!(text.contains("hayai_prebuild_duration_seconds_count"));
    a.shutdown().expect("a shuts down");
    let commits = rows(&dir.path().join("a-trace"), "commit_state.jsonl");
    let swapped = commits
        .iter()
        .filter(|r| r["event"] == "commit_finish" && r["apply_class"] == "prebuilt_own")
        .count();
    assert_eq!(swapped, 3);
}

/// The query methods of the RPC server on a node with three blocks.
#[test]
fn the_query_methods_show_the_chain_the_state_and_the_mempool() {
    let dir = scratch();
    let a = Node::start(&config(dir.path(), "a", &[], true)).expect("node a");
    let rpc_a = a.rpc_addr.expect("a serves RPC");
    let (height, tip) = generate(&a, 3);
    assert_eq!(height, 3);
    let call = |method: &str, params: Value| rpc(rpc_a, method, params);

    assert_eq!(call("getblockhash", json!([3]))["result"], tip.to_string());
    assert_eq!(call("getblockhash", json!([4]))["error"]["code"], -8);
    let block = call("getblock", json!([3, 0]))["result"].clone();
    let bytes = hex::decode(block.as_str().expect("hex")).expect("hex");
    let header = hayai_wire::header::BlockHeader::parse(&bytes).expect("header");
    assert_eq!(header.hash(), tip);
    assert_eq!(
        call("getblock", json!([tip.to_string(), 0]))["result"],
        block
    );

    let info = call("getblockchaininfo", json!([]))["result"].clone();
    assert_eq!(info["chain"], "regtest");
    assert_eq!(info["blocks"], 3);
    assert_eq!(info["bestblockhash"], tip.to_string());
    // Three coinbases of 6.25 ZEC, all transparent.
    assert_eq!(
        info["valuePools"][0],
        json!({"id": "transparent", "chainValueZat": 1_875_000_000u64})
    );
    assert_eq!(
        info["valuePools"][3],
        json!({"id": "orchard", "chainValueZat": 0})
    );

    let trees = call("z_gettreestate", json!([3]))["result"].clone();
    assert_eq!(trees["hash"], tip.to_string());
    assert_eq!(trees["time"], header.time);
    // The roots of the empty Sapling and Orchard trees, as zcashd prints them.
    assert_eq!(
        trees["sapling"]["commitments"]["finalRoot"],
        "3e49b5f954aa9d3545bc6c37744661eea48d7c34e3000d82b7f0010c30f4c2fb"
    );
    assert_eq!(
        trees["orchard"]["commitments"]["finalRoot"],
        "ae2935f1dfd8a24aed7c70df7de3a668eb7a49b1319880dde2bbd9031ae5d82f"
    );
    assert_eq!(call("z_gettreestate", json!([2]))["error"]["code"], -8);

    assert_eq!(call("getrawmempool", json!([]))["result"], json!([]));
    let refused = call("sendrawtransaction", json!(["00"]));
    assert_eq!(refused["error"]["code"], -26, "{refused}");
    a.shutdown().expect("a shuts down");
}
