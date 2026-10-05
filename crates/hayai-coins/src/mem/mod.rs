//! In-memory implementation of [`CoinsBacking`]: the whole coin set and the nullifier sets
//! in memory, persisted with sequential writes only.
//!
//! Files in the store directory:
//!
//! - `coins.log`: one record per write ([`CoinsBacking::write_generation`],
//!   [`CoinsBacking::write_batch`], [`CoinsBacking::insert_many`]), appended and synced
//!   after every [`MemConfig::fsync_every_generations`] records. Each record has a sequence
//!   number.
//! - `coins.snapshot`: the whole set at one sequence number, written by
//!   [`MemBacking::snapshot`] to `coins.snapshot.tmp`, synced, and renamed into place. The
//!   log is then truncated.
//!
//! [`MemBacking::open`] loads the snapshot and replays the log records with a later
//! sequence number. A torn last record is cut from the log and reported in [`Recovery`];
//! damage anywhere else is an error. A crash between the rename and the truncation leaves
//! log records that the snapshot already holds; their sequence numbers are not later than
//! the snapshot's, so the replay skips them.
//!
//! Readers take a per-shard read lock, so a lookup runs in parallel with other lookups and
//! waits only for a write to the same shard. Writes apply after their record is appended,
//! shard by shard: a reader sees each coin either before or after a write, and never a
//! partly written coin.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use parking_lot::{Mutex, RwLock};
use rayon::prelude::*;

mod log;
mod nfset;
mod snapshot;
mod table;

use self::log::{read_record, Batch, Next};
use self::nfset::NullifierShard;
use self::table::{unpack, CoinShard, Entry, Key, Packed};
use crate::{outpoint_key, BestBlock, Coin, CoinsBacking, Error, FlushGeneration, OutPoint, Pool};

/// Shards of the coin set and of each nullifier set, chosen by the first key byte. Txids
/// and nullifiers can be ground to land in one shard; that makes the shard larger but not
/// slower per lookup, because the hash inside a shard has random keys.
const SHARDS: usize = 256;
/// Batched lookups of up to this many keys run on the calling thread.
const SEQUENTIAL_LOOKUP: usize = 1024;
const LOG_FILE: &str = "coins.log";
const SNAPSHOT_FILE: &str = "coins.snapshot";
const SNAPSHOT_TMP_FILE: &str = "coins.snapshot.tmp";

/// Tunables of [`MemBacking`].
#[derive(Clone, Debug)]
pub struct MemConfig {
    /// The log is synced to disk after every this many records. The default is 1: a write
    /// returns after its record is on disk. 0 never syncs; the kernel then writes the log
    /// back in its own time, as RocksDB does for [`crate::RocksBacking`]'s unsynced write
    /// batches, and a power loss can lose the last records (never the order or the
    /// integrity of the rest).
    pub fsync_every_generations: u32,
}

impl Default for MemConfig {
    fn default() -> Self {
        MemConfig {
            fsync_every_generations: 1,
        }
    }
}

/// What [`MemBacking::open`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// Log records applied on top of the snapshot.
    pub replayed: u64,
    /// Bytes of a torn last record cut from the end of the log; 0 when the log ended on a
    /// record boundary.
    pub torn_tail_bytes: u64,
}

/// Errors of the files of [`MemBacking`].
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("log {} is corrupt at byte {offset}: {reason}", path.display())]
    CorruptLog {
        path: PathBuf,
        offset: u64,
        reason: &'static str,
    },
    #[error("snapshot {} is corrupt: {reason}", path.display())]
    CorruptSnapshot { path: PathBuf, reason: String },
    #[error(
        "log {} takes no more writes: a failed append or sync left it in an unknown state",
        path.display()
    )]
    LogPoisoned { path: PathBuf },
}

fn io_error(path: &Path) -> impl Fn(io::Error) -> PersistError + '_ {
    move |source| PersistError::Io {
        path: path.to_owned(),
        source,
    }
}

fn sync_dir(dir: &Path) -> Result<(), PersistError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_error(dir))
}

/// The log file and the sequence number of its last record. Its lock serializes the
/// writes and the snapshot.
struct Writer {
    log: File,
    path: PathBuf,
    /// Length of the log up to the end of the last whole record.
    len: u64,
    /// Sequence number of the last record written (or of the snapshot, when the log has
    /// none after it).
    seq: u64,
    unsynced: u32,
    fsync_every: u32,
    /// Set when a failed append or sync leaves the log in an unknown state.
    poisoned: bool,
}

impl Writer {
    fn check(&self) -> Result<(), PersistError> {
        if self.poisoned {
            return Err(PersistError::LogPoisoned {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    fn append(&mut self, batch: &Batch) -> Result<(), PersistError> {
        self.check()?;
        let record = batch.encode(self.seq + 1);
        if let Err(source) = self.log.write_all(&record) {
            // Cut the partial record, so that the next append does not follow it.
            let Ok(()) = self.log.set_len(self.len) else {
                self.poisoned = true;
                return Err(io_error(&self.path)(source));
            };
            return Err(io_error(&self.path)(source));
        }
        self.len += record.len() as u64;
        self.seq += 1;
        self.unsynced += 1;
        if self.fsync_every != 0 && self.unsynced >= self.fsync_every {
            if let Err(source) = self.log.sync_data() {
                // The record may or may not be on disk; the caller sees the write fail
                // while a restart could replay it. No later write may build on that.
                self.poisoned = true;
                return Err(io_error(&self.path)(source));
            }
            self.unsynced = 0;
        }
        Ok(())
    }

    fn truncate(&mut self) -> Result<(), PersistError> {
        if let Err(source) = self.log.set_len(0).and_then(|()| self.log.sync_all()) {
            self.poisoned = true;
            return Err(io_error(&self.path)(source));
        }
        self.len = 0;
        self.unsynced = 0;
        Ok(())
    }
}

/// Coins and nullifier sets in memory, persisted by a log and snapshots.
///
/// A coin costs 69 bytes in a dense per-shard vector plus 5 bytes per bucket of a hash
/// index; a nullifier costs 32 bytes in a sorted run. `docs/architecture.md` (hayai-coins)
/// has the measured figures and the mainnet sizing.
pub struct MemBacking {
    dir: PathBuf,
    hasher: ahash::RandomState,
    coins: Box<[RwLock<CoinShard>]>,
    nullifiers: [Box<[RwLock<NullifierShard>]>; 4],
    best_block: RwLock<Option<BestBlock>>,
    writer: Mutex<Writer>,
}

fn shard_of(key: &[u8]) -> usize {
    usize::from(key[0])
}

/// Answers a batch of keys shard by shard: `lookup(shard, positions)` runs once for every
/// shard the batch touches, with the positions in `keys` of that shard's keys, and returns
/// one answer per position. The answers come back in key order.
///
/// One lock per shard instead of one per key is the point: a lock is an atomic
/// read-modify-write, which on x86 also orders the memory accesses around it, so the cache
/// misses of consecutive keys cannot overlap (13,000 coin lookups on one thread: 1.57 ms
/// with a lock per key, 1.18 ms grouped). Large batches run the shards on the rayon pool.
fn by_shard<K, T>(keys: &[K], lookup: impl Fn(usize, &[u32]) -> Vec<T> + Sync) -> Vec<T>
where
    K: AsRef<[u8]> + Sync,
    T: Send + Clone + Default,
{
    let mut starts = [0usize; SHARDS + 1];
    let shards: Vec<u8> = keys.iter().map(|k| shard_of(k.as_ref()) as u8).collect();
    for &s in &shards {
        starts[usize::from(s) + 1] += 1;
    }
    for s in 0..SHARDS {
        starts[s + 1] += starts[s];
    }
    let mut fill = starts;
    let mut order = vec![0u32; keys.len()];
    for (position, &s) in shards.iter().enumerate() {
        order[fill[usize::from(s)]] = position as u32;
        fill[usize::from(s)] += 1;
    }
    let run = |s: usize| {
        let positions = &order[starts[s]..starts[s + 1]];
        match positions {
            [] => Vec::new(),
            _ => lookup(s, positions),
        }
    };
    let answers: Vec<Vec<T>> = match keys.len() <= SEQUENTIAL_LOOKUP {
        true => (0..SHARDS).map(run).collect(),
        false => (0..SHARDS).into_par_iter().map(run).collect(),
    };
    let mut out = vec![T::default(); keys.len()];
    for (s, answers) in answers.into_iter().enumerate() {
        for (&position, answer) in order[starts[s]..starts[s + 1]].iter().zip(answers) {
            out[position as usize] = answer;
        }
    }
    out
}

impl MemBacking {
    /// Opens or creates the store in `dir`: loads the snapshot, replays the later log
    /// records and cuts a torn last record.
    pub fn open(dir: &Path, config: &MemConfig) -> Result<(MemBacking, Recovery), Error> {
        fs::create_dir_all(dir).map_err(io_error(dir))?;
        let hasher = ahash::RandomState::new();
        let tmp = dir.join(SNAPSHOT_TMP_FILE);
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(&tmp)(e).into()),
        }
        let snapshot_path = dir.join(SNAPSHOT_FILE);
        let loaded = match snapshot_path
            .try_exists()
            .map_err(io_error(&snapshot_path))?
        {
            true => snapshot::load(&snapshot_path, &hasher)?,
            false => snapshot::Loaded {
                seq: 0,
                best_block: None,
                coins: (0..SHARDS)
                    .map(|_| CoinShard::with_capacity(0, &hasher))
                    .collect(),
                nullifiers: std::array::from_fn(|_| {
                    (0..SHARDS).map(|_| NullifierShard::new(&hasher)).collect()
                }),
            },
        };

        let log_path = dir.join(LOG_FILE);
        let created = !log_path.try_exists().map_err(io_error(&log_path))?;
        let log = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&log_path)
            .map_err(io_error(&log_path))?;
        if created {
            sync_dir(dir)?;
        }
        let backing = MemBacking {
            dir: dir.to_owned(),
            coins: loaded.coins.into_iter().map(RwLock::new).collect(),
            nullifiers: loaded
                .nullifiers
                .map(|pool| pool.into_iter().map(RwLock::new).collect()),
            best_block: RwLock::new(loaded.best_block),
            hasher,
            writer: Mutex::new(Writer {
                log,
                path: log_path,
                len: 0,
                seq: loaded.seq,
                unsynced: 0,
                fsync_every: config.fsync_every_generations,
                poisoned: false,
            }),
        };
        let recovery = backing.replay(loaded.seq)?;
        Ok((backing, recovery))
    }

    /// Applies the log records after `snapshot_seq` and cuts a torn tail.
    fn replay(&self, snapshot_seq: u64) -> Result<Recovery, PersistError> {
        let mut writer = self.writer.lock();
        let path = writer.path.clone();
        let file_len = writer.log.metadata().map_err(io_error(&path))?.len();
        let corrupt = |offset, reason| PersistError::CorruptLog {
            path: path.clone(),
            offset,
            reason,
        };
        let mut offset = 0u64;
        let mut last_seq: Option<u64> = None;
        let mut recovery = Recovery {
            replayed: 0,
            torn_tail_bytes: 0,
        };
        loop {
            match read_record(&mut writer.log, offset, file_len).map_err(io_error(&path))? {
                Next::Record { seq, batch, len } => {
                    match last_seq {
                        Some(prev) if seq != prev + 1 => {
                            return Err(corrupt(offset, "sequence numbers are not consecutive"))
                        }
                        None if seq > snapshot_seq + 1 => {
                            return Err(corrupt(
                                offset,
                                "the log starts after the snapshot: records are missing",
                            ))
                        }
                        _ => {}
                    }
                    last_seq = Some(seq);
                    if seq > snapshot_seq {
                        self.apply(batch);
                        recovery.replayed += 1;
                    }
                    offset += len;
                }
                Next::End => break,
                Next::TornTail => {
                    recovery.torn_tail_bytes = file_len - offset;
                    writer
                        .log
                        .set_len(offset)
                        .and_then(|()| writer.log.sync_all())
                        .map_err(io_error(&path))?;
                    break;
                }
                Next::Corrupt(reason) => return Err(corrupt(offset, reason)),
            }
        }
        writer.len = offset;
        if let Some(seq) = last_seq {
            writer.seq = writer.seq.max(seq);
        }
        Ok(recovery)
    }

    /// Applies a batch to the shards: adds, then spends, then nullifiers, each shard under
    /// its own write lock and the shards in parallel.
    fn apply(&self, batch: Batch) {
        let Batch {
            best_block,
            adds,
            spends,
            nullifiers,
        } = batch;
        let mut work: Vec<(Vec<Packed>, Vec<Key>)> =
            (0..SHARDS).map(|_| Default::default()).collect();
        for packed in adds {
            work[shard_of(packed.0.key())].0.push(packed);
        }
        for key in spends {
            work[shard_of(&key)].1.push(key);
        }
        work.into_par_iter()
            .enumerate()
            .filter(|(_, (adds, spends))| !adds.is_empty() || !spends.is_empty())
            .for_each(|(s, (adds, spends))| {
                let mut shard = self.coins[s].write();
                for packed in adds {
                    shard.insert(&self.hasher, packed);
                }
                for key in &spends {
                    shard.remove(&self.hasher, key);
                }
            });
        for (pool, pool_nullifiers) in nullifiers.into_iter().enumerate() {
            if pool_nullifiers.is_empty() {
                continue;
            }
            let mut work: Vec<Vec<[u8; 32]>> = vec![Vec::new(); SHARDS];
            for nullifier in pool_nullifiers {
                work[shard_of(&nullifier)].push(nullifier);
            }
            work.into_par_iter()
                .enumerate()
                .filter(|(_, nullifiers)| !nullifiers.is_empty())
                .for_each(|(s, nullifiers)| {
                    let mut shard = self.nullifiers[pool][s].write();
                    for nullifier in nullifiers {
                        shard.insert(nullifier);
                    }
                });
        }
        if let Some(best) = best_block {
            *self.best_block.write() = Some(best);
        }
    }

    /// Appends the batch to the log, then applies it.
    fn commit(&self, batch: Batch) -> Result<(), Error> {
        let mut writer = self.writer.lock();
        writer.append(&batch)?;
        self.apply(batch);
        Ok(())
    }

    /// The block of the last generation written, `None` before the first one. Recovery
    /// replays the blocks after it, as with [`crate::RocksBacking::best_block`].
    pub fn best_block(&self) -> Result<Option<BestBlock>, Error> {
        Ok(*self.best_block.read())
    }

    /// Number of unspent coins held.
    pub fn coin_count(&self) -> usize {
        self.coins.iter().map(|shard| shard.read().len()).sum()
    }

    /// Writes the whole set at the current best block as one sequential file, renames it
    /// into place and truncates the log. Returns the snapshot size in bytes.
    ///
    /// Writes wait for the snapshot (it holds the writer lock, so the set does not change
    /// under it); lookups do not.
    pub fn snapshot(&self) -> Result<u64, Error> {
        let mut writer = self.writer.lock();
        writer.check()?;
        let tmp = self.dir.join(SNAPSHOT_TMP_FILE);
        let path = self.dir.join(SNAPSHOT_FILE);
        let best_block = *self.best_block.read();
        let size = snapshot::write(&tmp, writer.seq, best_block, &self.coins, &self.nullifiers)
            .map_err(io_error(&tmp))?;
        fs::rename(&tmp, &path).map_err(io_error(&path))?;
        sync_dir(&self.dir)?;
        writer.truncate()?;
        Ok(size)
    }
}

impl CoinsBacking for MemBacking {
    fn get_many(&self, outpoints: &[OutPoint]) -> Result<Vec<Option<Coin>>, Error> {
        let keys: Vec<Key> = outpoints.iter().map(outpoint_key).collect();
        Ok(by_shard(&keys, |s, positions| {
            // Copy the entries under the lock, then rebuild the coins (one allocation
            // each) outside it.
            let packed: Vec<Option<Packed>> = {
                let shard = self.coins[s].read();
                positions
                    .iter()
                    .map(|&p| shard.get(&self.hasher, &keys[p as usize]))
                    .collect()
            };
            packed.iter().map(|p| p.as_ref().map(unpack)).collect()
        }))
    }

    fn write_batch(&self, adds: &[(&OutPoint, &Coin)], spends: &[&OutPoint]) -> Result<(), Error> {
        if adds.is_empty() && spends.is_empty() {
            return Ok(());
        }
        self.commit(Batch {
            best_block: None,
            adds: adds
                .iter()
                .map(|(outpoint, coin)| Entry::pack(outpoint_key(outpoint), coin))
                .collect(),
            spends: spends
                .iter()
                .map(|outpoint| outpoint_key(outpoint))
                .collect(),
            nullifiers: Default::default(),
        })
    }

    fn contains_many(&self, pool: Pool, nullifiers: &[[u8; 32]]) -> Result<Vec<bool>, Error> {
        let shards = &self.nullifiers[pool.index()];
        Ok(by_shard(nullifiers, |s, positions| {
            let shard = shards[s].read();
            positions
                .iter()
                .map(|&p| shard.contains(&nullifiers[p as usize]))
                .collect()
        }))
    }

    fn insert_many(&self, pool: Pool, nullifiers: &[[u8; 32]]) -> Result<(), Error> {
        if nullifiers.is_empty() {
            return Ok(());
        }
        let mut batch = Batch::default();
        batch.nullifiers[pool.index()] = nullifiers.to_vec();
        self.commit(batch)
    }

    fn write_generation(&self, generation: &FlushGeneration) -> Result<(), Error> {
        self.commit(Batch {
            best_block: Some(generation.best_block),
            adds: generation
                .adds
                .iter()
                .map(|(outpoint, coin)| Entry::pack(outpoint_key(outpoint), coin))
                .collect(),
            spends: generation.spends.iter().map(outpoint_key).collect(),
            nullifiers: generation.nullifiers.clone(),
        })
    }
}
