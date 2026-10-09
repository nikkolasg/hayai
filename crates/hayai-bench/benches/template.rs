//! Benchmark: hayai-template against a faithful port of Zakura's ZIP 317 selection
//! (`hayai_bench::zakura_zip317`).
//!
//! Candidates are synthetic: random sizes, fees, shielded counts and parent links drawn from a
//! fixed seed, so both sides select from the same set. hayai's numbers include building the
//! coinbase and the merkle, auth-data and block-commitment roots, which the Zakura port does not
//! do; its `getblocktemplate` path adds them on top.
//!
//! Groups:
//! - `template/build_from_scratch`: one template from N candidates (1,000 and 8,000).
//! - `template/incremental_add`: one high-fee candidate arrives into a full 8,000-candidate
//!   set; hayai applies the event, Zakura rebuilds.
//! - `template/tip_event`: a new tip with 100 conflicting candidates among 8,000;
//!   `hayai-empty` is the time until `TemplateEmpty` is emitted, `hayai-full` the whole event.
//! - `template/switch_after_block/<fixture>-<cold|warm>`: a fixture block arrives at a node
//!   whose template holds 8,000 candidates on its parent; the time until the full template
//!   on the new block exists. `hayai-serial`: `validate_block`, push, `on_tip`.
//!   `hayai-speculative`: `build_layer`, speculative push, `on_speculative_tip`; `verify`
//!   runs on another thread and is not on the measured path. `cold`: empty prepared store;
//!   `warm`: the store holds the block's transactions.
//! - `template/own_block_commit/<fixture>`: an own block (the store holds its body) from
//!   the block to the full template on it. `hayai-warm`: today's path, `validate_block`,
//!   push, `on_tip`. `hayai-swap`: the body was prebuilt after the template change
//!   (outside the measured path); `commit_prebuilt`, push, `on_tip`. The template holds
//!   8,000 synthetic candidates in both arms.

hayai_bench::bench_allocator!();

use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use hayai_bench::hayai_id;
use hayai_bench::scenarios::template::{
    candidates, coinbase_reserved, coinbase_spec, hayai_build, live_with, tip, zakura_build,
    zakura_limits, Switch, PARAMS,
};
use hayai_bench::zakura_zip317::select_mempool_transactions;
use hayai_bench::zebra_zip317::{self, CoinbaseCache};
use hayai_fixtures::standard_set;
use hayai_template::{Candidate, SetEvent, TemplateUpdate};
use hayai_wire::WtxId;
use rand::rngs::StdRng;
use rand::SeedableRng;

fn build_from_scratch(c: &mut Criterion) {
    let mut group = c.benchmark_group("template/build_from_scratch");
    let reserved = coinbase_reserved();
    for &n in &[1_000usize, 8_000] {
        let cands = candidates(n, 7);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new(hayai_id(""), n), &cands, |b, cands| {
            b.iter_batched(|| cands.clone(), hayai_build, BatchSize::LargeInput)
        });
        group.bench_with_input(BenchmarkId::new("zakura-zip317", n), &cands, |b, cands| {
            let mut rng = StdRng::seed_from_u64(1);
            b.iter_batched(
                || cands.clone(),
                |cands| zakura_build(cands, reserved, &mut rng),
                BatchSize::LargeInput,
            )
        });
    }
    group.finish();
}

/// The candidate that arrives in `template/incremental_add`: no parent, a fee above the
/// weight ratio cap, and the smallest size of 50 generated candidates. The order is weight
/// ratio, then size, so this candidate enters a full template.
fn newcomer() -> Candidate {
    let mut newcomer = candidates(50, 99)
        .into_iter()
        .min_by_key(Candidate::size_bytes)
        .expect("50 candidates");
    newcomer.fee = newcomer.conventional_fee * 2 * u64::from(PARAMS.weight_ratio_cap);
    newcomer.weight_ratio = PARAMS.weight_ratio(newcomer.fee, newcomer.conventional_fee);
    newcomer.unpaid_actions = PARAMS.unpaid_actions(newcomer.fee, newcomer.conventional_fee);
    newcomer.depends_on.clear();
    newcomer
}

fn incremental_add(c: &mut Criterion) {
    let mut group = c.benchmark_group("template/incremental_add");
    let n = 8_000;
    let cands = candidates(n, 7);
    let reserved = coinbase_reserved();
    let newcomer = newcomer();

    group.bench_function(hayai_id(""), |b| {
        b.iter_batched(
            || live_with(cands.clone(), 1),
            |mut live| {
                let update = live.apply(SetEvent::Added(newcomer.clone())).unwrap();
                let Some(TemplateUpdate::Changed(t)) = update else {
                    panic!("a high-fee arrival changes the template");
                };
                t.id
            },
            BatchSize::LargeInput,
        )
    });
    group.bench_function("zakura-zip317", |b| {
        let mut rng = StdRng::seed_from_u64(1);
        b.iter_batched(
            || {
                let mut all = cands.clone();
                all.push(newcomer.clone());
                all
            },
            |all| {
                select_mempool_transactions(all, &PARAMS, zakura_limits(reserved), &mut rng).len()
            },
            BatchSize::LargeInput,
        )
    });
    group.finish();
}

/// The cache of Zebra's zero-fee coinbase for the template height.
fn zebra_coinbase_cache() -> CoinbaseCache {
    CoinbaseCache::new(
        coinbase_spec()
            .build(1, 0)
            .expect("coinbase builds")
            .bytes
            .to_vec(),
    )
}

/// `template/build_from_scratch` and `template/incremental_add` id `zebra-zip317`: the port
/// of Zebra's selection on a coinbase cache hit (`hayai_bench::zebra_zip317`).
fn zebra_selection(c: &mut Criterion) {
    let cache = zebra_coinbase_cache();
    let mut group = c.benchmark_group("template/build_from_scratch");
    for &n in &[1_000usize, 8_000] {
        let cands = candidates(n, 7);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("zebra-zip317", n), &cands, |b, cands| {
            let mut rng = StdRng::seed_from_u64(1);
            b.iter_batched(
                || cands.clone(),
                |cands| {
                    zebra_zip317::select_mempool_transactions(
                        cands,
                        &PARAMS,
                        zakura_limits(0),
                        &cache,
                        &mut rng,
                    )
                    .len()
                },
                BatchSize::LargeInput,
            )
        });
    }
    group.finish();

    let mut group = c.benchmark_group("template/incremental_add");
    let mut all = candidates(8_000, 7);
    all.push(newcomer());
    group.bench_function("zebra-zip317", |b| {
        let mut rng = StdRng::seed_from_u64(1);
        b.iter_batched(
            || all.clone(),
            |all| {
                zebra_zip317::select_mempool_transactions(
                    all,
                    &PARAMS,
                    zakura_limits(0),
                    &cache,
                    &mut rng,
                )
                .len()
            },
            BatchSize::LargeInput,
        )
    });
    group.finish();
}

fn tip_event(c: &mut Criterion) {
    let mut group = c.benchmark_group("template/tip_event");
    let n = 8_000;
    let cands = candidates(n, 7);
    // Conflicts: 100 of the currently selected transactions, so the selection really changes.
    let conflicts: Vec<WtxId> = live_with(cands.clone(), 1)
        .selection()
        .take(100)
        .map(|c| c.wtxid)
        .collect();
    assert_eq!(conflicts.len(), 100);

    group.bench_function(hayai_id("empty"), |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut live = live_with(cands.clone(), 1);
                let start = Instant::now();
                let mut empty_at = None;
                live.on_tip(tip(2), &[], &conflicts, |u| {
                    if let TemplateUpdate::Empty(_) = u {
                        empty_at = Some(start.elapsed());
                    }
                })
                .unwrap();
                total += empty_at.expect("on_tip emits TemplateEmpty");
            }
            total
        })
    });
    group.bench_function(hayai_id("full"), |b| {
        b.iter_batched(
            || live_with(cands.clone(), 1),
            |mut live| {
                let mut full = None;
                live.on_tip(tip(2), &[], &conflicts, |u| {
                    if let TemplateUpdate::Full(t) = u {
                        full = Some(t);
                    }
                })
                .unwrap();
                full.expect("on_tip emits TemplateFull").txs.len()
            },
            BatchSize::LargeInput,
        )
    });
    group.finish();
}

/// The fixtures of `switch_after_block`: the heaviest Orchard, transparent and mixed blocks.
const SWITCH_FIXTURES: [&str; 3] = ["orchard-165x2", "transparent-6500x1", "mixed-2000x1-100x2"];

fn switch_after_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("template/switch_after_block");
    group.sample_size(10);
    for fixture in standard_set()
        .into_iter()
        .filter(|f| SWITCH_FIXTURES.contains(&f.name.as_str()))
    {
        for warm in [false, true] {
            let param = format!("{}-{}", fixture.name, if warm { "warm" } else { "cold" });
            let mut switch = Switch::new(&fixture, warm);
            group.bench_function(BenchmarkId::new(hayai_id("serial"), &param), |b| {
                b.iter_custom(|iters| (0..iters).map(|_| switch.serial()).sum())
            });
            group.bench_function(BenchmarkId::new(hayai_id("speculative"), &param), |b| {
                b.iter_custom(|iters| (0..iters).map(|_| switch.speculative()).sum())
            });
        }
    }
    group.finish();
}

fn own_block_commit(c: &mut Criterion) {
    let mut group = c.benchmark_group("template/own_block_commit");
    group.sample_size(20);
    for fixture in standard_set() {
        let mut switch = Switch::new(&fixture, true);
        group.bench_function(BenchmarkId::new(hayai_id("warm"), &fixture.name), |b| {
            b.iter_custom(|iters| (0..iters).map(|_| switch.serial()).sum())
        });
        group.bench_function(BenchmarkId::new(hayai_id("swap"), &fixture.name), |b| {
            b.iter_custom(|iters| (0..iters).map(|_| switch.swap()).sum())
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    build_from_scratch,
    incremental_add,
    zebra_selection,
    tip_event,
    switch_after_block,
    own_block_commit
);
criterion_main!(benches);
