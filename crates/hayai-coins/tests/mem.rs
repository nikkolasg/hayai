//! `MemBacking`: equality with `RocksBacking`, crash recovery, snapshots and concurrent reads.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use hayai_coins::stored_best_block;
use hayai_coins::{
    BestBlock, Coin, CoinsBacking, Config, Error, FlushGeneration, MemBacking, MemConfig, OutPoint,
    PersistError, Pool, Recovery, RocksBacking,
};
use proptest::prelude::*;

const LOG: &str = "coins.log";
const SNAPSHOT: &str = "coins.snapshot";
/// Bytes of a log record header.
const RECORD_HEADER: usize = 20;

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("scratch dir under target/")
}

fn open_mem(dir: &Path) -> (MemBacking, Recovery) {
    MemBacking::open(dir, &MemConfig::default()).expect("open mem store")
}

fn open_rocks(dir: &Path) -> RocksBacking {
    let config = Config {
        block_cache_bytes: 8 << 20,
        write_buffer_bytes: 4 << 20,
        background_jobs: 2,
    };
    RocksBacking::open(dir, &config).expect("open rocks store")
}

/// Outpoint `i`, spread over the shards by its first txid byte.
fn outpoint(i: u32) -> OutPoint {
    let mut txid = [0u8; 32];
    txid[0] = (i.wrapping_mul(97) % 256) as u8;
    txid[1..5].copy_from_slice(&i.to_le_bytes());
    OutPoint::new(txid, i % 4)
}

/// Coin version `v`: every script kind (P2PKH, P2SH, raw of several lengths) and both
/// coinbase flags.
fn coin(v: u32) -> Coin {
    let h = [(v % 251) as u8; 20];
    let script = match v % 4 {
        0 => [&[0x76, 0xa9, 0x14][..], &h, &[0x88, 0xac]].concat(),
        1 => [&[0xa9, 0x14][..], &h, &[0x87]].concat(),
        2 => vec![0x6a; (v % 40) as usize],
        _ => [&[33u8][..], &[2; 33], &[0xac]].concat(),
    };
    Coin {
        value: u64::from(v) * 1_000_003,
        script_pubkey: Bytes::from(script),
        height: v.wrapping_mul(2_654_435_761),
        is_coinbase: v.is_multiple_of(3),
    }
}

fn nullifier(i: u32) -> [u8; 32] {
    let mut nf = [0x5au8; 32];
    nf[0] = (i.wrapping_mul(131) % 256) as u8;
    nf[1..5].copy_from_slice(&i.to_le_bytes());
    nf
}

fn best(height: u32) -> BestBlock {
    BestBlock {
        height,
        hash: [(height % 256) as u8; 32],
    }
}

fn generation(
    height: u32,
    adds: &[(u32, u32)],
    spends: &[u32],
    nfs: &[(Pool, u32)],
) -> FlushGeneration {
    let mut nullifiers: [Vec<[u8; 32]>; 4] = Default::default();
    for &(pool, i) in nfs {
        nullifiers[pool.index()].push(nullifier(i));
    }
    FlushGeneration {
        adds: adds.iter().map(|&(k, v)| (outpoint(k), coin(v))).collect(),
        spends: spends.iter().map(|&k| outpoint(k)).collect(),
        nullifiers,
        best_block: best(height),
    }
}

const KEYS: u32 = 64;
const NULLIFIERS: u32 = 48;

/// Everything both stores can be asked: every key, every nullifier of every pool.
fn view(backing: &dyn CoinsBacking) -> (Vec<Option<Coin>>, Vec<Vec<bool>>) {
    let outpoints: Vec<OutPoint> = (0..KEYS).map(outpoint).collect();
    let nfs: Vec<[u8; 32]> = (0..NULLIFIERS).map(nullifier).collect();
    let coins = backing.get_many(&outpoints).expect("get_many");
    let sets = Pool::ALL
        .iter()
        .map(|&pool| backing.contains_many(pool, &nfs).expect("contains_many"))
        .collect();
    (coins, sets)
}

#[derive(Clone, Debug)]
enum Op {
    Generation {
        adds: Vec<(u32, u32)>,
        spends: Vec<u32>,
        nfs: Vec<(Pool, u32)>,
    },
    Batch {
        adds: Vec<(u32, u32)>,
        spends: Vec<u32>,
    },
    Insert {
        pool: Pool,
        nfs: Vec<u32>,
    },
    Snapshot,
    Reopen,
}

fn op() -> impl Strategy<Value = Op> {
    let adds = proptest::collection::vec((0..KEYS, any::<u32>()), 0..12);
    let spends = proptest::collection::vec(0..KEYS, 0..12);
    let pool = proptest::sample::select(Pool::ALL.to_vec());
    prop_oneof![
        4 => (
            adds.clone(),
            spends.clone(),
            proptest::collection::vec((pool.clone(), 0..NULLIFIERS), 0..8)
        )
            .prop_map(|(adds, spends, nfs)| Op::Generation { adds, spends, nfs }),
        2 => (adds, spends).prop_map(|(adds, spends)| Op::Batch { adds, spends }),
        1 => (pool, proptest::collection::vec(0..NULLIFIERS, 1..8))
            .prop_map(|(pool, nfs)| Op::Insert { pool, nfs }),
        1 => Just(Op::Snapshot),
        1 => Just(Op::Reopen),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 48,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// The same random sequence of writes, snapshots and restarts leaves both stores
    /// answering every query the same way, with the same best block.
    #[test]
    fn mem_backing_equals_rocks_backing(ops in proptest::collection::vec(op(), 1..30)) {
        let rocks_dir = scratch();
        let mem_dir = scratch();
        let rocks = open_rocks(rocks_dir.path());
        let (mut mem, _) = open_mem(mem_dir.path());
        let mut height = 0;
        for op in ops {
            match op {
                Op::Generation { adds, spends, nfs } => {
                    height += 1;
                    let g = generation(height, &adds, &spends, &nfs);
                    rocks.write_generation(&g).expect("rocks write");
                    mem.write_generation(&g).expect("mem write");
                }
                Op::Batch { adds, spends } => {
                    let adds: Vec<(OutPoint, Coin)> =
                        adds.iter().map(|&(k, v)| (outpoint(k), coin(v))).collect();
                    let adds: Vec<(&OutPoint, &Coin)> = adds.iter().map(|(o, c)| (o, c)).collect();
                    let spends: Vec<OutPoint> = spends.iter().map(|&k| outpoint(k)).collect();
                    let spends: Vec<&OutPoint> = spends.iter().collect();
                    rocks.write_batch(&adds, &spends).expect("rocks write");
                    mem.write_batch(&adds, &spends).expect("mem write");
                }
                Op::Insert { pool, nfs } => {
                    let nfs: Vec<[u8; 32]> = nfs.into_iter().map(nullifier).collect();
                    rocks.insert_many(pool, &nfs).expect("rocks insert");
                    mem.insert_many(pool, &nfs).expect("mem insert");
                }
                Op::Snapshot => {
                    mem.snapshot().expect("snapshot");
                    prop_assert_eq!(fs::metadata(mem_dir.path().join(LOG)).expect("log").len(), 0);
                }
                Op::Reopen => {
                    drop(mem);
                    let (reopened, recovery) = open_mem(mem_dir.path());
                    prop_assert_eq!(recovery.torn_tail_bytes, 0);
                    mem = reopened;
                }
            }
            prop_assert_eq!(view(&mem), view(&rocks));
            prop_assert_eq!(mem.best_block().expect("best"), rocks.best_block().expect("best"));
        }
    }
}

/// Three generations; returns the log offsets where each record starts and the log length.
fn three_generations(dir: &Path) -> (Vec<u64>, u64) {
    let (mem, _) = open_mem(dir);
    let mut starts = Vec::new();
    let log = dir.join(LOG);
    for g in [
        generation(1, &[(1, 1), (2, 2), (3, 3)], &[], &[(Pool::Orchard, 1)]),
        generation(2, &[(4, 4), (5, 5)], &[1], &[(Pool::Sapling, 2)]),
        generation(
            3,
            &[(6, 6), (2, 7)],
            &[3],
            &[(Pool::Orchard, 3), (Pool::Sprout, 4)],
        ),
    ] {
        starts.push(fs::metadata(&log).expect("log").len());
        mem.write_generation(&g).expect("write");
    }
    (starts, fs::metadata(&log).expect("log").len())
}

fn copy_store(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("entry");
        fs::copy(entry.path(), to.join(entry.file_name())).expect("copy");
    }
}

fn reference_after_two(dir: &Path) -> (Vec<Option<Coin>>, Vec<Vec<bool>>) {
    let (mem, _) = open_mem(dir);
    mem.write_generation(&generation(
        1,
        &[(1, 1), (2, 2), (3, 3)],
        &[],
        &[(Pool::Orchard, 1)],
    ))
    .expect("write");
    mem.write_generation(&generation(
        2,
        &[(4, 4), (5, 5)],
        &[1],
        &[(Pool::Sapling, 2)],
    ))
    .expect("write");
    view(&mem)
}

/// A crash that cuts the last record anywhere leaves the state after the previous record:
/// the torn bytes are reported and removed, and the next write follows the valid prefix.
#[test]
fn a_torn_last_record_is_cut_at_every_byte_offset() {
    let base = scratch();
    let (starts, len) = three_generations(base.path());
    let reference_dir = scratch();
    let reference = reference_after_two(reference_dir.path());
    let last = starts[2];
    for cut in last..len {
        let dir = scratch();
        copy_store(base.path(), dir.path());
        let log = fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join(LOG))
            .expect("open log");
        log.set_len(cut).expect("truncate");
        drop(log);
        let (mem, recovery) = open_mem(dir.path());
        assert_eq!(
            recovery,
            Recovery {
                replayed: 2,
                torn_tail_bytes: cut - last
            },
            "cut at {cut}"
        );
        assert_eq!(view(&mem), reference, "cut at {cut}");
        assert_eq!(mem.best_block().expect("best"), Some(best(2)));
        assert_eq!(fs::metadata(dir.path().join(LOG)).expect("log").len(), last);
        // The store keeps working after the cut, and a restart sees the new record.
        mem.write_generation(&generation(9, &[(9, 9)], &[], &[]))
            .expect("write after recovery");
        drop(mem);
        let (mem, recovery) = open_mem(dir.path());
        assert_eq!(
            recovery,
            Recovery {
                replayed: 3,
                torn_tail_bytes: 0
            }
        );
        assert_eq!(mem.best_block().expect("best"), Some(best(9)));
    }
}

/// An extension the file system filled with zeros is a torn tail too.
#[test]
fn a_zero_filled_tail_is_a_torn_tail() {
    let dir = scratch();
    let (_, len) = three_generations(dir.path());
    let log = fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join(LOG))
        .expect("open log");
    log.set_len(len + 4096).expect("extend");
    drop(log);
    let (mem, recovery) = open_mem(dir.path());
    assert_eq!(
        recovery,
        Recovery {
            replayed: 3,
            torn_tail_bytes: 4096
        }
    );
    assert_eq!(mem.best_block().expect("best"), Some(best(3)));
}

fn corrupt_log_offset(error: Error) -> u64 {
    let Error::Persist(PersistError::CorruptLog { offset, .. }) = error else {
        panic!("expected a corrupt log error, got {error}");
    };
    offset
}

/// Damage to a record that is not the last one is an error that names its offset, for a
/// flipped bit in the payload, in the length and in the magic.
#[test]
fn a_corrupt_middle_record_fails_the_open() {
    let base = scratch();
    let (starts, _) = three_generations(base.path());
    let middle = starts[1] as usize;
    for at in [middle + RECORD_HEADER + 10, middle + 5, middle] {
        let dir = scratch();
        copy_store(base.path(), dir.path());
        let path = dir.path().join(LOG);
        let mut bytes = fs::read(&path).expect("read log");
        bytes[at] ^= 0x40;
        fs::write(&path, &bytes).expect("write log");
        let Err(error) = MemBacking::open(dir.path(), &MemConfig::default()) else {
            panic!("open succeeded on a log damaged at byte {at}");
        };
        assert_eq!(
            corrupt_log_offset(error),
            middle as u64,
            "damage at byte {at}"
        );
    }
}

/// A snapshot holds the set at its sequence number; a restart loads it and replays only
/// the later records. Log records the snapshot already holds (a crash between the rename
/// and the truncation) are skipped.
#[test]
fn snapshot_and_log_replay_round_trip() {
    let dir = scratch();
    let reference_dir = scratch();
    let reference = open_rocks(reference_dir.path());
    let (mem, _) = open_mem(dir.path());
    let gens = [
        generation(
            1,
            &[(1, 1), (2, 2), (3, 3), (10, 10)],
            &[],
            &[(Pool::Orchard, 1)],
        ),
        generation(
            2,
            &[(4, 4), (5, 5)],
            &[1],
            &[(Pool::Sapling, 2), (Pool::Ironwood, 3)],
        ),
        generation(3, &[(6, 6), (2, 7)], &[3], &[(Pool::Orchard, 3)]),
        generation(4, &[(7, 11)], &[10], &[(Pool::Sprout, 5)]),
    ];
    for g in &gens[..2] {
        mem.write_generation(g).expect("write");
        reference.write_generation(g).expect("write");
    }
    let log_before_snapshot = fs::read(dir.path().join(LOG)).expect("read log");
    let size = mem.snapshot().expect("snapshot");
    assert_eq!(
        fs::metadata(dir.path().join(SNAPSHOT))
            .expect("snapshot")
            .len(),
        size
    );
    for g in &gens[2..] {
        mem.write_generation(g).expect("write");
        reference.write_generation(g).expect("write");
    }
    drop(mem);

    let (mem, recovery) = open_mem(dir.path());
    assert_eq!(
        recovery,
        Recovery {
            replayed: 2,
            torn_tail_bytes: 0
        }
    );
    assert_eq!(view(&mem), view(&reference));
    assert_eq!(mem.best_block().expect("best"), Some(best(4)));
    assert_eq!(mem.coin_count(), 5);
    drop(mem);

    // A crash after the rename and before the truncation: the log still holds the two
    // records the snapshot has.
    let crash = scratch();
    copy_store(dir.path(), crash.path());
    let (mem, _) = open_mem(crash.path());
    mem.snapshot().expect("snapshot");
    drop(mem);
    fs::write(crash.path().join(LOG), &log_before_snapshot).expect("restore log");
    let (mem, recovery) = open_mem(crash.path());
    assert_eq!(
        recovery,
        Recovery {
            replayed: 0,
            torn_tail_bytes: 0
        }
    );
    assert_eq!(view(&mem), view(&reference));
    assert_eq!(mem.best_block().expect("best"), Some(best(4)));
}

/// A log whose records start after the snapshot's sequence number is missing records.
#[test]
fn a_log_gap_after_the_snapshot_fails_the_open() {
    let dir = scratch();
    let (mem, _) = open_mem(dir.path());
    mem.write_generation(&generation(1, &[(1, 1)], &[], &[]))
        .expect("write");
    drop(mem);
    let first_log = fs::read(dir.path().join(LOG)).expect("read log");
    let (mem, _) = open_mem(dir.path());
    mem.write_generation(&generation(2, &[(2, 2)], &[], &[]))
        .expect("write");
    drop(mem);
    // Keep only the second record: the first one (sequence 1) is gone.
    let log = fs::read(dir.path().join(LOG)).expect("read log");
    fs::write(dir.path().join(LOG), &log[first_log.len()..]).expect("write log");
    let Err(error) = MemBacking::open(dir.path(), &MemConfig::default()) else {
        panic!("open succeeded on a log with a gap");
    };
    assert_eq!(corrupt_log_offset(error), 0);
}

/// A snapshot is renamed into place whole, so any damage to it is an error.
#[test]
fn a_damaged_snapshot_fails_the_open() {
    let base = scratch();
    let (mem, _) = open_mem(base.path());
    mem.write_generation(&generation(
        1,
        &[(1, 1), (2, 2)],
        &[],
        &[(Pool::Orchard, 1)],
    ))
    .expect("write");
    let size = mem.snapshot().expect("snapshot") as usize;
    drop(mem);
    let snapshot: PathBuf = base.path().join(SNAPSHOT);
    let bytes = fs::read(&snapshot).expect("read snapshot");
    for damage in [0, 30, size / 2, size - 1] {
        let dir = scratch();
        copy_store(base.path(), dir.path());
        let mut damaged = bytes.clone();
        damaged[damage] ^= 1;
        fs::write(dir.path().join(SNAPSHOT), &damaged).expect("write");
        let Err(Error::Persist(PersistError::CorruptSnapshot { .. })) =
            MemBacking::open(dir.path(), &MemConfig::default())
        else {
            panic!("open succeeded on a snapshot damaged at byte {damage}");
        };
    }
    let dir = scratch();
    copy_store(base.path(), dir.path());
    fs::write(dir.path().join(SNAPSHOT), &bytes[..size - 3]).expect("write");
    let Err(Error::Persist(PersistError::CorruptSnapshot { .. })) =
        MemBacking::open(dir.path(), &MemConfig::default())
    else {
        panic!("open succeeded on a truncated snapshot");
    };
}

/// Readers look up coins while generations are written. A coin no generation touches is
/// always found; a coin the generations replace or spend is always one whole version of
/// itself or absent, never a mix of two versions.
#[test]
fn readers_see_whole_coins_during_write_generation() {
    const STABLE: u32 = 50_000;
    const CHURN: u32 = 5_000;
    let dir = scratch();
    let backing = Arc::new(open_mem(dir.path()).0);
    let seed: Vec<(OutPoint, Coin)> = (0..STABLE + CHURN)
        .map(|i| (outpoint(i), coin(i)))
        .collect();
    let seed: Vec<(&OutPoint, &Coin)> = seed.iter().map(|(o, c)| (o, c)).collect();
    backing.write_batch(&seed, &[]).expect("seed");

    let stable: Vec<OutPoint> = (0..STABLE).map(outpoint).collect();
    let churn: Vec<OutPoint> = (STABLE..STABLE + CHURN).map(outpoint).collect();
    // Version r of churn coin i is coin(i + r * (STABLE + CHURN)); round 0 is the seed.
    let rounds = 20u32;
    let span = STABLE + CHURN;
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                let mut passes = 0;
                while !done.load(Ordering::Acquire) || passes == 0 {
                    let coins = backing.get_many(&stable).expect("get stable");
                    for (i, c) in coins.into_iter().enumerate() {
                        assert_eq!(c, Some(coin(i as u32)), "stable coin {i}");
                    }
                    let coins = backing.get_many(&churn).expect("get churn");
                    for (j, c) in coins.into_iter().enumerate() {
                        let i = STABLE + j as u32;
                        let Some(c) = c else { continue };
                        let whole = (0..=rounds).any(|r| c == coin(i + r * span));
                        assert!(whole, "churn coin {i} is not a whole version: {c:?}");
                    }
                    passes += 1;
                }
            });
        }
        for r in 1..=rounds {
            // Odd rounds spend the churn coins, even rounds add them back in a new version.
            let g = match r % 2 {
                1 => FlushGeneration {
                    adds: Vec::new(),
                    spends: churn.clone(),
                    nullifiers: Default::default(),
                    best_block: best(r),
                },
                _ => FlushGeneration {
                    adds: (STABLE..span)
                        .map(|i| (outpoint(i), coin(i + r * span)))
                        .collect(),
                    spends: Vec::new(),
                    nullifiers: Default::default(),
                    best_block: best(r),
                },
            };
            backing.write_generation(&g).expect("write generation");
        }
        done.store(true, Ordering::Release);
    });
    assert_eq!(backing.coin_count(), span as usize);
    assert_eq!(backing.best_block().expect("best"), Some(best(rounds)));
}

/// Each file below `dir` with its length, its modification time and its content.
fn listing(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime, Vec<u8>)> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        let meta = fs::metadata(&path).expect("metadata");
        match meta.is_dir() {
            true => files.extend(listing(&path)),
            false => files.push((
                path.clone(),
                meta.len(),
                meta.modified().expect("mtime"),
                fs::read(&path).expect("read"),
            )),
        }
    }
    files.sort();
    files
}

/// `stored_best_block` gives the best block that an open of the store gives, for each
/// backend, and it changes no file: a torn last record stays in the log of the memory
/// backend, and the RocksDB store gets no new file.
#[test]
fn the_stored_best_block_is_read_without_a_write() {
    // Memory backend: no generation, then a snapshot at 2, a record of 3 and a torn record.
    let dir = scratch();
    drop(open_mem(dir.path()));
    assert_eq!(stored_best_block(dir.path()).expect("empty store"), None);
    let (starts, len) = three_generations(dir.path());
    assert_eq!(stored_best_block(dir.path()).expect("log"), Some(best(3)));
    let log = fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join(LOG))
        .expect("open log");
    log.set_len(len - 1).expect("truncate");
    drop(log);
    let before = listing(dir.path());
    assert_eq!(stored_best_block(dir.path()).expect("torn"), Some(best(2)));
    assert_eq!(listing(dir.path()), before);
    let (mem, recovery) = open_mem(dir.path());
    assert_eq!(recovery.torn_tail_bytes, len - 1 - starts[2]);
    assert_eq!(mem.best_block().expect("best"), Some(best(2)));
    mem.snapshot().expect("snapshot");
    assert_eq!(stored_best_block(dir.path()).expect("snap"), Some(best(2)));
    mem.write_generation(&generation(5, &[(9, 9)], &[], &[]))
        .expect("write");
    assert_eq!(stored_best_block(dir.path()).expect("both"), Some(best(5)));

    // RocksDB backend: the generation is in the write-ahead log only.
    let dir = scratch();
    let rocks = open_rocks(dir.path());
    assert_eq!(stored_best_block(dir.path()).expect("empty store"), None);
    rocks
        .write_generation(&generation(7, &[(1, 1)], &[], &[]))
        .expect("write");
    drop(rocks);
    let before = listing(dir.path());
    assert_eq!(stored_best_block(dir.path()).expect("rocks"), Some(best(7)));
    assert_eq!(listing(dir.path()), before);
    assert_eq!(
        open_rocks(dir.path()).best_block().expect("best"),
        Some(best(7))
    );

    // A directory without a store is an error, and the read makes no file in it.
    let dir = scratch();
    let Err(_) = stored_best_block(dir.path()) else {
        panic!("a directory without a store");
    };
    assert_eq!(listing(dir.path()), Vec::new());
}
