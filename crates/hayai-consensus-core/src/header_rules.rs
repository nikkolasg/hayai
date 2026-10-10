//! The header rules that read no hash and no solution (protocol specification §7.6 and
//! §7.7.2): the version, the target against the proof-of-work limit, the time rules
//! against the median-time-past, and `nBits` against the expected value.
//!
//! References: zcashd `CheckBlockHeader` and `ContextualCheckBlockHeader`; Zakura
//! `zakura-header-chain/src/validation` (`context_free`, `contextual/validate.rs`).
//!
//! [`check_contextual`] applies these rules to a header that is not the genesis block.
//! [`check_local_time`] is the rule against the clock of the node. It is not a consensus
//! rule: its result changes with time. The adapter adds the proof of work: the solution
//! length, the hash against the target and the Equihash solution.
//!
//! The contextual rules read the blocks before the header ([`ParentChain`]). When the
//! context holds fewer blocks than a rule reads, that rule does not run and the result is
//! [`HeaderVerdict::ContextTooShort`]. The caller must handle that result: it is never a
//! pass.
//!
//! Regtest follows Zakura's Regtest (`disable_pow`): a header needs a target at or below
//! the limit and has no expected `nBits`. The time rules apply.

use crate::difficulty_rules::{
    expected_bits, median_time_past, needed, ContextTooShort, DifficultyError, Uint256,
};
use crate::rule_sets::RuleSet;
use crate::{ConsensusError, CoreSpec, ParentChain, MEDIAN_TIME_SPAN};

/// Lowest block version (zcashd `MIN_BLOCK_VERSION`).
/// Spec §7.6: the block version is at least 4.
pub const MIN_BLOCK_VERSION: u32 = 4;
/// A block's time is at most this number of seconds after its median-time-past (zcashd
/// `MAX_FUTURE_BLOCK_TIME_MTP`).
/// Spec §7.6: `nTime` is at most the median-time-past plus 90 · 60 s.
pub const MAX_FUTURE_BLOCK_TIME_MTP: u32 = 90 * 60;
/// A node accepts a block whose time is at most this number of seconds after its clock
/// (zcashd `MAX_FUTURE_BLOCK_TIME_LOCAL`).
/// Spec §7.6: a full validator refuses `nTime` more than 2 h after its clock.
pub const MAX_FUTURE_BLOCK_TIME_LOCAL: u32 = 2 * 60 * 60;

/// The fields of a block header that the rules of this module read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderFields {
    /// `nVersion`.
    pub version: u32,
    /// `nTime`.
    pub time: u32,
    /// `nBits`.
    pub bits: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    #[error("the genesis block has no parent: the header rules do not apply to it")]
    Genesis,
    #[error("block version {0:#010x} is below {MIN_BLOCK_VERSION} or has the high bit set")]
    Version(u32),
    #[error("bits {0:#010x} encode no target")]
    InvalidBits(u32),
    #[error("the target of bits {0:#010x} is above the proof-of-work limit")]
    TargetAboveLimit(u32),
    #[error("difficulty bits {got:#010x}, the chain requires {expected:#010x}")]
    WrongBits { expected: u32, got: u32 },
    #[error("time {time} is not after the median-time-past {median_time_past}")]
    TimeTooEarly { time: u32, median_time_past: u32 },
    #[error("time {time} is more than 90 min after the median-time-past: the limit is {limit}")]
    TimeTooLate { time: u32, limit: u32 },
    #[error("time {time} is more than 2 h after the clock of the node: the limit is {limit}")]
    TimeTooFarAhead { time: u32, limit: u32 },
    #[error(transparent)]
    Rules(#[from] ConsensusError),
    #[error("nBits {0:#010x} of a block of the context encode no target")]
    InvalidContextBits(u32),
}

/// The rules of [`check_contextual`] that did not run because the context is too short.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unchecked {
    /// The time rules did not run: the context holds fewer times than the
    /// median-time-past reads.
    pub time: bool,
    /// `nBits` was not compared with the expected value.
    pub bits: bool,
    /// What the context holds and what the rules that did not run read.
    pub context: ContextTooShort,
}

/// The result of the contextual header rules when no rule failed.
#[must_use = "a short context means that a rule did not run: the caller must handle it"]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderVerdict {
    /// Every rule ran and passed.
    Checked,
    /// The context holds fewer blocks than a rule reads. The rules of [`Unchecked`] did
    /// not run. Every other rule ran and passed.
    ContextTooShort(Unchecked),
}

/// The rules that read no hash and no solution: the version, the target against the
/// proof-of-work limit, the time against the median-time-past, and `nBits` against the
/// expected value of [`expected_bits`]. `rules` is the rule set of `chain.height`
/// ([`crate::rule_sets::rules_at`]): the caller selects it one time for every rule of the
/// block.
pub fn check_contextual(
    spec: &CoreSpec,
    rules: &RuleSet,
    header: HeaderFields,
    chain: ParentChain<'_>,
) -> Result<HeaderVerdict, HeaderError> {
    let height = chain.height;
    if height == 0 {
        return Err(HeaderError::Genesis);
    }
    check_version(header.version)?;
    // Spec §7.7.2: the target of `nBits` is at most `PoWLimit`.
    check_target(header.bits, &spec.pow_limit)?;

    // Spec §7.6: the median-time-past reads the 11 blocks before the header, or all of
    // them when fewer exist.
    let needed_times = needed(height, MEDIAN_TIME_SPAN);
    let median = if chain.times.len() >= needed_times {
        median_time_past(chain.times)
    } else {
        None
    };
    let time_unchecked = match median {
        Some(median_time_past) => {
            // Spec §7.6: `nTime` is strictly greater than the median-time-past.
            if header.time <= median_time_past {
                return Err(HeaderError::TimeTooEarly {
                    time: header.time,
                    median_time_past,
                });
            }
            // Spec §7.6: `nTime` is at most the median-time-past plus 90 min, from height 2
            // on Mainnet and from height 653,606 on Testnet.
            let limit = median_time_past.saturating_add(MAX_FUTURE_BLOCK_TIME_MTP);
            if height >= spec.max_time_start_height && header.time > limit {
                return Err(HeaderError::TimeTooLate {
                    time: header.time,
                    limit,
                });
            }
            false
        }
        None => true,
    };
    let mut unchecked = Unchecked {
        time: time_unchecked,
        bits: false,
        context: ContextTooShort {
            times: chain.times.len(),
            needed_times,
            bits: chain.bits.len(),
            needed_bits: 0,
        },
    };

    if !spec.disable_pow {
        // Spec §7.6: `nBits` equals `ThresholdBits(height)`.
        match expected_bits(spec, rules, header.time, chain) {
            Ok(expected) if expected == header.bits => {}
            Ok(expected) => {
                return Err(HeaderError::WrongBits {
                    expected,
                    got: header.bits,
                })
            }
            Err(DifficultyError::ContextTooShort(short)) => {
                unchecked.bits = true;
                unchecked.context.needed_times = short.needed_times.max(needed_times);
                unchecked.context.needed_bits = short.needed_bits;
            }
            Err(DifficultyError::Genesis) => return Err(HeaderError::Genesis),
            Err(DifficultyError::Rules(e)) => return Err(HeaderError::Rules(e)),
            Err(DifficultyError::InvalidContextBits(bits)) => {
                return Err(HeaderError::InvalidContextBits(bits))
            }
        }
    }
    if unchecked.time || unchecked.bits {
        return Ok(HeaderVerdict::ContextTooShort(unchecked));
    }
    Ok(HeaderVerdict::Checked)
}

/// The version rule: the version is at least [`MIN_BLOCK_VERSION`] as a signed 32-bit
/// integer. A version with the high bit set is negative for zcashd (`int32_t nVersion`,
/// `CheckBlockHeader`: `version-too-low`). Zakura rejects it by name
/// (`zakura-chain/src/block/serialize.rs:36-62`, `validate_header_version`).
///
/// Spec §7.6: the block version is at least 4, and a version above 4 has the rules of
/// version 4.
pub fn check_version(version: u32) -> Result<(), HeaderError> {
    if version >> 31 != 0 || version < MIN_BLOCK_VERSION {
        return Err(HeaderError::Version(version));
    }
    Ok(())
}

/// The target that `bits` encodes, when it is at most `pow_limit` (the proof-of-work limit
/// as a 256-bit little-endian integer).
///
/// Spec §7.7.2: the target of `nBits` is at most `PoWLimit`.
pub fn check_target(bits: u32, pow_limit: &[u8; 32]) -> Result<Uint256, HeaderError> {
    let Some(target) = Uint256::from_compact(bits) else {
        return Err(HeaderError::InvalidBits(bits));
    };
    if target > Uint256::from_le_bytes(pow_limit) {
        return Err(HeaderError::TargetAboveLimit(bits));
    }
    Ok(target)
}

/// The local rule: the time `time` of a header is at most 2 h after `now`, the clock of
/// the node in seconds. It is not a consensus rule. A header that fails can pass later.
///
/// Spec §7.6: a full validator refuses a block with `nTime` more than 2 h after its clock.
pub fn check_local_time(time: u32, now: u32) -> Result<(), HeaderError> {
    let limit = now.saturating_add(MAX_FUTURE_BLOCK_TIME_LOCAL);
    if time > limit {
        return Err(HeaderError::TimeTooFarAhead { time, limit });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain_spec::tests::regtest;
    use crate::rule_sets::rules_at;

    /// `check_contextual` with the rule set of `chain.height`.
    fn contextual(
        spec: &CoreSpec,
        header: HeaderFields,
        chain: ParentChain<'_>,
    ) -> Result<HeaderVerdict, HeaderError> {
        let rules = rules_at(spec, chain.height)?;
        check_contextual(spec, &rules, header, chain)
    }

    #[test]
    fn the_version_rule() {
        assert_eq!(check_version(4), Ok(()));
        assert_eq!(check_version(5), Ok(()));
        assert_eq!(check_version(3), Err(HeaderError::Version(3)));
        assert_eq!(
            check_version(0x8000_0004),
            Err(HeaderError::Version(0x8000_0004))
        );
    }

    #[test]
    fn the_target_limit() {
        let limit = Uint256::from_compact(0x1f07_ffff).unwrap().to_le_bytes();
        assert_eq!(
            check_target(0x1f07_ffff, &limit),
            Ok(Uint256::from_le_bytes(&limit))
        );
        let Ok(_) = check_target(0x1f07_fffe, &limit) else {
            panic!("a target below the limit");
        };
        assert_eq!(
            check_target(0x1f08_0000, &limit),
            Err(HeaderError::TargetAboveLimit(0x1f08_0000))
        );
        assert_eq!(
            check_target(0x1f80_0001, &limit),
            Err(HeaderError::InvalidBits(0x1f80_0001))
        );
    }

    #[test]
    fn the_local_time_rule() {
        assert_eq!(check_local_time(100, 100), Ok(()));
        assert_eq!(check_local_time(100 + 7_200, 100), Ok(()));
        assert_eq!(
            check_local_time(100 + 7_201, 100),
            Err(HeaderError::TimeTooFarAhead {
                time: 7_301,
                limit: 7_300
            })
        );
        assert_eq!(check_local_time(u32::MAX, u32::MAX), Ok(()));
    }

    /// Regtest waives the expected `nBits`: the version, the target limit and the time
    /// rules remain.
    #[test]
    fn regtest_applies_the_time_rules_and_the_target_limit() {
        let spec = regtest();
        let times = [1_000, 990, 980, 970, 960, 950, 940, 930, 920, 910, 900];
        let bits = [0x200f_0f0f; 11];
        let chain = ParentChain {
            height: 11,
            times: &times,
            bits: &bits,
        };
        let header = |time, bits| HeaderFields {
            version: 4,
            time,
            bits,
        };
        assert_eq!(
            contextual(&spec, header(951, 0x200f_0f0f), chain),
            Ok(HeaderVerdict::Checked)
        );
        assert_eq!(
            contextual(&spec, header(950, 0x200f_0f0f), chain),
            Err(HeaderError::TimeTooEarly {
                time: 950,
                median_time_past: 950
            })
        );
        assert_eq!(
            contextual(&spec, header(950 + 5_401, 0x200f_0f0f), chain),
            Err(HeaderError::TimeTooLate {
                time: 6_351,
                limit: 6_350
            })
        );
        assert_eq!(
            contextual(&spec, header(951, 0x2010_0000), chain),
            Err(HeaderError::TargetAboveLimit(0x2010_0000))
        );
        assert_eq!(
            contextual(&spec, header(951, 0), chain),
            Err(HeaderError::InvalidBits(0))
        );
        assert_eq!(
            contextual(
                &spec,
                HeaderFields {
                    version: 3,
                    ..header(951, 0x200f_0f0f)
                },
                chain
            ),
            Err(HeaderError::Version(3))
        );
        let genesis = ParentChain { height: 0, ..chain };
        assert_eq!(
            contextual(&spec, header(951, 0x200f_0f0f), genesis),
            Err(HeaderError::Genesis)
        );
        // A context of 10 times at height 11: the time rules did not run.
        let short = ParentChain {
            times: &times[..10],
            ..chain
        };
        assert_eq!(
            contextual(&spec, header(1, 0x200f_0f0f), short),
            Ok(HeaderVerdict::ContextTooShort(Unchecked {
                time: true,
                bits: false,
                context: ContextTooShort {
                    times: 10,
                    needed_times: 11,
                    bits: 11,
                    needed_bits: 0,
                },
            }))
        );
    }

    /// With the proof of work on, `nBits` is checked against the expected value, or it is
    /// reported as unchecked when the context is short.
    #[test]
    fn bits_are_checked_or_reported_as_unchecked() {
        let mut spec = regtest();
        spec.disable_pow = false;
        // The Mainnet limit, whose compact form is exact: 2^243 - 1.
        spec.pow_limit = [0; 32];
        for i in 0..30 {
            spec.pow_limit[i] = 0xff;
        }
        spec.pow_limit[30] = 0x07;
        spec.pow_limit_bits = 0x1f07_ffff;
        // Height 30: above the window of 17 plus the median span. The 28 blocks before it
        // have the limit and a spacing of 75 s. The compact limit decodes to its top 3
        // bytes, so the floor of the mean over the window timespan is one unit below it.
        let mut times = [0u32; 28];
        let mut bits = [0u32; 17];
        for i in 0..28 {
            times[i] = 100_000 - 75 * u32::try_from(i).unwrap();
        }
        for i in 0..17 {
            bits[i] = 0x1f07_ffff;
        }
        let chain = ParentChain {
            height: 30,
            times: &times,
            bits: &bits,
        };
        let header = |bits| HeaderFields {
            version: 4,
            time: 100_075,
            bits,
        };
        assert_eq!(
            contextual(&spec, header(0x1f07_fffe), chain),
            Ok(HeaderVerdict::Checked)
        );
        assert_eq!(
            contextual(&spec, header(0x1f07_ffff), chain),
            Err(HeaderError::WrongBits {
                expected: 0x1f07_fffe,
                got: 0x1f07_ffff
            })
        );
        let short = ParentChain {
            bits: &bits[..16],
            ..chain
        };
        assert_eq!(
            contextual(&spec, header(0x1f07_ffff), short),
            Ok(HeaderVerdict::ContextTooShort(Unchecked {
                time: false,
                bits: true,
                context: ContextTooShort {
                    times: 28,
                    needed_times: 28,
                    bits: 16,
                    needed_bits: 17,
                },
            }))
        );
        let mut broken = bits;
        broken[3] = 0x1f80_0001;
        let invalid = ParentChain {
            bits: &broken,
            ..chain
        };
        assert_eq!(
            contextual(&spec, header(0x1f07_ffff), invalid),
            Err(HeaderError::InvalidContextBits(0x1f80_0001))
        );
    }
}
