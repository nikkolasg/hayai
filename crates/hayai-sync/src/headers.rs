//! The fork-aware header chain. See `docs/plan-consensus-and-sync.md`, item B1.
//!
//! The chain holds every header that the node accepted and that is not final on a losing
//! fork. The entries form a tree with the genesis block as the root. Each entry has the
//! parent link, the height, the cumulative work and a [`Status`].
//!
//! # Best tip
//!
//! The best tip is the entry with the most cumulative work among the entries that are not
//! [`Status::Invalid`]. When two entries have equal work, the entry that the chain accepted
//! first is the best tip. This is the rule of Bitcoin Core and zcashd. Zebra and Zakura
//! take the larger hash. The first-seen rule gives a miner no reason to search for a
//! larger hash, and it keeps the node on the tip that it already builds on.
//!
//! # Finality
//!
//! The finalized height is the larger of these two values:
//!
//! - the height of the best tip minus the finality depth (1,000 blocks);
//! - the height of the last checkpoint at or below the best tip.
//!
//! The chain refuses a header whose branch leaves the best chain below the finalized
//! height. The chain removes the entries of the branches that leave the best chain below
//! the finalized height. The finalized height follows the best tip: it decreases when a
//! block of the best chain becomes [`Status::Invalid`].
//!
//! # Bodies that no peer sends
//!
//! The node can exclude a block and its descendants from the choice of the best tip
//! ([`HeaderChain::mark_unavailable`]) when no peer sends the body of the block. The mark
//! is in memory only, and [`HeaderChain::clear_unavailable`] removes each mark. The block
//! stays a valid header: the mark changes the chain that the node downloads, not the
//! validity of a block.
//!
//! # Side headers
//!
//! A header that does not extend the best tip and has no more work than the best tip is a
//! side entry. The chain holds at most [`ChainConfig::max_side_headers`] side entries when
//! a side header arrives (a reorg can put the blocks of the old best chain above the
//! bound until then). The bound does not count the unavailable entries: they are the chain
//! with the most work, and the node has them again when the mark ends. At the bound, the
//! side entry without a child and with the least work leaves the chain. A
//! new header with no more work than that entry is refused before it is in the log. A
//! header of an honest fork has about the work of the best tip, so it stays, and headers at
//! the minimum difficulty of Testnet leave first.
//!
//! # Memory and disk
//!
//! An entry in memory has 96 bytes: hash, cumulative work, offset in the header log,
//! parent, height, time, bits and status. The hash index adds 5 to 12 bytes for each entry
//! and the best chain adds 4 bytes. The full headers are in the header log
//! ([`crate::store`]). [`HeaderChain::headers_after`] reads them from the log.
//!
//! An entry keeps its position in memory for the life of the process. The position is the
//! first-seen order. A removed entry keeps its 96 bytes until the next start.
//!
//! # Start
//!
//! [`HeaderChain::open`] applies the records of the log in order. It does not run the
//! proof-of-work check, Equihash or the contextual rules again: the checksum of a record
//! shows that the chain wrote the record after these checks. The body states are not in
//! the log, except [`Status::Invalid`]. The node sets them again from its block state
//! ([`HeaderChain::mark_body_valid`]).
//!
//! The log has one header record for each header. When the chain accepts again a header
//! that it removed, the log gets a mark with the hash, and the entry uses the header
//! record that the log has. An earlier version wrote a second header record: the start
//! counts such a record ([`HeaderChain::duplicate_records`]) when the header has an entry,
//! and uses the first record.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::path::Path;

use hashbrown::HashTable;
use hayai_consensus::difficulty::block_work;
use hayai_consensus::header::{check_proof_of_work, check_version};
use hayai_consensus::{
    Checkpoints, HeaderRuleError, Network, DIFFICULTY_CONTEXT_BLOCKS, FINALITY_DEPTH,
};
use hayai_crypto::primitive_types::U256;
use hayai_wire::header::{BlockHash, BlockHeader, PowError};
use rayon::prelude::*;

use crate::locator::locator_heights;
use crate::store::{HeaderLog, LoadReport, Record, StoreError};

/// Position of the genesis entry.
const ROOT: u32 = 0;

/// Side entries that a chain holds, at most (Zakura `MAX_NON_FINALIZED_NODES_V1`).
pub const MAX_SIDE_HEADERS: usize = 65_536;

/// Configuration of a [`HeaderChain`].
#[derive(Debug, Clone)]
pub struct ChainConfig {
    pub network: Network,
    /// Depth below the best tip at which a block is final.
    pub finality_depth: u32,
    /// A header at a checkpoint height must have the checkpoint hash.
    pub checkpoints: Checkpoints,
    /// Side entries that the chain holds, at most.
    pub max_side_headers: usize,
}

impl ChainConfig {
    /// The configuration of `network` with the finality depth of the workspace
    /// ([`FINALITY_DEPTH`]) and the checkpoint list of the network
    /// ([`Network::checkpoints`]).
    pub fn new(network: Network) -> Self {
        Self {
            network,
            finality_depth: FINALITY_DEPTH,
            checkpoints: network.checkpoints().clone(),
            max_side_headers: MAX_SIDE_HEADERS,
        }
    }
}

/// What the contextual header rules read.
#[derive(Debug)]
pub struct HeaderContextView<'a> {
    pub network: Network,
    /// Height of the header under check.
    pub height: u32,
    /// The local clock, in seconds since the Unix epoch.
    pub now: u32,
    /// `nTime` of the ancestors of the header on its own branch, newest first: the parent,
    /// then the block before it. The length is the smaller of `height` and
    /// [`DIFFICULTY_CONTEXT_BLOCKS`] (113).
    pub times: &'a [u32],
    /// `nBits` of the same ancestors, in the same order.
    pub bits: &'a [u32],
}

/// The contextual header rules: difficulty and time. The node implements this trait with
/// `hayai_consensus::header::check_contextual` and `check_local_time`. The fields of
/// [`HeaderContextView`] are the fields of `hayai_consensus::ParentChain`.
pub trait HeaderRules {
    fn check(
        &self,
        header: &BlockHeader,
        context: &HeaderContextView<'_>,
    ) -> Result<(), HeaderRuleError>;
}

/// State of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The header passed the header rules. The node does not have the body.
    HeaderValid,
    /// The node has the body. The body is not validated.
    BodyKnown,
    /// The body is valid. The bodies of all ancestors are valid.
    BodyValid,
    /// The body is not valid, or the body of an ancestor is not valid.
    Invalid,
}

/// Height and hash of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tip {
    pub height: u32,
    pub hash: BlockHash,
}

/// The best tip moved from `old` to `new`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BestTipChange {
    pub old: Tip,
    pub new: Tip,
    /// The newest block that the chains of `old` and `new` share. It equals `old` when
    /// `new` extends `old`.
    pub fork_point: Tip,
}

impl BestTipChange {
    /// Whether the best chain lost blocks: `new` is not a descendant of `old`.
    pub fn is_reorg(&self) -> bool {
        self.fork_point != self.old
    }
}

/// Result of [`HeaderChain::accept_headers`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Accepted {
    /// Headers that became new entries.
    pub added: usize,
    /// Headers that the chain already had.
    pub known: usize,
    pub tip_change: Option<BestTipChange>,
}

/// Why the chain refused a header.
#[derive(Debug, thiserror::Error)]
pub enum RejectReason {
    /// A context-free rule (version, solution length, target, hash, Equihash) or a rule of
    /// the [`HeaderRules`].
    #[error(transparent)]
    Rule(#[from] HeaderRuleError),
    /// The parent is not in the chain. The caller asks the peer for the headers between
    /// its locator and this header.
    #[error("parent {0} is not in the header chain")]
    Unconnected(BlockHash),
    #[error("parent {0} is invalid")]
    InvalidParent(BlockHash),
    #[error("the header is known and invalid")]
    KnownInvalid,
    #[error("height {height} has the checkpoint {expected}")]
    CheckpointMismatch { height: u32, expected: BlockHash },
    #[error(
        "the branch leaves the best chain at height {fork_height}, below the finalized \
         height {finalized_height}"
    )]
    ForkBelowFinalized {
        fork_height: u32,
        finalized_height: u32,
    },
    /// The chain has its maximum of side entries, and each of them that can leave has
    /// more work than this header.
    #[error("the chain has its maximum of side headers with more work")]
    SideHeaderLimit,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A refused header of a batch. The headers before it are in the chain.
#[derive(Debug, thiserror::Error)]
#[error("header {index} ({hash}): {reason}")]
pub struct HeaderError {
    /// Position of the header in the batch.
    pub index: usize,
    pub hash: BlockHash,
    pub reason: RejectReason,
    /// What the headers before `index` changed.
    pub accepted: Accepted,
}

/// Why a body state change failed.
#[derive(Debug, thiserror::Error)]
pub enum MarkError {
    #[error("block {0} is not in the header chain")]
    Unknown(BlockHash),
    #[error("block {0} is invalid")]
    Invalid(BlockHash),
    #[error("block {0} has a valid body and cannot become invalid")]
    ValidBody(BlockHash),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why [`HeaderChain::open`] failed.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("the checkpoint at height 0 is {0}, not the genesis block of the network")]
    GenesisCheckpoint(BlockHash),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("header log record at offset {offset}: {reason}")]
    Replay { offset: u64, reason: RejectReason },
    #[error("header log record at offset {offset}: {reason}")]
    ReplayMark { offset: u64, reason: MarkError },
}

/// Public view of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryInfo {
    pub height: u32,
    pub time: u32,
    pub bits: u32,
    /// Work of the chain from the genesis block to this entry.
    pub work: U256,
    pub status: Status,
    /// Whether the entry is on the best chain.
    pub on_best_chain: bool,
    /// The node excluded the entry from the choice of the best tip
    /// ([`HeaderChain::mark_unavailable`]).
    pub unavailable: bool,
}

/// One header in memory.
struct Entry {
    hash: BlockHash,
    /// Work of the chain from the genesis block to this entry.
    work: U256,
    /// Offset of the header record in the header log. The genesis entry has no record.
    offset: u64,
    /// Position of the parent. The genesis entry names itself.
    parent: u32,
    height: u32,
    time: u32,
    bits: u32,
    /// `None`: the chain removed the entry. The entry is not in the hash index.
    status: Option<Status>,
    /// No peer sends the body of the entry or of an ancestor: the entry is not a
    /// candidate for the best tip.
    unavailable: bool,
}

/// The fields of a header that an entry reads.
#[derive(Clone, Copy)]
struct Link {
    hash: BlockHash,
    prev_hash: BlockHash,
    time: u32,
    bits: u32,
}

impl Link {
    fn of(header: &BlockHeader, hash: BlockHash) -> Self {
        Self {
            hash,
            prev_hash: header.prev_hash,
            time: header.time,
            bits: header.bits,
        }
    }
}

/// A header that passed the structural checks, before it becomes an entry.
struct Connection {
    parent: u32,
    height: u32,
    work: U256,
    /// The parent is unavailable.
    unavailable: bool,
}

/// The entries in memory. The header log is not part of this structure.
struct Dag {
    network: Network,
    finality_depth: u32,
    checkpoints: Checkpoints,
    max_side_headers: usize,
    /// Entries in the order of acceptance. A parent is before its children.
    entries: Vec<Entry>,
    /// Hash to position, for the entries that the chain did not remove.
    index: HashTable<u32>,
    hasher: RandomState,
    /// Position of the best-chain entry at each height. The last one is the best tip.
    best_chain: Vec<u32>,
    /// Entries that are not on the best chain and that the chain did not remove.
    side: BTreeSet<u32>,
    /// The position of the entry of each header that the chain removed and did not accept
    /// again. The log has the record of the header.
    removed: HashMap<BlockHash, u32>,
    /// No best-chain entry below this height has the state [`Status::HeaderValid`].
    download_from: u32,
}

impl Dag {
    fn new(config: ChainConfig) -> Result<Self, OpenError> {
        let ChainConfig {
            network,
            finality_depth,
            checkpoints,
            max_side_headers,
        } = config;
        let params = network.params();
        if let Some(hash) = checkpoints.hash_at(0) {
            if hash != params.genesis_hash {
                return Err(OpenError::GenesisCheckpoint(hash));
            }
        }
        // The genesis block of every network has the proof-of-work limit as its target.
        let Some(work) = block_work(params.pow_limit_bits) else {
            unreachable!("the proof-of-work limit is a target");
        };
        let mut dag = Self {
            network,
            finality_depth,
            checkpoints,
            max_side_headers,
            entries: Vec::new(),
            index: HashTable::new(),
            hasher: RandomState::new(),
            best_chain: vec![ROOT],
            side: BTreeSet::new(),
            removed: HashMap::new(),
            download_from: 1,
        };
        dag.insert_entry(Entry {
            hash: params.genesis_hash,
            work,
            offset: u64::MAX,
            parent: ROOT,
            height: 0,
            time: params.genesis_time,
            bits: params.pow_limit_bits,
            status: Some(Status::BodyValid),
            unavailable: false,
        });
        Ok(dag)
    }

    fn insert_entry(&mut self, entry: Entry) -> u32 {
        let id = u32::try_from(self.entries.len()).expect("fewer than 2^32 headers");
        let hash = self.hasher.hash_one(entry.hash.0);
        self.entries.push(entry);
        let (entries, hasher) = (&self.entries, &self.hasher);
        self.index.insert_unique(hash, id, |other| {
            hasher.hash_one(entries[*other as usize].hash.0)
        });
        id
    }

    fn entry(&self, id: u32) -> &Entry {
        &self.entries[id as usize]
    }

    fn lookup(&self, hash: &BlockHash) -> Option<u32> {
        self.index
            .find(self.hasher.hash_one(hash.0), |id| {
                self.entry(*id).hash == *hash
            })
            .copied()
    }

    fn best(&self) -> u32 {
        *self.best_chain.last().expect("the genesis entry")
    }

    fn is_best(&self, id: u32) -> bool {
        self.best_chain.get(self.entry(id).height as usize) == Some(&id)
    }

    fn tip(&self, id: u32) -> Tip {
        let entry = self.entry(id);
        Tip {
            height: entry.height,
            hash: entry.hash,
        }
    }

    fn finalized_height(&self) -> u32 {
        let best_height = self.entry(self.best()).height;
        let by_depth = best_height.saturating_sub(self.finality_depth);
        match self.checkpoints.last_at_or_below(best_height) {
            Some(checkpoint) => by_depth.max(checkpoint),
            None => by_depth,
        }
    }

    /// The checks that read only the header: the version and the proof of work of
    /// hayai-consensus (solution length, target at or below the limit, and on Mainnet and
    /// Testnet the hash against the target and the Equihash solution).
    fn check_context_free(&self, header: &BlockHeader) -> Result<(), HeaderRuleError> {
        check_version(header)?;
        check_proof_of_work(self.network, header)
    }

    /// The checks that read the entries: parent, checkpoint, finality.
    fn connect(
        &self,
        prev_hash: &BlockHash,
        bits: u32,
        hash: &BlockHash,
    ) -> Result<Connection, RejectReason> {
        let Some(parent) = self.lookup(prev_hash) else {
            return Err(RejectReason::Unconnected(*prev_hash));
        };
        let parent_entry = self.entry(parent);
        if let Some(Status::Invalid) = parent_entry.status {
            return Err(RejectReason::InvalidParent(*prev_hash));
        }
        let height = parent_entry.height + 1;
        if let Some(expected) = self.checkpoints.hash_at(height) {
            if expected != *hash {
                return Err(RejectReason::CheckpointMismatch { height, expected });
            }
        }
        // Each side entry leaves the best chain at or above the finalized height, because
        // `prune` removes the other side entries. So only a parent on the best chain can
        // start a branch below the finalized height.
        let finalized_height = self.finalized_height();
        if parent_entry.height < finalized_height && self.is_best(parent) {
            return Err(RejectReason::ForkBelowFinalized {
                fork_height: parent_entry.height,
                finalized_height,
            });
        }
        let Some(work) = block_work(bits) else {
            return Err(HeaderRuleError::Pow(PowError::InvalidBits(bits)).into());
        };
        let Some(work) = parent_entry.work.checked_add(work) else {
            return Err(HeaderRuleError::WorkOverflow.into());
        };
        Ok(Connection {
            parent,
            height,
            work,
            unavailable: parent_entry.unavailable,
        })
    }

    /// Writes into `times` and `bits` the values of the ancestors that the contextual
    /// rules read, from `parent` back.
    fn ancestors(&self, parent: u32, times: &mut Vec<u32>, bits: &mut Vec<u32>) {
        times.clear();
        bits.clear();
        let mut id = parent;
        while times.len() < DIFFICULTY_CONTEXT_BLOCKS {
            let entry = self.entry(id);
            times.push(entry.time);
            bits.push(entry.bits);
            if id == ROOT {
                break;
            }
            id = entry.parent;
        }
    }

    /// Adds the entry of a connected header and selects the best tip. `offset` is the
    /// offset of the header record.
    fn push(&mut self, link: Link, connection: Connection, offset: u64) {
        let best = self.best();
        self.removed.remove(&link.hash);
        let id = self.insert_entry(Entry {
            hash: link.hash,
            work: connection.work,
            offset,
            parent: connection.parent,
            height: connection.height,
            time: link.time,
            bits: link.bits,
            status: Some(Status::HeaderValid),
            unavailable: connection.unavailable,
        });
        // Strictly more work: on equal work the first-seen entry stays the best tip. An
        // unavailable entry is never the best tip, and the bound of the side entries does
        // not count it.
        if connection.unavailable {
            self.side.insert(id);
        } else if connection.work <= self.entry(best).work {
            self.side.insert(id);
            self.evict_side();
        } else if connection.parent == best {
            self.best_chain.push(id);
            self.prune();
        } else {
            self.side.insert(id);
            self.set_best(id);
            self.prune();
        }
    }

    /// Makes the chain of `new` the best chain. The entries that leave the best chain
    /// become side entries.
    fn set_best(&mut self, new: u32) {
        let mut branch = Vec::new();
        let mut id = new;
        while !self.is_best(id) {
            branch.push(id);
            id = self.entry(id).parent;
        }
        let keep = self.entry(id).height as usize + 1;
        self.side.extend(self.best_chain.drain(keep..));
        for id in branch.into_iter().rev() {
            self.side.remove(&id);
            self.best_chain.push(id);
        }
        self.download_from = self.download_from.min(keep as u32);
    }

    /// Removes the side entries whose branch leaves the best chain below the finalized
    /// height.
    fn prune(&mut self) {
        if self.side.is_empty() {
            return;
        }
        let finalized_height = self.finalized_height();
        let mut removed = Vec::new();
        // Ascending positions: a parent is before its children.
        for &id in &self.side {
            let parent = self.entry(self.entry(id).parent);
            let remove = match parent.status {
                None => true,
                Some(_) => parent.height < finalized_height && self.is_best(self.entry(id).parent),
            };
            if remove {
                self.entries[id as usize].status = None;
                removed.push(id);
            }
        }
        for id in removed {
            self.forget(id);
        }
    }

    /// Takes the side entry `id`, whose status is `None`, out of the side set and of the
    /// hash index.
    fn forget(&mut self, id: u32) {
        self.side.remove(&id);
        let hash = self.entry(id).hash;
        self.removed.insert(hash, id);
        let Ok(slot) = self
            .index
            .find_entry(self.hasher.hash_one(hash.0), |other| {
                self.entries[*other as usize].hash == hash
            })
        else {
            unreachable!("a side entry is in the hash index");
        };
        slot.remove();
    }

    /// The side entries that the bound counts: the ones that are not unavailable.
    fn counted_side(&self) -> usize {
        self.side
            .iter()
            .filter(|id| !self.entry(**id).unavailable)
            .count()
    }

    /// The counted side entry without a child in the side set that has the least work. On
    /// equal work it is the newest one.
    fn weakest_side_leaf(&self) -> Option<u32> {
        let parents: BTreeSet<u32> = self.side.iter().map(|id| self.entry(*id).parent).collect();
        self.side
            .iter()
            .filter(|id| !self.entry(**id).unavailable && !parents.contains(id))
            .min_by_key(|id| (self.entry(**id).work, std::cmp::Reverse(**id)))
            .copied()
    }

    /// Removes counted side entries while the chain has more than its maximum of them.
    fn evict_side(&mut self) {
        let mut counted = self.counted_side();
        while counted > self.max_side_headers {
            // Each counted entry can have an unavailable child. Then no entry leaves, and
            // the chain refuses the next side header (`side_is_full_for`).
            let Some(id) = self.weakest_side_leaf() else {
                break;
            };
            self.entries[id as usize].status = None;
            self.forget(id);
            counted -= 1;
        }
    }

    /// Whether the chain refuses the new header of `connection`: the header is a side
    /// header, the side set is full, and the header has no more work than the entry that
    /// would leave for it.
    fn side_is_full_for(&self, connection: &Connection) -> bool {
        let side = !connection.unavailable && connection.work <= self.entry(self.best()).work;
        if !side || self.counted_side() < self.max_side_headers {
            return false;
        }
        match self.weakest_side_leaf() {
            Some(id) => connection.work <= self.entry(id).work,
            None => true,
        }
    }

    /// The entry with the most work among `floor` and the side entries, without the
    /// invalid and the unavailable entries. On equal work it is the entry that the chain
    /// accepted first. `floor` is an entry that can be the best tip.
    fn most_work(&self, floor: u32) -> u32 {
        let mut best = floor;
        for &candidate in &self.side {
            let entry = self.entry(candidate);
            if entry.unavailable || matches!(entry.status, Some(Status::Invalid)) {
                continue;
            }
            let best_work = self.entry(best).work;
            if entry.work > best_work || (entry.work == best_work && candidate < best) {
                best = candidate;
            }
        }
        best
    }

    /// Sets the unavailable mark of `id` and of its descendants and selects the best tip.
    fn exclude(&mut self, id: u32) {
        let on_best_chain = self.is_best(id);
        self.entries[id as usize].unavailable = true;
        // A descendant is after its ancestor.
        for at in id as usize + 1..self.entries.len() {
            let parent = self.entries[at].parent as usize;
            if self.entries[parent].unavailable {
                self.entries[at].unavailable = true;
            }
        }
        if on_best_chain {
            let best = self.most_work(self.entry(id).parent);
            self.set_best(best);
            self.prune();
        }
    }

    /// Removes each unavailable mark and selects the best tip. Returns whether an entry
    /// had the mark.
    fn include_all(&mut self) -> bool {
        let mut any = false;
        for entry in &mut self.entries {
            any |= std::mem::take(&mut entry.unavailable);
        }
        if any {
            let best = self.most_work(self.best());
            if best != self.best() {
                self.set_best(best);
                self.prune();
            }
        }
        any
    }

    /// Sets `id` and its descendants to [`Status::Invalid`] and selects the best tip.
    fn invalidate(&mut self, id: u32) {
        let on_best_chain = self.is_best(id);
        self.entries[id as usize].status = Some(Status::Invalid);
        // A descendant is after its ancestor.
        for at in id as usize + 1..self.entries.len() {
            let Some(_) = self.entries[at].status else {
                continue;
            };
            let parent = self.entries[at].parent as usize;
            if let Some(Status::Invalid) = self.entries[parent].status {
                self.entries[at].status = Some(Status::Invalid);
            }
        }
        if !on_best_chain {
            return;
        }
        // The best tip is the parent of `id` or a side entry. Most work first, then the
        // entry that the chain accepted first.
        let best = self.most_work(self.entry(id).parent);
        self.set_best(best);
        self.prune();
    }

    /// The change of the best tip since it was `old`.
    fn tip_change(&self, old: u32) -> Option<BestTipChange> {
        let new = self.best();
        if new == old {
            return None;
        }
        let mut fork = old;
        while !self.is_best(fork) {
            fork = self.entry(fork).parent;
        }
        Some(BestTipChange {
            old: self.tip(old),
            new: self.tip(new),
            fork_point: self.tip(fork),
        })
    }

    fn memory_bytes(&self) -> usize {
        self.entries.capacity() * size_of::<Entry>()
            + self.index.allocation_size()
            + self.best_chain.capacity() * size_of::<u32>()
    }
}

/// What a start does with one header of the log: the entry, or no entry for a header that
/// this start refuses and an earlier start accepted. `offset` is the offset of the header
/// record.
fn replay_header(
    dag: &mut Dag,
    skipped: &mut HashSet<BlockHash>,
    link: Link,
    offset: u64,
) -> Result<(), RejectReason> {
    // A header whose parent the start skipped is skipped too.
    let connection = match skipped.contains(&link.prev_hash) {
        true => Err(RejectReason::Unconnected(link.prev_hash)),
        false => dag.connect(&link.prev_hash, link.bits, &link.hash),
    };
    match connection {
        Ok(connection) => dag.push(link, connection, offset),
        // The checkpoint list or the finalized height of this start refuses a header that
        // an earlier start accepted: a release with a new checkpoint, or a side header
        // whose branch is now final on the other side. The record stays in the log
        // without an entry.
        Err(RejectReason::CheckpointMismatch { .. } | RejectReason::ForkBelowFinalized { .. }) => {
            skipped.insert(link.hash);
        }
        Err(RejectReason::Unconnected(parent)) if skipped.contains(&parent) => {
            skipped.insert(link.hash);
        }
        Err(reason) => return Err(reason),
    }
    Ok(())
}

/// The header chain of one network. See the module documentation.
pub struct HeaderChain {
    dag: Dag,
    log: HeaderLog,
    /// Records of the log that have no entry after [`HeaderChain::open`].
    skipped_records: u64,
    /// Header records of the log that repeat a header with an entry.
    duplicate_records: u64,
}

impl HeaderChain {
    /// Opens the chain whose header log is the file `path`. A new file gives a chain with
    /// only the genesis block. The function cuts an incomplete last record of the log and
    /// reports it in the [`LoadReport`].
    pub fn open(config: ChainConfig, path: &Path) -> Result<(Self, LoadReport), OpenError> {
        let mut dag = Dag::new(config)?;
        let mut skipped: HashSet<BlockHash> = HashSet::new();
        let mut duplicate_records = 0u64;
        let (log, report) = HeaderLog::open(path, |offset, record| match record {
            Record::Header(header) => {
                let hash = header.hash();
                // A second record of a header with an entry changes nothing. The hash
                // covers each byte of the header, so the two records have the same header.
                let None = dag.lookup(&hash) else {
                    duplicate_records += 1;
                    return Ok(());
                };
                replay_header(&mut dag, &mut skipped, Link::of(&header, hash), offset)
                    .map_err(|reason| OpenError::Replay { offset, reason })
            }
            Record::Again(hash) => match dag.removed.get(&hash).copied() {
                Some(old) => {
                    let entry = dag.entry(old);
                    let link = Link {
                        hash,
                        prev_hash: dag.entry(entry.parent).hash,
                        time: entry.time,
                        bits: entry.bits,
                    };
                    let header_offset = entry.offset;
                    replay_header(&mut dag, &mut skipped, link, header_offset)
                        .map_err(|reason| OpenError::Replay { offset, reason })
                }
                // The start did not remove the header, or skipped it: the mark has no
                // effect.
                None if skipped.contains(&hash) => Ok(()),
                None => match dag.lookup(&hash) {
                    Some(_) => Ok(()),
                    None => Err(OpenError::ReplayMark {
                        offset,
                        reason: MarkError::Unknown(hash),
                    }),
                },
            },
            Record::Invalid(hash) => {
                match dag.lookup(&hash) {
                    Some(id) => dag.invalidate(id),
                    // The mark of a skipped header has no effect.
                    None if skipped.contains(&hash) => {}
                    None => {
                        return Err(OpenError::ReplayMark {
                            offset,
                            reason: MarkError::Unknown(hash),
                        })
                    }
                }
                Ok(())
            }
        })?;
        Ok((
            Self {
                dag,
                log,
                skipped_records: skipped.len() as u64,
                duplicate_records,
            },
            report,
        ))
    }

    /// Adds `headers` to the chain, in order. Each header must have its parent in the
    /// chain or before it in `headers`.
    ///
    /// Each new header passes the context-free checks, the structural checks (parent,
    /// checkpoint, finality) and `rules` with the ancestors of its own branch. The
    /// context-free checks of the batch run on the rayon pool. The function stops at the
    /// first header that fails. The headers before it stay in the chain.
    pub fn accept_headers(
        &mut self,
        headers: &[BlockHeader],
        rules: &dyn HeaderRules,
        now: u32,
    ) -> Result<Accepted, Box<HeaderError>> {
        let old = self.dag.best();
        let dag = &self.dag;
        let checked: Vec<(BlockHash, Result<(), HeaderRuleError>)> = headers
            .par_iter()
            .map(|header| {
                let hash = header.hash();
                let verdict = match dag.lookup(&hash) {
                    Some(_) => Ok(()),
                    None => dag.check_context_free(header),
                };
                (hash, verdict)
            })
            .collect();
        let mut accepted = Accepted::default();
        let mut context = (Vec::new(), Vec::new());
        for (index, (header, (hash, verdict))) in headers.iter().zip(checked).enumerate() {
            let added = verdict
                .map_err(RejectReason::Rule)
                .and_then(|()| self.accept_one(header, hash, rules, now, &mut context));
            match added {
                Ok(true) => accepted.added += 1,
                Ok(false) => accepted.known += 1,
                Err(reason) => {
                    accepted.tip_change = self.dag.tip_change(old);
                    return Err(Box::new(HeaderError {
                        index,
                        hash,
                        reason,
                        accepted,
                    }));
                }
            }
        }
        accepted.tip_change = self.dag.tip_change(old);
        Ok(accepted)
    }

    /// Adds one header that passed the context-free checks. `Ok(false)`: the chain has the
    /// header.
    fn accept_one(
        &mut self,
        header: &BlockHeader,
        hash: BlockHash,
        rules: &dyn HeaderRules,
        now: u32,
        (times, bits): &mut (Vec<u32>, Vec<u32>),
    ) -> Result<bool, RejectReason> {
        if let Some(id) = self.dag.lookup(&hash) {
            return match self.dag.entry(id).status {
                Some(Status::Invalid) => Err(RejectReason::KnownInvalid),
                _ => Ok(false),
            };
        }
        let connection = self.dag.connect(&header.prev_hash, header.bits, &hash)?;
        if self.dag.side_is_full_for(&connection) {
            return Err(RejectReason::SideHeaderLimit);
        }
        self.dag.ancestors(connection.parent, times, bits);
        rules.check(
            header,
            &HeaderContextView {
                network: self.dag.network,
                height: connection.height,
                now,
                times,
                bits,
            },
        )?;
        // The record is in the log before the entry is in memory. The log has the header
        // record of a header that the chain removed: the header gets a mark and no second
        // header record.
        let offset = match self.dag.removed.get(&hash) {
            Some(old) => {
                self.log.append_again(&hash)?;
                self.dag.entry(*old).offset
            }
            None => self.log.append_header(header)?,
        };
        self.dag.push(Link::of(header, hash), connection, offset);
        Ok(true)
    }

    /// The best tip.
    pub fn best_tip(&self) -> Tip {
        self.dag.tip(self.dag.best())
    }

    /// Cumulative work of the best tip.
    pub fn best_work(&self) -> U256 {
        self.dag.entry(self.dag.best()).work
    }

    /// The finalized height. See the module documentation.
    pub fn finalized_height(&self) -> u32 {
        self.dag.finalized_height()
    }

    /// The height of the last checkpoint at or below the best tip. `None`: the best chain
    /// reached no checkpoint. Each block of the best chain at or below this height is an
    /// ancestor of a checkpoint.
    pub fn last_checkpoint_reached(&self) -> Option<u32> {
        self.dag
            .checkpoints
            .last_at_or_below(self.best_tip().height)
    }

    /// The entry of `hash`. `None`: the chain never had the header, or removed it.
    pub fn entry(&self, hash: &BlockHash) -> Option<EntryInfo> {
        let id = self.dag.lookup(hash)?;
        let entry = self.dag.entry(id);
        Some(EntryInfo {
            height: entry.height,
            time: entry.time,
            bits: entry.bits,
            work: entry.work,
            status: entry.status.expect("an indexed entry has a status"),
            on_best_chain: self.dag.is_best(id),
            unavailable: entry.unavailable,
        })
    }

    /// The blocks of the best chain from `height` to the best tip, in height order.
    pub fn best_chain_from(&self, height: u32) -> impl Iterator<Item = Tip> + '_ {
        self.dag
            .best_chain
            .iter()
            .skip(height as usize)
            .map(|id| self.dag.tip(*id))
    }

    /// The block locator of the best chain: the hashes at
    /// [`locator_heights`](crate::locator::locator_heights), newest first.
    pub fn locator(&self) -> Vec<BlockHash> {
        locator_heights(self.best_tip().height)
            .into_iter()
            .map(|height| self.dag.entry(self.dag.best_chain[height as usize]).hash)
            .collect()
    }

    /// The answer to `getheaders`: the headers of the best chain after the first hash of
    /// `locator` that is on the best chain, up to the header `stop` and at most `max`
    /// headers. When no hash of `locator` is on the best chain, the answer starts after the
    /// genesis block (Bitcoin Core and zcashd `FindForkInGlobalIndex`). The function reads
    /// the headers from the header log.
    pub fn headers_after(
        &self,
        locator: &[BlockHash],
        stop: &BlockHash,
        max: usize,
    ) -> Result<Vec<BlockHeader>, StoreError> {
        let start = locator
            .iter()
            .filter_map(|hash| self.dag.lookup(hash))
            .find(|id| self.dag.is_best(*id))
            .map_or(0, |id| self.dag.entry(id).height as usize);
        let mut headers = Vec::new();
        for id in self.dag.best_chain.iter().skip(start + 1).take(max) {
            let entry = self.dag.entry(*id);
            headers.push(self.log.read_header(entry.offset)?);
            if entry.hash == *stop {
                break;
            }
        }
        Ok(headers)
    }

    /// The first `n` blocks of the best chain, in height order, whose body the node does
    /// not have ([`Status::HeaderValid`]).
    pub fn next_blocks_to_download(&mut self, n: usize) -> Vec<Tip> {
        let mut blocks = Vec::new();
        let from = self.dag.download_from as usize;
        for (at, id) in self.dag.best_chain.iter().enumerate().skip(from) {
            if blocks.len() == n {
                break;
            }
            match self.dag.entry(*id).status {
                Some(Status::HeaderValid) => blocks.push(self.dag.tip(*id)),
                // The blocks before the first missing body stay out of the next search.
                _ if blocks.is_empty() => self.dag.download_from = at as u32 + 1,
                _ => {}
            }
        }
        blocks
    }

    /// Records that the node has the body of `hash`.
    pub fn mark_body_received(&mut self, hash: &BlockHash) -> Result<(), MarkError> {
        let id = self.live(hash)?;
        let status = &mut self.dag.entries[id as usize].status;
        if let Some(Status::HeaderValid) = status {
            *status = Some(Status::BodyKnown);
        }
        Ok(())
    }

    /// Records that the node no longer has the body of `hash`: a [`Status::BodyKnown`] entry
    /// goes back to [`Status::HeaderValid`]. The node calls this when it drops a body that it
    /// did not validate. A valid body stays valid.
    pub fn mark_body_missing(&mut self, hash: &BlockHash) -> Result<(), MarkError> {
        let id = self.live(hash)?;
        let entry = &mut self.dag.entries[id as usize];
        if let Some(Status::BodyKnown) = entry.status {
            entry.status = Some(Status::HeaderValid);
            self.dag.download_from = self.dag.download_from.min(entry.height);
        }
        Ok(())
    }

    /// Records that the body of `hash` is valid. The bodies of its ancestors are then
    /// valid too, and the function records that.
    pub fn mark_body_valid(&mut self, hash: &BlockHash) -> Result<(), MarkError> {
        let mut id = self.live(hash)?;
        loop {
            let entry = &mut self.dag.entries[id as usize];
            if let Some(Status::BodyValid) = entry.status {
                return Ok(());
            }
            entry.status = Some(Status::BodyValid);
            id = entry.parent;
        }
    }

    /// Records that the body of `hash` is not valid. The block and its descendants become
    /// [`Status::Invalid`]. When the block is on the best chain, the best tip moves. The
    /// header log keeps the record, so the state is the same after a start.
    pub fn mark_invalid(&mut self, hash: &BlockHash) -> Result<Option<BestTipChange>, MarkError> {
        let Some(id) = self.dag.lookup(hash) else {
            return Err(MarkError::Unknown(*hash));
        };
        match self.dag.entry(id).status {
            Some(Status::Invalid) => return Ok(None),
            Some(Status::BodyValid) => return Err(MarkError::ValidBody(*hash)),
            _ => {}
        }
        self.log.append_invalid(hash)?;
        let old = self.dag.best();
        self.dag.invalidate(id);
        Ok(self.dag.tip_change(old))
    }

    /// Excludes `hash` and its descendants from the choice of the best tip: no peer sends
    /// the body of `hash`. When the block is on the best chain, the best tip moves to the
    /// chain with the most work among the other chains. The mark is in memory only: the
    /// header stays valid, and a start has no mark.
    pub fn mark_unavailable(
        &mut self,
        hash: &BlockHash,
    ) -> Result<Option<BestTipChange>, MarkError> {
        let id = self.live(hash)?;
        if let Some(Status::BodyValid) = self.dag.entry(id).status {
            return Err(MarkError::ValidBody(*hash));
        }
        let old = self.dag.best();
        self.dag.exclude(id);
        Ok(self.dag.tip_change(old))
    }

    /// Removes each mark of [`HeaderChain::mark_unavailable`] and selects the best tip
    /// among all chains. Returns whether an entry had the mark.
    pub fn clear_unavailable(&mut self) -> bool {
        self.dag.include_all()
    }

    /// The tips of the header tree: the best tip, then each entry outside the best chain
    /// that has no child.
    pub fn tips(&self) -> Vec<BlockHash> {
        let parents: std::collections::HashSet<u32> = self
            .dag
            .side
            .iter()
            .map(|id| self.dag.entry(*id).parent)
            .collect();
        std::iter::once(self.dag.best())
            .chain(
                self.dag
                    .side
                    .iter()
                    .copied()
                    .filter(|id| !parents.contains(id)),
            )
            .map(|id| self.dag.entry(id).hash)
            .collect()
    }

    /// The number of blocks of the chain of `hash` that are not on the chain of `other`:
    /// the blocks above the newest block that the two chains share. `None`: the chain does
    /// not have one of the two headers.
    pub fn blocks_not_on(&self, hash: &BlockHash, other: &BlockHash) -> Option<u32> {
        let (mut a, mut b) = (self.dag.lookup(hash)?, self.dag.lookup(other)?);
        let height = |id: u32| self.dag.entry(id).height;
        let start = height(a);
        while a != b {
            let (height_a, height_b) = (height(a), height(b));
            if height_a >= height_b {
                a = self.dag.entry(a).parent;
            }
            if height_b >= height_a {
                b = self.dag.entry(b).parent;
            }
        }
        Some(start - height(a))
    }

    /// The newest block of the chain of `hash` that is on the best chain: the block at
    /// which the branch of `hash` leaves the best chain, or `hash` itself.
    pub fn best_chain_ancestor(&self, hash: &BlockHash) -> Option<Tip> {
        let mut id = self.dag.lookup(hash)?;
        while !self.dag.is_best(id) {
            id = self.dag.entry(id).parent;
        }
        Some(self.dag.tip(id))
    }

    /// Header records of the log that had no entry at the start: the checkpoint list or
    /// the finalized height refused the header or an ancestor of it.
    pub fn skipped_records(&self) -> u64 {
        self.skipped_records
    }

    /// Header records of the log that repeated a header with an entry at the start. An
    /// earlier version wrote a second header record when it accepted a removed header
    /// again.
    pub fn duplicate_records(&self) -> u64 {
        self.duplicate_records
    }

    /// The position of `hash`, which must be in the chain and not invalid.
    fn live(&self, hash: &BlockHash) -> Result<u32, MarkError> {
        let Some(id) = self.dag.lookup(hash) else {
            return Err(MarkError::Unknown(*hash));
        };
        match self.dag.entry(id).status {
            Some(Status::Invalid) => Err(MarkError::Invalid(*hash)),
            _ => Ok(id),
        }
    }

    /// Entries in memory, the removed ones included.
    pub fn entries(&self) -> usize {
        self.dag.entries.len()
    }

    /// Bytes that the entries, the hash index and the best chain allocate.
    pub fn memory_bytes(&self) -> usize {
        self.dag.memory_bytes()
    }

    /// Makes the header log durable (`fsync`).
    pub fn sync(&self) -> Result<(), StoreError> {
        self.log.sync()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_is_96_bytes() {
        assert_eq!(size_of::<Entry>(), 96);
    }
}
