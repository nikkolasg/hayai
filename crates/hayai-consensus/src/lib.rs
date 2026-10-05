//! Network parameters and one rule set per network upgrade.
//!
//! Contract: `docs/architecture.md`, section hayai-consensus.
//!
//! - [`Network`] names the three networks. [`NetworkParams`] holds the values that depend
//!   only on the network: genesis block, Equihash parameters, proof-of-work limit, halving
//!   interval, slow start.
//! - [`Upgrade`] names every network upgrade. [`Network::activation_height`] gives the
//!   height of each one. Mainnet and Testnet heights come from the `zcash_protocol` crate of
//!   the crypto backend. The NU7 height is a constant of this crate on every backend.
//! - A [`RuleSet`] holds the rules of one upgrade. [`rules_at`] is the one interface that
//!   selects it for a network and a height. An upgrade that is active and has no rule set is
//!   [`ConsensusError::UnsupportedUpgrade`]. The caller must stop: it must not apply the
//!   rule set of an earlier upgrade. The NU7 rule set exists when the crypto backend has
//!   the NU7 branch id.
//! - [`subsidy`] holds the block subsidy schedule.
//! - [`nsm`] holds the Network Sustainability Mechanism of NU7: the fee share, the value
//!   balance and the reissuance.
//! - [`difficulty`] holds the difficulty adjustment: the expected `nBits` of a block and the
//!   work of a block.
//! - [`header`] holds the header rules. Every path that accepts a header calls it.
//! - [`Checkpoints`] is a checkpoint list. [`Network::checkpoints`] gives the list of a
//!   network, and [`Network::mandatory_checkpoint_height`] the height below which a block
//!   has no full validation.
//!
//! The message start (magic) of a network belongs to hayai-net.

#![forbid(unsafe_code)]

mod checkpoints;
pub mod coinbase;
pub mod difficulty;
pub mod founders;
pub mod funding;
pub mod header;
mod limits;
pub mod lockbox;
mod network;
pub mod nsm;
mod rules;
pub mod subsidy;

pub use checkpoints::{Checkpoints, DuplicateCheckpoint};
pub use difficulty::{ContextTooShort, ParentChain};
pub use header::{HeaderRuleError, HeaderVerdict};
pub use limits::BlockLimits;
pub use network::{Network, NetworkParams, RegtestConfig, RegtestConfigError, Upgrade};
pub use rules::{
    rules_at, CoinbaseRules, DifficultyParams, HistoryVersion, RuleSet, ShieldedPools, TxVersions,
};

use hayai_crypto::zcash_protocol;

/// Blocks a coinbase output must age before a transaction can spend it.
pub const COINBASE_MATURITY: u32 = zcash_protocol::consensus::COINBASE_MATURITY_BLOCKS;
/// Depth below the tip at which a block is final: the node does not reorganize deeper.
/// The value of Zebra and Zakura (`MAX_BLOCK_REORG_HEIGHT`).
pub const FINALITY_DEPTH: u32 = 1_000;
/// Expiry heights at or above this value are not valid (zcashd
/// `TX_EXPIRY_HEIGHT_THRESHOLD`).
pub const TX_EXPIRY_HEIGHT_THRESHOLD: u32 = 500_000_000;
/// Lock times below this value are block heights. Lock times at or above it are Unix times.
pub const LOCKTIME_THRESHOLD: u32 = 500_000_000;
/// Target block spacing before Blossom, in seconds.
pub const PRE_BLOSSOM_TARGET_SPACING: u32 = 150;
/// Target block spacing from Blossom until NU7, in seconds.
pub const POST_BLOSSOM_TARGET_SPACING: u32 = 75;
/// Target block spacing from NU7, in seconds (ZIP 218, `PostNU7PoWTargetSpacing`).
pub const POST_NU7_TARGET_SPACING: u32 = 25;
/// Blocks whose times form the median-time-past.
pub const MEDIAN_TIME_SPAN: usize = 11;
/// Newest blocks whose time and `bits` the difficulty rule of the next block reads: the
/// largest averaging window of the rule sets plus [`MEDIAN_TIME_SPAN`].
pub const DIFFICULTY_CONTEXT_BLOCKS: usize =
    DifficultyParams::POST_NU7.averaging_window as usize + MEDIAN_TIME_SPAN;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConsensusError {
    /// The upgrade is active at the height and this build has no rule set for it.
    #[error("{upgrade:?} is active at height {height} and this build has no rule set for it")]
    UnsupportedUpgrade { upgrade: Upgrade, height: u32 },
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
    /// The NSM value balance before NU7 is not the value of the network.
    #[error("the NSM value balance before NU7 is {found} zatoshis and must be {expected}")]
    NsmSeedMismatch { expected: u64, found: u64 },
}
