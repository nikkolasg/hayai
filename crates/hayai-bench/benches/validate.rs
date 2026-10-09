//! Benchmark: hayai-validate. See crates/hayai-bench/src/lib.rs for the shared fixtures.
//!
//! Group `validate/block/<fixture>`, one full block validation per iteration (parsed block
//! in, layer out) on a chain seeded with the fixture's funding set, and
//! `validate/block_windowed/<fixture>`, the same on a chain whose window holds 100 layers of
//! typical shape:
//! - `hayai-cold`: empty prepared store, every transaction prepared during validation;
//! - `hayai-warm`: the store holds every transaction (mempool-style preparation beforehand),
//!   so validation is contextual checks plus the coinbase;
//! - `hayai-checkpoint` (`validate/block` only): the checkpoint path
//!   (`hayai_validate::apply_checkpointed`), which runs no script and no proof. The block
//!   is the only checkpoint of the list;
//! - `zakura-model-cold` (transparent fixtures only): the same cryptographic work scheduled
//!   the way Zakura's pipeline schedules it, see
//!   `hayai_bench::scenarios::validate::zakura_model`. It is a model built from hayai's
//!   primitives, not Zakura's code: Zakura's verifier is a tower service graph that cannot be
//!   driven as a library without its state service.
//!
//! The stage breakdown (`Timings`) of one cold and one warm run per fixture is printed at the
//! end of the run.

hayai_bench::bench_allocator!();

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode};
use hayai_bench::chain_fixture::{chain_with_layers, harness};
use hayai_bench::hayai_id;
use hayai_bench::scenarios::validate::{has_shielded, zakura_model, zebra_model};
use hayai_bench::zakura_chain_clone::BlockShape;
use hayai_consensus::Checkpoints;
use hayai_fixtures::{standard_set, Fixture};
use hayai_validate::{apply_checkpointed, validate_block, Timings};

fn print_timings(name: &str, variant: &str, t: &Timings) {
    println!(
        "{name} {variant}: total {:.2?} | roots {:.2?} | lookup {:.2?} | prepare {:.2?} | scripts {:.2?} | shielded {:.2?} | context {:.2?} | trees {:.2?} | known {} unknown {}",
        t.total,
        t.roots,
        t.lookup,
        t.prepare_unknown,
        t.scripts,
        t.shielded,
        t.context,
        t.trees,
        t.known,
        t.unknown
    );
}

fn block(c: &mut Criterion) {
    let fixtures: Vec<Fixture> = standard_set();
    let mut breakdown: Vec<(String, &'static str, Timings)> = Vec::new();
    let mut group = c.benchmark_group("validate/block");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for fixture in &fixtures {
        let cold = harness(fixture);
        let view = cold.chain.view();
        // The first run builds the Orchard verifying key and pulls the coins into the cache;
        // the second is the reported breakdown.
        validate_block(cold.block.clone(), &cold.store, &view, &cold.cfg).expect("valid");
        let (_, t) =
            validate_block(cold.block.clone(), &cold.store, &view, &cold.cfg).expect("valid");
        breakdown.push((fixture.name.clone(), "cold", t));
        group.bench_function(BenchmarkId::new(hayai_id("cold"), &fixture.name), |b| {
            b.iter(|| {
                validate_block(cold.block.clone(), &cold.store, &view, &cold.cfg)
                    .expect("valid")
                    .0
                    .height
            })
        });

        let hash = cold.block.hash();
        let checkpoints = Checkpoints::new(vec![(fixture.height, hash)]).expect("one height");
        let (_, t) = apply_checkpointed(&cold.block, hash, &view, &cold.cfg, &checkpoints)
            .expect("the block is the checkpoint");
        breakdown.push((fixture.name.clone(), "checkpoint", t));
        group.bench_function(
            BenchmarkId::new(hayai_id("checkpoint"), &fixture.name),
            |b| {
                b.iter(|| {
                    apply_checkpointed(&cold.block, hash, &view, &cold.cfg, &checkpoints)
                        .expect("the block is the checkpoint")
                        .0
                        .height
                })
            },
        );

        let warm = harness(fixture);
        warm.fill_store();
        let view = warm.chain.view();
        let (_, t) =
            validate_block(warm.block.clone(), &warm.store, &view, &warm.cfg).expect("valid");
        breakdown.push((fixture.name.clone(), "warm", t));
        group.bench_function(BenchmarkId::new(hayai_id("warm"), &fixture.name), |b| {
            b.iter(|| {
                validate_block(warm.block.clone(), &warm.store, &view, &warm.cfg)
                    .expect("valid")
                    .0
                    .height
            })
        });

        if !has_shielded(&cold.block) {
            group.bench_function(BenchmarkId::new("zakura-model-cold", &fixture.name), |b| {
                b.iter(|| zakura_model(&cold.block, &cold).height)
            });
        }
    }
    group.finish();
    println!("\nvalidate/block stage breakdown (one run each):");
    for (name, variant, t) in &breakdown {
        print_timings(name, variant, t);
    }
}

/// `validate/block/<fixture>` id `zebra-model-cold` (transparent fixtures only): the same
/// cryptographic work scheduled the way upstream Zebra schedules it, see
/// `hayai_bench::scenarios::validate::zebra_model`. It is a model, not Zebra's code.
fn zebra_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("validate/block");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for fixture in &standard_set() {
        let cold = harness(fixture);
        if has_shielded(&cold.block) {
            continue;
        }
        group.bench_function(BenchmarkId::new("zebra-model-cold", &fixture.name), |b| {
            b.iter(|| zebra_model(&cold.block, &cold).height)
        });
    }
    group.finish();
}

/// Layers between the base and the fixture block in `block_windowed`: the chain's window.
const WINDOW_LAYERS: usize = 100;

/// `validate/block_windowed/<fixture>`: as `validate/block`, on a chain whose window holds
/// 100 layers of typical shape, so every input and nullifier lookup crosses the window.
fn block_windowed(c: &mut Criterion) {
    let fixtures: Vec<Fixture> = standard_set();
    let mut breakdown: Vec<(String, &'static str, Timings)> = Vec::new();
    let mut group = c.benchmark_group("validate/block_windowed");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for fixture in &fixtures {
        let cold = chain_with_layers(fixture, WINDOW_LAYERS, BlockShape::TYPICAL);
        let view = cold.chain.view();
        validate_block(cold.block.clone(), &cold.store, &view, &cold.cfg).expect("valid");
        let (_, t) =
            validate_block(cold.block.clone(), &cold.store, &view, &cold.cfg).expect("valid");
        breakdown.push((fixture.name.clone(), "cold", t));
        group.bench_function(BenchmarkId::new(hayai_id("cold"), &fixture.name), |b| {
            b.iter(|| {
                validate_block(cold.block.clone(), &cold.store, &view, &cold.cfg)
                    .expect("valid")
                    .0
                    .height
            })
        });

        let warm = chain_with_layers(fixture, WINDOW_LAYERS, BlockShape::TYPICAL);
        warm.fill_store();
        let view = warm.chain.view();
        let (_, t) =
            validate_block(warm.block.clone(), &warm.store, &view, &warm.cfg).expect("valid");
        breakdown.push((fixture.name.clone(), "warm", t));
        group.bench_function(BenchmarkId::new(hayai_id("warm"), &fixture.name), |b| {
            b.iter(|| {
                validate_block(warm.block.clone(), &warm.store, &view, &warm.cfg)
                    .expect("valid")
                    .0
                    .height
            })
        });
    }
    group.finish();
    println!("\nvalidate/block_windowed stage breakdown (one run each, {WINDOW_LAYERS} layers):");
    for (name, variant, t) in &breakdown {
        print_timings(name, variant, t);
    }
}

criterion_group!(benches, block, zebra_block, block_windowed);
criterion_main!(benches);
