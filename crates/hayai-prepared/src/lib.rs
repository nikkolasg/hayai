//! Prepared transactions: every context-free result of a transaction, computed once.
//!
//! Contract: `docs/architecture.md`, section hayai-prepared, and `docs/consensus.md`.
//!
//! - [`prepare`] turns a parsed transaction into a [`PreparedTx`]: spent coins, fee, sigops,
//!   nullifiers, note commitments, anchors, transparent script results and the shielded
//!   bundles queued on a [`ScopedBatch`]. It fails with the first rule the transaction
//!   breaks; nothing is skipped.
//! - [`Draft`] is the same work split so that a block validator can fetch the inputs of every
//!   unknown transaction in one round and run every script of the block as one flat
//!   parallel array ([`check_scripts`]).
//! - [`ShieldedBatcher`] / [`ScopedBatch`] batch-verify Orchard, Ironwood and Sapling
//!   bundles with the upstream batch validators and bisect a failing batch down to the
//!   failing bundles. They also verify the Groth16 JoinSplits of a v4 transaction and the
//!   JoinSplit signature.
//! - [`PreparedLookup`] gives the prepared transactions of a node by [`WtxId`]. A block
//!   validation takes the transactions it finds there as known. The store of the mempool
//!   (`hayai-mempool`) implements it.
//!
//! The mempool of a node, with its policy and its store, is in `hayai-mempool`.

#![forbid(unsafe_code)]

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use hayai_coins::{Coin, OutPoint, Pool};
use hayai_consensus::RuleSet;
use hayai_crypto::orchard::tree::MerkleHashOrchard;
use hayai_crypto::sapling_crypto::Node;
use hayai_crypto::{zcash_protocol, zcash_script};
use hayai_wire::{RawTx, WtxId};
use zcash_protocol::consensus::BranchId;
use zcash_script::interpreter::Flags;

mod coinbase;
mod orchard;
mod prepare;
mod sapling;
mod shielded;
mod sprout;

pub use crate::coinbase::CoinbaseError;
pub use crate::orchard::{circuit_version, OrchardKeys};
pub use crate::sapling::{
    SaplingKeys, SAPLING_OUTPUT_PARAMS_BLAKE2B, SAPLING_SPEND_PARAMS_BLAKE2B,
};
pub use prepare::{check_scripts, draft, prepare, Draft};
pub use shielded::{BatchOutcome, ScopedBatch, ShieldedBatcher, VerifyingKeys};
pub use sprout::{SproutKey, SPROUT_GROTH16_PARAMS_BLAKE2B, SPROUT_GROTH16_VK_BLAKE2B};

/// The consensus rules a context-free result was computed under: the branch id the
/// transaction was parsed and hashed with, and the script verification flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleEpoch {
    pub branch_id: BranchId,
    pub script_flags: Flags,
}

impl Hash for RuleEpoch {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.branch_id as u32).hash(state);
        self.script_flags.bits().hash(state);
    }
}

impl RuleEpoch {
    /// The epoch of block validation under `rules`.
    pub fn of(rules: &RuleSet) -> Self {
        Self {
            branch_id: rules.branch_id,
            script_flags: rules.script_flags,
        }
    }

    /// The flags zcashd applies to every transaction of a block
    /// (`ConnectBlock`: `SCRIPT_VERIFY_P2SH | SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY`), which
    /// Zakura's verifier also uses (`zakura-script/src/lib.rs:173-174`).
    /// Spec §7.12: BIP 16 and BIP 65 apply from the genesis block.
    pub const CONSENSUS_FLAGS: Flags = Flags::P2SH.union(Flags::CHECKLOCKTIMEVERIFY);

    /// The epoch of block validation for `branch_id`.
    pub fn consensus(branch_id: BranchId) -> Self {
        Self {
            branch_id,
            script_flags: Self::CONSENSUS_FLAGS,
        }
    }

    /// Whether NU5 rules (v5 transactions, Orchard, ZIP 203 coinbase expiry) are in force.
    pub fn nu5_active(&self) -> bool {
        nu5_or_later(self.branch_id)
    }
}

pub(crate) fn nu5_or_later(branch: BranchId) -> bool {
    !matches!(
        branch,
        BranchId::Sprout
            | BranchId::Overwinter
            | BranchId::Sapling
            | BranchId::Blossom
            | BranchId::Heartwood
            | BranchId::Canopy
    )
}

/// Note commitments a transaction adds to each tree, in transaction order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Commitments {
    pub orchard: Vec<MerkleHashOrchard>,
    pub sapling: Vec<Node>,
    /// The Ironwood tree has the node type of the Orchard tree.
    pub ironwood: Vec<MerkleHashOrchard>,
    /// Two commitments for each JoinSplit.
    pub sprout: Vec<[u8; 32]>,
}

/// A transaction with every context-free result attached.
///
/// `scripts_ok` is true once every transparent input passed script verification and
/// `shielded_ok` once every shielded bundle passed a batch ([`BatchOutcome`]); a transaction
/// without a bundle has `shielded_ok` set from the start. The store of the mempool rejects a
/// transaction unless both are true.
#[derive(Clone, Debug)]
pub struct PreparedTx {
    pub raw: Arc<RawTx>,
    pub epoch: RuleEpoch,
    /// Spent coins in input order; empty for a coinbase.
    pub spent: Vec<Coin>,
    /// Fee in zatoshis (zero for a coinbase).
    pub fee: u64,
    /// Legacy sigops of the transaction's own scripts plus P2SH redeem-script sigops.
    pub sigops: u32,
    pub nullifiers: Vec<(Pool, [u8; 32])>,
    pub commitments: Commitments,
    /// Distinct anchors referenced, per pool.
    pub anchors: Vec<(Pool, [u8; 32])>,
    pub orchard_actions: u32,
    pub ironwood_actions: u32,
    pub sapling_ios: u32,
    /// JoinSplit descriptions. Their anchors are not in `anchors`: a Sprout anchor can be
    /// a treestate inside the transaction, so hayai-state reads them from the transaction.
    pub joinsplits: u32,
    pub scripts_ok: bool,
    pub shielded_ok: bool,
    pub is_coinbase: bool,
    pub expiry_height: u32,
    pub lock_time: u32,
}

impl PreparedTx {
    pub fn wtxid(&self) -> WtxId {
        self.raw.wtxid()
    }

    /// Whether the transaction has a shielded bundle whose proofs and signatures are checked
    /// by a batch.
    pub fn has_shielded(&self) -> bool {
        self.orchard_actions > 0
            || self.ironwood_actions > 0
            || self.sapling_ios > 0
            || self.joinsplits > 0
    }

    /// Records the batch verdict for this transaction's bundles.
    pub fn set_shielded_ok(&mut self) {
        self.shielded_ok = true;
    }

    /// Outpoints spent, in input order.
    pub fn spent_outpoints(&self) -> impl Iterator<Item = &OutPoint> + '_ {
        self.raw
            .tx
            .transparent_bundle()
            .into_iter()
            .flat_map(|b| b.vin.iter().map(|i| i.prevout()))
    }
}

/// The prepared transactions of a node by [`WtxId`]. A block validation takes a transaction
/// that it finds here as known: it reuses the scripts and the proofs of the entry when the
/// entry is of the epoch of the block and fully verified.
pub trait PreparedLookup {
    /// The prepared transaction `id`, if the source holds it.
    fn prepared(&self, id: &WtxId) -> Option<Arc<PreparedTx>>;
}

/// Why a transaction cannot be prepared. Each variant is a consensus rule, a missing
/// capability, or a fault of the caller (`SpentCoins`); none of them is recoverable by
/// retrying with the same inputs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrepareError {
    #[error("transaction version {0} is not valid under branch {1:?}")]
    Version(String, BranchId),
    #[error("transaction branch {tx:?} differs from the epoch branch {epoch:?}")]
    BranchId { tx: BranchId, epoch: BranchId },
    #[error(
        "transaction has no inputs, no JoinSplits, no Sapling spends and no actions with \
         spends enabled"
    )]
    NoSource,
    #[error(
        "transaction has no outputs, no JoinSplits, no Sapling outputs and no actions with \
         outputs enabled"
    )]
    NoSink,
    #[error("the {0} pool is not active under branch {1:?}")]
    PoolNotActive(Pool, BranchId),
    #[error("input {0} repeats an earlier outpoint")]
    DuplicateInput(usize),
    #[error("coinbase has shielded spends")]
    CoinbaseShieldedSpend,
    #[error("coinbase has a JoinSplit")]
    CoinbaseJoinSplit,
    #[error("JoinSplit {0} has vpub_old and vpub_new above zero")]
    JoinSplitBothVpub(usize),
    #[error("JoinSplit {0} adds value to the Sprout pool from Canopy")]
    SproutPoolDeposit(usize),
    #[error("v4 transaction without Sapling spends or outputs has valueBalanceSapling {0}")]
    V4ValueBalance(i64),
    #[error("Orchard bundle with actions has neither enableSpends nor enableOutputs set")]
    OrchardFlags,
    #[error("Ironwood bundle with actions has neither enableSpends nor enableOutputs set")]
    IronwoodFlags,
    #[error("Orchard bundle has enableCrossAddress set from NU6.3")]
    OrchardCrossAddress,
    #[error("valueBalanceOrchard {0} adds value to the Orchard pool from NU6.3")]
    OrchardPoolDeposit(i64),
    #[error("coinbase has an Orchard bundle from NU6.3")]
    CoinbaseOrchardBundle,
    #[error(transparent)]
    CoinbaseShieldedOutput(#[from] CoinbaseError),
    #[error("coinbase scriptSig of {0} bytes is outside 2..=100")]
    CoinbaseScriptLength(usize),
    #[error("input {0} has a null previous outpoint in a non-coinbase transaction")]
    NullPrevout(usize),
    #[error("input {0} spends a coin that is not in the view")]
    MissingInput(usize),
    /// The caller of `draft` gave a list of spent coins that does not match the
    /// transaction: one coin for each transparent input, and no coin for a coinbase. It is
    /// a fault of the caller, not of the transaction.
    #[error("the caller gave {coins} spent coins for a transaction that spends {inputs}")]
    SpentCoins { inputs: usize, coins: usize },
    #[error("value overflow")]
    ValueOverflow,
    #[error("outputs exceed inputs: fee would be negative")]
    NegativeFee,
    #[error("input {0} script failed: {1}")]
    Script(usize, String),
    #[error("duplicate nullifier in {0}")]
    DuplicateNullifier(Pool),
    #[error("expiry height {0} is at or above the threshold")]
    ExpiryTooHigh(u32),
    #[error("not supported: {0}")]
    Unsupported(&'static str),
}

#[cfg(test)]
mod tests {
    use hayai_consensus::Upgrade;

    use super::*;

    #[test]
    fn the_epoch_of_a_rule_set_is_the_consensus_epoch_of_its_branch() {
        for upgrade in Upgrade::ALL {
            let Some(rules) = RuleSet::of(upgrade) else {
                assert_eq!(upgrade, Upgrade::Nu7);
                continue;
            };
            assert_eq!(RuleEpoch::of(rules), RuleEpoch::consensus(rules.branch_id));
            assert_eq!(
                RuleEpoch::of(rules).nu5_active(),
                rules.coinbase.expiry_is_height
            );
        }
    }
}
