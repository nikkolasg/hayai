//! What the driver does with a block that fails its validation.
//!
//! The header hash of a block commits to the transaction ids through the merkle root.
//! Before NU5 a transaction id commits to the whole transaction. From NU5 the id of a v5
//! or v6 transaction does not commit to the authorizing data (signatures, proofs,
//! scriptSig), and `hashBlockCommitments` commits to them. A peer can therefore send a
//! body that differs from the body of the header and has the same block hash in two ways:
//! changed authorizing data, and a duplicated transaction (the merkle tree of a list with
//! an odd length equals the tree of the list with its last entry twice, CVE-2012-2459).
//!
//! [`check_body`] refuses these two bodies before each commit path. A list with a repeated
//! transaction that is not such a mutation (its root is not the root of the list without
//! the repeat) is the list of the header: the block is invalid. The persistent
//! invalid mark of the header chain is only for a fault that the header hash commits to.

use hayai_prepared::PrepareError;
use hayai_state::history::{header_commitment, HeaderCommitment};
use hayai_state::{ChainView, ContextError};
use hayai_validate::{BlockError, ValidateConfig};
use hayai_wire::header::BlockHash;
use hayai_wire::{auth_data_root, RawBlock};

use super::NodeError;
use crate::config::Mode;
use crate::sync::merkle_mutation;

/// How the driver treats a validation error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Fault {
    /// The body is not the body that the header commits to. Another peer can have the
    /// right body: the peer gets the penalty and the header stays valid.
    WrongBody,
    /// The block breaks a consensus rule, and the header hash commits to the fault. The
    /// header chain records the block as invalid.
    Invalid,
    /// The node cannot validate the block: a missing verifying key, a state that the
    /// node does not hold, or an upgrade without a rule set. The block can be valid. The
    /// node stops, gives no penalty and writes no invalid mark.
    Local,
}

/// The [`Fault`] of each validation error. The caller ran [`check_body`] on the block.
pub(super) fn fault_of(error: &BlockError) -> Fault {
    match error {
        // The bytes are not the block of the header. [`check_body`] gives `MerkleRoot`
        // also for a merkle mutation: a list with a repeated transaction that has the
        // root of the list without it. A repeated transaction that is no mutation is
        // `DuplicateTxid`: the header commits to it, and the block is invalid.
        BlockError::Parse(_)
        | BlockError::MerkleRoot
        // From NU5 the commitment binds the authorizing data of the body. Before NU5 only
        // a miner can cause the error. The node does not tell the two cases apart, so
        // it never writes an invalid mark for this error.
        | BlockError::Context(ContextError::BlockCommitments) => Fault::WrongBody,
        // A capability or a state of this node.
        BlockError::Prepare {
            error: PrepareError::Unsupported(_) | PrepareError::SpentCoins { .. },
            ..
        }
        | BlockError::HeaderContext(_)
        | BlockError::Context(
            ContextError::SproutStateUnknown { .. }
            | ContextError::SpentMismatch { .. }
            | ContextError::Tree(_)
            | ContextError::History(_)
            | ContextError::Coinbase(hayai_consensus::coinbase::CoinbaseError::Consensus(_)),
        )
        // The driver selects the validation path from the checkpoint list and the header
        // chain. These errors show a fault of that selection.
        | BlockError::BelowMandatoryCheckpoint { .. }
        | BlockError::AboveLastCheckpoint { .. }
        | BlockError::NotOnCheckpointedChain { .. } => Fault::Local,
        BlockError::Header(_)
        | BlockError::Prepare { .. }
        | BlockError::Shielded(_)
        | BlockError::Context(_) => Fault::Invalid,
    }
}

/// The fatal error of a [`Fault::Local`].
pub(super) fn local_fault(
    mode: Mode,
    height: u32,
    hash: &BlockHash,
    error: &BlockError,
) -> NodeError {
    let hint = match (error, mode) {
        (BlockError::Context(ContextError::SproutStateUnknown { .. }), Mode::Shadow) => {
            " A shadow node has no Sprout treestate: `z_gettreestate` of the upstream node \
             gives the Sapling, Orchard and Ironwood trees only. Use a full node for a chain \
             range with JoinSplits."
        }
        _ => "",
    };
    NodeError(format!(
        "this node cannot validate the block {hash} at height {height}: {error}. The block \
         has no invalid mark and its peer has no penalty.{hint}"
    ))
}

/// Whether `raw` is the body that its header commits to on top of `view`, as far as the
/// merkle root does not show it: the list is no merkle mutation, and from NU5 the
/// authorizing data are the data of `hashBlockCommitments`. The error is the error that
/// the validation gives for such a body, and its [`Fault`] is [`Fault::WrongBody`].
///
/// `full` is false for a block of the checkpoint path, which checks the header commitment
/// itself.
pub(super) fn check_body(
    raw: &RawBlock,
    view: &ChainView,
    cfg: &ValidateConfig,
    full: bool,
) -> Result<(), BlockError> {
    // The body is a merkle mutation of the list that the root was built from.
    if let Some(_twice) = merkle_mutation(&raw.txids(), &raw.header.merkle_root) {
        return Err(BlockError::MerkleRoot);
    }
    if !full || !cfg.epoch().nu5_active() {
        return Ok(());
    }
    let root = auth_data_root(&raw.auth_digests());
    // The Sapling root is not read from NU5.
    match header_commitment(
        cfg.rules.branch_id,
        view.history().as_deref(),
        &[0; 32],
        &root,
    ) {
        HeaderCommitment::Expected(expected) if expected != raw.header.block_commitments => {
            Err(ContextError::BlockCommitments.into())
        }
        HeaderCommitment::Expected(_)
        | HeaderCommitment::Reserved
        | HeaderCommitment::ParentUnknown => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use hayai_coins::Pool;
    use hayai_consensus::{ConsensusError, HeaderRuleError, Upgrade};
    use hayai_crypto::zcash_primitives::transaction::TxId;

    use super::*;

    fn prepare(error: PrepareError) -> BlockError {
        BlockError::Prepare { tx: 1, error }
    }

    /// The invalid mark is only for a fault that the header hash commits to. A fault of
    /// the body alone is a wrong body, and a fault of this node stops the node.
    #[test]
    fn each_error_has_the_fault_of_its_cause() {
        let unsupported = ConsensusError::UnsupportedUpgrade {
            upgrade: Upgrade::Nu7,
            height: 9,
        };
        for (error, fault) in [
            (BlockError::MerkleRoot, Fault::WrongBody),
            (ContextError::BlockCommitments.into(), Fault::WrongBody),
            (
                prepare(PrepareError::Unsupported("orchard verifying key not built")),
                Fault::Local,
            ),
            (
                prepare(PrepareError::SpentCoins {
                    inputs: 1,
                    coins: 0,
                }),
                Fault::Local,
            ),
            (
                ContextError::SproutStateUnknown { tx: 1 }.into(),
                Fault::Local,
            ),
            (
                ContextError::Coinbase(unsupported.into()).into(),
                Fault::Local,
            ),
            (
                BlockError::BelowMandatoryCheckpoint {
                    height: 3,
                    mandatory: 5,
                },
                Fault::Local,
            ),
            (
                ContextError::DuplicateTxid(TxId::from_bytes([1; 32])).into(),
                Fault::Invalid,
            ),
            (
                ContextError::DoubleSpend { tx: 1, input: 0 }.into(),
                Fault::Invalid,
            ),
            (
                ContextError::BadAnchor {
                    pool: Pool::Orchard,
                    tx: 1,
                }
                .into(),
                Fault::Invalid,
            ),
            (prepare(PrepareError::NegativeFee), Fault::Invalid),
            (
                prepare(PrepareError::Script(0, "eval".into())),
                Fault::Invalid,
            ),
            (BlockError::Shielded(Vec::new()), Fault::Invalid),
            (HeaderRuleError::Version(3).into(), Fault::Invalid),
        ] {
            assert_eq!(fault_of(&error), fault, "{error}");
        }
    }

    /// The error of a block that this node cannot validate names the block, says that the
    /// block has no invalid mark, and in shadow mode names the limit of the Sprout state.
    #[test]
    fn the_error_of_a_local_fault_names_its_cause() {
        let hash = BlockHash([2; 32]);
        let error: BlockError = ContextError::SproutStateUnknown { tx: 4 }.into();
        let shadow = local_fault(Mode::Shadow, 77, &hash, &error).to_string();
        assert!(shadow.contains("height 77"), "{shadow}");
        assert!(
            shadow.contains("does not know the Sprout state"),
            "{shadow}"
        );
        assert!(shadow.contains("no invalid mark"), "{shadow}");
        assert!(shadow.contains("z_gettreestate"), "{shadow}");
        let full = local_fault(Mode::Full, 77, &hash, &error).to_string();
        assert!(!full.contains("z_gettreestate"), "{full}");
    }
}
