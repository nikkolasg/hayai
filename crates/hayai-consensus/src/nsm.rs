//! The Network Sustainability Mechanism (NSM) of NU7 (ZIP 235, ZIP 237): the seeds of
//! Mainnet and Testnet, and the wrappers of `hayai_consensus_core::nsm` for a caller with
//! a [`Network`]. The rules are in the core.

use hayai_consensus_core::nsm as core;
pub use hayai_consensus_core::nsm::{miner_fee_share, reissuance_bonus};

use crate::network::checked;
use crate::{ConsensusError, Network};

/// `INITIAL_NSM_VALUE_BALANCE` of Mainnet (Zakura `subsidy/constants/mainnet.rs:44`).
pub(crate) const MAINNET_SEED: u64 = 36_858_445_520;
/// `INITIAL_NSM_VALUE_BALANCE` of Testnet (Zakura `subsidy/constants/testnet.rs:27`).
pub(crate) const TESTNET_SEED: u64 = 55_768_414_957;

/// The NSM value balance that the block before NU7 must give on `network`
/// (`INITIAL_NSM_VALUE_BALANCE`, [`crate::ChainSpec::nsm_seed`]). `None` on Regtest: the
/// balance there is the balance that the chain gives.
pub fn expected_seed(network: Network) -> Option<u64> {
    core::expected_seed(network.core())
}

/// The NSM value balance after the block at `height`, whose chain value pools hold
/// `issued` zatoshis in total (`hayai_consensus_core::nsm::balance`).
pub fn balance(network: Network, height: u32, issued: u64) -> Result<u64, ConsensusError> {
    core::balance(network.core(), height, issued)
}

/// The NSM rules of the block at `height`, after which the chain value pools hold `issued`
/// zatoshis in total (`hayai_consensus_core::nsm::check_balance`).
pub fn check_balance(network: Network, height: u32, issued: u64) -> Result<(), ConsensusError> {
    core::check_balance(network.core(), height, issued)
}

/// The first height with the NSM reissuance on `network`
/// (`hayai_consensus_core::nsm::reissuance_height`). `None` when the network has no NU7
/// height or no such height.
pub fn reissuance_height(network: Network) -> Option<u32> {
    checked(core::reissuance_height(network.core()))
}

/// Whether the NSM reissuance is active at `height` on `network` (Zakura
/// `is_zip234_active`).
pub fn reissuance_active(network: Network, height: u32) -> bool {
    checked(core::reissuance_active(network.core(), height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{subsidy, Upgrade, MAX_MONEY};

    /// ZIP 237: `BLOCK_SUBSIDY_FRACTION` is `floor(LN2_SCALED / HalvingInterval)` over
    /// 10^10. From NU7 the interval is 3 post-Blossom intervals (5,040,000 blocks), and the
    /// numerator is 1,375. The 75 s interval gives the 4,126 of ZIP 234.
    #[test]
    fn the_reissuance_fraction_is_the_one_of_zip_237() {
        const LN2_SCALED: u128 = 6_931_680_000;
        let interval = Network::Testnet
            .core()
            .post_blossom_halving_interval()
            .unwrap();
        assert_eq!(interval, 1_680_000);
        assert_eq!(
            LN2_SCALED / u128::from(3 * interval),
            core::REISSUANCE_NUMERATOR
        );
        assert_eq!(LN2_SCALED / u128::from(interval), 4_126);
        assert_eq!(core::REISSUANCE_DENOMINATOR, 10_000_000_000);
        assert_eq!(reissuance_bonus(MAX_MONEY), Ok(288_750_000));
        assert_eq!(miner_fee_share(1_000), Ok(400));
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
        assert_eq!(expected_seed(network), Some(seed));
        assert_eq!(expected_seed(Network::Mainnet), Some(MAINNET_SEED));
        assert_eq!(expected_seed(Network::Regtest), None);
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
            hayai_consensus_core::subsidy_schedule::halving_height(
                Network::Testnet.core(),
                3,
                u32::MAX / 2
            ),
            Ok(Some(third))
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
