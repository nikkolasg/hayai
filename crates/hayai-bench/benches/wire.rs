//! Benchmark: hayai-wire against its Zakura baseline (`zakura-chain`).
//!
//! Groups: `wire/parse_block` (hayai: boundary scan then parallel parse with retained bytes,
//! txids and auth digests; hayai-sequential: the one-pass reference implementation; zakura:
//! deserialize, hash every transaction, serialize again), `wire/tx_wire_len` (the boundary
//! scanner alone over every transaction of the block), `wire/merkle_root` and
//! `wire/auth_data_root` (hayai: rayon-parallel trees; zakura: `merkle::Root` and
//! `merkle::AuthDataRoot`). Parameters are the fixture names of
//! `hayai_fixtures::standard_set`.

hayai_bench::bench_allocator!();

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hayai_bench::hayai_id;
use hayai_bench::{zakura_wire, zebra_wire};
use hayai_fixtures as fixtures;

fn parse_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire/parse_block");
    group
        .sample_size(20)
        .measurement_time(Duration::from_secs(8));
    for fixture in fixtures::standard_set() {
        group.throughput(Throughput::Bytes(fixture.bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::new(hayai_id(""), &fixture.name),
            &fixture,
            |b, f| b.iter(|| hayai_wire::RawBlock::parse(f.bytes.clone(), f.branch_id).unwrap()),
        );
        group.bench_with_input(
            BenchmarkId::new(hayai_id("sequential"), &fixture.name),
            &fixture,
            |b, f| {
                b.iter(|| {
                    hayai_wire::RawBlock::parse_sequential(f.bytes.clone(), f.branch_id).unwrap()
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("zakura", &fixture.name),
            &fixture,
            |b, f| b.iter(|| zakura_wire::parse_round_trip(&f.bytes)),
        );
    }
    group.finish();
}

/// `wire/parse_block` id `zebra`: upstream `zebra-chain` 13.0.1, the same work as the
/// `zakura` id (`hayai_bench::zebra_wire::parse_round_trip`).
fn zebra_parse_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire/parse_block");
    group
        .sample_size(20)
        .measurement_time(Duration::from_secs(8));
    for fixture in fixtures::standard_set() {
        group.throughput(Throughput::Bytes(fixture.bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("zebra", &fixture.name),
            &fixture,
            |b, f| b.iter(|| zebra_wire::parse_round_trip(&f.bytes)),
        );
    }
    group.finish();
}

fn tx_wire_len(c: &mut Criterion) {
    let mut group = c.benchmark_group("wire/tx_wire_len");
    for fixture in fixtures::standard_set() {
        let body_start = fixture.bytes.len()
            - fixture
                .parse()
                .txs
                .iter()
                .map(|t| t.bytes.len())
                .sum::<usize>();
        group.throughput(Throughput::Bytes((fixture.bytes.len() - body_start) as u64));
        group.bench_with_input(
            BenchmarkId::new(hayai_id(""), &fixture.name),
            &fixture,
            |b, f| {
                b.iter(|| {
                    let mut pos = body_start;
                    let mut count = 0usize;
                    while pos < f.bytes.len() {
                        pos += hayai_wire::tx_wire_len(&f.bytes[pos..]).unwrap();
                        count += 1;
                    }
                    count
                })
            },
        );
    }
    group.finish();
}

fn roots(c: &mut Criterion) {
    let inputs: Vec<_> = fixtures::standard_set()
        .into_iter()
        .map(|f| {
            let block = f.parse();
            let (zk_txids, zk_digests, _) = zakura_wire::parse_round_trip(&f.bytes);
            (
                f.name,
                block.txids(),
                block.auth_digests(),
                zk_txids,
                zk_digests,
            )
        })
        .collect();

    let mut group = c.benchmark_group("wire/merkle_root");
    for (name, txids, _, zk_txids, _) in &inputs {
        group.throughput(Throughput::Elements(txids.len() as u64));
        group.bench_with_input(BenchmarkId::new(hayai_id(""), name), txids, |b, t| {
            b.iter(|| hayai_wire::merkle_root(t))
        });
        group.bench_with_input(BenchmarkId::new("zakura", name), zk_txids, |b, t| {
            b.iter(|| zakura_wire::merkle_root(t))
        });
    }
    group.finish();

    let mut group = c.benchmark_group("wire/auth_data_root");
    for (name, _, digests, _, zk_digests) in &inputs {
        group.throughput(Throughput::Elements(digests.len() as u64));
        group.bench_with_input(BenchmarkId::new(hayai_id(""), name), digests, |b, d| {
            b.iter(|| hayai_wire::auth_data_root(d))
        });
        group.bench_with_input(BenchmarkId::new("zakura", name), zk_digests, |b, d| {
            b.iter(|| zakura_wire::auth_data_root(d))
        });
    }
    group.finish();
}

criterion_group!(benches, parse_block, zebra_parse_block, tx_wire_len, roots);
criterion_main!(benches);
