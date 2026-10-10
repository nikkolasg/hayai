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
//!   median-time-past, and `nBits` against the expected value
//!   (`hayai_consensus_core::header_rules::check_contextual`).
//! - [`check_local_time`]: the rule against the clock of the node. It is not a consensus
//!   rule: its result changes with time. It runs only when the caller gives a clock. A
//!   caller that validates a stored block again (a replay) must not give one.
//! - [`check_proof_of_work`]: the solution length, the hash against the target, and the
//!   Equihash solution. These rules read the hash and the solution, so they are in the
//!   adapter and not in the core.
//!
//! The contextual rules read the blocks before the header ([`ParentChain`]). When the
//! context holds fewer blocks than a rule reads, that rule does not run and the result is
//! [`HeaderVerdict::ContextTooShort`]. The caller must handle that result: it is never a
//! pass.
//!
//! Regtest follows Zakura's Regtest (`disable_pow`): a header needs a solution of the
//! right length and a target at or below the limit. It has no hash filter, no Equihash
//! verification and no expected `nBits`. The time rules apply.

use hayai_consensus_core::header_rules as core;
pub use hayai_consensus_core::header_rules::{
    HeaderFields, HeaderVerdict, Unchecked, MAX_FUTURE_BLOCK_TIME_LOCAL, MAX_FUTURE_BLOCK_TIME_MTP,
    MIN_BLOCK_VERSION,
};
use hayai_wire::header::{check_equihash, check_pow, check_target, BlockHeader, PowError};

use crate::rules::core_rules_at;
use crate::{ConsensusError, Network, ParentChain};

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

impl From<core::HeaderError> for HeaderRuleError {
    fn from(error: core::HeaderError) -> Self {
        match error {
            core::HeaderError::Genesis => HeaderRuleError::Genesis,
            core::HeaderError::Version(version) => HeaderRuleError::Version(version),
            core::HeaderError::InvalidBits(bits) => {
                HeaderRuleError::Pow(PowError::InvalidBits(bits))
            }
            core::HeaderError::TargetAboveLimit(bits) => {
                HeaderRuleError::Pow(PowError::TargetAboveLimit(bits))
            }
            core::HeaderError::WrongBits { expected, got } => {
                HeaderRuleError::WrongBits { expected, got }
            }
            core::HeaderError::TimeTooEarly {
                time,
                median_time_past,
            } => HeaderRuleError::TimeTooEarly {
                time,
                median_time_past,
            },
            core::HeaderError::TimeTooLate { time, limit } => {
                HeaderRuleError::TimeTooLate { time, limit }
            }
            core::HeaderError::TimeTooFarAhead { time, limit } => {
                HeaderRuleError::TimeTooFarAhead { time, limit }
            }
            core::HeaderError::Rules(error) => HeaderRuleError::Rules(error),
            core::HeaderError::InvalidContextBits(bits) => {
                HeaderRuleError::InvalidContextBits(bits)
            }
        }
    }
}

/// The fields of `header` that the core reads.
fn fields(header: &BlockHeader) -> HeaderFields {
    HeaderFields {
        version: header.version,
        time: header.time,
        bits: header.bits,
    }
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
/// expected value of `expected_bits`. An upgrade without a rule set in this build is
/// [`HeaderRuleError::Rules`], before the other rules run.
pub fn check_contextual(
    network: Network,
    header: &BlockHeader,
    chain: &ParentChain<'_>,
) -> Result<HeaderVerdict, HeaderRuleError> {
    let spec = network.core();
    // The core has a rule set for every upgrade. This build has one for the upgrades whose
    // branch id its crypto backend knows: the expected `nBits` of another upgrade is not a
    // rule of this build. Regtest has no expected `nBits`, so every upgrade is valid there.
    let rules = match network.params().disable_pow {
        true => hayai_consensus_core::rule_sets::rules_at(spec, chain.height)?,
        false => core_rules_at(network, chain.height)?,
    };
    Ok(core::check_contextual(
        spec,
        &rules,
        fields(header),
        *chain,
    )?)
}

/// The version rule: the version is at least [`MIN_BLOCK_VERSION`] as a signed 32-bit
/// integer (`hayai_consensus_core::header_rules::check_version`).
pub fn check_version(header: &BlockHeader) -> Result<(), HeaderRuleError> {
    Ok(core::check_version(header.version)?)
}

/// The solution has the length of the network's Equihash parameters. A header of another
/// network fails here before any hash work.
///
/// Spec §7.6: the solution of a Mainnet or Testnet header has 1344 bytes.
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
    // Spec §7.6: the block passes the difficulty filter of §7.7.2.
    check_pow(header, &net.pow_limit)?;
    // Spec §7.6: `solution` is a valid Equihash solution (§7.7.1).
    check_equihash(header, net.pow).map_err(|e| HeaderRuleError::Equihash(e.to_string()))
}

/// The local rule: the time of `header` is at most 2 h after `now`, the clock of the node
/// in seconds (`hayai_consensus_core::header_rules::check_local_time`). It is not a consensus
/// rule. A header that fails can pass later.
pub fn check_local_time(header: &BlockHeader, now: u32) -> Result<(), HeaderRuleError> {
    Ok(core::check_local_time(header.time, now)?)
}
