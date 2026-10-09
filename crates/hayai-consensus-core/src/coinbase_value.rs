//! The coinbase value rules of one block (protocol specification §7.1.2, §7.9, §7.10,
//! ZIP 236, ZIP 271).
//!
//! [`CoinbaseTerms::at`] collects what the coinbase of a height must pay: the founders'
//! reward before Canopy, one output for each funding stream with a script from Canopy,
//! and the lockbox disbursement outputs in the NU6.1 activation block.
//! [`CoinbaseTerms::check`] checks a coinbase against them. The block template takes its
//! outputs from the same terms, so the template and the validator agree.
//!
//! The checks follow Zakura's `subsidy_is_valid` and `miner_fees_are_valid`
//! (`zakura-consensus/src/block/check.rs:177-383`). From NU7 the coinbase gets the miner
//! share of the fees, and from the NSM reissuance height the subsidy has a bonus
//! ([`crate::nsm`]).

use crate::funding::Receiver;
use crate::rule_sets::RuleSet;
use crate::subsidy_schedule::Subsidy;
use crate::{
    add_money, founders, funding, lockbox, nsm, sub_money, subsidy_schedule, ConsensusError,
    CoreSpec, P2shScript, Upgrade,
};

/// Why the coinbase must have an output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    FoundersReward,
    FundingStream(Receiver),
    LockboxDisbursement,
}

/// One output that the coinbase must have: the exact value and the exact script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequiredOutput {
    pub kind: OutputKind,
    /// Zatoshis.
    pub value: u64,
    /// The `scriptPubKey`: `OP_HASH160 <script hash> OP_EQUAL`.
    pub script: P2shScript,
}

/// A transparent output of a coinbase, as [`CoinbaseTerms::check`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinbaseOutput {
    /// Zatoshis.
    pub value: u64,
    /// The `scriptPubKey`.
    pub script: Vec<u8>,
}

/// The value balances of the shielded bundles of a coinbase, as the transaction encodes
/// them. A negative balance is value that enters the pool. A coinbase without a bundle
/// has a balance of 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShieldedBalances {
    pub sapling: i64,
    pub orchard: i64,
    pub ironwood: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CoinbaseError {
    #[error(transparent)]
    Consensus(#[from] ConsensusError),
    /// No unmatched output has the script or the value of the required output.
    #[error("coinbase has no {kind:?} output of {value} zatoshis to script {script:02x?}")]
    MissingOutput {
        kind: OutputKind,
        value: u64,
        script: Vec<u8>,
    },
    /// An output pays the script of the required output with another value.
    #[error("coinbase {kind:?} output pays {found} zatoshis and must pay {expected}")]
    WrongAmount {
        kind: OutputKind,
        expected: u64,
        found: u64,
    },
    /// An output has the value of the required output and another script.
    #[error(
        "coinbase {kind:?} output of {value} zatoshis pays script {found:02x?} and must pay \
         {expected:02x?}"
    )]
    WrongScript {
        kind: OutputKind,
        value: u64,
        expected: Vec<u8>,
        found: Vec<u8>,
    },
    /// Before NU6: the coinbase pays more than the subsidy that it can pay out and the fees.
    #[error("coinbase pays {paid} zatoshis, more than the limit of {allowed}")]
    ValueAboveLimit { paid: i128, allowed: i128 },
    /// From NU6 (ZIP 236): the coinbase does not pay the subsidy that it can pay out and
    /// the fees exactly.
    #[error("coinbase pays {paid} zatoshis and must pay {required} exactly")]
    ValueNotExact { paid: i128, required: i128 },
    /// The block pays more out of the deferred pool than the pool holds.
    #[error("deferred pool of {before} zatoshis cannot pay a disbursement of {disbursed}")]
    NegativeDeferredPool { before: u64, disbursed: u64 },
}

/// What the coinbase of one height must pay and can pay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinbaseTerms {
    pub subsidy: Subsidy,
    /// The outputs that the coinbase must have. Each one needs its own coinbase output: two
    /// equal required outputs need two coinbase outputs.
    pub required: Vec<RequiredOutput>,
    /// Zatoshis that the required lockbox disbursement outputs take out of the deferred
    /// pool.
    pub disbursed: u64,
    /// ZIP 236, from NU6: the coinbase pays its limit exactly.
    pub exact_value: bool,
    /// From NU7: the coinbase gets the miner share of the fees
    /// ([`CoinbaseTerms::miner_fees`]).
    pub nsm_fee_share: bool,
    /// The part of the subsidy that the miner can pay to outputs of its choice: the
    /// subsidy without the deferred part, the founders' reward and the funding streams.
    /// The miner adds the fees of the block to it.
    ///
    /// Spec §7.8: `MinerSubsidy(height)`.
    pub miner_subsidy: u64,
}

/// Adds the required output `kind` of `value` zatoshis to `script`.
fn require(required: &mut Vec<RequiredOutput>, kind: OutputKind, value: u64, script: P2shScript) {
    required.push(RequiredOutput {
        kind,
        value,
        script,
    });
}

impl CoinbaseTerms {
    /// The terms of the coinbase at `height`, for a caller without the chain value pools.
    /// `rules` is the rule set of `height` ([`crate::rule_sets::rules_at`]): the caller selects
    /// it one time for every rule of the block.
    ///
    /// It fails with [`ConsensusError::IssuedSupplyUnknown`] from the NSM reissuance
    /// height: [`CoinbaseTerms::after`] gives the terms there.
    pub fn at(spec: &CoreSpec, rules: &RuleSet, height: u32) -> Result<Self, ConsensusError> {
        Self::terms(spec, rules, height, None)
    }

    /// The terms of the coinbase at `height`, in a block whose parent leaves `issued`
    /// zatoshis in the chain value pools in total. Block validation calls this function.
    /// `rules` is the rule set of `height`, as in [`CoinbaseTerms::at`].
    ///
    /// From the NSM reissuance height the subsidy is the subsidy of the halving schedule
    /// plus the reissuance bonus of the NSM value balance after the parent (Zakura
    /// `block_subsidy`, `zakura-chain/src/parameters/network/subsidy.rs:927-946`). It
    /// fails with [`ConsensusError::NegativeNsmBalance`] when that balance is negative.
    pub fn after(
        spec: &CoreSpec,
        rules: &RuleSet,
        height: u32,
        issued: u64,
    ) -> Result<Self, ConsensusError> {
        Self::terms(spec, rules, height, Some(issued))
    }

    fn terms(
        spec: &CoreSpec,
        rules: &RuleSet,
        height: u32,
        issued: Option<u64>,
    ) -> Result<Self, ConsensusError> {
        let mut total = subsidy_schedule::scheduled_subsidy(spec, height)?;
        // ZIP 237: from DEPLOYMENT_BLOCK_HEIGHT, BlockSubsidy adds AdditionalBlockSubsidy of
        // NSMValueBalance(height - 1).
        if nsm::reissuance_active(spec, height)? {
            let Some(issued) = issued else {
                return Err(ConsensusError::IssuedSupplyUnknown { height });
            };
            let Some(parent) = height.checked_sub(1) else {
                return Err(ConsensusError::Overflow);
            };
            let bonus = nsm::reissuance_bonus(nsm::balance(spec, parent, issued)?)?;
            total = add_money(total, bonus)?;
        }
        let mut terms = CoinbaseTerms {
            subsidy: Subsidy { total, deferred: 0 },
            required: Vec::new(),
            disbursed: 0,
            exact_value: rules.coinbase.exact_value,
            nsm_fee_share: rules.coinbase.nsm_fee_share,
            miner_subsidy: total,
        };
        // A block without a subsidy has no required output (Zakura `subsidy_is_valid`). The
        // NU6.1 disbursement rule of Spec §7.10 does not depend on the subsidy: only a
        // Regtest NU6.1 height after the last subsidy reaches this difference.
        if total == 0 {
            return Ok(terms);
        }
        // Spec §7.9: the founders' reward output before Canopy.
        if let Some(reward) = founders::founders_reward(spec, height)? {
            require(
                &mut terms.required,
                OutputKind::FoundersReward,
                reward.value,
                reward.script,
            );
        }
        // Spec §7.10: one output for each active stream with a script; DEFERRED_POOL adds
        // to totalDeferredOutput. ZIP 237: the streams take their share of the subsidy with
        // the reissuance bonus.
        let streams = funding::funding_streams(spec, height, total)?;
        let mut failure: Option<ConsensusError> = None;
        for i in 0..streams.len() {
            let stream = streams[i];
            match stream.script {
                Some(script) => require(
                    &mut terms.required,
                    OutputKind::FundingStream(stream.receiver),
                    stream.value,
                    script,
                ),
                None => match add_money(terms.subsidy.deferred, stream.value) {
                    Ok(deferred) => terms.subsidy.deferred = deferred,
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                },
            }
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        // Spec §7.10, ZIP 271: ZIP271DisbursementChunks outputs at ZIP271ActivationHeight,
        // paid from the deferred pool (totalDeferredInput).
        let mut disbursed = 0;
        if spec.activation_height(Upgrade::Nu6_1) == Some(height) {
            let disbursements = lockbox::disbursements(spec, height);
            // Zakura `subsidy_is_valid` (`check.rs:276-283`): a network without a
            // disbursement has no valid NU6.1 activation block.
            if disbursements.len() == 0 {
                return Err(ConsensusError::NoLockboxDisbursement { height });
            }
            let mut failure: Option<ConsensusError> = None;
            for d in 0..disbursements.len() {
                let disbursement = disbursements[d];
                for _ in 0..disbursement.count {
                    require(
                        &mut terms.required,
                        OutputKind::LockboxDisbursement,
                        disbursement.value,
                        disbursement.script,
                    );
                }
                let total = match disbursement.total() {
                    Ok(total) => total,
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                };
                disbursed = match add_money(disbursed, total) {
                    Ok(disbursed) => disbursed,
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                };
            }
            if let Some(failure) = failure {
                return Err(failure);
            }
        }
        terms.disbursed = disbursed;
        // Spec §7.8: `MinerSubsidy(height)`. The disbursement outputs are paid from the
        // deferred pool, not from the subsidy.
        let mut required = 0u64;
        let mut failure: Option<ConsensusError> = None;
        for i in 0..terms.required.len() {
            required = match add_money(required, terms.required[i].value) {
                Ok(required) => required,
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            };
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        let paid_from_subsidy = sub_money(required, disbursed)?;
        terms.miner_subsidy =
            sub_money(sub_money(total, terms.subsidy.deferred)?, paid_from_subsidy)?;
        Ok(terms)
    }

    /// The part of `fees`, the total fees of the block, that the coinbase gets: all of
    /// them before NU7, the miner share from NU7 (Zakura `miner_fee_share`,
    /// `zakura-chain/src/parameters/network/subsidy/fees.rs:20-41`).
    ///
    /// ZIP 235: from NU7 the total input value has `MinerFees(height)` in place of the fees.
    pub fn miner_fees(&self, fees: u64) -> Result<u64, ConsensusError> {
        match self.nsm_fee_share {
            true => nsm::miner_fee_share(fees),
            false => crate::money(fees),
        }
    }

    /// The value that the coinbase takes out of the block with `fees` zatoshis of fees: the
    /// subsidy and the fees of the miner, without the deferred part, plus the lockbox
    /// disbursement.
    ///
    /// ZIP 2001, ZIP 271, ZIP 235: the total input value (`BlockSubsidy` plus the fees or
    /// `MinerFees`, plus `totalDeferredInput`) minus `totalDeferredOutput` of the total
    /// output value.
    fn payable(&self, fees: u64) -> Result<i128, ConsensusError> {
        let miner_fees = self.miner_fees(fees)?;
        let (Some(income), Some(kept)) = (
            i128::from(self.subsidy.total).checked_add(i128::from(miner_fees)),
            i128::from(self.disbursed).checked_sub(i128::from(self.subsidy.deferred)),
        ) else {
            return Err(ConsensusError::Overflow);
        };
        let Some(payable) = income.checked_add(kept) else {
            return Err(ConsensusError::Overflow);
        };
        Ok(payable)
    }

    /// Checks the coinbase with the transparent `outputs` (value in zatoshis, script) and
    /// the `shielded` value balances in a block with `fees` zatoshis of fees.
    ///
    /// - Each required output matches one coinbase output of the same value and script
    ///   that no other required output matched.
    /// - The value that the coinbase pays is the value of its transparent outputs minus
    ///   its shielded value balances. From NU6 it equals the subsidy plus the fees of the
    ///   miner, without the deferred part, plus the lockbox disbursement. Before NU6 it is
    ///   at most that value.
    ///
    /// Spec §7.10: at least one distinct output for each required payment, also for equal
    /// payments.
    pub fn check(
        &self,
        outputs: &[CoinbaseOutput],
        shielded: ShieldedBalances,
        fees: u64,
    ) -> Result<(), CoinbaseError> {
        let mut matched: Vec<bool> = vec![false; outputs.len()];
        let mut unmatched: Option<usize> = None;
        for r in 0..self.required.len() {
            let required = &self.required[r];
            let mut found: Option<usize> = None;
            for o in 0..outputs.len() {
                if let Some(_) = found {
                    continue;
                }
                let output = &outputs[o];
                if !matched[o]
                    && output.value == required.value
                    && output.script[..] == required.script[..]
                {
                    found = Some(o);
                }
            }
            let Some(index) = found else {
                unmatched = Some(r);
                break;
            };
            matched[index] = true;
        }
        if let Some(r) = unmatched {
            return Err(unmatched_error(&self.required[r], outputs, &matched));
        }

        let mut transparent: i128 = 0;
        let mut overflow = false;
        for o in 0..outputs.len() {
            let Some(sum) = transparent.checked_add(i128::from(outputs[o].value)) else {
                overflow = true;
                break;
            };
            transparent = sum;
        }
        if overflow {
            return Err(ConsensusError::Overflow.into());
        }
        let (Some(less_sapling), Some(shielded_rest)) = (
            transparent.checked_sub(i128::from(shielded.sapling)),
            i128::from(shielded.orchard).checked_add(i128::from(shielded.ironwood)),
        ) else {
            return Err(ConsensusError::Overflow.into());
        };
        let Some(paid) = less_sapling.checked_sub(shielded_rest) else {
            return Err(ConsensusError::Overflow.into());
        };
        let payable = self.payable(fees)?;
        // ZIP 236: from NU6 the total output value equals the total input value. Spec
        // §7.1.2: before NU6 it is at most the total input value.
        if self.exact_value {
            if paid != payable {
                return Err(CoinbaseError::ValueNotExact {
                    paid,
                    required: payable,
                });
            }
        } else if paid > payable {
            return Err(CoinbaseError::ValueAboveLimit {
                paid,
                allowed: payable,
            });
        }
        Ok(())
    }

    /// The deferred pool after the block, from a pool of `before` zatoshis: the pool gains
    /// the deferred part of the subsidy and loses the lockbox disbursement.
    pub fn deferred_pool_after(&self, before: u64) -> Result<u64, CoinbaseError> {
        let Some(after) =
            lockbox::deferred_pool_after(before, self.subsidy.deferred, self.disbursed)
        else {
            return Err(CoinbaseError::NegativeDeferredPool {
                before,
                disbursed: self.disbursed,
            });
        };
        Ok(after)
    }
}

/// The error for a required output that no unmatched output of `outputs` matches.
fn unmatched_error(
    required: &RequiredOutput,
    outputs: &[CoinbaseOutput],
    matched: &[bool],
) -> CoinbaseError {
    let kind = required.kind;
    let mut same_script: Option<usize> = None;
    for o in 0..outputs.len() {
        if let Some(_) = same_script {
            continue;
        }
        if !matched[o] && outputs[o].script[..] == required.script[..] {
            same_script = Some(o);
        }
    }
    let mut same_value: Option<usize> = None;
    for o in 0..outputs.len() {
        if let Some(_) = same_value {
            continue;
        }
        if !matched[o] && outputs[o].value == required.value {
            same_value = Some(o);
        }
    }
    let mut expected = Vec::with_capacity(required.script.len());
    for i in 0..required.script.len() {
        expected.push(required.script[i]);
    }
    match (same_script, same_value) {
        (Some(o), _) => CoinbaseError::WrongAmount {
            kind,
            expected: required.value,
            found: outputs[o].value,
        },
        (None, Some(o)) => {
            let found = outputs[o].script.clone();
            CoinbaseError::WrongScript {
                kind,
                value: required.value,
                expected,
                found,
            }
        }
        (None, None) => CoinbaseError::MissingOutput {
            kind,
            value: required.value,
            script: expected,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain_spec::tests::regtest;
    use crate::funding::tests::{regtest_with_streams, SCRIPT_A, SCRIPT_B, SCRIPT_C};
    use crate::lockbox::Disbursement;
    use crate::rule_sets::rules_at;
    use crate::MAX_MONEY;

    const MINER: &[u8] = &[0x51];
    const REGTEST_SUBSIDY: u64 = 625_000_000;

    /// `CoinbaseTerms::at` with the rule set of `height`.
    fn at(spec: &CoreSpec, height: u32) -> Result<CoinbaseTerms, ConsensusError> {
        let rules = rules_at(spec, height)?;
        CoinbaseTerms::at(spec, &rules, height)
    }

    /// `CoinbaseTerms::after` with the rule set of `height`.
    fn after(spec: &CoreSpec, height: u32, issued: u64) -> Result<CoinbaseTerms, ConsensusError> {
        let rules = rules_at(spec, height)?;
        CoinbaseTerms::after(spec, &rules, height, issued)
    }

    /// The outputs of a coinbase that pays `miner` zatoshis to the miner and every
    /// required output of `terms`.
    fn output(value: u64, script: &[u8]) -> CoinbaseOutput {
        CoinbaseOutput {
            value,
            script: script.to_vec(),
        }
    }

    fn outputs(terms: &CoinbaseTerms, miner: u64) -> Vec<CoinbaseOutput> {
        let mut outputs = vec![output(miner, MINER)];
        for required in &terms.required {
            outputs.push(output(required.value, &required.script));
        }
        outputs
    }

    fn check(
        terms: &CoinbaseTerms,
        outputs: &[CoinbaseOutput],
        fees: u64,
    ) -> Result<(), CoinbaseError> {
        terms.check(outputs, ShieldedBalances::default(), fees)
    }

    fn kinds(terms: &CoinbaseTerms) -> Vec<OutputKind> {
        terms.required.iter().map(|output| output.kind).collect()
    }

    /// The outputs of `outputs` with output `index` changed to `value` and `script`, and
    /// the difference of the value moved to the miner output, so that the value rule holds.
    fn changed(
        outputs: &[CoinbaseOutput],
        index: usize,
        value: u64,
        script: &[u8],
    ) -> Vec<CoinbaseOutput> {
        let mut outputs = outputs.to_vec();
        outputs[0].value = outputs[0].value + outputs[index].value - value;
        outputs[index] = output(value, script);
        outputs
    }

    /// Regtest with NU6 at 20, NU6.1 at 30 and `disbursements` of one output each.
    fn regtest_with_disbursements(disbursements: &[Disbursement]) -> CoreSpec {
        let mut spec = regtest();
        spec.activation_heights[Upgrade::Nu6.index()] = Some(20);
        spec.activation_heights[Upgrade::Nu6_1.index()] = Some(30);
        spec.lockbox_disbursements = disbursements.to_vec();
        spec.checked().expect("a valid spec")
    }

    static THREE: [Disbursement; 3] = [
        Disbursement {
            count: 1,
            value: 1_000,
            script: SCRIPT_A,
        },
        Disbursement {
            count: 1,
            value: 0,
            script: SCRIPT_B,
        },
        Disbursement {
            count: 1,
            value: 1_000,
            script: SCRIPT_A,
        },
    ];

    #[test]
    fn regtest_terms_give_the_subsidy_to_the_miner() {
        let terms = at(&regtest(), 1).unwrap();
        assert_eq!(kinds(&terms), Vec::<OutputKind>::new());
        assert_eq!(
            (terms.miner_subsidy, terms.exact_value),
            (REGTEST_SUBSIDY, false)
        );
        assert_eq!(
            terms.subsidy,
            Subsidy {
                total: REGTEST_SUBSIDY,
                deferred: 0
            }
        );
        let fees = 1_234;
        assert_eq!(
            check(&terms, &outputs(&terms, terms.miner_subsidy + fees), fees),
            Ok(())
        );
        // Before NU6 the value is a limit.
        assert_eq!(check(&terms, &outputs(&terms, 0), fees), Ok(()));
        assert_eq!(
            check(&terms, &outputs(&terms, REGTEST_SUBSIDY + fees + 1), fees),
            Err(CoinbaseError::ValueAboveLimit {
                paid: i128::from(REGTEST_SUBSIDY + fees + 1),
                allowed: i128::from(REGTEST_SUBSIDY + fees),
            })
        );
        assert_eq!(terms.miner_fees(1_000), Ok(1_000));
        assert_eq!(
            terms.miner_fees(MAX_MONEY + 1),
            Err(ConsensusError::MoneyOverflow)
        );
        // The genesis block has no subsidy and no required output.
        let genesis = at(&regtest(), 0).unwrap();
        assert_eq!((genesis.subsidy.total, genesis.required.len()), (0, 0));
    }

    /// The disbursements of a Regtest configuration, with the meaning of Zakura's Regtest
    /// parameters: each entry is one output of the NU6.1 activation block, and the
    /// deferred pool pays it.
    #[test]
    fn a_configured_chain_pays_its_disbursements_at_nu6_1() {
        let spec = regtest_with_disbursements(&THREE);
        for height in [29, 31] {
            let terms = at(&spec, height).unwrap();
            assert_eq!(
                (kinds(&terms), terms.disbursed),
                (Vec::new(), 0),
                "{height}"
            );
        }
        let terms = at(&spec, 30).unwrap();
        assert_eq!(kinds(&terms), vec![OutputKind::LockboxDisbursement; 3]);
        assert_eq!(terms.disbursed, 2_000);
        assert_eq!(terms.miner_subsidy, REGTEST_SUBSIDY);
        assert!(terms.exact_value);
        assert_eq!(terms.deferred_pool_after(2_000), Ok(0));
        assert_eq!(
            terms.deferred_pool_after(1_999),
            Err(CoinbaseError::NegativeDeferredPool {
                before: 1_999,
                disbursed: 2_000
            })
        );
        let kind = OutputKind::LockboxDisbursement;
        let valid = outputs(&terms, REGTEST_SUBSIDY);
        assert_eq!(check(&terms, &valid, 0), Ok(()));
        assert_eq!(
            check(&terms, &changed(&valid, 1, 999, &SCRIPT_A), 0),
            Err(CoinbaseError::WrongAmount {
                kind,
                expected: 1_000,
                found: 999,
            })
        );
        assert_eq!(
            check(&terms, &changed(&valid, 2, 0, &SCRIPT_C), 0),
            Err(CoinbaseError::WrongScript {
                kind,
                value: 0,
                expected: SCRIPT_B.to_vec(),
                found: SCRIPT_C.to_vec(),
            })
        );
        // Two equal entries need two outputs.
        assert_eq!(
            check(&terms, &valid[..3], 0),
            Err(CoinbaseError::MissingOutput {
                kind,
                value: 1_000,
                script: SCRIPT_A.to_vec(),
            })
        );
        // From NU6 the deferred part and the disbursement enter the exact value.
        assert_eq!(
            check(&terms, &outputs(&terms, REGTEST_SUBSIDY + 1), 0),
            Err(CoinbaseError::ValueNotExact {
                paid: i128::from(REGTEST_SUBSIDY) + 2_001,
                required: i128::from(REGTEST_SUBSIDY) + 2_000,
            })
        );
    }

    /// Zakura refuses each block at the NU6.1 activation height of a chain without a
    /// lockbox disbursement, while the block has a subsidy (`subsidy_is_valid`,
    /// `zakura-consensus/src/block/check.rs:183-185,276-283`).
    #[test]
    fn a_chain_without_a_disbursement_has_no_nu6_1_activation_block() {
        let spec = regtest_with_disbursements(&[]);
        assert_eq!(
            at(&spec, 30),
            Err(ConsensusError::NoLockboxDisbursement { height: 30 })
        );
        for height in [29, 31] {
            assert_eq!(kinds(&at(&spec, height).unwrap()), Vec::new());
        }
        // A block without a subsidy has no required output and no such rule.
        let mut late = regtest();
        late.activation_heights[Upgrade::Nu6.index()] = Some(20);
        late.activation_heights[Upgrade::Nu6_1.index()] = Some(288 * 64);
        assert_eq!(subsidy_schedule::scheduled_subsidy(&late, 288 * 64), Ok(0));
        assert_eq!(kinds(&at(&late, 288 * 64).unwrap()), Vec::new());
    }

    /// The funding streams of a configured chain: the terms pay each stream with a script
    /// and keep the deferred part out of the outputs.
    #[test]
    fn a_configured_chain_pays_its_funding_streams() {
        let spec = regtest_with_streams();
        let mg = OutputKind::FundingStream(Receiver::MajorGrants);
        let ecc = OutputKind::FundingStream(Receiver::Ecc);
        for (height, required, deferred) in [
            (9, None, 0),
            (10, Some((mg, 50_000_000, SCRIPT_A)), 75_000_000),
            (11, Some((mg, 50_000_000, SCRIPT_C)), 75_000_000),
            (17, Some((mg, 50_000_000, SCRIPT_B)), 75_000_000),
            (21, Some((mg, 50_000_000, SCRIPT_B)), 75_000_000),
            (22, None, 0),
            (23, Some((ecc, 43_750_000, SCRIPT_B)), 0),
            (28, Some((ecc, 43_750_000, SCRIPT_B)), 0),
            (29, None, 0),
        ] {
            let terms = at(&spec, height).unwrap();
            let required: Vec<RequiredOutput> = required
                .into_iter()
                .map(|(kind, value, script)| RequiredOutput {
                    kind,
                    value,
                    script,
                })
                .collect();
            let paid: u64 = required.iter().map(|output| output.value).sum();
            assert_eq!(terms.required, required, "{height}");
            assert_eq!(terms.subsidy.deferred, deferred, "{height}");
            assert_eq!(
                subsidy_schedule::block_subsidy(&spec, height),
                Ok(terms.subsidy),
                "{height}"
            );
            assert_eq!(terms.miner_subsidy, REGTEST_SUBSIDY - deferred - paid);
        }
        // Height 21: NU6, so the coinbase pays the exact value.
        let terms = at(&spec, 21).unwrap();
        let valid = outputs(&terms, 500_000_000);
        assert_eq!(check(&terms, &valid, 0), Ok(()));
        assert_eq!(
            check(&terms, &changed(&valid, 1, 0, MINER), 0),
            Err(CoinbaseError::MissingOutput {
                kind: mg,
                value: 50_000_000,
                script: SCRIPT_B.to_vec(),
            })
        );
        assert_eq!(
            check(&terms, &changed(&valid, 1, 49_999_999, &SCRIPT_B), 0),
            Err(CoinbaseError::WrongAmount {
                kind: mg,
                expected: 50_000_000,
                found: 49_999_999,
            })
        );
        // The script of another address period.
        assert_eq!(
            check(&terms, &changed(&valid, 1, 50_000_000, &SCRIPT_A), 0),
            Err(CoinbaseError::WrongScript {
                kind: mg,
                value: 50_000_000,
                expected: SCRIPT_B.to_vec(),
                found: SCRIPT_A.to_vec(),
            })
        );
        // The deferred part goes to no output.
        assert_eq!(
            check(&terms, &outputs(&terms, 575_000_000), 0),
            Err(CoinbaseError::ValueNotExact {
                paid: 625_000_000,
                required: 550_000_000,
            })
        );
        // Value that enters a shielded pool is paid value.
        let shielded = ShieldedBalances {
            sapling: -100,
            orchard: -20,
            ironwood: -3,
        };
        assert_eq!(
            terms.check(&outputs(&terms, 500_000_000 - 123), shielded, 0),
            Ok(())
        );
    }

    /// NU7 at 9 with the test reissuance height 12: the fee share, and from 12 the bonus of
    /// the balance after the parent.
    #[test]
    fn the_terms_with_the_nsm() {
        let mut spec = regtest();
        for upgrade in [
            Upgrade::Nu6,
            Upgrade::Nu6_1,
            Upgrade::Nu6_2,
            Upgrade::Nu6_3,
            Upgrade::Nu7,
        ] {
            spec.activation_heights[upgrade.index()] = Some(9);
        }
        // No disbursement at the NU6.1 height 9: a Regtest NU6.1 height has one in a test.
        static ONE: [Disbursement; 1] = [Disbursement {
            count: 1,
            value: 0,
            script: SCRIPT_A,
        }];
        spec.lockbox_disbursements = ONE.to_vec();
        spec.test_reissuance_height = Some(12);
        let spec = spec.checked().expect("a valid spec");
        let terms = at(&spec, 10).unwrap();
        assert!(terms.nsm_fee_share && terms.exact_value);
        // The NU7 era: a third of 625,000,000.
        assert_eq!(terms.subsidy.total, 208_333_333);
        assert_eq!(terms.miner_fees(1_001), Ok(401));
        let miner = terms.miner_subsidy + 401;
        assert_eq!(check(&terms, &outputs(&terms, miner), 1_001), Ok(()));
        let Err(CoinbaseError::ValueNotExact { .. }) =
            check(&terms, &outputs(&terms, terms.miner_subsidy + 1_001), 1_001)
        else {
            panic!("the fees above the miner share stay out of the coinbase");
        };
        // From the reissuance height the pools are necessary.
        assert_eq!(
            at(&spec, 12),
            Err(ConsensusError::IssuedSupplyUnknown { height: 12 })
        );
        assert_eq!(after(&spec, 11, 0), at(&spec, 11));
        let scheduled =
            u64::try_from(subsidy_schedule::scheduled_issuance(&spec, 11).unwrap()).unwrap();
        for (balance, bonus) in [(0, 0), (1, 1), (7_272_727, 1), (7_272_728, 2)] {
            let terms = after(&spec, 12, scheduled - balance).unwrap();
            assert_eq!(terms.subsidy.total, 208_333_333 + bonus, "{balance}");
            assert_eq!(terms.miner_subsidy, 208_333_333 + bonus);
        }
        assert_eq!(
            after(&spec, 12, scheduled + 1),
            Err(ConsensusError::NegativeNsmBalance {
                height: 11,
                scheduled: u128::from(scheduled),
                issued: scheduled + 1,
            })
        );
    }

    /// Fees above `MAX_MONEY` are not an amount: the check refuses them before the value
    /// rule.
    #[test]
    fn fees_above_max_money_are_an_error() {
        let terms = at(&regtest(), 1).unwrap();
        assert_eq!(
            check(&terms, &outputs(&terms, 1), MAX_MONEY + 1),
            Err(CoinbaseError::Consensus(ConsensusError::MoneyOverflow))
        );
    }
}
