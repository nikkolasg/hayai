//! RocksDB implementation of [`CoinsBacking`].

use std::path::Path;

use rayon::prelude::*;
use rocksdb::{
    BlockBasedOptions, Cache, ColumnFamilyDescriptor, DBCompressionType, DataBlockIndexType,
    Options, WriteBatch, DB,
};

use crate::{outpoint_key, BestBlock, Coin, CoinsBacking, Error, FlushGeneration, OutPoint, Pool};

/// Tunables of the on-disk store.
#[derive(Clone, Debug)]
pub struct Config {
    /// Size of the LRU block cache shared by every column family. The working set (recent
    /// coins, filters and indexes) should fit; the default is 256 MiB, which holds the whole
    /// 2 M-coin benchmark store (lookup of 13,000 inputs measured equal at 256 MiB and
    /// 1 GiB) and fits a small node. A node with a larger coin set raises it.
    pub block_cache_bytes: usize,
    /// Memtable size per column family. The default is 64 MiB: a flush of 26,000 records took
    /// ~35 ms into a 32 MiB memtable and ~65 ms into a 256 MiB one (skiplist inserts slow
    /// down with memtable size), while a larger memtable only defers SST writes that happen
    /// in the background anyway.
    pub write_buffer_bytes: usize,
    /// RocksDB background threads for flushes and compactions. The default is 4.
    pub background_jobs: i32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            block_cache_bytes: 256 << 20,
            write_buffer_bytes: 64 << 20,
            background_jobs: 4,
        }
    }
}

const CF_COINS: &str = "coins";
const CF_NULLIFIERS: [&str; 4] = ["nf_sprout", "nf_sapling", "nf_orchard", "nf_ironwood"];
/// Store-level records: the best block.
const CF_META: &str = "meta";
const KEY_BEST_BLOCK: &[u8] = b"best_block";
/// `height LE || hash`.
const BEST_BLOCK_BYTES: usize = 36;

/// Keys per `multi_get` call. RocksDB's `MultiGet` is single-threaded per call, so a block's
/// thousands of lookups are split into chunks that run on the rayon pool; each chunk still
/// batches its filter checks and block reads inside RocksDB.
const MULTI_GET_CHUNK: usize = 256;

/// Coins and nullifier sets on RocksDB.
///
/// Column families: `coins` (36-byte outpoint key to encoded [`Coin`]), `nf_sprout`,
/// `nf_sapling`, `nf_orchard`, `nf_ironwood` (32-byte nullifier key to an empty value) and
/// `meta` (`best_block` to `height LE || hash`, written in the same batch as the coins and
/// nullifiers of a generation).
pub struct RocksBacking {
    db: DB,
}

impl RocksBacking {
    /// Opens or creates the store at `path`.
    pub fn open(path: &Path, config: &Config) -> Result<Self, Error> {
        let cache = Cache::new_lru_cache(config.block_cache_bytes);

        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);
        // Keep every SST file open: a point lookup then never goes through the table cache
        // and never pays a file open on a miss of that cache.
        db_opts.set_max_open_files(-1);
        db_opts.set_max_background_jobs(config.background_jobs);
        // Flushes and compactions run in the background; writes should not stall the state
        // writer that calls flush on the coins cache.
        db_opts.set_max_total_wal_size(1 << 30);

        let cfs = vec![
            ColumnFamilyDescriptor::new(CF_COINS, point_lookup_options(config, &cache)),
            ColumnFamilyDescriptor::new(CF_NULLIFIERS[0], point_lookup_options(config, &cache)),
            ColumnFamilyDescriptor::new(CF_NULLIFIERS[1], point_lookup_options(config, &cache)),
            ColumnFamilyDescriptor::new(CF_NULLIFIERS[2], point_lookup_options(config, &cache)),
            ColumnFamilyDescriptor::new(CF_NULLIFIERS[3], point_lookup_options(config, &cache)),
            ColumnFamilyDescriptor::new(CF_META, Options::default()),
        ];
        let db = DB::open_cf_descriptors(&db_opts, path, cfs)?;
        Ok(RocksBacking { db })
    }

    /// The block whose state the store holds: the record of the last generation written,
    /// `None` for a store no generation reached yet. Recovery replays the blocks after it.
    pub fn best_block(&self) -> Result<Option<BestBlock>, Error> {
        let cf = self.cf(CF_META)?;
        let Some(bytes) = self.db.get_pinned_cf(cf, KEY_BEST_BLOCK)? else {
            return Ok(None);
        };
        let Some((height, hash)) = bytes
            .split_first_chunk::<4>()
            .filter(|(_, hash)| hash.len() == BEST_BLOCK_BYTES - 4)
        else {
            return Err(Error::MalformedBestBlock(bytes.len()));
        };
        Ok(Some(BestBlock {
            height: u32::from_le_bytes(*height),
            hash: hash.try_into().expect("32 bytes"),
        }))
    }

    fn cf(&self, name: &'static str) -> Result<&rocksdb::ColumnFamily, Error> {
        let Some(cf) = self.db.cf_handle(name) else {
            return Err(Error::MissingColumnFamily(name));
        };
        Ok(cf)
    }

    /// Runs one `multi_get` per chunk of `keys` in parallel and maps every found value with
    /// `decode(position, value)` straight from RocksDB's pinned slice (no intermediate
    /// copy); the result is positional.
    fn multi_get_chunked<K, T, F>(
        &self,
        cf: &rocksdb::ColumnFamily,
        keys: &[K],
        decode: F,
    ) -> Result<Vec<Option<T>>, Error>
    where
        K: AsRef<[u8]> + Sync,
        T: Send,
        F: Fn(usize, &[u8]) -> Result<T, Error> + Sync,
    {
        let get_chunk = |base: usize, chunk: &[K]| -> Result<Vec<Option<T>>, Error> {
            self.db
                .batched_multi_get_cf(cf, chunk, false)
                .into_iter()
                .enumerate()
                .map(|(i, slot)| match slot? {
                    Some(slice) => Ok(Some(decode(base + i, &slice)?)),
                    None => Ok(None),
                })
                .collect()
        };
        if keys.len() <= MULTI_GET_CHUNK {
            return get_chunk(0, keys);
        }
        let chunks: Vec<Vec<Option<T>>> = keys
            .par_chunks(MULTI_GET_CHUNK)
            .enumerate()
            .map(|(c, chunk)| get_chunk(c * MULTI_GET_CHUNK, chunk))
            .collect::<Result<_, _>>()?;
        Ok(chunks.into_iter().flatten().collect())
    }
}

/// Column family options tuned for point lookups by random 32-to-36-byte keys. The same
/// settings serve the coins and the nullifier sets: both are hash-keyed, never iterated, and
/// mostly queried for keys that are not there (spent coins, fresh nullifiers).
fn point_lookup_options(config: &Config, cache: &Cache) -> Options {
    let mut table = BlockBasedOptions::default();
    // Ribbon filter at 10 bits per key: a nullifier check or a double-spend check is a lookup
    // of a key that must not exist, and the filter answers it without touching a data block.
    // Ribbon costs about 30 % less memory than a Bloom filter at the same false positive rate.
    table.set_ribbon_filter(10.0);
    // One cache for data, index and filter blocks, sized from the configuration instead of
    // RocksDB's 32 MiB default; index and filter blocks are charged to it so their footprint
    // is bounded and visible.
    table.set_block_cache(cache);
    table.set_cache_index_and_filter_blocks(true);
    table.set_cache_index_and_filter_blocks_with_high_priority(true);
    // Pin the filter and index blocks of level 0 files and the top-level index of partitioned
    // ones so that cache pressure from data blocks never evicts the structures every lookup
    // needs first.
    table.set_pin_l0_filter_and_index_blocks_in_cache(true);
    table.set_pin_top_level_index_and_filter(true);
    // Hash index inside data blocks turns the binary search over a block's keys into a
    // direct probe (this is what `optimize_for_point_lookup` sets).
    table.set_data_block_index_type(DataBlockIndexType::BinaryAndHash);
    table.set_data_block_hash_ratio(0.75);
    // Small blocks: a point read pulls 4 KiB of mostly unrelated coins into the cache rather
    // than 16 KiB or more.
    table.set_block_size(4096);
    // Format version 5 is the most recent with the smaller index encoding.
    table.set_format_version(5);

    let mut opts = Options::default();
    opts.set_block_based_table_factory(&table);
    // No compression: a coin record is a 36-byte random key plus ~40 bytes of value whose bulk
    // is a script holding a 20-byte hash, and a nullifier is 32 random bytes. LZ4 would gain
    // a few percent of space and cost a decompression on every data block read.
    opts.set_compression_type(DBCompressionType::None);
    // Memtable Bloom filter on whole keys so that a point lookup of a key that is not in the
    // memtable skips the skiplist search (also part of `optimize_for_point_lookup`).
    opts.set_memtable_whole_key_filtering(true);
    opts.set_memtable_prefix_bloom_ratio(0.02);
    opts.set_write_buffer_size(config.write_buffer_bytes);
    // Dynamic level sizing keeps the bottom level at ~90 % of the data so a lookup checks as
    // few levels as possible and space amplification stays near 1.1.
    opts.set_level_compaction_dynamic_level_bytes(true);
    opts
}

/// Appends the encoded coins and spends of one flush to `batch`.
fn put_coins<'a>(
    batch: &mut WriteBatch,
    cf: &rocksdb::ColumnFamily,
    adds: impl Iterator<Item = (&'a OutPoint, &'a Coin)>,
    spends: impl Iterator<Item = &'a OutPoint>,
) {
    let mut value = Vec::new();
    for (outpoint, coin) in adds {
        value.clear();
        coin.encode_into(&mut value);
        batch.put_cf(cf, outpoint_key(outpoint), &value);
    }
    for outpoint in spends {
        batch.delete_cf(cf, outpoint_key(outpoint));
    }
}

impl CoinsBacking for RocksBacking {
    fn get_many(&self, outpoints: &[OutPoint]) -> Result<Vec<Option<Coin>>, Error> {
        let cf = self.cf(CF_COINS)?;
        let keys: Vec<[u8; 36]> = outpoints.iter().map(outpoint_key).collect();
        self.multi_get_chunked(cf, &keys, |position, bytes| {
            Coin::decode(bytes).map_err(|reason| Error::Malformed {
                outpoint: outpoints[position].clone(),
                reason,
            })
        })
    }

    fn write_batch(&self, adds: &[(&OutPoint, &Coin)], spends: &[&OutPoint]) -> Result<(), Error> {
        let cf = self.cf(CF_COINS)?;
        let mut batch = WriteBatch::default();
        put_coins(
            &mut batch,
            cf,
            adds.iter().map(|(o, c)| (*o, *c)),
            spends.iter().copied(),
        );
        self.db.write(batch)?;
        Ok(())
    }

    fn contains_many(&self, pool: Pool, nullifiers: &[[u8; 32]]) -> Result<Vec<bool>, Error> {
        let cf = self.cf(CF_NULLIFIERS[pool.index()])?;
        let values = self.multi_get_chunked(cf, nullifiers, |_, _| Ok(()))?;
        Ok(values
            .into_iter()
            .map(|value| {
                let Some(()) = value else { return false };
                true
            })
            .collect())
    }

    fn insert_many(&self, pool: Pool, nullifiers: &[[u8; 32]]) -> Result<(), Error> {
        let cf = self.cf(CF_NULLIFIERS[pool.index()])?;
        let mut batch = WriteBatch::default();
        for nullifier in nullifiers {
            batch.put_cf(cf, nullifier, []);
        }
        self.db.write(batch)?;
        Ok(())
    }

    fn write_generation(&self, generation: &FlushGeneration) -> Result<(), Error> {
        let mut batch = WriteBatch::default();
        put_coins(
            &mut batch,
            self.cf(CF_COINS)?,
            generation.adds.iter().map(|(o, c)| (o, c)),
            generation.spends.iter(),
        );
        for pool in Pool::ALL {
            let cf = self.cf(CF_NULLIFIERS[pool.index()])?;
            for nullifier in &generation.nullifiers[pool.index()] {
                batch.put_cf(cf, nullifier, []);
            }
        }
        let mut record = [0u8; BEST_BLOCK_BYTES];
        record[..4].copy_from_slice(&generation.best_block.height.to_le_bytes());
        record[4..].copy_from_slice(&generation.best_block.hash);
        batch.put_cf(self.cf(CF_META)?, KEY_BEST_BLOCK, record);
        self.db.write(batch)?;
        Ok(())
    }
}
