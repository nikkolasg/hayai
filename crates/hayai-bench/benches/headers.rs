//! Benchmark: the header check of the relay on the best chain index.
//!
//! Groups:
//! - `headers/parent_context`: the context of a header whose parent is the tip of an
//!   index of 300 committed headers (113 times and `nBits`, newest first).
//! - `headers/verify_regtest`: `StandardHeaderCheck::verify` of a Regtest header on that
//!   index: the known-block and parent lookups, the context, the proof-of-work hash and
//!   the time rules. Regtest has no Equihash and no expected `nBits`, so the number is
//!   the cost of the check itself, without the proof of work of Mainnet.
//! - `headers/check_regtest`: the relay's `check`, which also records the header as
//!   pending (the bench removes the pending entry on each iteration).

hayai_bench::bench_allocator!();

use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hayai_consensus::Network;
use hayai_relay::{HeaderCheck, StandardHeaderCheck};
use hayai_sync::index::{now_secs, HeaderIndex, SeedBlock};
use hayai_wire::header::{BlockHash, BlockHeader, PowParams};

fn header(prev: BlockHash, time: u32) -> BlockHeader {
    BlockHeader {
        version: 4,
        prev_hash: prev,
        merkle_root: [1; 32],
        block_commitments: [2; 32],
        time,
        bits: Network::Regtest.params().pow_limit_bits,
        nonce: [0; 32],
        solution: vec![0; PowParams::REGTEST.solution_len()],
    }
}

fn bench(c: &mut Criterion) {
    let params = Network::Regtest.params();
    let index = Arc::new(HeaderIndex::new(
        0,
        &[SeedBlock {
            hash: params.genesis_hash,
            time: params.genesis_time,
            bits: None,
        }],
    ));
    let now = now_secs();
    let n = 300u32;
    let mut prev = params.genesis_hash;
    for i in 1..=n {
        let h = header(prev, now - (n - i) * 75);
        prev = h.hash();
        index.push(h);
    }
    let next = header(prev, now);
    let check = StandardHeaderCheck {
        context: index.clone(),
        network: Network::Regtest,
        trust_short_context: false,
    };
    assert_eq!(check.verify(&next).map(|v| v.height), Ok(n + 1));
    let mut g = c.benchmark_group("headers");
    g.bench_function("parent_context", |b| {
        b.iter(|| black_box(index.parent_context(black_box(&prev))))
    });
    g.bench_function("verify_regtest", |b| {
        b.iter(|| black_box(check.verify(black_box(&next))))
    });
    g.bench_function("check_regtest", |b| {
        b.iter(|| {
            let r = check.check(black_box(&next));
            index.remove_pending(&next.hash());
            black_box(r)
        })
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
