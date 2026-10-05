//! Benchmark: drop-in data-structure candidates on hayai's key shapes.
//!
//! Every key is random bytes (txids, nullifiers, outpoints, short ids), so every hasher under
//! test is randomly keyed. Wall time comes from criterion; allocated bytes come from the
//! counting global allocator and are printed to stderr once per parameter as `[mem] ...`
//! lines, outside the timed loops.
//!
//! Groups:
//! - `hasher/{build,hit,miss}/<hasher>/<key>/<n>`: `std::collections::HashMap<K, u32, S>` with
//!   the current `ahash::RandomState` against foldhash (fast and quality) and rapidhash's
//!   randomised state, on txid, outpoint and nullifier keys, 13,000 and 1,000,000 entries;
//!   build inserts every key into a map sized up front, hit and miss are 13,000 lookups.
//! - `concurrent_map/{lookup_6500,mixed_32r_1w}/<map>`: `DashMap<WtxId, Arc<Payload>>` (the
//!   prepared-transaction store) against papaya and scc, all on `ahash::RandomState`, 100,000
//!   resident entries. `lookup_6500` is one thread looking up every transaction of a block;
//!   `mixed_32r_1w` is 32 threads of 10,000 lookups (90 % hits) with one thread inserting and
//!   removing 1,000 entries, timed from a barrier to the last thread's finish.
//! - `smallvec/{vec,smallvec4}/<len>`: `Vec<(u8, [u8; 32])>` against `SmallVec<[_; 4]>`: push
//!   `len` items, then sum one byte over them.
//! - `layer_sets/{build,walk}/<layout>`: a chain view of 100 layers of 13,000 outpoints: an
//!   ahash `HashSet` per layer against a sorted `Vec<OutPoint>` and a sorted packed
//!   `Vec<[u8; 36]>` with binary search; `walk` is 13,000 lookups newest layer first, 99 %
//!   misses that visit every layer.
//! - `multitable/{build,hit,miss}/<table>/<n>`: the short-id index of compact-block
//!   reconstruction, `[u8; 6]` keys to 64-byte values: `AHashMap<[u8; 6], Option<[u8; 64]>>`
//!   sized up front against `MultiTableFiltered<6, 64>` at utilisation 0.77 and 0.90. The
//!   multitable arms need the `multitable` feature (a git dependency, kept out of the
//!   default build): `cargo bench -p hayai-bench --bench structures --features multitable`.

#[cfg(not(feature = "mimalloc"))]
use std::alloc::System;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hash};
#[cfg(feature = "multitable")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use hayai_bench::sysmetrics::{heap_snapshot, CountingAlloc};
#[cfg(feature = "multitable")]
use multitable::{InsertError, MultiTableFiltered};
use rand::rngs::StdRng;
use rand::{Rng, RngCore, SeedableRng};
use smallvec::SmallVec;

// ---------------------------------------------------------------------------------------------
// Counting allocator
// ---------------------------------------------------------------------------------------------

// The same choice as `bench_allocator!` (mimalloc under the feature, glibc malloc otherwise),
// wrapped in the counting allocator of `sysbench` so the memory numbers come from the same
// counters; a second `#[global_allocator]` next to `bench_allocator!` would not compile.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: CountingAlloc<hayai_bench::mimalloc::MiMalloc> =
    CountingAlloc(hayai_bench::mimalloc::MiMalloc);
#[cfg(not(feature = "mimalloc"))]
#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc(System);

/// Heap traffic of `f`: bytes allocated (sum of allocation sizes, growth included), number of
/// allocations, and bytes still live when it returns.
struct HeapUse {
    allocated: u64,
    count: u64,
    live: u64,
}

fn heap_use<T>(f: impl FnOnce() -> T) -> (T, HeapUse) {
    let before = heap_snapshot();
    let out = f();
    let after = heap_snapshot();
    (
        out,
        HeapUse {
            allocated: after.alloc_bytes - before.alloc_bytes,
            count: after.alloc_count - before.alloc_count,
            live: after.live_bytes.saturating_sub(before.live_bytes),
        },
    )
}

fn report_mem(group: &str, arm: &str, param: &str, use_: &HeapUse) {
    eprintln!(
        "[mem] {group}/{arm}/{param}: allocated={} B ({} allocs), live={} B",
        use_.allocated, use_.count, use_.live
    );
}

// ---------------------------------------------------------------------------------------------
// Key shapes
// ---------------------------------------------------------------------------------------------

type Txid = [u8; 32];

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Nullifier([u8; 32]);

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct OutPoint {
    hash: [u8; 32],
    n: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct WtxId {
    txid: [u8; 32],
    auth_digest: [u8; 32],
}

type ShortId = [u8; 6];

trait RandomKey: Copy + Eq + Hash {
    fn random(rng: &mut StdRng) -> Self;
}

impl RandomKey for Txid {
    fn random(rng: &mut StdRng) -> Self {
        let mut k = [0u8; 32];
        rng.fill_bytes(&mut k);
        k
    }
}

impl RandomKey for Nullifier {
    fn random(rng: &mut StdRng) -> Self {
        Nullifier(Txid::random(rng))
    }
}

impl RandomKey for OutPoint {
    fn random(rng: &mut StdRng) -> Self {
        OutPoint {
            hash: Txid::random(rng),
            n: rng.gen_range(0..8),
        }
    }
}

impl RandomKey for WtxId {
    fn random(rng: &mut StdRng) -> Self {
        WtxId {
            txid: Txid::random(rng),
            auth_digest: Txid::random(rng),
        }
    }
}

impl RandomKey for ShortId {
    fn random(rng: &mut StdRng) -> Self {
        let mut k = [0u8; 6];
        rng.fill_bytes(&mut k);
        k
    }
}

/// `n` distinct random keys.
fn random_keys<K: RandomKey>(rng: &mut StdRng, n: usize) -> Vec<K> {
    let mut seen: HashSet<K, ahash::RandomState> =
        HashSet::with_capacity_and_hasher(n, ahash::RandomState::new());
    let mut keys = Vec::with_capacity(n);
    while keys.len() < n {
        let k = K::random(rng);
        if seen.insert(k) {
            keys.push(k);
        }
    }
    keys
}

/// `n` keys drawn at random (with replacement) from `keys`.
fn sample<K: Copy>(rng: &mut StdRng, keys: &[K], n: usize) -> Vec<K> {
    (0..n).map(|_| keys[rng.gen_range(0..keys.len())]).collect()
}

/// `n` random keys none of which is in `present`.
fn fresh_keys<K: RandomKey>(rng: &mut StdRng, present: &[K], n: usize) -> Vec<K> {
    let present: HashSet<K, ahash::RandomState> = present.iter().copied().collect();
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let k = K::random(rng);
        if !present.contains(&k) {
            out.push(k);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// 1. hasher
// ---------------------------------------------------------------------------------------------

const LOOKUPS: usize = 13_000;

fn hasher_arm<K: RandomKey, S: BuildHasher + Default>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    hasher: &str,
    param: &str,
    keys: &[K],
    hits: &[K],
    misses: &[K],
) {
    let build = || {
        let mut map: HashMap<K, u32, S> =
            HashMap::with_capacity_and_hasher(keys.len(), S::default());
        for (i, k) in keys.iter().enumerate() {
            map.insert(*k, i as u32);
        }
        map
    };
    group.bench_function(BenchmarkId::new(format!("build/{hasher}"), param), |b| {
        b.iter(build)
    });
    let map = build();
    assert_eq!(
        hits.iter().filter(|k| map.contains_key(k)).count(),
        hits.len()
    );
    assert_eq!(misses.iter().filter(|k| map.contains_key(k)).count(), 0);
    group.bench_function(BenchmarkId::new(format!("hit/{hasher}"), param), |b| {
        b.iter(|| {
            let mut acc = 0u32;
            for k in hits {
                if let Some(v) = map.get(k) {
                    acc = acc.wrapping_add(*v);
                }
            }
            acc
        })
    });
    group.bench_function(BenchmarkId::new(format!("miss/{hasher}"), param), |b| {
        b.iter(|| {
            let mut acc = 0u32;
            for k in misses {
                if let Some(v) = map.get(k) {
                    acc = acc.wrapping_add(*v);
                }
            }
            acc
        })
    });
}

fn hasher_key<K: RandomKey>(c: &mut Criterion, key: &str) {
    let mut group = c.benchmark_group("hasher");
    for n in [13_000usize, 1_000_000] {
        let mut rng = StdRng::seed_from_u64(0x4841_5348 ^ n as u64);
        let keys = random_keys::<K>(&mut rng, n);
        let hits = sample(&mut rng, &keys, LOOKUPS);
        let misses = fresh_keys(&mut rng, &keys, LOOKUPS);
        let param = format!("{key}/{n}");
        hasher_arm::<K, ahash::RandomState>(&mut group, "ahash", &param, &keys, &hits, &misses);
        hasher_arm::<K, foldhash::fast::RandomState>(
            &mut group,
            "foldhash-fast",
            &param,
            &keys,
            &hits,
            &misses,
        );
        hasher_arm::<K, foldhash::quality::RandomState>(
            &mut group,
            "foldhash-quality",
            &param,
            &keys,
            &hits,
            &misses,
        );
        hasher_arm::<K, rapidhash::fast::RandomState>(
            &mut group,
            "rapidhash",
            &param,
            &keys,
            &hits,
            &misses,
        );
    }
    group.finish();
}

fn bench_hasher(c: &mut Criterion) {
    hasher_key::<Txid>(c, "txid");
    hasher_key::<OutPoint>(c, "outpoint");
    hasher_key::<Nullifier>(c, "nullifier");
}

// ---------------------------------------------------------------------------------------------
// 2. concurrent_map
// ---------------------------------------------------------------------------------------------

struct Payload([u8; 400]);

const RESIDENT: usize = 100_000;
const BLOCK_LOOKUPS: usize = 6_500;
const READERS: usize = 32;
const READER_LOOKUPS: usize = 10_000;
const CHURN: usize = 1_000;

/// The operations hayai needs from a concurrent map, so the three candidates run one code
/// path. A lookup batch is one thread's work on one block: papaya pins its guard once per batch.
trait ConcurrentMap: Sync + Sized {
    const NAME: &'static str;
    fn new() -> Self;
    fn insert(&self, key: WtxId, value: Arc<Payload>);
    fn remove(&self, key: &WtxId);
    /// Looks every key up and returns the number found plus the first payload byte of each.
    fn lookup_batch(&self, keys: &[WtxId]) -> (usize, u32);
}

type DashCandidate = dashmap::DashMap<WtxId, Arc<Payload>, ahash::RandomState>;

impl ConcurrentMap for DashCandidate {
    const NAME: &'static str = "dashmap";
    fn new() -> Self {
        dashmap::DashMap::default()
    }
    fn insert(&self, key: WtxId, value: Arc<Payload>) {
        dashmap::DashMap::insert(self, key, value);
    }
    fn remove(&self, key: &WtxId) {
        dashmap::DashMap::remove(self, key);
    }
    fn lookup_batch(&self, keys: &[WtxId]) -> (usize, u32) {
        let (mut found, mut acc) = (0usize, 0u32);
        for k in keys {
            if let Some(v) = self.get(k) {
                found += 1;
                acc = acc.wrapping_add(v.0[0] as u32);
            }
        }
        (found, acc)
    }
}

type PapayaCandidate = papaya::HashMap<WtxId, Arc<Payload>, ahash::RandomState>;

impl ConcurrentMap for PapayaCandidate {
    const NAME: &'static str = "papaya";
    fn new() -> Self {
        papaya::HashMap::with_hasher(ahash::RandomState::new())
    }
    fn insert(&self, key: WtxId, value: Arc<Payload>) {
        self.pin().insert(key, value);
    }
    fn remove(&self, key: &WtxId) {
        self.pin().remove(key);
    }
    fn lookup_batch(&self, keys: &[WtxId]) -> (usize, u32) {
        let (mut found, mut acc) = (0usize, 0u32);
        let map = self.pin();
        for k in keys {
            if let Some(v) = map.get(k) {
                found += 1;
                acc = acc.wrapping_add(v.0[0] as u32);
            }
        }
        (found, acc)
    }
}

type SccCandidate = scc::HashMap<WtxId, Arc<Payload>, ahash::RandomState>;

impl ConcurrentMap for SccCandidate {
    const NAME: &'static str = "scc";
    fn new() -> Self {
        scc::HashMap::with_hasher(ahash::RandomState::new())
    }
    fn insert(&self, key: WtxId, value: Arc<Payload>) {
        self.insert_sync(key, value)
            .unwrap_or_else(|_| panic!("scc: key already present"));
    }
    fn remove(&self, key: &WtxId) {
        self.remove_sync(key);
    }
    fn lookup_batch(&self, keys: &[WtxId]) -> (usize, u32) {
        let (mut found, mut acc) = (0usize, 0u32);
        for k in keys {
            if let Some(byte) = self.read_sync(k, |_, v| v.0[0]) {
                found += 1;
                acc = acc.wrapping_add(byte as u32);
            }
        }
        (found, acc)
    }
}

struct ConcurrentFixture {
    keys: Vec<WtxId>,
    payloads: Vec<Arc<Payload>>,
    block: Vec<WtxId>,
    reader_probes: Vec<Vec<WtxId>>,
    churn: Vec<WtxId>,
}

impl ConcurrentFixture {
    fn new() -> Self {
        let mut rng = StdRng::seed_from_u64(0x00C0_FFEE);
        let keys = random_keys::<WtxId>(&mut rng, RESIDENT);
        let payloads = (0..RESIDENT)
            .map(|i| Arc::new(Payload([i as u8; 400])))
            .collect();
        let block = sample(&mut rng, &keys, BLOCK_LOOKUPS);
        let churn = fresh_keys(&mut rng, &keys, CHURN);
        let reader_probes = (0..READERS)
            .map(|_| {
                let hits = READER_LOOKUPS * 9 / 10;
                let mut probes = sample(&mut rng, &keys, hits);
                probes.extend(fresh_keys(&mut rng, &keys, READER_LOOKUPS - hits));
                // Interleave hits and misses rather than running them in two blocks.
                for i in (1..probes.len()).rev() {
                    probes.swap(i, rng.gen_range(0..=i));
                }
                probes
            })
            .collect();
        ConcurrentFixture {
            keys,
            payloads,
            block,
            reader_probes,
            churn,
        }
    }
}

fn concurrent_arm<M: ConcurrentMap>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    fx: &ConcurrentFixture,
) {
    let (map, mem) = heap_use(|| {
        let map = M::new();
        for (k, p) in fx.keys.iter().zip(&fx.payloads) {
            map.insert(*k, Arc::clone(p));
        }
        map
    });
    report_mem(
        "concurrent_map",
        M::NAME,
        &format!("{RESIDENT}-entries-excl-payload"),
        &mem,
    );
    assert_eq!(map.lookup_batch(&fx.block).0, BLOCK_LOOKUPS);

    group.bench_function(BenchmarkId::new("lookup_6500", M::NAME), |b| {
        b.iter(|| map.lookup_batch(black_box(&fx.block)))
    });

    let churn_payload = Arc::new(Payload([0xAA; 400]));
    group.bench_function(BenchmarkId::new("mixed_32r_1w", M::NAME), |b| {
        b.iter_custom(|iters| {
            let barrier = Barrier::new(READERS + 2);
            let mut total = Duration::ZERO;
            std::thread::scope(|s| {
                for probes in &fx.reader_probes {
                    let (barrier, map) = (&barrier, &map);
                    s.spawn(move || {
                        for _ in 0..iters {
                            barrier.wait();
                            black_box(map.lookup_batch(probes));
                            barrier.wait();
                        }
                    });
                }
                {
                    let (barrier, map, churn, payload) =
                        (&barrier, &map, &fx.churn, &churn_payload);
                    s.spawn(move || {
                        for _ in 0..iters {
                            barrier.wait();
                            for k in churn {
                                map.insert(*k, Arc::clone(payload));
                            }
                            for k in churn {
                                map.remove(k);
                            }
                            barrier.wait();
                        }
                    });
                }
                for _ in 0..iters {
                    barrier.wait();
                    let start = Instant::now();
                    barrier.wait();
                    total += start.elapsed();
                }
            });
            total
        })
    });
}

fn bench_concurrent_map(c: &mut Criterion) {
    let fx = ConcurrentFixture::new();
    let mut group = c.benchmark_group("concurrent_map");
    group.sample_size(30);
    concurrent_arm::<DashCandidate>(&mut group, &fx);
    concurrent_arm::<PapayaCandidate>(&mut group, &fx);
    concurrent_arm::<SccCandidate>(&mut group, &fx);
    group.finish();
}

// ---------------------------------------------------------------------------------------------
// 3. smallvec
// ---------------------------------------------------------------------------------------------

type Item = (u8, [u8; 32]);
type Small4 = SmallVec<[Item; 4]>;

fn bench_smallvec(c: &mut Criterion) {
    eprintln!(
        "[size] size_of Vec<(u8,[u8;32])> = {} B, SmallVec<[(u8,[u8;32]);4]> = {} B",
        std::mem::size_of::<Vec<Item>>(),
        std::mem::size_of::<Small4>()
    );
    let items: Vec<Item> = (0..8u8).map(|i| (i, [i; 32])).collect();
    let mut group = c.benchmark_group("smallvec");
    for len in [0usize, 1, 2, 4, 8] {
        let src = &items[..len];
        let (_, vec_mem) = heap_use(|| {
            let mut v: Vec<Item> = Vec::new();
            for item in black_box(src) {
                v.push(*item);
            }
            black_box(v)
        });
        let (_, small_mem) = heap_use(|| {
            let mut v: Small4 = SmallVec::new();
            for item in black_box(src) {
                v.push(*item);
            }
            black_box(v)
        });
        eprintln!(
            "[allocs] smallvec/{len}: Vec {} allocs ({} B), SmallVec<4> {} allocs ({} B)",
            vec_mem.count, vec_mem.allocated, small_mem.count, small_mem.allocated
        );
        group.bench_function(BenchmarkId::new("vec", len), |b| {
            b.iter(|| {
                let mut v: Vec<Item> = Vec::new();
                for item in black_box(src) {
                    v.push(*item);
                }
                v.iter().map(|e| e.1[3] as u32).sum::<u32>()
            })
        });
        group.bench_function(BenchmarkId::new("smallvec4", len), |b| {
            b.iter(|| {
                let mut v: Small4 = SmallVec::new();
                for item in black_box(src) {
                    v.push(*item);
                }
                v.iter().map(|e| e.1[3] as u32).sum::<u32>()
            })
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------------------------
// 4. layer_sets
// ---------------------------------------------------------------------------------------------

const LAYER_SIZE: usize = 13_000;
const LAYERS: usize = 100;
const LAYER_HITS: usize = LOOKUPS / 100;

type PackedOutPoint = [u8; 36];

fn pack(op: &OutPoint) -> PackedOutPoint {
    let mut out = [0u8; 36];
    out[..32].copy_from_slice(&op.hash);
    out[32..].copy_from_slice(&op.n.to_le_bytes());
    out
}

/// One layer layout: built from an unsorted block's worth of outpoints, probed by key.
trait Layer: Sized {
    const NAME: &'static str;
    fn build(unsorted: &[OutPoint]) -> Self;
    fn contains(&self, key: &OutPoint) -> bool;
}

type HashLayer = HashSet<OutPoint, ahash::RandomState>;

impl Layer for HashLayer {
    const NAME: &'static str = "ahash-set";
    fn build(unsorted: &[OutPoint]) -> Self {
        let mut set = HashSet::with_capacity_and_hasher(unsorted.len(), ahash::RandomState::new());
        set.extend(unsorted.iter().copied());
        set
    }
    fn contains(&self, key: &OutPoint) -> bool {
        HashSet::contains(self, key)
    }
}

struct SortedLayer(Vec<OutPoint>);

impl Layer for SortedLayer {
    const NAME: &'static str = "sorted-vec";
    fn build(unsorted: &[OutPoint]) -> Self {
        let mut v = unsorted.to_vec();
        v.sort_unstable();
        SortedLayer(v)
    }
    fn contains(&self, key: &OutPoint) -> bool {
        self.0.binary_search(key).is_ok()
    }
}

struct PackedLayer(Vec<PackedOutPoint>);

impl Layer for PackedLayer {
    const NAME: &'static str = "sorted-packed";
    fn build(unsorted: &[OutPoint]) -> Self {
        let mut v: Vec<PackedOutPoint> = unsorted.iter().map(pack).collect();
        v.sort_unstable();
        PackedLayer(v)
    }
    fn contains(&self, key: &OutPoint) -> bool {
        self.0.binary_search(&pack(key)).is_ok()
    }
}

fn walk<L: Layer>(layers: &[L], probes: &[OutPoint]) -> usize {
    let mut hits = 0;
    for key in probes {
        if layers.iter().rev().any(|l| l.contains(key)) {
            hits += 1;
        }
    }
    hits
}

fn layer_arm<L: Layer>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    unsorted: &[Vec<OutPoint>],
    probes: &[OutPoint],
) {
    let (_, mem) = heap_use(|| black_box(L::build(&unsorted[0])));
    report_mem(
        "layer_sets",
        L::NAME,
        &format!("one-layer-{LAYER_SIZE}"),
        &mem,
    );
    group.bench_function(BenchmarkId::new("build", L::NAME), |b| {
        b.iter(|| L::build(black_box(&unsorted[0])))
    });
    let layers: Vec<L> = unsorted.iter().map(|u| L::build(u)).collect();
    assert_eq!(walk(&layers, probes), LAYER_HITS);
    group.bench_function(BenchmarkId::new("walk", L::NAME), |b| {
        b.iter(|| walk(&layers, black_box(probes)))
    });
}

fn bench_layer_sets(c: &mut Criterion) {
    let mut rng = StdRng::seed_from_u64(0x001A_7E75);
    let all = random_keys::<OutPoint>(&mut rng, LAYER_SIZE * LAYERS);
    let unsorted: Vec<Vec<OutPoint>> = all.chunks(LAYER_SIZE).map(|c| c.to_vec()).collect();
    let mut probes = fresh_keys(&mut rng, &all, LOOKUPS - LAYER_HITS);
    probes.extend(sample(&mut rng, &all, LAYER_HITS));
    for i in (1..probes.len()).rev() {
        probes.swap(i, rng.gen_range(0..=i));
    }
    let mut group = c.benchmark_group("layer_sets");
    group.sample_size(30);
    layer_arm::<HashLayer>(&mut group, &unsorted, &probes);
    layer_arm::<SortedLayer>(&mut group, &unsorted, &probes);
    layer_arm::<PackedLayer>(&mut group, &unsorted, &probes);
    group.finish();
}

// ---------------------------------------------------------------------------------------------
// 5. multitable
// ---------------------------------------------------------------------------------------------

const SHORT_ID_LOOKUPS: usize = 2_000;
type ShortValue = [u8; 64];
type ShortMap = ahash::AHashMap<ShortId, Option<ShortValue>>;
#[cfg(feature = "multitable")]
type ShortTable = MultiTableFiltered<6, 64, 8, ahash::RandomState>;

/// The short-id index under test, built once per block from the store.
trait ShortIndex: Sized {
    fn build(keys: &[ShortId], values: &[ShortValue]) -> Self;
    fn get_first_byte(&self, key: &ShortId) -> Option<u8>;
}

impl ShortIndex for ShortMap {
    fn build(keys: &[ShortId], values: &[ShortValue]) -> Self {
        let mut map = ahash::AHashMap::with_capacity(keys.len());
        for (k, v) in keys.iter().zip(values) {
            map.insert(*k, Some(*v));
        }
        map
    }
    fn get_first_byte(&self, key: &ShortId) -> Option<u8> {
        self.get(key).and_then(|v| v.map(|v| v[0]))
    }
}

#[cfg(feature = "multitable")]
struct Table(ShortTable);

/// Builds that hit `InsertError::Full` and were retried with a fresh random hasher. The table
/// never grows, so an unlucky seed (about one build in 10,000 at 100,000 keys) has to be
/// rebuilt; that rebuild is part of what a user of the table pays and stays in the timed loop.
#[cfg(feature = "multitable")]
static MULTITABLE_REBUILDS: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "multitable")]
impl Table {
    fn build_with(keys: &[ShortId], values: &[ShortValue], utilization: f64) -> Self {
        for _ in 0..8 {
            let mut table = ShortTable::with_capacity(keys.len() as u64, utilization);
            let outcome = keys
                .iter()
                .zip(values)
                .try_for_each(|(k, v)| table.insert(*k, *v));
            match outcome {
                Ok(()) => return Table(table),
                Err(InsertError::Full) => {
                    MULTITABLE_REBUILDS.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => panic!("multitable insert at utilization {utilization}: {e}"),
            }
        }
        panic!("multitable at utilization {utilization}: full on 8 consecutive random seeds");
    }
}

#[cfg(feature = "multitable")]
struct Table77(Table);
#[cfg(feature = "multitable")]
struct Table90(Table);

#[cfg(feature = "multitable")]
impl ShortIndex for Table77 {
    fn build(keys: &[ShortId], values: &[ShortValue]) -> Self {
        Table77(Table::build_with(keys, values, 0.77))
    }
    fn get_first_byte(&self, key: &ShortId) -> Option<u8> {
        self.0 .0.get(key).map(|v| v[0])
    }
}

#[cfg(feature = "multitable")]
impl ShortIndex for Table90 {
    fn build(keys: &[ShortId], values: &[ShortValue]) -> Self {
        Table90(Table::build_with(keys, values, 0.90))
    }
    fn get_first_byte(&self, key: &ShortId) -> Option<u8> {
        self.0 .0.get(key).map(|v| v[0])
    }
}

fn short_index_arm<T: ShortIndex>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    keys: &[ShortId],
    values: &[ShortValue],
    hits: &[ShortId],
    misses: &[ShortId],
) {
    let n = keys.len();
    let (table, mem) = heap_use(|| T::build(keys, values));
    report_mem("multitable", name, &n.to_string(), &mem);
    assert_eq!(
        hits.iter()
            .filter(|k| table.get_first_byte(k).is_some())
            .count(),
        hits.len()
    );
    assert_eq!(
        misses
            .iter()
            .filter(|k| table.get_first_byte(k).is_some())
            .count(),
        0
    );
    group.bench_function(BenchmarkId::new(format!("build/{name}"), n), |b| {
        b.iter(|| T::build(black_box(keys), black_box(values)))
    });
    let lookups = |probes: &[ShortId]| {
        let mut acc = 0u32;
        for k in probes {
            if let Some(byte) = table.get_first_byte(k) {
                acc = acc.wrapping_add(byte as u32);
            }
        }
        acc
    };
    group.bench_function(BenchmarkId::new(format!("hit/{name}"), n), |b| {
        b.iter(|| lookups(black_box(hits)))
    });
    group.bench_function(BenchmarkId::new(format!("miss/{name}"), n), |b| {
        b.iter(|| lookups(black_box(misses)))
    });
}

fn bench_multitable(c: &mut Criterion) {
    let mut group = c.benchmark_group("multitable");
    for n in [10_000usize, 100_000] {
        let mut rng = StdRng::seed_from_u64(0x5A0D ^ n as u64);
        let keys = random_keys::<ShortId>(&mut rng, n);
        let values: Vec<ShortValue> = (0..n)
            .map(|_| {
                let mut v = [0u8; 64];
                rng.fill_bytes(&mut v);
                v
            })
            .collect();
        let hits = sample(&mut rng, &keys, SHORT_ID_LOOKUPS);
        let misses = fresh_keys(&mut rng, &keys, SHORT_ID_LOOKUPS);
        short_index_arm::<ShortMap>(&mut group, "ahash-map", &keys, &values, &hits, &misses);
        #[cfg(feature = "multitable")]
        short_index_arm::<Table77>(
            &mut group,
            "multitable-0.77",
            &keys,
            &values,
            &hits,
            &misses,
        );
        #[cfg(feature = "multitable")]
        short_index_arm::<Table90>(
            &mut group,
            "multitable-0.90",
            &keys,
            &values,
            &hits,
            &misses,
        );
    }
    group.finish();
    #[cfg(feature = "multitable")]
    eprintln!(
        "[multitable] builds retried after InsertError::Full: {}",
        MULTITABLE_REBUILDS.load(Ordering::Relaxed)
    );
}

criterion_group!(
    benches,
    bench_hasher,
    bench_concurrent_map,
    bench_smallvec,
    bench_layer_sets,
    bench_multitable
);
criterion_main!(benches);
