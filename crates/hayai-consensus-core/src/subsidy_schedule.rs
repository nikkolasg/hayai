//! The block subsidy schedule (protocol specification §7.8, zcashd `GetBlockSubsidy`,
//! ZIP 208, ZIP 218).
//!
//! The schedule is the same function on every chain: the slow start, the halvings, and
//! the changes of the target spacing. Blossom halves the subsidy of one block and doubles
//! the blocks between two halvings. NU7 (ZIP 218) divides the subsidy by 3 and multiplies
//! the blocks between two halvings by 3. The spec supplies the slow start interval, the
//! pre-Blossom halving interval and the activation heights. The arithmetic is integer
//! arithmetic as in zcashd and in Zakura (`zakura-chain/src/parameters/network/
//! subsidy.rs`, `halving` and `halving_block_subsidy`).
//!
//! From the NSM reissuance height ([`crate::nsm::reissuance_height`]) the block subsidy is
//! the subsidy of this schedule plus a bonus that depends on the chain value pools.
//! [`block_subsidy`] fails at such a height: [`crate::coinbase_value::CoinbaseTerms::after`]
//! gives the subsidy there.

use crate::{
    funding, nsm, ConsensusError, CoreSpec, Upgrade, POST_BLOSSOM_TARGET_SPACING,
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
pub const MAX_BLOCK_SUBSIDY: u64 = 1_250_000_000;

/// The number of target spacing eras.
const ERAS: usize = 3;

/// The target spacing of each era, with the upgrade that starts the era (Zakura
/// `NetworkUpgrade::target_spacings`, `network_upgrade.rs:497-515`). The spacing of the
/// difficulty rule of an upgrade is the spacing of its era (the test
/// `rule_sets::tests::the_rules_of_each_upgrade`).
const SPACING_ERAS: [(Upgrade, u32); ERAS] = [
    (Upgrade::Sprout, PRE_BLOSSOM_TARGET_SPACING),
    (Upgrade::Blossom, POST_BLOSSOM_TARGET_SPACING),
    (Upgrade::Nu7, POST_NU7_TARGET_SPACING),
];

/// The first height of each era of `spec`, in era order. `None`: the upgrade of the era has
/// no height in `spec`, and the era is not in the schedule.
fn era_starts(spec: &CoreSpec) -> [Option<u32>; ERAS] {
    let mut starts = [None; ERAS];
    for i in 0..ERAS {
        starts[i] = spec.activation_height(SPACING_ERAS[i].0);
    }
    starts
}

/// The block subsidy at `height`.
///
/// It fails with [`ConsensusError::IssuedSupplyUnknown`] from the NSM reissuance height.
///
/// Spec §7.8: `BlockSubsidy(height)` and `totalDeferredOutput(height)`.
pub fn block_subsidy(spec: &CoreSpec, height: u32) -> Result<Subsidy, ConsensusError> {
    if nsm::reissuance_active(spec, height)? {
        return Err(ConsensusError::IssuedSupplyUnknown { height });
    }
    let total = scheduled_subsidy(spec, height)?;
    Ok(Subsidy {
        total,
        deferred: funding::deferred_value(spec, height, total)?,
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
pub fn halving(spec: &CoreSpec, height: u32) -> Result<u32, ConsensusError> {
    let shift = spec.slow_start_interval / 2;
    if height < shift {
        return Ok(0);
    }
    let Some(shift_seconds) = i64::from(shift).checked_mul(i64::from(PRE_BLOSSOM_TARGET_SPACING))
    else {
        return Err(ConsensusError::Overflow);
    };
    let mut seconds = -shift_seconds;
    let starts = era_starts(spec);
    let mut failure: Option<ConsensusError> = None;
    for i in 0..ERAS {
        let start = starts[i];
        let Some(start) = start else {
            continue;
        };
        if start > height {
            continue;
        }
        // The era ends at the start of the next era of the schedule at or below `height`.
        let mut end = height;
        let mut found = false;
        for j in (i + 1)..ERAS {
            if found {
                continue;
            }
            let next = starts[j];
            if let Some(next) = next {
                if next <= height {
                    end = next;
                    found = true;
                }
            }
        }
        let Some(blocks) = end.checked_sub(start) else {
            failure = Some(ConsensusError::UncheckedSpec);
            break;
        };
        let Some(era_seconds) = i64::from(blocks).checked_mul(i64::from(SPACING_ERAS[i].1)) else {
            failure = Some(ConsensusError::Overflow);
            break;
        };
        let Some(total) = seconds.checked_add(era_seconds) else {
            failure = Some(ConsensusError::Overflow);
            break;
        };
        seconds = total;
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    let Some(interval_seconds) = i64::from(spec.pre_blossom_halving_interval)
        .checked_mul(i64::from(PRE_BLOSSOM_TARGET_SPACING))
    else {
        return Err(ConsensusError::Overflow);
    };
    let Some(index) = seconds.checked_div(interval_seconds) else {
        return Err(ConsensusError::DivisionByZero);
    };
    // The total is negative above the shift only when Blossom activates below the shift.
    // The index is then 0.
    if index < 0 {
        return Ok(0);
    }
    let Ok(index) = u32::try_from(index) else {
        return Err(ConsensusError::Overflow);
    };
    Ok(index)
}

/// The first height above `height` at which [`scheduled_subsidy`] can change after the
/// slow start: the start of the next target spacing era or the next halving. `None` when
/// neither exists.
pub fn next_subsidy_change(spec: &CoreSpec, height: u32) -> Result<Option<u32>, ConsensusError> {
    let starts = era_starts(spec);
    let mut era: Option<u32> = None;
    for i in 0..ERAS {
        let start = starts[i];
        let Some(start) = start else {
            continue;
        };
        if start > height {
            era = match era {
                Some(earliest) if earliest <= start => Some(earliest),
                _ => Some(start),
            };
        }
    }
    let index = halving(spec, height)?;
    // `halving` does not decrease with the height: a binary search finds its next step.
    let mut next_halving: Option<u32> = None;
    if halving(spec, u32::MAX)? > index {
        let Some(mut low) = height.checked_add(1) else {
            return Err(ConsensusError::Overflow);
        };
        let mut high = u32::MAX;
        let mut failure: Option<ConsensusError> = None;
        while low < high {
            let middle = match midpoint(low, high) {
                Ok(middle) => middle,
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            };
            let above = match halving(spec, middle) {
                Ok(halving) => halving > index,
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            };
            if above {
                high = middle;
            } else {
                let Some(next) = middle.checked_add(1) else {
                    failure = Some(ConsensusError::Overflow);
                    break;
                };
                low = next;
            }
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        next_halving = Some(low);
    }
    Ok(match (era, next_halving) {
        (Some(era), Some(halving)) => Some(era.min(halving)),
        (change, None) | (None, change) => change,
    })
}

/// The first height with the halving index `index`. `None` when no height at or below
/// `max_height` has it.
pub fn halving_height(
    spec: &CoreSpec,
    index: u32,
    max_height: u32,
) -> Result<Option<u32>, ConsensusError> {
    if halving(spec, max_height)? < index {
        return Ok(None);
    }
    let (mut low, mut high) = (0, max_height);
    let mut failure: Option<ConsensusError> = None;
    while low < high {
        let middle = match midpoint(low, high) {
            Ok(middle) => middle,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        let below = match halving(spec, middle) {
            Ok(halving) => halving < index,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        if below {
            let Some(next) = middle.checked_add(1) else {
                failure = Some(ConsensusError::Overflow);
                break;
            };
            low = next;
        } else {
            high = middle;
        }
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    Ok(Some(low))
}

/// `low + (high - low) / 2`: the middle of a search range with `low <= high`.
fn midpoint(low: u32, high: u32) -> Result<u32, ConsensusError> {
    let Some(span) = high.checked_sub(low) else {
        return Err(ConsensusError::Overflow);
    };
    let Some(middle) = low.checked_add(span / 2) else {
        return Err(ConsensusError::Overflow);
    };
    Ok(middle)
}

/// `n * (n + 1) / 2`: the sum of 1 to `n`.
fn sum_to(n: u128) -> Result<u128, ConsensusError> {
    let Some(next) = n.checked_add(1) else {
        return Err(ConsensusError::Overflow);
    };
    let Some(product) = n.checked_mul(next) else {
        return Err(ConsensusError::Overflow);
    };
    Ok(product / 2)
}

/// The sum of [`scheduled_subsidy`] of the heights 0 to `height` (Zakura
/// `scheduled_issuance_zatoshis`, `subsidy.rs:800-869`).
///
/// ZIP 237: `S_A(height)`, the sum of `ScheduledBlockSubsidy` from height 0, without
/// `AdditionalBlockSubsidy`.
///
/// The slow start part is a closed form that holds while the halving index is 0, which is
/// true on every checked spec: the slow start ends before the first halving.
pub fn scheduled_issuance(spec: &CoreSpec, height: u32) -> Result<u128, ConsensusError> {
    let interval = spec.slow_start_interval;
    let mut total = 0u128;
    if interval > 0 {
        let rate = u128::from(MAX_BLOCK_SUBSIDY / u64::from(interval));
        let shift = u128::from(interval / 2);
        let Some(last_ramp) = interval.checked_sub(1) else {
            return Err(ConsensusError::Overflow);
        };
        let last = u128::from(height.min(last_ramp));
        // `rate * h` below the shift, `rate * (h + 1)` from the shift.
        let Some(before_shift) = shift.checked_sub(1) else {
            return Err(ConsensusError::UncheckedSpec);
        };
        let Some(ramp) = sum_to(last.min(before_shift))?.checked_mul(rate) else {
            return Err(ConsensusError::Overflow);
        };
        total = ramp;
        if last >= shift {
            let Some(after_last) = last.checked_add(1) else {
                return Err(ConsensusError::Overflow);
            };
            let Some(steps) = sum_to(after_last)?.checked_sub(sum_to(shift)?) else {
                return Err(ConsensusError::Overflow);
            };
            let Some(rest) = steps.checked_mul(rate) else {
                return Err(ConsensusError::Overflow);
            };
            let Some(sum) = total.checked_add(rest) else {
                return Err(ConsensusError::Overflow);
            };
            total = sum;
        }
    }
    let mut first = interval.max(1);
    let mut failure: Option<ConsensusError> = None;
    while first <= height {
        let subsidy = match scheduled_subsidy(spec, first) {
            Ok(subsidy) => subsidy,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        let next_change = match next_subsidy_change(spec, first) {
            Ok(next) => next,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        let last = match next_change {
            Some(next) => {
                let Some(before_next) = next.checked_sub(1) else {
                    failure = Some(ConsensusError::Overflow);
                    break;
                };
                before_next.min(height)
            }
            None => height,
        };
        let Some(span) = last.checked_sub(first) else {
            failure = Some(ConsensusError::Overflow);
            break;
        };
        let Some(blocks) = u128::from(span).checked_add(1) else {
            failure = Some(ConsensusError::Overflow);
            break;
        };
        let Some(run) = blocks.checked_mul(u128::from(subsidy)) else {
            failure = Some(ConsensusError::Overflow);
            break;
        };
        let Some(sum) = total.checked_add(run) else {
            failure = Some(ConsensusError::Overflow);
            break;
        };
        total = sum;
        if last == u32::MAX {
            break;
        }
        let Some(next) = last.checked_add(1) else {
            failure = Some(ConsensusError::Overflow);
            break;
        };
        first = next;
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    Ok(total)
}

/// The subsidy of the halving schedule at `height`, without the NSM reissuance bonus
/// (Zakura `halving_block_subsidy`, `subsidy.rs:948-984`).
///
/// The genesis block has no subsidy on any chain: no rule reads its coinbase.
///
/// Spec §7.8: `BlockSubsidy(height)`: the slow start ramp below `SlowStartInterval`, then
/// `MaxBlockSubsidy` over the spacing ratio and `2^Halving(height)`. ZIP 218: from NU7 the
/// subsidy is `MaxBlockSubsidy * 25 / 150` over `2^Halving(height)`. ZIP 237: this is
/// `ScheduledBlockSubsidy(height)`.
pub fn scheduled_subsidy(spec: &CoreSpec, height: u32) -> Result<u64, ConsensusError> {
    if height == 0 {
        return Ok(0);
    }
    let halvings = halving(spec, height)?;
    // zcashd: "Force block reward to zero when right shift is undefined".
    if halvings >= 64 {
        return Ok(0);
    }
    let slow_start = spec.slow_start_interval;
    if height < slow_start {
        // Spec §7.8: SlowStartRate * height below SlowStartShift, SlowStartRate * (height +
        // 1) from it. The ramp skips one step at the slow start shift.
        let rate = MAX_BLOCK_SUBSIDY / u64::from(slow_start);
        let steps = if height < slow_start / 2 {
            u64::from(height)
        } else {
            u64::from(height) + 1
        };
        let Some(subsidy) = rate.checked_mul(steps) else {
            return Err(ConsensusError::MoneyOverflow);
        };
        return Ok(subsidy);
    }
    // The subsidy of one block follows the target spacing, so the issuance of one second
    // does not change: Blossom divides the subsidy by 2, NU7 by 3 more.
    let starts = era_starts(spec);
    let mut spacing: Option<u32> = None;
    for i in 0..ERAS {
        let start = starts[i];
        if let Some(start) = start {
            if start <= height {
                spacing = Some(SPACING_ERAS[i].1);
            }
        }
    }
    let Some(spacing) = spacing else {
        return Err(ConsensusError::UncheckedSpec);
    };
    let Some(scaled) = MAX_BLOCK_SUBSIDY.checked_mul(u64::from(spacing)) else {
        return Err(ConsensusError::MoneyOverflow);
    };
    let Some(subsidy) = (scaled / u64::from(PRE_BLOSSOM_TARGET_SPACING)).checked_shr(halvings)
    else {
        return Err(ConsensusError::Overflow);
    };
    Ok(subsidy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain_spec::tests::regtest;

    fn total(spec: &CoreSpec, height: u32) -> u64 {
        block_subsidy(spec, height).unwrap().total
    }

    #[test]
    fn regtest_subsidy_halves_every_288_blocks_after_blossom() {
        let spec = regtest();
        assert_eq!(total(&spec, 0), 0);
        assert_eq!(total(&spec, 1), 625_000_000);
        assert_eq!(total(&spec, 286), 625_000_000);
        assert_eq!(total(&spec, 287), 312_500_000);
        assert_eq!(total(&spec, 574), 312_500_000);
        assert_eq!(total(&spec, 575), 156_250_000);
        assert_eq!(total(&spec, 288 * 64), 0);
        assert_eq!(
            block_subsidy(&spec, 287),
            Ok(Subsidy {
                total: 312_500_000,
                deferred: 0,
            })
        );
        assert_eq!(halving_height(&spec, 1, u32::MAX), Ok(Some(287)));
        assert_eq!(halving_height(&spec, 2, u32::MAX), Ok(Some(575)));
        assert_eq!(halving_height(&spec, 2, 574), Ok(None));
        assert_eq!(next_subsidy_change(&spec, 1), Ok(Some(287)));
        assert_eq!(next_subsidy_change(&spec, 287), Ok(Some(575)));
    }

    /// A slow start of 20,000 blocks: the ramp of Mainnet and Testnet.
    #[test]
    fn the_slow_start_ramp_skips_the_middle_step() {
        let mut spec = regtest();
        spec.slow_start_interval = 20_000;
        spec.pre_blossom_halving_interval = 840_000;
        spec.first_halving = None;
        assert_eq!(total(&spec, 0), 0);
        assert_eq!(total(&spec, 1), 62_500);
        assert_eq!(total(&spec, 9_999), 62_500 * 9_999);
        assert_eq!(total(&spec, 10_000), 62_500 * 10_001);
        assert_eq!(total(&spec, 19_999), 62_500 * 20_000);
        assert_eq!(total(&spec, 20_000), 625_000_000);
        let mut sum = 0u128;
        for height in 0..=20_010 {
            sum += u128::from(scheduled_subsidy(&spec, height).unwrap());
            if matches!(
                height,
                0 | 1 | 9_999 | 10_000 | 10_001 | 19_999 | 20_000 | 20_010
            ) {
                assert_eq!(scheduled_issuance(&spec, height), Ok(sum), "{height}");
            }
        }
    }

    /// Each arithmetic failure of the schedule is an error, not a panic.
    #[test]
    fn a_spec_with_a_zero_interval_gives_an_error() {
        let mut spec = regtest();
        spec.pre_blossom_halving_interval = 0;
        assert_eq!(halving(&spec, 1), Err(ConsensusError::DivisionByZero));
        assert_eq!(block_subsidy(&spec, 1), Err(ConsensusError::DivisionByZero));
        assert_eq!(
            scheduled_issuance(&spec, 1),
            Err(ConsensusError::DivisionByZero)
        );
        let mut unordered = regtest();
        unordered.activation_heights[Upgrade::Blossom.index()] = Some(100);
        unordered.activation_heights[Upgrade::Nu7.index()] = Some(50);
        assert_eq!(halving(&unordered, 100), Err(ConsensusError::UncheckedSpec));
        let mut no_sprout = regtest();
        no_sprout.activation_heights[Upgrade::Sprout.index()] = None;
        no_sprout.activation_heights[Upgrade::Blossom.index()] = Some(5);
        assert_eq!(
            scheduled_subsidy(&no_sprout, 3),
            Err(ConsensusError::UncheckedSpec)
        );
        assert_eq!(sum_to(u128::MAX), Err(ConsensusError::Overflow));
    }
}
