//! The block subsidy schedule (protocol specification §7.8, ZIP 208, ZIP 218): the
//! wrappers of `hayai_consensus_core::subsidy_schedule` for a caller with a [`Network`]. The
//! schedule is in the core.

use hayai_consensus_core::subsidy_schedule as core;
pub use hayai_consensus_core::subsidy_schedule::Subsidy;

use crate::network::checked;
use crate::{rules_at, ConsensusError, Network};

/// The block subsidy at `height` on `network` (`hayai_consensus_core::subsidy_schedule::block_subsidy`).
///
/// It fails with [`ConsensusError::UnsupportedUpgrade`] when the upgrade that is active at
/// `height` has no rule set, and with [`ConsensusError::IssuedSupplyUnknown`] from the NSM
/// reissuance height.
pub fn block_subsidy(network: Network, height: u32) -> Result<Subsidy, ConsensusError> {
    rules_at(network, height)?;
    core::block_subsidy(network.core(), height)
}

/// The halving index of `height` on `network` (`hayai_consensus_core::subsidy_schedule::halving`).
pub fn halving(network: Network, height: u32) -> u32 {
    checked(core::halving(network.core(), height))
}

/// The sum of the scheduled subsidies of the heights 0 to `height` on `network`
/// (`hayai_consensus_core::subsidy_schedule::scheduled_issuance`).
pub fn scheduled_issuance(network: Network, height: u32) -> u128 {
    checked(core::scheduled_issuance(network.core(), height))
}

/// The subsidy of the halving schedule at `height` on `network`, without the NSM
/// reissuance bonus (`hayai_consensus_core::subsidy_schedule::scheduled_subsidy`).
pub fn scheduled_subsidy(network: Network, height: u32) -> u64 {
    checked(core::scheduled_subsidy(network.core(), height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{nsm, RuleSet, Upgrade};
    use hayai_consensus_core::subsidy_schedule::{halving_height, next_subsidy_change};

    const ZEC: u64 = 100_000_000;

    fn total(network: Network, height: u32) -> u64 {
        block_subsidy(network, height).unwrap().total
    }

    #[test]
    fn regtest_subsidy_halves_every_288_blocks_after_blossom() {
        assert_eq!(total(Network::Regtest, 0), 0);
        assert_eq!(total(Network::Regtest, 1), 625_000_000);
        assert_eq!(total(Network::Regtest, 286), 625_000_000);
        assert_eq!(total(Network::Regtest, 287), 312_500_000);
        assert_eq!(total(Network::Regtest, 574), 312_500_000);
        assert_eq!(total(Network::Regtest, 575), 156_250_000);
        assert_eq!(total(Network::Regtest, 288 * 64), 0);
        assert_eq!(
            block_subsidy(Network::Regtest, 287),
            Ok(Subsidy {
                total: 312_500_000,
                deferred: 0,
            })
        );
    }

    #[test]
    fn mainnet_subsidy_follows_the_schedule() {
        let total = |height| total(Network::Mainnet, height);
        // Slow start: linear ramp, skipping the middle payout.
        assert_eq!(total(0), 0);
        assert_eq!(total(1), 62_500);
        assert_eq!(total(9_999), 62_500 * 9_999);
        assert_eq!(total(10_000), 62_500 * 10_001);
        assert_eq!(total(19_999), 62_500 * 20_000);
        assert_eq!(total(20_000), 25 * ZEC / 2);
        // Blossom halves the subsidy; the halvings follow.
        assert_eq!(total(653_599), 25 * ZEC / 2);
        assert_eq!(total(653_600), 25 * ZEC / 4);
        assert_eq!(total(1_046_399), 25 * ZEC / 4);
        assert_eq!(total(1_046_400), 25 * ZEC / 8);
        assert_eq!(total(2_726_399), 25 * ZEC / 8);
        assert_eq!(total(2_726_400), 25 * ZEC / 16);
        assert_eq!(total(4_406_399), 25 * ZEC / 16);
        assert_eq!(total(4_406_400), 25 * ZEC / 32);
    }

    #[test]
    fn testnet_subsidy_follows_the_schedule() {
        let total = |height| total(Network::Testnet, height);
        assert_eq!(total(0), 0);
        assert_eq!(total(1), 62_500);
        assert_eq!(total(9_999), 62_500 * 9_999);
        assert_eq!(total(10_000), 62_500 * 10_001);
        assert_eq!(total(20_000), 25 * ZEC / 2);
        // Blossom at 584,000. The first halving at 1,116,000 (protocol specification
        // §7.10.1, Zakura `testnet::FIRST_HALVING`). Canopy at 1,028,500 does not change
        // the subsidy.
        assert_eq!(total(583_999), 25 * ZEC / 2);
        assert_eq!(total(584_000), 25 * ZEC / 4);
        assert_eq!(total(1_028_500), 25 * ZEC / 4);
        assert_eq!(total(1_115_999), 25 * ZEC / 4);
        assert_eq!(total(1_116_000), 25 * ZEC / 8);
        assert_eq!(total(2_795_999), 25 * ZEC / 8);
        assert_eq!(total(2_796_000), 25 * ZEC / 16);
        // NU6 at 2,976,000, NU6.1 at 3,536,500 and NU6.3 at 4,134,000 do not change it.
        for height in [2_976_000, 3_536_500, 4_134_000] {
            assert_eq!(total(height - 1), 25 * ZEC / 16);
            assert_eq!(total(height), 25 * ZEC / 16);
        }
    }

    /// The values of Zebra's `halving_for_network` and `block_subsidy_for_network` tests,
    /// for the heights at which the schedule has no NU7 era: every height on Mainnet, and
    /// the heights up to the second halving on Testnet.
    #[test]
    fn halvings_and_subsidies_without_the_nu7_era() {
        for (network, first_halving, blossom) in [
            (Network::Mainnet, 1_046_400u32, 653_600),
            (Network::Testnet, 1_116_000, 584_000),
        ] {
            let params = network.params();
            let interval = network.core().post_blossom_halving_interval().unwrap();
            assert_eq!(network.activation_height(Upgrade::Blossom), Some(blossom));
            let halving = |height| halving(network, height);
            let subsidy = |height| scheduled_subsidy(network, height);

            assert_eq!(halving(params.slow_start_interval + 1), 0);
            assert_eq!(halving(blossom - 1), 0);
            assert_eq!(halving(blossom), 0);
            assert_eq!(halving(first_halving - 1), 0);
            assert_eq!(halving(first_halving), 1);
            assert_eq!(halving(first_halving + 1), 1);
            assert_eq!(halving(first_halving + interval), 2);

            assert_eq!(subsidy(params.slow_start_interval + 1), 1_250_000_000);
            assert_eq!(subsidy(blossom - 1), 1_250_000_000);
            assert_eq!(subsidy(blossom), 625_000_000);
            assert_eq!(subsidy(first_halving), 312_500_000);
            assert_eq!(subsidy(first_halving + interval), 156_250_000);
            if network != Network::Mainnet {
                continue;
            }
            for n in [2, 9, 19, 29, 39, 62, 63] {
                assert_eq!(halving(first_halving + n * interval), n + 1);
            }
            assert_eq!(subsidy(first_halving + 6 * interval), 4_882_812);
            assert_eq!(subsidy(first_halving + 28 * interval), 1);
            for n in [29, 39, 49, 59, 62, 63, 64] {
                assert_eq!(subsidy(first_halving + n * interval), 0);
            }
            for height in [i32::MAX as u32 / 4, i32::MAX as u32 / 2, i32::MAX as u32] {
                assert_eq!(subsidy(height), 0);
            }
        }
    }

    /// ZIP 218 on Testnet, NU7 at `A` = 4,465,026: the subsidy of one block is
    /// `floor(1,250,000,000 * 25 / 150)` after the halvings, and a halving interval has
    /// 5,040,000 blocks. The third halving moves from 4,476,000 to `A + 3 * (4,476,000 -
    /// A)`.
    #[test]
    fn the_testnet_schedule_follows_the_nu7_spacing() {
        let network = Network::Testnet;
        let spec = network.core();
        let nu7 = 4_465_026;
        let third = nu7 + 3 * (4_476_000 - nu7);
        assert_eq!(third, 4_497_948);
        for (height, index, subsidy) in [
            (nu7 - 1, 2, 156_250_000),
            (nu7, 2, 52_083_333),
            (nu7 + 1, 2, 52_083_333),
            (4_476_000, 2, 52_083_333),
            (third - 1, 2, 52_083_333),
            (third, 3, 26_041_666),
            (third + 5_040_000 - 1, 3, 26_041_666),
            (third + 5_040_000, 4, 13_020_833),
        ] {
            assert_eq!(halving(network, height), index, "{height}");
            assert_eq!(scheduled_subsidy(network, height), subsidy, "{height}");
        }
        assert_eq!(halving_height(spec, 3, u32::MAX), Ok(Some(third)));
        assert_eq!(
            halving_height(spec, 4, u32::MAX),
            Ok(Some(third + 5_040_000))
        );
        assert_eq!(halving_height(spec, 3, third - 1), Ok(None));
        assert_eq!(next_subsidy_change(spec, nu7 - 1), Ok(Some(nu7)));
        assert_eq!(next_subsidy_change(spec, nu7), Ok(Some(third)));
        assert_eq!(next_subsidy_change(spec, 2_796_000), Ok(Some(nu7)));
    }

    /// The scheduled issuance is the sum of the subsidies of the heights.
    #[test]
    fn the_scheduled_issuance_is_the_sum_of_the_subsidies() {
        for network in [Network::Mainnet, Network::Testnet] {
            let mut sum = 0u128;
            for height in 0..=20_010 {
                sum += u128::from(scheduled_subsidy(network, height));
                if matches!(
                    height,
                    0 | 1 | 9_999 | 10_000 | 10_001 | 19_999 | 20_000 | 20_010
                ) {
                    assert_eq!(
                        scheduled_issuance(network, height),
                        sum,
                        "{network:?} {height}"
                    );
                }
            }
        }
        // Across Blossom, the halvings and NU7: each run of equal subsidies adds its
        // blocks times its subsidy.
        let network = Network::Testnet;
        let step = |from: u32, to: u32| {
            scheduled_issuance(network, to) - scheduled_issuance(network, from)
        };
        assert_eq!(step(583_998, 584_001), 1_250_000_000 + 2 * 625_000_000);
        assert_eq!(step(1_115_998, 1_116_001), 625_000_000 + 2 * 312_500_000);
        assert_eq!(step(4_465_024, 4_465_027), 156_250_000 + 2 * 52_083_333);
        assert_eq!(step(4_497_946, 4_497_949), 52_083_333 + 2 * 26_041_666);
        assert_eq!(
            step(4_465_025, 4_497_947),
            u128::from(4_497_947u32 - 4_465_025) * 52_083_333
        );
        // Regtest has no slow start: 625,000,000 for each of the heights 1 to 286.
        assert_eq!(scheduled_issuance(Network::Regtest, 0), 0);
        assert_eq!(scheduled_issuance(Network::Regtest, 286), 286 * 625_000_000);
        assert_eq!(
            scheduled_issuance(Network::Regtest, 288),
            286 * 625_000_000 + 2 * 312_500_000
        );
        // The schedule ends: the total stays below 21,000,000 ZEC.
        let all = scheduled_issuance(Network::Mainnet, u32::MAX);
        assert_eq!(all, scheduled_issuance(Network::Mainnet, u32::MAX - 1));
        assert!(all < 2_100_000_000_000_000);
    }

    #[test]
    fn the_deferred_part_is_the_lockbox_stream() {
        // Mainnet: 12 % of the subsidy from NU6 to the end of the last stream.
        for (height, deferred) in [
            (2_726_399, 0),
            (2_726_400, 18_750_000),
            (3_146_399, 18_750_000),
            (3_146_400, 18_750_000),
            (4_406_399, 18_750_000),
            (4_406_400, 0),
        ] {
            let subsidy = block_subsidy(Network::Mainnet, height).unwrap();
            assert_eq!(subsidy.deferred, deferred, "Mainnet {height}");
        }
        // Testnet: no stream exists from 3,396,000 to the NU6.1 activation.
        for (height, deferred) in [
            (2_975_999, 0),
            (2_976_000, 18_750_000),
            (3_395_999, 18_750_000),
            (3_396_000, 0),
            (3_536_499, 0),
            (3_536_500, 18_750_000),
        ] {
            let subsidy = block_subsidy(Network::Testnet, height).unwrap();
            assert_eq!(subsidy.deferred, deferred, "Testnet {height}");
        }
    }

    /// The subsidy across NU7 on Testnet: a third from the NU7 height with the NU7 rule
    /// set, and an error without it.
    #[test]
    fn the_subsidy_at_the_nu7_boundary() {
        let Some(nu7) = Network::Testnet.activation_height(Upgrade::Nu7) else {
            panic!("Testnet has an NU7 height on every backend");
        };
        assert_eq!(
            block_subsidy(Network::Testnet, nu7 - 1),
            Ok(Subsidy {
                total: 156_250_000,
                deferred: 18_750_000,
            })
        );
        for height in [nu7, nu7 + 1] {
            let expected = match RuleSet::of(Upgrade::Nu7) {
                Some(_) => Ok(Subsidy {
                    total: 52_083_333,
                    deferred: 6_249_999,
                }),
                None => Err(ConsensusError::UnsupportedUpgrade {
                    upgrade: Upgrade::Nu7,
                    height,
                }),
            };
            assert_eq!(block_subsidy(Network::Testnet, height), expected);
        }
    }

    /// From the NSM reissuance height the subsidy depends on the chain value pools:
    /// `block_subsidy` gives no value.
    #[test]
    fn the_subsidy_from_the_reissuance_height_needs_the_pools() {
        let Some(start) = nsm::reissuance_height(Network::Testnet) else {
            panic!("Testnet has a reissuance height");
        };
        let Some(_) = RuleSet::of(Upgrade::Nu7) else {
            return;
        };
        assert_eq!(
            block_subsidy(Network::Testnet, start - 1).map(|s| s.total),
            Ok(26_041_666)
        );
        assert_eq!(
            block_subsidy(Network::Testnet, start),
            Err(ConsensusError::IssuedSupplyUnknown { height: start })
        );
    }
}
