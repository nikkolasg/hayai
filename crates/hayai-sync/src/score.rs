//! The misbehaviour score of each peer. See `docs/plan-consensus-and-sync.md`, item B5.
//!
//! This module is pure logic. It has no clock and no I/O: the caller gives the time in
//! seconds. `hayai-net` keeps one [`ScoreBoard`] keyed by the IP address of a peer, and the
//! block download keeps the same reasons for the peers that it uses.
//!
//! - Each [`Misbehaviour`] adds points. The points decay: one point each
//!   [`DECAY_SECS_PER_POINT`] seconds.
//! - At [`DISCONNECT_SCORE`] points the verdict is [`Verdict::Disconnect`]. At [`BAN_SCORE`]
//!   points the verdict is [`Verdict::Ban`], and the caller refuses the peer for
//!   [`BAN_SECS`] seconds.
//! - A [`Misbehaviour::Stall`] adds no points. [`STALL_LIMIT`] stalls give
//!   [`Verdict::Disconnect`] and never a ban, because a slow peer is not a hostile peer.
//!   `hayai-net` refuses a new connection of the peer while it has [`STALL_LIMIT`] stalls:
//!   one stall decays each [`STALL_DECAY_SECS`] seconds.
//! - A failure of the compact relay that is not a fault of the sender (a short-id
//!   collision, an unknown batch, an unknown candidate) has no reason here. The caller must
//!   not record it.

use std::collections::HashMap;
use std::hash::Hash;

/// Points at which the caller disconnects the peer.
pub const DISCONNECT_SCORE: u32 = 50;
/// Points at which the caller bans the peer (zcashd `DEFAULT_BANSCORE_THRESHOLD`, Zakura
/// `MAX_PEER_MISBEHAVIOR_SCORE`).
pub const BAN_SCORE: u32 = 100;
/// Duration of a ban in seconds (zcashd `DEFAULT_MISBEHAVING_BANTIME`: 24 hours).
pub const BAN_SECS: u64 = 24 * 60 * 60;
/// Seconds after which one point is removed.
pub const DECAY_SECS_PER_POINT: u64 = 60;
/// Stalls at which the caller disconnects the peer.
pub const STALL_LIMIT: u32 = 2;
/// Seconds after which one stall is removed.
pub const STALL_DECAY_SECS: u64 = 10 * 60;

/// Why a peer loses score.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Misbehaviour {
    /// A header that fails a rule that needs no chain context: proof of work, Equihash,
    /// version, solution length. A header that fails a contextual rule on the best chain.
    InvalidHeader,
    /// A block that does not match its header, or a block that consensus rejects.
    InvalidBlock,
    /// A transaction with an invalid proof or an invalid signature.
    InvalidProof,
    /// A transaction that fails another consensus rule.
    InvalidTransaction,
    /// A frame that does not decode, a payload above its size limit, or a message that the
    /// negotiated protocol does not permit. An honest peer with a newer protocol can send a
    /// message that this decoder does not know, so one such frame disconnects the peer (as
    /// Zebra does) and a second one before the points decay bans it.
    Malformed,
    /// Headers that do not connect to a known header.
    UnconnectedHeaders,
    /// Data that the node did not request and that the protocol does not send unrequested.
    Unsolicited,
    /// A request that the peer did not answer in time.
    Stall,
}

impl Misbehaviour {
    /// The points of this reason.
    pub fn points(self) -> u32 {
        match self {
            Misbehaviour::InvalidHeader
            | Misbehaviour::InvalidBlock
            | Misbehaviour::InvalidProof => 100,
            Misbehaviour::Malformed => 50,
            Misbehaviour::UnconnectedHeaders | Misbehaviour::Unsolicited => 20,
            Misbehaviour::InvalidTransaction => 10,
            Misbehaviour::Stall => 0,
        }
    }
}

/// What the caller must do with the peer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Keep,
    Disconnect,
    Ban,
}

/// The score of one peer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PeerScore {
    points: u32,
    /// Time from which the next point decays.
    points_since: u64,
    stalls: u32,
    /// Time from which the next stall decays.
    stalls_since: u64,
}

impl PeerScore {
    pub fn new(now: u64) -> Self {
        Self {
            points: 0,
            points_since: now,
            stalls: 0,
            stalls_since: now,
        }
    }

    /// Applies the decay up to `now`. A time before the last update changes nothing.
    fn settle(&mut self, now: u64) {
        let decayed = now.saturating_sub(self.points_since) / DECAY_SECS_PER_POINT;
        if decayed >= u64::from(self.points) {
            self.points = 0;
            self.points_since = self.points_since.max(now);
        } else {
            self.points -= decayed as u32;
            self.points_since += decayed * DECAY_SECS_PER_POINT;
        }
        let forgotten = now.saturating_sub(self.stalls_since) / STALL_DECAY_SECS;
        if forgotten >= u64::from(self.stalls) {
            self.stalls = 0;
            self.stalls_since = self.stalls_since.max(now);
        } else {
            self.stalls -= forgotten as u32;
            self.stalls_since += forgotten * STALL_DECAY_SECS;
        }
    }

    /// The points at `now`.
    pub fn points(&self, now: u64) -> u32 {
        let mut copy = *self;
        copy.settle(now);
        copy.points
    }

    /// The stalls at `now`.
    pub fn stalls(&self, now: u64) -> u32 {
        let mut copy = *self;
        copy.settle(now);
        copy.stalls
    }

    /// Records one misbehaviour at `now` and returns the verdict.
    pub fn record(&mut self, reason: Misbehaviour, now: u64) -> Verdict {
        self.settle(now);
        if let Misbehaviour::Stall = reason {
            self.stalls += 1;
        }
        self.points = self.points.saturating_add(reason.points());
        if self.points >= BAN_SCORE {
            Verdict::Ban
        } else if self.points >= DISCONNECT_SCORE || self.stalls >= STALL_LIMIT {
            Verdict::Disconnect
        } else {
            Verdict::Keep
        }
    }

    fn is_clear(&self, now: u64) -> bool {
        self.points(now) == 0 && self.stalls(now) == 0
    }
}

/// The scores of many peers, with a bound on the number of entries.
pub struct ScoreBoard<K> {
    scores: HashMap<K, PeerScore>,
    capacity: usize,
}

impl<K: Hash + Eq + Clone> ScoreBoard<K> {
    /// A board that holds at most `capacity` peers. `capacity` must not be zero.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "a score board needs a capacity");
        Self {
            scores: HashMap::new(),
            capacity,
        }
    }

    pub fn len(&self) -> usize {
        self.scores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }

    /// The points of `key` at `now`. A peer without an entry has zero points.
    pub fn points(&self, key: &K, now: u64) -> u32 {
        self.scores.get(key).map_or(0, |s| s.points(now))
    }

    /// The stalls of `key` at `now`. A peer without an entry has none.
    pub fn stalls(&self, key: &K, now: u64) -> u32 {
        self.scores.get(key).map_or(0, |s| s.stalls(now))
    }

    /// Records one misbehaviour of `key` at `now` and returns the verdict.
    ///
    /// A ban removes the entry: the caller holds the ban, and the peer starts from zero
    /// after it. When the board is full, the entries without score go first, then the entry
    /// with the fewest points.
    pub fn record(&mut self, key: K, reason: Misbehaviour, now: u64) -> Verdict {
        if !self.scores.contains_key(&key) && self.scores.len() >= self.capacity {
            self.scores.retain(|_, s| !s.is_clear(now));
            if self.scores.len() >= self.capacity {
                let lowest = self
                    .scores
                    .iter()
                    .min_by_key(|(_, s)| s.points(now))
                    .map(|(k, _)| k.clone());
                if let Some(lowest) = lowest {
                    self.scores.remove(&lowest);
                }
            }
        }
        let verdict = self
            .scores
            .entry(key.clone())
            .or_insert_with(|| PeerScore::new(now))
            .record(reason, now);
        if let Verdict::Ban = verdict {
            self.scores.remove(&key);
        }
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_follow_the_table() {
        assert_eq!(Misbehaviour::InvalidHeader.points(), 100);
        assert_eq!(Misbehaviour::InvalidBlock.points(), 100);
        assert_eq!(Misbehaviour::InvalidProof.points(), 100);
        assert_eq!(Misbehaviour::Malformed.points(), 50);
        assert_eq!(Misbehaviour::UnconnectedHeaders.points(), 20);
        assert_eq!(Misbehaviour::Unsolicited.points(), 20);
        assert_eq!(Misbehaviour::InvalidTransaction.points(), 10);
        assert_eq!(Misbehaviour::Stall.points(), 0);
    }

    #[test]
    fn thresholds_give_disconnect_then_ban() {
        let mut s = PeerScore::new(0);
        assert_eq!(s.record(Misbehaviour::Unsolicited, 0), Verdict::Keep);
        assert_eq!(s.record(Misbehaviour::Unsolicited, 0), Verdict::Keep);
        assert_eq!(s.points(0), 40);
        assert_eq!(
            s.record(Misbehaviour::InvalidTransaction, 0),
            Verdict::Disconnect
        );
        assert_eq!(s.record(Misbehaviour::Unsolicited, 0), Verdict::Disconnect);
        assert_eq!(s.record(Misbehaviour::Unsolicited, 0), Verdict::Disconnect);
        assert_eq!(s.points(0), 90);
        assert_eq!(s.record(Misbehaviour::InvalidTransaction, 0), Verdict::Ban);
        let mut s = PeerScore::new(0);
        assert_eq!(s.record(Misbehaviour::InvalidBlock, 0), Verdict::Ban);
        // One malformed frame disconnects. A second one before the points decay bans.
        let mut s = PeerScore::new(0);
        assert_eq!(s.record(Misbehaviour::Malformed, 0), Verdict::Disconnect);
        assert_eq!(s.record(Misbehaviour::Malformed, 60), Verdict::Disconnect);
        assert_eq!(s.points(60), 99);
        assert_eq!(s.record(Misbehaviour::Malformed, 60), Verdict::Ban);
    }

    #[test]
    fn points_decay_one_each_minute() {
        let mut s = PeerScore::new(1_000);
        s.record(Misbehaviour::Unsolicited, 1_000);
        assert_eq!(s.points(1_000 + 59), 20);
        assert_eq!(s.points(1_000 + 60), 19);
        assert_eq!(s.points(1_000 + 19 * 60 + 59), 1);
        assert_eq!(s.points(1_000 + 20 * 60), 0);
        assert_eq!(s.points(u64::MAX), 0);
        // A partial minute is kept across a record.
        assert_eq!(
            s.record(Misbehaviour::Unsolicited, 1_000 + 90),
            Verdict::Keep
        );
        assert_eq!(s.points(1_000 + 90), 39);
        assert_eq!(s.points(1_000 + 120), 38);
        // A clock that goes back changes nothing.
        assert_eq!(s.points(0), 39);
    }

    #[test]
    fn decay_keeps_a_slow_offender_connected() {
        let mut s = PeerScore::new(0);
        // 10 points each 10 minutes never reaches a threshold.
        for i in 0..100u64 {
            assert_eq!(
                s.record(Misbehaviour::InvalidTransaction, i * 600),
                Verdict::Keep
            );
        }
        // The same faults in one minute do.
        let mut s = PeerScore::new(0);
        let verdicts: Vec<Verdict> = (0..10)
            .map(|i| s.record(Misbehaviour::InvalidTransaction, i))
            .collect();
        assert_eq!(verdicts[3], Verdict::Keep);
        assert_eq!(verdicts[4], Verdict::Disconnect);
        assert_eq!(verdicts[9], Verdict::Ban);
    }

    #[test]
    fn stalls_disconnect_and_never_ban() {
        let mut s = PeerScore::new(0);
        assert_eq!(s.record(Misbehaviour::Stall, 0), Verdict::Keep);
        assert_eq!(s.points(0), 0);
        for _ in 0..50 {
            assert_eq!(s.record(Misbehaviour::Stall, 1), Verdict::Disconnect);
        }
        assert_eq!(s.points(1), 0);
        // One stall is removed each ten minutes.
        let mut s = PeerScore::new(0);
        s.record(Misbehaviour::Stall, 0);
        assert_eq!(s.stalls(STALL_DECAY_SECS - 1), 1);
        assert_eq!(s.stalls(STALL_DECAY_SECS), 0);
        assert_eq!(
            s.record(Misbehaviour::Stall, STALL_DECAY_SECS),
            Verdict::Keep
        );
    }

    #[test]
    fn board_scores_each_key_alone_and_forgets_a_banned_key() {
        let mut board = ScoreBoard::new(8);
        assert_eq!(
            board.record("a", Misbehaviour::Unsolicited, 0),
            Verdict::Keep
        );
        assert_eq!(
            board.record("b", Misbehaviour::Unsolicited, 0),
            Verdict::Keep
        );
        assert_eq!(board.points(&"a", 0), 20);
        assert_eq!(board.points(&"c", 0), 0);
        assert_eq!(
            board.record("a", Misbehaviour::InvalidBlock, 0),
            Verdict::Ban
        );
        assert_eq!(board.points(&"a", 0), 0);
        assert_eq!(board.len(), 1);
    }

    #[test]
    fn board_is_bounded() {
        let mut board = ScoreBoard::new(3);
        board.record(1u32, Misbehaviour::InvalidTransaction, 0);
        board.record(2, Misbehaviour::Unsolicited, 0);
        board.record(3, Misbehaviour::Unsolicited, 0);
        // Full, nothing decayed: the entry with the fewest points goes.
        board.record(4, Misbehaviour::Unsolicited, 0);
        assert_eq!(board.len(), 3);
        assert_eq!(board.points(&1, 0), 0);
        assert_eq!(board.points(&2, 0), 20);
        // Full, all decayed: the clear entries go.
        board.record(5, Misbehaviour::Unsolicited, 100_000);
        assert_eq!(board.len(), 1);
        assert!(!board.is_empty());
    }
}
