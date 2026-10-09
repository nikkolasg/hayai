//! The consensus rules of Zcash as pure functions, in the Rust subset that Charon and
//! Aeneas translate to Lean.
//!
//! Contract: `docs/architecture.md`, section hayai-consensus-core.
//!
//! - Context in, result out. Each function takes the data that the caller fetched
//!   ([`CoreSpec`], heights, times, amounts, scripts) and returns a value or an error. The
//!   crate has no IO, no clock, no threads, no locks and no global state.
//! - No crypto, no parsing, no upstream types. Branch ids are `u32`, script flags are bit
//!   flags, scripts are bytes, hashes are inputs.
//! - Index loops, no iterator adapter chains, no `HashMap`, no closures that capture `&mut`.
//! - Checked arithmetic on each amount and each height, with the `MAX_MONEY` bound on
//!   amounts. A failure is an error variant, never a panic.
//! - One module for each section of the protocol specification or ZIP: [`spec`] (§5.3,
//!   ZIP 200), [`rules`] (the rule set of each upgrade), [`limits`], [`subsidy`] (§7.8),
//!   [`funding`] (§7.10, ZIP 207, ZIP 214, ZIP 1015), [`lockbox`] (ZIP 2001, ZIP 271),
//!   [`founders`] (§7.9), [`nsm`] (ZIP 235, ZIP 237), [`difficulty`] (§7.7), [`header`]
//!   (§7.6) and [`coinbase_value`] (§7.1.2, §7.10, ZIP 236).
//!
//! hayai-consensus is the adapter around this crate: the networks, the address decoding,
//! the checkpoints and the upstream types.
//!
//! The test `tests/subset.rs` greps the sources for the tokens outside the subset.

#![forbid(unsafe_code)]
// The subset of Aeneas has index loops, `len() == 0` and pattern matching on `Option`;
// these lints ask for iterators, `copy_from_slice`, `is_empty()` and `is_some()`.
#![allow(
    clippy::needless_range_loop,
    clippy::manual_memcpy,
    clippy::len_zero,
    clippy::manual_range_contains,
    clippy::redundant_pattern_matching
)]

pub mod block_limits;
pub mod chain_spec;
pub mod coinbase_value;
pub mod difficulty_rules;
pub mod founders;
pub mod funding;
pub mod header_rules;
pub mod lockbox;
pub mod nsm;
pub mod rule_sets;
pub mod subsidy_schedule;

pub use block_limits::BlockLimits;
pub use chain_spec::{CoreSpec, SpecError, Upgrade, UPGRADES};
pub use difficulty_rules::{ContextTooShort, ParentChain};

/// `MAX_MONEY`: 21,000,000 ZEC in zatoshis (protocol specification §5.3). No amount of
/// this crate is above it.
pub const MAX_MONEY: u64 = 2_100_000_000_000_000;
/// Blocks a coinbase output must age before a transaction can spend it.
/// Spec §7.1.2: no spend of a coinbase output less than 100 blocks old.
/// ZIP 218: the value stays 100 blocks from NU7.
pub const COINBASE_MATURITY: u32 = 100;
/// Expiry heights at or above this value are not valid (zcashd
/// `TX_EXPIRY_HEIGHT_THRESHOLD`).
pub const TX_EXPIRY_HEIGHT_THRESHOLD: u32 = 500_000_000;
/// Lock times below this value are block heights. Lock times at or above it are Unix times.
pub const LOCKTIME_THRESHOLD: u32 = 500_000_000;
/// Target block spacing before Blossom, in seconds.
/// ZIP 208: `PreBlossomPoWTargetSpacing` is 150 s.
pub const PRE_BLOSSOM_TARGET_SPACING: u32 = 150;
/// Target block spacing from Blossom until NU7, in seconds.
/// ZIP 208: `PostBlossomPoWTargetSpacing` is 75 s.
pub const POST_BLOSSOM_TARGET_SPACING: u32 = 75;
/// Target block spacing from NU7, in seconds.
/// ZIP 218: `PostNU7PoWTargetSpacing` is 25 s.
pub const POST_NU7_TARGET_SPACING: u32 = 25;
/// Blocks whose times form the median-time-past.
/// Spec §7.6: `PoWMedianBlockSpan` is 11 blocks.
pub const MEDIAN_TIME_SPAN: usize = 11;
/// `PostNU7PoWAveragingWindow` (ZIP 218): the largest averaging window of the rule sets.
/// The test `rule_sets::tests::the_largest_window_is_the_nu7_window` compares it with the rule
/// sets.
pub const LARGEST_AVERAGING_WINDOW: usize = 102;
/// Newest blocks whose time and `bits` the difficulty rule of the next block reads: the
/// largest averaging window of the rule sets plus [`MEDIAN_TIME_SPAN`].
pub const DIFFICULTY_CONTEXT_BLOCKS: usize = LARGEST_AVERAGING_WINDOW + MEDIAN_TIME_SPAN;

/// A `scriptPubKey` that pays a P2SH address in the prescribed way:
/// `OP_HASH160 <20-byte script hash> OP_EQUAL` (spec §7.10). Each recipient of a funding
/// stream, each lockbox disbursement and each founders' address of this crate is one.
pub type P2shScript = [u8; 23];

/// Why a rule gives no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConsensusError {
    /// The upgrade is active at the height and this build has no rule set for it. Only the
    /// adapter builds this error: every upgrade has a rule set in this crate.
    #[error("{upgrade:?} is active at height {height} and this build has no rule set for it")]
    UnsupportedUpgrade { upgrade: Upgrade, height: u32 },
    /// The upgrade of a branch id has no rule set in this build. Only the adapter builds
    /// this error (`RuleSet::of_branch`): every upgrade has a rule set in this crate.
    #[error("this build has no rule set for {0:?}")]
    NoRuleSet(Upgrade),
    #[error("consensus branch id {0:#010x} belongs to no network upgrade")]
    UnknownBranch(u32),
    /// NSM reissuance is active at the height: the block subsidy depends on the issued
    /// supply after the parent block, and the caller gave none.
    #[error("the block subsidy at height {height} needs the issued supply after the parent block")]
    IssuedSupplyUnknown { height: u32 },
    /// The chain value pools hold more than the subsidy schedule issued: the NSM value
    /// balance is negative.
    #[error("the NSM value balance at height {height} is negative: the schedule issued {scheduled} zatoshis and the chain value pools hold {issued}")]
    NegativeNsmBalance {
        height: u32,
        scheduled: u128,
        issued: u64,
    },
    /// The network has no lockbox disbursement for its NU6.1 activation block. Zakura
    /// refuses each block at that height (`zakura-consensus/src/block/check.rs:279-283`).
    #[error(
        "the network has no lockbox disbursement for the NU6.1 activation block at height {height}"
    )]
    NoLockboxDisbursement { height: u32 },
    /// The NSM value balance before NU7 is not the value of the network.
    #[error("the NSM value balance before NU7 is {found} zatoshis and must be {expected}")]
    NsmSeedMismatch { expected: u64, found: u64 },
    /// An amount is above `MAX_MONEY`, or an operation on amounts leaves 64 bits.
    #[error("an amount is above MAX_MONEY or an operation on amounts overflows")]
    MoneyOverflow,
    /// An operation on heights, times or totals leaves its integer range.
    #[error("an operation on heights, times or totals overflows")]
    Overflow,
    /// A division by 0: a spec value or a window of 0 blocks.
    #[error("a division by 0")]
    DivisionByZero,
    /// A value of the spec that [`CoreSpec::checked`] refuses reached a rule: Sprout is not
    /// active at height 0, a table index is out of range, or a stream set has no first
    /// halving. A checked spec never gives this error.
    #[error("the spec did not pass the checks of CoreSpec::checked")]
    UncheckedSpec,
}

/// `a + b` as an amount: at most `MAX_MONEY`.
pub(crate) fn add_money(a: u64, b: u64) -> Result<u64, ConsensusError> {
    let Some(sum) = a.checked_add(b) else {
        return Err(ConsensusError::MoneyOverflow);
    };
    if sum > MAX_MONEY {
        return Err(ConsensusError::MoneyOverflow);
    }
    Ok(sum)
}

/// `a - b` as an amount.
pub(crate) fn sub_money(a: u64, b: u64) -> Result<u64, ConsensusError> {
    let Some(difference) = a.checked_sub(b) else {
        return Err(ConsensusError::MoneyOverflow);
    };
    Ok(difference)
}

/// `amount` when it is at most `MAX_MONEY`.
pub(crate) fn money(amount: u64) -> Result<u64, ConsensusError> {
    if amount > MAX_MONEY {
        return Err(ConsensusError::MoneyOverflow);
    }
    Ok(amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_operations_stay_below_max_money() {
        assert_eq!(add_money(MAX_MONEY - 1, 1), Ok(MAX_MONEY));
        assert_eq!(add_money(MAX_MONEY, 1), Err(ConsensusError::MoneyOverflow));
        assert_eq!(add_money(u64::MAX, 1), Err(ConsensusError::MoneyOverflow));
        assert_eq!(sub_money(5, 5), Ok(0));
        assert_eq!(sub_money(5, 6), Err(ConsensusError::MoneyOverflow));
        assert_eq!(money(MAX_MONEY), Ok(MAX_MONEY));
        assert_eq!(money(MAX_MONEY + 1), Err(ConsensusError::MoneyOverflow));
        assert_eq!(DIFFICULTY_CONTEXT_BLOCKS, 113);
    }
}
