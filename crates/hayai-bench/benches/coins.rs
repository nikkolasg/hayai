//! Benchmark: hayai-coins (RocksDB and in-memory backings) against Zakura's UTXO layout
//! (`hayai_bench::zakura_utxo_layout`).
//!
//! The stores are seeded once with the same 2,002,000 coins and cached under
//! `bench-fixtures/` at the repository root; groups that mutate a store work on a copy under
//! `target/`. Every outpoint is derived from a fixed seed so runs are repeatable. `hayai` ids
//! run on `RocksBacking`, `hayai-mem` ids on `MemBacking` (both behind the same
//! `CoinsCache`).
//!
//! Groups:
//! - `coins/lookup_block_inputs/{1000,13000}`: the inputs of one block. `hayai` and
//!   `hayai-mem` empty the cache map before each iteration so every input goes to the backing
//!   in one batched round; `hayai-mem-backing` is `MemBacking::get_many` without the cache
//!   (no insertion into the map); `hayai-warm` is the second access (all map hits);
//!   `zakura-layout-1round` is two point gets per input once; `zakura-node-3rounds` is what
//!   the node does (verifier, contextual check, finalization), three serial rounds of two
//!   gets each.
//! - `coins/commit_block/13000`: apply a block spending 13,000 coins and creating 13,000, with
//!   one read round and one write batch in every store (hayai: fetch, spend, add, flush).
//!   `hayai-mem` syncs the log after the write (the default); `hayai-mem-nofsync` does not,
//!   as RocksDB does not sync its write-ahead log by default.
//! - `coins/fresh_spend/13000`: a block creating 13,000 coins followed by a block spending
//!   them inside one flush window. The hayai flush writes nothing; the baseline writes and
//!   deletes every record.
//! - `coins/snapshot/{write,load}/2002000`: `MemBacking::snapshot` of the seeded set, and
//!   `MemBacking::open` of that snapshot. `crc32c` and `blake2b` hash the snapshot file's
//!   bytes: the two checksum candidates of the snapshot and log format.
//! - `coins/memory_per_coin`: heap bytes per coin of `MemBacking` (loaded from the
//!   snapshot) and of the rejected layouts (`hashmap`, `compact`, `sharded-compact`), heap
//!   bytes per nullifier of `MemBacking`'s sorted runs and of sharded hash sets, read from
//!   the counting allocator around each build and printed as `[mem]` lines; the timed ids
//!   are a 13,000-coin `get_many` and a 1,000-nullifier `contains_many` on each layout.
//!   The table goes to `bench-results/coins-memory.json`.

#[cfg(not(feature = "mimalloc"))]
use std::alloc::System;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{
    criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode, Throughput,
};
use hayai_bench::hayai_id;
use hayai_bench::scenarios::coins::layouts::{CoinLayout, HashNullifiers, COIN_LAYOUTS};
use hayai_bench::scenarios::coins::{
    block_txs, hayai_commit, mem_config, open_hayai, open_mem, zakura_commit, Fixtures, Universe,
    BLOCK_INPUTS, COMMIT_BASE_HEIGHT, SEEDED_COINS,
};
use hayai_bench::sysmetrics::{heap_snapshot, CountingAlloc};
use hayai_bench::zakura_utxo_layout::{OutputLocation, ZakuraUtxoDb};
use hayai_bench::zebra_utxo_layout::ZebraUtxoDb;
use hayai_coins::{
    Coin, CoinsBacking, CoinsCache, FlushStats, MemBacking, MemConfig, OutPoint, Pool,
};
use rand::{Rng, SeedableRng};

// The same choice as `bench_allocator!` (mimalloc under the feature, glibc malloc otherwise),
// wrapped in the counting allocator so that `coins/memory_per_coin` reads live heap bytes.
// Every group of this bench runs on it.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: CountingAlloc<hayai_bench::mimalloc::MiMalloc> =
    CountingAlloc(hayai_bench::mimalloc::MiMalloc);
#[cfg(not(feature = "mimalloc"))]
#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc(System);

const FRESH_BASE_HEIGHT: u32 = 500_000;
const FRESH_SEED: u64 = 0x6672_6573_6821_2121;
/// Nullifiers of the `coins/memory_per_coin` nullifier rows.
const NULLIFIERS: usize = SEEDED_COINS;

/// One cold lookup round per iteration: the cache map is emptied first.
fn cold_lookup(cache: &mut CoinsCache, outpoints: &[OutPoint], iters: u64) -> Duration {
    let mut total = Duration::ZERO;
    for _ in 0..iters {
        cache.drop_clean();
        let start = Instant::now();
        let coins = cache.fetch_many(outpoints).expect("fetch_many");
        total += start.elapsed();
        assert_eq!(coins.iter().flatten().count(), outpoints.len());
    }
    total
}

fn bench_lookup(c: &mut Criterion, universe: &Universe, fixtures: &Fixtures) {
    let mut cache = open_hayai(&fixtures.hayai);
    let mut mem = open_mem(&fixtures.mem, &mem_config());
    let zakura = ZakuraUtxoDb::open(&fixtures.zakura).expect("open zakura layout");
    let mut group = c.benchmark_group("coins/lookup_block_inputs");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(3));

    for &inputs in &[1_000usize, BLOCK_INPUTS] {
        let outpoints = universe.block_inputs(inputs);
        group.throughput(Throughput::Elements(inputs as u64));

        group.bench_with_input(
            BenchmarkId::new(hayai_id(""), inputs),
            &outpoints,
            |b, outpoints| b.iter_custom(|iters| cold_lookup(&mut cache, outpoints, iters)),
        );
        group.bench_with_input(
            BenchmarkId::new(hayai_id("mem"), inputs),
            &outpoints,
            |b, outpoints| b.iter_custom(|iters| cold_lookup(&mut mem, outpoints, iters)),
        );
        group.bench_with_input(
            BenchmarkId::new(hayai_id("mem-backing"), inputs),
            &outpoints,
            |b, outpoints| {
                b.iter(|| {
                    let coins = mem.backing().get_many(outpoints).expect("get_many");
                    assert_eq!(coins.iter().flatten().count(), outpoints.len());
                    coins
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new(hayai_id("warm"), inputs),
            &outpoints,
            |b, outpoints| {
                cache.fetch_many(outpoints).expect("warm up");
                b.iter(|| cache.fetch_many(outpoints).expect("fetch_many"))
            },
        );
        group.bench_with_input(
            BenchmarkId::new("zakura-layout-1round", inputs),
            &outpoints,
            |b, outpoints| {
                b.iter(|| {
                    let found = zakura.lookup_block_inputs_1round(outpoints);
                    assert_eq!(found.iter().flatten().count(), outpoints.len());
                    found
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("zakura-node-3rounds", inputs),
            &outpoints,
            |b, outpoints| b.iter(|| zakura.lookup_block_inputs_zakura_style(outpoints)),
        );
    }
    group.finish();
}

/// `coins/lookup_block_inputs/13000` id `zebra`: Zebra's seven gets per input over the
/// Zakura layout's database, opened with Zebra's options
/// (`hayai_bench::zebra_utxo_layout`). It runs after `bench_lookup`, which has closed its
/// handle on the same database.
fn bench_lookup_zebra(c: &mut Criterion, universe: &Universe, fixtures: &Fixtures) {
    let zebra = ZebraUtxoDb::open(&fixtures.zakura).expect("open zakura layout as zebra");
    let mut group = c.benchmark_group("coins/lookup_block_inputs");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(3));
    let outpoints = universe.block_inputs(BLOCK_INPUTS);
    group.throughput(Throughput::Elements(BLOCK_INPUTS as u64));
    group.bench_with_input(
        BenchmarkId::new("zebra", BLOCK_INPUTS),
        &outpoints,
        |b, outpoints| {
            b.iter(|| {
                let found = zebra.lookup_block_inputs_zebra_style(outpoints);
                assert_eq!(found.iter().flatten().count(), outpoints.len());
                found
            })
        },
    );
    group.finish();
}

/// Commits blocks `*next..` of the commit universe into `cache`, one per iteration.
fn commit_blocks(
    cache: &mut CoinsCache,
    universe: &Universe,
    next: &mut usize,
    iters: u64,
) -> Duration {
    let mut total = Duration::ZERO;
    for _ in 0..iters {
        let (created, spent) = universe.commit_block(*next);
        *next += 1;
        let spent_outpoints: Vec<OutPoint> = spent.iter().map(|s| s.outpoint.clone()).collect();
        let start = Instant::now();
        hayai_commit(cache, created, &spent_outpoints);
        total += start.elapsed();
    }
    total
}

fn bench_commit(c: &mut Criterion, universe: &Universe, fixtures: &Fixtures) {
    let mut cache = open_hayai(&fixtures.scratch("hayai", "commit"));
    let mut mem = open_mem(&fixtures.scratch("mem", "commit"), &mem_config());
    let mut mem_nofsync = open_mem(
        &fixtures.scratch("mem", "commit-nofsync"),
        &MemConfig {
            fsync_every_generations: 0,
        },
    );
    let zakura = ZakuraUtxoDb::open(&fixtures.scratch("zakura", "commit")).expect("open");
    let mut group = c.benchmark_group("coins/commit_block");
    group
        .sample_size(10)
        .sampling_mode(SamplingMode::Flat)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5))
        .throughput(Throughput::Elements(2 * BLOCK_INPUTS as u64));

    for (id, cache) in [
        (hayai_id(""), &mut cache),
        (hayai_id("mem"), &mut mem),
        (hayai_id("mem-nofsync"), &mut mem_nofsync),
    ] {
        let mut next_block = 0usize;
        group.bench_function(BenchmarkId::new(id, BLOCK_INPUTS), |b| {
            b.iter_custom(|iters| commit_blocks(cache, universe, &mut next_block, iters))
        });
    }

    let mut zakura_next_block = 0usize;
    group.bench_function(BenchmarkId::new("zakura", BLOCK_INPUTS), |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let (created, spent) = universe.commit_block(zakura_next_block);
                let height = COMMIT_BASE_HEIGHT + zakura_next_block as u32;
                zakura_next_block += 1;
                let spent_outpoints: Vec<OutPoint> =
                    spent.iter().map(|s| s.outpoint.clone()).collect();
                let txs = block_txs(&created);
                let start = Instant::now();
                zakura_commit(&zakura, height, &txs, &spent_outpoints);
                total += start.elapsed();
            }
            total
        })
    });
    group.finish();
}

/// Adds a fresh block and spends it inside one flush window, one block per iteration;
/// returns the time and ten outpoints of the last block.
fn fresh_blocks(cache: &mut CoinsCache, next: &mut u64, iters: u64) -> (Duration, Vec<OutPoint>) {
    let mut total = Duration::ZERO;
    let mut sample = Vec::new();
    for _ in 0..iters {
        let height = FRESH_BASE_HEIGHT + 2 * *next as u32;
        let created = Universe::created_block(FRESH_SEED ^ *next, height);
        *next += 1;
        let outpoints: Vec<OutPoint> = created.iter().map(|s| s.outpoint.clone()).collect();
        sample = outpoints[..10].to_vec();
        let start = Instant::now();
        for s in created {
            cache.add(s.outpoint, s.coin).expect("add");
        }
        let coins = cache.fetch_many(&outpoints).expect("fetch inputs");
        assert_eq!(coins.iter().flatten().count(), BLOCK_INPUTS);
        for outpoint in &outpoints {
            cache.spend(outpoint).expect("spend fresh");
        }
        let stats = cache.flush().expect("flush");
        total += start.elapsed();
        assert_eq!(stats, FlushStats::default(), "fresh spends write nothing");
    }
    (total, sample)
}

fn bench_fresh_spend(c: &mut Criterion, fixtures: &Fixtures) {
    let mut cache = open_hayai(&fixtures.scratch("hayai", "fresh"));
    let mut mem = open_mem(&fixtures.scratch("mem", "fresh"), &mem_config());
    let zakura = ZakuraUtxoDb::open(&fixtures.scratch("zakura", "fresh")).expect("open");
    let mut group = c.benchmark_group("coins/fresh_spend");
    group
        .sample_size(10)
        .sampling_mode(SamplingMode::Flat)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5))
        .throughput(Throughput::Elements(BLOCK_INPUTS as u64));

    for (id, cache) in [(hayai_id(""), &mut cache), (hayai_id("mem"), &mut mem)] {
        let mut next = 0u64;
        let mut sample: Vec<OutPoint> = Vec::new();
        group.bench_function(BenchmarkId::new(id, BLOCK_INPUTS), |b| {
            b.iter_custom(|iters| {
                let (time, last) = fresh_blocks(cache, &mut next, iters);
                sample = last;
                time
            })
        });
        let on_disk = cache.backing().get_many(&sample).expect("get_many");
        assert_eq!(
            on_disk.iter().flatten().count(),
            0,
            "fresh-spent coins never reached the backing"
        );
    }

    let mut zakura_next = 0u64;
    group.bench_function(BenchmarkId::new("zakura", BLOCK_INPUTS), |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let height = FRESH_BASE_HEIGHT + 2 * zakura_next as u32;
                let created = Universe::created_block(FRESH_SEED ^ zakura_next, height);
                zakura_next += 1;
                let outpoints: Vec<OutPoint> = created.iter().map(|s| s.outpoint.clone()).collect();
                let txs = block_txs(&created);
                let start = Instant::now();
                zakura
                    .write_block(height, &txs, &[])
                    .expect("creating block");
                let found = zakura.lookup_block_inputs_1round(&outpoints);
                let locations: Vec<OutputLocation> = found
                    .into_iter()
                    .map(|f| f.expect("input exists").0)
                    .collect();
                zakura
                    .write_block(height + 1, &[], &locations)
                    .expect("spending block");
                total += start.elapsed();
            }
            total
        })
    });
    group.finish();
}

/// Resident set size of the process.
fn rss_bytes() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").expect("statm");
    let pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .expect("resident field")
        .parse()
        .expect("number");
    // SAFETY: sysconf has no preconditions.
    pages * unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64
}

/// Gives freed heap back to the kernel so that the next RSS delta starts from a clean state.
fn trim_heap() {
    #[cfg(not(feature = "mimalloc"))]
    // SAFETY: malloc_trim only releases free memory of glibc's arenas.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Runs `build` and returns its result, the live heap and RSS growth per entry, and the
/// build time, and records them as one row of `coins-memory.json`.
fn measure<T>(
    rows: &mut Vec<serde_json::Value>,
    set: &str,
    layout: &str,
    entries: usize,
    build: impl FnOnce() -> T,
) -> T {
    trim_heap();
    let (heap0, rss0) = (heap_snapshot().live_bytes, rss_bytes());
    let start = Instant::now();
    let built = build();
    let elapsed = start.elapsed();
    let (heap1, rss1) = (heap_snapshot().live_bytes, rss_bytes());
    let heap = (heap1.saturating_sub(heap0)) as f64 / entries as f64;
    let rss = rss1.saturating_sub(rss0) as f64 / entries as f64;
    eprintln!(
        "[mem] {set} {layout}: heap {heap:.1} B/entry, rss {rss:.1} B/entry, build {:.1} ms ({entries} entries)",
        elapsed.as_secs_f64() * 1e3
    );
    rows.push(serde_json::json!({
        "set": set,
        "layout": layout,
        "entries": entries,
        "heap_bytes_per_entry": heap,
        "rss_bytes_per_entry": rss,
        "build_ms": elapsed.as_secs_f64() * 1e3,
    }));
    built
}

fn bench_memory(c: &mut Criterion, universe: &Universe, fixtures: &Fixtures) {
    let mut group = c.benchmark_group("coins/memory_per_coin");
    group
        .sample_size(10)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    let mut rows = Vec::new();
    let lookups = universe.block_inputs(BLOCK_INPUTS);

    // MemBacking as a restart builds it: loaded from the snapshot.
    let mem = measure(&mut rows, "coins", "mem", SEEDED_COINS, || {
        MemBacking::open(&fixtures.mem, &mem_config())
            .expect("open mem store")
            .0
    });
    assert_eq!(mem.coin_count(), SEEDED_COINS);
    group.bench_function(BenchmarkId::new("mem", "get_many_13000"), |b| {
        b.iter(|| {
            let found = mem.get_many(&lookups).expect("get_many");
            assert_eq!(found.iter().flatten().count(), lookups.len());
            found
        })
    });
    drop(mem);

    let coins: Vec<(OutPoint, Coin)> = (0..SEEDED_COINS)
        .map(|k| {
            let s = universe.seeded(k);
            (s.outpoint, s.coin)
        })
        .collect();
    for name in COIN_LAYOUTS {
        let layout = measure(&mut rows, "coins", name, coins.len(), || {
            CoinLayout::build(name, &coins)
        });
        group.bench_function(BenchmarkId::new(name, "get_many_13000"), |b| {
            b.iter(|| {
                let found = layout.get_many(&lookups);
                assert_eq!(found.iter().flatten().count(), lookups.len());
                found
            })
        });
    }
    drop(coins);

    let mut rng = rand::rngs::StdRng::seed_from_u64(0x6e75_6c6c);
    let nullifiers: Vec<[u8; 32]> = (0..NULLIFIERS).map(|_| rng.gen()).collect();
    // A block's checks: one in ten is a member (a double spend), the rest are fresh.
    let queries: Vec<[u8; 32]> = (0..1_000)
        .map(|i| match i % 10 {
            0 => nullifiers[i * 997],
            _ => rng.gen(),
        })
        .collect();
    let scratch = hayai_bench::scratch_dir();
    {
        let (store, _) = MemBacking::open(scratch.path(), &mem_config()).expect("open");
        store
            .insert_many(Pool::Orchard, &nullifiers)
            .expect("insert");
        store.snapshot().expect("snapshot");
    }
    let store = measure(&mut rows, "nullifiers", "mem", NULLIFIERS, || {
        MemBacking::open(scratch.path(), &mem_config())
            .expect("open")
            .0
    });
    group.bench_function(BenchmarkId::new("mem", "contains_1000"), |b| {
        b.iter(|| {
            let found = store
                .contains_many(Pool::Orchard, &queries)
                .expect("contains");
            assert_eq!(found.iter().filter(|&&f| f).count(), 100);
            found
        })
    });
    drop(store);
    let sets = measure(&mut rows, "nullifiers", "hashset", NULLIFIERS, || {
        HashNullifiers::build(&nullifiers)
    });
    group.bench_function(BenchmarkId::new("hashset", "contains_1000"), |b| {
        b.iter(|| {
            let found = sets.contains_many(&queries);
            assert_eq!(found.iter().filter(|&&f| f).count(), 100);
            found
        })
    });
    group.finish();

    // The report reads this table; criterion has no slot for a non-timed measurement.
    let out = hayai_bench::results_dir().join("coins-memory.json");
    std::fs::write(
        &out,
        serde_json::to_vec_pretty(&rows).expect("serializable"),
    )
    .unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));
}

fn open_backing(dir: &Path) -> MemBacking {
    MemBacking::open(dir, &mem_config())
        .expect("open mem store")
        .0
}

fn bench_snapshot(c: &mut Criterion, fixtures: &Fixtures) {
    let dir = fixtures.scratch("mem", "snapshot");
    let backing = Arc::new(open_backing(&dir));
    let mut group = c.benchmark_group("coins/snapshot");
    group
        .sample_size(10)
        .sampling_mode(SamplingMode::Flat)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5))
        .throughput(Throughput::Elements(SEEDED_COINS as u64));
    let mut size = 0;
    group.bench_function(BenchmarkId::new(hayai_id("mem-write"), SEEDED_COINS), |b| {
        b.iter(|| size = backing.snapshot().expect("snapshot"))
    });
    eprintln!("[snapshot] {SEEDED_COINS} coins: {size} bytes");
    drop(backing);
    group.bench_function(BenchmarkId::new(hayai_id("mem-load"), SEEDED_COINS), |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                let loaded = open_backing(&dir);
                total += start.elapsed();
                assert_eq!(loaded.coin_count(), SEEDED_COINS);
            }
            total
        })
    });
    let bytes = std::fs::read(dir.join("coins.snapshot")).expect("read snapshot");
    group.throughput(Throughput::Bytes(bytes.len() as u64));
    group.bench_function(BenchmarkId::new("crc32c", SEEDED_COINS), |b| {
        b.iter(|| crc32c::crc32c(&bytes))
    });
    group.bench_function(BenchmarkId::new("blake2b", SEEDED_COINS), |b| {
        b.iter(|| blake2b_simd::blake2b(&bytes))
    });
    group.finish();
}

fn bench(c: &mut Criterion) {
    let universe = Universe::new();
    let fixtures = Fixtures::prepare(&universe);
    bench_memory(c, &universe, &fixtures);
    bench_snapshot(c, &fixtures);
    bench_lookup(c, &universe, &fixtures);
    bench_lookup_zebra(c, &universe, &fixtures);
    bench_commit(c, &universe, &fixtures);
    bench_fresh_spend(c, &fixtures);
}

criterion_group!(benches, bench);
criterion_main!(benches);
