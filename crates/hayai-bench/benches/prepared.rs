//! Benchmark: hayai-prepared. See crates/hayai-bench/src/lib.rs for the shared fixtures.
//!
//! Groups:
//! - `prepared/prepare_tx/<fixture>`: one cold `prepare` per iteration, cycling through the
//!   fixture's transactions (coin fetch from the chain view, sighash digests, every input's
//!   script, nullifiers and commitments, bundle queued). Shielded proof verification is the
//!   batch cost measured in `prepared/bisection`.
//! - `prepared/store_lookup/<hit %>`: 6,500 `PreparedStore::get` calls per iteration against
//!   a store holding the 6,500-transaction fixture, with the given share of ids present.
//! - `prepared/bisection/64x2-1bad`: 64 Orchard bundles of which one has a flipped proof
//!   byte. `hayai` is one scoped batch finalized with bisection; `zakura-fallback` is what
//!   Zakura does on a failed batch: the failed whole-batch verification followed by every
//!   bundle verified on its own. `prepared/bisection/64x2-valid` is the no-failure cost of
//!   one batch for reference.

hayai_bench::bench_allocator!();

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hayai_bench::chain_fixture::harness;
use hayai_bench::fixtures::{orchard_block, standard_set, transparent_block};
use hayai_bench::hayai_id;
use hayai_coins::CoinsView;
use hayai_crypto::zcash_primitives;
use hayai_prepared::{prepare, ScopedBatch};
use hayai_wire::{RawTx, WtxId};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use zcash_primitives::transaction::TxId;

fn prepare_tx(c: &mut Criterion) {
    let mut group = c.benchmark_group("prepared/prepare_tx");
    group.throughput(Throughput::Elements(1));
    for fixture in standard_set() {
        let h = harness(&fixture);
        let view = h.chain.view();
        let txs = &h.block.txs[1..];
        let mut next = 0usize;
        group.bench_function(BenchmarkId::new(hayai_id(""), &fixture.name), |b| {
            b.iter(|| {
                let raw = txs[next % txs.len()].clone();
                next += 1;
                let mut batch = ScopedBatch::new(&h.cfg.keys);
                prepare(raw, h.cfg.epoch(), &view as &dyn CoinsView, &mut batch).expect("valid")
            })
        });
    }
    group.finish();
}

fn store_lookup(c: &mut Criterion) {
    let fixture = transparent_block(6500, 1);
    let h = harness(&fixture);
    h.fill_store();
    let present: Vec<WtxId> = h.block.txs[1..].iter().map(RawTx::wtxid).collect();
    let mut rng = StdRng::seed_from_u64(0x1007);
    let mut group = c.benchmark_group("prepared/store_lookup");
    group.throughput(Throughput::Elements(present.len() as u64));
    for hit_percent in [100u32, 50, 0] {
        let ids: Vec<WtxId> = present
            .iter()
            .map(|id| {
                if rng.gen_range(0..100) < hit_percent {
                    *id
                } else {
                    WtxId {
                        txid: TxId::from_bytes(rng.gen()),
                        auth_digest: rng.gen(),
                    }
                }
            })
            .collect();
        group.bench_function(BenchmarkId::new(hayai_id(""), hit_percent), |b| {
            b.iter(|| {
                let mut hits = 0usize;
                for id in &ids {
                    if let Some(found) = h.store.get(id) {
                        hits += found.spent.len();
                    }
                }
                hits
            })
        });
    }
    group.finish();
}

/// Verifies `n` Orchard bundles with Zakura's failure handling: one whole batch, then each
/// bundle alone when the batch fails.
fn verify_each_singly(h: &hayai_bench::chain_fixture::Harness, raws: &[RawTx]) -> Vec<WtxId> {
    let view = h.chain.view();
    let mut whole = ScopedBatch::new(&h.cfg.keys);
    for raw in raws {
        prepare(raw.clone(), h.cfg.epoch(), &view, &mut whole).expect("context-free rules hold");
    }
    if whole.finalize().failed.is_empty() {
        return Vec::new();
    }
    let mut failed = Vec::new();
    for raw in raws {
        let mut single = ScopedBatch::new(&h.cfg.keys);
        prepare(raw.clone(), h.cfg.epoch(), &view, &mut single).expect("context-free rules hold");
        failed.extend(single.finalize().failed);
    }
    failed
}

fn bisection(c: &mut Criterion) {
    const BUNDLES: usize = 64;
    let fixture = orchard_block(165, 2);
    let h = harness(&fixture);
    let view = h.chain.view();
    let valid: Vec<RawTx> = h.block.txs[1..=BUNDLES].to_vec();
    let mut with_bad = valid.clone();
    let mut bytes = with_bad[BUNDLES / 3].bytes.to_vec();
    let n = bytes.len();
    bytes[n - 200] ^= 0x01;
    with_bad[BUNDLES / 3] = RawTx::parse(Bytes::from(bytes), fixture.branch_id).expect("parses");
    let bad = with_bad[BUNDLES / 3].wtxid();
    // Build the verifying key outside the measurement.
    let mut warm = ScopedBatch::new(&h.cfg.keys);
    prepare(valid[0].clone(), h.cfg.epoch(), &view, &mut warm).expect("valid");
    assert!(warm.finalize().failed.is_empty());

    let mut group = c.benchmark_group("prepared/bisection");
    group.sample_size(10);
    let hayai = |raws: &[RawTx]| {
        let mut batch = ScopedBatch::new(&h.cfg.keys);
        for raw in raws {
            prepare(raw.clone(), h.cfg.epoch(), &view, &mut batch)
                .expect("context-free rules hold");
        }
        batch.finalize().failed
    };
    group.bench_function(BenchmarkId::new(hayai_id(""), "64x2-1bad"), |b| {
        b.iter(|| {
            let failed = hayai(&with_bad);
            assert_eq!(failed, vec![bad]);
            failed
        })
    });
    group.bench_function(BenchmarkId::new("zakura-fallback", "64x2-1bad"), |b| {
        b.iter(|| {
            let failed = verify_each_singly(&h, &with_bad);
            assert_eq!(failed, vec![bad]);
            failed
        })
    });
    group.bench_function(BenchmarkId::new(hayai_id(""), "64x2-valid"), |b| {
        b.iter(|| {
            let failed = hayai(&valid);
            assert!(failed.is_empty());
            failed
        })
    });
    group.finish();
}

criterion_group!(benches, prepare_tx, store_lookup, bisection);
criterion_main!(benches);
