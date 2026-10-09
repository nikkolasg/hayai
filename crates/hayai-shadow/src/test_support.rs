//! Test support of shadow mode: an in-process mock of the followed node's JSON-RPC, and
//! synthetic Regtest chains for it. The node tests of `hayai-node` use it under the feature
//! `test-support`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use bytes::Bytes;
use hayai_consensus::Network;
use hayai_crypto::incrementalmerkletree::frontier::CommitmentTree;
use hayai_crypto::orchard::tree::MerkleHashOrchard;
use hayai_crypto::zcash_primitives::merkle_tree::write_commitment_tree;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_template::{CoinbaseSpec, CoinbaseTx};
use hayai_trees::OrchardFrontier;
use hayai_wire::header::{BlockHash, BlockHeader, PowParams};
use hayai_wire::{merkle_root, RawBlock, RawTx};
use parking_lot::Mutex;
use serde_json::{json, Value};

/// The `nBits` of every Regtest block: the proof-of-work limit.
pub const REGTEST_POW_LIMIT_BITS: u32 = Network::Regtest.params().pow_limit_bits;

pub type Handler = dyn Fn(&str, &Value) -> Result<Value, (i64, String)> + Send + Sync;

/// A JSON-RPC 2.0 server on loopback that answers with `handler` and records each call.
pub struct MockRpc {
    pub addr: SocketAddr,
    calls: Arc<Mutex<Vec<String>>>,
}

impl MockRpc {
    pub fn serve(handler: Arc<Handler>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = calls.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let handler = handler.clone();
                let log = log.clone();
                thread::spawn(move || {
                    let mut writer = stream.try_clone().expect("clone");
                    let mut reader = BufReader::new(stream);
                    let mut len = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let line = line.trim_end();
                        if line.is_empty() {
                            break;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length: ")
                        {
                            len = v.parse().expect("length");
                        }
                    }
                    let mut body = vec![0; len];
                    reader.read_exact(&mut body).expect("body");
                    let request: Value = serde_json::from_slice(&body).expect("JSON");
                    let method = request["method"].as_str().expect("method").to_string();
                    log.lock().push(method.clone());
                    let response = match handler(&method, &request["params"]) {
                        Ok(result) => {
                            json!({"jsonrpc": "2.0", "id": request["id"], "result": result})
                        }
                        Err((code, message)) => json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "error": {"code": code, "message": message},
                        }),
                    };
                    let body = response.to_string();
                    let _ = write!(
                        writer,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        Self { addr, calls }
    }

    pub fn count(&self, method: &str) -> usize {
        self.calls.lock().iter().filter(|m| *m == method).count()
    }
}

pub fn scratch() -> tempfile::TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
    std::fs::create_dir_all(&base).expect("scratch base");
    tempfile::tempdir_in(base).expect("scratch dir")
}

/// The coinbase that the template builds for `height` on `network`, with no fees.
pub fn coinbase_on(network: Network, height: u32) -> CoinbaseTx {
    CoinbaseSpec {
        script_pubkey: vec![0x51],
        miner_data: Vec::new(),
        network,
    }
    .build(height, 0)
    .expect("coinbase")
}

/// The Regtest coinbase of `height`: one output with the block subsidy.
pub fn coinbase(height: u32) -> CoinbaseTx {
    coinbase_on(Network::Regtest, height)
}

/// The Regtest subsidy of the first blocks: the value of the one output of [`coinbase`].
pub const REGTEST_SUBSIDY: u64 = 625_000_000;

/// A Mainnet coinbase of the Canopy funding streams: four outputs (the miner, then the
/// three streams). The coins tests use it as a transaction with more than one output.
pub fn four_output_tx() -> CoinbaseTx {
    coinbase_on(Network::Mainnet, 2_000_000)
}

/// A Regtest block at `height` on `prev` with the coinbase bytes `coinbase`.
pub fn block_with(prev: BlockHash, height: u32, coinbase: &[u8]) -> RawBlock {
    let tx = RawTx::parse(Bytes::copy_from_slice(coinbase), BranchId::Nu5).expect("a coinbase");
    let header = BlockHeader {
        version: 4,
        prev_hash: prev,
        merkle_root: merkle_root(&[tx.txid]),
        block_commitments: [0; 32],
        time: 1_700_000_000 + height,
        bits: REGTEST_POW_LIMIT_BITS,
        nonce: [height as u8; 32],
        solution: vec![0; PowParams::REGTEST.solution_len()],
    };
    let mut bytes = header.serialize();
    bytes.push(1);
    bytes.extend_from_slice(coinbase);
    RawBlock::parse(Bytes::from(bytes), BranchId::Nu5).expect("block parses")
}

/// A Regtest block at `height` on `prev` whose coinbase pays the Regtest terms.
pub fn block(prev: BlockHash, height: u32) -> RawBlock {
    block_with(prev, height, &coinbase(height).bytes)
}

/// [`block`] whose coinbase pays one zatoshi more than the subsidy.
pub fn overpaying_block(prev: BlockHash, height: u32) -> RawBlock {
    let mut bytes = coinbase(height).bytes.to_vec();
    let value = REGTEST_SUBSIDY.to_le_bytes();
    let positions: Vec<usize> = bytes
        .windows(8)
        .enumerate()
        .filter(|(_, window)| *window == value)
        .map(|(at, _)| at)
        .collect();
    let [at] = positions[..] else {
        panic!("the output value is at one position, found {positions:?}");
    };
    bytes[at..at + 8].copy_from_slice(&(REGTEST_SUBSIDY + 1).to_le_bytes());
    block_with(prev, height, &bytes)
}

pub fn orchard_leaves(n: u8) -> Vec<MerkleHashOrchard> {
    (1..=n)
        .map(|i| {
            let mut bytes = [0u8; 32];
            bytes[0] = i;
            MerkleHashOrchard::from_bytes(&bytes).expect("canonical field element")
        })
        .collect()
}

pub fn final_state(frontier: &OrchardFrontier) -> String {
    let tree = CommitmentTree::from_frontier(frontier.frontier());
    let mut bytes = Vec::new();
    write_commitment_tree(&tree, &mut bytes).expect("serialize");
    hex::encode(bytes)
}

/// A `z_gettreestate` answer with empty trees.
pub fn empty_trees() -> Value {
    json!({"sapling": {"commitments": {}}, "orchard": {"commitments": {}}})
}

/// A synthetic upstream chain from a start block at height 0: `getblock`, `getblockhash`,
/// `getbestblockhash`, `z_gettreestate` (empty trees). The mock has no `getblocksubsidy`:
/// hayai computes the subsidy.
pub fn upstream_chain(blocks: Vec<RawBlock>, start: BlockHash) -> MockRpc {
    upstream_chain_with_ironwood(blocks, start, None)
}

/// [`upstream_chain`] whose blocks after the start block have the Ironwood tree state
/// `ironwood` (hex), when given.
pub fn upstream_chain_with_ironwood(
    blocks: Vec<RawBlock>,
    start: BlockHash,
    ironwood: Option<String>,
) -> MockRpc {
    let mut by_hash: HashMap<String, (u32, String)> = HashMap::new();
    let mut hashes = vec![start];
    for (i, b) in blocks.iter().enumerate() {
        by_hash.insert(b.hash().to_string(), (i as u32 + 1, hex::encode(&b.bytes)));
        hashes.push(b.hash());
    }
    let best = *hashes.last().expect("non-empty");
    MockRpc::serve(Arc::new(move |method: &str, params: &Value| match method {
        "getbestblockhash" => Ok(json!(best.to_string())),
        "getblockhash" => Ok(json!(
            hashes[params[0].as_u64().unwrap_or(0) as usize].to_string()
        )),
        "getblock" if params[1] == 0 => Ok(json!(by_hash[params[0].as_str().unwrap_or("")].1)),
        "getblock" => Ok(json!({
            "height": 0,
            "time": 1_700_000_000u32,
            "bits": format!("{REGTEST_POW_LIMIT_BITS:08x}"),
            "valuePools": [
                {"id": "transparent", "chainValueZat": 0},
                {"id": "sprout", "chainValueZat": 0},
                {"id": "sapling", "chainValueZat": 0},
                {"id": "orchard", "chainValueZat": 0},
            ],
        })),
        "z_gettreestate" => {
            let mut trees = json!({"sapling": {"commitments": {}}, "orchard": {"commitments": {}}});
            if let (Some(state), true) = (&ironwood, params[0] != start.to_string().as_str()) {
                trees["ironwood"] = json!({"commitments": {"finalState": state}});
            }
            Ok(trees)
        }
        _ => Err((-32601, "Method not found".into())),
    }))
}

pub fn chain_of(n: u32, start: BlockHash) -> Vec<RawBlock> {
    chain_from(1, n, start)
}

/// `n` Regtest blocks on `start`, the first one at height `first`.
pub fn chain_from(first: u32, n: u32, start: BlockHash) -> Vec<RawBlock> {
    let mut prev = start;
    (first..first + n)
        .map(|h| {
            let b = block(prev, h);
            prev = b.hash();
            b
        })
        .collect()
}
