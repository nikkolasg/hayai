//! The networks and the adapter around the pure consensus core (hayai-consensus-core).
//!
//! Contract: `docs/architecture.md`, section hayai-consensus.
//!
//! - [`Network`] names the three built-in networks and a custom chain. A [`ChainSpec`]
//!   holds every value of a network that the rules read: the activation heights,
//!   [`NetworkParams`] (genesis block, Equihash parameters, proof-of-work limit, halving
//!   interval, slow start), the checkpoints, the funding streams, the lockbox
//!   disbursements, the founders' addresses and the NSM seed. [`Network::spec`] gives the
//!   spec of a network, and [`Network::core`] the [`CoreSpec`] that the rules of the core
//!   read: the same values with the addresses decoded to scripts. A crate defines another
//!   chain as a spec, and [`ChainSpec::network`] checks it and gives its
//!   [`Network::Custom`].
//! - [`Upgrade`] names every network upgrade. [`Network::activation_height`] gives the
//!   height of each one. The Mainnet and Testnet heights are the heights of the
//!   `zcash_protocol` crate of the crypto backend, and the NU7 height is the same on every
//!   backend. [`branch_id`] gives the upstream branch id of an upgrade when the backend has
//!   it.
//! - A [`RuleSet`] holds the rules of one upgrade with the upstream branch id and script
//!   flags. [`rules_at`] is the one interface that selects it for a network and a height.
//!   An upgrade that is active and has no rule set is
//!   [`ConsensusError::UnsupportedUpgrade`]. The caller must stop: it must not apply the
//!   rule set of an earlier upgrade. The NU7 rule set exists when the crypto backend has
//!   the NU7 branch id.
//! - [`subsidy`], [`funding`], [`lockbox`], [`founders`], [`nsm`], [`difficulty`] and
//!   [`coinbase`] wrap the modules of the core for a caller with a [`Network`]. They do no
//!   rule work.
//! - [`header`] holds the header rules. Every path that accepts a header calls it. The
//!   proof of work (solution length, hash, Equihash) is here; the other rules are in the
//!   core.
//! - [`Checkpoints`] is a checkpoint list. [`Network::checkpoints`] gives the list of a
//!   network, and [`Network::mandatory_checkpoint_height`] the height below which a block
//!   has no full validation.
//!
//! The message start (magic) of a network belongs to hayai-net.

#![forbid(unsafe_code)]

mod address;
mod checkpoints;
pub mod coinbase;
pub mod difficulty;
pub mod founders;
pub mod funding;
pub mod header;
pub mod lockbox;
mod network;
pub mod nsm;
mod rules;
pub mod subsidy;

pub use address::address_of;
pub use checkpoints::{Checkpoints, DuplicateCheckpoint};
pub use hayai_consensus_core::rule_sets::{
    CoinbaseRules, DifficultyParams, HistoryVersion, ShieldedPools, TxVersions,
};
pub use hayai_consensus_core::{
    BlockLimits, ConsensusError, ContextTooShort, CoreSpec, P2shScript, ParentChain, Upgrade,
    COINBASE_MATURITY, DIFFICULTY_CONTEXT_BLOCKS, LOCKTIME_THRESHOLD, MAX_MONEY, MEDIAN_TIME_SPAN,
    POST_BLOSSOM_TARGET_SPACING, POST_NU7_TARGET_SPACING, PRE_BLOSSOM_TARGET_SPACING,
    TX_EXPIRY_HEIGHT_THRESHOLD,
};
pub use header::{HeaderRuleError, HeaderVerdict};
pub use network::{
    branch_id, ChainSpec, ChainSpecError, CheckedSpec, Network, NetworkParams, RegtestConfig,
    RegtestDisbursement, RegtestFundingStreams, RegtestRecipient,
};
pub use rules::{rules_at, RuleSet};

/// Depth below the tip at which a block is final: the node does not reorganize deeper.
/// The value of Zebra and Zakura (`MAX_BLOCK_REORG_HEIGHT`).
///
/// ZIP 218: a node should set `MAX_REORG_LENGTH` to 600 blocks from NU7. hayai keeps
/// 1,000 blocks at every height, as Zakura (`zakura-chain/src/parameters/constants.rs:30`).
pub const FINALITY_DEPTH: u32 = 1_000;

#[cfg(test)]
mod tests {
    use hayai_crypto::zcash_protocol;

    /// The constants of the core are the constants of the upstream crates.
    #[test]
    fn the_core_constants_are_the_upstream_constants() {
        assert_eq!(
            super::COINBASE_MATURITY,
            zcash_protocol::consensus::COINBASE_MATURITY_BLOCKS
        );
        assert_eq!(super::MAX_MONEY, zcash_protocol::value::MAX_MONEY);
        assert_eq!(super::DIFFICULTY_CONTEXT_BLOCKS, 113);
    }
}
