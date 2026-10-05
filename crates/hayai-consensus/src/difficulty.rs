//! Difficulty adjustment: the `nBits` that a block must have (protocol specification
//! §7.7.3, `ThresholdBits`), the Testnet minimum-difficulty rule (ZIP 205 and ZIP 208) and
//! the work of a block (§7.7.5).
//!
//! References: zcashd `pow.cpp` (`GetNextWorkRequired`, `CalculateNextWorkRequired`) and
//! Zakura `zakura-header-chain/src/validation/contextual/adjusted_difficulty.rs`.
//!
//! The rule for the block at `height` with time `time`:
//!
//! 1. Testnet only, from height 299,188: when `time` is more than 6 target spacings after
//!    the time of the parent, the result is the proof-of-work limit.
//! 2. When `height <= PoWAveragingWindow` (17), the result is the proof-of-work limit.
//! 3. `MeanTarget` is the mean of the targets of the 17 blocks before `height`.
//! 4. `ActualTimespan` is `MedianTime(height) - MedianTime(height - 17)`. `MedianTime(h)`
//!    is the median of the times of the 11 blocks before `h`.
//! 5. `ActualTimespanDamped` is `AveragingWindowTimespan + (ActualTimespan -
//!    AveragingWindowTimespan) / 4`, with a division that truncates toward zero.
//!    `AveragingWindowTimespan` is 17 target spacings of `height`.
//! 6. `ActualTimespanBounded` keeps the damped value between 84 % and 132 % of
//!    `AveragingWindowTimespan`.
//! 7. The target is `floor(MeanTarget / AveragingWindowTimespan) * ActualTimespanBounded`,
//!    at most the proof-of-work limit. The result is its compact form.
//!
//! The rule reads the times of the 28 blocks before `height` and the `nBits` of the 17
//! blocks before it. A shorter context is [`DifficultyError::ContextTooShort`]: the
//! function never computes a value from a part of the window.

use hayai_crypto::primitive_types::U256;
use hayai_wire::header::{compact_from_target, expand_target};

use crate::{rules_at, ConsensusError, DifficultyParams, Network, MEDIAN_TIME_SPAN};

/// The blocks before a header, as the header rules read them.
#[derive(Clone, Copy, Debug)]
pub struct ParentChain<'a> {
    /// Height of the header that the rules check: the height of its parent plus 1.
    pub height: u32,
    /// `nTime` of the blocks before the header, newest first. The first entry belongs to
    /// the parent.
    pub times: &'a [u32],
    /// `nBits` of the blocks before the header, newest first. The first entry belongs to
    /// the parent.
    pub bits: &'a [u32],
}

/// The context holds fewer blocks than a rule reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "the context holds {times} block times and {bits} nBits values, the rule reads \
     {needed_times} and {needed_bits}"
)]
pub struct ContextTooShort {
    pub times: usize,
    pub needed_times: usize,
    pub bits: usize,
    pub needed_bits: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DifficultyError {
    #[error("the genesis block has no difficulty rule")]
    Genesis,
    #[error(transparent)]
    ContextTooShort(#[from] ContextTooShort),
    #[error(transparent)]
    Rules(#[from] ConsensusError),
    /// A block of the context has `nBits` that encode no target. Such a block is not valid,
    /// so the context is not a chain of checked headers.
    #[error("nBits {0:#010x} of a block of the context encode no target")]
    InvalidContextBits(u32),
}

/// The target that `bits` encodes. `None` when `bits` encode no target (negative, zero or
/// overflow).
pub fn target_from_compact(bits: u32) -> Option<U256> {
    expand_target(bits).map(|target| U256::from_little_endian(&target))
}

/// The compact form of `target` (zcashd `arith_uint256::GetCompact`).
pub fn compact_from_u256(target: U256) -> u32 {
    let mut bytes = [0u8; 32];
    target.to_little_endian(&mut bytes);
    compact_from_target(&bytes)
}

/// The work of a block with target `bits`: `floor(2^256 / (target + 1))` (protocol
/// specification §7.7.5, the ZIP 221 field `nSubTreeTotalWork`). The cumulative work of a
/// chain is the sum of the work of its blocks. `None` when `bits` encode no target.
pub fn block_work(bits: u32) -> Option<U256> {
    let target = target_from_compact(bits)?;
    // `expand_target` bounds the target below 2^256 - 1, so `target + 1` does not overflow.
    // `floor((2^256 - 1 - target) / (target + 1)) + 1` equals the formula (Bitcoin's
    // `GetBlockProof`) without a 257-bit numerator.
    Some((!target / (target + 1)) + 1)
}

/// The median of `times` as the specification defines it: the element at index
/// `floor(len / 2)` of the sorted list. `None` for an empty list.
pub fn median_time(times: &[u32]) -> Option<u32> {
    let mut sorted = times.to_vec();
    sorted.sort_unstable();
    sorted.get(sorted.len() / 2).copied()
}

/// The median-time-past of the header after `times` (newest first): the median of the
/// newest [`MEDIAN_TIME_SPAN`] times. `None` for an empty list.
pub fn median_time_past(times: &[u32]) -> Option<u32> {
    median_time(&times[..times.len().min(MEDIAN_TIME_SPAN)])
}

/// The times that the rule of the block at `height` reads: one per block before `height`,
/// at most `span`.
fn needed(height: u32, span: usize) -> usize {
    span.min(usize::try_from(height).unwrap_or(usize::MAX))
}

/// The `nBits` that the block at `chain.height` with time `time` must have on `network`.
///
/// Regtest has no such rule in hayai (`NetworkParams::disable_pow`): the header rules do
/// not call this function there.
pub fn expected_bits(
    network: Network,
    time: u32,
    chain: &ParentChain<'_>,
) -> Result<u32, DifficultyError> {
    let height = chain.height;
    if height == 0 {
        return Err(DifficultyError::Genesis);
    }
    let params = &rules_at(network, height)?.difficulty;
    let net = network.params();
    let short = |needed_times: usize, needed_bits: usize| ContextTooShort {
        times: chain.times.len(),
        needed_times,
        bits: chain.bits.len(),
        needed_bits,
    };

    // ZIP 205 and ZIP 208: a Testnet block more than 6 target spacings after its parent
    // can use the limit (zcashd `nPowAllowMinDifficultyBlocksAfterHeight`).
    if matches!(net.min_difficulty_start_height, Some(start) if height >= start) {
        let Some(parent_time) = chain.times.first() else {
            return Err(short(1, 0).into());
        };
        let gap = i64::from(time) - i64::from(*parent_time);
        if gap > i64::from(params.min_difficulty_gap_spacings * params.target_spacing) {
            return Ok(net.pow_limit_bits);
        }
    }

    let window = params.averaging_window as usize;
    if height <= params.averaging_window {
        return Ok(net.pow_limit_bits);
    }

    let needed_times = needed(height, window + MEDIAN_TIME_SPAN);
    if chain.times.len() < needed_times || chain.bits.len() < window {
        return Err(short(needed_times, window).into());
    }
    let times = &chain.times[..needed_times];
    let mean = mean_target(&chain.bits[..window])?;
    let (Some(newer), Some(older)) = (median_time_past(times), median_time(&times[window..]))
    else {
        unreachable!("the height is above the window, so both spans hold a time");
    };
    let timespan = bounded_timespan(params, i64::from(newer) - i64::from(older));

    let limit = U256::from_little_endian(&net.pow_limit);
    let scaled = mean / U256::from(averaging_window_timespan(params));
    // A product above 2^256 - 1 is above the limit.
    let target = scaled
        .checked_mul(U256::from(timespan))
        .map_or(limit, |target| target.min(limit));
    Ok(compact_from_u256(target))
}

/// `AveragingWindowTimespan`: the averaging window at the target spacing, in seconds.
fn averaging_window_timespan(params: &DifficultyParams) -> u32 {
    params.averaging_window * params.target_spacing
}

/// `MeanTarget`: the mean of the targets of `bits`, rounded down.
///
/// The sum of the quotients plus the quotient of the sum of the remainders equals the
/// quotient of the sum, and no step exceeds 256 bits for any window length (Zakura
/// `mean_target_difficulty`).
fn mean_target(bits: &[u32]) -> Result<U256, DifficultyError> {
    let count = U256::from(bits.len());
    let mut quotients = U256::zero();
    let mut remainders = U256::zero();
    for compact in bits {
        let Some(target) = target_from_compact(*compact) else {
            return Err(DifficultyError::InvalidContextBits(*compact));
        };
        quotients += target / count;
        remainders += target % count;
    }
    Ok(quotients + remainders / count)
}

/// `ActualTimespanBounded` of `actual` (`ActualTimespan`, in seconds).
fn bounded_timespan(params: &DifficultyParams, actual: i64) -> u64 {
    let window = i64::from(averaging_window_timespan(params));
    // Rust's integer division truncates toward zero, as the specification requires.
    let damped = window + (actual - window) / i64::from(params.damping_factor);
    let min = window * i64::from(100 - params.max_adjust_up_percent) / 100;
    let max = window * i64::from(100 + params.max_adjust_down_percent) / 100;
    let bounded = damped.clamp(min, max);
    u64::try_from(bounded).expect("the lower bound is not negative")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_is_the_upper_middle_element() {
        assert_eq!(median_time(&[]), None);
        assert_eq!(median_time(&[7]), Some(7));
        assert_eq!(median_time(&[9, 3]), Some(9));
        assert_eq!(median_time(&[5, 1, 3]), Some(3));
        assert_eq!(median_time(&[4, 2, 1, 3]), Some(3));
        // The median-time-past reads the newest 11 times only.
        let times: Vec<u32> = (0..28).rev().collect();
        assert_eq!(median_time_past(&times), Some(22));
        assert_eq!(median_time_past(&times[20..]), Some(4));
        assert_eq!(median_time_past(&[]), None);
    }

    #[test]
    fn timespan_bounds_of_both_spacings() {
        let pre = DifficultyParams::PRE_BLOSSOM;
        let post = DifficultyParams::POST_BLOSSOM;
        assert_eq!(averaging_window_timespan(&pre), 2_550);
        assert_eq!(averaging_window_timespan(&post), 1_275);
        // On target: no change.
        assert_eq!(bounded_timespan(&pre, 2_550), 2_550);
        assert_eq!(bounded_timespan(&post, 1_275), 1_275);
        // The damping divides the difference by 4 and truncates toward zero.
        assert_eq!(bounded_timespan(&pre, 2_550 + 7), 2_551);
        assert_eq!(bounded_timespan(&pre, 2_550 - 7), 2_549);
        assert_eq!(bounded_timespan(&pre, 2_550 - 3), 2_550);
        // The bounds: 84 % and 132 %, rounded down.
        assert_eq!(bounded_timespan(&pre, 0), 2_142);
        assert_eq!(bounded_timespan(&pre, -1_000_000), 2_142);
        assert_eq!(bounded_timespan(&pre, 1_000_000), 3_366);
        assert_eq!(bounded_timespan(&post, 0), 1_071);
        assert_eq!(bounded_timespan(&post, 1_000_000), 1_683);
        // The first actual timespans that reach each bound: damped = window + (a - w) / 4.
        assert_eq!(bounded_timespan(&pre, 2_550 - 4 * 408), 2_142);
        assert_eq!(bounded_timespan(&pre, 2_550 - 4 * 407), 2_143);
        assert_eq!(bounded_timespan(&pre, 2_550 + 4 * 816), 3_366);
        assert_eq!(bounded_timespan(&pre, 2_550 + 4 * 815), 3_365);
    }

    #[test]
    fn mean_target_is_the_floor_of_the_exact_mean() {
        // Nine targets of 2^216 and eight of 2^217: floor(25 * 2^216 / 17).
        let bits: Vec<u32> = (0..17)
            .map(|i| if i % 2 == 0 { 0x1c01_0000 } else { 0x1c02_0000 })
            .collect();
        let expected = (U256::from(25u8) << 216) / U256::from(17u8);
        assert_eq!(mean_target(&bits), Ok(expected));
        // 17 Regtest limits sum to 2^256 - 1: the mean does not overflow.
        assert_eq!(
            mean_target(&[0x200f_0f0f; 17]),
            Ok(target_from_compact(0x200f_0f0f).unwrap())
        );
        assert_eq!(
            mean_target(&[0x1f07_ffff, 0x1f80_0001]),
            Err(DifficultyError::InvalidContextBits(0x1f80_0001))
        );
    }

    /// Bitcoin's known values and the golden values of Zakura's
    /// `zakura-chain/src/work/difficulty/tests/vectors.rs` (`COMPACT_DIFFICULTY_CASES`).
    #[test]
    fn block_work_vectors() {
        assert_eq!(block_work(0x1d00_ffff), Some(U256::from(0x1_0001_0001u64)));
        assert_eq!(block_work(0x207f_ffff), Some(U256::from(2u64)));
        assert_eq!(block_work(0x2000_7fff), Some(U256::from(512u64)));
        // Mainnet limit 0x0007ffff << 216: floor(2^256 / (target + 1)) = 8192.
        assert_eq!(block_work(0x1f07_ffff), Some(U256::from(8_192u64)));
        let golden = [
            (
                0x0112_3456,
                "0d79435e50d79435e50d79435e50d79435e50d79435e50d79435e50d79435e50",
            ),
            (
                0x0200_8000,
                "01fc07f01fc07f01fc07f01fc07f01fc07f01fc07f01fc07f01fc07f01fc07f0",
            ),
            (
                0x0500_9234,
                "00000001c040c95a099201bcaf85db4e7f2e21e18707c8d55a887643b95afb2f",
            ),
            (
                0x0412_3456,
                "0000000e10005c64415f04ef3e387b97db388404db9fdfaab2b1918f6783471d",
            ),
        ];
        for (bits, work) in golden {
            let expected = U256::from_str_radix(work, 16).expect("hex");
            assert_eq!(block_work(bits), Some(expected), "{bits:#010x}");
        }
        for invalid in [0x0100_3456, 0x0492_3456, 0x0100_0001, 0x1d80_0000, 0] {
            assert_eq!(block_work(invalid), None, "{invalid:#010x}");
        }
    }

    #[test]
    fn compact_and_u256_round_trip() {
        for bits in [
            0x1d00_ffff,
            0x1f07_ffff,
            0x2007_ffff,
            0x200f_0f0f,
            0x1c01_7878,
        ] {
            let Some(target) = target_from_compact(bits) else {
                panic!("{bits:#x} decodes");
            };
            assert_eq!(compact_from_u256(target), bits);
        }
        assert_eq!(
            target_from_compact(0x0300_1234),
            Some(U256::from(0x1234u64))
        );
        assert_eq!(compact_from_u256(U256::from(0x80u64)), 0x0200_8000);
        // The compact form of the full limit is the network's compact limit.
        for network in Network::ALL {
            let params = network.params();
            let limit = U256::from_little_endian(&params.pow_limit);
            assert_eq!(compact_from_u256(limit), params.pow_limit_bits);
        }
    }
}
