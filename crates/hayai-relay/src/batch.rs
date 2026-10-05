//! Batches and lanes (`docs/protocol-compact-relay.md`, Terminology and Transaction
//! dissemination).
//!
//! A batch is an ordered list of WtxIds published by a lane owner. A receiver keeps the last
//! [`LaneStore::BATCHES_PER_LANE`] batches of each of at most [`LaneStore::MAX_LANES`]
//! lanes and drops a lane after [`LaneStore::LANE_EXPIRY`] without an announcement. Time
//! comes from the caller so that expiry is testable and independent of a wall clock.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use hayai_wire::{TxLookup, WtxId};

use crate::message::{BatchAnnounce, LaneId};

/// `BLAKE2b-256("hayai:batch" || wtxid_0 || … || wtxid_n)`: a 32-byte BLAKE2b digest of the
/// ASCII prefix followed by the 64-byte WtxIds, with no personalization and no count.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct BatchId(pub [u8; 32]);

impl BatchId {
    pub const PREFIX: &'static [u8] = b"hayai:batch";

    pub fn compute(ids: &[WtxId]) -> Self {
        let mut state = blake2b_simd::Params::new().hash_length(32).to_state();
        state.update(Self::PREFIX);
        for id in ids {
            state.update(&id.to_bytes());
        }
        BatchId(state.finalize().as_bytes().try_into().expect("32 bytes"))
    }
}

/// A batch as stored by a receiver.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Batch {
    pub id: BatchId,
    pub lane: LaneId,
    pub seq: u64,
    pub ids: Vec<WtxId>,
}

impl Batch {
    /// Completeness of the batch against a transaction store.
    pub fn status(&self, store: &dyn TxLookup) -> BatchStatus {
        let mut missing = Vec::new();
        for id in &self.ids {
            let None = store.get(id) else {
                continue;
            };
            missing.push(*id);
        }
        BatchStatus {
            complete: missing.is_empty(),
            missing,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BatchStatus {
    pub complete: bool,
    /// Ids the store does not hold, in batch order; empty when complete.
    pub missing: Vec<WtxId>,
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum LaneError {
    #[error("announced batch id does not match the hash of its ids")]
    BatchIdMismatch,
    #[error("lane limit of {0} reached; new lane ignored")]
    TooManyLanes(usize),
    #[error("batch seq {seq} is not after the lane's last seq {last}")]
    StaleSeq { seq: u64, last: u64 },
    #[error("batch has no transactions")]
    EmptyBatch,
}

struct Lane {
    batches: VecDeque<Batch>,
    last_seen: Instant,
}

/// Receiver-side store of lanes and their recent batches.
pub struct LaneStore {
    lanes: HashMap<LaneId, Lane>,
    /// Lane and seq of every stored batch id, for lookup by reference.
    by_id: HashMap<BatchId, (LaneId, u64)>,
}

impl Default for LaneStore {
    fn default() -> Self {
        Self::new()
    }
}

impl LaneStore {
    pub const MAX_LANES: usize = 64;
    pub const BATCHES_PER_LANE: usize = 256;
    pub const LANE_EXPIRY: Duration = Duration::from_secs(10 * 60);

    pub fn new() -> Self {
        LaneStore {
            lanes: HashMap::new(),
            by_id: HashMap::new(),
        }
    }

    /// Records an announced batch and returns its completeness against `store`.
    ///
    /// Re-announcing a known batch id is idempotent. A new lane beyond the lane limit, a
    /// seq that does not advance the lane, or a batch id that does not hash its ids is an
    /// error and leaves the store unchanged. Expired lanes are dropped first.
    pub fn insert(
        &mut self,
        announce: &BatchAnnounce,
        store: &dyn TxLookup,
        now: Instant,
    ) -> Result<BatchStatus, LaneError> {
        self.expire(now);
        if announce.ids.is_empty() {
            return Err(LaneError::EmptyBatch);
        }
        if BatchId::compute(&announce.ids) != announce.batch_id {
            return Err(LaneError::BatchIdMismatch);
        }
        if let Some(existing) = self.get(&announce.batch_id) {
            return Ok(existing.status(store));
        }
        let lane = match self.lanes.get_mut(&announce.lane_id) {
            Some(lane) => lane,
            None => {
                if self.lanes.len() >= Self::MAX_LANES {
                    return Err(LaneError::TooManyLanes(Self::MAX_LANES));
                }
                self.lanes.entry(announce.lane_id).or_insert(Lane {
                    batches: VecDeque::with_capacity(Self::BATCHES_PER_LANE),
                    last_seen: now,
                })
            }
        };
        if let Some(last) = lane.batches.back() {
            if announce.seq <= last.seq {
                return Err(LaneError::StaleSeq {
                    seq: announce.seq,
                    last: last.seq,
                });
            }
        }
        lane.last_seen = now;
        if lane.batches.len() == Self::BATCHES_PER_LANE {
            let evicted = lane.batches.pop_front().expect("ring is full");
            self.by_id.remove(&evicted.id);
        }
        let batch = Batch {
            id: announce.batch_id,
            lane: announce.lane_id,
            seq: announce.seq,
            ids: announce.ids.clone(),
        };
        let status = batch.status(store);
        self.by_id
            .insert(batch.id, (announce.lane_id, announce.seq));
        lane.batches.push_back(batch);
        Ok(status)
    }

    pub fn get(&self, id: &BatchId) -> Option<&Batch> {
        let (lane_id, seq) = self.by_id.get(id)?;
        let lane = self.lanes.get(lane_id)?;
        // Seqs are strictly increasing within a lane, so the ring is sorted by seq.
        let pos = lane.batches.binary_search_by_key(seq, |b| b.seq).ok()?;
        lane.batches.get(pos)
    }

    pub fn status(&self, id: &BatchId, store: &dyn TxLookup) -> Option<BatchStatus> {
        self.get(id).map(|b| b.status(store))
    }

    /// Drops every lane whose last announcement is `LANE_EXPIRY` or more in the past.
    pub fn expire(&mut self, now: Instant) {
        let expired: Vec<LaneId> = self
            .lanes
            .iter()
            .filter(|(_, lane)| now.duration_since(lane.last_seen) >= Self::LANE_EXPIRY)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            let Some(lane) = self.lanes.remove(&id) else {
                continue;
            };
            for batch in lane.batches {
                self.by_id.remove(&batch.id);
            }
        }
    }

    /// Every stored batch, in no particular order.
    pub fn batches(&self) -> impl Iterator<Item = &Batch> {
        self.lanes.values().flat_map(|lane| lane.batches.iter())
    }

    pub fn lane_count(&self) -> usize {
        self.lanes.len()
    }

    pub fn batch_count(&self) -> usize {
        self.by_id.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{make_tx, wtxid, MemStore};

    fn announce(lane: u8, seq: u64, ids: Vec<WtxId>) -> BatchAnnounce {
        BatchAnnounce {
            lane_id: [lane; 32],
            seq,
            batch_id: BatchId::compute(&ids),
            ids,
        }
    }

    #[test]
    fn batch_id_is_prefixed_blake2b_256() {
        let ids = vec![wtxid(1), wtxid(2)];
        let mut preimage = b"hayai:batch".to_vec();
        for id in &ids {
            preimage.extend_from_slice(&id.to_bytes());
        }
        let expected = blake2b_simd::Params::new().hash_length(32).hash(&preimage);
        assert_eq!(BatchId::compute(&ids).0, expected.as_bytes());
        assert_ne!(
            BatchId::compute(&ids),
            BatchId::compute(&[wtxid(2), wtxid(1)])
        );
        assert_ne!(BatchId::compute(&[]), BatchId::compute(&[wtxid(1)]));
    }

    #[test]
    fn status_tracks_store() {
        let (tx1, tx2) = (make_tx(1), make_tx(2));
        let mut store = MemStore::default();
        store.insert(tx1.clone());
        let mut lanes = LaneStore::new();
        let t0 = Instant::now();
        let a = announce(1, 1, vec![tx1.wtxid(), tx2.wtxid()]);
        let status = lanes.insert(&a, &store, t0).unwrap();
        assert_eq!(
            status,
            BatchStatus {
                complete: false,
                missing: vec![tx2.wtxid()]
            }
        );
        store.insert(tx2);
        assert_eq!(
            lanes.status(&a.batch_id, &store),
            Some(BatchStatus {
                complete: true,
                missing: vec![]
            })
        );
        // Re-announcing is idempotent.
        assert!(lanes.insert(&a, &store, t0).unwrap().complete);
        assert_eq!(lanes.batch_count(), 1);
        assert_eq!(lanes.get(&a.batch_id).unwrap().ids, a.ids);
    }

    #[test]
    fn rejects_bad_id_empty_and_stale_seq() {
        let store = MemStore::default();
        let mut lanes = LaneStore::new();
        let t0 = Instant::now();
        let mut bad = announce(1, 1, vec![wtxid(1)]);
        bad.batch_id = BatchId([0; 32]);
        assert_eq!(
            lanes.insert(&bad, &store, t0),
            Err(LaneError::BatchIdMismatch)
        );
        assert_eq!(
            lanes.insert(&announce(1, 1, vec![]), &store, t0),
            Err(LaneError::EmptyBatch)
        );
        lanes
            .insert(&announce(1, 5, vec![wtxid(1)]), &store, t0)
            .unwrap();
        assert_eq!(
            lanes.insert(&announce(1, 5, vec![wtxid(2)]), &store, t0),
            Err(LaneError::StaleSeq { seq: 5, last: 5 })
        );
        assert_eq!(
            lanes.insert(&announce(1, 4, vec![wtxid(2)]), &store, t0),
            Err(LaneError::StaleSeq { seq: 4, last: 5 })
        );
        assert_eq!(lanes.batch_count(), 1);
    }

    #[test]
    fn ring_keeps_last_256_batches() {
        let store = MemStore::default();
        let mut lanes = LaneStore::new();
        let t0 = Instant::now();
        let mut ids = Vec::new();
        for seq in 0..300u64 {
            let a = announce(
                1,
                seq,
                vec![wtxid(seq as u8), wtxid((seq >> 8) as u8 + 100)],
            );
            ids.push(a.batch_id);
            lanes.insert(&a, &store, t0).unwrap();
        }
        assert_eq!(lanes.batch_count(), LaneStore::BATCHES_PER_LANE);
        for (seq, id) in ids.iter().enumerate() {
            let expected = (seq >= 300 - LaneStore::BATCHES_PER_LANE).then_some(seq as u64);
            assert_eq!(lanes.get(id).map(|b| b.seq), expected, "seq {seq}");
        }
        assert_eq!(lanes.get(&ids[299]).unwrap().seq, 299);
    }

    #[test]
    fn lane_limit_and_expiry() {
        let store = MemStore::default();
        let mut lanes = LaneStore::new();
        let t0 = Instant::now();
        for lane in 0..LaneStore::MAX_LANES {
            lanes
                .insert(
                    &announce(lane as u8, 0, vec![wtxid(lane as u8)]),
                    &store,
                    t0,
                )
                .unwrap();
        }
        assert_eq!(
            lanes.insert(&announce(200, 0, vec![wtxid(99)]), &store, t0),
            Err(LaneError::TooManyLanes(LaneStore::MAX_LANES))
        );
        // Lane 0 announces again just before the others expire.
        let t1 = t0 + LaneStore::LANE_EXPIRY - Duration::from_secs(1);
        let fresh = announce(0, 1, vec![wtxid(110)]);
        lanes.insert(&fresh, &store, t1).unwrap();
        let t2 = t0 + LaneStore::LANE_EXPIRY;
        lanes.expire(t2);
        assert_eq!(lanes.lane_count(), 1);
        assert_eq!(lanes.batch_count(), 2);
        assert_eq!(lanes.get(&fresh.batch_id).map(|b| b.seq), Some(1));
        let None = lanes.get(&announce(1, 0, vec![wtxid(1)]).batch_id) else {
            panic!("expired lane still serves its batch");
        };
        // A new lane fits again, and insert expires on its own clock.
        lanes
            .insert(&announce(200, 0, vec![wtxid(99)]), &store, t2)
            .unwrap();
        let t3 = t2 + LaneStore::LANE_EXPIRY;
        lanes
            .insert(&announce(201, 0, vec![wtxid(98)]), &store, t3)
            .unwrap();
        assert_eq!(lanes.lane_count(), 1);
    }
}
