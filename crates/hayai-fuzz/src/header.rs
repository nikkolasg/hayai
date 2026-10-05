//! The header classes: the header rules of hayai-consensus against the header code of
//! Zakura, on generated contexts and on changed headers of published block vectors.
//!
//! The block cases run without header rules, because their headers have no proof of
//! work. These classes check a header alone.
//!
//! - `header-context`: the version, the target limit, the time rules and the expected
//!   `nBits` of a header on a generated chain of times and `nBits`
//!   (`hayai_consensus::header::check_contextual` against zakura-header-chain).
//! - `pow`: the hash filter and the Equihash solution of a changed Mainnet header
//!   (`hayai_consensus::header::check_proof_of_work` against the copied
//!   `difficulty_is_valid` and `equihash_solution_is_valid`).

use std::collections::BTreeMap;
use std::io::Cursor;
use std::time::{Duration, Instant};

use chrono::DateTime;
use hayai_consensus::header::{check_contextual, check_proof_of_work, HeaderVerdict};
use hayai_consensus::{ParentChain, Upgrade};
use hayai_wire::header::BlockHeader;
use rayon::prelude::*;
use zakura_chain::block::{self, Height};
use zakura_chain::serialization::ZcashDeserialize;
use zakura_chain::work::difficulty::CompactDifficulty;
use zakura_header_chain::{
    validate_compact_target, validate_contextual_difficulty_and_time,
    validate_encoding_version_hash, AdjustedDifficulty,
};

use crate::context::Net;
use crate::model::Header;
use crate::reference::zakura_consensus::block_check;
use crate::rng::{case_seed, Rng};
use crate::run::{ClassStats, Finding, Outcome, Plan, Report};
use crate::verdict::{compare, RuleClass, Verdict};
use crate::{hayai_side, reference};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderClass {
    Context,
    Pow,
}

impl HeaderClass {
    pub const ALL: [HeaderClass; 2] = [HeaderClass::Context, HeaderClass::Pow];

    pub fn name(self) -> &'static str {
        match self {
            HeaderClass::Context => "header-context",
            HeaderClass::Pow => "pow",
        }
    }

    pub fn from_name(name: &str) -> Option<HeaderClass> {
        HeaderClass::ALL
            .into_iter()
            .find(|class| class.name() == name)
    }
}

/// A header and the chain before it.
#[derive(Clone, Debug)]
pub struct HeaderCase {
    pub class: HeaderClass,
    pub network: Net,
    /// The height of the header.
    pub height: u32,
    pub header: Header,
    /// `nTime` of the blocks before the header, newest first.
    pub times: Vec<u32>,
    /// `nBits` of the blocks before the header, newest first.
    pub bits: Vec<u32>,
}

/// The header of the Mainnet block 1 and of the Mainnet block 1,687,106, from the block
/// vectors of hayai-wire (the published Zebra test vectors).
fn pow_seeds() -> [(u32, Header); 2] {
    let header = |hex_block: &str| {
        let bytes = hex::decode(hex_block.trim()).expect("the vector is hex");
        Header::parse(&bytes).0
    };
    [
        (
            1,
            header(include_str!(
                "../../hayai-wire/tests/vectors/block-main-0-000-001.hex"
            )),
        ),
        (
            1_687_106,
            header(include_str!(
                "../../hayai-wire/tests/vectors/block-main-1-687-106.hex"
            )),
        ),
    ]
}

fn pow_case(rng: &mut Rng) -> HeaderCase {
    let seeds = pow_seeds();
    let (height, mut header) = seeds[rng.below(2)].clone();
    let flip = |bytes: &mut [u8], rng: &mut Rng| {
        let at = rng.below(bytes.len());
        bytes[at] ^= 1 << rng.below(8);
    };
    match rng.below(10) {
        // The header of the vector: valid.
        0 => {}
        1 => flip(&mut header.nonce, rng),
        2 | 3 => flip(&mut header.solution, rng),
        4 => header.time ^= 1 << rng.below(32),
        5 => header.bits ^= 1 << rng.below(32),
        6 => header.version ^= 1 << rng.below(32),
        7 => flip(&mut header.merkle, rng),
        8 => {
            let len = *rng.pick(&[0usize, 36, 1343, 1345, 400]);
            header.solution.resize(len, 0);
        }
        // A target above the limit of the network, or a value that encodes no target.
        _ => {
            header.bits = *rng.pick(&[
                0x2007_ffff,
                0x1f07_ffff,
                0x1f08_0000,
                0x0080_0000,
                0x0180_0000,
                0xff00_0001,
                0,
            ])
        }
    }
    HeaderCase {
        class: HeaderClass::Pow,
        network: Net::Mainnet,
        height,
        header,
        times: Vec::new(),
        bits: Vec::new(),
    }
}

/// A valid compact target at or below the limit of the networks.
fn context_bits(rng: &mut Rng) -> u32 {
    match rng.below(8) {
        0 => 0x1f07_ffff,
        1 => 0x2007_ffff,
        2 => 0x1c01_0000,
        _ => {
            let exponent = 0x1a + rng.below(5) as u32;
            let mantissa = 0x00_8000 + rng.below(0x7f_0000) as u32;
            (exponent << 24) | mantissa
        }
    }
}

fn context_case(rng: &mut Rng) -> HeaderCase {
    let network = if rng.chance(1, 2) {
        Net::Mainnet
    } else {
        Net::Testnet
    };
    let hayai_network = hayai_side::network(network);
    // Heights near the changes of the header rules, low heights, and any height.
    let mut anchors: Vec<u32> = [
        Upgrade::Blossom,
        Upgrade::Canopy,
        Upgrade::Nu5,
        Upgrade::Nu6_3,
    ]
    .into_iter()
    .filter_map(|upgrade| hayai_network.activation_height(upgrade))
    .collect();
    anchors.extend([1, 2, 11, 17, 18, 27, 28, 29, 299_188, 653_606]);
    let end = match hayai_network.activation_height(Upgrade::Nu7) {
        Some(nu7) => nu7 - 1,
        None => 4_600_000,
    };
    let height = if rng.chance(2, 3) {
        let anchor = *rng.pick(&anchors);
        (i64::from(anchor) + [0i64, 0, 1, -1, 5, 17, 28, -5][rng.below(8)]).max(1) as u32
    } else {
        1 + rng.below(end as usize) as u32
    }
    .min(end);

    // 28 blocks before the header, or all of them at a low height.
    let span = 28.min(height as usize);
    let spacing = *rng.pick(&[75u32, 150, 75, 30, 600, 1]);
    let mut time: u32 = 1_600_000_000 + rng.below(100_000_000) as u32;
    let base_bits = context_bits(rng);
    let mut times = Vec::with_capacity(span);
    let mut bits = Vec::with_capacity(span);
    for _ in 0..span {
        time = match rng.below(12) {
            // A long gap: the minimum difficulty rule of Testnet reads the gap to the
            // parent.
            0 => time + spacing * (5 + rng.below(4) as u32),
            // A time before the time of the block before.
            1 => time.saturating_sub(rng.below(200) as u32),
            _ => time + 1 + rng.below(2 * spacing as usize) as u32,
        };
        times.push(time);
        bits.push(if rng.chance(1, 6) {
            context_bits(rng)
        } else {
            base_bits
        });
    }
    // Newest first.
    times.reverse();
    bits.reverse();

    let mut sorted: Vec<u32> = times.iter().take(11).copied().collect();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];
    let parent_time = times[0];
    let header_time = match rng.below(10) {
        0..=3 => rng.near_u32(&[
            median,
            median + 5_400,
            parent_time + 6 * 75,
            parent_time + 6 * 150,
        ]),
        4 => parent_time + 6 * 75 + rng.below(3) as u32,
        _ => parent_time + 1 + rng.below(2 * spacing as usize) as u32,
    };
    let mut case = HeaderCase {
        class: HeaderClass::Context,
        network,
        height,
        header: Header {
            version: if rng.chance(1, 12) {
                rng.near_u32(&[0, 3, 4, 5, 0x7fff_ffff, 0x8000_0000, 0x8000_0004, u32::MAX])
            } else {
                4
            },
            prev: [0; 32],
            merkle: [0; 32],
            commitments: [0; 32],
            time: header_time,
            bits: 0,
            nonce: [0; 32],
            solution: vec![0; 1344],
        },
        times,
        bits,
    };
    // The `nBits` that the reference expects, so that a valid header is a common case,
    // or a value near it, or another value.
    let expected = reference_expected_bits(&case);
    case.header.bits = match (rng.below(8), expected) {
        (0, Some(expected)) => expected.wrapping_add(1),
        (1, Some(expected)) => expected.wrapping_sub(1),
        (2, _) => context_bits(rng),
        (3, _) => *rng.pick(&[0, 0x0080_0000, 0x2100_ffff, 0x2007_ffff, 0x1f07_ffff]),
        (_, Some(expected)) => expected,
        (_, None) => base_bits,
    };
    case
}

/// The case with the case seed `seed`.
pub fn case(class: HeaderClass, seed: u64) -> HeaderCase {
    let mut rng = Rng::new(seed);
    match class {
        HeaderClass::Context => context_case(&mut rng),
        HeaderClass::Pow => pow_case(&mut rng),
    }
}

fn header_bytes(header: &Header) -> Vec<u8> {
    let mut bytes = Vec::new();
    header.write(&mut bytes);
    bytes
}

fn reference_context(case: &HeaderCase) -> Option<Vec<(CompactDifficulty, DateTime<chrono::Utc>)>> {
    case.bits
        .iter()
        .zip(&case.times)
        .map(|(bits, time)| {
            let bits = CompactDifficulty::from_bytes_in_display_order(&bits.to_be_bytes()).ok()?;
            Some((bits, DateTime::from_timestamp(i64::from(*time), 0)?))
        })
        .collect()
}

fn reference_adjustment(case: &HeaderCase) -> Option<AdjustedDifficulty> {
    AdjustedDifficulty::new_from_header_time(
        DateTime::from_timestamp(i64::from(case.header.time), 0)?,
        Height(case.height - 1),
        &reference::network(case.network),
        reference_context(case)?,
    )
    .ok()
}

/// The `nBits` that the reference expects for the header of `case`.
fn reference_expected_bits(case: &HeaderCase) -> Option<u32> {
    let adjustment = reference_adjustment(case)?;
    let compact = adjustment.expected_difficulty_threshold();
    let bytes = compact.bytes_in_display_order();
    Some(u32::from_be_bytes(bytes))
}

fn reference_verdict(case: &HeaderCase) -> Verdict {
    let network = reference::network(case.network);
    let bytes = header_bytes(&case.header);
    let header = match block::Header::zcash_deserialize(&mut Cursor::new(&bytes)) {
        Ok(header) => header,
        Err(e) => return Verdict::reject(RuleClass::Parse, e),
    };
    // `validate_encoding_version_hash`, then the rules of the class.
    let hash = match validate_encoding_version_hash(&header) {
        Ok(hash) => hash,
        Err(e) => return Verdict::reject(RuleClass::Header, e),
    };
    match case.class {
        HeaderClass::Pow => {
            let height = Height(case.height);
            if let Err(e) = block_check::difficulty_is_valid(&header, &network, &height, &hash) {
                return Verdict::reject(RuleClass::Header, e);
            }
            match block_check::equihash_solution_is_valid(&header, &network) {
                Ok(()) => Verdict::Accept,
                Err(e) => Verdict::reject(RuleClass::Header, e),
            }
        }
        HeaderClass::Context => {
            if let Err(e) = validate_compact_target(&header, &network) {
                return Verdict::reject(RuleClass::Header, e);
            }
            let Some(adjustment) = reference_adjustment(case) else {
                return Verdict::NotCovered("the reference refuses the context".into());
            };
            match validate_contextual_difficulty_and_time(header.difficulty_threshold, adjustment) {
                Ok(()) => Verdict::Accept,
                Err(e) => Verdict::reject(RuleClass::Header, e),
            }
        }
    }
}

fn hayai_verdict(case: &HeaderCase) -> Verdict {
    let network = hayai_side::network(case.network);
    let bytes = header_bytes(&case.header);
    let header = match BlockHeader::parse(&bytes) {
        Ok(header) => header,
        Err(e) => return Verdict::reject(RuleClass::Parse, e),
    };
    match case.class {
        HeaderClass::Pow => match check_proof_of_work(network, &header) {
            Ok(()) => Verdict::Accept,
            Err(e) => Verdict::reject(RuleClass::Header, e),
        },
        HeaderClass::Context => {
            let chain = ParentChain {
                height: case.height,
                times: &case.times,
                bits: &case.bits,
            };
            match check_contextual(network, &header, &chain) {
                Ok(HeaderVerdict::Checked) => Verdict::Accept,
                Ok(HeaderVerdict::ContextTooShort(unchecked)) => Verdict::reject(
                    RuleClass::Other,
                    format!("context too short: {unchecked:?}"),
                ),
                Err(e) => Verdict::reject(RuleClass::Header, e),
            }
        }
    }
}

/// The two verdicts of a header case. A panic of an implementation is a verdict.
pub fn check(case: &HeaderCase) -> Outcome {
    let guarded = |f: &dyn Fn(&HeaderCase) -> Verdict| match crate::guarded(|| f(case)) {
        Ok(verdict) => verdict,
        Err(panic) => Verdict::Panic(panic),
    };
    let hayai = guarded(&hayai_verdict);
    let reference = guarded(&reference_verdict);
    let comparison = compare(&hayai, &reference);
    Outcome {
        hayai,
        reference,
        comparison,
        known: None,
    }
}

const BATCH: u64 = 4_096;

/// Runs the header classes of `classes` with the limits of `plan` and adds the counts to
/// `report`. A finding of a header class has no recipe: its case file holds the class and
/// the case seed, and the text of the case.
pub fn run(plan: &Plan, classes: &[HeaderClass], report: &mut Report) {
    for &class in classes {
        let started = Instant::now();
        let deadline = plan.seconds.map(|s| started + Duration::from_secs(s));
        let mut stats = ClassStats::default();
        let mut kept: BTreeMap<String, usize> = BTreeMap::new();
        let mut next = 0u64;
        loop {
            let end = match plan.iterations {
                Some(limit) => (next + BATCH).min(limit),
                None => next + BATCH,
            };
            if next >= end || matches!(deadline, Some(deadline) if Instant::now() >= deadline) {
                break;
            }
            let results: Vec<(u64, HeaderCase, Outcome)> = (next..end)
                .into_par_iter()
                .map(|index| {
                    let seed = case_seed(plan.seed, class.name(), index);
                    let case = case(class, seed);
                    let outcome = check(&case);
                    (seed, case, outcome)
                })
                .collect();
            next = end;
            for (seed, case, outcome) in results {
                stats.record(&outcome);
                if !outcome.comparison.is_finding() {
                    continue;
                }
                let count = kept.entry(format!("{:?}", outcome.comparison)).or_default();
                if *count >= plan.per_signature {
                    continue;
                }
                *count += 1;
                let text = format!("{case:?}");
                let finding = Finding {
                    class: class.name().to_string(),
                    case_seed: seed,
                    recipe: None,
                    header_case: Some(text),
                    outcome,
                    block_sha256: String::new(),
                    block_bytes: 0,
                };
                if let Some(dir) = &plan.out_dir {
                    crate::run::write_finding(dir, &finding);
                }
                report.findings.push(finding);
            }
        }
        stats.seconds = started.elapsed().as_secs_f64();
        report.classes.insert(class.name().to_string(), stats);
    }
}
