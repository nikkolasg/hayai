//! The outcome of one implementation for one case, and the comparison of two outcomes.

use serde::{Deserialize, Serialize};

/// The class of the rule that rejected a block. The two implementations name and order
/// their rules differently, so the classes are wide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RuleClass {
    /// The wire encoding of the block or of a transaction.
    Parse,
    /// A header field rule.
    Header,
    /// The merkle root, or a duplicate transaction.
    Merkle,
    /// The commitments field of the header.
    Commitments,
    /// The position, the count, the height or the input of the coinbase.
    CoinbaseForm,
    /// The value, the required outputs or the shielded outputs of the coinbase.
    CoinbaseTerms,
    /// Version, version group and consensus branch of a transaction.
    TxVersion,
    /// A transaction without inputs or outputs, flags, and the other structure rules.
    TxStructure,
    /// Expiry height and lock time.
    TxTime,
    /// A transparent input: missing, spent twice, coinbase maturity.
    TransparentInput,
    /// Values: a negative fee, an overflow, a pool below zero.
    Value,
    Script,
    /// A proof or a signature of a shielded bundle.
    ShieldedProof,
    /// A nullifier that the chain or the block has.
    Nullifier,
    /// An anchor that is not a root of the chain.
    Anchor,
    /// A chain value pool below zero.
    ValuePool,
    /// The append to the history tree.
    History,
    /// Signature operations, shielded action counts and sizes.
    Limits,
    /// The implementation has no support for a part of the block.
    Unsupported,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Accept,
    Reject {
        class: RuleClass,
        /// The error text. It is for the reader of a finding. The comparison does not
        /// read it.
        detail: String,
    },
    /// The implementation panicked.
    Panic(String),
    /// The oracle has no reference for a part of the block, and no other rule rejected
    /// the block.
    NotCovered(String),
}

impl Verdict {
    pub fn reject(class: RuleClass, detail: impl ToString) -> Self {
        Verdict::Reject {
            class,
            detail: detail.to_string(),
        }
    }
}

/// The comparison of the verdict of hayai with the verdict of the reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Comparison {
    BothAccept,
    /// Both reject with the same rule class.
    BothReject,
    /// Both reject, with different rule classes. This is not a finding: a block can break
    /// two rules, and the implementations check their rules in different orders.
    BothRejectOtherClass,
    /// hayai accepts a block that the reference rejects.
    HayaiAccepts,
    /// hayai rejects a block that the reference accepts.
    HayaiRejects,
    /// One implementation panicked.
    Panic,
    /// The reference accepts and hayai rejects with a rule that the oracle does not have:
    /// the case gives no information.
    NoOracle,
}

impl Comparison {
    /// A disagreement on the outcome, or a panic.
    pub fn is_finding(self) -> bool {
        matches!(
            self,
            Comparison::HayaiAccepts | Comparison::HayaiRejects | Comparison::Panic
        )
    }
}

/// Rule classes of hayai for which the oracle has no rule. A reject of hayai with such a
/// class against an accept of the reference is [`Comparison::NoOracle`].
///
/// The anchor sets, the chain value pools and the history tree are in zakura-state, which
/// does not link into this process.
const NO_ORACLE: [RuleClass; 3] = [RuleClass::Anchor, RuleClass::ValuePool, RuleClass::History];

pub fn compare(hayai: &Verdict, reference: &Verdict) -> Comparison {
    match (hayai, reference) {
        (Verdict::Panic(_), _) | (_, Verdict::Panic(_)) => Comparison::Panic,
        // hayai has no "not covered" verdict.
        (Verdict::NotCovered(_), _) => Comparison::NoOracle,
        (Verdict::Accept, Verdict::Accept) => Comparison::BothAccept,
        (Verdict::Accept, Verdict::Reject { .. }) => Comparison::HayaiAccepts,
        (Verdict::Reject { .. }, Verdict::NotCovered(_)) => Comparison::NoOracle,
        (Verdict::Accept, Verdict::NotCovered(_)) => Comparison::NoOracle,
        (Verdict::Reject { class, .. }, Verdict::Accept) => {
            if NO_ORACLE.contains(class) {
                Comparison::NoOracle
            } else {
                Comparison::HayaiRejects
            }
        }
        (Verdict::Reject { class: a, .. }, Verdict::Reject { class: b, .. }) => {
            if a == b {
                Comparison::BothReject
            } else {
                Comparison::BothRejectOtherClass
            }
        }
    }
}
