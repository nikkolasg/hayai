//! Benchmark: hayai-sinsemilla against its Zakura baseline. See crates/hayai-bench/src/lib.rs for
//! the shared fixtures and the JSON result writer.
//!
//! Group `sinsemilla/merkle_crh`, one MerkleCRH^Orchard hash per element:
//! - `hayai`: scalar path (`merkle_crh_orchard`).
//! - `hayai-lanes/<n>`: lane kernel on one thread with `n` lanes (`merkle_crh_orchard_lanes`).
//! - `hayai-many/<n>`: `merkle_crh_orchard_many`, rayon pool, policy chooses the evaluator.
//! - `zakura`: Zakura's node `combine` (zakura-chain's `Node`, weighted evaluator, scalar).
//! - `zakura-batch/<n>`: zakura-sinsemilla's `hash_words_batch` with `n` messages.
//! - `upstream`: `orchard::tree::MerkleHashOrchard::combine`.
//!
//! Group `sinsemilla/invert`, one Pallas base field inversion per element: `hayai`
//! (`invert_vartime`, Bernstein–Yang) and `upstream` (`Fp::invert`, square-and-multiply).

hayai_bench::bench_allocator!();

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use ff::{Field, PrimeField};
use hayai_bench::{hayai_id, library_id};
use hayai_crypto::rng::{SeedableRng, StdRng};
use hayai_crypto::{ff, incrementalmerkletree, orchard, pasta_curves};
use incrementalmerkletree::{Hashable, Level};

use hayai_sinsemilla::{
    invert_vartime, merkle_crh_orchard, merkle_crh_orchard_lanes, merkle_crh_orchard_many,
    merkle_crh_words, WORDS,
};
use pasta_curves::pallas;

const LANE_COUNTS: [usize; 3] = [32, 256, 4096];
const LEVEL: u8 = 0;

fn random_pairs(n: usize) -> Vec<(pallas::Base, pallas::Base)> {
    let mut rng = StdRng::seed_from_u64(0x5e5e);
    (0..n)
        .map(|_| {
            (
                pallas::Base::random(&mut rng),
                pallas::Base::random(&mut rng),
            )
        })
        .collect()
}

fn zakura_node(x: &pallas::Base) -> zk_chain::orchard::tree::Node {
    // The zakura stack uses ff 0.14, re-exported through its pasta fork.
    use zk_pasta::group::ff::PrimeField as _;
    zk_pasta::pallas::Base::from_repr(x.to_repr())
        .expect("canonical field element")
        .into()
}

fn upstream_node(x: &pallas::Base) -> orchard::tree::MerkleHashOrchard {
    orchard::tree::MerkleHashOrchard::from_bytes(&x.to_repr()).expect("canonical field element")
}

fn bench(c: &mut Criterion) {
    let pairs = random_pairs(*LANE_COUNTS.iter().max().expect("lane counts"));
    let mut group = c.benchmark_group("sinsemilla/merkle_crh");
    group.throughput(Throughput::Elements(1));

    // The scalar ids cycle through distinct inputs so that table lookups are not all L1 hits.
    let mut next = 0usize;
    group.bench_function(hayai_id(""), |b| {
        b.iter(|| {
            let (left, right) = pairs[next % pairs.len()];
            next += 1;
            merkle_crh_orchard(LEVEL, left, right)
        })
    });

    for &n in &LANE_COUNTS {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::new(hayai_id("lanes"), n),
            &pairs[..n],
            |b, pairs| b.iter(|| merkle_crh_orchard_lanes(LEVEL, pairs)),
        );
        group.bench_with_input(
            BenchmarkId::new(hayai_id("many"), n),
            &pairs[..n],
            |b, pairs| b.iter(|| merkle_crh_orchard_many(LEVEL, pairs)),
        );
    }

    group.throughput(Throughput::Elements(1));
    let zakura_pairs: Vec<_> = pairs
        .iter()
        .map(|(l, r)| (zakura_node(l), zakura_node(r)))
        .collect();
    let mut next = 0usize;
    group.bench_function("zakura", |b| {
        b.iter(|| {
            let (l, r) = &zakura_pairs[next % zakura_pairs.len()];
            next += 1;
            zk_chain::orchard::tree::Node::combine(Level::from(LEVEL), l, r)
        })
    });

    let weighted = zk_sinsemilla::weighted::UncheckedFixedLengthHashDomain::<WORDS>::new(
        &zk_sinsemilla::HashDomain::new("z.cash:Orchard-MerkleCRH"),
    );
    let messages: Vec<[u16; WORDS]> = pairs
        .iter()
        .map(|(l, r)| merkle_crh_words(LEVEL, l, r))
        .collect();
    for &n in &LANE_COUNTS {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::new("zakura-batch", n),
            &messages[..n],
            |b, messages| b.iter(|| weighted.hash_words_batch(messages)),
        );
    }

    group.throughput(Throughput::Elements(1));
    let upstream_pairs: Vec<_> = pairs
        .iter()
        .map(|(l, r)| (upstream_node(l), upstream_node(r)))
        .collect();
    let mut next = 0usize;
    group.bench_function(library_id(), |b| {
        b.iter(|| {
            let (l, r) = &upstream_pairs[next % upstream_pairs.len()];
            next += 1;
            orchard::tree::MerkleHashOrchard::combine(Level::from(LEVEL), l, r)
        })
    });
    group.finish();
}

fn invert(c: &mut Criterion) {
    let values: Vec<pallas::Base> = random_pairs(1024).into_iter().map(|(l, _)| l).collect();
    let mut group = c.benchmark_group("sinsemilla/invert");
    group.throughput(Throughput::Elements(1));
    let mut next = 0usize;
    group.bench_function(hayai_id(""), |b| {
        b.iter(|| {
            let x = &values[next % values.len()];
            next += 1;
            invert_vartime(x)
        })
    });
    let mut next = 0usize;
    group.bench_function(library_id(), |b| {
        b.iter(|| {
            let x = &values[next % values.len()];
            next += 1;
            x.invert()
        })
    });
    group.finish();
}

criterion_group!(benches, bench, invert);
criterion_main!(benches);
