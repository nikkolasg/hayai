//! The best chain as headers: what the P2P layer serves (`getheaders`, `has_block`) and what
//! the header check reads (the height of the parent, and the times and the `nBits` of the
//! blocks before the header).
//!
//! hayai-state keeps coins, nullifiers and trees, not headers, so the node keeps this index
//! beside the chain. The driver pushes a header after each commit and pops one per
//! disconnected block.
//!
//! The index also holds pending headers: headers that passed the check at the relay and
//! whose blocks wait in the driver's queue. The relay forwards a block after its header
//! check and before its validation, so a child can arrive while its parent is still in the
//! queue. Its header check then finds the parent among the pending headers.

use std::collections::HashMap;
use std::time::Instant;

use hayai_consensus::difficulty::median_time_past;
use hayai_consensus::header::{check_header, HeaderVerdict};
use hayai_consensus::{ParentChain, DIFFICULTY_CONTEXT_BLOCKS};
use hayai_relay::{HeaderCheck, HeaderError, ParentInfo};
use hayai_trace::{event, Table, Tracer};
use hayai_wire::header::{BlockHash, BlockHeader};
use parking_lot::RwLock;
use serde_json::json;
use std::sync::Arc;

use crate::metrics::NodeMetrics;
use crate::params::NetParams;

/// A block at the start of the index. The node did not receive it as a block, so the
/// index holds no header for it: the genesis block of a full node, the start block of a
/// shadow node and the blocks before it, or the base block of a restart and the blocks
/// before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeedBlock {
    pub hash: BlockHash,
    pub time: u32,
    /// `nBits` of the block. `None` when the node does not know it: the genesis block,
    /// whose header the node does not parse. No rule reads the `nBits` of the genesis
    /// block (the blocks up to height 17 have the proof-of-work limit).
    pub bits: Option<u32>,
}

impl SeedBlock {
    /// The seed of an index from `ancestors` (`(hash, time)`, oldest first) and `bits`
    /// (oldest first). The last entry of each list belongs to the same block. An ancestor
    /// that is older than every entry of `bits` has no `nBits`.
    pub fn from_lists(ancestors: &[(BlockHash, u32)], bits: &[u32]) -> Vec<SeedBlock> {
        let without_bits = ancestors.len().saturating_sub(bits.len());
        let known = &bits[bits.len().saturating_sub(ancestors.len())..];
        ancestors
            .iter()
            .enumerate()
            .map(|(i, (hash, time))| SeedBlock {
                hash: *hash,
                time: *time,
                bits: i.checked_sub(without_bits).map(|k| known[k]),
            })
            .collect()
    }
}

struct Entry {
    hash: BlockHash,
    time: u32,
    /// `nBits` of the block: of its header, or of its seed.
    bits: Option<u32>,
    /// `None` for the seed entries.
    header: Option<BlockHeader>,
}

struct Pending {
    prev: BlockHash,
    height: u32,
    time: u32,
    bits: u32,
}

/// Pending headers kept at most; a header older than the tip leaves on the next commit.
const MAX_PENDING: usize = 4096;

struct Inner {
    /// Height of `entries[0]`.
    first_height: u32,
    entries: Vec<Entry>,
    by_hash: HashMap<BlockHash, u32>,
    pending: HashMap<BlockHash, Pending>,
}

/// Header index of the best chain.
pub struct HeaderIndex {
    inner: RwLock<Inner>,
}

impl HeaderIndex {
    /// An index whose oldest entries are `seed` (oldest first), the last one at `height`.
    pub fn new(height: u32, seed: &[SeedBlock]) -> Self {
        assert!(!seed.is_empty(), "the index starts at a known block");
        let first_height = height + 1 - seed.len() as u32;
        let entries: Vec<Entry> = seed
            .iter()
            .map(|block| Entry {
                hash: block.hash,
                time: block.time,
                bits: block.bits,
                header: None,
            })
            .collect();
        let by_hash = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.hash, first_height + i as u32))
            .collect();
        Self {
            inner: RwLock::new(Inner {
                first_height,
                entries,
                by_hash,
                pending: HashMap::new(),
            }),
        }
    }

    /// `(hash, time)` of the block at `height` and of up to `count - 1` blocks before it,
    /// oldest first. `None` when the index does not hold `height`.
    pub fn ancestors_at(&self, height: u32, count: usize) -> Option<Vec<(BlockHash, u32)>> {
        let inner = self.inner.read();
        let end = (height.checked_sub(inner.first_height)? as usize) + 1;
        let entries = inner.entries.get(end.saturating_sub(count)..end)?;
        Some(entries.iter().map(|e| (e.hash, e.time)).collect())
    }

    pub fn tip(&self) -> (u32, BlockHash) {
        let inner = self.inner.read();
        let last = inner.entries.last().expect("never empty");
        (
            inner.first_height + inner.entries.len() as u32 - 1,
            last.hash,
        )
    }

    pub fn height_of(&self, hash: &BlockHash) -> Option<u32> {
        self.inner.read().by_hash.get(hash).copied()
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.inner.read().by_hash.contains_key(hash)
    }

    pub fn is_pending(&self, hash: &BlockHash) -> bool {
        self.inner.read().pending.contains_key(hash)
    }

    /// Median of the times of `hash` and its ten predecessors that the index holds.
    pub fn median_time_past(&self, hash: &BlockHash) -> Option<u32> {
        median_time_past(&self.parent_context(hash)?.times)
    }

    /// The context of a header whose parent is `hash`, a committed block or a pending
    /// header: the height of `hash`, and the times and the `nBits` of `hash` and of the
    /// blocks before it, newest first, at most [`DIFFICULTY_CONTEXT_BLOCKS`] of each. The
    /// `nBits` list ends before the first block whose `nBits` the index does not know.
    pub fn parent_context(&self, hash: &BlockHash) -> Option<ParentInfo> {
        let inner = self.inner.read();
        let mut times: Vec<u32> = Vec::with_capacity(DIFFICULTY_CONTEXT_BLOCKS);
        let mut bits: Vec<u32> = Vec::with_capacity(DIFFICULTY_CONTEXT_BLOCKS);
        let mut cursor = *hash;
        let mut height = None;
        while let Some(p) = inner.pending.get(&cursor) {
            height.get_or_insert(p.height);
            if times.len() < DIFFICULTY_CONTEXT_BLOCKS {
                times.push(p.time);
                bits.push(p.bits);
            }
            cursor = p.prev;
        }
        let committed = *inner.by_hash.get(&cursor)?;
        let height = height.unwrap_or(committed);
        let end = (committed - inner.first_height) as usize + 1;
        let start = end.saturating_sub(DIFFICULTY_CONTEXT_BLOCKS - times.len());
        let mut bits_known = true;
        for entry in inner.entries[start..end].iter().rev() {
            times.push(entry.time);
            match entry.bits {
                Some(entry_bits) if bits_known => bits.push(entry_bits),
                _ => bits_known = false,
            }
        }
        Some(ParentInfo {
            height,
            times,
            bits,
        })
    }

    /// Records a header that passed the check and whose block goes to the driver.
    pub fn add_pending(&self, header: &BlockHeader, height: u32) {
        let mut inner = self.inner.write();
        if inner.pending.len() >= MAX_PENDING {
            return;
        }
        inner.pending.insert(
            header.hash(),
            Pending {
                prev: header.prev_hash,
                height,
                time: header.time,
                bits: header.bits,
            },
        );
    }

    /// Forgets a pending header whose block failed validation.
    pub fn remove_pending(&self, hash: &BlockHash) {
        self.inner.write().pending.remove(hash);
    }

    /// Appends the header of the block committed on the tip and drops the pending headers
    /// at or below its height.
    pub fn push(&self, header: BlockHeader) {
        let mut inner = self.inner.write();
        let hash = header.hash();
        let height = inner.first_height + inner.entries.len() as u32;
        assert_eq!(
            inner.entries.last().map(|e| e.hash),
            Some(header.prev_hash),
            "the header extends the tip"
        );
        inner.by_hash.insert(hash, height);
        inner.entries.push(Entry {
            hash,
            time: header.time,
            bits: Some(header.bits),
            header: Some(header),
        });
        inner.pending.retain(|_, p| p.height > height);
    }

    /// Removes the tip entry. The seed is never popped.
    pub fn pop(&self) -> Option<BlockHash> {
        let mut inner = self.inner.write();
        let Some(Entry {
            header: Some(_), ..
        }) = inner.entries.last()
        else {
            return None;
        };
        let entry = inner.entries.pop()?;
        inner.by_hash.remove(&entry.hash);
        Some(entry.hash)
    }

    /// Drops the oldest entries so that about `keep` remain: a full node reads only the
    /// newest blocks from this index. The entries leave in groups, so that a commit does
    /// not move the whole list.
    pub fn prune(&self, keep: usize) {
        let mut inner = self.inner.write();
        let excess = inner.entries.len().saturating_sub(keep);
        if excess < keep / 4 + 1 {
            return;
        }
        let dropped: Vec<Entry> = inner.entries.drain(..excess).collect();
        for entry in dropped {
            inner.by_hash.remove(&entry.hash);
        }
        inner.first_height += excess as u32;
    }

    /// Headers after the first locator hash the index holds, up to `stop` and at most
    /// `limit`.
    pub fn headers_after(
        &self,
        locator: &[BlockHash],
        stop: &BlockHash,
        limit: usize,
    ) -> Vec<BlockHeader> {
        let inner = self.inner.read();
        let Some(start) = locator.iter().find_map(|h| inner.by_hash.get(h)) else {
            return Vec::new();
        };
        let from = (*start - inner.first_height) as usize + 1;
        let mut out = Vec::new();
        for entry in inner.entries.iter().skip(from) {
            let Some(header) = &entry.header else {
                continue;
            };
            out.push(header.clone());
            if entry.hash == *stop || out.len() == limit {
                break;
            }
        }
        out
    }
}

fn now_secs() -> u32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    u32::try_from(secs).unwrap_or(u32::MAX)
}

/// The header check that gates forwarding and validation: block not already in the chain,
/// parent known, then every header rule of `hayai_consensus::header::check_header` on the
/// context of the index, with the clock of the node.
///
/// The index of a shadow node starts at a block above the genesis block. Its seed holds
/// the time and the `nBits` of the start block and of the blocks before it,
/// [`DIFFICULTY_CONTEXT_BLOCKS`] blocks in all (fewer only when the chain is shorter), so
/// every rule runs from the first block after the start. A
/// context that is too short for a rule (an index with a shorter seed) is never a pass: the
/// header passes the rules that did not run only when `trust_short_context` is set, and
/// each such header increments `hayai_shadow_trusted_bits_total`. Without it the header
/// is rejected.
pub struct NodeHeaderCheck {
    pub params: NetParams,
    pub index: Arc<HeaderIndex>,
    pub tracer: Tracer,
    pub metrics: Arc<NodeMetrics>,
    /// Shadow mode: the node trusts upstream for the rules that its context cannot check.
    /// Full mode starts at the genesis block and never sets it.
    pub trust_short_context: bool,
}

impl NodeHeaderCheck {
    fn rules(&self, header: &BlockHeader, hash: &BlockHash) -> Result<u32, HeaderError> {
        if self.index.contains(hash) {
            return Err(HeaderError::AlreadyInChain(*hash));
        }
        let Some(parent) = self.index.parent_context(&header.prev_hash) else {
            return Err(HeaderError::ParentUnknown(header.prev_hash));
        };
        let height = parent.height + 1;
        let chain = ParentChain {
            height,
            times: &parent.times,
            bits: &parent.bits,
        };
        match check_header(self.params.kind, header, &chain, Some(now_secs()))? {
            HeaderVerdict::Checked => {}
            HeaderVerdict::ContextTooShort(_) if self.trust_short_context => {
                self.metrics.trusted_bits.inc();
            }
            HeaderVerdict::ContextTooShort(unchecked) => {
                return Err(HeaderError::ContextTooShort(unchecked));
            }
        }
        Ok(height)
    }
}

impl NodeHeaderCheck {
    /// The rules, with a `block_header_checked` row. Returns the block's height.
    pub fn verify(&self, header: &BlockHeader) -> Result<u32, HeaderError> {
        let started = Instant::now();
        let hash = header.hash();
        let verdict = self.rules(header, &hash);
        let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.tracer.emit(
            Table::BlockSync,
            event::BLOCK_HEADER_CHECKED,
            || match &verdict {
                Ok(height) => json!({
                    "hash": hash.to_string(),
                    "height": height,
                    "result": "ok",
                    "elapsed_us": elapsed_us,
                }),
                Err(e) => json!({
                    "hash": hash.to_string(),
                    "result": "rejected",
                    "reason": e.to_string(),
                    "elapsed_us": elapsed_us,
                }),
            },
        );
        verdict
    }
}

/// The relay's check: the rules, then the header becomes pending until the driver commits
/// or rejects its block.
impl HeaderCheck for NodeHeaderCheck {
    fn check(&self, header: &BlockHeader) -> Result<(), HeaderError> {
        let height = self.verify(header)?;
        self.index.add_pending(header, height);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{NetworkKind, REGTEST_POW_LIMIT_BITS};
    use hayai_consensus::header::{check_solution_length, HeaderRuleError};
    use hayai_wire::header::{PowError, PowParams};

    const REGTEST_GENESIS_TIME: u32 = 1_296_688_602;

    fn regtest_genesis_hash() -> BlockHash {
        NetParams::new(NetworkKind::Regtest).genesis().0
    }

    fn header(prev: BlockHash, time: u32, bits: u32) -> BlockHeader {
        BlockHeader {
            version: 4,
            prev_hash: prev,
            merkle_root: [1; 32],
            block_commitments: [2; 32],
            time,
            bits,
            nonce: [0; 32],
            solution: vec![0; PowParams::REGTEST.solution_len()],
        }
    }

    /// A seed block without `nBits`.
    fn seed_block(hash: BlockHash, time: u32) -> SeedBlock {
        SeedBlock {
            hash,
            time,
            bits: None,
        }
    }

    fn check(index: Arc<HeaderIndex>) -> NodeHeaderCheck {
        check_on(NetworkKind::Regtest, index, false)
    }

    fn check_on(
        kind: NetworkKind,
        index: Arc<HeaderIndex>,
        trust_short_context: bool,
    ) -> NodeHeaderCheck {
        NodeHeaderCheck {
            params: NetParams::new(kind),
            index,
            tracer: Tracer::disabled(),
            metrics: Arc::new(NodeMetrics::new(&hayai_rpc::Registry::new())),
            trust_short_context,
        }
    }

    /// A header of Zebra's Mainnet vectors.
    fn mainnet_header(height: u32) -> BlockHeader {
        let name = format!(
            "{}/../hayai-bench/tests/vectors/block-main-0-000-{height:03}.hex",
            env!("CARGO_MANIFEST_DIR")
        );
        let hex = std::fs::read_to_string(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
        BlockHeader::parse(&hex::decode(hex.trim()).expect("hex")).expect("a header")
    }

    #[test]
    fn index_tracks_heights_times_and_headers() {
        let genesis = regtest_genesis_hash();
        let index = Arc::new(HeaderIndex::new(0, &[seed_block(genesis, 100)]));
        let mut prev = genesis;
        let mut hashes = Vec::new();
        for i in 1..=12u32 {
            let h = header(prev, 100 + i * 10, REGTEST_POW_LIMIT_BITS);
            prev = h.hash();
            hashes.push(prev);
            index.push(h);
        }
        assert_eq!(index.tip(), (12, prev));
        assert_eq!(index.height_of(&hashes[4]), Some(5));
        // Times 100, 110, ..., 220: the last eleven end at 220 and start at 120.
        assert_eq!(index.median_time_past(&prev), Some(170));
        assert_eq!(index.median_time_past(&genesis), Some(100));
        let after = index.headers_after(&[hashes[9], genesis], &BlockHash([0; 32]), 2000);
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].hash(), hashes[10]);
        let after = index.headers_after(&[genesis], &hashes[2], 2000);
        assert_eq!(after.len(), 3);
        assert_eq!(index.pop(), Some(prev));
        assert_eq!(index.tip().0, 11);
        assert!(!index.contains(&prev));
        let empty = Arc::new(HeaderIndex::new(0, &[seed_block(genesis, 1)]));
        assert_eq!(empty.pop(), None);
    }

    #[test]
    fn pending_headers_extend_the_committed_chain_until_their_commit() {
        let genesis = regtest_genesis_hash();
        let index = Arc::new(HeaderIndex::new(0, &[seed_block(genesis, 100)]));
        let c = check(index.clone());
        let now = now_secs();
        let first = header(genesis, now, REGTEST_POW_LIMIT_BITS);
        let second = header(first.hash(), now + 1, REGTEST_POW_LIMIT_BITS);
        // Without the parent pending, the child has no parent.
        assert_eq!(
            c.check(&second),
            Err(HeaderError::ParentUnknown(first.hash()))
        );
        assert_eq!(c.check(&first), Ok(()));
        assert!(index.is_pending(&first.hash()));
        assert_eq!(
            index.parent_context(&first.hash()),
            Some(ParentInfo {
                height: 1,
                times: vec![now, 100],
                bits: vec![REGTEST_POW_LIMIT_BITS],
            })
        );
        assert_eq!(index.median_time_past(&first.hash()), Some(now));
        assert_eq!(c.check(&second), Ok(()));
        assert_eq!(
            index.parent_context(&second.hash()),
            Some(ParentInfo {
                height: 2,
                times: vec![now + 1, now, 100],
                bits: vec![REGTEST_POW_LIMIT_BITS; 2],
            })
        );
        assert_eq!(index.median_time_past(&second.hash()), Some(now));
        // The commit of the parent drops its pending entry; the child stays pending.
        index.push(first.clone());
        assert!(!index.is_pending(&first.hash()));
        assert!(index.is_pending(&second.hash()));
        index.remove_pending(&second.hash());
        assert!(!index.is_pending(&second.hash()));
        // The pure rules leave no pending entry.
        assert_eq!(c.verify(&second), Ok(2));
        assert!(!index.is_pending(&second.hash()));
    }

    #[test]
    fn regtest_check_applies_the_waiver_and_the_time_rules() {
        let genesis = regtest_genesis_hash();
        let index = Arc::new(HeaderIndex::new(
            0,
            &[seed_block(genesis, REGTEST_GENESIS_TIME)],
        ));
        let c = check(index.clone());
        let now = now_secs();
        // Height 1 is exempt from the median-time-past maximum. The solution is never
        // verified and the hash meets no filter.
        let first = header(genesis, now, REGTEST_POW_LIMIT_BITS);
        assert_eq!(c.check(&first), Ok(()));
        index.push(first.clone());
        assert_eq!(
            c.check(&first),
            Err(HeaderError::AlreadyInChain(first.hash()))
        );
        let early = header(first.hash(), now - 1, REGTEST_POW_LIMIT_BITS);
        let Err(HeaderError::Rule(HeaderRuleError::TimeTooEarly { .. })) = c.check(&early) else {
            panic!("time at or below the median time past");
        };
        // The median of the two times is the time of the first block. Height 2 is the
        // first height with the maximum.
        let late = header(first.hash(), now + 5_401, REGTEST_POW_LIMIT_BITS);
        let Err(HeaderError::Rule(HeaderRuleError::TimeTooLate { .. })) = c.check(&late) else {
            panic!("time beyond the median time past plus 90 min");
        };
        let easy = header(first.hash(), now + 1, 0x2010_0000);
        assert_eq!(
            c.check(&easy),
            Err(HeaderError::Rule(HeaderRuleError::Pow(
                PowError::TargetAboveLimit(0x2010_0000)
            )))
        );
        // Regtest has no expected bits: a harder target than the limit passes.
        assert_eq!(c.verify(&header(first.hash(), now + 1, 0x1f07_ffff)), Ok(2));
        let orphan = header(BlockHash([9; 32]), now + 1, REGTEST_POW_LIMIT_BITS);
        assert_eq!(
            c.check(&orphan),
            Err(HeaderError::ParentUnknown(BlockHash([9; 32])))
        );
        let mut old = header(first.hash(), now + 1, REGTEST_POW_LIMIT_BITS);
        old.version = 3;
        assert_eq!(
            c.check(&old),
            Err(HeaderError::Rule(HeaderRuleError::Version(3)))
        );
        // The shape rule of the waiver: a (200, 9) solution is not a Regtest solution.
        let mut mainnet = header(first.hash(), now + 1, REGTEST_POW_LIMIT_BITS);
        mainnet.solution = vec![0; PowParams::MAINNET.solution_len()];
        assert_eq!(
            c.check(&mainnet),
            Err(HeaderError::Rule(HeaderRuleError::SolutionLength {
                expected: 36,
                got: 1344
            }))
        );
        assert_eq!(
            c.check(&header(first.hash(), now + 1, REGTEST_POW_LIMIT_BITS)),
            Ok(())
        );
    }

    /// The recorded zcashd and Zakura Regtest genesis block parses, has the hash of the
    /// constant that the index starts from, and its 36-byte solution passes the shape rule.
    #[test]
    fn recorded_regtest_genesis_matches_the_index_seed() {
        let hex = include_str!("../../hayai-wire/tests/vectors/block-regtest-0-000-000.hex");
        let bytes = hex::decode(hex.trim()).expect("hex");
        let genesis = BlockHeader::parse(&bytes).expect("a Regtest header parses");
        assert_eq!(genesis.hash(), regtest_genesis_hash());
        assert_eq!(
            NetParams::new(NetworkKind::Regtest).genesis().1,
            REGTEST_GENESIS_TIME
        );
        assert_eq!(genesis.time, REGTEST_GENESIS_TIME);
        assert_eq!(genesis.bits, REGTEST_POW_LIMIT_BITS);
        assert_eq!(
            check_solution_length(NetworkKind::Regtest, &genesis),
            Ok(())
        );
        let Err(HeaderRuleError::SolutionLength { .. }) =
            check_solution_length(NetworkKind::Testnet, &genesis)
        else {
            panic!("a Regtest solution on Testnet");
        };
    }

    /// The context of a header: times from the pending headers, then the committed
    /// entries, newest first, at most 113. The bits end before the first seed entry.
    #[test]
    fn the_context_holds_113_times_and_the_known_bits() {
        let seed: Vec<SeedBlock> = (0..11u8)
            .map(|i| seed_block(BlockHash([i; 32]), 50 + u32::from(i)))
            .collect();
        let index = HeaderIndex::new(110, &seed);
        let mut prev = seed[10].hash;
        assert_eq!(
            index.parent_context(&prev),
            Some(ParentInfo {
                height: 110,
                times: (50..=60).rev().collect(),
                bits: Vec::new(),
            })
        );
        for i in 1..=105u32 {
            let h = header(prev, 100 + i, 0x2000_0000 + i);
            prev = h.hash();
            index.push(h);
        }
        let pending = header(prev, 500, 0x1f00_0001);
        index.add_pending(&pending, 216);
        let Some(context) = index.parent_context(&pending.hash()) else {
            panic!("a pending header has a context");
        };
        assert_eq!(context.height, 216);
        // The pending header, the 105 committed headers, then 7 of the 11 seed entries.
        let mut times = vec![500];
        times.extend((101..=205).rev());
        times.extend((54..=60).rev());
        assert_eq!(context.times.len(), DIFFICULTY_CONTEXT_BLOCKS);
        assert_eq!(context.times, times);
        let mut bits = vec![0x1f00_0001];
        bits.extend((1..=105).rev().map(|i| 0x2000_0000 + i));
        assert_eq!(context.bits, bits);
        assert_eq!(index.parent_context(&BlockHash([0xee; 32])), None);
    }

    /// A node from the Mainnet genesis block: the real blocks 1 to 10 pass every rule
    /// with the context of the index, and no header is trusted.
    #[test]
    fn mainnet_headers_from_genesis_pass_without_trust() {
        let genesis = mainnet_header(0);
        let index = Arc::new(HeaderIndex::new(
            0,
            &[seed_block(genesis.hash(), genesis.time)],
        ));
        let c = check_on(NetworkKind::Mainnet, index.clone(), false);
        for height in 1..=10 {
            let h = mainnet_header(height);
            assert_eq!(c.verify(&h), Ok(height));
            // A harder target than the chain requires is not the expected one.
            let mut hard = h.clone();
            hard.bits = 0x1f07_fffe;
            assert_eq!(
                c.verify(&hard),
                Err(HeaderError::Rule(HeaderRuleError::WrongBits {
                    expected: 0x1f07_ffff,
                    got: 0x1f07_fffe
                }))
            );
            index.push(h);
        }
        assert_eq!(c.metrics.trusted_bits.get(), 0);
    }

    /// An index that starts above the genesis block without the ancestors of its start:
    /// the context of the next header is too short. The header passes only when the node
    /// trusts its source, and the counter records it.
    #[test]
    fn a_short_context_is_trusted_and_counted_or_rejected() {
        let start = mainnet_header(1);
        let next = mainnet_header(2);
        let seed = [seed_block(start.hash(), start.time)];
        let strict = check_on(
            NetworkKind::Mainnet,
            Arc::new(HeaderIndex::new(1, &seed)),
            false,
        );
        let Err(HeaderError::ContextTooShort(unchecked)) = strict.verify(&next) else {
            panic!("the median-time-past of height 2 reads two times");
        };
        assert!(unchecked.time);
        assert_eq!(strict.metrics.trusted_bits.get(), 0);

        let index = Arc::new(HeaderIndex::new(1, &seed));
        let trusting = check_on(NetworkKind::Mainnet, index.clone(), true);
        assert_eq!(trusting.verify(&next), Ok(2));
        assert_eq!(trusting.metrics.trusted_bits.get(), 1);
        // The rules that the context allows still run on a trusted header.
        let mut hard = next.clone();
        hard.bits = 0x1f07_fffe;
        assert!(matches!(
            trusting.verify(&hard),
            Err(HeaderError::Rule(HeaderRuleError::WrongBits { .. }))
        ));
        let mut bad = next.clone();
        bad.nonce[0] ^= 1;
        assert!(matches!(
            trusting.verify(&bad),
            Err(HeaderError::Rule(
                HeaderRuleError::Pow(PowError::HashAboveTarget) | HeaderRuleError::Equihash(_)
            ))
        ));
        assert_eq!(trusting.metrics.trusted_bits.get(), 1);
    }

    /// The seed lists of a restart: the `nBits` list is aligned with the newest ancestor,
    /// and an ancestor that is older than the list has no `nBits`.
    #[test]
    fn seed_blocks_align_the_bits_with_the_newest_ancestor() {
        let ancestors: Vec<(BlockHash, u32)> = (0..4u8)
            .map(|i| (BlockHash([i; 32]), u32::from(i)))
            .collect();
        let bits_of = |bits: &[u32]| -> Vec<Option<u32>> {
            SeedBlock::from_lists(&ancestors, bits)
                .iter()
                .map(|block| block.bits)
                .collect()
        };
        assert_eq!(bits_of(&[]), [None; 4]);
        assert_eq!(bits_of(&[7, 8]), [None, None, Some(7), Some(8)]);
        assert_eq!(bits_of(&[5, 6, 7, 8]), [Some(5), Some(6), Some(7), Some(8)]);
        // A list that is longer than the ancestors gives its newest entries.
        assert_eq!(
            bits_of(&[3, 4, 5, 6, 7, 8]),
            [Some(5), Some(6), Some(7), Some(8)]
        );
        let seed = SeedBlock::from_lists(&ancestors, &[7, 8]);
        assert_eq!((seed[3].hash, seed[3].time), (BlockHash([3; 32]), 3));
    }

    /// A seed of 28 blocks with their `nBits` gives the whole context of the difficulty
    /// rule: the expected `nBits` of the first block after the start has a value.
    #[test]
    fn a_seed_of_28_blocks_gives_the_whole_difficulty_context() {
        use hayai_consensus::difficulty::{expected_bits, DifficultyError};

        let blocks = |count: u32| -> Vec<SeedBlock> {
            (0..count)
                .map(|i| SeedBlock {
                    hash: BlockHash([i as u8 + 1; 32]),
                    time: 1_700_000_000 + i * 75,
                    bits: Some(0x1c01_0000),
                })
                .collect()
        };
        let next_bits = |seed: &[SeedBlock]| {
            let index = HeaderIndex::new(3_000_000, seed);
            let (height, tip) = index.tip();
            let parent = index.parent_context(&tip).expect("the tip has a context");
            assert_eq!(parent.times.len(), seed.len());
            assert_eq!(parent.bits.len(), seed.len());
            let chain = ParentChain {
                height: height + 1,
                times: &parent.times,
                bits: &parent.bits,
            };
            let time = seed[seed.len() - 1].time + 75;
            expected_bits(NetworkKind::Mainnet, time, &chain)
        };
        // Blocks at the target spacing keep the target, less the remainder of the division
        // by the window timespan (`floor(MeanTarget / AveragingWindowTimespan)`).
        assert_eq!(next_bits(&blocks(28)), Ok(0x1c00_ffff));
        assert!(matches!(
            next_bits(&blocks(27)),
            Err(DifficultyError::ContextTooShort(_))
        ));
        // Seed blocks without `nBits` give the times only.
        let mut without_bits = blocks(28);
        for block in &mut without_bits[11..] {
            block.bits = None;
        }
        let index = HeaderIndex::new(3_000_000, &without_bits);
        let parent = index.parent_context(&index.tip().1).expect("a context");
        assert_eq!((parent.times.len(), parent.bits.len()), (28, 0));
    }

    /// A shadow start above the genesis block with the whole seed: every header rule runs
    /// on the first block after the start, with no trust, and the counter stays at 0.
    #[test]
    fn a_whole_seed_needs_no_trust() {
        let seed: Vec<SeedBlock> = (0..=5)
            .map(|height| {
                let header = mainnet_header(height);
                SeedBlock {
                    hash: header.hash(),
                    time: header.time,
                    bits: Some(header.bits),
                }
            })
            .collect();
        for trust in [false, true] {
            let index = Arc::new(HeaderIndex::new(5, &seed));
            let c = check_on(NetworkKind::Mainnet, index.clone(), trust);
            for height in 6..=10 {
                let h = mainnet_header(height);
                assert_eq!(c.verify(&h), Ok(height));
                index.push(h);
            }
            assert_eq!(c.metrics.trusted_bits.get(), 0);
        }
    }
}
