//! Benchmark: hayai-relay compact blocks against full-block relay.
//!
//! Groups:
//! - `relay/bytes_on_wire`: a table (printed, not timed) of full block bytes against the
//!   compact block with short ids, with one batch reference, and the candidate block that
//!   references a published candidate equal to the block (the fixture in canonical order).
//! - `relay/reconstruct`: time to rebuild a block from a store holding every transaction,
//!   by short ids, by one batch reference, and from a candidate reference (resolution of
//!   the set, canonical order, merkle check and assembly).
//! - `relay/full_block_parse`: the baseline a full-body relay pays on receipt, with
//!   hayai-wire and with `zakura-chain`'s `Block::zcash_deserialize`.
//! - `relay/short_id_index`: time to build the per-block short-id index over 10k and 50k
//!   stored ids.
//! - `relay/forward_latency`: time from a received compact block to the forwarded one when
//!   the node lacks one transaction. Protocol version 1 reconstructs first, which costs one
//!   `BlockTxnRequest` round trip to a simulated peer that answers after 20 ms. Version 2
//!   carries the transaction as a full id, verifies the id list against the header and
//!   forwards at once.
//!
//! Fixtures: the transparent, Orchard and mixed blocks of `hayai_fixtures` and two
//! real mainnet NU5 blocks from `crates/hayai-wire/tests/vectors`.

hayai_bench::bench_allocator!();

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use hayai_bench::scenarios::relay::{
    all_fixtures, canonical, published_candidate, whole_block_batch, zakura_parse_with_ids,
    Fixture, MemStore,
};
use hayai_relay::{
    encode, reconstruct, resolve, resolve_candidate, BlockTxn, BlockTxnRequest, CompactBlock,
    CompactBuilder, IdForm, LaneStore, Message, ShortIdIndex, ShortIdKey,
};
use hayai_wire::{RawBlock, RawTx, TxLookup, WtxId};
use zk_chain::serialization::ZcashDeserialize;

fn short_form(_: usize, _: &WtxId) -> IdForm {
    IdForm::Short
}

fn frame_len(cb: &CompactBlock) -> usize {
    encode(&Message::CompactBlock(Box::new(cb.clone()))).len()
}

fn bytes_on_wire(fixtures: &[Fixture]) {
    println!();
    println!("relay/bytes_on_wire");
    println!(
        "{:<28} {:>5} {:>10} {:>12} {:>7} {:>12} {:>7} {:>12} {:>9}",
        "fixture",
        "txs",
        "full",
        "short-ids",
        "ratio",
        "batch-ref",
        "ratio",
        "cand-ref",
        "ref-only"
    );
    let mut rows = Vec::new();
    for f in fixtures {
        let store = MemStore::holding(&f.block, 0);
        let (batch, _) = whole_block_batch(&f.block, &store);
        let short = CompactBlock::from_block(&f.block, &[], short_form, 1);
        let batched = CompactBlock::from_block(&f.block, &[&batch], short_form, 1);
        assert_eq!(batched.batch_refs.len(), 1);
        assert!(batched.short_ids.is_empty());
        let full = f.block.bytes.len();
        let (s, b) = (frame_len(&short), frame_len(&batched));
        let ordered = canonical(&f.block, f.branch);
        let (_, _, candidate) = published_candidate(&ordered, &store);
        let cb = CompactBuilder::from_block(&ordered, 1)
            .build_candidate(&[&candidate], short_form)
            .expect("a block equal to the candidate has a candidate form");
        let c = encode(&Message::CandidateBlock(Box::new(cb.clone()))).len();
        // The frame without the header and the coinbase bytes: the reference itself.
        let reference = c - cb.header.len() - cb.coinbase.len();
        println!(
            "{:<28} {:>5} {:>10} {:>12} {:>6.1}x {:>12} {:>6.1}x {:>12} {:>9}",
            f.name,
            f.block.txs.len(),
            full,
            s,
            full as f64 / s as f64,
            b,
            full as f64 / b as f64,
            c,
            reference
        );
        rows.push(serde_json::json!({
            "fixture": f.name,
            "txs": f.block.txs.len(),
            "full_bytes": full,
            "short_id_bytes": s,
            "batch_ref_bytes": b,
            "candidate_ref_bytes": c,
            "candidate_ref_only_bytes": reference,
        }));
    }
    println!();
    // The report reads this table; criterion has no slot for a non-timed measurement.
    let out = hayai_bench::results_dir().join("relay-bytes.json");
    std::fs::write(
        &out,
        serde_json::to_vec_pretty(&rows).expect("serializable"),
    )
    .unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));
}

fn bench_reconstruct(c: &mut Criterion, fixtures: &[Fixture]) {
    let mut group = c.benchmark_group("relay/reconstruct");
    for f in fixtures {
        let store = MemStore::holding(&f.block, 10_000);
        let (batch, lanes) = whole_block_batch(&f.block, &store);
        let short = CompactBlock::from_block(&f.block, &[], short_form, 1);
        let batched = CompactBlock::from_block(&f.block, &[&batch], short_form, 1);
        group.throughput(Throughput::Bytes(f.block.bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("short_ids_store10k", &f.name),
            &short,
            |b, cb| {
                b.iter(|| reconstruct(cb, &store, &lanes, f.branch).expect("complete"));
            },
        );
        group.bench_with_input(BenchmarkId::new("batch_ref", &f.name), &batched, |b, cb| {
            b.iter(|| reconstruct(cb, &store, &lanes, f.branch).expect("complete"));
        });
        let ordered = canonical(&f.block, f.branch);
        let ordered_store = MemStore::holding(&ordered, 10_000);
        let (lanes, candidates, candidate) = published_candidate(&ordered, &ordered_store);
        let cb = CompactBuilder::from_block(&ordered, 1)
            .build_candidate(&[&candidate], short_form)
            .expect("candidate form");
        group.bench_with_input(BenchmarkId::new("candidate_ref", &f.name), &cb, |b, cb| {
            b.iter(|| {
                resolve_candidate(cb, &ordered_store, &candidates, &lanes, f.branch)
                    .expect("resolves")
                    .into_partial()
                    .expect("complete")
                    .assemble()
                    .expect("merkle root matches")
            });
        });
    }
    group.finish();
}

fn bench_full_block_parse(c: &mut Criterion, fixtures: &[Fixture]) {
    let mut group = c.benchmark_group("relay/full_block_parse");
    for f in fixtures {
        let bytes = f.block.bytes.clone();
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("hayai_wire", &f.name),
            &bytes,
            |b, bytes| {
                b.iter_batched(
                    || bytes.clone(),
                    |bytes| RawBlock::parse(bytes, f.branch).expect("parses"),
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("zakura_chain", &f.name),
            &bytes,
            |b, bytes| {
                b.iter(|| zk_chain::block::Block::zcash_deserialize(&bytes[..]).expect("parses"));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("zakura_chain_with_ids", &f.name),
            &bytes,
            |b, bytes| {
                b.iter(|| zakura_parse_with_ids(bytes));
            },
        );
    }
    group.finish();
}

fn bench_short_id_index(c: &mut Criterion, fixtures: &[Fixture]) {
    let mut group = c.benchmark_group("relay/short_id_index");
    let block = &fixtures[0].block;
    let key = ShortIdKey::from_header(&block.bytes[..block.header.serialized_len()], 1);
    for size in [10_000usize, 50_000] {
        let store = MemStore::holding(block, size);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &store, |b, store| {
            b.iter(|| ShortIdIndex::build(&key, store));
        });
    }
    group.finish();
}

/// A store that lacks one transaction of the block.
struct Lacking<'a> {
    inner: &'a MemStore,
    missing: WtxId,
}

impl TxLookup for Lacking<'_> {
    fn get(&self, id: &WtxId) -> Option<std::sync::Arc<RawTx>> {
        if *id == self.missing {
            return None;
        }
        self.inner.get(id)
    }
    fn for_each_id(&self, f: &mut dyn FnMut(&WtxId)) {
        self.inner.for_each_id(&mut |id| {
            if *id != self.missing {
                f(id);
            }
        });
    }
    fn len(&self) -> usize {
        self.inner.len() - 1
    }
}

/// Delay of the simulated peer that answers `BlockTxnRequest` on the version 1 path.
const ROUND_TRIP: Duration = Duration::from_millis(20);

type Request = (BlockTxnRequest, mpsc::Sender<BlockTxn>);

fn simulated_peer(block: &RawBlock) -> (mpsc::Sender<Request>, thread::JoinHandle<()>) {
    let txs: Vec<_> = block.txs.iter().map(|t| t.bytes.clone()).collect();
    let (tx, rx) = mpsc::channel::<Request>();
    let handle = thread::spawn(move || {
        for (request, reply) in rx {
            thread::sleep(ROUND_TRIP);
            let answer = BlockTxn {
                block_hash: request.block_hash,
                txs: request
                    .indexes
                    .iter()
                    .map(|&i| txs[i as usize].clone())
                    .collect(),
            };
            let _ = reply.send(answer);
        }
    });
    (tx, handle)
}

fn bench_forward_latency(c: &mut Criterion, fixtures: &[Fixture]) {
    let mut group = c.benchmark_group("relay/forward_latency");
    group.sample_size(20);
    for f in fixtures {
        let full = MemStore::holding(&f.block, 10_000);
        let missing_index = f.block.txs.len() / 2;
        let missing = f.block.txs[missing_index].wtxid();
        let store = Lacking {
            inner: &full,
            missing,
        };
        let lanes = LaneStore::new();
        let (peer, handle) = simulated_peer(&f.block);

        // Version 1: the missing transaction is a short id the store cannot resolve.
        let v1 = CompactBlock::from_block(&f.block, &[], short_form, 1);
        group.bench_with_input(
            BenchmarkId::new("v1_reconstruct_first", &f.name),
            &v1,
            |b, cb| {
                b.iter(|| {
                    let mut partial = resolve(cb, &store, &lanes, f.branch).expect("resolves");
                    let unknown = partial.unknown();
                    assert_eq!(unknown.len(), 1);
                    let (reply, answer) = mpsc::channel();
                    peer.send((
                        BlockTxnRequest::for_missing(partial.block_hash(), unknown.clone()),
                        reply,
                    ))
                    .expect("peer alive");
                    let txn = answer.recv().expect("answer");
                    partial
                        .apply_block_txn(&txn, &unknown, f.branch)
                        .expect("applies");
                    partial.verify_ids(None).expect("ids verify");
                    let block = partial.assemble().expect("complete");
                    CompactBlock::from_block(&block, &[], short_form, 2)
                });
            },
        );

        // Version 2: the missing transaction is a full id; the id list verifies without it.
        let v2 = CompactBlock::from_block(
            &f.block,
            &[],
            |i, _| {
                if i == missing_index {
                    IdForm::Full
                } else {
                    IdForm::Short
                }
            },
            1,
        );
        group.bench_with_input(
            BenchmarkId::new("v2_forward_on_ids", &f.name),
            &v2,
            |b, cb| {
                b.iter(|| {
                    let partial = resolve(cb, &store, &lanes, f.branch).expect("resolves");
                    assert!(partial.unknown().is_empty());
                    partial.verify_ids(None).expect("ids verify");
                    partial
                        .builder(2)
                        .expect("ids complete")
                        .build(&[], short_form)
                });
            },
        );
        drop(peer);
        handle.join().expect("peer thread");
    }
    group.finish();
}

fn bench(c: &mut Criterion) {
    let fixtures = all_fixtures();
    bytes_on_wire(&fixtures);
    bench_reconstruct(c, &fixtures);
    bench_full_block_parse(c, &fixtures);
    bench_short_id_index(c, &fixtures);
    bench_forward_latency(c, &fixtures);
}

criterion_group!(benches, bench);
criterion_main!(benches);
