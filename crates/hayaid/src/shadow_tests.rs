//! Shadow mode against an in-process mock of the followed node's JSON-RPC: the client,
//! the seed, the upstream-backed coins and the follower.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use bytes::Bytes;
use crossbeam_channel::unbounded;
use hayai_coins::{Coin, CoinsBacking, OutPoint, Pool, RocksBacking};
use hayai_crypto::incrementalmerkletree::frontier::CommitmentTree;
use hayai_crypto::orchard::tree::MerkleHashOrchard;
use hayai_crypto::zcash_primitives::merkle_tree::write_commitment_tree;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_rpc::Registry;
use hayai_template::{CoinbaseSpec, CoinbaseTx};
use hayai_trees::OrchardFrontier;
use hayai_wire::header::{BlockHash, BlockHeader, PowParams};
use hayai_wire::{merkle_root, RawBlock, RawTx};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::backing::{SpentLog, UpstreamBacking};
use crate::headers::{HeaderIndex, SeedBlock};
use crate::metrics::NodeMetrics;
use crate::node::Event;
use crate::params::{NetParams, NetworkKind, REGTEST_POW_LIMIT_BITS};
use crate::shadow::{self, Follower};
use crate::upstream::{Upstream, UpstreamError};
use hayai_state::ValuePools;

type Handler = dyn Fn(&str, &Value) -> Result<Value, (i64, String)> + Send + Sync;

/// A JSON-RPC 2.0 server on loopback that answers with `handler` and records each call.
struct MockRpc {
    addr: SocketAddr,
    calls: Arc<Mutex<Vec<String>>>,
}

impl MockRpc {
    fn serve(handler: Arc<Handler>) -> Self {
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

    fn count(&self, method: &str) -> usize {
        self.calls.lock().iter().filter(|m| *m == method).count()
    }
}

fn scratch() -> tempfile::TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
    std::fs::create_dir_all(&base).expect("scratch base");
    tempfile::tempdir_in(base).expect("scratch dir")
}

/// The coinbase that the template builds for `height` on `network`, with no fees.
fn coinbase_on(network: NetworkKind, height: u32) -> CoinbaseTx {
    CoinbaseSpec {
        script_pubkey: vec![0x51],
        miner_data: Vec::new(),
        network,
    }
    .build(height, 0)
    .expect("coinbase")
}

/// The Regtest coinbase of `height`: one output with the block subsidy.
fn coinbase(height: u32) -> CoinbaseTx {
    coinbase_on(NetworkKind::Regtest, height)
}

/// The Regtest subsidy of the first blocks: the value of the one output of [`coinbase`].
const REGTEST_SUBSIDY: u64 = 625_000_000;

/// A Mainnet coinbase of the Canopy funding streams: four outputs (the miner, then the
/// three streams). The coins tests use it as a transaction with more than one output.
fn four_output_tx() -> CoinbaseTx {
    coinbase_on(NetworkKind::Mainnet, 2_000_000)
}

/// A Regtest block at `height` on `prev` with the coinbase bytes `coinbase`.
fn block_with(prev: BlockHash, height: u32, coinbase: &[u8]) -> RawBlock {
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
fn block(prev: BlockHash, height: u32) -> RawBlock {
    block_with(prev, height, &coinbase(height).bytes)
}

/// [`block`] whose coinbase pays one zatoshi more than the subsidy.
fn overpaying_block(prev: BlockHash, height: u32) -> RawBlock {
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

fn orchard_leaves(n: u8) -> Vec<MerkleHashOrchard> {
    (1..=n)
        .map(|i| {
            let mut bytes = [0u8; 32];
            bytes[0] = i;
            MerkleHashOrchard::from_bytes(&bytes).expect("canonical field element")
        })
        .collect()
}

fn final_state(frontier: &OrchardFrontier) -> String {
    let tree = CommitmentTree::from_frontier(frontier.frontier());
    let mut bytes = Vec::new();
    write_commitment_tree(&tree, &mut bytes).expect("serialize");
    hex::encode(bytes)
}

#[test]
fn client_reads_transactions_and_errors() {
    let cb = coinbase(5);
    let tx_hex = hex::encode(&cb.bytes);
    let known = cb.txid.to_string();
    let mock = MockRpc::serve(Arc::new(move |method: &str, params: &Value| match method {
        "getrawtransaction" if params[0] == known.as_str() => {
            Ok(json!({"hex": tx_hex, "height": 5, "confirmations": 3}))
        }
        "getrawtransaction" => Err((-5, "No such mempool or blockchain transaction".into())),
        "getblockcount" => Ok(json!(42)),
        _ => Err((-32601, "Method not found".into())),
    }));
    let upstream = Upstream::new(mock.addr);
    let Ok(Some((bytes, Some(5)))) = upstream.transaction(&cb.txid) else {
        panic!("a mined transaction with its height");
    };
    assert_eq!(bytes, cb.bytes);
    let other = coinbase(6).txid;
    let Ok(None) = upstream.transaction(&other) else {
        panic!("an unknown transaction is None");
    };
    assert_eq!(upstream.block_count().expect("count"), 42);
    let Err(UpstreamError::Rpc { code: -32601, .. }) = upstream.best_block_hash() else {
        panic!("a JSON-RPC error is an Rpc error");
    };
}

#[test]
fn upstream_coins_fill_misses_below_the_start_height_only() {
    let old = four_output_tx();
    let new = coinbase(120);
    let txs: HashMap<String, (String, u32)> = [
        (old.txid.to_string(), (hex::encode(&old.bytes), 90)),
        (new.txid.to_string(), (hex::encode(&new.bytes), 120)),
    ]
    .into_iter()
    .collect();
    let mock = MockRpc::serve(Arc::new(move |method: &str, params: &Value| {
        match (method, txs.get(params[0].as_str().unwrap_or(""))) {
            ("getrawtransaction", Some((hex_tx, height))) => {
                Ok(json!({"hex": hex_tx, "height": height}))
            }
            ("getrawtransaction", None) => Err((-5, "unknown".into())),
            _ => Err((-32601, "Method not found".into())),
        }
    }));
    let dir = scratch();
    let inner: Arc<dyn CoinsBacking> =
        Arc::new(RocksBacking::open(dir.path(), &hayai_coins::Config::default()).expect("rocks"));
    let metrics = Arc::new(NodeMetrics::new(&Registry::new()));
    let backing = UpstreamBacking::new(
        inner.clone(),
        Arc::new(Upstream::new(mock.addr)),
        100,
        BranchId::Nu5,
        metrics.clone(),
        SpentLog::open(&dir.path().join("spent.log"), None).expect("spent log"),
    );
    let own = OutPoint::new([7; 32], 0);
    let own_coin = Coin {
        value: 5,
        script_pubkey: Bytes::from_static(&[0x51]),
        height: 101,
        is_coinbase: false,
    };
    inner
        .write_batch(&[(&own, &own_coin)], &[])
        .expect("inner write");
    let outpoints = [
        OutPoint::new(*old.txid.as_ref(), 0),
        OutPoint::new(*old.txid.as_ref(), 2),
        OutPoint::new(*old.txid.as_ref(), 4),
        OutPoint::new(*new.txid.as_ref(), 0),
        OutPoint::new([9; 32], 0),
        own.clone(),
    ];
    let got = backing.get_many(&outpoints).expect("read");
    let Some(first) = &got[0] else {
        panic!("a coin created before the start comes from upstream");
    };
    // The miner output and the Zcash Foundation stream of Mainnet block 2,000,000.
    assert_eq!(first.value, 250_000_000);
    assert_eq!(first.script_pubkey.as_ref(), &[0x51]);
    assert_eq!(first.height, 90);
    assert!(first.is_coinbase);
    let Some(third) = &got[1] else {
        panic!("output 2 exists");
    };
    assert_eq!(third.value, 15_625_000);
    assert_eq!(third.script_pubkey.len(), 23);
    let (None, None) = (&got[2], &got[3]) else {
        panic!("no output 4; a transaction after the start is hayai's own history");
    };
    let None = &got[4] else {
        panic!("an unknown transaction has no coin");
    };
    assert_eq!(got[5], Some(own_coin));
    assert_eq!(metrics.trusted_coins.get(), 2);
    // One request per distinct transaction; the inner hit costs none.
    assert_eq!(mock.count("getrawtransaction"), 3);

    // A coin hayai spent is gone for good, without asking upstream.
    let spent = OutPoint::new(*old.txid.as_ref(), 0);
    backing.write_batch(&[], &[&spent]).expect("spend");
    let got = backing
        .get_many(std::slice::from_ref(&spent))
        .expect("read");
    let None = &got[0] else {
        panic!("a spent coin reads as absent");
    };
    assert_eq!(mock.count("getrawtransaction"), 3);

    // Nullifiers the inner store lacks rest on upstream and are counted.
    backing
        .insert_many(Pool::Orchard, &[[1; 32]])
        .expect("insert");
    let present = backing
        .contains_many(Pool::Orchard, &[[1; 32], [2; 32], [3; 32]])
        .expect("contains");
    assert_eq!(present, vec![true, false, false]);
    assert_eq!(metrics.trusted_nullifiers.get(), 2);
}

/// A restart keeps the outpoints that earlier generations spent, so a coin created before the
/// start height that hayai spent is not fetched from upstream again. The records above the
/// best block of the inner store are dropped.
#[test]
fn spent_outpoints_survive_a_restart_up_to_the_best_block() {
    let dir = scratch();
    let path = dir.path().join("spent.log");
    let inner: Arc<dyn CoinsBacking> = Arc::new(
        RocksBacking::open(&dir.path().join("coins"), &hayai_coins::Config::default())
            .expect("rocks"),
    );
    let mock = MockRpc::serve(Arc::new(|_: &str, _: &Value| Err((-5, "unknown".into()))));
    let metrics = Arc::new(NodeMetrics::new(&Registry::new()));
    let open = |best: Option<u32>| {
        UpstreamBacking::new(
            inner.clone(),
            Arc::new(Upstream::new(mock.addr)),
            100,
            BranchId::Nu5,
            metrics.clone(),
            SpentLog::open(&path, best).expect("spent log"),
        )
    };
    let generation = |height: u32, spends: Vec<OutPoint>| hayai_coins::FlushGeneration {
        adds: Vec::new(),
        spends,
        nullifiers: Default::default(),
        best_block: hayai_coins::BestBlock {
            height,
            hash: [height as u8; 32],
        },
    };
    let (a, b) = (OutPoint::new([1; 32], 0), OutPoint::new([2; 32], 1));
    let backing = open(None);
    inner
        .write_batch(&[], &[])
        .expect("the inner store is open");
    backing
        .write_generation(&generation(110, vec![a.clone()]))
        .expect("generation 110");
    backing
        .write_generation(&generation(120, vec![b.clone()]))
        .expect("generation 120");
    drop(backing);
    // A spent outpoint reads as absent without a request; any other one is asked upstream.
    let spent = |backing: &UpstreamBacking, outpoint: &OutPoint| {
        let before = mock.count("getrawtransaction");
        backing
            .get_many(std::slice::from_ref(outpoint))
            .expect("read");
        mock.count("getrawtransaction") == before
    };
    let both = open(Some(120));
    assert!(spent(&both, &a) && spent(&both, &b));
    drop(both);
    // The inner store reached 110 only: the record of 120 is dropped.
    let first = open(Some(110));
    assert!(spent(&first, &a) && !spent(&first, &b));
    drop(first);
    // The log was cut: a later open at 120 does not see the dropped record again.
    let again = open(Some(120));
    assert!(spent(&again, &a) && !spent(&again, &b));
    drop(again);
    // No generation reached the inner store.
    let none = open(None);
    assert!(!spent(&none, &a) && !spent(&none, &b));
}

/// An upstream chain for the seed: `count` blocks, the first one at height `first`. The
/// block at position `i` has the hash `[i + 1; 32]`, the time `1_000 + 10 * i` and the
/// `nBits` `0x1f00_0000 + i`.
struct SeedChain {
    first: u32,
    count: u8,
    /// The `valuePools` of every block: `(id, chainValueZat)`.
    pools: Vec<(&'static str, u64)>,
    /// The block at this height has no `previousblockhash`.
    cut: Option<u32>,
    /// The `getblock` answers have no `bits`.
    without_bits: bool,
    /// The `z_gettreestate` answer.
    trees: Value,
}

impl SeedChain {
    fn regtest(count: u8, trees: Value) -> Self {
        Self {
            first: 0,
            count,
            pools: vec![
                ("transparent", 1),
                ("sprout", 2),
                ("sapling", 222),
                ("orchard", 333),
                ("ironwood", 444),
            ],
            cut: None,
            without_bits: false,
            trees,
        }
    }

    fn hash(position: u8) -> BlockHash {
        BlockHash([position + 1; 32])
    }

    fn block(position: u8) -> SeedBlock {
        SeedBlock {
            hash: Self::hash(position),
            time: 1_000 + 10 * u32::from(position),
            bits: Some(0x1f00_0000 + u32::from(position)),
        }
    }

    fn serve(self) -> MockRpc {
        let position_of: HashMap<String, u8> = (0..self.count)
            .map(|i| (Self::hash(i).to_string(), i))
            .collect();
        let Self {
            first,
            pools,
            cut,
            without_bits,
            trees,
            ..
        } = self;
        MockRpc::serve(Arc::new(move |method: &str, params: &Value| match method {
            "getblockhash" => {
                let height = params[0].as_u64().unwrap_or(0) as u32;
                Ok(json!(Self::hash((height - first) as u8).to_string()))
            }
            "getblock" => {
                let i = position_of[params[0].as_str().unwrap_or("")];
                let height = first + u32::from(i);
                let block = Self::block(i);
                let pools: Vec<Value> = pools
                    .iter()
                    .map(|(id, value)| json!({"id": id, "chainValueZat": value}))
                    .collect();
                let mut info = json!({
                    "height": height,
                    "time": block.time,
                    "valuePools": pools,
                });
                if let (Some(bits), false) = (block.bits, without_bits) {
                    info["bits"] = json!(format!("{bits:08x}"));
                }
                if i > 0 && cut != Some(height) {
                    info["previousblockhash"] = json!(Self::hash(i - 1).to_string());
                }
                Ok(info)
            }
            "z_gettreestate" => Ok(trees.clone()),
            _ => Err((-32601, "Method not found".into())),
        }))
    }
}

/// A `z_gettreestate` answer with empty trees.
fn empty_trees() -> Value {
    json!({"sapling": {"commitments": {}}, "orchard": {"commitments": {}}})
}

#[test]
fn seed_reads_the_start_state_and_parses_the_tree_state() {
    let mut orchard = OrchardFrontier::empty();
    orchard.append_many(&orchard_leaves(5)).expect("append");
    let state = final_state(&orchard);
    let mut ironwood = OrchardFrontier::empty();
    ironwood.append_many(&orchard_leaves(3)).expect("append");
    let ironwood_state = final_state(&ironwood);
    let trees = json!({
        "sapling": {"commitments": {}},
        "orchard": {"commitments": {"finalState": state}},
        "ironwood": {"commitments": {"finalState": ironwood_state}},
    });
    let mock = SeedChain::regtest(141, trees).serve();
    let params = NetParams::new(NetworkKind::Regtest);
    let seed = shadow::seed(&Upstream::new(mock.addr), params, Some(135)).expect("seed");
    assert_eq!(seed.height, 135);
    assert_eq!(seed.hash, SeedChain::hash(135));
    // The start block and the 112 blocks before it, each with its time and its bits: the
    // whole context of the header rules of block 136.
    assert_eq!(seed.ancestors.len(), 113);
    let expected: Vec<SeedBlock> = (23..=135).map(SeedChain::block).collect();
    assert_eq!(seed.ancestors, expected);
    assert_eq!(seed.ancestors[112].time, 2_350);
    assert_eq!(seed.ancestors[112].bits, Some(0x1f00_0087));
    assert_eq!(mock.count("getblock"), 113);
    // Every value pool that upstream reports. Regtest has no NU6: the deferred pool that
    // upstream does not report is zero.
    assert_eq!(
        seed.value_pools,
        ValuePools {
            transparent: 1,
            sprout: 2,
            sapling: 222,
            orchard: 333,
            ironwood: 444,
            deferred: 0,
        }
    );
    assert_eq!(seed.ironwood.frontier(), ironwood.frontier());
    assert_ne!(seed.ironwood.root(), orchard.root());
    assert_eq!(seed.orchard.root(), orchard.root());
    assert_eq!(seed.orchard.frontier(), orchard.frontier());
    assert_eq!(
        seed.sapling.root(),
        hayai_trees::SaplingFrontier::empty().root()
    );
    // Near the genesis block the chain has fewer than 113 blocks: the seed holds all of
    // them.
    let seed = shadow::seed(&Upstream::new(mock.addr), params, Some(3)).expect("seed");
    let expected: Vec<SeedBlock> = (0..=3).map(SeedChain::block).collect();
    assert_eq!(seed.ancestors, expected);
}

/// An upstream node that does not give the 113 blocks of the header context, or their
/// `nBits`, gives no seed: the node does not start with a rule that cannot run.
#[test]
fn a_seed_without_the_whole_header_context_fails() {
    let params = NetParams::new(NetworkKind::Regtest);
    let cut = SeedChain {
        cut: Some(130),
        ..SeedChain::regtest(141, empty_trees())
    }
    .serve();
    let Err(message) = shadow::seed(&Upstream::new(cut.addr), params, Some(135)).map(|_| ()) else {
        panic!("six blocks are not the context of block 136");
    };
    assert!(message.contains("6 of the 113 blocks"), "{message}");
    // The same chain gives a seed at a height whose 113 blocks it holds.
    let seed = shadow::seed(&Upstream::new(cut.addr), params, Some(129)).expect("seed at 129");
    assert_eq!(seed.ancestors.len(), 113);

    let without_bits = SeedChain {
        without_bits: true,
        ..SeedChain::regtest(41, empty_trees())
    }
    .serve();
    let Err(message) =
        shadow::seed(&Upstream::new(without_bits.addr), params, Some(35)).map(|_| ())
    else {
        panic!("a block without bits gives no seed");
    };
    assert!(message.contains("no bits"), "{message}");
}

/// The value pools of the seed. A pool that upstream does not report is zero only when no
/// block up to the start height can change it. From NU6 the deferred pool is required and
/// is not zero: hayai checks the lockbox terms against it.
#[test]
fn the_seed_requires_every_value_pool_that_a_rule_reads() {
    let params = NetParams::new(NetworkKind::Mainnet);
    let nu6 = 2_726_400;
    let first = nu6 - 120;
    let base = vec![
        ("transparent", 10),
        ("sprout", 20),
        ("sapling", 30),
        ("orchard", 40),
    ];
    let seed_at = |pools: Vec<(&'static str, u64)>, height: u32| {
        let mock = SeedChain {
            first,
            count: 130,
            pools,
            ..SeedChain::regtest(0, empty_trees())
        }
        .serve();
        shadow::seed(&Upstream::new(mock.addr), params, Some(height)).map(|seed| seed.value_pools)
    };
    let with = |extra: &[(&'static str, u64)]| {
        let mut pools = base.clone();
        pools.extend_from_slice(extra);
        pools
    };
    // The last block before NU6: no block added to the deferred pool yet.
    assert_eq!(
        seed_at(base.clone(), nu6 - 1),
        Ok(ValuePools {
            transparent: 10,
            sprout: 20,
            sapling: 30,
            orchard: 40,
            ironwood: 0,
            deferred: 0,
        })
    );
    // From NU6 the deferred pool is required, and it is not zero.
    for pools in [base.clone(), with(&[("lockbox", 0)])] {
        for height in [nu6, nu6 + 5] {
            let Err(message) = seed_at(pools.clone(), height) else {
                panic!("a seed at {height} without the deferred pool");
            };
            assert!(message.contains("deferred value pool"), "{message}");
            assert!(message.contains("lockbox"), "{message}");
        }
    }
    let seeded = seed_at(with(&[("lockbox", 18_750_000)]), nu6).expect("the deferred pool");
    assert_eq!(seeded.deferred, 18_750_000);
    assert_eq!((seeded.transparent, seeded.sprout), (10, 20));
    // The transparent, Sprout, Sapling and Orchard pools are required at every height.
    for missing in ["transparent", "sprout", "sapling", "orchard"] {
        let pools: Vec<_> = with(&[("lockbox", 5)])
            .into_iter()
            .filter(|(id, _)| *id != missing)
            .collect();
        let Err(message) = seed_at(pools, nu6 - 1) else {
            panic!("a seed without the {missing} pool");
        };
        assert!(
            message.contains(&format!("no {missing} value pool")),
            "{message}"
        );
    }
}

/// The Ironwood tree of upstream. Before NU6.3 an answer without it is the empty tree. From
/// NU6.3 an answer without it is an error: hayai does not replace the tree of upstream
/// with the empty tree.
#[test]
fn the_ironwood_tree_state_is_required_from_nu6_3() {
    use crate::upstream::TreeState;
    let mut tree = OrchardFrontier::empty();
    tree.append_many(&orchard_leaves(2)).expect("append");
    let bytes = hex::decode(final_state(&tree)).expect("hex");
    let without = TreeState::default();
    let before = shadow::frontiers(&without, false).expect("before NU6.3");
    assert_eq!(before.ironwood, OrchardFrontier::empty());
    assert_eq!(before.orchard, OrchardFrontier::empty());
    let Err(message) = shadow::frontiers(&without, true).map(|_| ()) else {
        panic!("no Ironwood tree state from NU6.3");
    };
    assert!(message.contains("Ironwood"), "{message}");
    let with = TreeState {
        ironwood: Some(bytes),
        ..TreeState::default()
    };
    for active in [false, true] {
        let trees = shadow::frontiers(&with, active).expect("an Ironwood tree state");
        assert_eq!(trees.ironwood.frontier(), tree.frontier());
        assert_eq!(trees.orchard, OrchardFrontier::empty());
    }
    // The rule set of the height gives the flag: Regtest has no NU6.3, Mainnet has it from
    // height 3,428,143.
    let active = |kind, height| {
        NetParams::new(kind)
            .rules_at(height)
            .expect("a rule set")
            .pools
            .ironwood
    };
    assert!(!active(NetworkKind::Regtest, 1_000_000));
    assert!(!active(NetworkKind::Mainnet, 3_428_142));
    assert!(active(NetworkKind::Mainnet, 3_428_143));
    assert!(active(NetworkKind::Testnet, 4_134_000));
}

#[test]
fn follower_reports_new_blocks_and_reorgs_from_the_fork_point() {
    let start = 3_000_000;
    let genesis = BlockHash([0xaa; 32]);
    let b1 = block(genesis, start + 1);
    let b2 = block(b1.hash(), start + 2);
    let b2_alt = block(b1.hash(), start + 2 + 1000);
    let blocks: HashMap<String, String> = [&b1, &b2, &b2_alt]
        .iter()
        .map(|b| (b.hash().to_string(), hex::encode(&b.bytes)))
        .collect();
    let best = Arc::new(Mutex::new(b2.hash()));
    let best_read = best.clone();
    let mut orchard = OrchardFrontier::empty();
    orchard.append_many(&orchard_leaves(3)).expect("append");
    let state = final_state(&orchard);
    let mut ironwood = OrchardFrontier::empty();
    ironwood.append_many(&orchard_leaves(2)).expect("append");
    let ironwood_state = final_state(&ironwood);
    let mock = MockRpc::serve(Arc::new(move |method: &str, params: &Value| match method {
        "getbestblockhash" => Ok(json!(best_read.lock().to_string())),
        "getblock" => Ok(json!(blocks[params[0].as_str().unwrap_or("")])),
        "z_gettreestate" => Ok(json!({
            "sapling": {"commitments": {}},
            "orchard": {"commitments": {"finalState": state}},
            "ironwood": {"commitments": {"finalState": ironwood_state}},
        })),
        _ => Err((-32601, "Method not found".into())),
    }));
    let index = Arc::new(HeaderIndex::new(
        start,
        &[SeedBlock {
            hash: genesis,
            time: 1_600_000_000,
            bits: None,
        }],
    ));
    let (tx, rx) = unbounded();
    let mut follower = Follower::new(
        Arc::new(Upstream::new(mock.addr)),
        NetParams::new(NetworkKind::Testnet),
        index,
        tx,
    );
    assert!(follower.step().expect("step"));
    let Ok(Event::Upstream { fork, blocks }) = rx.try_recv() else {
        panic!("a report");
    };
    assert_eq!(fork, genesis);
    let got: Vec<(BlockHash, u32)> = blocks.iter().map(|b| (b.raw.hash(), b.height)).collect();
    assert_eq!(got, vec![(b1.hash(), start + 1), (b2.hash(), start + 2)]);
    assert_eq!(blocks[0].orchard_root, orchard.root().to_bytes());
    assert_eq!(blocks[0].ironwood_root, ironwood.root().to_bytes());
    assert_ne!(blocks[0].ironwood_root, blocks[0].orchard_root);
    // The same tip is not reported twice.
    assert!(follower.step().expect("step"));
    let Err(_) = rx.try_recv() else {
        panic!("no report for an unchanged tip");
    };
    // An upstream reorg reports from the last common block of the earlier report.
    *best.lock() = b2_alt.hash();
    assert!(follower.step().expect("step"));
    let Ok(Event::Upstream { fork, blocks }) = rx.try_recv() else {
        panic!("a reorg report");
    };
    assert_eq!(fork, b1.hash());
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].raw.hash(), b2_alt.hash());
    assert_eq!(blocks[0].height, start + 2);
    assert_eq!(mock.count("getblock"), 3);
}

/// A synthetic upstream chain from a start block at height 0: `getblock`, `getblockhash`,
/// `getbestblockhash`, `z_gettreestate` (empty trees). The mock has no `getblocksubsidy`:
/// hayai computes the subsidy.
fn upstream_chain(blocks: Vec<RawBlock>, start: BlockHash) -> MockRpc {
    upstream_chain_with_ironwood(blocks, start, None)
}

/// [`upstream_chain`] whose blocks after the start block have the Ironwood tree state
/// `ironwood` (hex), when given.
fn upstream_chain_with_ironwood(
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

fn chain_of(n: u32, start: BlockHash) -> Vec<RawBlock> {
    chain_from(1, n, start)
}

/// `n` Regtest blocks on `start`, the first one at height `first`.
fn chain_from(first: u32, n: u32, start: BlockHash) -> Vec<RawBlock> {
    let mut prev = start;
    (first..first + n)
        .map(|h| {
            let b = block(prev, h);
            prev = b.hash();
            b
        })
        .collect()
}

fn shadow_config(dir: &std::path::Path, rpc: SocketAddr) -> crate::Config {
    crate::Config::parse(&format!(
        r#"
[network]
network = "regtest"
mode = "shadow"
[state]
data_dir = "{data}"
[trace]
dir = "{trace}"
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
        config.metrics.listen_addr = Some("127.0.0.1:0".parse().expect("an address"));
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
    assert_eq!(
        mock.calls
            .lock()
            .iter()
            .filter(|m| *m == "getblock")
            .count(),
        113 + 3
    );
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
