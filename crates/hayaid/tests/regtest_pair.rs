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
use hayai_node::{Config, Node};
use hayai_wire::header::BlockHeader;
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
network = "Regtest"
listen_addr = "127.0.0.1:0"
peers = [{peers}]

[state]
cache_dir = "{data}"
backend = "{backend}"
flush_interval_blocks = 4

[rpc]
listen_addr = "127.0.0.1:0"

[metrics]
endpoint_addr = "127.0.0.1:0"

[network.zakura]
trace_dir = "{trace}"

[trace]
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

/// The address of the RPC server of a node, and the credentials of its cookie file.
struct RpcClient {
    addr: SocketAddr,
    authorization: String,
}

fn client(node: &Node) -> RpcClient {
    let cookie = node.rpc_cookie.as_ref().expect("the node has a cookie");
    RpcClient {
        addr: node.rpc_addr.expect("the node serves RPC"),
        authorization: hayai_http::cookie::authorization(cookie).expect("cookie file"),
    }
}

fn rpc(client: &RpcClient, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = format!(
        "POST / HTTP/1.1\r\nHost: x\r\nAuthorization: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        client.authorization,
        body.len()
    );
    let (status, body) = http(client.addr, &request);
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

    let rpc_a = client(&a);
    let generated = rpc(&rpc_a, "generate", json!([10]));
    let hashes = generated["result"]
        .as_array()
        .unwrap_or_else(|| panic!("generate failed: {generated}"));
    assert_eq!(hashes.len(), 10);
    assert_eq!(rpc(&rpc_a, "getblockcount", json!([]))["result"], 10);

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
    let rpc_b = client(&b);
    assert_eq!(
        rpc(&rpc_b, "getbestblockhash", json!([]))["result"],
        hashes[9]
    );
    let Value::Object(gbt) = rpc(&rpc_b, "getblocktemplate", json!([]))["result"].clone() else {
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
    let generated = rpc(&client(node), "generate", json!([n]));
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
        // The base in memory is at block 30. The last flush of the coins (each 4 blocks)
        // was at block 1,028, with the base at block 28.
        let text = metrics_text(&node);
        assert!(text.contains("\nhayai_base_height 30\n"), "{backend}");
        assert!(
            text.contains("\nstate_finalized_block_height 28\n"),
            "{backend}"
        );
        // The clean stop flushes the coins of the base.
        let finalized = "\nstate_finalized_block_height 30\n";
        node.shutdown().expect("clean stop");

        let node = Node::start(&cfg).expect("restart");
        assert_eq!(node.tip.tip(), tip, "{backend}: clean restart");
        // The restart puts the base at the same block, and counts its own commits only.
        let text = metrics_text(&node);
        assert!(text.contains(finalized), "{backend}: {text}");
        assert!(text.contains("\nzcash_chain_verified_block_height 1030\n"));
        assert!(text.contains("\nzcash_chain_verified_block_total 0\n"));
        let tip = generate(&node, 5);
        assert_eq!(
            tip.0, 1_035,
            "{backend}: the restarted node produces blocks"
        );
        // The producer built on the restored history tree: the node accepts its blocks.
        assert_eq!(
            rpc(&client(&node), "getblockcount", json!([]))["result"],
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
        "[network]\nnetwork = \"Mainnet\"\n[state]\ncache_dir = \"{}\"\n[mining]\nminer_script = \"51\"\n",
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
        "[network]\nnetwork = \"Mainnet\"\n[state]\ncache_dir = \"{}\"\n[mining]\nminer_address = \"t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs\"\n",
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

/// The metrics with the names of Zakura (`docs/zakura-compat.md`, Metrics) on a chain of
/// 10 blocks: each name is in the output, with the value of the chain or of the requests.
#[test]
fn the_metrics_with_the_names_of_zakura_have_the_values_of_the_node() {
    let dir = scratch();
    let a = Node::start(&config(dir.path(), "a", &[], true)).expect("node a");
    let rpc_a = client(&a);
    generate(&a, 10);
    assert_eq!(rpc(&rpc_a, "getblockcount", json!([]))["result"], 10);
    assert_eq!(
        rpc(&rpc_a, "getblockhash", json!([11]))["error"]["code"],
        -8
    );
    assert_eq!(rpc(&rpc_a, "nomethod", json!([]))["error"]["code"], -32601);
    let text = metrics_text(&a);
    for line in [
        "zcash_chain_verified_block_height 10",
        "state_memory_best_committed_block_height 10",
        "state_finalized_block_height 0",
        "zcash_chain_verified_block_total 10",
        "sync_block_verify_duration_seconds_count{result=\"success\"} 10",
        "zcash_mempool_size_transactions 0",
        "zcash_mempool_size_bytes 0",
        "zcash_net_peers 0",
        "sync_downloads_in_flight 0",
        "rpc_requests_total{method=\"generate\",status=\"success\"} 1",
        "rpc_requests_total{method=\"getblockcount\",status=\"success\"} 1",
        "rpc_requests_total{method=\"getblockhash\",status=\"error\"} 1",
        "rpc_requests_total{method=\"unknown\",status=\"error\"} 1",
        "rpc_errors_total{method=\"getblockhash\",error_code=\"-8\"} 1",
        "rpc_errors_total{method=\"unknown\",error_code=\"-32601\"} 1",
        "rpc_request_duration_seconds_count{method=\"generate\"} 1",
        "rpc_active_requests 0",
    ] {
        assert!(text.contains(&format!("\n{line}\n")), "{line}\n{text}");
    }
    // The node reads these values from the process and from the template: the test
    // reads the name and the type only.
    for name in [
        "process_resident_memory_bytes gauge",
        "process_cpu_seconds_total counter",
        "hayai_template_updates_total counter",
    ] {
        assert!(text.contains(&format!("\n# TYPE {name}\n")), "{name}");
    }
    assert!(
        !text.contains("nomethod"),
        "an unknown method is not a label"
    );
    a.shutdown().expect("a shuts down");
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
    let rpc_a = client(&a);
    for height in 1..=3u64 {
        // Longer than PREBUILD_INTERVAL: the driver prebuilds the template meanwhile.
        std::thread::sleep(hayai_node::PREBUILD_INTERVAL * 3);
        let generated = rpc(&rpc_a, "generate", json!([1]));
        assert_eq!(
            generated["result"].as_array().map(Vec::len),
            Some(1),
            "{generated}"
        );
        assert_eq!(rpc(&rpc_a, "getblockcount", json!([]))["result"], height);
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
    let rpc_a = client(&a);
    let (height, tip) = generate(&a, 3);
    assert_eq!(height, 3);
    let call = |method: &str, params: Value| rpc(&rpc_a, method, params);

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

/// A node writes the cookie file to its data directory and serves the holder of its
/// content only. The clean stop removes the file. With `enable_cookie_auth = false` the
/// node writes no file and serves each client.
#[test]
fn the_rpc_server_needs_the_cookie_unless_the_config_turns_it_off() {
    let dir = scratch();
    let post = |addr: SocketAddr, authorization: &str| {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"generate","params":[1]}"#;
        http(
            addr,
            &format!(
                "POST / HTTP/1.1\r\nHost: x\r\n{authorization}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
        .0
    };

    let cfg = config(dir.path(), "a", &[], true);
    let node = Node::start(&cfg).expect("node a");
    let file = dir.path().join("a-data").join(".cookie");
    assert_eq!(node.rpc_cookie.as_deref(), Some(file.as_path()));
    let content = fs::read_to_string(&file).expect("cookie file");
    assert!(content.starts_with("__cookie__:"), "{content}");
    let addr = node.rpc_addr.expect("RPC");
    assert_eq!(post(addr, ""), 401);
    assert_eq!(node.tip.tip().0, 0, "no block without the credentials");
    let authorization = hayai_http::cookie::authorization(&file).expect("cookie file");
    assert_eq!(
        post(addr, &format!("Authorization: {authorization}\r\n")),
        200
    );
    assert_eq!(node.tip.tip().0, 1);
    node.shutdown().expect("clean stop");
    assert!(!file.exists(), "the clean stop removes the cookie file");

    // A second start has another secret: the old credentials do not open it.
    let node = Node::start(&cfg).expect("restart");
    assert_ne!(fs::read_to_string(&file).expect("cookie file"), content);
    let addr = node.rpc_addr.expect("RPC");
    assert_eq!(
        post(addr, &format!("Authorization: {authorization}\r\n")),
        401
    );
    node.shutdown().expect("clean stop");

    // The cookie directory of the configuration, and no authentication.
    let mut cfg = config(dir.path(), "b", &[], true);
    cfg.rpc.cookie_dir = Some(dir.path().join("cookies"));
    let node = Node::start(&cfg).expect("node b");
    let file = dir.path().join("cookies").join(".cookie");
    assert_eq!(node.rpc_cookie.as_deref(), Some(file.as_path()));
    assert!(file.exists());
    node.shutdown().expect("clean stop");

    let mut cfg = config(dir.path(), "c", &[], true);
    cfg.rpc.enable_cookie_auth = false;
    let node = Node::start(&cfg).expect("node c");
    assert_eq!(node.rpc_cookie, None);
    assert!(!dir.path().join("c-data").join(".cookie").exists());
    assert_eq!(post(node.rpc_addr.expect("RPC"), ""), 200);
    assert_eq!(node.tip.tip().0, 1);
    node.shutdown().expect("clean stop");

    // A cookie file that the node cannot write is a start error.
    let mut cfg = config(dir.path(), "d", &[], true);
    fs::write(dir.path().join("plain"), "").expect("a file");
    cfg.rpc.cookie_dir = Some(dir.path().join("plain"));
    let Err(e) = Node::start(&cfg) else {
        panic!("a node without its cookie file must not start");
    };
    assert!(e.to_string().contains("RPC cookie file"), "{e}");
}

// ----- the methods that pools and operators use -----

/// A producer with three blocks, and a caller of its RPC server.
fn rpc_node(dir: &Path) -> (Node, impl Fn(&str, Value) -> Value) {
    let a = Node::start(&config(dir, "a", &[], true)).expect("node a");
    let addr = client(&a);
    generate(&a, 3);
    (a, move |method: &str, params: Value| {
        rpc(&addr, method, params)
    })
}

/// The header of the block at `height`, from `getblock` with verbosity 0.
fn header_at(call: &impl Fn(&str, Value) -> Value, height: u32) -> (BlockHeader, Vec<u8>) {
    let block = call("getblock", json!([height, 0]))["result"].clone();
    let bytes = hex::decode(block.as_str().expect("hex")).expect("hex");
    (BlockHeader::parse(&bytes).expect("header"), bytes)
}

fn display(mut bytes: [u8; 32]) -> String {
    bytes.reverse();
    hex::encode(bytes)
}

/// The roots of the empty Sapling and Orchard trees, as zcashd and Zakura print them.
const EMPTY_SAPLING_ROOT: &str = "3e49b5f954aa9d3545bc6c37744661eea48d7c34e3000d82b7f0010c30f4c2fb";
const EMPTY_ORCHARD_ROOT: &str = "ae2935f1dfd8a24aed7c70df7de3a668eb7a49b1319880dde2bbd9031ae5d82f";

#[test]
fn getinfo_states_the_node_and_its_chain() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    assert_eq!(
        call("getinfo", json!([]))["result"],
        json!({
            "version": 10_000,
            "build": "v0.1.0",
            "subversion": "/hayai:0.1.0/",
            "protocolversion": a.relay.config().protocol_version,
            "blocks": 3,
            "connections": 0,
            "difficulty": 1.0,
            "testnet": true,
            "paytxfee": 0.0,
            "relayfee": 1e-6,
        })
    );
    a.shutdown().expect("a shuts down");
}

/// The solution rate that the blocks 0 to `tip` give: the work of the blocks above the
/// oldest one (17 for each Regtest block) over the time between the oldest and the newest
/// block time.
fn expected_rate(call: &impl Fn(&str, Value) -> Value, genesis_time: u32, tip: u32) -> u64 {
    let mut times = vec![genesis_time];
    times.extend((1..=tip).map(|height| header_at(call, height).0.time));
    let span = times.iter().max().unwrap() - times.iter().min().unwrap();
    match span {
        0 => 0,
        span => 17 * u64::from(tip) / u64::from(span),
    }
}

/// The time of the Regtest genesis block.
const GENESIS_TIME: u32 = 1_296_688_602;

#[test]
fn getmininginfo_has_the_tip_block_and_the_solution_rate() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    let (_, block) = header_at(&call, 3);
    let rate = expected_rate(&call, GENESIS_TIME, 3);
    assert_eq!(
        call("getmininginfo", json!([]))["result"],
        json!({
            "blocks": 3,
            "currentblocksize": block.len(),
            "currentblocktx": 0,
            "networksolps": rate,
            "networkhashps": rate,
            "chain": "test",
            "testnet": true,
        })
    );
    a.shutdown().expect("a shuts down");
}

#[test]
fn getblocksubsidy_has_the_regtest_subsidy() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    let subsidy = json!({
        "miner": 6.25,
        "founders": 0.0,
        "fundingstreamstotal": 0.0,
        "lockboxtotal": 0.0,
        "totalblocksubsidy": 6.25,
    });
    // Without a height: the tip. The block after the tip takes the value pools of the tip.
    for params in [json!([]), json!([1]), json!([4])] {
        assert_eq!(call("getblocksubsidy", params)["result"], subsidy);
    }
    // The second halving of Regtest: 144 blocks before Blossom at height 1, then 288.
    assert_eq!(
        call("getblocksubsidy", json!([288]))["result"]["miner"],
        3.125
    );
    assert_eq!(call("getblocksubsidy", json!(["x"]))["error"]["code"], -1);
    a.shutdown().expect("a shuts down");
}

#[test]
fn getnetworksolps_and_getnetworkhashps_follow_the_block_times() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    let whole = expected_rate(&call, GENESIS_TIME, 3);
    for method in ["getnetworksolps", "getnetworkhashps"] {
        // 120 blocks, the averaging window and 500 blocks all reach the genesis block.
        for params in [
            json!([]),
            json!([0]),
            json!([500]),
            json!([120, -1]),
            json!([120, 9]),
        ] {
            assert_eq!(call(method, params.clone())["result"], whole, "{params}");
        }
        // One block and its parent, and the genesis block alone.
        let times: Vec<u32> = (2..=3)
            .map(|height| header_at(&call, height).0.time)
            .collect();
        let one = match times[1].abs_diff(times[0]) {
            0 => 0,
            span => 17 / u64::from(span),
        };
        assert_eq!(call(method, json!([1]))["result"], one);
        assert_eq!(call(method, json!([1, 0]))["result"], 0);
    }
    a.shutdown().expect("a shuts down");
}

#[test]
fn getdifficulty_is_1_on_regtest() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    assert_eq!(call("getdifficulty", json!([]))["result"], 1.0);
    a.shutdown().expect("a shuts down");
}

#[test]
fn getnetworkinfo_states_the_p2p_values_of_the_node() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    let network = |name: &str, reachable: bool| {
        json!({
            "name": name,
            "limited": false,
            "reachable": reachable,
            "proxy": "",
            "proxy_randomize_credentials": false,
        })
    };
    assert_eq!(
        call("getnetworkinfo", json!([]))["result"],
        json!({
            "version": 10_000,
            "subversion": "/hayai:0.1.0/",
            "protocolversion": a.relay.config().protocol_version,
            // NODE_NETWORK and the bit of the compact relay (1 << 26).
            "localservices": "0000000004000001",
            "timeoffset": 0,
            "connections": 0,
            "networks": [network("ipv4", true), network("ipv6", true), network("onion", false)],
            "relayfee": 1e-6,
            "localaddresses": [],
            "warnings": "",
        })
    );
    a.shutdown().expect("a shuts down");
}

/// Waits until `condition` gives a value, at most 30 s.
fn wait_some<T>(what: &str, mut condition: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(value) = condition() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A caller of the RPC server of `node`.
fn caller(node: &Node) -> impl Fn(&str, Value) -> Value {
    let addr = client(node);
    move |method: &str, params: Value| rpc(&addr, method, params)
}

/// Two connected nodes: `b` dials `a`, which has three blocks.
fn connected_pair(dir: &Path) -> (Node, Node) {
    let (a, _) = rpc_node(dir);
    let b =
        Node::start(&config(dir, "b", &[a.p2p_addr.expect("a listens")], false)).expect("node b");
    wait_some("the session", || {
        let established = |node: &Node| node.relay.peers().iter().any(|p| p.established);
        (established(&a) && established(&b)).then_some(())
    });
    (a, b)
}

#[test]
fn getpeerinfo_lists_each_connected_peer() {
    let dir = scratch();
    let (a, b) = connected_pair(dir.path());
    let (call_a, call_b) = (caller(&a), caller(&b));
    assert_eq!(call_a("getinfo", json!([]))["result"]["connections"], 1);
    let version = a.relay.config().protocol_version;
    // The dialled address is the P2P address of the other node. The other node sees a
    // port that the system chose.
    let outbound = call_b("getpeerinfo", json!([]))["result"].clone();
    assert_eq!(
        outbound,
        json!([{
            "addr": a.p2p_addr.expect("a listens").to_string(),
            "subver": "/hayai:0.1.0/",
            "version": version,
            "inbound": false,
        }])
    );
    let inbound = call_a("getpeerinfo", json!([]))["result"].clone();
    assert_eq!(inbound.as_array().map(Vec::len), Some(1), "{inbound}");
    assert_eq!(inbound[0]["inbound"], true);
    assert_eq!(inbound[0]["subver"], "/hayai:0.1.0/");
    assert!(inbound[0]["addr"]
        .as_str()
        .expect("addr")
        .starts_with("127.0.0.1:"));
    for node in [a, b] {
        node.shutdown().expect("shuts down");
    }
}

#[test]
fn ping_measures_the_time_to_each_peer() {
    let dir = scratch();
    let (a, b) = connected_pair(dir.path());
    let call_a = caller(&a);
    let first = call_a("getpeerinfo", json!([]))["result"].clone();
    assert_eq!(first[0].get("pingtime"), None, "{first}");
    assert_eq!(
        call_a("ping", json!([])),
        json!({"jsonrpc": "2.0", "result": null, "id": 1})
    );
    let time = wait_some("the ping time", || {
        call_a("getpeerinfo", json!([]))["result"][0]["pingtime"].as_f64()
    });
    assert!((0.0..30.0).contains(&time), "{time}");
    for node in [a, b] {
        node.shutdown().expect("shuts down");
    }
}

#[test]
fn getmempoolinfo_counts_the_mempool() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    assert_eq!(
        call("getmempoolinfo", json!([]))["result"],
        json!({ "size": 0, "bytes": 0, "usage": 0 })
    );
    a.shutdown().expect("a shuts down");
}

#[test]
fn getblockheader_has_the_header_and_its_place_in_the_chain() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    let (header, _) = header_at(&call, 2);
    let hash = header.hash().to_string();
    let (next, _) = header_at(&call, 3);
    let mut nonce = header.nonce;
    nonce.reverse();
    let expected = json!({
        "hash": hash,
        "confirmations": 2,
        "height": 2,
        "version": 4,
        "merkleroot": display(header.merkle_root),
        "blockcommitments": display(header.block_commitments),
        "finalsaplingroot": EMPTY_SAPLING_ROOT,
        "time": header.time,
        "nonce": hex::encode(nonce),
        "solution": "00".repeat(36),
        "bits": "200f0f0f",
        "difficulty": 1.0,
        "previousblockhash": header.prev_hash.to_string(),
        "nextblockhash": next.hash().to_string(),
    });
    for params in [json!([2]), json!(["2"]), json!([hash]), json!([hash, true])] {
        assert_eq!(call("getblockheader", params)["result"], expected);
    }
    assert_eq!(
        call("getblockheader", json!([hash, false]))["result"],
        hex::encode(header.serialize())
    );
    // The tip has no next block.
    let tip = call("getblockheader", json!([3]))["result"].clone();
    assert_eq!(tip["confirmations"], 1);
    assert_eq!(tip.get("nextblockhash"), None);
    // A height above the tip, an unknown hash, and the genesis block, which the node does
    // not store.
    assert_eq!(call("getblockheader", json!([4]))["error"]["code"], -8);
    assert_eq!(
        call("getblockheader", json!(["11".repeat(32)]))["error"]["code"],
        -5
    );
    assert_eq!(call("getblockheader", json!([0]))["error"]["code"], -5);
    assert_eq!(call("getblockheader", json!([2, 1]))["error"]["code"], -1);
    a.shutdown().expect("a shuts down");
}

#[test]
fn getblock_with_verbosity_1_has_the_block_and_the_state_after_it() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    let (header, bytes) = header_at(&call, 2);
    let block = hayai_wire::RawBlock::parse(
        bytes.clone().into(),
        hayai_crypto::zcash_protocol::consensus::BranchId::Nu5,
    )
    .expect("a block");
    let mut expected = call("getblockheader", json!([2]))["result"].clone();
    let pool = |id: &str, zat: u64, delta: u64| {
        json!({
            "id": id,
            "chainValue": zat as f64 / 1e8,
            "chainValueZat": zat,
            "monitored": zat != 0,
            "valueDelta": delta as f64 / 1e8,
            "valueDeltaZat": delta,
        })
    };
    for (key, value) in [
        ("size", json!(bytes.len())),
        ("nTx", json!(1)),
        ("tx", json!([display(*block.txs[0].txid.as_ref())])),
        ("finalorchardroot", json!(EMPTY_ORCHARD_ROOT)),
        (
            "chainSupply",
            json!({ "chainValue": 12.5, "chainValueZat": 1_250_000_000u64, "monitored": true }),
        ),
        (
            "valuePools",
            json!([
                pool("transparent", 1_250_000_000, 625_000_000),
                pool("sprout", 0, 0),
                pool("sapling", 0, 0),
                pool("orchard", 0, 0),
                pool("lockbox", 0, 0),
                pool("ironwood", 0, 0),
            ]),
        ),
        // The trees are empty, and Regtest has no NU6.3 height here.
        ("trees", json!({})),
    ] {
        expected[key] = value;
    }
    let hash = header.hash().to_string();
    // The verbosity 1 is the default.
    for params in [json!([2]), json!([2, 1]), json!([hash, 1])] {
        assert_eq!(call("getblock", params)["result"], expected);
    }
    assert_eq!(call("getblock", json!([2, 2]))["error"]["code"], -8);
    assert_eq!(call("getblock", json!([4, 1]))["error"]["code"], -8);
    assert_eq!(
        call("getblock", json!(["11".repeat(32), 1]))["error"]["code"],
        -5
    );
    a.shutdown().expect("a shuts down");
}

/// A node with one chain has one tip. A node that left a branch of two blocks for a chain
/// with more work lists that branch as a valid fork.
#[test]
fn getchaintips_lists_the_active_tip_and_a_left_branch() {
    let dir = scratch();
    let (a, call_a) = rpc_node(dir.path());
    let (height, tip) = a.tip.tip();
    let active = json!({
        "height": height,
        "hash": tip.to_string(),
        "branchlen": 0,
        "status": "active",
    });
    assert_eq!(call_a("getchaintips", json!([]))["result"], json!([active]));

    // Another coinbase script than the script of the first node: the blocks of the two
    // nodes differ when they have the same time.
    let mut other = config(dir.path(), "b", &[], true);
    other.mining.miner_address = None;
    other.mining.miner_script = Some("51".into());
    let b = Node::start(&other).expect("node b");
    let (_, left) = generate(&b, 2);
    b.relay
        .connect(a.p2p_addr.expect("a listens"))
        .expect("b dials a");
    assert!(b.tip.wait_height(3, Duration::from_secs(60)));
    assert_eq!(b.tip.tip(), (height, tip));
    let addr = client(&b);
    assert_eq!(
        rpc(&addr, "getchaintips", json!([]))["result"],
        json!([
            active,
            { "height": 2, "hash": left.to_string(), "branchlen": 2, "status": "valid-fork" },
        ])
    );
    for node in [a, b] {
        node.shutdown().expect("shuts down");
    }
}

#[test]
fn validateaddress_takes_the_transparent_addresses_of_the_network() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    let p2pkh = "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV";
    let p2sh = "t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUnh";
    assert_eq!(
        call("validateaddress", json!([p2pkh]))["result"],
        json!({ "isvalid": true, "address": p2pkh, "isscript": false })
    );
    assert_eq!(
        call("validateaddress", json!([p2sh]))["result"],
        json!({ "isvalid": true, "address": p2sh, "isscript": true })
    );
    // A Mainnet address, a Sapling address of Regtest, and text that is no address.
    for address in [
        "t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs",
        "zregtestsapling1f0x0t0dgnpyt0au5wl06g7ylnzwanhs5pegkpuvnnly6l4aeactlwdp2479ne6h8zvupqtx8hxt",
        "x",
    ] {
        assert_eq!(
            call("validateaddress", json!([address]))["result"],
            json!({ "isvalid": false }),
            "{address}"
        );
    }
    assert_eq!(call("validateaddress", json!([]))["error"]["code"], -1);
    a.shutdown().expect("a shuts down");
}

#[test]
fn z_validateaddress_takes_each_address_kind_of_the_network() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    for (address, kind) in [
        ("tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV", "p2pkh"),
        ("t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUnh", "p2sh"),
        (
            "zregtestsapling1f0x0t0dgnpyt0au5wl06g7ylnzwanhs5pegkpuvnnly6l4aeactlwdp2479ne6h8zvupqtx8hxt",
            "sapling",
        ),
        (
            "uregtest14re0eqzj6guwjy8fc2hvdjswh5pqvnty480846uervx9jdqn6cmq784ene8nu9spkl22mghk8nc6d6jsy3j452vdz90nak8yzyqj4j9q",
            "unified",
        ),
    ] {
        assert_eq!(
            call("z_validateaddress", json!([address]))["result"],
            json!({ "isvalid": true, "address": address, "address_type": kind, "ismine": false }),
            "{address}"
        );
    }
    // A Mainnet address, a Sapling address of Testnet, and text that is no address.
    for address in [
        "t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs",
        "ztestsapling1f0x0t0dgnpyt0au5wl06g7ylnzwanhs5pegkpuvnnly6l4aeactlwdp2479ne6h8zvupq5zw6hv",
        "x",
    ] {
        assert_eq!(
            call("z_validateaddress", json!([address]))["result"],
            json!({ "isvalid": false }),
            "{address}"
        );
    }
    a.shutdown().expect("a shuts down");
}

#[test]
fn addnode_puts_an_address_into_the_address_book() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    let addr: SocketAddr = "127.0.0.1:1".parse().expect("an address");
    let None = a.relay.peer_manager().book().get(&addr).map(|_| ()) else {
        panic!("the book is empty at the start");
    };
    let added = call("addnode", json!([addr.to_string(), "add"]));
    assert_eq!(added, json!({"jsonrpc": "2.0", "result": null, "id": 1}));
    let Some(()) = a.relay.peer_manager().book().get(&addr).map(|_| ()) else {
        panic!("the book does not have the address");
    };
    let again = call("addnode", json!([addr.to_string(), "add"]));
    assert_eq!(again["error"]["code"], -23, "{again}");
    assert_eq!(
        call("addnode", json!([addr.to_string(), "remove"]))["error"]["code"],
        -1
    );
    a.shutdown().expect("a shuts down");
}

#[test]
fn getbestblockheightandhash_has_the_tip() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    let (height, hash) = a.tip.tip();
    // Zakura prints this hash as the list of its bytes, in the order of the wire.
    assert_eq!(
        call("getbestblockheightandhash", json!([]))["result"],
        json!({ "height": height, "hash": hash.0 })
    );
    a.shutdown().expect("a shuts down");
}

#[test]
fn getdeprecationinfo_has_no_end_of_service() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    assert_eq!(call("getdeprecationinfo", json!([]))["result"], json!({}));
    a.shutdown().expect("a shuts down");
}

/// `stop` asks the owner of the node to stop it. The methods that need an index of the
/// transactions or a change of the state are unknown methods.
#[test]
fn stop_asks_for_the_shutdown_and_unserved_methods_are_unknown() {
    let dir = scratch();
    let (a, call) = rpc_node(dir.path());
    for method in [
        "getrawtransaction",
        "gettxout",
        "getaddressbalance",
        "getaddresstxids",
        "getaddressutxos",
        "z_getsubtreesbyindex",
        "z_listunifiedreceivers",
        "invalidateblock",
        "reconsiderblock",
        "nosuchmethod",
    ] {
        let answer = call(method, json!([]));
        assert_eq!(answer["error"]["code"], -32601, "{method}: {answer}");
    }
    let Err(_) = a.stop_requested().try_recv() else {
        panic!("a stop request before the call");
    };
    assert_eq!(call("stop", json!([]))["result"], "hayaid server stopping");
    a.stop_requested()
        .recv_timeout(Duration::from_secs(10))
        .expect("the stop request");
    a.shutdown().expect("a shuts down");
}
