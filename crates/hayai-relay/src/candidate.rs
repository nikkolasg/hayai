//! Template candidates as lanes (`docs/protocol-compact-relay.md`, section Candidates).
//!
//! A lane owner publishes each change of its block template as one batch of the added
//! transactions ([`crate::BatchAnnounce`], `seq` = template revision) and one
//! [`CandidateAnnounce`]: the batches that hold the template's transactions on its parent,
//! and the positions of those batches that left the template. A candidate is therefore
//! content-addressed by its batch ids, and a receiver that holds the batches knows its exact
//! set. [`LanePublisher`] produces the announcements from the sequence of templates.
//! [`CandidateStore`] keeps the last [`CandidateStore::PER_LANE`] candidates of each lane.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use hayai_wire::header::BlockHash;
use hayai_wire::WtxId;

use crate::batch::{BatchId, LaneStore};
use crate::message::{BatchAnnounce, CandidateAnnounce, LaneId};

/// Batches one candidate may name. A publisher that reaches it starts its candidate again
/// as one batch, so an announcement stays under 2.1 kB of batch ids.
pub const MAX_CANDIDATE_BATCHES: usize = 64;

/// A candidate with its id list: the ids of its batches in lane order, without the removed
/// positions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCandidate {
    pub lane: LaneId,
    pub seq: u64,
    pub parent: BlockHash,
    pub ids: Vec<WtxId>,
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum CandidateError {
    #[error("lane limit of {0} reached; new lane ignored")]
    TooManyLanes(usize),
    #[error("candidate seq {seq} is not after the lane's last seq {last}")]
    StaleSeq { seq: u64, last: u64 },
    #[error("candidate names {0} batches, more than {MAX_CANDIDATE_BATCHES}")]
    TooManyBatches(usize),
    #[error("candidate batch {0:?} is not in the lane store")]
    UnknownBatch(BatchId),
    #[error("removed position {index} is not below the {count} ids of the batches")]
    RemovedOutOfRange { index: u32, count: usize },
    #[error("removed positions are not strictly increasing at {0}")]
    RemovedOrder(u32),
}

/// The id list of `announce`: the ids of its batches, concatenated in order, without the
/// removed positions. Every batch must be in `lanes`.
pub fn expand(
    announce: &CandidateAnnounce,
    lanes: &LaneStore,
) -> Result<ResolvedCandidate, CandidateError> {
    let mut all = Vec::new();
    for id in &announce.batches {
        let Some(batch) = lanes.get(id) else {
            return Err(CandidateError::UnknownBatch(*id));
        };
        all.extend_from_slice(&batch.ids);
    }
    let ids = without_positions(all, &announce.removed)?;
    Ok(ResolvedCandidate {
        lane: announce.lane_id,
        seq: announce.seq,
        parent: announce.parent,
        ids,
    })
}

/// `ids` without the strictly increasing positions `removed`.
pub(crate) fn without_positions(
    ids: Vec<WtxId>,
    removed: &[u32],
) -> Result<Vec<WtxId>, CandidateError> {
    let mut prev = None;
    for &index in removed {
        if matches!(prev, Some(p) if index <= p) {
            return Err(CandidateError::RemovedOrder(index));
        }
        prev = Some(index);
        if index as usize >= ids.len() {
            return Err(CandidateError::RemovedOutOfRange {
                index,
                count: ids.len(),
            });
        }
    }
    let mut removed = removed.iter().peekable();
    Ok(ids
        .into_iter()
        .enumerate()
        .filter(|(i, _)| match removed.peek() {
            Some(&&r) if r as usize == *i => {
                removed.next();
                false
            }
            _ => true,
        })
        .map(|(_, id)| id)
        .collect())
}

struct Lane {
    candidates: VecDeque<CandidateAnnounce>,
    last_seen: Instant,
}

/// Receiver-side store of the recent candidates of each lane.
#[derive(Default)]
pub struct CandidateStore {
    lanes: HashMap<LaneId, Lane>,
}

impl CandidateStore {
    /// Candidates kept per lane: a miner can work on a template some revisions old.
    pub const PER_LANE: usize = 16;
    pub const MAX_LANES: usize = LaneStore::MAX_LANES;
    pub const LANE_EXPIRY: Duration = LaneStore::LANE_EXPIRY;

    pub fn new() -> Self {
        Self::default()
    }

    /// Records a candidate. Returns `false` for a candidate already stored. A new lane
    /// beyond the lane limit, a seq that does not advance the lane, or too many batches is
    /// an error and leaves the store unchanged. Expired lanes are dropped first.
    pub fn insert(
        &mut self,
        announce: CandidateAnnounce,
        now: Instant,
    ) -> Result<bool, CandidateError> {
        self.expire(now);
        if announce.batches.len() > MAX_CANDIDATE_BATCHES {
            return Err(CandidateError::TooManyBatches(announce.batches.len()));
        }
        if let Some(known) = self.get(&announce.lane_id, announce.seq) {
            if *known == announce {
                return Ok(false);
            }
        }
        let lane = match self.lanes.get_mut(&announce.lane_id) {
            Some(lane) => lane,
            None => {
                if self.lanes.len() >= Self::MAX_LANES {
                    return Err(CandidateError::TooManyLanes(Self::MAX_LANES));
                }
                self.lanes.entry(announce.lane_id).or_insert(Lane {
                    candidates: VecDeque::with_capacity(Self::PER_LANE),
                    last_seen: now,
                })
            }
        };
        if let Some(last) = lane.candidates.back() {
            if announce.seq <= last.seq {
                return Err(CandidateError::StaleSeq {
                    seq: announce.seq,
                    last: last.seq,
                });
            }
        }
        lane.last_seen = now;
        if lane.candidates.len() == Self::PER_LANE {
            lane.candidates.pop_front();
        }
        lane.candidates.push_back(announce);
        Ok(true)
    }

    pub fn get(&self, lane: &LaneId, seq: u64) -> Option<&CandidateAnnounce> {
        let lane = self.lanes.get(lane)?;
        let pos = lane.candidates.binary_search_by_key(&seq, |c| c.seq).ok()?;
        lane.candidates.get(pos)
    }

    /// The candidates that extend `parent`, newest first within each lane.
    pub fn on_parent(&self, parent: &BlockHash) -> Vec<&CandidateAnnounce> {
        self.lanes
            .values()
            .flat_map(|lane| lane.candidates.iter().rev())
            .filter(|c| c.parent == *parent)
            .collect()
    }

    /// Drops every lane whose last announcement is `LANE_EXPIRY` or more in the past.
    pub fn expire(&mut self, now: Instant) {
        self.lanes
            .retain(|_, lane| now.duration_since(lane.last_seen) < Self::LANE_EXPIRY);
    }

    pub fn candidate_count(&self) -> usize {
        self.lanes.values().map(|l| l.candidates.len()).sum()
    }
}

/// What one template change publishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publication {
    /// The added transactions, absent when the change only removed transactions.
    pub batch: Option<BatchAnnounce>,
    pub candidate: CandidateAnnounce,
    /// The candidate's id list.
    pub resolved: ResolvedCandidate,
}

/// The lane of a block template: turns each template into a batch of its additions and a
/// candidate announcement.
pub struct LanePublisher {
    lane: LaneId,
    parent: Option<BlockHash>,
    batches: Vec<BatchId>,
    /// The ids of `batches`, concatenated.
    ids: Vec<WtxId>,
    position: HashMap<WtxId, u32>,
    removed: BTreeSet<u32>,
}

impl LanePublisher {
    pub fn new(lane: LaneId) -> Self {
        Self {
            lane,
            parent: None,
            batches: Vec::new(),
            ids: Vec::new(),
            position: HashMap::new(),
            removed: BTreeSet::new(),
        }
    }

    pub fn lane(&self) -> LaneId {
        self.lane
    }

    /// Publishes the template revision `seq` on `parent` with the transactions `ids`. A
    /// new parent, or a candidate that reached [`MAX_CANDIDATE_BATCHES`], starts the
    /// candidate again from one batch of every id. Otherwise the batch holds the ids that
    /// are not in the candidate yet, in the order of `ids`; an id that left and comes back
    /// takes its old position again. `seq` must increase from call to call.
    pub fn publish(&mut self, parent: BlockHash, seq: u64, ids: &[WtxId]) -> Publication {
        if self.parent != Some(parent) || self.batches.len() >= MAX_CANDIDATE_BATCHES {
            self.parent = Some(parent);
            self.batches.clear();
            self.ids.clear();
            self.position.clear();
            self.removed.clear();
        }
        let wanted: HashSet<&WtxId> = ids.iter().collect();
        let mut added = Vec::new();
        for id in ids {
            match self.position.get(id) {
                Some(&p) => {
                    self.removed.remove(&p);
                }
                None => added.push(*id),
            }
        }
        for (p, id) in self.ids.iter().enumerate() {
            if !wanted.contains(id) {
                self.removed.insert(p as u32);
            }
        }
        let batch = match added.is_empty() {
            true => None,
            false => {
                let batch_id = BatchId::compute(&added);
                for id in &added {
                    let p = u32::try_from(self.ids.len()).expect("a lane holds under 2^32 ids");
                    self.position.insert(*id, p);
                    self.ids.push(*id);
                }
                self.batches.push(batch_id);
                Some(BatchAnnounce {
                    lane_id: self.lane,
                    seq,
                    batch_id,
                    ids: added,
                })
            }
        };
        let candidate = CandidateAnnounce {
            lane_id: self.lane,
            seq,
            parent,
            batches: self.batches.clone(),
            removed: self.removed.iter().copied().collect(),
        };
        let resolved = ResolvedCandidate {
            lane: self.lane,
            seq,
            parent,
            ids: self
                .ids
                .iter()
                .enumerate()
                .filter(|(p, _)| !self.removed.contains(&(*p as u32)))
                .map(|(_, id)| *id)
                .collect(),
        };
        Publication {
            batch,
            candidate,
            resolved,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{wtxid, MemStore};

    fn parent(n: u8) -> BlockHash {
        BlockHash([n; 32])
    }

    fn sorted(mut ids: Vec<WtxId>) -> Vec<WtxId> {
        ids.sort();
        ids
    }

    /// Each template change gives one batch of additions; the receiver rebuilds the exact
    /// set from the batches and the removed positions.
    #[test]
    fn publisher_and_receiver_agree_on_the_set() {
        let mut publisher = LanePublisher::new([9; 32]);
        let mut lanes = LaneStore::new();
        let mut candidates = CandidateStore::new();
        let store = MemStore::default();
        let now = Instant::now();
        let templates: Vec<Vec<WtxId>> = vec![
            vec![wtxid(1), wtxid(2), wtxid(3)],
            vec![wtxid(1), wtxid(4), wtxid(2), wtxid(3)],
            vec![wtxid(1), wtxid(4), wtxid(3)],
            vec![wtxid(4), wtxid(3)],
            vec![wtxid(2), wtxid(4), wtxid(3), wtxid(5)],
        ];
        let mut batches = 0;
        for (seq, ids) in templates.iter().enumerate() {
            let p = publisher.publish(parent(1), seq as u64 + 10, ids);
            if let Some(batch) = &p.batch {
                batches += 1;
                lanes.insert(batch, &store, now).unwrap();
            }
            assert!(candidates.insert(p.candidate.clone(), now).unwrap());
            let stored = candidates.get(&[9; 32], seq as u64 + 10).unwrap();
            let resolved = expand(stored, &lanes).unwrap();
            assert_eq!(resolved, p.resolved);
            assert_eq!(sorted(resolved.ids), sorted(ids.clone()));
        }
        // Templates 3 and 4 only removed or brought back known ids.
        assert_eq!(batches, 3);
        // A new parent starts again from one batch.
        let p = publisher.publish(parent(2), 20, &[wtxid(3), wtxid(6)]);
        assert_eq!(p.candidate.batches.len(), 1);
        assert!(p.candidate.removed.is_empty());
        assert_eq!(p.batch.unwrap().ids, vec![wtxid(3), wtxid(6)]);
    }

    #[test]
    fn a_long_candidate_starts_again() {
        let mut publisher = LanePublisher::new([1; 32]);
        let mut ids = Vec::new();
        for n in 0..MAX_CANDIDATE_BATCHES as u8 {
            ids.push(wtxid(n));
            let p = publisher.publish(parent(1), u64::from(n), &ids);
            assert_eq!(p.candidate.batches.len(), n as usize + 1);
        }
        let p = publisher.publish(parent(1), 100, &ids);
        assert_eq!(p.candidate.batches.len(), 1);
        assert_eq!(p.batch.unwrap().ids.len(), MAX_CANDIDATE_BATCHES);
    }

    #[test]
    fn store_limits_and_lookups() {
        let mut store = CandidateStore::new();
        let now = Instant::now();
        let announce = |lane: u8, seq: u64, p: u8| CandidateAnnounce {
            lane_id: [lane; 32],
            seq,
            parent: parent(p),
            batches: vec![],
            removed: vec![],
        };
        for seq in 0..20 {
            store.insert(announce(1, seq, 1), now).unwrap();
        }
        assert_eq!(store.candidate_count(), CandidateStore::PER_LANE);
        let None = store.get(&[1; 32], 3) else {
            panic!("the oldest candidates leave the ring");
        };
        assert_eq!(store.get(&[1; 32], 19).unwrap().seq, 19);
        assert_eq!(store.insert(announce(1, 19, 1), now), Ok(false));
        assert_eq!(
            store.insert(announce(1, 18, 2), now),
            Err(CandidateError::StaleSeq { seq: 18, last: 19 })
        );
        store.insert(announce(2, 0, 2), now).unwrap();
        assert_eq!(store.on_parent(&parent(2)).len(), 1);
        assert_eq!(store.on_parent(&parent(1))[0].seq, 19);
        let mut big = announce(3, 0, 1);
        big.batches = vec![BatchId([0; 32]); MAX_CANDIDATE_BATCHES + 1];
        assert_eq!(
            store.insert(big, now),
            Err(CandidateError::TooManyBatches(MAX_CANDIDATE_BATCHES + 1))
        );
        store.expire(now + CandidateStore::LANE_EXPIRY);
        assert_eq!(store.candidate_count(), 0);
    }

    #[test]
    fn removed_positions_are_checked() {
        let ids = vec![wtxid(1), wtxid(2), wtxid(3)];
        assert_eq!(
            without_positions(ids.clone(), &[0, 2]).unwrap(),
            vec![wtxid(2)]
        );
        assert_eq!(
            without_positions(ids.clone(), &[3]),
            Err(CandidateError::RemovedOutOfRange { index: 3, count: 3 })
        );
        assert_eq!(
            without_positions(ids, &[1, 1]),
            Err(CandidateError::RemovedOrder(1))
        );
    }
}
