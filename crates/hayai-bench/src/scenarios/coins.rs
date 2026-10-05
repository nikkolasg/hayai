//! The coin universe, seeded fixture stores and per-block bodies of the coins benchmarks
//! (`benches/coins.rs`) and of the `coins_*` sysbench scenarios.
//!
//! The stores (RocksDB, in-memory snapshot, Zakura layout) are seeded once with the same
//! 2,002,000 coins and cached under `bench-fixtures/` at the repository root; scenarios that
//! mutate a store work on a copy under `target/`. Every outpoint is derived from a fixed seed
//! so runs are repeatable.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use hayai_coins::{
    Coin, CoinsBacking, CoinsCache, Config, FlushStats, MemBacking, MemConfig, OutPoint,
    RocksBacking,
};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use zk_chain::transparent::{Output, Script};

use super::{Built, Impl};
use crate::zakura_utxo_layout::{BlockTx, OutputLocation, ZakuraUtxoDb};

pub const BLOCK_INPUTS: usize = 13_000;
/// Seeded coins: 154 blocks' worth of inputs, so block `b >= 154` of the commit benchmark
/// spends exactly the coins block `b - 154` created and the UTXO set never runs dry.
pub const SEEDED_COINS: usize = 154 * BLOCK_INPUTS;
/// Seeding layout for the Zakura baseline: 1,000 transactions of 2 outputs per block.
const SEED_TXS_PER_BLOCK: usize = 1_000;
const SEED_OUTPUTS_PER_TX: usize = 2;
const SEED_COINS_PER_BLOCK: usize = SEED_TXS_PER_BLOCK * SEED_OUTPUTS_PER_TX;
const SEED_BLOCKS: usize = SEEDED_COINS / SEED_COINS_PER_BLOCK;
/// Heights of blocks the commit benchmark creates start after the seeded ones.
pub const COMMIT_BASE_HEIGHT: u32 = SEED_BLOCKS as u32 + 1;
const TXID_SEED: u64 = 0x6861_7961_6920_636f;
const CREATED_SEED: u64 = 0x6372_6561_7465_6421;
const FIXTURE_VERSION: &str = "v1";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn p2pkh(txid: &[u8; 32]) -> Vec<u8> {
    let mut script = Vec::with_capacity(25);
    script.extend_from_slice(&[0x76, 0xa9, 0x14]);
    script.extend_from_slice(&txid[..20]);
    script.extend_from_slice(&[0x88, 0xac]);
    script
}

fn zk_output(coin: &Coin) -> Output {
    Output {
        value: coin.value.try_into().expect("value is a valid amount"),
        lock_script: Script::new(&coin.script_pubkey),
    }
}

/// One coin of a synthetic block.
pub struct Seeded {
    pub outpoint: OutPoint,
    pub coin: Coin,
}

/// The deterministic coin universe shared by both stores.
pub struct Universe {
    seeded_txids: Vec<[u8; 32]>,
    /// Seeded coins in the order the commit benchmark spends them: a permutation, so that a
    /// block spends coins from all over the set rather than the neighbours it was seeded
    /// with (adjacent keys in the Zakura layout, which would turn its reads into cache hits).
    spend_order: Vec<u32>,
}

impl Default for Universe {
    fn default() -> Self {
        Self::new()
    }
}

impl Universe {
    pub fn new() -> Self {
        assert_eq!(SEEDED_COINS % SEED_COINS_PER_BLOCK, 0);
        let mut rng = StdRng::seed_from_u64(TXID_SEED);
        let seeded_txids = (0..SEEDED_COINS / SEED_OUTPUTS_PER_TX)
            .map(|_| rng.gen::<[u8; 32]>())
            .collect();
        let mut spend_order: Vec<u32> = (0..SEEDED_COINS as u32).collect();
        spend_order.shuffle(&mut rng);
        Universe {
            seeded_txids,
            spend_order,
        }
    }

    pub fn seeded(&self, k: usize) -> Seeded {
        let txid = self.seeded_txids[k / SEED_OUTPUTS_PER_TX];
        let output_index = (k % SEED_OUTPUTS_PER_TX) as u32;
        let block = k / SEED_COINS_PER_BLOCK;
        Seeded {
            outpoint: OutPoint::new(txid, output_index),
            coin: Coin {
                value: 1_000 + k as u64,
                script_pubkey: Bytes::from(p2pkh(&txid)),
                height: block as u32 + 1,
                is_coinbase: false,
            },
        }
    }

    /// The coins a synthetic block creates: `BLOCK_INPUTS` one-output transactions.
    pub fn created_block(seed: u64, height: u32) -> Vec<Seeded> {
        let mut rng = StdRng::seed_from_u64(seed);
        (0..BLOCK_INPUTS)
            .map(|i| {
                let txid: [u8; 32] = rng.gen();
                Seeded {
                    outpoint: OutPoint::new(txid, 0),
                    coin: Coin {
                        value: 5_000 + i as u64,
                        script_pubkey: Bytes::from(p2pkh(&txid)),
                        height,
                        is_coinbase: false,
                    },
                }
            })
            .collect()
    }

    /// Block `b` of the commit benchmark: what it creates and what it spends.
    pub fn commit_block(&self, b: usize) -> (Vec<Seeded>, Vec<Seeded>) {
        let height = COMMIT_BASE_HEIGHT + b as u32;
        let created = Self::created_block(CREATED_SEED ^ b as u64, height);
        let seeded_blocks = SEEDED_COINS / BLOCK_INPUTS;
        let spent = if b < seeded_blocks {
            self.spend_order[b * BLOCK_INPUTS..(b + 1) * BLOCK_INPUTS]
                .iter()
                .map(|&k| self.seeded(k as usize))
                .collect()
        } else {
            let source = b - seeded_blocks;
            Self::created_block(
                CREATED_SEED ^ source as u64,
                COMMIT_BASE_HEIGHT + source as u32,
            )
        };
        (created, spent)
    }

    /// `inputs` distinct seeded outpoints drawn with the seed `inputs`.
    pub fn block_inputs(&self, inputs: usize) -> Vec<OutPoint> {
        let mut rng = StdRng::seed_from_u64(inputs as u64);
        rand::seq::index::sample(&mut rng, SEEDED_COINS, inputs)
            .into_iter()
            .map(|k| self.seeded(k).outpoint)
            .collect()
    }
}

pub fn block_txs(coins: &[Seeded]) -> Vec<BlockTx> {
    coins
        .iter()
        .map(|s| BlockTx {
            txid: *s.outpoint.hash(),
            outputs: vec![zk_output(&s.coin)],
        })
        .collect()
}

fn copy_dir(from: &Path, to: &Path) {
    if to.exists() {
        fs::remove_dir_all(to).expect("remove old scratch copy");
    }
    fs::create_dir_all(to).expect("create scratch dir");
    for entry in fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.metadata().expect("metadata").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

fn hayai_config() -> Config {
    Config::default()
}

/// Seeded fixture directories, created on first use.
pub struct Fixtures {
    pub hayai: PathBuf,
    /// A `MemBacking` store: one snapshot of the seeded coins and an empty log.
    pub mem: PathBuf,
    pub zakura: PathBuf,
}

impl Fixtures {
    pub fn prepare(universe: &Universe) -> Self {
        let root = repo_root().join("bench-fixtures");
        fs::create_dir_all(&root).expect("bench-fixtures dir");
        let hayai = root.join(format!("coins-hayai-{FIXTURE_VERSION}"));
        let zakura = root.join(format!("coins-zakura-{FIXTURE_VERSION}"));
        let mem = root.join(format!("coins-mem-{FIXTURE_VERSION}"));
        if !mem.join("SEEDED").exists() {
            if mem.exists() {
                fs::remove_dir_all(&mem).expect("remove partial fixture");
            }
            let start = Instant::now();
            Self::seed_mem(universe, &mem);
            eprintln!("seeded mem coins in {:?}", start.elapsed());
        }
        if !hayai.join("SEEDED").exists() {
            if hayai.exists() {
                fs::remove_dir_all(&hayai).expect("remove partial fixture");
            }
            let start = Instant::now();
            Self::seed_hayai(universe, &hayai);
            eprintln!("seeded hayai coins in {:?}", start.elapsed());
        }
        if !zakura.join("SEEDED").exists() {
            if zakura.exists() {
                fs::remove_dir_all(&zakura).expect("remove partial fixture");
            }
            let start = Instant::now();
            Self::seed_zakura(universe, &zakura);
            eprintln!("seeded zakura layout in {:?}", start.elapsed());
        }
        eprintln!(
            "db size: hayai {} MiB, mem snapshot {} MiB, zakura layout {} MiB ({} coins)",
            crate::sysmetrics::dir_size(&hayai).expect("fixture size") >> 20,
            crate::sysmetrics::dir_size(&mem).expect("fixture size") >> 20,
            crate::sysmetrics::dir_size(&zakura).expect("fixture size") >> 20,
            SEEDED_COINS
        );
        Fixtures { hayai, mem, zakura }
    }

    fn seed_mem(universe: &Universe, path: &Path) {
        let (backing, _) = MemBacking::open(path, &mem_config()).expect("open mem store");
        for chunk in (0..SEEDED_COINS).collect::<Vec<_>>().chunks(100_000) {
            let coins: Vec<Seeded> = chunk.iter().map(|&k| universe.seeded(k)).collect();
            let adds: Vec<(&OutPoint, &Coin)> =
                coins.iter().map(|s| (&s.outpoint, &s.coin)).collect();
            backing.write_batch(&adds, &[]).expect("seed write");
        }
        backing.snapshot().expect("seed snapshot");
        drop(backing);
        fs::write(path.join("SEEDED"), FIXTURE_VERSION).expect("marker");
    }

    fn seed_hayai(universe: &Universe, path: &Path) {
        let backing = RocksBacking::open(path, &hayai_config()).expect("open hayai store");
        for chunk in (0..SEEDED_COINS).collect::<Vec<_>>().chunks(100_000) {
            let coins: Vec<Seeded> = chunk.iter().map(|&k| universe.seeded(k)).collect();
            let adds: Vec<(&OutPoint, &Coin)> =
                coins.iter().map(|s| (&s.outpoint, &s.coin)).collect();
            backing.write_batch(&adds, &[]).expect("seed write");
        }
        drop(backing);
        fs::write(path.join("SEEDED"), FIXTURE_VERSION).expect("marker");
    }

    fn seed_zakura(universe: &Universe, path: &Path) {
        let db = ZakuraUtxoDb::open(path).expect("open zakura layout");
        for block in 0..SEED_BLOCKS {
            let txs: Vec<BlockTx> = (0..SEED_TXS_PER_BLOCK)
                .map(|t| {
                    let first = block * SEED_COINS_PER_BLOCK + t * SEED_OUTPUTS_PER_TX;
                    let coins: Vec<Seeded> = (first..first + SEED_OUTPUTS_PER_TX)
                        .map(|k| universe.seeded(k))
                        .collect();
                    BlockTx {
                        txid: *coins[0].outpoint.hash(),
                        outputs: coins.iter().map(|s| zk_output(&s.coin)).collect(),
                    }
                })
                .collect();
            db.write_block(block as u32 + 1, &txs, &[])
                .expect("seed block");
        }
        drop(db);
        fs::write(path.join("SEEDED"), FIXTURE_VERSION).expect("marker");
    }

    /// A private copy for a group that mutates the store.
    pub fn scratch(&self, which: &str, group: &str) -> PathBuf {
        let from = match which {
            "hayai" => &self.hayai,
            "mem" => &self.mem,
            "zakura" => &self.zakura,
            other => panic!("unknown store {other}"),
        };
        let to = repo_root()
            .join("target/coins-bench-scratch")
            .join(format!("{which}-{group}"));
        copy_dir(from, &to);
        to
    }
}

pub fn open_hayai(path: &Path) -> CoinsCache {
    let backing = RocksBacking::open(path, &hayai_config()).expect("open hayai store");
    CoinsCache::new(Arc::new(backing))
}

/// The configuration of the in-memory store: the default (a sync per log record).
pub fn mem_config() -> MemConfig {
    MemConfig::default()
}

/// A coins cache over a `MemBacking` opened in `path` with `config`.
pub fn open_mem(path: &Path, config: &MemConfig) -> CoinsCache {
    let (backing, _) = MemBacking::open(path, config).expect("open mem store");
    CoinsCache::new(Arc::new(backing))
}

/// The timed body of a hayai block commit: one fetch round, spends, adds, one flush.
pub fn hayai_commit(cache: &mut CoinsCache, created: Vec<Seeded>, spent: &[OutPoint]) {
    let coins = cache.fetch_many(spent).expect("fetch inputs");
    assert_eq!(coins.iter().flatten().count(), BLOCK_INPUTS, "inputs exist");
    for outpoint in spent {
        cache.spend(outpoint).expect("spend");
    }
    for s in created {
        cache.add(s.outpoint, s.coin).expect("add");
    }
    let stats = cache.flush().expect("flush");
    assert_eq!(
        stats,
        FlushStats {
            adds: BLOCK_INPUTS,
            spends: BLOCK_INPUTS
        }
    );
}

/// The timed body of a block commit in Zakura's layout: one lookup round, one write batch.
pub fn zakura_commit(db: &ZakuraUtxoDb, height: u32, txs: &[BlockTx], spent: &[OutPoint]) {
    let found = db.lookup_block_inputs_1round(spent);
    let locations: Vec<OutputLocation> = found
        .into_iter()
        .map(|f| f.expect("input exists").0)
        .collect();
    db.write_block(height, txs, &locations)
        .expect("write block");
}

/// `coins_lookup_13000`: the inputs of one block, hayai from an emptied in-memory map (one
/// batched RocksDB round) against Zakura's node-style three rounds of two gets each.
pub fn build_lookup(imp: Impl) -> Built {
    let universe = Universe::new();
    let fixtures = Fixtures::prepare(&universe);
    let outpoints = universe.block_inputs(BLOCK_INPUTS);
    match imp {
        Impl::Hayai => {
            let mut cache = open_hayai(&fixtures.hayai);
            Built::new(move |m| {
                cache.drop_clean();
                let coins = m.timed(|| cache.fetch_many(&outpoints).expect("fetch_many"));
                assert_eq!(coins.iter().flatten().count(), outpoints.len());
            })
        }
        Impl::Zebra => unreachable!("{}", super::NO_ZEBRA),
        Impl::Zakura => {
            let db = ZakuraUtxoDb::open(&fixtures.zakura).expect("open zakura layout");
            Built::new(move |m| {
                let found = m.timed(|| db.lookup_block_inputs_zakura_style(&outpoints));
                assert_eq!(found.iter().flatten().count(), outpoints.len());
            })
        }
    }
}

/// `coins_commit_13000`: apply a block spending 13,000 coins and creating 13,000, on a
/// scratch copy of the seeded store.
pub fn build_commit(imp: Impl) -> Built {
    let universe = Universe::new();
    let fixtures = Fixtures::prepare(&universe);
    let mut next_block = 0usize;
    match imp {
        Impl::Hayai => {
            let dir = fixtures.scratch("hayai", "sysbench");
            let mut cache = open_hayai(&dir);
            Built::new(move |m| {
                let (created, spent) = universe.commit_block(next_block);
                next_block += 1;
                let spent: Vec<OutPoint> = spent.iter().map(|s| s.outpoint.clone()).collect();
                m.timed(|| hayai_commit(&mut cache, created, &spent));
            })
            .with_scratch(dir)
        }
        Impl::Zebra => unreachable!("{}", super::NO_ZEBRA),
        Impl::Zakura => {
            let dir = fixtures.scratch("zakura", "sysbench");
            let db = ZakuraUtxoDb::open(&dir).expect("open");
            Built::new(move |m| {
                let (created, spent) = universe.commit_block(next_block);
                let height = COMMIT_BASE_HEIGHT + next_block as u32;
                next_block += 1;
                let spent: Vec<OutPoint> = spent.iter().map(|s| s.outpoint.clone()).collect();
                let txs = block_txs(&created);
                m.timed(|| zakura_commit(&db, height, &txs, &spent));
            })
            .with_scratch(dir)
        }
    }
}

/// The rejected in-memory layouts of the coin set and of a nullifier set, measured by
/// `coins/memory_per_coin` against `MemBacking` (dense shards with a position index, and
/// sorted nullifier runs).
pub mod layouts {
    use std::collections::{HashMap, HashSet};

    use bytes::Bytes;
    use hayai_coins::{outpoint_key, Coin, OutPoint};
    use parking_lot::RwLock;
    use rayon::prelude::*;

    /// Keys per rayon task of a batched lookup, as in `MemBacking`.
    const LOOKUP_CHUNK: usize = 1024;
    const SHARDS: usize = 256;

    type Key = [u8; 36];

    /// Compact coin value as in `MemBacking`: amount, height, tag, 20-byte script hash.
    #[derive(Clone, Copy)]
    #[repr(C)]
    struct Packed {
        value: [u8; 8],
        height: [u8; 4],
        tag: u8,
        hash: [u8; 20],
    }

    const TAG_P2PKH: u8 = 0;
    const TAG_RAW: u8 = 2;
    const COINBASE: u8 = 0x80;

    /// The bench coins are all P2PKH; other scripts take the raw side map.
    fn pack(coin: &Coin) -> (Packed, Option<Box<[u8]>>) {
        let s = &coin.script_pubkey[..];
        let p2pkh = s.len() == 25 && s[..3] == [0x76, 0xa9, 0x14] && s[23..] == [0x88, 0xac];
        let mut hash = [0u8; 20];
        let (kind, raw) = match p2pkh {
            true => {
                hash.copy_from_slice(&s[3..23]);
                (TAG_P2PKH, None)
            }
            false => (TAG_RAW, Some(Box::from(s))),
        };
        let packed = Packed {
            value: coin.value.to_le_bytes(),
            height: coin.height.to_le_bytes(),
            tag: kind | if coin.is_coinbase { COINBASE } else { 0 },
            hash,
        };
        (packed, raw)
    }

    fn unpack(p: &Packed, raw: Option<&[u8]>) -> Coin {
        let script = match (p.tag & !COINBASE, raw) {
            (TAG_P2PKH, _) => {
                let mut s = Vec::with_capacity(25);
                s.extend_from_slice(&[0x76, 0xa9, 0x14]);
                s.extend_from_slice(&p.hash);
                s.extend_from_slice(&[0x88, 0xac]);
                Bytes::from(s)
            }
            (_, raw) => Bytes::copy_from_slice(raw.expect("raw script")),
        };
        Coin {
            value: u64::from_le_bytes(p.value),
            script_pubkey: script,
            height: u32::from_le_bytes(p.height),
            is_coinbase: p.tag & COINBASE != 0,
        }
    }

    /// Compact values inline in a hash map, scripts that do not compress in a side map.
    pub struct CompactTable {
        map: HashMap<Key, Packed, ahash::RandomState>,
        raw: HashMap<Key, Box<[u8]>, ahash::RandomState>,
    }

    impl CompactTable {
        fn build<'a>(coins: impl Iterator<Item = &'a (OutPoint, Coin)>, n: usize) -> Self {
            let mut table = CompactTable {
                map: HashMap::with_capacity_and_hasher(n, ahash::RandomState::new()),
                raw: HashMap::with_hasher(ahash::RandomState::new()),
            };
            for (o, c) in coins {
                let key = outpoint_key(o);
                let (packed, raw) = pack(c);
                table.map.insert(key, packed);
                if let Some(raw) = raw {
                    table.raw.insert(key, raw);
                }
            }
            table
        }

        fn get(&self, key: &Key) -> Option<Coin> {
            let p = self.map.get(key)?;
            Some(unpack(p, self.raw.get(key).map(|r| &r[..])))
        }
    }

    /// One rejected coin layout.
    pub enum CoinLayout {
        /// `HashMap<OutPoint, Coin>`: the coins cache's own entry type.
        Baseline(HashMap<OutPoint, Coin, ahash::RandomState>),
        /// One compact table.
        Compact(CompactTable),
        /// 256 compact tables keyed by the first txid byte, built in parallel.
        ShardedCompact(Vec<RwLock<CompactTable>>),
    }

    pub const COIN_LAYOUTS: [&str; 3] = ["hashmap", "compact", "sharded-compact"];

    impl CoinLayout {
        pub fn build(name: &str, coins: &[(OutPoint, Coin)]) -> CoinLayout {
            match name {
                "hashmap" => {
                    let mut map =
                        HashMap::with_capacity_and_hasher(coins.len(), ahash::RandomState::new());
                    for (o, c) in coins {
                        // An independent script allocation per coin, as a decode makes.
                        let coin = Coin {
                            script_pubkey: Bytes::copy_from_slice(&c.script_pubkey),
                            ..c.clone()
                        };
                        map.insert(o.clone(), coin);
                    }
                    CoinLayout::Baseline(map)
                }
                "compact" => CoinLayout::Compact(CompactTable::build(coins.iter(), coins.len())),
                "sharded-compact" => {
                    let mut by_shard = vec![Vec::new(); SHARDS];
                    for (i, (o, _)) in coins.iter().enumerate() {
                        by_shard[usize::from(o.hash()[0])].push(i);
                    }
                    CoinLayout::ShardedCompact(
                        by_shard
                            .into_par_iter()
                            .map(|idx| {
                                let n = idx.len();
                                RwLock::new(CompactTable::build(
                                    idx.into_iter().map(|i| &coins[i]),
                                    n,
                                ))
                            })
                            .collect(),
                    )
                }
                other => panic!("unknown layout {other}"),
            }
        }

        fn get(&self, o: &OutPoint) -> Option<Coin> {
            match self {
                CoinLayout::Baseline(map) => map.get(o).cloned(),
                CoinLayout::Compact(t) => t.get(&outpoint_key(o)),
                CoinLayout::ShardedCompact(shards) => {
                    let key = outpoint_key(o);
                    shards[usize::from(key[0])].read().get(&key)
                }
            }
        }

        /// Positional lookup on the rayon pool, as `MemBacking::get_many` runs it.
        pub fn get_many(&self, outpoints: &[OutPoint]) -> Vec<Option<Coin>> {
            let mut out = vec![None; outpoints.len()];
            out.par_chunks_mut(LOOKUP_CHUNK)
                .zip(outpoints.par_chunks(LOOKUP_CHUNK))
                .for_each(|(out, outpoints)| {
                    for (slot, o) in out.iter_mut().zip(outpoints) {
                        *slot = self.get(o);
                    }
                });
            out
        }
    }

    /// The rejected nullifier layout: 256 ahash sets keyed by the first byte.
    pub struct HashNullifiers(Vec<HashSet<[u8; 32], ahash::RandomState>>);

    impl HashNullifiers {
        pub fn build(nullifiers: &[[u8; 32]]) -> HashNullifiers {
            let mut by_shard = vec![Vec::new(); SHARDS];
            for nf in nullifiers {
                by_shard[usize::from(nf[0])].push(*nf);
            }
            HashNullifiers(
                by_shard
                    .into_par_iter()
                    .map(|nfs| {
                        let mut set =
                            HashSet::with_capacity_and_hasher(nfs.len(), ahash::RandomState::new());
                        set.extend(nfs);
                        set
                    })
                    .collect(),
            )
        }

        pub fn contains_many(&self, nullifiers: &[[u8; 32]]) -> Vec<bool> {
            nullifiers
                .iter()
                .map(|nf| self.0[usize::from(nf[0])].contains(nf))
                .collect()
        }
    }
}
