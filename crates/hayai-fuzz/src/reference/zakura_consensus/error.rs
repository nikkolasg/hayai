//! The transaction and block error types of zakura-consensus (`src/error.rs`).
//!
//! Copied items: `TransactionError`, `BlockError`, and three `From` conversions. Removed:
//! the `proptest` attributes, the variants `TransactionError::ValidateContextError` and
//! `BlockError::AlreadyInChain` (their fields are types of zakura-state), the other
//! conversions and the classification methods (they need tower, zakura-state or the node).

use chrono::{DateTime, Utc};
use thiserror::Error;

use zakura_chain::{
    amount, block, ironwood, orchard,
    parameters::subsidy::SubsidyError,
    sapling, sprout,
    transparent::{self, MIN_TRANSPARENT_COINBASE_MATURITY},
};

use super::{transaction_check::MAX_STANDARD_SCRIPTSIG_SIZE, MAX_BLOCK_SIGOPS};

/// Workaround for format string identifier rules.
const MAX_EXPIRY_HEIGHT: block::Height = block::Height::MAX_EXPIRY_HEIGHT;

/// Errors for semantic transaction validation.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum TransactionError {
    #[error("first transaction must be coinbase")]
    CoinbasePosition,

    #[error("coinbase input found in non-coinbase transaction")]
    CoinbaseAfterFirst,

    #[error("coinbase transaction MUST NOT have any JoinSplit descriptions")]
    CoinbaseHasJoinSplit,

    #[error("coinbase transaction MUST NOT have any Spend descriptions")]
    CoinbaseHasSpend,

    #[error("coinbase transaction MUST NOT have any Output descriptions pre-Heartwood")]
    CoinbaseHasOutputPreHeartwood,

    #[error("coinbase transaction MUST NOT have the EnableSpendsOrchard flag set")]
    CoinbaseHasEnableSpendsOrchard,

    #[error("coinbase transaction MUST NOT have the EnableSpendsIronwood flag set")]
    CoinbaseHasEnableSpendsIronwood,

    #[error("coinbase transaction MUST NOT have an Orchard shielded bundle")]
    CoinbaseHasOrchardShieldedData,

    #[error("coinbase transaction Sapling or Orchard outputs MUST be decryptable with an all-zero outgoing viewing key")]
    CoinbaseOutputsNotDecryptable,

    #[error("coinbase inputs MUST NOT exist in mempool")]
    CoinbaseInMempool,

    #[error("non-coinbase transactions MUST NOT have coinbase inputs")]
    NonCoinbaseHasCoinbaseInput,

    #[error("the tx is not coinbase, but it should be")]
    NotCoinbase,

    #[error("transaction is locked until after block height {}", _0.0)]
    LockedUntilAfterBlockHeight(block::Height),

    #[error("transaction is locked until after block time {0}")]
    LockedUntilAfterBlockTime(DateTime<Utc>),

    #[error(
        "coinbase expiry {expiry_height:?} must be the same as the block {block_height:?} \
         after NU5 activation, failing transaction: {transaction_hash:?}"
    )]
    CoinbaseExpiryBlockHeight {
        expiry_height: Option<zakura_chain::block::Height>,
        block_height: zakura_chain::block::Height,
        transaction_hash: zakura_chain::transaction::Hash,
    },

    #[error("could not construct coinbase tx: {0}")]
    CoinbaseConstruction(String),

    #[error(
        "expiry {expiry_height:?} must be less than the maximum {MAX_EXPIRY_HEIGHT:?} \
         coinbase: {is_coinbase}, block: {block_height:?}, failing transaction: {transaction_hash:?}"
    )]
    MaximumExpiryHeight {
        expiry_height: zakura_chain::block::Height,
        is_coinbase: bool,
        block_height: zakura_chain::block::Height,
        transaction_hash: zakura_chain::transaction::Hash,
    },

    #[error(
        "transaction must not be mined at a block {block_height:?} \
         greater than its expiry {expiry_height:?}, failing transaction {transaction_hash:?}"
    )]
    ExpiredTransaction {
        expiry_height: zakura_chain::block::Height,
        block_height: zakura_chain::block::Height,
        transaction_hash: zakura_chain::transaction::Hash,
    },

    #[error("coinbase transaction failed subsidy validation: {0}")]
    Subsidy(#[from] SubsidyError),

    #[error("transaction version number MUST be >= 4")]
    WrongVersion,

    #[error("transaction version {0} not supported by the network upgrade {1:?}")]
    UnsupportedByNetworkUpgrade(u32, zakura_chain::parameters::NetworkUpgrade),

    #[error("must have at least one input: transparent, shielded spend, or joinsplit")]
    NoInputs,

    #[error("must have at least one output: transparent, shielded output, or joinsplit")]
    NoOutputs,

    #[error("if there are no Spends or Outputs, the value balance MUST be 0.")]
    BadBalance,

    #[error("could not verify a transparent script: {0}")]
    Script(#[from] zakura_script::Error),

    #[error("Sapling proof or signature verification failed")]
    SaplingVerificationFailed,

    #[error("Orchard or Ironwood Halo2 proof verification failed")]
    Halo2VerificationFailed,

    #[error("spend description cv and rk MUST NOT be of small order")]
    SmallOrder,

    // TODO: the underlying error is bellman::VerificationError, but it does not implement
    // Arbitrary as required here.
    #[error("spend proof MUST be valid given a primary input formed from the other fields except spendAuthSig: {0}")]
    Groth16(String),

    // TODO: the underlying error is io::Error, but it does not implement Clone as required here.
    #[error("Groth16 proof is malformed: {0}")]
    MalformedGroth16(String),

    #[error(
        "Sprout joinSplitSig MUST represent a valid signature under joinSplitPubKey of dataToBeSigned: {0}"
    )]
    Ed25519(#[from] zakura_chain::primitives::ed25519::Error),

    #[error("Sapling bindingSig MUST represent a valid signature under the transaction binding validating key bvk of SigHash: {0}")]
    RedJubjub(zakura_chain::primitives::redjubjub::Error),

    #[error("Orchard bindingSig MUST represent a valid signature under the transaction binding validating key bvk of SigHash: {0}")]
    RedPallas(zakura_chain::primitives::reddsa::Error),

    #[error("could not convert an asynchronous verification error: {0}")]
    InternalDowncastError(String),

    #[error("either vpub_old or vpub_new must be zero")]
    BothVPubsNonZero,

    #[error("adding to the sprout pool is disabled after Canopy")]
    DisabledAddToSproutPool,

    #[error("adding to the orchard pool is disabled after NU6.3")]
    DisabledAddToOrchardPool,

    #[error("could not calculate the transaction fee")]
    IncorrectFee,

    #[error("transparent double-spend: {_0:?} is spent twice")]
    DuplicateTransparentSpend(transparent::OutPoint),

    #[error("sprout double-spend: duplicate nullifier: {_0:?}")]
    DuplicateSproutNullifier(sprout::Nullifier),

    #[error("sapling double-spend: duplicate nullifier: {_0:?}")]
    DuplicateSaplingNullifier(sapling::Nullifier),

    #[error("orchard double-spend: duplicate nullifier: {_0:?}")]
    DuplicateOrchardNullifier(orchard::Nullifier),

    #[error("ironwood double-spend: duplicate nullifier: {_0:?}")]
    DuplicateIronwoodNullifier(ironwood::Nullifier),

    #[error("must have at least one active orchard flag")]
    NotEnoughFlags,

    #[error("must have at least enable spend or enable output flag set")]
    NotEnoughIronwoodFlags,

    #[error("Orchard transactions MUST NOT have the EnableCrossAddress flag set")]
    OrchardHasEnableCrossAddress,

    #[error("could not find transparent input UTXO in the best chain or mempool")]
    TransparentInputNotFound,


    #[error("could not validate mempool transaction lock time on best chain: {0}")]
    // TODO: turn this into a typed error
    ValidateMempoolLockTimeError(String),

    #[error(
        "immature transparent coinbase spend: \
        attempt to spend {outpoint:?} at {spend_height:?}, \
        but spends are invalid before {min_spend_height:?}, \
        which is {MIN_TRANSPARENT_COINBASE_MATURITY:?} blocks \
        after it was created at {created_height:?}"
    )]
    #[non_exhaustive]
    ImmatureTransparentCoinbaseSpend {
        outpoint: transparent::OutPoint,
        spend_height: block::Height,
        min_spend_height: block::Height,
        created_height: block::Height,
    },

    #[error(
        "unshielded transparent coinbase spend: {outpoint:?} \
         must be spent in a transaction which only has shielded outputs"
    )]
    #[non_exhaustive]
    UnshieldedTransparentCoinbaseSpend {
        outpoint: transparent::OutPoint,
        min_spend_height: block::Height,
    },

    #[error(
        "failed to verify ZIP-317 transaction rules, transaction was not inserted to mempool: {0}"
    )]
    Zip317(#[from] zakura_chain::transaction::zip317::Error),

    // Mempool standardness (policy) rejections, applied before script verification.
    // These are not consensus rules: the same input scripts are valid in blocks.
    #[error(
        "mempool transaction input {input_index} has a {size} byte scriptSig, \
         above the {MAX_STANDARD_SCRIPTSIG_SIZE} byte standardness limit"
    )]
    NonStandardScriptSigSize { input_index: usize, size: usize },

    #[error("mempool transaction input {input_index} has a non-push-only scriptSig")]
    NonStandardScriptSigNotPushOnly { input_index: usize },

    #[error("mempool transaction has non-standard transparent inputs")]
    NonStandardInputs,

    #[error("transaction uses an incorrect consensus branch id")]
    WrongConsensusBranchId,

    #[error(
        "mempool transaction uses the NU6.2 consensus branch id during the NU6.3 grace period"
    )]
    WrongConsensusBranchIdNu6_3GracePeriod,

    #[error("wrong tx format: tx version is ≥ 5, but `nConsensusBranchId` is missing")]
    MissingConsensusBranchId,

    #[error("Orchard action count {actions} exceeds the per-block limit of {limit}")]
    OrchardActionsExceedBlockLimit { actions: u32, limit: u32 },

    #[error("Ironwood action count {actions} exceeds the per-block limit of {limit}")]
    IronwoodActionsExceedBlockLimit { actions: u32, limit: u32 },

    #[error("Sapling spends + outputs count {ios} exceeds the per-block limit of {limit}")]
    SaplingIOsExceedBlockLimit { ios: u32, limit: u32 },

    #[error("Sprout JoinSplit count {joinsplits} exceeds the per-block limit of {limit}")]
    SproutJoinSplitsExceedBlockLimit { joinsplits: u32, limit: u32 },

    #[error(
        "shielded cost {cost} \
         (Orchard actions + Ironwood actions + Sapling spends + Sapling outputs \
         + 2 * Sprout JoinSplits) \
         exceeds the per-block global shielded budget of {limit}"
    )]
    ShieldedCostExceedsBlockBudget { cost: u32, limit: u32 },

    #[error("input/output error")]
    Io(String),

    #[error("failed to convert a slice")]
    TryFromSlice(String),

    #[error("invalid amount")]
    Amount(String),

    #[error("invalid balance")]
    Balance(String),

    #[error("Orchard proof has a non-canonical size")]
    OrchardProofSize,

    #[error("Ironwood proof has a non-canonical size")]
    IronwoodProofSize,

    #[error("unexpected error")]
    Other(String),
}

impl From<amount::Error> for TransactionError {
    fn from(err: amount::Error) -> Self {
        TransactionError::Amount(err.to_string())
    }
}

#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum BlockError {
    #[error(transparent)]
    InvalidHeaderEncoding(#[from] zakura_header_chain::HeaderEncodingError),

    #[error("block contains invalid transactions")]
    Transaction(#[from] TransactionError),

    #[error("block has no transactions")]
    NoTransactions,

    #[error("block has mismatched merkle root")]
    BadMerkleRoot {
        actual: zakura_chain::block::merkle::Root,
        expected: zakura_chain::block::merkle::Root,
    },

    #[error("block contains duplicate transactions")]
    DuplicateTransaction,


    #[error("invalid block {0:?}: missing block height")]
    MissingHeight(zakura_chain::block::Hash),

    #[error("invalid block height {0:?} in {1:?}: greater than the maximum height {2:?}")]
    MaxHeight(
        zakura_chain::block::Height,
        zakura_chain::block::Hash,
        zakura_chain::block::Height,
    ),

    #[error("invalid difficulty threshold in block header {0:?} {1:?}")]
    InvalidDifficulty(zakura_chain::block::Height, zakura_chain::block::Hash),

    #[error("block {0:?} has a difficulty threshold {2:?} that is easier than the {3:?} difficulty limit {4:?}, hash: {1:?}")]
    TargetDifficultyLimit(
        zakura_chain::block::Height,
        zakura_chain::block::Hash,
        zakura_chain::work::difficulty::ExpandedDifficulty,
        zakura_chain::parameters::Network,
        zakura_chain::work::difficulty::ExpandedDifficulty,
    ),

    #[error(
        "block {0:?} on {3:?} has a hash {1:?} that is easier than its difficulty threshold {2:?}"
    )]
    DifficultyFilter(
        zakura_chain::block::Height,
        zakura_chain::block::Hash,
        zakura_chain::work::difficulty::ExpandedDifficulty,
        zakura_chain::parameters::Network,
    ),

    #[error("transaction has wrong consensus branch id for block network upgrade")]
    WrongTransactionConsensusBranchId,

    #[error(
        "block {height:?} {hash:?} has {sigops} legacy transparent signature operations, \
         but the limit is {MAX_BLOCK_SIGOPS}"
    )]
    TooManyTransparentSignatureOperations {
        height: zakura_chain::block::Height,
        hash: zakura_chain::block::Hash,
        sigops: u32,
    },

    #[error("summing miner fees for block {height:?} {hash:?} failed: {source:?}")]
    SummingMinerFees {
        height: zakura_chain::block::Height,
        hash: zakura_chain::block::Hash,
        source: amount::Error,
    },

    #[error("unexpected error occurred: {0}")]
    Other(String),
}

impl From<SubsidyError> for BlockError {
    fn from(err: SubsidyError) -> BlockError {
        BlockError::Transaction(TransactionError::Subsidy(err))
    }
}

impl From<amount::Error> for BlockError {
    fn from(e: amount::Error) -> Self {
        Self::from(SubsidyError::from(e))
    }
}
