//! Benchmark: hayai-blockstore against Zakura's block layout.
//!
//! Group `blockstore/get_block`: serving one block's wire bytes. hayai reads one record from
//! a flat file through its height index; the zakura baseline
//! (`hayai_bench::zakura_block_layout`) reads the header row and one RocksDB row per
//! transaction, deserializes them, assembles the block and serializes it. Parameters are the
//! fixture names of `hayai_fixtures::standard_set`.

hayai_bench::bench_allocator!();

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hayai_bench::hayai_id;
use hayai_bench::zebra_block_layout::ZebraBlockDb;
use hayai_bench::{scratch_dir, zakura_block_layout::ZakuraBlockDb};
use hayai_blockstore::BlockStore;
use hayai_fixtures as fixtures;
use zk_chain::block::Block;
use zk_chain::serialization::ZcashDeserialize;

fn get_block(c: &mut Criterion) {
    let set = fixtures::standard_set();
    let dir = scratch_dir();
    let store = BlockStore::open(dir.path().join("hayai")).unwrap();
    let zakura = ZakuraBlockDb::open(&dir.path().join("zakura")).unwrap();
    for (i, f) in set.iter().enumerate() {
        let height = f.height + i as u32;
        store.append(height, &f.parse()).unwrap();
        zakura
            .write_block(height, &Block::zcash_deserialize(&f.bytes[..]).unwrap())
            .unwrap();
    }

    let mut group = c.benchmark_group("blockstore/get_block");
    group
        .sample_size(30)
        .measurement_time(Duration::from_secs(6));
    for (i, f) in set.iter().enumerate() {
        let height = f.height + i as u32;
        group.throughput(Throughput::Bytes(f.bytes.len() as u64));
        group.bench_with_input(BenchmarkId::new(hayai_id(""), &f.name), &height, |b, h| {
            b.iter(|| store.get_bytes(*h).unwrap().unwrap())
        });
        group.bench_with_input(BenchmarkId::new("zakura", &f.name), &height, |b, h| {
            b.iter(|| zakura.serve_block(*h).unwrap())
        });
    }
    group.finish();
}

/// `blockstore/get_block` id `zebra`: Zebra's rows, options and `zebra-chain` 13.0.1 parse
/// (`hayai_bench::zebra_block_layout`).
fn zebra_get_block(c: &mut Criterion) {
    let set = fixtures::standard_set();
    let dir = scratch_dir();
    let zebra = ZebraBlockDb::open(dir.path()).unwrap();
    for (i, f) in set.iter().enumerate() {
        let block = zb_chain::serialization::ZcashDeserialize::zcash_deserialize(&f.bytes[..])
            .expect("zebra parses the fixture");
        zebra.write_block(f.height + i as u32, &block).unwrap();
    }

    let mut group = c.benchmark_group("blockstore/get_block");
    group
        .sample_size(30)
        .measurement_time(Duration::from_secs(6));
    for (i, f) in set.iter().enumerate() {
        let height = f.height + i as u32;
        group.throughput(Throughput::Bytes(f.bytes.len() as u64));
        group.bench_with_input(BenchmarkId::new("zebra", &f.name), &height, |b, h| {
            b.iter(|| zebra.serve_block(*h).unwrap())
        });
    }
    group.finish();
}

criterion_group!(benches, get_block, zebra_get_block);
criterion_main!(benches);
