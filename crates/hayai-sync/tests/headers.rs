//! The header chain on generated Regtest headers.
//!
//! Regtest has the proof-of-work waiver, so a generated header needs no Equihash solution
//! and no hash below its target. The tests give each header a `bits` value at or below the
//! Regtest limit to set its work.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use hayai_consensus::{
    Checkpoints, DuplicateCheckpoint, HeaderRuleError, Network, DIFFICULTY_CONTEXT_BLOCKS,
};
use hayai_crypto::primitive_types::U256;
use hayai_sync::headers::{
    BestTipChange, ChainConfig, HeaderChain, HeaderContextView, HeaderRules, MarkError, OpenError,
    RejectReason, Status, Tip,
};
use hayai_sync::locator::locator_heights;
use hayai_sync::store::{HeaderLog, StoreError};
use hayai_wire::header::{BlockHash, BlockHeader, PowError, PowParams};
use proptest::prelude::*;

/// The Regtest limit: work 17 for each block.
const EASY: u32 = 0x200f_0f0f;
/// Work 32 for each block.
const HARD: u32 = 0x2007_ffff;
/// Work 8,192 for each block.
const HARDEST: u32 = 0x1f07_ffff;
/// Frame, kind byte and a 177-byte Regtest header.
const RECORD_BYTES: u64 = 20 + 1 + 177;
const NOW: u32 = 2_000_000_000;
const ZERO: BlockHash = BlockHash([0; 32]);

/// The contextual rule that accepts every header.
struct Permissive;

impl HeaderRules for Permissive {
    fn check(&self, _: &BlockHeader, _: &HeaderContextView<'_>) -> Result<(), HeaderRuleError> {
        Ok(())
    }
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("scratch dir under target/")
}

fn genesis() -> BlockHash {
    Network::Regtest.params().genesis_hash
}

fn regtest() -> ChainConfig {
    ChainConfig::new(Network::Regtest)
}

fn open(dir: &Path, config: ChainConfig) -> HeaderChain {
    let (chain, report) = HeaderChain::open(config, &dir.join("headers.log")).unwrap();
    assert_eq!(report.torn_bytes, 0);
    chain
}

/// A header on `prev`. `salt` makes the hash unique.
fn header(prev: BlockHash, time: u32, bits: u32, salt: u64) -> BlockHeader {
    let mut merkle_root = [0u8; 32];
    merkle_root[..8].copy_from_slice(&salt.to_le_bytes());
    BlockHeader {
        version: 4,
        prev_hash: prev,
        merkle_root,
        block_commitments: [0; 32],
        time,
        bits,
        nonce: [0; 32],
        solution: vec![0; PowParams::REGTEST.solution_len()],
    }
}

/// `n` headers in a row on `prev`. The salts start at `salt`.
fn branch(prev: BlockHash, n: usize, bits: u32, salt: u64) -> Vec<BlockHeader> {
    let mut headers: Vec<BlockHeader> = Vec::with_capacity(n);
    let mut prev = prev;
    for i in 0..n as u64 {
        let next = header(prev, (salt + i) as u32, bits, salt + i);
        prev = next.hash();
        headers.push(next);
    }
    headers
}

fn accept(chain: &mut HeaderChain, headers: &[BlockHeader]) -> Option<BestTipChange> {
    let mut first: Option<BestTipChange> = None;
    let mut last = None;
    // Batches of the size of a `headers` message.
    for batch in headers.chunks(160) {
        let accepted = chain.accept_headers(batch, &Permissive, NOW).unwrap();
        assert_eq!(accepted.added, batch.len());
        if let Some(change) = accepted.tip_change {
            first.get_or_insert(change);
            last = Some(change);
        }
    }
    let (first, last) = (first?, last?);
    Some(BestTipChange {
        old: first.old,
        new: last.new,
        fork_point: first.fork_point,
    })
}

fn reject(chain: &mut HeaderChain, headers: &[BlockHeader]) -> RejectReason {
    chain
        .accept_headers(headers, &Permissive, NOW)
        .expect_err("the chain must refuse the header")
        .reason
}

fn tip(height: u32, header: &BlockHeader) -> Tip {
    Tip {
        height,
        hash: header.hash(),
    }
}

fn status(chain: &HeaderChain, header: &BlockHeader) -> Option<Status> {
    chain.entry(&header.hash()).map(|entry| entry.status)
}

#[test]
fn best_tip_is_the_most_work_not_the_most_blocks() {
    let dir = scratch();
    let mut chain = open(dir.path(), regtest());
    let main = branch(genesis(), 3_000, EASY, 0);
    let change = accept(&mut chain, &main).unwrap();
    assert_eq!(change.new, tip(3_000, &main[2_999]));
    assert!(!change.is_reorg());
    assert_eq!(chain.best_work(), U256::from(17 * 3_001u64));

    // A branch from height 2,000 with 32 work for each block against 17. After 531 blocks
    // its work is 2,001 x 17 + 531 x 32 = 51,009, at most the 51,017 of the main chain.
    let fork = branch(main[1_999].hash(), 600, HARD, 10_000);
    assert_eq!(accept(&mut chain, &fork[..531]), None);
    assert_eq!(chain.best_tip(), tip(3_000, &main[2_999]));
    let entry = chain.entry(&fork[530].hash()).unwrap();
    assert_eq!((entry.height, entry.on_best_chain), (2_531, false));
    assert_eq!(entry.work, U256::from(2_001 * 17 + 531 * 32u64));

    // Block 532 of the branch has more work: the best tip moves to a lower height.
    let change = accept(&mut chain, &fork[531..]).unwrap();
    assert!(change.is_reorg());
    assert_eq!(change.old, tip(3_000, &main[2_999]));
    assert_eq!(change.fork_point, tip(2_000, &main[1_999]));
    assert_eq!(change.new, tip(2_600, &fork[599]));
    assert!(!chain.entry(&main[2_999].hash()).unwrap().on_best_chain);
    assert!(chain.entry(&main[1_999].hash()).unwrap().on_best_chain);
    let best: Vec<Tip> = chain.best_chain_from(1_999).collect();
    assert_eq!(best.len(), 602);
    assert_eq!(best[0], tip(1_999, &main[1_998]));
    assert_eq!(best[2], tip(2_001, &fork[0]));

    // More blocks with less work do not win.
    let long = branch(main[2_999].hash(), 100, EASY, 20_000);
    assert_eq!(accept(&mut chain, &long), None);
    assert_eq!(chain.best_tip(), tip(2_600, &fork[599]));
}

#[test]
fn equal_work_keeps_the_first_seen_tip() {
    let dir = scratch();
    let mut chain = open(dir.path(), regtest());
    let first = header(genesis(), 1, EASY, 1);
    let second = header(genesis(), 1, EASY, 2);
    assert_eq!(
        accept(&mut chain, std::slice::from_ref(&first))
            .unwrap()
            .new,
        tip(1, &first)
    );
    assert_eq!(accept(&mut chain, std::slice::from_ref(&second)), None);
    assert_eq!(chain.best_tip(), tip(1, &first));

    // More work on the second branch moves the tip.
    let child = header(second.hash(), 2, EASY, 3);
    let change = accept(&mut chain, std::slice::from_ref(&child)).unwrap();
    assert_eq!(change.fork_point.height, 0);
    assert!(change.is_reorg());

    // `first` and `second` have equal work again when `child` is invalid: the first-seen
    // one is the best tip.
    let change = chain.mark_invalid(&child.hash()).unwrap().unwrap();
    assert_eq!(change.new, tip(1, &first));
    assert_eq!(change.fork_point.height, 0);
}

#[test]
fn invalid_reaches_every_descendant() {
    let dir = scratch();
    let mut chain = open(dir.path(), regtest());
    let a = branch(genesis(), 10, EASY, 0);
    let b = branch(a[2].hash(), 3, EASY, 100);
    let c = branch(a[6].hash(), 2, EASY, 200);
    accept(&mut chain, &a);
    accept(&mut chain, &b);
    accept(&mut chain, &c);
    chain.mark_body_valid(&a[1].hash()).unwrap();

    let change = chain.mark_invalid(&a[3].hash()).unwrap().unwrap();
    assert_eq!(change.old, tip(10, &a[9]));
    assert_eq!(change.new, tip(6, &b[2]));
    assert_eq!(change.fork_point, tip(3, &a[2]));
    for header in a[3..].iter().chain(&c) {
        assert_eq!(status(&chain, header), Some(Status::Invalid));
    }
    assert_eq!(status(&chain, &a[0]), Some(Status::BodyValid));
    assert_eq!(status(&chain, &a[1]), Some(Status::BodyValid));
    assert_eq!(status(&chain, &a[2]), Some(Status::HeaderValid));
    assert_eq!(status(&chain, &b[2]), Some(Status::HeaderValid));
    // A second mark changes nothing.
    assert_eq!(chain.mark_invalid(&a[5].hash()).unwrap(), None);

    let on_invalid = header(a[9].hash(), 50, HARDEST, 300);
    assert!(matches!(
        reject(&mut chain, std::slice::from_ref(&on_invalid)),
        RejectReason::InvalidParent(parent) if parent == a[9].hash()
    ));
    assert!(matches!(
        reject(&mut chain, &a[4..5]),
        RejectReason::KnownInvalid
    ));
    assert!(matches!(
        chain.mark_body_valid(&a[4].hash()),
        Err(MarkError::Invalid(_))
    ));
    assert!(matches!(
        chain.mark_body_received(&c[0].hash()),
        Err(MarkError::Invalid(_))
    ));
    assert!(matches!(
        chain.mark_invalid(&a[1].hash()),
        Err(MarkError::ValidBody(_))
    ));
    assert!(matches!(
        chain.mark_invalid(&genesis()),
        Err(MarkError::ValidBody(_))
    ));
    assert!(matches!(
        chain.mark_invalid(&on_invalid.hash()),
        Err(MarkError::Unknown(_))
    ));

    // An invalid block on a side branch does not move the best tip.
    let d = branch(a[2].hash(), 2, EASY, 400);
    accept(&mut chain, &d);
    assert_eq!(chain.mark_invalid(&d[0].hash()).unwrap(), None);
    assert_eq!(status(&chain, &d[1]), Some(Status::Invalid));
    assert_eq!(chain.best_tip(), tip(6, &b[2]));
}

/// A rule that compares the context with the ancestors that the test finds by a walk over
/// the `prev_hash` links of every generated header.
struct ContextOracle {
    /// Hash to `(prev_hash, time, bits)`.
    headers: HashMap<BlockHash, (BlockHash, u32, u32)>,
    /// Hash to height.
    heights: RefCell<HashMap<BlockHash, u32>>,
    /// A header with this time fails the rule.
    refuse_time: u32,
}

impl HeaderRules for ContextOracle {
    fn check(
        &self,
        header: &BlockHeader,
        context: &HeaderContextView<'_>,
    ) -> Result<(), HeaderRuleError> {
        if header.time == self.refuse_time {
            return Err(HeaderRuleError::TimeTooEarly {
                time: header.time,
                median_time_past: header.time,
            });
        }
        let params = Network::Regtest.params();
        let mut expected = Vec::new();
        let mut hash = header.prev_hash;
        while expected.len() < DIFFICULTY_CONTEXT_BLOCKS {
            let Some((prev, time, bits)) = self.headers.get(&hash) else {
                assert_eq!(hash, params.genesis_hash);
                expected.push((params.genesis_time, params.pow_limit_bits));
                break;
            };
            expected.push((*time, *bits));
            hash = *prev;
        }
        let got: Vec<(u32, u32)> = context
            .times
            .iter()
            .copied()
            .zip(context.bits.iter().copied())
            .collect();
        assert_eq!(context.times.len(), context.bits.len());
        assert_eq!(got, expected);
        let mut heights = self.heights.borrow_mut();
        let height = heights[&header.prev_hash] + 1;
        assert_eq!(context.height, height);
        assert_eq!(
            context.times.len(),
            (height as usize).min(DIFFICULTY_CONTEXT_BLOCKS)
        );
        assert_eq!(context.network, Network::Regtest);
        assert_eq!(context.now, NOW);
        heights.insert(header.hash(), height);
        Ok(())
    }
}

#[test]
fn rules_get_the_ancestors_of_the_branch() {
    let dir = scratch();
    let mut chain = open(dir.path(), regtest());
    let main = branch(genesis(), 100, EASY, 0);
    // The branches leave the main chain inside and outside the 28-block context.
    let near = branch(main[89].hash(), 40, HARD, 1_000);
    let far = branch(main[9].hash(), 40, HARDEST, 2_000);
    let all: Vec<&BlockHeader> = main.iter().chain(&near).chain(&far).collect();
    let oracle = ContextOracle {
        headers: all
            .iter()
            .map(|h| (h.hash(), (h.prev_hash, h.time, h.bits)))
            .collect(),
        heights: RefCell::new(HashMap::from([(genesis(), 0)])),
        refuse_time: 2_020,
    };
    for headers in [&main, &near] {
        let accepted = chain.accept_headers(headers, &oracle, NOW).unwrap();
        assert_eq!(accepted.added, headers.len());
    }
    assert_eq!(oracle.heights.borrow().len(), 141);

    // Header 20 of `far` fails the rule. The 20 headers before it are in the chain.
    let error = chain.accept_headers(&far, &oracle, NOW).unwrap_err();
    assert_eq!(error.index, 20);
    assert_eq!(error.hash, far[20].hash());
    assert!(matches!(
        error.reason,
        RejectReason::Rule(HeaderRuleError::TimeTooEarly { time: 2_020, .. })
    ));
    assert_eq!(error.accepted.added, 20);
    assert_eq!(error.accepted.tip_change.unwrap().new, tip(30, &far[19]));
    assert_eq!(status(&chain, &far[19]), Some(Status::HeaderValid));
    assert_eq!(status(&chain, &far[20]), None);
    assert_eq!(chain.best_tip(), tip(30, &far[19]));
}

#[test]
fn context_free_checks_and_batch_order() {
    let dir = scratch();
    let mut chain = open(dir.path(), regtest());
    let main = branch(genesis(), 5, EASY, 0);

    // The version is below 4 as a signed integer: 3, and each value with the high bit set.
    for version in [3, 0x8000_0000, 0x8000_0004] {
        let mut old_version = main[0].clone();
        old_version.version = version;
        assert!(matches!(
            reject(&mut chain, &[old_version]),
            RejectReason::Rule(HeaderRuleError::Version(found)) if found == version
        ));
    }
    // Regtest has no hash filter: a header can state the target 1, with a work of 2^255.
    // The second header of that work makes the work of the chain 2^256 or more.
    const TARGET_ONE: u32 = 0x0101_0000;
    let heavy = branch(genesis(), 2, TARGET_ONE, 9_000);
    let error = chain.accept_headers(&heavy, &Permissive, NOW).unwrap_err();
    assert_eq!((error.index, error.accepted.added), (1, 1));
    assert!(matches!(
        error.reason,
        RejectReason::Rule(HeaderRuleError::WorkOverflow)
    ));
    assert_eq!(
        chain.best_work(),
        (U256::from(1u64) << 255) + U256::from(17u64)
    );
    let dir = scratch();
    let mut chain = open(dir.path(), regtest());
    let mut mainnet_solution = main[0].clone();
    mainnet_solution.solution = vec![0; PowParams::MAINNET.solution_len()];
    assert!(matches!(
        reject(&mut chain, &[mainnet_solution]),
        RejectReason::Rule(HeaderRuleError::SolutionLength {
            expected: 36,
            got: 1344
        })
    ));
    for bits in [0x2010_0000, 0x200f_0f10, 0x2080_0001, 0] {
        assert!(matches!(
            reject(&mut chain, &[header(genesis(), 1, bits, 9)]),
            RejectReason::Rule(HeaderRuleError::Pow(
                PowError::TargetAboveLimit(got) | PowError::InvalidBits(got)
            )) if got == bits
        ));
    }
    assert_eq!(chain.entries(), 1);

    // A child before its parent: the batch stops at the child.
    let error = chain
        .accept_headers(&[main[1].clone(), main[0].clone()], &Permissive, NOW)
        .unwrap_err();
    assert_eq!(error.index, 0);
    assert!(matches!(
        error.reason,
        RejectReason::Unconnected(parent) if parent == main[0].hash()
    ));
    assert_eq!(error.accepted.added, 0);

    // Known headers are counted, also when the batch has a header twice.
    accept(&mut chain, &main[..3]);
    let mut batch = main.clone();
    batch.push(main[4].clone());
    let accepted = chain.accept_headers(&batch, &Permissive, NOW).unwrap();
    assert_eq!((accepted.added, accepted.known), (2, 4));
    let change = accepted.tip_change.unwrap();
    assert_eq!(
        (change.old, change.new),
        (tip(3, &main[2]), tip(5, &main[4]))
    );
    assert_eq!(chain.entries(), 6);
}

#[test]
fn locator_and_headers_after() {
    let dir_a = scratch();
    let dir_b = scratch();
    let mut a = open(dir_a.path(), regtest());
    let mut b = open(dir_b.path(), regtest());
    let common = branch(genesis(), 3_777, EASY, 0);
    let only_a = branch(common[3_776].hash(), 1_223, EASY, 10_000);
    let only_b = branch(common[3_776].hash(), 500, EASY, 20_000);
    accept(&mut a, &common);
    accept(&mut a, &only_a);
    accept(&mut b, &common);
    accept(&mut b, &only_b);
    let chain_a: Vec<&BlockHeader> = common.iter().chain(&only_a).collect();
    let chain_b: Vec<&BlockHeader> = common.iter().chain(&only_b).collect();

    // The locator names the blocks of chain A at the locator heights.
    let locator = a.locator();
    let heights = locator_heights(5_000);
    assert_eq!(
        heights[..11],
        [5_000, 4_999, 4_998, 4_997, 4_996, 4_995, 4_994, 4_993, 4_992, 4_991, 4_989]
    );
    assert_eq!(locator.len(), heights.len());
    for (hash, height) in locator.iter().zip(&heights) {
        let expected = match height.checked_sub(1) {
            Some(at) => chain_a[at as usize].hash(),
            None => genesis(),
        };
        assert_eq!(*hash, expected);
    }

    // Brute force: the newest block of chain B that the locator names.
    let best_b: Vec<Tip> = b.best_chain_from(0).collect();
    let start = best_b
        .iter()
        .rev()
        .find(|tip| locator.contains(&tip.hash))
        .unwrap()
        .height as usize;
    assert!(start <= 3_777, "the block is on both chains");
    let gap = heights.windows(2).map(|w| w[0] - w[1]).find(|_| true);
    assert_eq!(gap, Some(1));
    let step = heights
        .windows(2)
        .find(|w| w[1] as usize <= 3_777)
        .map(|w| w[0] - w[1])
        .unwrap();
    assert!(3_777 - start < step as usize);

    let served = b.headers_after(&locator, &ZERO, 160).unwrap();
    assert_eq!(served.len(), 160);
    for (i, header) in served.iter().enumerate() {
        assert_eq!(header, chain_b[start + i]);
    }
    // Chain A accepts the answer: the headers connect to its entries.
    let accepted = a.accept_headers(&served, &Permissive, NOW).unwrap();
    assert_eq!(accepted.added + accepted.known, 160);

    // The stop hash ends the answer. The limit ends the answer.
    let stop = chain_b[start + 9].hash();
    assert_eq!(b.headers_after(&locator, &stop, 160).unwrap().len(), 10);
    assert_eq!(b.headers_after(&locator, &stop, 4).unwrap().len(), 4);
    // The answer ends at the best tip.
    let near_tip = [chain_b[4_270].hash()];
    let served = b.headers_after(&near_tip, &ZERO, 160).unwrap();
    assert_eq!(served.len(), 6);
    assert_eq!(served[5], *chain_b[4_276]);
    assert_eq!(
        b.headers_after(&[best_b[4_277].hash], &ZERO, 160).unwrap(),
        vec![]
    );
    // No known hash: the answer starts after the genesis block.
    let served = b.headers_after(&[BlockHash([7; 32])], &ZERO, 3).unwrap();
    assert_eq!(served, common[..3].to_vec());
    assert_eq!(
        b.headers_after(&[], &ZERO, 2).unwrap(),
        common[..2].to_vec()
    );
    // A hash on a side branch does not start the answer. The next hash does.
    let side = header(common[4_000 - 300].hash(), 5, EASY, 30_000);
    accept(&mut b, std::slice::from_ref(&side));
    let served = b
        .headers_after(&[side.hash(), common[99].hash()], &ZERO, 1)
        .unwrap();
    assert_eq!(served, vec![common[100].clone()]);
}

#[test]
fn branches_below_the_finalized_height_are_removed_and_refused() {
    let dir = scratch();
    let mut chain = open(dir.path(), regtest());
    let main = branch(genesis(), 1_500, EASY, 0);
    accept(&mut chain, &main[..1_000]);
    assert_eq!(chain.finalized_height(), 0);
    // Two side branches: from height 400 and from height 401.
    let low = branch(main[399].hash(), 3, EASY, 10_000);
    let high = branch(main[400].hash(), 3, EASY, 20_000);
    accept(&mut chain, &low);
    accept(&mut chain, &high);

    accept(&mut chain, &main[1_000..1_400]);
    assert_eq!(chain.finalized_height(), 400);
    assert_eq!(status(&chain, &low[2]), Some(Status::HeaderValid));

    // Height 1,401: the finalized height is 401, above the fork point of `low`.
    accept(&mut chain, &main[1_400..1_401]);
    assert_eq!(chain.finalized_height(), 401);
    for header in &low {
        assert_eq!(status(&chain, header), None);
    }
    assert_eq!(status(&chain, &high[2]), Some(Status::HeaderValid));
    // The removed entries stay in memory until the next start.
    assert_eq!(chain.entries(), 1 + 1_401 + 6);

    // A branch from below the finalized height is refused, also with more work.
    let deep = header(main[399].hash(), 7, HARDEST, 30_000);
    assert!(matches!(
        reject(&mut chain, std::slice::from_ref(&deep)),
        RejectReason::ForkBelowFinalized {
            fork_height: 400,
            finalized_height: 401
        }
    ));
    // A header on a removed entry does not connect.
    let on_removed = header(low[2].hash(), 7, HARDEST, 30_001);
    assert!(matches!(
        reject(&mut chain, std::slice::from_ref(&on_removed)),
        RejectReason::Unconnected(_)
    ));
    // The removed headers themselves are refused as a branch below the finalized height.
    assert!(matches!(
        reject(&mut chain, &low),
        RejectReason::ForkBelowFinalized { .. }
    ));
    // A branch from the finalized height is accepted and can become the best chain.
    let at_final = branch(main[400].hash(), 3, HARDEST, 40_000);
    let change = accept(&mut chain, &at_final).unwrap();
    assert_eq!(change.fork_point, tip(401, &main[400]));
    assert_eq!(change.new, tip(404, &at_final[2]));
    // The best tip is lower, so the finalized height is lower. The old chain stays.
    assert_eq!(chain.finalized_height(), 0);
    assert_eq!(status(&chain, &main[1_400]), Some(Status::HeaderValid));
    accept(&mut chain, &main[1_401..]);
    assert_eq!(chain.best_tip(), tip(404, &at_final[2]));
}

#[test]
fn checkpoints_fix_the_chain() {
    let dir = scratch();
    let main = branch(genesis(), 60, EASY, 0);
    let other = branch(genesis(), 60, EASY, 1_000);
    let mut config = regtest();
    config.checkpoints =
        Checkpoints::new(vec![(50, main[49].hash()), (20, main[19].hash())]).unwrap();
    let mut chain = open(dir.path(), config.clone());

    // Another chain stops at the first checkpoint height.
    let error = chain.accept_headers(&other, &Permissive, NOW).unwrap_err();
    assert_eq!(error.index, 19);
    assert!(matches!(
        error.reason,
        RejectReason::CheckpointMismatch { height: 20, expected } if expected == main[19].hash()
    ));
    assert_eq!(chain.best_tip(), tip(19, &other[18]));
    assert_eq!(chain.finalized_height(), 0);

    // The chain with the checkpoints. At height 20 the other branch is removed.
    accept(&mut chain, &main[..19]);
    assert_eq!(status(&chain, &other[18]), Some(Status::HeaderValid));
    accept(&mut chain, &main[19..40]);
    assert_eq!(chain.finalized_height(), 20);
    assert_eq!(status(&chain, &other[18]), None);
    assert!(matches!(
        reject(&mut chain, &[header(main[9].hash(), 1, HARDEST, 2_000)]),
        RejectReason::ForkBelowFinalized {
            fork_height: 10,
            finalized_height: 20
        }
    ));
    // A branch between the checkpoints must have the next checkpoint.
    let between = branch(main[29].hash(), 25, EASY, 3_000);
    let error = chain
        .accept_headers(&between, &Permissive, NOW)
        .unwrap_err();
    assert_eq!(error.index, 19);
    assert!(matches!(
        error.reason,
        RejectReason::CheckpointMismatch { height: 50, .. }
    ));
    assert_eq!(chain.best_tip(), tip(49, &between[18]));
    accept(&mut chain, &main[40..]);
    assert_eq!(chain.best_tip(), tip(60, &main[59]));
    assert_eq!(chain.finalized_height(), 50);
    assert_eq!(status(&chain, &between[0]), None);

    // The log holds headers that agree with the checkpoints. With another checkpoint
    // list, the start skips each record that the list refuses and the records of its
    // descendants: a release with a new checkpoint must start on the log of the release
    // before it.
    drop(chain);
    let (reopened, _) = HeaderChain::open(config.clone(), &dir.path().join("headers.log")).unwrap();
    assert_eq!(reopened.best_tip(), tip(60, &main[59]));
    assert_eq!(reopened.skipped_records(), 0);
    drop(reopened);
    // No header of the log has the checkpoint hash at height 10: each chain ends at 9.
    config.checkpoints = Checkpoints::new(vec![(10, BlockHash([9; 32]))]).unwrap();
    let (reopened, _) = HeaderChain::open(config.clone(), &dir.path().join("headers.log")).unwrap();
    assert_eq!(reopened.best_tip().height, 9);
    assert!(reopened.skipped_records() >= 51);
    assert_eq!(status(&reopened, &main[9]), None);
    drop(reopened);
    // A list with a new checkpoint on the best chain of the log, at a height where the
    // log also holds a header of another branch (`other[9]`): the start skips that record
    // and the best chain is the chain of the log.
    config.checkpoints = Checkpoints::new(vec![(10, main[9].hash())]).unwrap();
    let (reopened, _) = HeaderChain::open(config.clone(), &dir.path().join("headers.log")).unwrap();
    assert_eq!(reopened.best_tip(), tip(60, &main[59]));
    assert!(reopened.skipped_records() >= 1);
    assert_eq!(status(&reopened, &other[9]), None);
    drop(reopened);
    assert_eq!(
        Checkpoints::new(vec![(10, BlockHash([9; 32])), (10, main[9].hash())]),
        Err(DuplicateCheckpoint(10))
    );
    let mut config = regtest();
    config.checkpoints = Checkpoints::new(vec![(0, main[0].hash())]).unwrap();
    assert!(matches!(
        HeaderChain::open(config, &dir.path().join("other.log")),
        Err(OpenError::GenesisCheckpoint(_))
    ));
}

#[test]
fn download_order_and_body_states() {
    let dir = scratch();
    let mut chain = open(dir.path(), regtest());
    let main = branch(genesis(), 20, EASY, 0);
    accept(&mut chain, &main);
    let heights = |tips: Vec<Tip>| tips.iter().map(|t| t.height).collect::<Vec<_>>();
    assert_eq!(heights(chain.next_blocks_to_download(4)), [1, 2, 3, 4]);
    assert_eq!(chain.next_blocks_to_download(100).len(), 20);
    assert_eq!(chain.next_blocks_to_download(0), vec![]);

    chain.mark_body_received(&main[0].hash()).unwrap();
    chain.mark_body_received(&main[2].hash()).unwrap();
    assert_eq!(status(&chain, &main[2]), Some(Status::BodyKnown));
    assert_eq!(heights(chain.next_blocks_to_download(3)), [2, 4, 5]);
    // A valid body makes the ancestors valid.
    chain.mark_body_valid(&main[5].hash()).unwrap();
    for header in &main[..6] {
        assert_eq!(status(&chain, header), Some(Status::BodyValid));
    }
    chain.mark_body_received(&main[5].hash()).unwrap();
    assert_eq!(status(&chain, &main[5]), Some(Status::BodyValid));
    assert_eq!(heights(chain.next_blocks_to_download(2)), [7, 8]);
    // A dropped body is missing again. A valid body stays valid.
    chain.mark_body_received(&main[7].hash()).unwrap();
    assert_eq!(heights(chain.next_blocks_to_download(2)), [7, 9]);
    chain.mark_body_missing(&main[7].hash()).unwrap();
    assert_eq!(status(&chain, &main[7]), Some(Status::HeaderValid));
    chain.mark_body_missing(&main[5].hash()).unwrap();
    assert_eq!(status(&chain, &main[5]), Some(Status::BodyValid));
    assert_eq!(heights(chain.next_blocks_to_download(2)), [7, 8]);
    assert!(matches!(
        chain.mark_body_received(&BlockHash([1; 32])),
        Err(MarkError::Unknown(_))
    ));

    // A reorganization to a branch from height 3: the blocks of the branch are next.
    let fork = branch(main[2].hash(), 4, HARDEST, 100);
    let change = accept(&mut chain, &fork).unwrap();
    assert_eq!(change.fork_point, tip(3, &main[2]));
    assert_eq!(
        chain.next_blocks_to_download(10),
        [
            tip(4, &fork[0]),
            tip(5, &fork[1]),
            tip(6, &fork[2]),
            tip(7, &fork[3])
        ]
    );
    // An invalid body moves the tip back to the old branch. Its blocks 4 to 6 have bodies.
    let change = chain.mark_invalid(&fork[0].hash()).unwrap().unwrap();
    assert_eq!(change.new, tip(20, &main[19]));
    assert_eq!(change.fork_point, tip(3, &main[2]));
    assert_eq!(heights(chain.next_blocks_to_download(2)), [7, 8]);
}

#[test]
fn log_round_trip_torn_tail_and_damage() {
    let dir = scratch();
    let path = dir.path().join("headers.log");
    let mut config = regtest();
    config.finality_depth = 50;
    let main = branch(genesis(), 300, EASY, 0);
    let old_side = branch(main[99].hash(), 5, EASY, 1_000);
    let side = branch(main[279].hash(), 30, HARD, 2_000);
    let late = branch(main[289].hash(), 3, EASY, 3_000);

    let mut chain = open(dir.path(), config.clone());
    accept(&mut chain, &main[..120]);
    accept(&mut chain, &old_side);
    accept(&mut chain, &main[120..]);
    assert_eq!(status(&chain, &old_side[0]), None);
    // `side` becomes the best chain. Then its block 11 is invalid.
    accept(&mut chain, &side);
    assert_eq!(chain.best_tip(), tip(310, &side[29]));
    chain.mark_body_valid(&side[4].hash()).unwrap();
    chain.mark_invalid(&side[10].hash()).unwrap();
    assert_eq!(chain.best_tip(), tip(300, &main[299]));
    accept(&mut chain, &late);
    chain.sync().unwrap();

    let all: Vec<&BlockHeader> = main
        .iter()
        .chain(&old_side)
        .chain(&side)
        .chain(&late)
        .collect();
    let view = |chain: &HeaderChain| {
        let entries: Vec<_> = all.iter().map(|h| chain.entry(&h.hash())).collect();
        let best: Vec<Tip> = chain.best_chain_from(0).collect();
        let served = chain
            .headers_after(&[main[249].hash()], &ZERO, 160)
            .unwrap();
        (
            entries,
            best,
            served,
            chain.finalized_height(),
            chain.best_work(),
        )
    };
    // The start does not restore the body states.
    let mut expected = view(&chain);
    for entry in expected.0.iter_mut().flatten() {
        if let Status::BodyValid = entry.status {
            entry.status = Status::HeaderValid;
        }
    }
    assert_eq!(expected.2, main[250..].to_vec());
    let file_len = std::fs::metadata(&path).unwrap().len();
    // 338 header records and one invalid-block record.
    assert_eq!(file_len, 338 * RECORD_BYTES + 20 + 1 + 32);
    drop(chain);

    let (reopened, report) = HeaderChain::open(config.clone(), &path).unwrap();
    assert_eq!((report.records, report.torn_bytes), (339, 0));
    assert_eq!(view(&reopened), expected);
    // The start applies every header record, the records of removed entries included.
    assert_eq!(reopened.entries(), 339);
    drop(reopened);

    // A torn tail: the last record misses 5 bytes. The start cuts the record.
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(file_len - 5).unwrap();
    drop(file);
    let (mut reopened, report) = HeaderChain::open(config.clone(), &path).unwrap();
    assert_eq!((report.records, report.torn_bytes), (338, RECORD_BYTES - 5));
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        file_len - RECORD_BYTES
    );
    assert_eq!(status(&reopened, &late[2]), None);
    assert_eq!(status(&reopened, &late[1]), Some(Status::HeaderValid));
    // The chain appends after the cut.
    accept(&mut reopened, &late[2..]);
    assert_eq!(view(&reopened), expected);
    drop(reopened);

    // A tail of zero bytes (a file extension without data) is a torn tail.
    let mut bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len() as u64, file_len);
    let mut extended = bytes.clone();
    extended.extend_from_slice(&[0u8; 300]);
    std::fs::write(&path, &extended).unwrap();
    let (reopened, report) = HeaderChain::open(config.clone(), &path).unwrap();
    assert_eq!((report.records, report.torn_bytes), (339, 300));
    assert_eq!(view(&reopened), expected);
    drop(reopened);

    // A last record with a damaged payload is a torn tail.
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    let (_, report) = HeaderChain::open(config.clone(), &path).unwrap();
    assert_eq!((report.records, report.torn_bytes), (338, RECORD_BYTES));
    bytes[last] ^= 1;

    // Damage in a record that is not the last one is an error: in the payload, then in
    // the frame.
    let record = 100 * RECORD_BYTES as usize;
    for (at, reason) in [
        (record + 60, "record payload fails its checksum"),
        (record + 5, "record frame fails its checksum"),
    ] {
        let mut damaged = bytes.clone();
        damaged[at] ^= 0x40;
        std::fs::write(&path, &damaged).unwrap();
        let Err(OpenError::Store(StoreError::Corrupt {
            offset,
            reason: got,
        })) = HeaderChain::open(config.clone(), &path)
        else {
            panic!("the start must refuse a damaged record");
        };
        assert_eq!((offset, got), (record as u64, reason));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            damaged,
            "the file is not changed"
        );
    }

    // A log of another chain does not connect.
    std::fs::write(&path, &bytes[RECORD_BYTES as usize..]).unwrap();
    assert!(matches!(
        HeaderChain::open(config, &path),
        Err(OpenError::Replay {
            offset: 0,
            reason: RejectReason::Unconnected(_)
        })
    ));
}

/// The time to accept 100,000 Regtest headers in batches of 160, and the memory of an
/// entry. Run with `--nocapture` to read the values.
#[test]
fn accept_100_000_headers() {
    let dir = scratch();
    let mut chain = open(dir.path(), regtest());
    let headers = branch(genesis(), 100_000, EASY, 0);
    let start = Instant::now();
    for batch in headers.chunks(160) {
        chain.accept_headers(batch, &Permissive, NOW).unwrap();
    }
    let elapsed = start.elapsed();
    assert_eq!(chain.best_tip(), tip(100_000, &headers[99_999]));
    let entries = chain.entries();
    let memory = chain.memory_bytes();
    println!(
        "accept 100,000 Regtest headers: {elapsed:?} ({:.2} us per header); memory {memory} \
         bytes for {entries} entries ({:.1} bytes per entry, allocated capacity included)",
        elapsed.as_secs_f64() * 1e6 / 100_000.0,
        memory as f64 / entries as f64,
    );
    let start = Instant::now();
    drop(chain);
    let chain = open(dir.path(), regtest());
    println!(
        "start from the log of 100,000 headers: {:?}",
        start.elapsed()
    );
    assert_eq!(chain.best_tip(), tip(100_000, &headers[99_999]));
    // 96 bytes for the entry, 4 for the best chain, about 5 for the hash index, and the
    // spare capacity of the vectors (at most a factor of 2).
    assert!(
        memory / entries < 2 * 110,
        "{memory} bytes for {entries} entries"
    );
}

// ---- Property test against a reference model ------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Added,
    Known,
    KnownInvalid,
    Unconnected,
    InvalidParent,
    ForkBelowFinalized,
}

struct ModelEntry {
    hash: BlockHash,
    parent: usize,
    height: u32,
    work: u64,
    invalid: bool,
    /// The chain has the entry.
    alive: bool,
}

/// The reference model: every answer comes from a search over all entries.
struct Model {
    entries: Vec<ModelEntry>,
    depth: u32,
}

impl Model {
    fn new(depth: u32) -> Self {
        Self {
            entries: vec![ModelEntry {
                hash: genesis(),
                parent: 0,
                height: 0,
                work: 17,
                invalid: false,
                alive: true,
            }],
            depth,
        }
    }

    fn find(&self, hash: &BlockHash) -> Option<usize> {
        self.entries.iter().position(|e| e.alive && e.hash == *hash)
    }

    /// Most work, then first seen.
    fn best(&self) -> usize {
        let mut best = 0;
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.alive && !entry.invalid && entry.work > self.entries[best].work {
                best = i;
            }
        }
        best
    }

    /// The entries from the genesis block to `i`.
    fn chain_to(&self, mut i: usize) -> Vec<usize> {
        let mut chain = vec![i];
        while i != 0 {
            i = self.entries[i].parent;
            chain.push(i);
        }
        chain.reverse();
        chain
    }

    /// The newest entry that the chains to `a` and to `b` share.
    fn fork_point(&self, a: usize, b: usize) -> usize {
        let (a, b) = (self.chain_to(a), self.chain_to(b));
        let shared = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        a[shared - 1]
    }

    fn finalized_height(&self) -> u32 {
        self.entries[self.best()].height.saturating_sub(self.depth)
    }

    fn fork_height(&self, i: usize) -> u32 {
        self.entries[self.fork_point(i, self.best())].height
    }

    fn prune(&mut self) {
        let finalized = self.finalized_height();
        let best_chain = self.chain_to(self.best());
        for i in 0..self.entries.len() {
            if self.entries[i].alive && !best_chain.contains(&i) && self.fork_height(i) < finalized
            {
                self.entries[i].alive = false;
            }
        }
    }

    fn add(&mut self, header: &BlockHeader, work: u64) -> Outcome {
        let hash = header.hash();
        if let Some(i) = self.find(&hash) {
            return match self.entries[i].invalid {
                true => Outcome::KnownInvalid,
                false => Outcome::Known,
            };
        }
        let Some(parent) = self.find(&header.prev_hash) else {
            return Outcome::Unconnected;
        };
        if self.entries[parent].invalid {
            return Outcome::InvalidParent;
        }
        if self.fork_height(parent) < self.finalized_height() {
            return Outcome::ForkBelowFinalized;
        }
        self.entries.push(ModelEntry {
            hash,
            parent,
            height: self.entries[parent].height + 1,
            work: self.entries[parent].work + work,
            invalid: false,
            alive: true,
        });
        self.prune();
        Outcome::Added
    }

    fn invalidate(&mut self, i: usize) {
        for j in i..self.entries.len() {
            if self.chain_to(j).contains(&i) {
                self.entries[j].invalid = true;
            }
        }
        self.prune();
    }

    fn tip(&self, i: usize) -> Tip {
        Tip {
            height: self.entries[i].height,
            hash: self.entries[i].hash,
        }
    }

    fn change(&self, old: usize) -> Option<BestTipChange> {
        let new = self.best();
        (new != old).then(|| BestTipChange {
            old: self.tip(old),
            new: self.tip(new),
            fork_point: self.tip(self.fork_point(old, new)),
        })
    }
}

/// Compares every observable value of the chain with the model.
fn assert_same(chain: &HeaderChain, model: &Model, generated: &[BlockHeader]) {
    let best = model.best();
    assert_eq!(chain.best_tip(), model.tip(best));
    assert_eq!(chain.best_work(), U256::from(model.entries[best].work));
    assert_eq!(chain.finalized_height(), model.finalized_height());
    let best_chain = model.chain_to(best);
    let expected: Vec<Tip> = best_chain.iter().map(|i| model.tip(*i)).collect();
    assert_eq!(chain.best_chain_from(0).collect::<Vec<_>>(), expected);
    for header in generated {
        let hash = header.hash();
        let entry = chain.entry(&hash);
        let Some(i) = model.find(&hash) else {
            assert_eq!(entry, None, "{hash} is not in the model");
            continue;
        };
        let entry = entry.unwrap_or_else(|| panic!("{hash} is in the model"));
        let expected = &model.entries[i];
        assert_eq!(entry.height, expected.height);
        assert_eq!(entry.work, U256::from(expected.work));
        assert_eq!(entry.status == Status::Invalid, expected.invalid);
        assert_eq!(entry.on_best_chain, best_chain.contains(&i));
    }
}

fn outcome_of(reason: &RejectReason) -> Outcome {
    match reason {
        RejectReason::KnownInvalid => Outcome::KnownInvalid,
        RejectReason::Unconnected(_) => Outcome::Unconnected,
        RejectReason::InvalidParent(_) => Outcome::InvalidParent,
        RejectReason::ForkBelowFinalized { .. } => Outcome::ForkBelowFinalized,
        other => panic!("unexpected reason: {other}"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 300,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// Random fork shapes, duplicate and unconnected headers, invalid blocks and a small
    /// finality depth. After each step the chain equals the model. At the end the chain
    /// that starts from the log equals the model too.
    #[test]
    fn chain_equals_the_reference_model(
        depth in 1u32..8,
        ops in proptest::collection::vec((0u8..12, any::<u16>(), 0usize..3), 1..150),
    ) {
        let dir = scratch();
        let mut config = regtest();
        config.finality_depth = depth;
        let mut chain = open(dir.path(), config.clone());
        let mut model = Model::new(depth);
        // Every header that the test made, the refused ones included.
        let mut generated: Vec<BlockHeader> = Vec::new();
        let works = [(EASY, 17u64), (HARD, 32), (HARDEST, 8_192)];

        for (step, (kind, pick, work)) in ops.into_iter().enumerate() {
            let pick = pick as usize;
            let old = model.best();
            let bits = works[work].0;
            let parent_of_new = match kind {
                // On the newest header.
                0..=4 => Some(generated.last().map_or(genesis(), BlockHeader::hash)),
                // On the best tip.
                5 => Some(chain.best_tip().hash),
                // On a header of any age, or on the genesis block.
                6..=8 => Some(match pick % (generated.len() + 1) {
                    0 => genesis(),
                    at => generated[at - 1].hash(),
                }),
                // On a header that does not exist.
                9 => Some(BlockHash([pick as u8; 32])),
                _ => None,
            };
            if let Some(parent) = parent_of_new {
                generated.push(header(parent, step as u32, bits, step as u64));
            }
            match kind {
                // A header again, and an invalid block.
                10 | 11 if generated.is_empty() => continue,
                11 => {
                    let hash = generated[pick % generated.len()].hash();
                    let result = chain.mark_invalid(&hash);
                    match model.find(&hash) {
                        None => prop_assert!(matches!(result, Err(MarkError::Unknown(_)))),
                        Some(i) => {
                            model.invalidate(i);
                            prop_assert_eq!(result.unwrap(), model.change(old));
                        }
                    }
                }
                _ => {
                    let header = match kind {
                        10 => generated[pick % generated.len()].clone(),
                        _ => generated.last().unwrap().clone(),
                    };
                    let work = works.iter().find(|(bits, _)| *bits == header.bits).unwrap().1;
                    let expected = model.add(&header, work);
                    let (got, change) = match chain.accept_headers(&[header], &Permissive, NOW) {
                        Ok(accepted) if accepted.added == 1 => (Outcome::Added, accepted.tip_change),
                        Ok(accepted) => (Outcome::Known, accepted.tip_change),
                        Err(error) => (outcome_of(&error.reason), error.accepted.tip_change),
                    };
                    prop_assert_eq!(got, expected);
                    prop_assert_eq!(change, model.change(old));
                }
            }
            assert_same(&chain, &model, &generated);
        }

        drop(chain);
        let chain = open(dir.path(), config);
        assert_same(&chain, &model, &generated);
    }
}

/// The chain holds a bounded number of side headers. At the bound the side header without
/// a child and with the least work leaves, and a new side header with no more work than
/// that one is refused before it is in the log.
#[test]
fn side_headers_have_a_bound() {
    let dir = scratch();
    let mut config = regtest();
    config.max_side_headers = 3;
    let mut chain = open(dir.path(), config);
    let main = branch(genesis(), 10, HARD, 0);
    accept(&mut chain, &main);
    // Three side headers on the blocks 5, 6 and 7: the work grows with the height.
    let sides: Vec<BlockHeader> = (5..8)
        .map(|at| header(main[at - 1].hash(), 1, EASY, 1_000 + at as u64))
        .collect();
    for side in &sides {
        accept(&mut chain, std::slice::from_ref(side));
    }
    let log = dir.path().join("headers.log");
    let bytes = std::fs::metadata(&log).unwrap().len();
    // A side header with the work of the weakest one is refused and not written.
    let weak = header(main[4].hash(), 2, EASY, 2_000);
    assert!(matches!(
        reject(&mut chain, std::slice::from_ref(&weak)),
        RejectReason::SideHeaderLimit
    ));
    assert_eq!(std::fs::metadata(&log).unwrap().len(), bytes);
    // A side header with more work takes the place of the weakest one.
    let strong = header(main[8].hash(), 2, EASY, 3_000);
    accept(&mut chain, std::slice::from_ref(&strong));
    assert_eq!(status(&chain, &sides[0]), None);
    assert_eq!(status(&chain, &sides[1]), Some(Status::HeaderValid));
    assert_eq!(status(&chain, &strong), Some(Status::HeaderValid));
    // A header that makes its branch the best chain is never a side header.
    let fork = branch(main[8].hash(), 2, HARDEST, 4_000);
    let change = accept(&mut chain, &fork).unwrap();
    assert_eq!(change.new, tip(11, &fork[1]));
    // A start gives the same entries.
    drop(chain);
    let mut config = regtest();
    config.max_side_headers = 3;
    let reopened = open(dir.path(), config);
    assert_eq!(reopened.best_tip(), tip(11, &fork[1]));
    assert_eq!(status(&reopened, &sides[0]), None);
    assert_eq!(status(&reopened, &strong), Some(Status::HeaderValid));
}

/// A block that no peer sends leaves the choice of the best tip with its descendants. The
/// best tip is then the tip of the chain with the most work among the other chains. The
/// headers stay valid, and the end of the mark gives the first best tip again.
#[test]
fn an_unavailable_block_moves_the_best_tip_until_the_mark_ends() {
    let dir = scratch();
    let mut chain = open(dir.path(), regtest());
    let common = branch(genesis(), 5, EASY, 0);
    accept(&mut chain, &common);
    let short = branch(common[4].hash(), 3, EASY, 1_000);
    let long = branch(common[4].hash(), 6, EASY, 2_000);
    accept(&mut chain, &short);
    accept(&mut chain, &long);
    assert_eq!(chain.best_tip(), tip(11, &long[5]));
    assert_eq!(
        chain.best_chain_ancestor(&short[2].hash()),
        Some(tip(5, &common[4]))
    );

    let change = chain.mark_unavailable(&long[1].hash()).unwrap().unwrap();
    assert_eq!(change.new, tip(8, &short[2]));
    assert_eq!(change.fork_point, tip(5, &common[4]));
    let entry = chain.entry(&long[5].hash()).unwrap();
    assert_eq!(
        (entry.status, entry.unavailable),
        (Status::HeaderValid, true)
    );
    assert!(!chain.entry(&long[0].hash()).unwrap().unavailable);
    assert_eq!(
        chain.best_chain_ancestor(&long[5].hash()),
        Some(tip(5, &common[4]))
    );
    // A header on an unavailable block is accepted and is unavailable too.
    let more = branch(long[5].hash(), 2, EASY, 3_000);
    let accepted = chain.accept_headers(&more, &Permissive, NOW).unwrap();
    assert_eq!((accepted.added, accepted.tip_change), (2, None));
    assert!(chain.entry(&more[1].hash()).unwrap().unavailable);
    // The other chain grows and stays the best chain while it has less work.
    let next = branch(short[2].hash(), 1, EASY, 4_000);
    accept(&mut chain, &next);
    assert_eq!(chain.best_tip(), tip(9, &next[0]));

    assert!(chain.clear_unavailable());
    assert_eq!(chain.best_tip(), tip(13, &more[1]));
    assert!(!chain.entry(&long[5].hash()).unwrap().unavailable);
    assert!(!chain.clear_unavailable());
    // A valid body cannot become unavailable.
    chain.mark_body_valid(&common[4].hash()).unwrap();
    assert!(matches!(
        chain.mark_unavailable(&common[4].hash()),
        Err(MarkError::ValidBody(_))
    ));
}

/// The first sync of Testnet, 2026-10-05: the node excluded a block of the best chain,
/// and more blocks left the best chain than the bound of the side headers. The next header
/// on the excluded chain made the chain remove the tip of that chain. The chain accepted
/// the removed headers again later and wrote a second record for each of them, and the
/// next start refused the log. The bound does not count the excluded headers, each header
/// has one record, and a start gives the same chain.
#[test]
fn an_excluded_chain_longer_than_the_side_bound_keeps_its_headers_and_one_record_each() {
    let dir = scratch();
    let mut config = regtest();
    config.max_side_headers = 3;
    let mut chain = open(dir.path(), config.clone());
    let main = branch(genesis(), 10, EASY, 0);
    accept(&mut chain, &main);
    // The blocks 5 to 10 leave the best chain: 6 headers, and the bound is 3.
    let change = chain.mark_unavailable(&main[4].hash()).unwrap().unwrap();
    assert_eq!(change.new, tip(4, &main[3]));
    // The header sync continues on the excluded chain.
    let more = branch(main[9].hash(), 2, EASY, 1_000);
    let accepted = chain.accept_headers(&more, &Permissive, NOW).unwrap();
    assert_eq!((accepted.added, accepted.tip_change), (2, None));
    for header in main[4..].iter().chain(&more) {
        assert_eq!(status(&chain, header), Some(Status::HeaderValid));
    }
    // A peer sends the headers again, before and after the end of the exclusion.
    let log = dir.path().join("headers.log");
    let bytes = std::fs::metadata(&log).unwrap().len();
    let again: Vec<BlockHeader> = main[4..].iter().chain(&more).cloned().collect();
    let accepted = chain.accept_headers(&again, &Permissive, NOW).unwrap();
    assert_eq!((accepted.added, accepted.known), (0, 8));
    assert!(chain.clear_unavailable());
    assert_eq!(chain.best_tip(), tip(12, &more[1]));
    let accepted = chain.accept_headers(&again, &Permissive, NOW).unwrap();
    assert_eq!((accepted.added, accepted.known), (0, 8));
    assert_eq!(std::fs::metadata(&log).unwrap().len(), bytes);

    drop(chain);
    let (reopened, report) = HeaderChain::open(config, &log).unwrap();
    assert_eq!(report.records, 12);
    assert_eq!(reopened.duplicate_records(), 0);
    assert_eq!(reopened.best_tip(), tip(12, &more[1]));
}

/// A side header that left the chain at the bound has its header record in the log. When
/// the chain accepts the header again, the log gets a mark of 32 bytes and no second
/// header record, and a start gives the entries of the run.
#[test]
fn a_removed_header_that_comes_back_gets_a_mark_and_no_second_header_record() {
    // The frame, the kind and the hash.
    const MARK_BYTES: u64 = 20 + 1 + 32;
    let dir = scratch();
    let mut config = regtest();
    config.max_side_headers = 2;
    let mut chain = open(dir.path(), config.clone());
    let main = branch(genesis(), 10, HARD, 0);
    accept(&mut chain, &main);
    // A side branch of 2 headers, then a side header with more work: the tip of the
    // branch leaves.
    let weak = branch(main[4].hash(), 2, EASY, 1_000);
    accept(&mut chain, &weak);
    let strong = header(main[8].hash(), 2, EASY, 2_000);
    accept(&mut chain, std::slice::from_ref(&strong));
    assert_eq!(status(&chain, &weak[1]), None);
    let log = dir.path().join("headers.log");
    assert_eq!(std::fs::metadata(&log).unwrap().len(), 13 * RECORD_BYTES);
    // The removed header has more work than the weakest side header, so the chain
    // accepts it again. A header on it makes its branch the best chain.
    let accepted = chain.accept_headers(&weak[1..], &Permissive, NOW).unwrap();
    assert_eq!(accepted.added, 1);
    assert_eq!(
        std::fs::metadata(&log).unwrap().len(),
        13 * RECORD_BYTES + MARK_BYTES
    );
    assert_eq!(status(&chain, &weak[1]), None);
    let accepted = chain.accept_headers(&weak[1..], &Permissive, NOW).unwrap();
    assert_eq!(accepted.added, 1);

    // A start reads the two marks and removes the header as the run did.
    drop(chain);
    let (reopened, report) = HeaderChain::open(config.clone(), &log).unwrap();
    assert_eq!(report.records, 15);
    assert_eq!(reopened.duplicate_records(), 0);
    assert_eq!(reopened.skipped_records(), 0);
    assert_eq!(reopened.best_tip(), tip(10, &main[9]));
    assert_eq!(status(&reopened, &weak[1]), None);
    assert_eq!(status(&reopened, &weak[0]), Some(Status::HeaderValid));
    assert_eq!(status(&reopened, &strong), Some(Status::HeaderValid));
    drop(reopened);

    // A mark of a header that the log does not have stops the start.
    let (mut raw, _) = HeaderLog::open(&log, |_, _| Ok::<(), StoreError>(())).unwrap();
    raw.append_again(&BlockHash([7; 32])).unwrap();
    drop(raw);
    assert!(matches!(
        HeaderChain::open(config, &log),
        Err(OpenError::ReplayMark {
            reason: MarkError::Unknown(_),
            ..
        })
    ));
}

/// A log of an earlier version has a second record of a header. The start uses the first
/// record, counts the second one, and gives the chain of the log without it. A record
/// that fails its checksum in the middle of such a log stops the start.
#[test]
fn a_second_record_of_a_header_is_counted_and_changes_nothing() {
    let dir = scratch();
    let path = dir.path().join("headers.log");
    let main = branch(genesis(), 6, EASY, 0);
    let mut chain = open(dir.path(), regtest());
    accept(&mut chain, &main[..4]);
    drop(chain);
    // The writer of the earlier version: the headers 3 and 4 again, then new headers.
    let (mut log, _) = HeaderLog::open(&path, |_, _| Ok::<(), StoreError>(())).unwrap();
    for header in main[2..].iter() {
        log.append_header(header).unwrap();
    }
    drop(log);

    let (reopened, report) = HeaderChain::open(regtest(), &path).unwrap();
    assert_eq!(report.records, 8);
    assert_eq!(reopened.duplicate_records(), 2);
    assert_eq!(reopened.skipped_records(), 0);
    assert_eq!(reopened.best_tip(), tip(6, &main[5]));
    assert_eq!(
        reopened.headers_after(&[genesis()], &ZERO, 10).unwrap(),
        main
    );
    drop(reopened);

    // One changed byte in the first of the two second records.
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[(4 * RECORD_BYTES + RECORD_BYTES / 2) as usize] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(matches!(
        HeaderChain::open(regtest(), &path),
        Err(OpenError::Store(StoreError::Corrupt { .. }))
    ));
}
