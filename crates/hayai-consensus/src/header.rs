//! The header rules, in one place for every path: the relay header check, the header
//! index of the node, header synchronization, block validation and the replay at a
//! restart.
//!
//! References: protocol specification §7.6 (block header rules) and §7.7; zcashd
//! `CheckBlockHeader` and `ContextualCheckBlockHeader`; Zakura
//! `zakura-header-chain/src/validation` (`context_free`, `contextual/validate.rs`).
//!
//! [`check_header`] applies every rule of a header that is not the genesis block. It has
//! three parts, which a caller can also run on their own:
//!
//! - [`check_contextual`]: the version, the target limit, the time rules against the
//!   median-time-past, and `nBits` against the expected value.
//! - [`check_local_time`]: the rule against the clock of the node. It is not a consensus
//!   rule: its result changes with time. It runs only when the caller gives a clock. A
//!   caller that validates a stored block again (a replay) must not give one.
//! - [`check_proof_of_work`]: the solution length, the hash against the target, and the
//!   Equihash solution.
//!
//! The contextual rules read the blocks before the header ([`ParentChain`]). When the
//! context holds fewer blocks than a rule reads, that rule does not run and the result is
//! [`HeaderVerdict::ContextTooShort`]. The caller must handle that result: it is never a
//! pass.
//!
//! Regtest follows Zakura's Regtest (`disable_pow`): a header needs a solution of the
//! right length and a target at or below the limit. It has no hash filter, no Equihash
//! verification and no expected `nBits`. The time rules apply.

use hayai_wire::header::{check_equihash, check_pow, check_target, BlockHeader, PowError};

use crate::difficulty::{expected_bits, median_time_past, ContextTooShort, DifficultyError};
use crate::{ConsensusError, Network, ParentChain, MEDIAN_TIME_SPAN};

/// Lowest block version (zcashd `MIN_BLOCK_VERSION`).
pub const MIN_BLOCK_VERSION: u32 = 4;
/// A block's time is at most this number of seconds after its median-time-past (zcashd
/// `MAX_FUTURE_BLOCK_TIME_MTP`).
pub const MAX_FUTURE_BLOCK_TIME_MTP: u32 = 90 * 60;
/// A node accepts a block whose time is at most this number of seconds after its clock
/// (zcashd `MAX_FUTURE_BLOCK_TIME_LOCAL`).
pub const MAX_FUTURE_BLOCK_TIME_LOCAL: u32 = 2 * 60 * 60;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HeaderRuleError {
    #[error("the genesis block has no parent: the header rules do not apply to it")]
    Genesis,
    #[error("block version {0:#010x} is below {MIN_BLOCK_VERSION} or has the high bit set")]
    Version(u32),
    #[error("solution of {got} bytes, the network's Equihash parameters need {expected}")]
    SolutionLength { expected: usize, got: usize },
    #[error(transparent)]
    Pow(#[from] PowError),
    #[error("difficulty bits {got:#010x}, the chain requires {expected:#010x}")]
    WrongBits { expected: u32, got: u32 },
    #[error("time {time} is not after the median-time-past {median_time_past}")]
    TimeTooEarly { time: u32, median_time_past: u32 },
    #[error("time {time} is more than 90 min after the median-time-past: the limit is {limit}")]
    TimeTooLate { time: u32, limit: u32 },
    #[error("time {time} is more than 2 h after the clock of the node: the limit is {limit}")]
    TimeTooFarAhead { time: u32, limit: u32 },
    /// The `equihash::Error` message. The upstream type has no comparable detail.
    #[error("equihash: {0}")]
    Equihash(String),
    #[error(transparent)]
    Rules(#[from] ConsensusError),
    #[error("nBits {0:#010x} of a block of the context encode no target")]
    InvalidContextBits(u32),
    /// The work of the chain with this header is 2^256 or more. A network with a hash
    /// filter cannot have such a chain. Regtest has no hash filter, so a header can state
    /// each target at or below the limit.
    #[error("the cumulative work of the chain with this header overflows 256 bits")]
    WorkOverflow,
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

/// Every rule of `header` at `chain.height` on `network`: the consensus rules, and the
/// local time rule when `now` (the clock of the node, in seconds) is given. The cheap
/// rules run first: a header costs a hash and an Equihash verification only after its
/// other rules pass.
pub fn check_header(
    network: Network,
    header: &BlockHeader,
    chain: &ParentChain<'_>,
    now: Option<u32>,
) -> Result<HeaderVerdict, HeaderRuleError> {
    let verdict = check_contextual(network, header, chain)?;
    if let Some(now) = now {
        check_local_time(header, now)?;
    }
    check_proof_of_work(network, header)?;
    Ok(verdict)
}

/// The rules that read no hash and no solution: the version, the target against the
/// proof-of-work limit, the time against the median-time-past, and `nBits` against the
/// expected value of [`expected_bits`].
pub fn check_contextual(
    network: Network,
    header: &BlockHeader,
    chain: &ParentChain<'_>,
) -> Result<HeaderVerdict, HeaderRuleError> {
    let height = chain.height;
    if height == 0 {
        return Err(HeaderRuleError::Genesis);
    }
    let net = network.params();
    check_version(header)?;
    check_target(header.bits, &net.pow_limit)?;

    let needed_times = MEDIAN_TIME_SPAN.min(usize::try_from(height).unwrap_or(usize::MAX));
    let median = median_time_past(chain.times).filter(|_| chain.times.len() >= needed_times);
    let time_unchecked = match median {
        Some(median_time_past) => {
            if header.time <= median_time_past {
                return Err(HeaderRuleError::TimeTooEarly {
                    time: header.time,
                    median_time_past,
                });
            }
            let limit = median_time_past.saturating_add(MAX_FUTURE_BLOCK_TIME_MTP);
            if height >= net.max_time_start_height && header.time > limit {
                return Err(HeaderRuleError::TimeTooLate {
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

    if !net.disable_pow {
        match expected_bits(network, header.time, chain) {
            Ok(expected) if expected == header.bits => {}
            Ok(expected) => {
                return Err(HeaderRuleError::WrongBits {
                    expected,
                    got: header.bits,
                })
            }
            Err(DifficultyError::ContextTooShort(short)) => {
                unchecked.bits = true;
                unchecked.context.needed_times = short.needed_times.max(needed_times);
                unchecked.context.needed_bits = short.needed_bits;
            }
            Err(DifficultyError::Genesis) => return Err(HeaderRuleError::Genesis),
            Err(DifficultyError::Rules(e)) => return Err(HeaderRuleError::Rules(e)),
            Err(DifficultyError::InvalidContextBits(bits)) => {
                return Err(HeaderRuleError::InvalidContextBits(bits))
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
pub fn check_version(header: &BlockHeader) -> Result<(), HeaderRuleError> {
    if header.version >> 31 != 0 || header.version < MIN_BLOCK_VERSION {
        return Err(HeaderRuleError::Version(header.version));
    }
    Ok(())
}

/// The solution has the length of the network's Equihash parameters. A header of another
/// network fails here before any hash work.
pub fn check_solution_length(
    network: Network,
    header: &BlockHeader,
) -> Result<(), HeaderRuleError> {
    let expected = network.params().pow.solution_len();
    match header.solution.len() {
        got if got == expected => Ok(()),
        got => Err(HeaderRuleError::SolutionLength { expected, got }),
    }
}

/// The proof of work of `header`: the solution length, the target at or below the limit,
/// the hash at or below the target, and the Equihash solution with the network's
/// parameters. A network with `disable_pow` (Regtest) checks the solution length and the
/// target limit only.
pub fn check_proof_of_work(network: Network, header: &BlockHeader) -> Result<(), HeaderRuleError> {
    let net = network.params();
    check_solution_length(network, header)?;
    if net.disable_pow {
        check_target(header.bits, &net.pow_limit)?;
        return Ok(());
    }
    check_pow(header, &net.pow_limit)?;
    check_equihash(header, net.pow).map_err(|e| HeaderRuleError::Equihash(e.to_string()))
}

/// The local rule: the time of `header` is at most 2 h after `now`, the clock of the node
/// in seconds. It is not a consensus rule. A header that fails can pass later.
pub fn check_local_time(header: &BlockHeader, now: u32) -> Result<(), HeaderRuleError> {
    let limit = now.saturating_add(MAX_FUTURE_BLOCK_TIME_LOCAL);
    if header.time > limit {
        return Err(HeaderRuleError::TimeTooFarAhead {
            time: header.time,
            limit,
        });
    }
    Ok(())
}
