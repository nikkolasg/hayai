//! The block subsidy schedule (protocol specification §7.8, zcashd `GetBlockSubsidy`,
//! ZIP 208, ZIP 218).
//!
//! The schedule is the same function on every network: the slow start, the halvings, and
//! the changes of the target spacing. Blossom halves the subsidy of one block and doubles
//! the blocks between two halvings. NU7 (ZIP 218) divides the subsidy by 3 and multiplies
//! the blocks between two halvings by 3. The network supplies the slow start interval, the
//! pre-Blossom halving interval and the activation heights. The arithmetic is integer
//! arithmetic as in zcashd and in Zakura (`zakura-chain/src/parameters/network/
//! subsidy.rs`, `halving` and `halving_block_subsidy`).
//!
//! From the NSM reissuance height ([`crate::nsm::reissuance_height`]) the block subsidy is
//! the subsidy of this schedule plus a bonus that depends on the chain value pools.
//! [`block_subsidy`] fails at such a height: [`crate::coinbase::CoinbaseTerms::after`]
//! gives the subsidy there.

use crate::{
    funding, nsm, rules_at, ConsensusError, Network, Upgrade, POST_BLOSSOM_TARGET_SPACING,
    POST_NU7_TARGET_SPACING, PRE_BLOSSOM_TARGET_SPACING,
};

/// The block subsidy of one height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subsidy {
    /// Total subsidy (zcashd `GetBlockSubsidy`): the miner's share, the founders' reward,
    /// every funding stream and the deferred part.
    pub total: u64,
    /// The part of `total` that the coinbase does not pay out: the value of the funding
    /// stream with the deferred pool (lockbox) as its recipient (ZIP 1015, ZIP 214
    /// revision 2, from NU6).
    pub deferred: u64,
}

/// 12.5 ZEC in zatoshis (`MaxBlockSubsidy`).
///
/// Spec §7.8: `MaxBlockSubsidy` of `SlowStartRate` and `BlockSubsidy`.
const MAX_BLOCK_SUBSIDY: u64 = 1_250_000_000;

/// The target spacing of each era, with the upgrade that starts the era (Zakura
/// `NetworkUpgrade::target_spacings`, `network_upgrade.rs:497-515`). The spacing of the
/// difficulty rule of an upgrade is the spacing of its era (the test
/// `rules::tests::the_rules_of_each_upgrade`).
const SPACING_ERAS: [(Upgrade, u32); 3] = [
    (Upgrade::Sprout, PRE_BLOSSOM_TARGET_SPACING),
    (Upgrade::Blossom, POST_BLOSSOM_TARGET_SPACING),
    (Upgrade::Nu7, POST_NU7_TARGET_SPACING),
];

/// The first height and the target spacing of each era of `network`, in height order. An
/// era whose upgrade has no height on `network` is not in the list.
fn spacing_eras(network: Network) -> impl Iterator<Item = (u32, u32)> {
    SPACING_ERAS
        .into_iter()
        .filter_map(move |(upgrade, spacing)| Some((network.activation_height(upgrade)?, spacing)))
}

/// The block subsidy at `height` on `network`.
///
/// It fails with [`ConsensusError::UnsupportedUpgrade`] when the upgrade that is active at
/// `height` has no rule set, and with [`ConsensusError::IssuedSupplyUnknown`] from the NSM
/// reissuance height.
pub fn block_subsidy(network: Network, height: u32) -> Result<Subsidy, ConsensusError> {
    rules_at(network, height)?;
    if nsm::reissuance_active(network, height) {
        return Err(ConsensusError::IssuedSupplyUnknown { height });
    }
    let total = total_subsidy(network, height);
    Ok(Subsidy {
        total,
        deferred: funding::deferred_value(network, height, total),
    })
}

/// The halving index of `height` (`Halving(height)`, protocol specification §7.8, with
/// the NU7 era of ZIP 218).
///
/// Each target spacing era adds its blocks times its spacing to a total of block seconds.
/// The index is that total over the pre-Blossom halving interval in seconds. The total
/// starts at the slow start shift. This is Zakura's `halving` (`subsidy.rs:523-563`).
///
/// Spec §7.8: `Halving(height)`, 0 below `SlowStartShift`, with the 75 s era from Blossom.
/// ZIP 218 adds the 25 s era from NU7.
pub fn halving(network: Network, height: u32) -> u32 {
    let params = network.params();
    let shift = params.slow_start_interval / 2;
    if height < shift {
        return 0;
    }
    let mut seconds = -i64::from(shift) * i64::from(PRE_BLOSSOM_TARGET_SPACING);
    let mut eras = spacing_eras(network)
        .filter(|(start, _)| *start <= height)
        .peekable();
    while let Some((start, spacing)) = eras.next() {
        let end = eras.peek().map_or(height, |(next, _)| *next);
        seconds += i64::from(end - start) * i64::from(spacing);
    }
    let interval_seconds =
        i64::from(params.pre_blossom_halving_interval) * i64::from(PRE_BLOSSOM_TARGET_SPACING);
    // The total is negative above the shift only when Blossom activates below the shift.
    // The index is then 0.
    let Ok(index) = u32::try_from((seconds / interval_seconds).max(0)) else {
        unreachable!("a halving index of a 32-bit height fits in 32 bits");
    };
    index
}

/// The first height above `height` at which [`total_subsidy`] can change after the slow
/// start: the start of the next target spacing era or the next halving. `None` when
/// neither exists.
fn next_subsidy_change(network: Network, height: u32) -> Option<u32> {
    let era = spacing_eras(network)
        .map(|(start, _)| start)
        .find(|start| *start > height);
    let index = halving(network, height);
    // `halving` does not decrease with the height: a binary search finds its next step.
    let next_halving = (halving(network, u32::MAX) > index).then(|| {
        let (mut low, mut high) = (height + 1, u32::MAX);
        while low < high {
            let middle = low + (high - low) / 2;
            if halving(network, middle) > index {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        low
    });
    match (era, next_halving) {
        (Some(era), Some(halving)) => Some(era.min(halving)),
        (change, None) | (None, change) => change,
    }
}

/// The first height with the halving index `index`. `None` when no height at or below
/// `max_height` has it.
pub(crate) fn halving_height(network: Network, index: u32, max_height: u32) -> Option<u32> {
    if halving(network, max_height) < index {
        return None;
    }
    let (mut low, mut high) = (0, max_height);
    while low < high {
        let middle = low + (high - low) / 2;
        if halving(network, middle) < index {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    Some(low)
}

/// The sum of [`total_subsidy`] of the heights 0 to `height` (Zakura
/// `scheduled_issuance_zatoshis`, `subsidy.rs:800-869`).
///
/// ZIP 237: `S_A(height)`, the sum of `ScheduledBlockSubsidy` from height 0, without
/// `AdditionalBlockSubsidy`.
///
/// The slow start part is a closed form that holds while the halving index is 0, which is
/// true on every network of this crate: the slow start ends before the first halving.
pub fn scheduled_issuance(network: Network, height: u32) -> u128 {
    let interval = network.params().slow_start_interval;
    let mut total = 0u128;
    if interval > 0 {
        let rate = u128::from(MAX_BLOCK_SUBSIDY / u64::from(interval));
        let sum_to = |n: u128| n * (n + 1) / 2;
        let shift = u128::from(interval / 2);
        let last = u128::from(height.min(interval - 1));
        // `rate * h` below the shift, `rate * (h + 1)` from the shift.
        total += sum_to(last.min(shift - 1)) * rate;
        if last >= shift {
            total += (sum_to(last + 1) - sum_to(shift)) * rate;
        }
    }
    let mut first = interval.max(1);
    while first <= height {
        let subsidy = total_subsidy(network, first);
        let last =
            next_subsidy_change(network, first).map_or(height, |next| (next - 1).min(height));
        total += u128::from(last - first + 1) * u128::from(subsidy);
        if last == u32::MAX {
            break;
        }
        first = last + 1;
    }
    total
}

/// The subsidy of the halving schedule at `height`, with no check of the upgrade at
/// `height` and without the NSM reissuance bonus (Zakura `halving_block_subsidy`,
/// `subsidy.rs:948-984`).
///
/// The genesis block has no subsidy on any network: no rule reads its coinbase.
///
/// Spec §7.8: `BlockSubsidy(height)`: the slow start ramp below `SlowStartInterval`, then
/// `MaxBlockSubsidy` over the spacing ratio and `2^Halving(height)`. ZIP 218: from NU7 the
/// subsidy is `MaxBlockSubsidy * 25 / 150` over `2^Halving(height)`. ZIP 237: this is
/// `ScheduledBlockSubsidy(height)`.
pub(crate) fn total_subsidy(network: Network, height: u32) -> u64 {
    if height == 0 {
        return 0;
    }
    let halvings = halving(network, height);
    // zcashd: "Force block reward to zero when right shift is undefined".
    if halvings >= 64 {
        return 0;
    }
    let params = network.params();
    if height < params.slow_start_interval {
        // Spec §7.8: SlowStartRate * height below SlowStartShift, SlowStartRate * (height +
        // 1) from it. The ramp skips one step at the slow start shift.
        let rate = MAX_BLOCK_SUBSIDY / u64::from(params.slow_start_interval);
        return if height < params.slow_start_interval / 2 {
            rate * u64::from(height)
        } else {
            rate * (u64::from(height) + 1)
        };
    }
    // The subsidy of one block follows the target spacing, so the issuance of one second
    // does not change: Blossom divides the subsidy by 2, NU7 by 3 more.
    let Some((_, spacing)) = spacing_eras(network)
        .filter(|(start, _)| *start <= height)
        .last()
    else {
        unreachable!("the first era starts at height 0");
    };
    (MAX_BLOCK_SUBSIDY * u64::from(spacing) / u64::from(PRE_BLOSSOM_TARGET_SPACING)) >> halvings
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let interval = params.post_blossom_halving_interval();
            assert_eq!(network.activation_height(Upgrade::Blossom), Some(blossom));
            let halving = |height| halving(network, height);
            let subsidy = |height| total_subsidy(network, height);

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
            assert_eq!(total_subsidy(network, height), subsidy, "{height}");
        }
        assert_eq!(halving_height(network, 3, u32::MAX), Some(third));
        assert_eq!(
            halving_height(network, 4, u32::MAX),
            Some(third + 5_040_000)
        );
        assert_eq!(halving_height(network, 3, third - 1), None);
        assert_eq!(next_subsidy_change(network, nu7 - 1), Some(nu7));
        assert_eq!(next_subsidy_change(network, nu7), Some(third));
        assert_eq!(next_subsidy_change(network, 2_796_000), Some(nu7));
    }

    /// The scheduled issuance is the sum of the subsidies of the heights.
    #[test]
    fn the_scheduled_issuance_is_the_sum_of_the_subsidies() {
        for network in [Network::Mainnet, Network::Testnet] {
            let mut sum = 0u128;
            for height in 0..=20_010 {
                sum += u128::from(total_subsidy(network, height));
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
            let expected = match crate::RuleSet::of(Upgrade::Nu7) {
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
        let Some(_) = crate::RuleSet::of(Upgrade::Nu7) else {
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
