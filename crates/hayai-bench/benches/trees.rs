//! Benchmark: hayai-trees against its Zakura baseline. See crates/hayai-bench/src/lib.rs for
//! the shared fixtures and the JSON result writer.
//!
//! Group `trees/orchard_append`, parameter = leaves per block, one block append plus the root
//! per iteration from a frontier at a fixed non-aligned position (per-leaf throughput):
//! - `hayai`: `OrchardFrontier::append_many`.
//! - `zakura`: zakura-chain's `NoteCommitmentTree::append_batch` (its parallel batch frontier)
//!   followed by `root()`.
//! - `upstream`: `Frontier::append` per leaf followed by `Frontier::root`.
//!
//! Group `trees/sapling_append` has `hayai` and `upstream` with the same shape.

hayai_bench::bench_allocator!();

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hayai_bench::scenarios::trees::{
    frontier_at, hayai_append, sapling_leaf, zakura_append, OrchardInputs, START_POSITION,
};
use hayai_bench::{hayai_id, library_id};
use hayai_crypto::rng::{SeedableRng, StdRng};
use hayai_crypto::sapling_crypto;
use hayai_trees::SaplingFrontier;
use sapling_crypto::Node;

const BLOCK_SIZES: [usize; 3] = [64, 330, 2048];

fn orchard(c: &mut Criterion) {
    let OrchardInputs {
        start,
        leaves,
        zakura_start,
        zakura_leaves,
    } = OrchardInputs::new(*BLOCK_SIZES.iter().max().expect("sizes"));

    let mut group = c.benchmark_group("trees/orchard_append");
    for &n in &BLOCK_SIZES {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::new(hayai_id(""), n),
            &leaves[..n],
            |b, leaves| b.iter(|| hayai_append(&start, leaves)),
        );
        group.bench_with_input(
            BenchmarkId::new("zakura", n),
            &zakura_leaves[..n],
            |b, leaves| b.iter(|| zakura_append(&zakura_start, leaves)),
        );
        group.bench_with_input(
            BenchmarkId::new(library_id(), n),
            &leaves[..n],
            |b, leaves| {
                b.iter(|| {
                    let mut frontier = start.clone();
                    for leaf in leaves {
                        assert!(frontier.append(*leaf));
                    }
                    frontier.root()
                })
            },
        );
    }
    group.finish();
}

fn sapling(c: &mut Criterion) {
    let mut rng = StdRng::seed_from_u64(0x5a9);
    let mut leaf = || sapling_leaf(&mut rng);
    let start = frontier_at(START_POSITION, &mut leaf);
    let leaves: Vec<Node> = (0..*BLOCK_SIZES.iter().max().expect("sizes"))
        .map(|_| leaf())
        .collect();

    let mut group = c.benchmark_group("trees/sapling_append");
    for &n in &BLOCK_SIZES {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::new(hayai_id(""), n),
            &leaves[..n],
            |b, leaves| {
                b.iter(|| {
                    let mut frontier = SaplingFrontier::from_frontier(start.clone());
                    frontier.append_many(leaves).expect("fits")
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new(library_id(), n),
            &leaves[..n],
            |b, leaves| {
                b.iter(|| {
                    let mut frontier = start.clone();
                    for leaf in leaves {
                        assert!(frontier.append(*leaf));
                    }
                    frontier.root()
                })
            },
        );
    }
    group.finish();
}

criterion_group!(benches, orchard, sapling);
criterion_main!(benches);
