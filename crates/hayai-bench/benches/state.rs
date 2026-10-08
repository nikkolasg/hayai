//! Benchmark: hayai-state against Zakura's chain clone (`hayai_bench::zakura_chain_clone`).
//!
//! Groups:
//! - `state/push_block/<window blocks>`: committing one block to a chain whose window holds
//!   that many blocks of the audit's per-block shape (50 nullifiers per pool, 1,000 spent
//!   outpoints, 1,000 transactions). `hayai-layer` builds the block's layer maps, pushes it,
//!   finalizes the oldest layer into the coins cache and takes a view; `zakura-clone` deep
//!   clones the chain's index maps, inserts the block and drops the oldest, as Zakura's
//!   `Chain` does on every push while a snapshot is alive.
//! - `state/contextual_check/<fixture>`: every contextual rule of the fixture block plus its
//!   layer, against a one-block chain.
//! - `state/lookup_through_window/<layers>`: 13,000 coin lookups through a window of that
//!   many typical layers (`BlockShape::TYPICAL`), 90 % of the keys in the base and 10 %
//!   created inside the window: `hayai-index` is the window index, `hayai-walk` the
//!   newest-first walk of the layers it replaced (`ChainView::get_coins_by_walk`).
//! - `state/commit_prebuilt/<fixture>`: commit of a fixture block whose body the store
//!   holds, with a known history tree. `hayai-swap`: `commit_prebuilt` on a body prebuilt
//!   before the block (the header and coinbase rules, then the prebuilt layer).
//!   `hayai-warm`: `validate_block` with a warm store, today's path. `hayai-prebuild`: the
//!   prebuild itself, which a node pays once per template change (own template) or per
//!   candidate (peers' lanes).

hayai_bench::bench_allocator!();

use std::sync::Arc;

use bytes::Bytes;
use criterion::BatchSize;
use criterion::{
    criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode, Throughput,
};
use hayai_bench::chain_fixture::{harness, harness_with_history};
use hayai_bench::hayai_id;
use hayai_bench::scenarios::state::{new_chain, push};
use hayai_bench::zakura_chain_clone::{BlockEntries, BlockShape, ZakuraChainClone};
use hayai_bench::zebra_chain_clone::{ZebraBlockEntries, ZebraChainClone};
use hayai_coins::{Coin, CoinsView, OutPoint};
use hayai_fixtures::standard_set;
use hayai_prepared::{prepare, ScopedBatch};
use hayai_state::{contextual_check, CheckConfig, PreparedBlock};
use hayai_validate::{commit_prebuilt, prebuild, validate_block};
use hayai_wire::WtxId;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;

const WINDOWS: [usize; 2] = [100, 1_000];

fn push_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("state/push_block");
    group.sampling_mode(SamplingMode::Flat);
    for window in WINDOWS {
        let shape = BlockShape::AUDIT;

        let mut chain = new_chain();
        for _ in 0..window {
            push(&mut chain, shape, window);
        }
        group.bench_function(BenchmarkId::new(hayai_id("layer"), window), |b| {
            b.iter(|| push(&mut chain, shape, window))
        });

        let mut zakura = ZakuraChainClone::with_blocks(window, shape);
        group.sample_size(10);
        group.bench_function(BenchmarkId::new("zakura-clone", window), |b| {
            b.iter(|| {
                let entries = BlockEntries::synthetic(zakura.next_height(), shape);
                zakura.push_block(entries, window);
                zakura.len()
            })
        });
    }
    group.finish();
}

/// `state/push_block/1000` id `zebra-clone`: Zebra's two deep clones per push of a full
/// window (`hayai_bench::zebra_chain_clone`), with the audit's block shape.
fn zebra_push_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("state/push_block");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(10);
    let window = 1_000;
    let shape = BlockShape::AUDIT;
    let mut zebra = ZebraChainClone::with_blocks(window, shape);
    group.bench_function(BenchmarkId::new("zebra-clone", window), |b| {
        b.iter(|| {
            let entries = ZebraBlockEntries::synthetic(zebra.next_height(), shape);
            zebra.push_block(entries, window);
            zebra.len()
        })
    });
    group.finish();
}

fn contextual(c: &mut Criterion) {
    let mut group = c.benchmark_group("state/contextual_check");
    for fixture in standard_set() {
        let h = harness(&fixture);
        let view = h.chain.view();
        let mut batch = ScopedBatch::new(&h.cfg.keys);
        let coinbase =
            prepare(h.block.txs[0].clone(), h.cfg.epoch(), &view, &mut batch).expect("valid");
        let mut txs = vec![Arc::new(coinbase)];
        txs.extend(h.prepare_all());
        let block = PreparedBlock::new(h.block.clone(), txs);
        let cfg = CheckConfig {
            network: h.cfg.network,
            rules: &h.cfg.rules,
        };
        group.bench_function(BenchmarkId::new(hayai_id(""), &fixture.name), |b| {
            b.iter(|| {
                contextual_check(&view, &block, &cfg)
                    .expect("valid")
                    .layer
                    .height
            })
        });
    }
    group.finish();
}

const LOOKUPS: usize = 13_000;
const LOOKUP_WINDOW: usize = 100;

fn lookup_through_window(c: &mut Criterion) {
    let mut group = c.benchmark_group("state/lookup_through_window");
    let shape = BlockShape::TYPICAL;
    let mut chain = new_chain();
    // The base holds the keys the window does not: 90 % of the lookups.
    let base_keys: Vec<OutPoint> = (0..LOOKUPS * 9 / 10)
        .map(|i| {
            let mut txid = [0xb0u8; 32];
            txid[..8].copy_from_slice(&(i as u64).to_le_bytes());
            OutPoint::new(txid, 0)
        })
        .collect();
    {
        let mut base = chain.base().write();
        for outpoint in &base_keys {
            base.coins
                .add(
                    outpoint.clone(),
                    Coin {
                        value: 1,
                        script_pubkey: Bytes::from_static(&[0x51]),
                        height: 0,
                        is_coinbase: false,
                    },
                )
                .expect("distinct keys");
        }
    }
    for _ in 0..LOOKUP_WINDOW {
        push(&mut chain, shape, LOOKUP_WINDOW);
    }
    let view = chain.view();
    // The other 10 %: coins created across the window, oldest layer first. Each layer
    // spends the coins of the layer before it, so only the newest layer's keys resolve to
    // a coin; the others stop at the spending layer.
    let base_count = base_keys.len();
    let mut keys = base_keys;
    let window_keys = LOOKUPS - keys.len();
    let per_layer = window_keys.div_ceil(LOOKUP_WINDOW);
    for layer in view.layers() {
        keys.extend(layer.created.keys().take(per_layer).cloned());
    }
    keys.truncate(LOOKUPS);
    let mut rng = StdRng::seed_from_u64(13);
    keys.shuffle(&mut rng);
    let expected = view.get_coins_by_walk(&keys);
    assert_eq!(view.get_coins(&keys), expected, "index equals walk");
    let found = expected.iter().flatten().count();
    assert_eq!(
        found,
        base_count + per_layer,
        "base keys and the newest layer's coins"
    );

    group.throughput(Throughput::Elements(LOOKUPS as u64));
    group.bench_function(BenchmarkId::new(hayai_id("index"), LOOKUP_WINDOW), |b| {
        b.iter(|| view.get_coins(&keys).len())
    });
    group.bench_function(BenchmarkId::new(hayai_id("walk"), LOOKUP_WINDOW), |b| {
        b.iter(|| view.get_coins_by_walk(&keys).len())
    });
    group.finish();
}

fn commit_prebuilt_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("state/commit_prebuilt");
    group.sample_size(20);
    for fixture in standard_set() {
        let h = harness_with_history(&fixture);
        h.fill_store();
        let view = h.chain.view();
        let ids: Vec<WtxId> = h.block.txs[1..].iter().map(|t| t.wtxid()).collect();
        // The first validation pulls the coins into the cache, as for the warm numbers.
        validate_block(h.block.clone(), &h.store, &view, &h.cfg).expect("valid");
        group.bench_function(BenchmarkId::new(hayai_id("swap"), &fixture.name), |b| {
            b.iter_batched(
                || prebuild(&ids, &h.store, &view, &h.cfg).expect("prebuilt"),
                |body| {
                    commit_prebuilt(&h.block, body, &view, &h.cfg)
                        .expect("valid")
                        .0
                        .height
                },
                BatchSize::PerIteration,
            )
        });
        group.bench_function(BenchmarkId::new(hayai_id("warm"), &fixture.name), |b| {
            b.iter(|| {
                validate_block(h.block.clone(), &h.store, &view, &h.cfg)
                    .expect("valid")
                    .0
                    .height
            })
        });
        group.bench_function(BenchmarkId::new(hayai_id("prebuild"), &fixture.name), |b| {
            b.iter(|| {
                prebuild(&ids, &h.store, &view, &h.cfg)
                    .expect("prebuilt")
                    .height
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    push_block,
    zebra_push_block,
    contextual,
    lookup_through_window,
    commit_prebuilt_bench
);
criterion_main!(benches);
