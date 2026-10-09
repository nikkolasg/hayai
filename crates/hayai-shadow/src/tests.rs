//! Shadow mode against an in-process mock of the followed node's JSON-RPC: the client,
//! the seed, the upstream-backed coins and the follower.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use crossbeam_channel::unbounded;
use hayai_coins::{Coin, CoinsBacking, OutPoint, Pool, RocksBacking};
use hayai_consensus::Network;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_metrics::Registry;
use hayai_state::ValuePools;
use hayai_sync::index::{HeaderIndex, SeedBlock};
use hayai_trees::OrchardFrontier;
use hayai_wire::header::BlockHash;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::backing::{SpentLog, UpstreamBacking};
use crate::test_support::*;
use crate::upstream::{Upstream, UpstreamError};
use crate::{self as shadow, Follower, UpstreamBlock, UpstreamSink};

/// A sink that keeps each report of the follower.
struct ChannelSink(crossbeam_channel::Sender<(BlockHash, Vec<UpstreamBlock>)>);

impl UpstreamSink for ChannelSink {
    fn on_upstream(&self, fork: BlockHash, blocks: Vec<UpstreamBlock>) -> bool {
        self.0.send((fork, blocks)).is_ok()
    }
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
    let registry = Registry::new();
    let trusted_coins = registry.counter("trusted_coins", "", &[]);
    let trusted_nullifiers = registry.counter("trusted_nullifiers", "", &[]);
    let backing = UpstreamBacking::new(
        inner.clone(),
        Arc::new(Upstream::new(mock.addr)),
        100,
        BranchId::Nu5,
        trusted_coins.clone(),
        trusted_nullifiers.clone(),
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
    assert_eq!(trusted_coins.get(), 2);
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
    assert_eq!(trusted_nullifiers.get(), 2);
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
    let registry = Registry::new();
    let trusted_coins = registry.counter("trusted_coins", "", &[]);
    let trusted_nullifiers = registry.counter("trusted_nullifiers", "", &[]);
    let open = |best: Option<u32>| {
        UpstreamBacking::new(
            inner.clone(),
            Arc::new(Upstream::new(mock.addr)),
            100,
            BranchId::Nu5,
            trusted_coins.clone(),
            trusted_nullifiers.clone(),
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
    let params = Network::Regtest;
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
    let params = Network::Regtest;
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
    let params = Network::Mainnet;
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
    let active = |kind: Network, height| {
        hayai_consensus::rules_at(kind, height)
            .expect("a rule set")
            .pools
            .ironwood
    };
    assert!(!active(Network::Regtest, 1_000_000));
    assert!(!active(Network::Mainnet, 3_428_142));
    assert!(active(Network::Mainnet, 3_428_143));
    assert!(active(Network::Testnet, 4_134_000));
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
        Network::Testnet,
        index,
        ChannelSink(tx),
    );
    assert!(follower.step().expect("step"));
    let Ok((fork, blocks)) = rx.try_recv() else {
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
    let Ok((fork, blocks)) = rx.try_recv() else {
        panic!("a reorg report");
    };
    assert_eq!(fork, b1.hash());
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].raw.hash(), b2_alt.hash());
    assert_eq!(blocks[0].height, start + 2);
    assert_eq!(mock.count("getblock"), 3);
}
