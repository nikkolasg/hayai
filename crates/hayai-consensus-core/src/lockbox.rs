//! The deferred pool (lockbox): what a block adds to it and takes from it.
//!
//! From NU6 a funding stream pays a share of the block subsidy to the deferred pool
//! (ZIP 1015, ZIP 2001; [`crate::subsidy_schedule::Subsidy::deferred`]). The coinbase of the NU6.1
//! activation block takes 78,750 ZEC out of the pool in ten equal outputs (ZIP 271,
//! ZIP 1016). Zakura checks the same outputs (`zakura-consensus/src/block/check.rs:
//! 268-290`). The disbursements of a chain are data of its spec
//! ([`CoreSpec::lockbox_disbursements`]).

use crate::chain_spec::SpecError;
use crate::{add_money, ConsensusError, CoreSpec, P2shScript, Upgrade, MAX_MONEY};

/// Lockbox disbursement outputs of one coinbase: `count` outputs of `value` zatoshis each
/// to `script`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Disbursement {
    /// `ZIP271DisbursementChunks`.
    pub count: usize,
    /// `ZIP271DisbursementAmount / ZIP271DisbursementChunks` in zatoshis.
    pub value: u64,
    /// The script of `ZIP271DisbursementAddress`, a P2SH address.
    pub script: P2shScript,
}

impl Disbursement {
    /// The value that all outputs take out of the deferred pool.
    pub fn total(&self) -> Result<u64, ConsensusError> {
        let Ok(count) = u64::try_from(self.count) else {
            return Err(ConsensusError::MoneyOverflow);
        };
        let Some(total) = self.value.checked_mul(count) else {
            return Err(ConsensusError::MoneyOverflow);
        };
        if total > MAX_MONEY {
            return Err(ConsensusError::MoneyOverflow);
        }
        Ok(total)
    }
}

/// Checks the disbursements of a spec, as Zakura does for the disbursements of its Regtest
/// parameters (`check_lockbox_disbursements`): the sum of the values is a valid amount of
/// money.
pub fn check_disbursements(disbursements: &[Disbursement]) -> Result<(), SpecError> {
    let mut total = 0u64;
    for i in 0..disbursements.len() {
        let Ok(value) = disbursements[i].total() else {
            return Err(SpecError::DisbursementAmount);
        };
        let Ok(sum) = add_money(total, value) else {
            return Err(SpecError::DisbursementAmount);
        };
        total = sum;
    }
    Ok(())
}

/// The lockbox disbursements that the coinbase at `height` must pay. Empty at every height
/// but the NU6.1 activation height, and empty at that height on a chain without a
/// disbursement.
///
/// Spec §7.10: [NU6.1 onward] the disbursement outputs are in the block at
/// `ZIP271ActivationHeight` only.
pub fn disbursements(spec: &CoreSpec, height: u32) -> &[Disbursement] {
    if spec.activation_height(Upgrade::Nu6_1) != Some(height) {
        return &[];
    }
    &spec.lockbox_disbursements
}

/// The deferred pool after a block that adds `deferred` zatoshis and pays `disbursed`
/// zatoshis out of the pool, from a pool of `before` zatoshis. `None` when the block pays
/// out more than the pool holds, or when the pool would be above `MAX_MONEY`.
///
/// ZIP 2001: the deferred pool gains `totalDeferredOutput`. ZIP 271: it loses
/// `totalDeferredInput` and must not become negative. The check is on the pool after the
/// block, as in Spec §4.17 and in Zakura. ZIP 271 orders the deduction before the gain.
pub fn deferred_pool_after(before: u64, deferred: u64, disbursed: u64) -> Option<u64> {
    let Ok(gained) = add_money(before, deferred) else {
        return None;
    };
    gained.checked_sub(disbursed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain_spec::tests::regtest;
    use crate::funding::tests::SCRIPT_A;

    #[test]
    fn the_disbursement_is_in_the_nu6_1_activation_block_only() {
        static ONE: [Disbursement; 1] = [Disbursement {
            count: 10,
            value: 787_500_000_000,
            script: SCRIPT_A,
        }];
        let mut spec = regtest();
        spec.activation_heights[Upgrade::Nu6.index()] = Some(20);
        spec.activation_heights[Upgrade::Nu6_1.index()] = Some(30);
        spec.lockbox_disbursements = ONE.to_vec();
        assert_eq!(disbursements(&spec, 29), &[]);
        assert_eq!(disbursements(&spec, 31), &[]);
        assert_eq!(disbursements(&spec, 30), &ONE);
        assert_eq!(ONE[0].total(), Ok(78_750 * 100_000_000));
        assert_eq!(disbursements(&regtest(), 30), &[]);
    }

    #[test]
    fn the_pool_grows_by_the_deferred_part_and_shrinks_by_the_disbursement() {
        assert_eq!(deferred_pool_after(5, 7, 0), Some(12));
        assert_eq!(deferred_pool_after(5, 7, 12), Some(0));
        assert_eq!(deferred_pool_after(5, 7, 13), None);
        assert_eq!(deferred_pool_after(u64::MAX, 1, 1), None);
        assert_eq!(deferred_pool_after(MAX_MONEY, 1, 1), None);
        assert_eq!(deferred_pool_after(MAX_MONEY, 0, 1), Some(MAX_MONEY - 1));
    }

    #[test]
    fn the_disbursement_amounts_are_checked() {
        let one = |count, value| Disbursement {
            count,
            value,
            script: SCRIPT_A,
        };
        assert_eq!(
            check_disbursements(&[one(1, MAX_MONEY - 1), one(1, 1)]),
            Ok(())
        );
        assert_eq!(
            check_disbursements(&[one(1, MAX_MONEY), one(1, 1)]),
            Err(SpecError::DisbursementAmount)
        );
        assert_eq!(
            check_disbursements(&[one(2, u64::MAX / 2 + 1)]),
            Err(SpecError::DisbursementAmount)
        );
        assert_eq!(
            one(2, u64::MAX / 2 + 1).total(),
            Err(ConsensusError::MoneyOverflow)
        );
        assert_eq!(
            one(1, MAX_MONEY + 1).total(),
            Err(ConsensusError::MoneyOverflow)
        );
    }
}
