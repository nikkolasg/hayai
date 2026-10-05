//! Nullifier sets: unflushed nullifiers in memory in front of the backing's per-pool sets.

use std::collections::HashSet;
use std::sync::Arc;

use crate::{CoinsBacking, Error, Pool};

/// The nullifier set of one pool.
///
/// Same ownership as [`crate::CoinsCache`]: the state writer inserts and flushes through
/// `&mut self`, readers query through `&self`. A membership query answers from the pending
/// set, then the generation in flight, and asks the backing in one batch for the rest.
pub struct NullifierSet {
    pool: Pool,
    pending: HashSet<[u8; 32], ahash::RandomState>,
    /// The pending set of the flush in flight ([`NullifierSet::begin_flush`]), readable
    /// until [`NullifierSet::end_flush`].
    flushing: Option<HashSet<[u8; 32], ahash::RandomState>>,
    backing: Arc<dyn CoinsBacking>,
}

impl NullifierSet {
    /// An empty pending set for `pool` over `backing`.
    pub fn new(pool: Pool, backing: Arc<dyn CoinsBacking>) -> Self {
        NullifierSet {
            pool,
            pending: HashSet::with_hasher(ahash::RandomState::new()),
            flushing: None,
            backing,
        }
    }

    fn in_memory(&self, nullifier: &[u8; 32]) -> bool {
        if self.pending.contains(nullifier) {
            return true;
        }
        match &self.flushing {
            Some(flushing) => flushing.contains(nullifier),
            None => false,
        }
    }

    /// The pool this set belongs to.
    pub fn pool(&self) -> Pool {
        self.pool
    }

    /// Positional membership test over the pending set and the backing.
    pub fn contains_many(&self, nullifiers: &[[u8; 32]]) -> Result<Vec<bool>, Error> {
        let mut result = vec![false; nullifiers.len()];
        let mut misses: Vec<usize> = Vec::new();
        for (position, nullifier) in nullifiers.iter().enumerate() {
            if self.in_memory(nullifier) {
                result[position] = true;
            } else {
                misses.push(position);
            }
        }
        if misses.is_empty() {
            return Ok(result);
        }
        let keys: Vec<[u8; 32]> = misses.iter().map(|&p| nullifiers[p]).collect();
        let found = self.backing.contains_many(self.pool, &keys)?;
        for (position, present) in misses.into_iter().zip(found) {
            result[position] = present;
        }
        Ok(result)
    }

    /// Adds nullifiers to the pending set. Uniqueness is a contextual rule checked by the
    /// caller with [`NullifierSet::contains_many`] before the block is applied.
    pub fn insert_many(&mut self, nullifiers: &[[u8; 32]]) {
        self.pending.extend(nullifiers.iter().copied());
    }

    /// Phase one of a flush: moves the pending set to the generation in flight (still
    /// answering queries) and returns its nullifiers for the write. Fails when a generation
    /// is already in flight.
    pub fn begin_flush(&mut self) -> Result<Vec<[u8; 32]>, Error> {
        let None = self.flushing else {
            return Err(Error::FlushInFlight);
        };
        let pending = std::mem::replace(
            &mut self.pending,
            HashSet::with_hasher(ahash::RandomState::new()),
        );
        let nullifiers: Vec<[u8; 32]> = pending.iter().copied().collect();
        self.flushing = Some(pending);
        Ok(nullifiers)
    }

    /// Phase three of a flush, after the generation is on disk: drops the generation in
    /// flight. Fails when none is in flight.
    pub fn end_flush(&mut self) -> Result<(), Error> {
        let Some(_) = self.flushing.take() else {
            return Err(Error::NoFlushInFlight);
        };
        Ok(())
    }

    /// Writes the pending set to the backing in one batch and returns how many it wrote:
    /// the three phases in a row.
    pub fn flush(&mut self) -> Result<usize, Error> {
        let nullifiers = self.begin_flush()?;
        if !nullifiers.is_empty() {
            self.backing.insert_many(self.pool, &nullifiers)?;
        }
        self.end_flush()?;
        Ok(nullifiers.len())
    }

    /// Number of unflushed nullifiers (the generation in flight not included).
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

/// One [`NullifierSet`] per pool.
pub struct NullifierStore {
    sets: [NullifierSet; 4],
}

impl NullifierStore {
    /// Empty pending sets for every pool over `backing`.
    pub fn new(backing: Arc<dyn CoinsBacking>) -> Self {
        NullifierStore {
            sets: Pool::ALL.map(|pool| NullifierSet::new(pool, backing.clone())),
        }
    }

    /// The set of one pool.
    pub fn pool(&self, pool: Pool) -> &NullifierSet {
        &self.sets[pool.index()]
    }

    /// The set of one pool, for insertion.
    pub fn pool_mut(&mut self, pool: Pool) -> &mut NullifierSet {
        &mut self.sets[pool.index()]
    }

    /// Phase one of a flush for every pool, in [`Pool::ALL`] order.
    pub fn begin_flush(&mut self) -> Result<[Vec<[u8; 32]>; 4], Error> {
        let mut taken: [Vec<[u8; 32]>; 4] = Default::default();
        for (set, out) in self.sets.iter_mut().zip(taken.iter_mut()) {
            *out = set.begin_flush()?;
        }
        Ok(taken)
    }

    /// Phase three of a flush for every pool.
    pub fn end_flush(&mut self) -> Result<(), Error> {
        for set in &mut self.sets {
            set.end_flush()?;
        }
        Ok(())
    }

    /// Flushes every pool and returns the total written.
    pub fn flush(&mut self) -> Result<usize, Error> {
        let mut written = 0;
        for set in &mut self.sets {
            written += set.flush()?;
        }
        Ok(written)
    }
}
