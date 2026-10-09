//! The shadow node against an in-process mock of the followed node's JSON-RPC
//! (`hayai_shadow::test_support`): the validation of the upstream chain, the trust of a
//! short seed, the error rows, the resume from the data directory.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;

use hayai_shadow::test_support::*;
use hayai_trees::OrchardFrontier;
use hayai_wire::header::BlockHash;
use serde_json::{json, Value};

fn shadow_config(dir: &std::path::Path, rpc: SocketAddr) -> crate::Config {
    crate::Config::parse(&format!(
        r#"
[network]
network = "Regtest"
mode = "shadow"
[state]
cache_dir = "{data}"
[network.zakura]
trace_dir = "{trace}"

[trace]
node = "shadow"
[mining]
miner_script = "51"
[shadow]
rpc_addr = "{rpc}"
start_height = 0
poll_interval_ms = 20
"#,
        data = dir.join("data").display(),
        trace = dir.join("trace").display(),
    ))
    .expect("shadow config")
}

fn trace_rows(dir: &std::path::Path, file: &str) -> Vec<Value> {
    std::fs::read_to_string(dir.join("trace").join(file))
        .expect("trace")
        .lines()
        .map(|l| serde_json::from_str(l).expect("row"))
        .collect()
}

#[test]
fn shadow_node_validates_the_upstream_chain_and_records_agreement() {
    let start = BlockHash([0x5a; 32]);
    let mock = upstream_chain(chain_of(5, start), start);
    let dir = scratch();
    let node = crate::Node::start(&shadow_config(dir.path(), mock.addr)).expect("shadow node");
    assert!(
        node.tip.wait_height(5, std::time::Duration::from_secs(30)),
        "shadow node at {:?}",
        node.tip.tip()
    );
    node.shutdown().expect("shutdown");
    let commits = trace_rows(dir.path(), "commit_state.jsonl");
    let verdicts: Vec<(u64, bool)> = commits
        .iter()
        .filter(|r| r["event"] == "upstream_verdict")
        .map(|r| (r["height"].as_u64().expect("height"), r["agree"] == true))
        .collect();
    assert_eq!(verdicts, (1..=5).map(|h| (h, true)).collect::<Vec<_>>());
    let unchecked = commits
        .iter()
        .filter(|r| r["event"] == "block_validated" && r["block_commitments_checked"] == false)
        .count();
    assert_eq!(unchecked, 5);
    let empty = commits
        .iter()
        .filter(|r| r["event"] == "block_validated" && r["class"] == "empty")
        .count();
    assert_eq!(empty, 5);
    let received = trace_rows(dir.path(), "block_sync.jsonl")
        .iter()
        .filter(|r| r["event"] == "block_received" && r["source"] == "upstream_rpc")
        .count();
    assert_eq!(received, 5);
    // hayai computes the subsidy and the coinbase terms of each block itself.
    assert_eq!(mock.count("getblocksubsidy"), 0);
}

/// The value of the counter `name` in the `/metrics` text of `node`.
fn counter(node: &crate::Node, name: &str) -> u64 {
    let addr = node.metrics_addr.expect("the node serves metrics");
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    stream
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .expect("request");
    let mut text = String::new();
    stream.read_to_string(&mut text).expect("response");
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(' '))
        .unwrap_or_else(|| panic!("{name} is in the metrics:\n{text}"));
    value.parse().expect("a counter value")
}

/// A shadow start above the genesis block. The seed holds the start block and the 112
/// blocks before it with their times and `nBits`, so the header rules of the first block
/// after the start have their whole context: `hayai_shadow_trusted_bits_total` reads 0.
/// It still reads 0 after a restart, because the state record holds the same context.
#[test]
fn a_shadow_start_above_genesis_trusts_no_header() {
    let start_height = 140u32;
    let ancestor = |height: u32| {
        let mut hash = [0xb7; 32];
        hash[..4].copy_from_slice(&height.to_le_bytes());
        BlockHash(hash)
    };
    let start = ancestor(start_height);
    let blocks = chain_from(start_height + 1, 6, start);
    let heights: HashMap<String, u32> = (0..=start_height)
        .map(|h| (ancestor(h).to_string(), h))
        .collect();
    let bytes: HashMap<String, String> = blocks
        .iter()
        .map(|b| (b.hash().to_string(), hex::encode(&b.bytes)))
        .collect();
    let mock_for = |tip: usize| {
        let (heights, bytes) = (heights.clone(), bytes.clone());
        let best = blocks[tip - 1].hash();
        MockRpc::serve(Arc::new(move |method: &str, params: &Value| match method {
            "getbestblockhash" => Ok(json!(best.to_string())),
            "getblockhash" => Ok(json!(
                ancestor(params[0].as_u64().unwrap_or(0) as u32).to_string()
            )),
            "getblock" if params[1] == 0 => Ok(json!(bytes[params[0].as_str().unwrap_or("")])),
            "getblock" => {
                let height = heights[params[0].as_str().unwrap_or("")];
                let mut info = json!({
                    "height": height,
                    "time": 1_700_000_000 + height,
                    "bits": format!("{REGTEST_POW_LIMIT_BITS:08x}"),
                    "valuePools": [
                        {"id": "transparent", "chainValueZat": u64::from(height) * REGTEST_SUBSIDY},
                        {"id": "sprout", "chainValueZat": 0},
                        {"id": "sapling", "chainValueZat": 0},
                        {"id": "orchard", "chainValueZat": 0},
                    ],
                });
                if height > 0 {
                    info["previousblockhash"] = json!(ancestor(height - 1).to_string());
                }
                Ok(info)
            }
            "z_gettreestate" => Ok(empty_trees()),
            _ => Err((-32601, "Method not found".into())),
        }))
    };
    let wait = std::time::Duration::from_secs(30);
    let dir = scratch();
    let config = |rpc: SocketAddr| {
        let mut config = shadow_config(dir.path(), rpc);
        let Some(shadow) = &mut config.shadow else {
            panic!("a shadow configuration");
        };
        shadow.start_height = Some(start_height);
        config.metrics.endpoint_addr = Some("127.0.0.1:0".parse().expect("an address"));
        config
    };
    let mock = mock_for(3);
    let node = crate::Node::start(&config(mock.addr)).expect("shadow node");
    assert!(
        node.tip.wait_height(start_height + 3, wait),
        "at {:?}",
        node.tip.tip()
    );
    // The seed read the start block and the 112 blocks before it.
    assert_eq!(mock.count("getblockhash"), 1);
    assert_eq!(mock.count("getblock"), 113 + 3);
    assert_eq!(counter(&node, "hayai_shadow_trusted_bits_total"), 0);
    assert_eq!(counter(&node, "hayai_shadow_agreements_total"), 3);
    node.shutdown().expect("clean stop");

    // A restart reads no seed. The index and the base have the context of the record.
    let longer = mock_for(6);
    let node = crate::Node::start(&config(longer.addr)).expect("restart");
    assert!(
        node.tip.wait_height(start_height + 6, wait),
        "at {:?}",
        node.tip.tip()
    );
    assert_eq!(longer.count("getblockhash"), 0, "a restart reads no seed");
    assert_eq!(counter(&node, "hayai_shadow_trusted_bits_total"), 0);
    node.shutdown().expect("clean stop");
}

#[test]
fn a_block_hayai_rejects_is_an_error_row_and_stops_the_node() {
    let start = BlockHash([0x6b; 32]);
    // The coinbase of block 1 pays one zatoshi more than the Regtest subsidy.
    let first = overpaying_block(start, 1);
    let second = block(first.hash(), 2);
    let mock = upstream_chain(vec![first, second], start);
    let dir = scratch();
    let node = crate::Node::start(&shadow_config(dir.path(), mock.addr)).expect("shadow node");
    let Ok(Err(reason)) = node.done().recv_timeout(std::time::Duration::from_secs(30)) else {
        panic!("the node did not stop");
    };
    assert!(reason.contains("disagreement"), "{reason}");
    // `done()` delivered the driver's error; the shutdown still closes the traces.
    node.shutdown().expect("shutdown after the reported error");
    let errors: Vec<Value> = trace_rows(dir.path(), "commit_state.jsonl")
        .into_iter()
        .filter(|r| r["event"] == "upstream_verdict")
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["level"], "error");
    assert_eq!(errors[0]["agree"], false);
    assert_eq!(errors[0]["height"], 1);
    let reason = errors[0]["reason"].as_str().expect("reason");
    assert!(
        reason.contains("coinbase pays 625000001 zatoshis, more than the limit of 625000000"),
        "{reason}"
    );
}

/// The Ironwood root of each block is compared with the root of upstream, as the Sapling
/// and Orchard roots are. Upstream reports an Ironwood tree with two commitments after the
/// first block, and the block has no Ironwood output.
#[test]
fn an_ironwood_root_that_differs_from_upstream_is_a_disagreement() {
    let start = BlockHash([0x7c; 32]);
    let mut tree = OrchardFrontier::empty();
    tree.append_many(&orchard_leaves(2)).expect("append");
    let mock = upstream_chain_with_ironwood(chain_of(2, start), start, Some(final_state(&tree)));
    let dir = scratch();
    let node = crate::Node::start(&shadow_config(dir.path(), mock.addr)).expect("shadow node");
    let Ok(Err(reason)) = node.done().recv_timeout(std::time::Duration::from_secs(30)) else {
        panic!("the node did not stop");
    };
    assert!(reason.contains("disagreement"), "{reason}");
    node.shutdown().expect("shutdown after the reported error");
    let errors: Vec<Value> = trace_rows(dir.path(), "commit_state.jsonl")
        .into_iter()
        .filter(|r| r["event"] == "upstream_verdict")
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["agree"], false);
    assert_eq!(errors[0]["height"], 1);
    let reason = errors[0]["reason"].as_str().expect("reason");
    assert!(reason.contains("tree roots differ"), "{reason}");
    assert!(reason.contains("ironwood"), "{reason}");
}

#[test]
fn a_failed_seed_leaves_the_data_dir_as_it_was() {
    let mock = MockRpc::serve(Arc::new(|_: &str, _: &Value| {
        Err((-28, "starting up".into()))
    }));
    let dir = scratch();
    let cfg = shadow_config(dir.path(), mock.addr);
    let Err(e) = crate::Node::start(&cfg) else {
        panic!("a seed from a node that is not ready must fail");
    };
    assert!(e.to_string().contains("shadow seed"), "{e}");
    assert!(
        !dir.path().join("data").exists(),
        "a failed first start leaves no data_dir"
    );
    // A data_dir that existed and was empty stays empty, and the next start works.
    std::fs::create_dir(dir.path().join("data")).expect("data_dir");
    let Err(_) = crate::Node::start(&cfg) else {
        panic!("a seed from a node that is not ready must fail");
    };
    assert_eq!(
        std::fs::read_dir(dir.path().join("data"))
            .expect("data_dir")
            .count(),
        0
    );
    let start = BlockHash([0x3c; 32]);
    let ready = upstream_chain(chain_of(2, start), start);
    let node = crate::Node::start(&shadow_config(dir.path(), ready.addr)).expect("second start");
    assert!(node.tip.wait_height(2, std::time::Duration::from_secs(30)));
    node.shutdown().expect("shutdown");
}

#[test]
fn a_shadow_node_resumes_from_its_data_dir_after_a_clean_stop_and_a_crash() {
    let start = BlockHash([0x4d; 32]);
    let wait = std::time::Duration::from_secs(30);
    let mock8 = upstream_chain(chain_of(8, start), start);
    let dir = scratch();
    let node = crate::Node::start(&shadow_config(dir.path(), mock8.addr)).expect("first start");
    assert!(node.tip.wait_height(8, wait), "at {:?}", node.tip.tip());
    let tip8 = node.tip.tip();
    node.shutdown().expect("clean stop");
    assert_eq!(
        mock8.count("getblockhash"),
        1,
        "the seed reads the start block once"
    );

    // A clean restart replays the blocks from the block files: the tip is back before the
    // follower runs, and no seed is read.
    let node = crate::Node::start(&shadow_config(dir.path(), mock8.addr)).expect("restart");
    assert_eq!(node.tip.tip(), tip8);
    node.abandon().expect("crash");
    assert_eq!(mock8.count("getblockhash"), 1, "a restart reads no seed");

    // After a crash the node resumes as well, and follows a longer upstream chain.
    let mock10 = upstream_chain(chain_of(10, start), start);
    let node = crate::Node::start(&shadow_config(dir.path(), mock10.addr)).expect("after crash");
    assert_eq!(node.tip.tip(), tip8);
    assert!(node.tip.wait_height(10, wait), "at {:?}", node.tip.tip());
    assert_eq!(mock10.count("getblockhash"), 0, "a restart reads no seed");
    node.shutdown().expect("clean stop");
}
