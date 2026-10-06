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

use hayai_crypto::zcash_protocol::value::MAX_MONEY;

use crate::{subsidy, ConsensusError, Network, Upgrade};

/// The largest height of Zakura (`Height::MAX`, `u32::MAX / 2`). The search for the
/// reissuance height ends there.
const MAX_HEIGHT: u32 = u32::MAX / 2;
/// `BLOCK_SUBSIDY_FRACTION` of the reissuance: 1,375 / 10,000,000,000 of the balance for
/// each block (Zakura `subsidy.rs:586,591`).
///
/// ZIP 237: `BLOCK_SUBSIDY_FRACTION` from NU7 is `floor(LN2_SCALED / 5,040,000) / 10^10`.
/// The reissuance height is at or above NU7, so the fraction is a constant.
const REISSUANCE_NUMERATOR: u128 = 1_375;
const REISSUANCE_DENOMINATOR: u128 = 10_000_000_000;
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
pub const fn miner_fee_share(fees: u64) -> u64 {
    fees - (fees as u128 * 6 / 10) as u64
}

/// The NSM value balance that the block before NU7 must give on `network`
/// (`INITIAL_NSM_VALUE_BALANCE`; Zakura `subsidy/constants/mainnet.rs:44`,
/// `subsidy/constants/testnet.rs:27`). `None` on Regtest: the balance there is the
/// balance that the chain gives.
///
/// ZIP 237: `NSMValueBalance(NU7ActivationHeight - 1)` is `INITIAL_NSM_VALUE_BALANCE`.
pub const fn expected_seed(network: Network) -> Option<u64> {
    match network {
        Network::Mainnet => Some(36_858_445_520),
        Network::Testnet => Some(55_768_414_957),
        Network::Regtest | Network::ConfiguredRegtest(_) => None,
    }
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
pub fn balance(network: Network, height: u32, issued: u64) -> Result<u64, ConsensusError> {
    let scheduled = subsidy::scheduled_issuance(network, height);
    scheduled
        .checked_sub(u128::from(issued))
        .and_then(|balance| u64::try_from(balance).ok())
        .filter(|balance| *balance <= MAX_MONEY)
        .ok_or(ConsensusError::NegativeNsmBalance {
            height,
            scheduled,
            issued,
        })
}

/// The NSM rules of the block at `height`, after which the chain value pools hold `issued`
/// zatoshis in total.
///
/// - The block before NU7 on a network with [`expected_seed`]: the balance is the seed.
/// - A block from NU7: the balance is not negative.
/// - Every other block has no NSM rule.
///
/// ZIP 237: [NU7 onward] a block that makes `NSMValueBalance` negative is not valid.
pub fn check_balance(network: Network, height: u32, issued: u64) -> Result<(), ConsensusError> {
    let Some(nu7) = network.activation_height(Upgrade::Nu7) else {
        return Ok(());
    };
    if height >= nu7 {
        balance(network, height, issued)?;
    } else if height + 1 == nu7 {
        if let Some(expected) = expected_seed(network) {
            let found = balance(network, height, issued)?;
            if found != expected {
                return Err(ConsensusError::NsmSeedMismatch { expected, found });
            }
        }
    }
    Ok(())
}

/// The first height with the NSM reissuance on `network`. `None` when the network has no
/// NU7 height or no such height.
///
/// The height is the first one after the third halving and before the fourth at which
/// the reissuance bonus of a chain with no value out of circulation is below the subsidy
/// of the halving schedule (Zakura `nsm_reissuance_crossing_height` and
/// `first_nsm_crossing_in_subsidy_run`, `subsidy.rs:609-684`). It depends on the network
/// parameters only. A Regtest configuration of a test can name the height
/// (`RegtestConfig::with_test_reissuance_height`).
///
/// ZIP 237: `DEPLOYMENT_BLOCK_HEIGHT` (ZIP 259 `NSM_REISSUANCE_HEIGHT`), the first `h` in
/// `max(A, H_3 + 1) <= h < H_4` with `ceil(1,375 * (MAX_MONEY - S_A(h - 1)) / 10^10) <
/// B_A(h)`, by the closed form of the ZIP.
pub fn reissuance_height(network: Network) -> Option<u32> {
    let nu7 = network.activation_height(Upgrade::Nu7)?;
    if let Network::ConfiguredRegtest(config) = network {
        if let Some(height) = config.test_reissuance_height() {
            return Some(height.max(nu7));
        }
    }
    let third = subsidy::halving_height(network, REISSUANCE_HALVING, MAX_HEIGHT)?;
    let run_end = match subsidy::halving_height(network, REISSUANCE_HALVING + 1, MAX_HEIGHT) {
        Some(fourth) => fourth - 1,
        None => MAX_HEIGHT,
    };
    let first = (third + 1).max(nu7);
    let subsidy = u128::from(subsidy::total_subsidy(network, first));
    if first > run_end || subsidy == 0 {
        return None;
    }
    // `ceil(fraction * reserve) < subsidy` holds when the reserve is at most this value.
    let max_reserve = (subsidy - 1) * REISSUANCE_DENOMINATOR / REISSUANCE_NUMERATOR;
    let reserve =
        u128::from(MAX_MONEY).saturating_sub(subsidy::scheduled_issuance(network, first - 1));
    let blocks = reserve.saturating_sub(max_reserve).div_ceil(subsidy);
    let height = u128::from(first) + blocks;
    u32::try_from(height)
        .ok()
        .filter(|height| *height <= run_end)
}

/// Whether the NSM reissuance is active at `height` on `network` (Zakura
/// `is_zip234_active`).
pub fn reissuance_active(network: Network, height: u32) -> bool {
    matches!(reissuance_height(network), Some(start) if height >= start)
}

/// The reissuance bonus of a block whose parent leaves an NSM value balance of `balance`
/// zatoshis: `ceil(balance * 1,375 / 10,000,000,000)` (Zakura `reissuance_bonus`).
///
/// ZIP 237: `AdditionalBlockSubsidy(height)` is the ceiling of `BLOCK_SUBSIDY_FRACTION`
/// times `NSMValueBalance(height - 1)`.
pub const fn reissuance_bonus(balance: u64) -> u64 {
    (balance as u128 * REISSUANCE_NUMERATOR).div_ceil(REISSUANCE_DENOMINATOR) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert_eq!(miner_fee_share(fees), miner, "{fees}");
        }
        assert_eq!(
            miner_fee_share(u64::MAX),
            u64::MAX - (u64::MAX / 10 * 6 + 3)
        );
    }

    /// ZIP 237: `BLOCK_SUBSIDY_FRACTION` is `floor(LN2_SCALED / HalvingInterval)` over
    /// 10^10. From NU7 the interval is 3 post-Blossom intervals (5,040,000 blocks), and the
    /// numerator is 1,375. The 75 s interval gives the 4,126 of ZIP 234.
    #[test]
    fn the_reissuance_fraction_is_the_one_of_zip_237() {
        const LN2_SCALED: u128 = 6_931_680_000;
        let interval = Network::Testnet.params().post_blossom_halving_interval();
        assert_eq!(interval, 1_680_000);
        assert_eq!(LN2_SCALED / u128::from(3 * interval), REISSUANCE_NUMERATOR);
        assert_eq!(LN2_SCALED / u128::from(interval), 4_126);
        assert_eq!(REISSUANCE_DENOMINATOR, 10_000_000_000);
    }

    #[test]
    fn the_bonus_rounds_up() {
        assert_eq!(reissuance_bonus(0), 0);
        assert_eq!(reissuance_bonus(1), 1);
        assert_eq!(reissuance_bonus(7_272_727), 1);
        assert_eq!(reissuance_bonus(7_272_728), 2);
        assert_eq!(reissuance_bonus(10_000_000_000), 1_375);
        assert_eq!(reissuance_bonus(10_000_000_001), 1_376);
        assert_eq!(reissuance_bonus(MAX_MONEY), 288_750_000);
    }

    /// The balance is the scheduled issuance minus the pools, at each height.
    #[test]
    fn the_balance_is_the_scheduled_issuance_minus_the_pools() {
        let network = Network::Testnet;
        let scheduled = subsidy::scheduled_issuance(network, 4_465_025);
        let Ok(scheduled) = u64::try_from(scheduled) else {
            panic!("the Testnet schedule is below MAX_MONEY");
        };
        assert_eq!(balance(network, 4_465_025, scheduled), Ok(0));
        assert_eq!(balance(network, 4_465_025, scheduled - 7), Ok(7));
        assert_eq!(
            balance(network, 4_465_025, scheduled + 1),
            Err(ConsensusError::NegativeNsmBalance {
                height: 4_465_025,
                scheduled: u128::from(scheduled),
                issued: scheduled + 1,
            })
        );
    }

    /// Testnet: the seed rule at NU7 - 1, the non-negative rule from NU7, no rule before.
    #[test]
    fn the_balance_rules_start_at_the_block_before_nu7() {
        let network = Network::Testnet;
        let nu7 = 4_465_026;
        let seed = 55_768_414_957;
        let issued = |height| {
            let Ok(scheduled) = u64::try_from(subsidy::scheduled_issuance(network, height)) else {
                panic!("the Testnet schedule is below MAX_MONEY");
            };
            scheduled - seed
        };
        // Before the seed block the pools have no NSM rule.
        assert_eq!(check_balance(network, nu7 - 2, u64::MAX), Ok(()));
        // The seed block: the balance is the constant.
        assert_eq!(check_balance(network, nu7 - 1, issued(nu7 - 1)), Ok(()));
        assert_eq!(
            check_balance(network, nu7 - 1, issued(nu7 - 1) + 1),
            Err(ConsensusError::NsmSeedMismatch {
                expected: seed,
                found: seed - 1,
            })
        );
        // From NU7: any balance at or above 0.
        for height in [nu7, nu7 + 1] {
            assert_eq!(check_balance(network, height, issued(height)), Ok(()));
            assert_eq!(check_balance(network, height, 0), Ok(()));
            let all = issued(height) + seed;
            assert_eq!(check_balance(network, height, all), Ok(()));
            let Err(ConsensusError::NegativeNsmBalance { .. }) =
                check_balance(network, height, all + 1)
            else {
                panic!("a negative balance at {height}");
            };
        }
        // A network without an NU7 height has no NSM rule.
        assert_eq!(check_balance(Network::Mainnet, 3_500_000, u64::MAX), Ok(()));
        assert_eq!(check_balance(Network::Regtest, 5, u64::MAX), Ok(()));
    }

    /// The reissuance height of each network. The Testnet value is the value of Zakura's
    /// `nsm_reissuance_height` (the test `conformance_subsidy` of hayai-bench compares
    /// them).
    #[test]
    fn the_reissuance_height_of_each_network() {
        assert_eq!(reissuance_height(Network::Mainnet), None);
        assert_eq!(reissuance_height(Network::Regtest), None);
        let Some(start) = reissuance_height(Network::Testnet) else {
            panic!("Testnet has a reissuance height");
        };
        let third = 4_465_026 + 3 * (4_476_000 - 4_465_026);
        assert_eq!(
            subsidy::halving_height(Network::Testnet, 3, MAX_HEIGHT),
            Some(third)
        );
        assert_eq!(start, 7_305_222);
        assert!(start > third && start < third + 3 * 1_680_000, "{start}");
        assert!(!reissuance_active(Network::Testnet, start - 1));
        assert!(reissuance_active(Network::Testnet, start));
    }

    /// The reissuance height of a test configuration (Zakura `nsm_reissuance_height`,
    /// `subsidy.rs:697,708-713`): the configured height, at least the NU7 height, and
    /// none without an NU7 height.
    #[test]
    fn a_test_configuration_names_the_reissuance_height() {
        use crate::RegtestConfig;
        let network = |nu7: Option<u32>, reissuance: Option<u32>| {
            let heights: Vec<(Upgrade, u32)> = nu7.map(|h| (Upgrade::Nu7, h)).into_iter().collect();
            let config = RegtestConfig::new(&heights, Vec::new(), 0).expect("valid");
            match reissuance {
                Some(height) => config.with_test_reissuance_height(height).network(),
                None => config.network(),
            }
        };
        assert_eq!(reissuance_height(network(Some(9), None)), None);
        assert_eq!(reissuance_height(network(Some(9), Some(12))), Some(12));
        assert_eq!(reissuance_height(network(Some(9), Some(9))), Some(9));
        assert_eq!(reissuance_height(network(Some(9), Some(4))), Some(9));
        assert_eq!(reissuance_height(network(None, Some(12))), None);
        let regtest = network(Some(9), Some(12));
        assert!(!reissuance_active(regtest, 11));
        assert!(reissuance_active(regtest, 12));
    }
}
