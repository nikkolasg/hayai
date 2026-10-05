//! The in-memory coins cache in front of a [`CoinsBacking`].

use std::collections::hash_map::Entry as MapEntry;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::{Coin, CoinsBacking, CoinsView, Error, OutPoint};

/// One cached outpoint.
///
/// `coin == None` is a spent coin whose deletion has not reached disk yet (a tombstone). A
/// fresh coin was created since the last flush, so the disk has never seen it: spending it
/// removes the entry and nothing is ever written. Whether an entry is dirty is recorded in
/// `Inner::dirty`, so that a flush visits the block's entries and not the whole cache.
struct Entry {
    coin: Option<Coin>,
    fresh: bool,
}

impl Entry {
    fn memory_bytes(&self) -> usize {
        let script = match &self.coin {
            Some(coin) => coin.script_pubkey.len(),
            None => 0,
        };
        std::mem::size_of::<(OutPoint, Entry)>() + script
    }
}

/// Bytes charged for an outpoint in the dirty index.
const DIRTY_INDEX_BYTES: usize = std::mem::size_of::<OutPoint>();

struct Inner {
    map: HashMap<OutPoint, Entry, ahash::RandomState>,
    /// Outpoints whose entry differs from the backing: added coins and tombstones. Bitcoin
    /// Core keeps the same index as a linked list through its entries (PR 28280) so that a
    /// flush does not scan the entire cache.
    dirty: HashSet<OutPoint, ahash::RandomState>,
    /// The dirty index of the flush in flight ([`CoinsCache::begin_flush`]): its entries
    /// stay in `map` and readable until [`CoinsCache::end_flush`] marks them clean.
    flushing: Option<HashSet<OutPoint, ahash::RandomState>>,
    /// Approximate bytes held by `map`, `dirty` and `flushing`, maintained on every insert
    /// and remove.
    bytes: usize,
}

impl Inner {
    fn insert(&mut self, outpoint: OutPoint, entry: Entry) {
        self.bytes += entry.memory_bytes();
        if let Some(previous) = self.map.insert(outpoint, entry) {
            self.bytes -= previous.memory_bytes();
        }
    }

    fn remove(&mut self, outpoint: &OutPoint) -> Option<Entry> {
        let removed = self.map.remove(outpoint);
        if let Some(removed) = &removed {
            self.bytes -= removed.memory_bytes();
        }
        removed
    }

    fn mark_dirty(&mut self, outpoint: &OutPoint) {
        if self.dirty.insert(outpoint.clone()) {
            self.bytes += DIRTY_INDEX_BYTES;
        }
    }

    fn unmark_dirty(&mut self, outpoint: &OutPoint) {
        if self.dirty.remove(outpoint) {
            self.bytes -= DIRTY_INDEX_BYTES;
        }
    }
}

/// What one [`CoinsCache::flush`] wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushStats {
    /// Coins written to the backing.
    pub adds: usize,
    /// Coins deleted from the backing.
    pub spends: usize,
}

/// The coins part of a flush generation: copies of the dirty entries at
/// [`CoinsCache::begin_flush`].
#[derive(Debug, Default)]
pub struct CoinsFlush {
    pub adds: Vec<(OutPoint, Coin)>,
    pub spends: Vec<OutPoint>,
}

/// Coins cache modeled on Bitcoin Core's `CCoinsViewCache`.
///
/// Ownership and locking: the state writer owns the cache and is the only one that adds,
/// spends and flushes, so those methods take `&mut self` and need no lock. Reads take `&self`
/// because validators share the cache while the writer is idle; a read that misses pulls the
/// coins from the backing into the map, and that insertion is the only reason for the inner
/// `RwLock`. The borrow checker rules out a concurrent writer, so a reader never observes a
/// half-applied block.
pub struct CoinsCache {
    inner: RwLock<Inner>,
    backing: Arc<dyn CoinsBacking>,
}

impl CoinsCache {
    /// An empty cache over `backing`.
    pub fn new(backing: Arc<dyn CoinsBacking>) -> Self {
        CoinsCache {
            inner: RwLock::new(Inner {
                map: HashMap::with_hasher(ahash::RandomState::new()),
                dirty: HashSet::with_hasher(ahash::RandomState::new()),
                flushing: None,
                bytes: 0,
            }),
            backing,
        }
    }

    /// The store this cache flushes to.
    pub fn backing(&self) -> &Arc<dyn CoinsBacking> {
        &self.backing
    }

    /// Looks up many outpoints: cache hits from the map, then one [`CoinsBacking::get_many`]
    /// for the misses, which are inserted as clean entries. A spent coin (tombstone) and a
    /// coin absent from the backing both read as `None`; absence is not cached.
    pub fn fetch_many(&self, outpoints: &[OutPoint]) -> Result<Vec<Option<Coin>>, Error> {
        let mut result: Vec<Option<Coin>> = Vec::with_capacity(outpoints.len());
        let mut misses: Vec<usize> = Vec::new();
        {
            let inner = self.inner.read();
            for (position, outpoint) in outpoints.iter().enumerate() {
                match inner.map.get(outpoint) {
                    Some(entry) => result.push(entry.coin.clone()),
                    None => {
                        misses.push(position);
                        result.push(None);
                    }
                }
            }
        }
        if misses.is_empty() {
            return Ok(result);
        }

        let miss_outpoints: Vec<OutPoint> = misses
            .iter()
            .map(|&position| outpoints[position].clone())
            .collect();
        let found = self.backing.get_many(&miss_outpoints)?;

        let mut inner = self.inner.write();
        let Inner { map, bytes, .. } = &mut *inner;
        for ((position, outpoint), coin) in misses.into_iter().zip(miss_outpoints).zip(found) {
            let Some(coin) = coin else { continue };
            match map.entry(outpoint) {
                // Another reader inserted the same coin between our unlock and relock; both
                // copies came from the backing, so either serves.
                MapEntry::Occupied(occupied) => result[position] = occupied.get().coin.clone(),
                MapEntry::Vacant(vacant) => {
                    let entry = Entry {
                        coin: Some(coin.clone()),
                        fresh: false,
                    };
                    *bytes += entry.memory_bytes();
                    vacant.insert(entry);
                    result[position] = Some(coin);
                }
            }
        }
        Ok(result)
    }

    /// Records a new coin. It is fresh and dirty: it reaches disk only if it is still unspent
    /// at the next flush. Fails if the cache already holds `outpoint` unspent; the backing is
    /// not consulted because a duplicate outpoint is already rejected by the txid uniqueness
    /// rule before a block gets here.
    pub fn add(&mut self, outpoint: OutPoint, coin: Coin) -> Result<(), Error> {
        let inner = self.inner.get_mut();
        let fresh = match inner.map.get(&outpoint) {
            Some(Entry { coin: Some(_), .. }) => return Err(Error::DuplicateCoin(outpoint)),
            // Replacing a tombstone: the backing may still hold the old coin, so the new one
            // must be written over it at the next flush.
            Some(Entry { coin: None, .. }) => false,
            None => true,
        };
        inner.mark_dirty(&outpoint);
        inner.insert(
            outpoint,
            Entry {
                coin: Some(coin),
                fresh,
            },
        );
        Ok(())
    }

    /// Spends a coin and returns it. A fresh coin is removed outright and never reaches disk;
    /// any other coin becomes a tombstone that the next flush deletes. A coin not in the
    /// cache is read from the backing first, so callers that have the whole block's inputs
    /// should [`CoinsCache::fetch_many`] them beforehand to keep this a map operation.
    pub fn spend(&mut self, outpoint: &OutPoint) -> Result<Coin, Error> {
        if !self.inner.get_mut().map.contains_key(outpoint) {
            self.fetch_many(std::slice::from_ref(outpoint))?;
        }
        let inner = self.inner.get_mut();
        let Some(entry) = inner.map.get_mut(outpoint) else {
            return Err(Error::MissingCoin(outpoint.clone()));
        };
        let Some(coin) = entry.coin.take() else {
            return Err(Error::MissingCoin(outpoint.clone()));
        };
        let fresh = entry.fresh;
        // The script left the map with the coin; a removed entry also gives back its slot.
        inner.bytes -= coin.script_pubkey.len();
        if fresh {
            let Some(_) = inner.remove(outpoint) else {
                unreachable!("entry was just found");
            };
            inner.unmark_dirty(outpoint);
        } else {
            inner.mark_dirty(outpoint);
        }
        Ok(coin)
    }

    /// Phase one of a flush: takes the dirty index as the generation in flight and returns
    /// copies of its coins and tombstones for the write. The entries stay in the map, so a
    /// read during the write still sees them; they are marked not fresh, so a spend during
    /// the write becomes a tombstone that the next flush deletes from disk. Fails when a
    /// generation is already in flight.
    pub fn begin_flush(&mut self) -> Result<CoinsFlush, Error> {
        let inner = self.inner.get_mut();
        let None = inner.flushing else {
            return Err(Error::FlushInFlight);
        };
        let dirty = std::mem::replace(
            &mut inner.dirty,
            HashSet::with_hasher(ahash::RandomState::new()),
        );
        let mut flush = CoinsFlush::default();
        for outpoint in &dirty {
            let Some(entry) = inner.map.get_mut(outpoint) else {
                unreachable!("dirty index names an outpoint that is not cached");
            };
            match &entry.coin {
                Some(coin) => {
                    entry.fresh = false;
                    flush.adds.push((outpoint.clone(), coin.clone()));
                }
                None => flush.spends.push(outpoint.clone()),
            }
        }
        inner.flushing = Some(dirty);
        Ok(flush)
    }

    /// Phase three of a flush, after the generation is on disk: drops the tombstones of
    /// the generation and marks its other entries clean. An entry the writer changed again
    /// during the write is in the new dirty index and stays as it is. Clean entries stay
    /// cached. Fails when no generation is in flight.
    pub fn end_flush(&mut self) -> Result<(), Error> {
        let inner = self.inner.get_mut();
        let Some(flushed) = inner.flushing.take() else {
            return Err(Error::NoFlushInFlight);
        };
        let Inner {
            map, dirty, bytes, ..
        } = inner;
        *bytes -= flushed.len() * DIRTY_INDEX_BYTES;
        for outpoint in flushed {
            if dirty.contains(&outpoint) {
                continue;
            }
            let Some(entry) = map.get(&outpoint) else {
                unreachable!("the generation names an outpoint that is not cached");
            };
            let None = entry.coin else { continue };
            let Some(removed) = map.remove(&outpoint) else {
                unreachable!("entry was just found");
            };
            *bytes -= removed.memory_bytes();
        }
        Ok(())
    }

    /// Writes every dirty coin and every tombstone to the backing in one batch, then drops
    /// the tombstones and marks the rest clean: the three phases in a row, for a caller
    /// that does not share the cache with readers during the write.
    pub fn flush(&mut self) -> Result<FlushStats, Error> {
        let flush = self.begin_flush()?;
        let adds: Vec<(&OutPoint, &Coin)> = flush.adds.iter().map(|(o, c)| (o, c)).collect();
        let spends: Vec<&OutPoint> = flush.spends.iter().collect();
        let stats = FlushStats {
            adds: adds.len(),
            spends: spends.len(),
        };
        self.backing.write_batch(&adds, &spends)?;
        self.end_flush()?;
        Ok(stats)
    }

    /// Flushes when the cache holds more than `limit` bytes and then evicts the clean
    /// entries, so that memory returns under the limit; hot coins re-enter at their next
    /// fetch.
    pub fn flush_if_over(&mut self, limit: usize) -> Result<Option<FlushStats>, Error> {
        if self.memory_bytes() <= limit {
            return Ok(None);
        }
        let stats = self.flush()?;
        self.drop_clean();
        Ok(Some(stats))
    }

    /// Evicts every clean entry, keeping the unflushed work (the dirty index and the
    /// generation in flight).
    pub fn drop_clean(&mut self) {
        let inner = self.inner.get_mut();
        let Inner {
            map,
            dirty,
            flushing,
            bytes,
        } = inner;
        let mut freed = 0;
        map.retain(|outpoint, entry| {
            if dirty.contains(outpoint) {
                return true;
            }
            if let Some(flushing) = flushing {
                if flushing.contains(outpoint) {
                    return true;
                }
            }
            freed += entry.memory_bytes();
            false
        });
        *bytes -= freed;
    }

    /// Number of cached entries, tombstones included.
    pub fn len(&self) -> usize {
        self.inner.read().map.len()
    }

    /// Whether the cache holds no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of entries the next flush would write or delete (the generation in flight
    /// not included).
    pub fn dirty_len(&self) -> usize {
        self.inner.read().dirty.len()
    }

    /// Approximate bytes held by the cache.
    pub fn memory_bytes(&self) -> usize {
        self.inner.read().bytes
    }
}

impl CoinsView for CoinsCache {
    /// A backing store failure is unrecoverable for a node reading its own database, so it
    /// is a panic rather than an absent coin: an absent coin would make a valid block fail
    /// validation silently.
    fn get_coins(&self, outpoints: &[OutPoint]) -> Vec<Option<Coin>> {
        match self.fetch_many(outpoints) {
            Ok(coins) => coins,
            Err(error) => panic!("coins backing store failed: {error}"),
        }
    }
}
