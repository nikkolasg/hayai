//! Cache and backing semantics against a RocksDB store in a temporary directory.

use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use hayai_coins::{
    BestBlock, Coin, CoinsBacking, CoinsCache, CoinsView, Config, Error, FlushGeneration,
    FlushStats, NullifierSet, NullifierStore, OutPoint, Pool, RocksBacking,
};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

fn open(dir: &Path) -> Arc<RocksBacking> {
    let config = Config {
        block_cache_bytes: 8 << 20,
        write_buffer_bytes: 4 << 20,
        background_jobs: 2,
    };
    Arc::new(RocksBacking::open(dir, &config).expect("open store"))
}

fn outpoint(i: u32) -> OutPoint {
    let mut txid = [0u8; 32];
    txid[..4].copy_from_slice(&i.to_le_bytes());
    txid[31] = 0xcc;
    OutPoint::new(txid, i % 3)
}

fn coin(i: u32) -> Coin {
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend_from_slice(&[i as u8; 20]);
    script.extend_from_slice(&[0x88, 0xac]);
    Coin {
        value: u64::from(i) * 1_000 + 1,
        script_pubkey: Bytes::from(script),
        height: i / 10,
        is_coinbase: i.is_multiple_of(7),
    }
}

fn nullifier(i: u32) -> [u8; 32] {
    let mut nf = [0xeeu8; 32];
    nf[..4].copy_from_slice(&i.to_be_bytes());
    nf
}

#[test]
fn fresh_spent_coin_never_reaches_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backing = open(dir.path());
    let mut cache = CoinsCache::new(backing.clone());

    cache.add(outpoint(1), coin(1)).expect("add");
    cache.add(outpoint(2), coin(2)).expect("add");
    let spent = cache.spend(&outpoint(1)).expect("spend fresh");
    assert_eq!(spent, coin(1));
    assert_eq!(
        cache.len(),
        1,
        "fresh spent coin is removed, not tombstoned"
    );

    let stats = cache.flush().expect("flush");
    assert_eq!(stats, FlushStats { adds: 1, spends: 0 });

    let on_disk = backing
        .get_many(&[outpoint(1), outpoint(2)])
        .expect("get_many");
    assert_eq!(on_disk, vec![None, Some(coin(2))]);

    // A coin that was flushed and is spent later becomes a tombstone and one delete.
    cache.spend(&outpoint(2)).expect("spend flushed coin");
    assert_eq!(cache.dirty_len(), 1);
    assert_eq!(cache.fetch_many(&[outpoint(2)]).expect("fetch"), vec![None]);
    let stats = cache.flush().expect("flush");
    assert_eq!(stats, FlushStats { adds: 0, spends: 1 });
    assert!(cache.is_empty());
    assert_eq!(
        backing.get_many(&[outpoint(2)]).expect("get_many"),
        vec![None]
    );
    assert_eq!(cache.memory_bytes(), 0);
}

#[test]
fn flush_then_reopen_keeps_coins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let n = 1_000u32;
    {
        let backing = open(dir.path());
        let mut cache = CoinsCache::new(backing);
        for i in 0..n {
            cache.add(outpoint(i), coin(i)).expect("add");
        }
        // Spend a few before the flush so they must never show up.
        for i in (0..n).step_by(100) {
            cache.spend(&outpoint(i)).expect("spend");
        }
        let stats = cache.flush().expect("flush");
        assert_eq!(
            stats,
            FlushStats {
                adds: 990,
                spends: 0
            }
        );
    }
    let backing = open(dir.path());
    let cache = CoinsCache::new(backing.clone());
    let outpoints: Vec<OutPoint> = (0..n).map(outpoint).collect();
    // More than one multi_get chunk, so the parallel path and its ordering are exercised.
    let coins = cache.fetch_many(&outpoints).expect("fetch_many");
    for i in 0..n {
        let expected = if i % 100 == 0 { None } else { Some(coin(i)) };
        assert_eq!(coins[i as usize], expected, "coin {i}");
    }
    assert_eq!(cache.len(), 990);
    assert!(cache.memory_bytes() > 990 * 36);
}

#[test]
fn batched_lookups_match_single_lookups() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backing = open(dir.path());
    let mut cache = CoinsCache::new(backing.clone());
    // On disk: 0..300. In cache only (fresh): 300..350. Spent after flush: every 10th.
    for i in 0..300 {
        cache.add(outpoint(i), coin(i)).expect("add");
    }
    cache.flush().expect("flush");
    cache.drop_clean();
    for i in 300..350 {
        cache.add(outpoint(i), coin(i)).expect("add");
    }
    for i in (0..300).step_by(10) {
        cache.spend(&outpoint(i)).expect("spend");
    }

    let mut rng = StdRng::seed_from_u64(7);
    let queries: Vec<OutPoint> = (0..600).map(|_| outpoint(rng.gen_range(0..400))).collect();

    let batched = cache.fetch_many(&queries).expect("fetch_many");
    for (query, got) in queries.iter().zip(&batched) {
        assert_eq!(&cache.get_coin(query), got, "{query:?}");
    }
    let direct = backing.get_many(&queries).expect("get_many");
    for (query, got) in queries.iter().zip(&direct) {
        let single = backing
            .get_many(std::slice::from_ref(query))
            .expect("single get_many");
        assert_eq!(&single[0], got, "{query:?}");
    }
}

#[test]
fn spend_and_add_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backing = open(dir.path());
    let mut cache = CoinsCache::new(backing);

    assert!(matches!(
        cache.spend(&outpoint(9)),
        Err(Error::MissingCoin(_))
    ));
    cache.add(outpoint(9), coin(9)).expect("add");
    assert!(matches!(
        cache.add(outpoint(9), coin(10)),
        Err(Error::DuplicateCoin(_))
    ));
    cache.flush().expect("flush");
    cache.spend(&outpoint(9)).expect("spend");
    assert!(matches!(
        cache.spend(&outpoint(9)),
        Err(Error::MissingCoin(_))
    ));
    // Re-creating a spent outpoint overwrites the tombstone and is written at the flush.
    cache
        .add(outpoint(9), coin(11))
        .expect("add over tombstone");
    let stats = cache.flush().expect("flush");
    assert_eq!(stats, FlushStats { adds: 1, spends: 0 });
    assert_eq!(cache.get_coin(&outpoint(9)), Some(coin(11)));
}

#[test]
fn flush_if_over_and_drop_clean_account_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backing = open(dir.path());
    let mut cache = CoinsCache::new(backing);
    for i in 0..100 {
        cache.add(outpoint(i), coin(i)).expect("add");
    }
    let before = cache.memory_bytes();
    assert!(before > 100 * (36 + 25));
    assert_eq!(cache.flush_if_over(usize::MAX).expect("no flush"), None);
    assert_eq!(cache.dirty_len(), 100);
    let flushed = cache.flush_if_over(before - 1).expect("flush");
    assert_eq!(
        flushed,
        Some(FlushStats {
            adds: 100,
            spends: 0
        })
    );
    assert_eq!(cache.dirty_len(), 0);
    assert_eq!(
        cache.memory_bytes(),
        0,
        "over the limit: clean entries evicted"
    );
    assert!(cache.is_empty());

    // A plain flush keeps the clean entries; drop_clean keeps only the dirty ones.
    for i in 0..100 {
        cache.add(outpoint(i + 100), coin(i)).expect("add");
    }
    cache.flush().expect("flush");
    assert_eq!(cache.len(), 100);
    cache.add(outpoint(999), coin(1)).expect("add");
    cache.spend(&outpoint(100)).expect("spend flushed coin");
    cache.drop_clean();
    assert_eq!(cache.len(), 2, "fresh coin and tombstone survive");
    assert_eq!(cache.dirty_len(), 2);
    cache.flush().expect("flush");
    assert_eq!(cache.len(), 1);
    let entry_bytes = cache.memory_bytes();
    assert!(
        entry_bytes > 36 + 25 && entry_bytes < 200,
        "one clean entry"
    );
}

#[test]
fn nullifier_sets_are_per_pool_and_persist_on_flush() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backing = open(dir.path());
    let mut store = NullifierStore::new(backing.clone());

    let orchard: Vec<[u8; 32]> = (0..500).map(nullifier).collect();
    store.pool_mut(Pool::Orchard).insert_many(&orchard);
    assert_eq!(store.pool(Pool::Orchard).pending_len(), 500);

    // Pending nullifiers answer true before any flush, and only in their own pool.
    let probe = [nullifier(0), nullifier(499), nullifier(500)];
    assert_eq!(
        store
            .pool(Pool::Orchard)
            .contains_many(&probe)
            .expect("contains"),
        vec![true, true, false]
    );
    assert_eq!(
        store
            .pool(Pool::Sapling)
            .contains_many(&probe)
            .expect("contains"),
        vec![false, false, false]
    );
    assert_eq!(
        backing.contains_many(Pool::Orchard, &probe).expect("disk"),
        vec![false, false, false],
        "nothing on disk before the flush"
    );

    assert_eq!(store.flush().expect("flush"), 500);
    assert_eq!(store.pool(Pool::Orchard).pending_len(), 0);
    assert_eq!(store.flush().expect("empty flush"), 0);
    drop(store);
    drop(backing);

    let backing = open(dir.path());
    let reopened = NullifierSet::new(Pool::Orchard, backing.clone());
    assert_eq!(
        reopened.contains_many(&probe).expect("contains"),
        vec![true, true, false]
    );
    assert_eq!(
        backing.contains_many(Pool::Sapling, &probe).expect("disk"),
        vec![false, false, false]
    );
    // A mix of pending and flushed answers positionally.
    let mut mixed = NullifierSet::new(Pool::Orchard, backing);
    mixed.insert_many(&[nullifier(500)]);
    assert_eq!(
        mixed.contains_many(&probe).expect("contains"),
        vec![true, true, true]
    );
}

/// Phases one and three under a lock shared with readers, phase two outside it, as the
/// state writer runs them. A reader looks up every coin of the generation while the write
/// runs and never misses one; the writer changes entries of the generation during the
/// write and the next flush carries those changes.
#[test]
fn readers_see_every_coin_during_a_flush_of_a_large_generation() {
    use parking_lot::RwLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    let dir = tempfile::tempdir().expect("tempdir");
    let backing = open(dir.path());
    let n = 100_000u32;
    let cache = RwLock::new(CoinsCache::new(backing.clone()));
    for i in 0..n {
        cache.write().add(outpoint(i), coin(i)).expect("add");
    }
    let outpoints: Vec<OutPoint> = (0..n).map(outpoint).collect();

    let flush = cache.write().begin_flush().expect("begin");
    assert_eq!(flush.adds.len(), n as usize);
    assert_eq!(cache.read().dirty_len(), 0);
    let generation = FlushGeneration {
        adds: flush.adds,
        spends: flush.spends,
        nullifiers: Default::default(),
        best_block: BestBlock {
            height: 7,
            hash: [0x77; 32],
        },
    };

    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            let mut rounds = 0;
            // Coin 0 is the one the writer spends during the write.
            while !done.load(Ordering::Acquire) || rounds == 0 {
                for chunk in outpoints[1..].chunks(5_000) {
                    let coins = cache.read().fetch_many(chunk).expect("fetch");
                    for (o, c) in chunk.iter().zip(coins) {
                        let Some(_) = c else {
                            panic!("{o:?} went missing during the flush");
                        };
                    }
                }
                rounds += 1;
            }
        });
        // The writer keeps committing while the generation is written: a spend of a coin
        // of the generation and a new coin.
        {
            let mut w = cache.write();
            w.spend(&outpoint(0))
                .expect("spend a coin of the generation");
            w.add(outpoint(n), coin(n)).expect("add");
            assert_eq!(w.dirty_len(), 2);
        }
        backing.write_generation(&generation).expect("write");
        cache.write().end_flush().expect("end");
        {
            let w = cache.read();
            assert_eq!(
                w.dirty_len(),
                2,
                "the changes made during the write stay dirty"
            );
            assert_eq!(w.fetch_many(&[outpoint(0)]).expect("fetch"), vec![None]);
        }
        done.store(true, Ordering::Release);
    });

    // The next flush deletes the spent coin and writes the new one.
    let stats = cache.write().flush().expect("second flush");
    assert_eq!(stats, FlushStats { adds: 1, spends: 1 });
    assert_eq!(
        backing
            .get_many(&[outpoint(0), outpoint(1), outpoint(n)])
            .expect("disk"),
        vec![None, Some(coin(1)), Some(coin(n))]
    );
    assert_eq!(
        cache.read().len(),
        n as usize,
        "tombstone dropped, the rest cached"
    );
}

#[test]
fn best_block_is_written_with_the_generation_and_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let backing = open(dir.path());
        assert_eq!(backing.best_block().expect("read"), None);
        let generation = FlushGeneration {
            adds: vec![(outpoint(1), coin(1)), (outpoint(2), coin(2))],
            spends: vec![outpoint(3)],
            nullifiers: [
                vec![],
                vec![nullifier(1)],
                vec![nullifier(2), nullifier(3)],
                vec![],
            ],
            best_block: BestBlock {
                height: 2_600_000,
                hash: [0xab; 32],
            },
        };
        backing.write_generation(&generation).expect("write");
        assert_eq!(
            backing.best_block().expect("read"),
            Some(generation.best_block)
        );
    }
    let backing = open(dir.path());
    assert_eq!(
        backing.best_block().expect("read"),
        Some(BestBlock {
            height: 2_600_000,
            hash: [0xab; 32],
        })
    );
    assert_eq!(
        backing
            .get_many(&[outpoint(1), outpoint(2), outpoint(3)])
            .expect("disk"),
        vec![Some(coin(1)), Some(coin(2)), None]
    );
    assert_eq!(
        backing
            .contains_many(Pool::Orchard, &[nullifier(1), nullifier(2), nullifier(3)])
            .expect("disk"),
        vec![false, true, true]
    );
    assert_eq!(
        backing
            .contains_many(Pool::Sapling, &[nullifier(1)])
            .expect("disk"),
        vec![true]
    );
}

/// A process that stops after phase one, or after the write, leaves a store that is the
/// state after exactly one best block: the generation is one atomic batch.
#[test]
fn a_crash_between_the_flush_phases_leaves_one_consistent_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = BestBlock {
        height: 10,
        hash: [0x10; 32],
    };
    let second = BestBlock {
        height: 20,
        hash: [0x20; 32],
    };
    {
        let backing = open(dir.path());
        let mut cache = CoinsCache::new(backing.clone());
        let mut nullifiers = NullifierStore::new(backing.clone());
        for i in 0..100 {
            cache.add(outpoint(i), coin(i)).expect("add");
        }
        nullifiers
            .pool_mut(Pool::Orchard)
            .insert_many(&[nullifier(10)]);
        let coins = cache.begin_flush().expect("begin");
        let pools = nullifiers.begin_flush().expect("begin");
        backing
            .write_generation(&FlushGeneration {
                adds: coins.adds,
                spends: coins.spends,
                nullifiers: pools,
                best_block: first,
            })
            .expect("write");
        cache.end_flush().expect("end");
        nullifiers.end_flush().expect("end");

        // Blocks 11..=20: spend half of the coins, add new ones, reveal a nullifier; the
        // process stops after phase one of their flush.
        for i in (0..100).step_by(2) {
            cache.spend(&outpoint(i)).expect("spend");
        }
        for i in 100..150 {
            cache.add(outpoint(i), coin(i)).expect("add");
        }
        nullifiers
            .pool_mut(Pool::Orchard)
            .insert_many(&[nullifier(20)]);
        let coins = cache.begin_flush().expect("begin");
        let pools = nullifiers.begin_flush().expect("begin");
        // Reads still see the generation in flight.
        assert_eq!(
            nullifiers
                .pool(Pool::Orchard)
                .contains_many(&[nullifier(10), nullifier(20), nullifier(30)])
                .expect("contains"),
            vec![true, true, false]
        );
        assert_eq!(coins.adds.len(), 50);
        assert_eq!(coins.spends.len(), 50);
        assert_eq!(pools[Pool::Orchard.index()], vec![nullifier(20)]);
        // Dropped before the write: nothing of the second generation reaches the disk.
    }
    {
        let backing = open(dir.path());
        assert_eq!(backing.best_block().expect("read"), Some(first));
        let all: Vec<OutPoint> = (0..150).map(outpoint).collect();
        let on_disk = backing.get_many(&all).expect("disk");
        for i in 0..150u32 {
            let expected = if i < 100 { Some(coin(i)) } else { None };
            assert_eq!(
                on_disk[i as usize], expected,
                "coin {i} after the first record"
            );
        }
        assert_eq!(
            backing
                .contains_many(Pool::Orchard, &[nullifier(10), nullifier(20)])
                .expect("disk"),
            vec![true, false]
        );
        // Recovery replays blocks 11..=20 and writes their generation; a stop after that
        // write but before phase three changes nothing on disk.
        let mut cache = CoinsCache::new(backing.clone());
        for i in (0..100).step_by(2) {
            cache.spend(&outpoint(i)).expect("spend");
        }
        for i in 100..150 {
            cache.add(outpoint(i), coin(i)).expect("add");
        }
        let coins = cache.begin_flush().expect("begin");
        backing
            .write_generation(&FlushGeneration {
                adds: coins.adds,
                spends: coins.spends,
                nullifiers: [vec![], vec![], vec![nullifier(20)], vec![]],
                best_block: second,
            })
            .expect("write");
    }
    let backing = open(dir.path());
    assert_eq!(backing.best_block().expect("read"), Some(second));
    let all: Vec<OutPoint> = (0..150).map(outpoint).collect();
    let on_disk = backing.get_many(&all).expect("disk");
    for i in 0..150u32 {
        let expected = if i < 100 && i % 2 == 0 {
            None
        } else {
            Some(coin(i))
        };
        assert_eq!(
            on_disk[i as usize], expected,
            "coin {i} after the second record"
        );
    }
    assert_eq!(
        backing
            .contains_many(Pool::Orchard, &[nullifier(10), nullifier(20)])
            .expect("disk"),
        vec![true, true]
    );
}
