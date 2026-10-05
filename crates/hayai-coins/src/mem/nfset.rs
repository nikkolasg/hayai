//! The nullifier shards of [`super::MemBacking`]: one sorted run and a small hash set.
//!
//! A run costs 32 bytes per nullifier against 63–75 bytes for a hash set entry
//! (`CHANGES.md` has the measurements); a lookup is one probe of the small set and one
//! binary search. New nullifiers go to the small set and merge into the run when the set
//! holds more than an eighth of the run, so each nullifier is copied about eight times on
//! average and the set stays small.

use std::collections::HashSet;

/// The small set never merges below this size, so a nearly empty pool does not merge on
/// every insert.
const RECENT_MIN: usize = 64;

pub(super) struct NullifierShard {
    /// Sorted, without duplicates.
    run: Vec<[u8; 32]>,
    recent: HashSet<[u8; 32], ahash::RandomState>,
}

impl NullifierShard {
    pub(super) fn new(hasher: &ahash::RandomState) -> Self {
        NullifierShard {
            run: Vec::new(),
            recent: HashSet::with_hasher(hasher.clone()),
        }
    }

    /// A shard over a run read from a snapshot. Fails when the run is not strictly
    /// increasing, since the binary search would then miss members.
    pub(super) fn from_run(
        run: Vec<[u8; 32]>,
        hasher: &ahash::RandomState,
    ) -> Result<Self, &'static str> {
        if run.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("nullifier run is not strictly increasing");
        }
        Ok(NullifierShard {
            run,
            recent: HashSet::with_hasher(hasher.clone()),
        })
    }

    pub(super) fn contains(&self, nullifier: &[u8; 32]) -> bool {
        if self.recent.contains(nullifier) {
            return true;
        }
        let Ok(_) = self.run.binary_search(nullifier) else {
            return false;
        };
        true
    }

    pub(super) fn insert(&mut self, nullifier: [u8; 32]) {
        self.recent.insert(nullifier);
        if self.recent.len() > RECENT_MIN.max(self.run.len() / 8) {
            self.run = self.merged();
            self.recent = HashSet::with_hasher(self.recent.hasher().clone());
        }
    }

    /// The run and the small set as one sorted vector without duplicates.
    pub(super) fn merged(&self) -> Vec<[u8; 32]> {
        let mut recent: Vec<[u8; 32]> = self.recent.iter().copied().collect();
        recent.sort_unstable();
        let mut out = Vec::with_capacity(self.run.len() + recent.len());
        let (mut a, mut b) = (self.run.iter().peekable(), recent.iter().peekable());
        loop {
            let next = match (a.peek(), b.peek()) {
                (Some(x), Some(y)) if x < y => a.next(),
                (Some(x), Some(y)) if x > y => b.next(),
                (Some(_), Some(_)) => {
                    b.next();
                    a.next()
                }
                (Some(_), None) => a.next(),
                (None, Some(_)) => b.next(),
                (None, None) => break,
            };
            out.extend(next.copied());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn merges_keep_every_member_once() {
        let hasher = ahash::RandomState::new();
        let mut shard = NullifierShard::new(&hasher);
        let mut model = BTreeSet::new();
        for i in 0u32..5_000 {
            let mut nf = [0u8; 32];
            nf[..4].copy_from_slice(&(i.wrapping_mul(2_654_435_761) % 3_000).to_be_bytes());
            shard.insert(nf);
            model.insert(nf);
        }
        assert_eq!(shard.merged(), model.iter().copied().collect::<Vec<_>>());
        for nf in &model {
            assert!(shard.contains(nf));
        }
        assert!(!shard.contains(&[0xff; 32]));
        assert!(shard.recent.len() <= RECENT_MIN.max(shard.run.len() / 8));
        assert_eq!(
            NullifierShard::from_run(vec![[1; 32], [1; 32]], &hasher).err(),
            Some("nullifier run is not strictly increasing")
        );
    }
}
