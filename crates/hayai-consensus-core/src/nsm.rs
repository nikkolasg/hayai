//! The Network Sustainability Mechanism (NSM) of NU7: ZIP 235 and ZIP 237, which ZIP 259
//! deploys. NU7 does not deploy ZIP 233 or ZIP 234.
//!
//! - Fee share (ZIP 235; Zakura `zakura-chain/src/parameters/network/subsidy/fees.rs`):
//!   from NU7 the coinbase gets the fees of the block minus `floor(6 * fees / 10)`. The
//!   rest stays out of the chain value pools.
//! - NSM value balance (ZIP 237; Zakura `Block::nsm_value_balance_change`,
//!   `zakura-chain/src/block.rs:359-416`): the value that the halving schedule issued and
//!   that the chain value pools do not hold. Zakura sets the balance in the block before
//!   NU7 to the scheduled issuance minus the total of the pools, and each later block adds
//!   its scheduled subsidy minus its change of the total. The sum of these steps is
//!   [`balance`]: the scheduled issuance up to the height minus the total of the pools.
//!   hayai computes the balance from the pools and stores no value for it.
//! - Seed check (Zakura `ValueBalance::initial_nsm_value_balance`,
//!   `zakura-chain/src/value_balance.rs:377-414`): on Mainnet and Testnet the balance in
//!   the block before NU7 must be the constant of the network ([`expected_seed`]).
//! - Non-negative balance (ZIP 237; Zakura `nsm_value_balance_is_non_negative`,
//!   `zakura-state/src/service/check.rs:46-98`): from NU7 a block that makes the balance
//!   negative is not valid.
//! - Reissuance (ZIP 237; Zakura `subsidy.rs:565-797`): from [`reissuance_height`] the
//!   block subsidy is the subsidy of the halving schedule plus [`reissuance_bonus`] of the
//!   balance after the parent block.

use crate::{money, sub_money, subsidy_schedule, ConsensusError, CoreSpec, Upgrade, MAX_MONEY};

/// The largest height of Zakura (`Height::MAX`, `u32::MAX / 2`). The search for the
/// reissuance height ends there.
const MAX_HEIGHT: u32 = u32::MAX / 2;
/// `BLOCK_SUBSIDY_FRACTION` of the reissuance: 1,375 / 10,000,000,000 of the balance for
/// each block (Zakura `subsidy.rs:586,591`).
///
/// ZIP 237: `BLOCK_SUBSIDY_FRACTION` from NU7 is `floor(LN2_SCALED / 5,040,000) / 10^10`.
/// The reissuance height is at or above NU7, so the fraction is a constant.
pub const REISSUANCE_NUMERATOR: u128 = 1_375;
pub const REISSUANCE_DENOMINATOR: u128 = 10_000_000_000;
/// The reissuance starts in the era of this halving index (Zakura
/// `NSM_REISSUANCE_START_HALVING`).
///
/// ZIP 237: the reissuance height is after `H_3`, the first height of halving 3.
const REISSUANCE_HALVING: u32 = 3;

/// The part of `fees`, the total fees of a block, that the coinbase of a block with the
/// NSM fee share gets: `fees - floor(6 * fees / 10)`. The division rounds one time, on
/// the total.
///
/// ZIP 235: `MinerFees = TransactionFees - NSMFeeContribution`, with
/// `NSMFeeContribution = floor(6 * TransactionFees / 10)` on the aggregate fees.
pub fn miner_fee_share(fees: u64) -> Result<u64, ConsensusError> {
    let fees = money(fees)?;
    let Some(scaled) = u128::from(fees).checked_mul(6) else {
        return Err(ConsensusError::MoneyOverflow);
    };
    let Ok(contribution) = u64::try_from(scaled / 10) else {
        return Err(ConsensusError::MoneyOverflow);
    };
    sub_money(fees, contribution)
}

/// The NSM value balance that the block before NU7 must give (`INITIAL_NSM_VALUE_BALANCE`,
/// [`CoreSpec::nsm_seed`]). `None`: the balance is the balance that the chain gives.
///
/// ZIP 237: `NSMValueBalance(NU7ActivationHeight - 1)` is `INITIAL_NSM_VALUE_BALANCE`.
pub fn expected_seed(spec: &CoreSpec) -> Option<u64> {
    spec.nsm_seed
}

/// The NSM value balance after the block at `height`, whose chain value pools hold
/// `issued` zatoshis in total: the scheduled issuance up to `height` minus `issued`.
///
/// It fails with [`ConsensusError::NegativeNsmBalance`] when the pools hold more than the
/// schedule issued, or when the balance is above `MAX_MONEY` (Zakura holds the balance in
/// an amount).
///
/// ZIP 237: `NSMValueBalance(height)`. The recursion of the ZIP sums to this closed form
/// from `NU7ActivationHeight - 1`, because each block changes the pools by
/// `ScheduledBlockSubsidy + AdditionalBlockSubsidy - removed` (ZIP 235, ZIP 236).
pub fn balance(spec: &CoreSpec, height: u32, issued: u64) -> Result<u64, ConsensusError> {
    let scheduled = subsidy_schedule::scheduled_issuance(spec, height)?;
    let negative = ConsensusError::NegativeNsmBalance {
        height,
        scheduled,
        issued,
    };
    let Some(balance) = scheduled.checked_sub(u128::from(issued)) else {
        return Err(negative);
    };
    let Ok(balance) = u64::try_from(balance) else {
        return Err(negative);
    };
    if balance > MAX_MONEY {
        return Err(negative);
    }
    Ok(balance)
}

/// The NSM rules of the block at `height`, after which the chain value pools hold `issued`
/// zatoshis in total.
///
/// - The block before NU7 on a chain with [`expected_seed`]: the balance is the seed.
/// - A block from NU7: the balance is not negative.
/// - Every other block has no NSM rule.
///
/// ZIP 237: [NU7 onward] a block that makes `NSMValueBalance` negative is not valid.
pub fn check_balance(spec: &CoreSpec, height: u32, issued: u64) -> Result<(), ConsensusError> {
    let Some(nu7) = spec.activation_height(Upgrade::Nu7) else {
        return Ok(());
    };
    if height >= nu7 {
        balance(spec, height, issued)?;
        return Ok(());
    }
    let Some(next) = height.checked_add(1) else {
        return Err(ConsensusError::Overflow);
    };
    if next == nu7 {
        if let Some(expected) = expected_seed(spec) {
            let found = balance(spec, height, issued)?;
            if found != expected {
                return Err(ConsensusError::NsmSeedMismatch { expected, found });
            }
        }
    }
    Ok(())
}

/// The first height with the NSM reissuance. `None` when the chain has no NU7 height or no
/// such height.
///
/// The height is the first one after the third halving and before the fourth at which
/// the reissuance bonus of a chain with no value out of circulation is below the subsidy
/// of the halving schedule (Zakura `nsm_reissuance_crossing_height` and
/// `first_nsm_crossing_in_subsidy_run`, `subsidy.rs:609-684`). It depends on the spec
/// only. A test spec can name the height ([`CoreSpec::test_reissuance_height`]).
///
/// ZIP 237: `DEPLOYMENT_BLOCK_HEIGHT` (ZIP 259 `NSM_REISSUANCE_HEIGHT`), the first `h` in
/// `max(A, H_3 + 1) <= h < H_4` with `ceil(1,375 * (MAX_MONEY - S_A(h - 1)) / 10^10) <
/// B_A(h)`, by the closed form of the ZIP.
pub fn reissuance_height(spec: &CoreSpec) -> Result<Option<u32>, ConsensusError> {
    let Some(nu7) = spec.activation_height(Upgrade::Nu7) else {
        return Ok(None);
    };
    if let Some(height) = spec.test_reissuance_height {
        return Ok(Some(height.max(nu7)));
    }
    let Some(third) = subsidy_schedule::halving_height(spec, REISSUANCE_HALVING, MAX_HEIGHT)?
    else {
        return Ok(None);
    };
    let run_end = match subsidy_schedule::halving_height(spec, REISSUANCE_HALVING + 1, MAX_HEIGHT)?
    {
        Some(fourth) => fourth - 1,
        None => MAX_HEIGHT,
    };
    let first = (third + 1).max(nu7);
    let subsidy = u128::from(subsidy_schedule::scheduled_subsidy(spec, first)?);
    if first > run_end || subsidy == 0 {
        return Ok(None);
    }
    // `ceil(fraction * reserve) < subsidy` holds when the reserve is at most this value.
    let Some(max_reserve) = (subsidy - 1).checked_mul(REISSUANCE_DENOMINATOR) else {
        return Err(ConsensusError::Overflow);
    };
    let max_reserve = max_reserve / REISSUANCE_NUMERATOR;
    let reserve = u128::from(MAX_MONEY)
        .saturating_sub(subsidy_schedule::scheduled_issuance(spec, first - 1)?);
    let excess = reserve.saturating_sub(max_reserve);
    // `ceil(excess / subsidy)`.
    let Some(rounded) = excess.checked_add(subsidy - 1) else {
        return Err(ConsensusError::Overflow);
    };
    let blocks = rounded / subsidy;
    let Some(height) = u128::from(first).checked_add(blocks) else {
        return Err(ConsensusError::Overflow);
    };
    let Ok(height) = u32::try_from(height) else {
        return Ok(None);
    };
    if height > run_end {
        return Ok(None);
    }
    Ok(Some(height))
}

/// Whether the NSM reissuance is active at `height` (Zakura `is_zip234_active`).
pub fn reissuance_active(spec: &CoreSpec, height: u32) -> Result<bool, ConsensusError> {
    match reissuance_height(spec)? {
        Some(start) => Ok(height >= start),
        None => Ok(false),
    }
}

/// The reissuance bonus of a block whose parent leaves an NSM value balance of `balance`
/// zatoshis: `ceil(balance * 1,375 / 10,000,000,000)` (Zakura `reissuance_bonus`).
///
/// ZIP 237: `AdditionalBlockSubsidy(height)` is the ceiling of `BLOCK_SUBSIDY_FRACTION`
/// times `NSMValueBalance(height - 1)`.
pub fn reissuance_bonus(balance: u64) -> Result<u64, ConsensusError> {
    let balance = money(balance)?;
    let Some(scaled) = u128::from(balance).checked_mul(REISSUANCE_NUMERATOR) else {
        return Err(ConsensusError::MoneyOverflow);
    };
    let Some(rounded) = scaled.checked_add(REISSUANCE_DENOMINATOR - 1) else {
        return Err(ConsensusError::MoneyOverflow);
    };
    let Ok(bonus) = u64::try_from(rounded / REISSUANCE_DENOMINATOR) else {
        return Err(ConsensusError::MoneyOverflow);
    };
    Ok(bonus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain_spec::tests::regtest;

    /// The division rounds down, so the miner gets the remainder. The test
    /// `conformance_nu7` of hayai-bench compares the function with Zakura's
    /// `miner_fee_share`.
    #[test]
    fn the_miner_gets_the_fees_minus_six_tenths_rounded_down() {
        for (fees, miner) in [
            (0, 0),
            (1, 1),
            (2, 1),
            (3, 2),
            (4, 2),
            (5, 2),
            (9, 4),
            (10, 4),
            (11, 5),
            (1_000, 400),
            (MAX_MONEY, MAX_MONEY - MAX_MONEY / 10 * 6),
        ] {
            assert_eq!(miner_fee_share(fees), Ok(miner), "{fees}");
        }
        // Fees above MAX_MONEY are not an amount.
        assert_eq!(
            miner_fee_share(MAX_MONEY + 1),
            Err(ConsensusError::MoneyOverflow)
        );
        assert_eq!(
            miner_fee_share(u64::MAX),
            Err(ConsensusError::MoneyOverflow)
        );
    }

    #[test]
    fn the_bonus_rounds_up() {
        assert_eq!(reissuance_bonus(0), Ok(0));
        assert_eq!(reissuance_bonus(1), Ok(1));
        assert_eq!(reissuance_bonus(7_272_727), Ok(1));
        assert_eq!(reissuance_bonus(7_272_728), Ok(2));
        assert_eq!(reissuance_bonus(10_000_000_000), Ok(1_375));
        assert_eq!(reissuance_bonus(10_000_000_001), Ok(1_376));
        assert_eq!(reissuance_bonus(MAX_MONEY), Ok(288_750_000));
        assert_eq!(
            reissuance_bonus(MAX_MONEY + 1),
            Err(ConsensusError::MoneyOverflow)
        );
    }

    /// Regtest with NU6 to NU7 at height 9 and the test reissuance height 12.
    fn regtest_nu7(reissuance: Option<u32>) -> CoreSpec {
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
        spec.test_reissuance_height = reissuance;
        spec
    }

    /// The reissuance height of a test configuration (Zakura `nsm_reissuance_height`,
    /// `subsidy.rs:697,708-713`): the configured height, at least the NU7 height, and
    /// none without an NU7 height.
    #[test]
    fn a_test_configuration_names_the_reissuance_height() {
        assert_eq!(reissuance_height(&regtest_nu7(None)), Ok(None));
        assert_eq!(reissuance_height(&regtest_nu7(Some(12))), Ok(Some(12)));
        assert_eq!(reissuance_height(&regtest_nu7(Some(9))), Ok(Some(9)));
        assert_eq!(reissuance_height(&regtest_nu7(Some(4))), Ok(Some(9)));
        let mut no_nu7 = regtest();
        no_nu7.test_reissuance_height = Some(12);
        assert_eq!(reissuance_height(&no_nu7), Ok(None));
        let spec = regtest_nu7(Some(12));
        assert_eq!(reissuance_active(&spec, 11), Ok(false));
        assert_eq!(reissuance_active(&spec, 12), Ok(true));
    }

    /// Regtest NU7 at 9: the seed rule has no seed, the non-negative rule holds from NU7.
    #[test]
    fn the_balance_rules_on_a_regtest_with_nu7() {
        let mut spec = regtest_nu7(None);
        let scheduled = |height| {
            u64::try_from(subsidy_schedule::scheduled_issuance(&regtest_nu7(None), height).unwrap())
                .unwrap()
        };
        assert_eq!(check_balance(&spec, 7, u64::MAX), Ok(()));
        assert_eq!(check_balance(&spec, 8, u64::MAX), Ok(()));
        assert_eq!(balance(&spec, 9, scheduled(9) - 7), Ok(7));
        assert_eq!(check_balance(&spec, 9, scheduled(9)), Ok(()));
        assert_eq!(
            check_balance(&spec, 9, scheduled(9) + 1),
            Err(ConsensusError::NegativeNsmBalance {
                height: 9,
                scheduled: u128::from(scheduled(9)),
                issued: scheduled(9) + 1,
            })
        );
        // With a seed, the block before NU7 must give it.
        spec.nsm_seed = Some(1_000);
        assert_eq!(check_balance(&spec, 8, scheduled(8) - 1_000), Ok(()));
        assert_eq!(
            check_balance(&spec, 8, scheduled(8) - 999),
            Err(ConsensusError::NsmSeedMismatch {
                expected: 1_000,
                found: 999,
            })
        );
        assert_eq!(check_balance(&regtest(), 5, u64::MAX), Ok(()));
        assert_eq!(check_balance(&spec, u32::MAX, 0), Ok(()));
    }
}
