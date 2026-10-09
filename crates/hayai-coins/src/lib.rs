//! Outpoint-keyed coins and nullifier stores with an in-memory cache and batched flush.
//!
//! The model is Bitcoin Core's `CCoinsViewCache`: a block commit adds and spends coins in a
//! hash map owned by the state writer, and the disk only sees the net effect of a window of
//! blocks when [`CoinsCache::flush`] writes one batch. A coin created and spent inside the
//! window never reaches disk. Reads for a block are issued as one batched lookup
//! ([`CoinsView::get_coins`]), cache hits first and one [`CoinsBacking::get_many`] for the
//! misses.
//!
//! Layers:
//!
//! - [`CoinsBacking`] is the disk: batched point reads and one write batch per flush, plus the
//!   per-pool nullifier sets. `RocksBacking` implements it on RocksDB, under the feature
//!   `rocksdb`. [`MemBacking`] keeps the whole set in memory and persists it with an
//!   append-only log and snapshots.
//! - [`CoinsCache`] sits in front of a backing and is the only writer.
//! - [`NullifierSet`] and [`NullifierStore`] keep the unflushed nullifiers in memory in front of
//!   the backing's sets.
//!
//! A flush has three phases so that the state writer holds its lock only for the two short
//! ones: `begin_flush` takes the dirty coins and the pending nullifiers into a
//! [`FlushGeneration`] (the entries stay readable from the caches), the generation is
//! written outside the lock as one atomic batch together with its [`BestBlock`] record
//! ([`CoinsBacking::write_generation`]), and `end_flush` marks the written entries clean.
//! Recovery after a crash reads the best block (`best_block` of the backing) and replays
//! the blocks after it: the batch is atomic, so the disk is always the state after exactly
//! that block.

use std::fmt;

use bytes::Bytes;
use hayai_crypto::zcash_transparent;
pub use zcash_transparent::bundle::OutPoint;

mod cache;
mod mem;
mod nullifiers;
#[cfg(feature = "rocksdb")]
mod rocks;

pub use cache::{CoinsCache, CoinsFlush, FlushStats};
pub use mem::{MemBacking, MemConfig, PersistError, Recovery};
pub use nullifiers::{NullifierSet, NullifierStore};
#[cfg(feature = "rocksdb")]
pub use rocks::{Config, RocksBacking};

/// The best block of the coins store in `dir`, of the memory backend or of the RocksDB
/// backend, read without a write to `dir` and without a load of the coin set. It is the
/// value that `best_block` gives after an open of the store.
///
/// Without the feature `rocksdb`, a directory without the log of the memory backend is an
/// error.
///
/// The function is safe while a node has the store open: it gives a best block that the
/// store had, or an error when the node changes the files during the read.
pub fn stored_best_block(dir: &std::path::Path) -> Result<Option<BestBlock>, Error> {
    // The memory backend makes its log at the first open.
    match dir.join(mem::LOG_FILE).exists() {
        true => MemBacking::stored_best_block(dir),
        #[cfg(feature = "rocksdb")]
        false => RocksBacking::stored_best_block(dir),
        // The open of the missing log gives the error.
        #[cfg(not(feature = "rocksdb"))]
        false => MemBacking::stored_best_block(dir),
    }
}

/// An unspent transparent output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coin {
    /// Value in zatoshis.
    pub value: u64,
    /// The output's locking script.
    pub script_pubkey: Bytes,
    /// Height of the block that created the coin.
    pub height: u32,
    /// Whether the creating transaction is a coinbase (for the maturity rule).
    pub is_coinbase: bool,
}

/// Fixed part of the on-disk coin record: value (8) + height (4) + coinbase flag (1).
const COIN_HEADER_BYTES: usize = 13;

impl Coin {
    /// Serializes the coin as `value LE || height LE || is_coinbase || script_pubkey`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(COIN_HEADER_BYTES + self.script_pubkey.len());
        self.encode_into(&mut out);
        out
    }

    /// Appends the [`Coin::encode`] record to `out` (a flush reuses one buffer).
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.value.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.push(u8::from(self.is_coinbase));
        out.extend_from_slice(&self.script_pubkey);
    }

    /// Parses a record written by [`Coin::encode`].
    pub fn decode(bytes: &[u8]) -> Result<Self, MalformedCoin> {
        let Some((header, script)) = bytes.split_at_checked(COIN_HEADER_BYTES) else {
            return Err(MalformedCoin::TooShort(bytes.len()));
        };
        let value = u64::from_le_bytes(header[0..8].try_into().expect("8 bytes"));
        let height = u32::from_le_bytes(header[8..12].try_into().expect("4 bytes"));
        let is_coinbase = match header[12] {
            0 => false,
            1 => true,
            other => return Err(MalformedCoin::CoinbaseFlag(other)),
        };
        Ok(Coin {
            value,
            script_pubkey: Bytes::copy_from_slice(script),
            height,
            is_coinbase,
        })
    }

    /// Approximate heap plus inline size of the coin, for cache accounting.
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Coin>() + self.script_pubkey.len()
    }
}

/// A coin record on disk that does not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MalformedCoin {
    #[error("coin record of {0} bytes is shorter than the fixed header")]
    TooShort(usize),
    #[error("coin record has coinbase flag {0}, expected 0 or 1")]
    CoinbaseFlag(u8),
}

/// Size of the on-disk key of a coin: txid (32) followed by the output index (4, LE).
pub const OUTPOINT_KEY_BYTES: usize = 36;

/// Encodes an outpoint as its 36-byte key `txid || index LE`.
pub fn outpoint_key(outpoint: &OutPoint) -> [u8; OUTPOINT_KEY_BYTES] {
    let mut key = [0u8; OUTPOINT_KEY_BYTES];
    key[..32].copy_from_slice(outpoint.hash());
    key[32..].copy_from_slice(&outpoint.n().to_le_bytes());
    key
}

/// The shielded pools with a nullifier set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pool {
    Sprout,
    Sapling,
    Orchard,
    Ironwood,
}

impl Pool {
    /// Every pool, in column family order.
    pub const ALL: [Pool; 4] = [Pool::Sprout, Pool::Sapling, Pool::Orchard, Pool::Ironwood];

    /// Position in [`Pool::ALL`].
    pub const fn index(self) -> usize {
        match self {
            Pool::Sprout => 0,
            Pool::Sapling => 1,
            Pool::Orchard => 2,
            Pool::Ironwood => 3,
        }
    }
}

impl fmt::Display for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Pool::Sprout => "sprout",
            Pool::Sapling => "sapling",
            Pool::Orchard => "orchard",
            Pool::Ironwood => "ironwood",
        };
        f.write_str(name)
    }
}

/// Errors of the coins and nullifier stores.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[cfg(feature = "rocksdb")]
    #[error("rocksdb: {0}")]
    Rocks(#[from] rocksdb::Error),
    #[cfg(feature = "rocksdb")]
    #[error("column family {0} is missing from the database")]
    MissingColumnFamily(&'static str),
    #[error("coin {outpoint:?}: {reason}")]
    Malformed {
        outpoint: OutPoint,
        reason: MalformedCoin,
    },
    #[error("best block record of {0} bytes is malformed")]
    MalformedBestBlock(usize),
    #[error("coin {0:?} already exists unspent")]
    DuplicateCoin(OutPoint),
    #[error("coin {0:?} is missing or already spent")]
    MissingCoin(OutPoint),
    #[error("a flush is already in flight")]
    FlushInFlight,
    #[error("no flush is in flight")]
    NoFlushInFlight,
    #[error(transparent)]
    Persist(#[from] PersistError),
}

/// The block whose state the disk holds after a flush.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BestBlock {
    pub height: u32,
    pub hash: [u8; 32],
}

/// One flush, taken from the caches under the state lock and written outside it: the coins
/// to write and delete, the nullifiers to insert per pool ([`Pool::index`]) and the best
/// block record, all in one atomic batch.
#[derive(Debug)]
pub struct FlushGeneration {
    pub adds: Vec<(OutPoint, Coin)>,
    pub spends: Vec<OutPoint>,
    pub nullifiers: [Vec<[u8; 32]>; 4],
    pub best_block: BestBlock,
}

impl FlushGeneration {
    /// What writing the generation does to the coins on disk.
    pub fn stats(&self) -> FlushStats {
        FlushStats {
            adds: self.adds.len(),
            spends: self.spends.len(),
        }
    }

    /// Nullifiers across all pools.
    pub fn nullifier_count(&self) -> usize {
        self.nullifiers.iter().map(Vec::len).sum()
    }
}

/// Read access to a coin set. The batched form is the primary one: a block names every
/// outpoint it spends, so a validator fetches them in one call.
pub trait CoinsView: Send + Sync {
    /// Looks up many outpoints at once; the result is positional.
    fn get_coins(&self, outpoints: &[OutPoint]) -> Vec<Option<Coin>>;

    /// Looks up one outpoint.
    fn get_coin(&self, outpoint: &OutPoint) -> Option<Coin> {
        let mut coins = self.get_coins(std::slice::from_ref(outpoint));
        let Some(coin) = coins.pop() else {
            unreachable!("get_coins returns one slot per outpoint");
        };
        coin
    }
}

/// The persistent store behind a [`CoinsCache`]: coins plus one nullifier set per pool.
///
/// Every method is a batch so that the database sees one round trip per block. Errors are
/// database failures; a caller that cannot continue without the store treats them as fatal.
pub trait CoinsBacking: Send + Sync {
    /// Reads many coins in one round; the result is positional.
    fn get_many(&self, outpoints: &[OutPoint]) -> Result<Vec<Option<Coin>>, Error>;

    /// Atomically writes new coins and deletes spent ones.
    fn write_batch(&self, adds: &[(&OutPoint, &Coin)], spends: &[&OutPoint]) -> Result<(), Error>;

    /// Membership test for many nullifiers of one pool; the result is positional.
    fn contains_many(&self, pool: Pool, nullifiers: &[[u8; 32]]) -> Result<Vec<bool>, Error>;

    /// Atomically inserts many nullifiers into one pool's set.
    fn insert_many(&self, pool: Pool, nullifiers: &[[u8; 32]]) -> Result<(), Error>;

    /// Atomically writes a whole generation: its coins, its nullifiers and its best block
    /// record. Either all of it is on disk or none of it.
    fn write_generation(&self, generation: &FlushGeneration) -> Result<(), Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coin_round_trip() {
        let coin = Coin {
            value: 123_456_789,
            script_pubkey: Bytes::from_static(&[0x76, 0xa9, 0x14, 7, 0x88, 0xac]),
            height: 2_500_000,
            is_coinbase: true,
        };
        assert_eq!(Coin::decode(&coin.encode()), Ok(coin));
    }

    #[test]
    fn coin_decode_rejects_garbage() {
        assert_eq!(Coin::decode(&[0; 5]), Err(MalformedCoin::TooShort(5)));
        let mut bytes = [0u8; COIN_HEADER_BYTES];
        bytes[12] = 7;
        assert_eq!(Coin::decode(&bytes), Err(MalformedCoin::CoinbaseFlag(7)));
    }

    #[test]
    fn stored_best_block_of_a_directory_without_a_store_is_an_error() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let Err(_) = stored_best_block(dir.path()) else {
            panic!("a directory without a store");
        };
    }

    #[test]
    fn outpoint_key_layout() {
        let outpoint = OutPoint::new([0xab; 32], 0x0102_0304);
        let key = outpoint_key(&outpoint);
        assert_eq!(&key[..32], &[0xab; 32]);
        assert_eq!(&key[32..], &[0x04, 0x03, 0x02, 0x01]);
    }
}
