//! Difficulty adjustment: the `nBits` that a block must have (protocol specification
//! §7.7.3, `ThresholdBits`), the Testnet minimum-difficulty rule (ZIP 205, ZIP 208 and
//! ZIP 218), the compact target form (§7.7.4) and the work of a block (§7.7.5).
//!
//! References: zcashd `pow.cpp` (`GetNextWorkRequired`, `CalculateNextWorkRequired`) and
//! Zakura `zakura-header-chain/src/validation/contextual/adjusted_difficulty.rs`.
//!
//! The parameters come from the rule set of `height` ([`DifficultyParams`]). The averaging
//! window `W` is 17 blocks before NU7 and 102 blocks from NU7 (ZIP 218). The rule for the
//! block at `height` with time `time`:
//!
//! 1. Testnet only, from height 299,188: when `time` is more than 450 s after the time of
//!    the parent (6 target spacings before NU7, 18 from NU7; 900 s before Blossom), the
//!    block must have the proof-of-work limit.
//! 2. When `height <= W`, the result is the proof-of-work limit.
//! 3. `MeanTarget` is the mean of the targets of the `W` blocks before `height`.
//! 4. `ActualTimespan` is `MedianTime(height) - MedianTime(height - W)`. `MedianTime(h)`
//!    is the median of the times of the 11 blocks before `h`.
//! 5. `ActualTimespanDamped` is `AveragingWindowTimespan + (ActualTimespan -
//!    AveragingWindowTimespan) / 4`, with a division that truncates toward zero.
//!    `AveragingWindowTimespan` is `W` target spacings of `height`.
//! 6. `ActualTimespanBounded` keeps the damped value between 84 % and 132 % of
//!    `AveragingWindowTimespan`.
//! 7. The target is `floor(MeanTarget / AveragingWindowTimespan) * ActualTimespanBounded`,
//!    at most the proof-of-work limit. The result is its compact form.
//!
//! The rule reads the times of the `W + 11` blocks before `height` and the `nBits` of the
//! `W` blocks before it: 28 and 17 before NU7, 113 and 102 from NU7. A shorter context is
//! [`DifficultyError::ContextTooShort`]: the function never computes a value from a part
//! of the window.
//!
//! The 256-bit arithmetic is [`Uint256`]: the operations of §7.7.3 to §7.7.5 only. The
//! adapter compares it with `primitive_types::U256`.

use core::cmp::Ordering;

use crate::rule_sets::{DifficultyParams, RuleSet};
use crate::{ConsensusError, CoreSpec, MEDIAN_TIME_SPAN};

/// A 256-bit unsigned integer: 4 little-endian 64-bit limbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Uint256(pub [u64; 4]);

/// The low and the high 64 bits of `x`.
fn split(x: u128) -> (u64, u64) {
    let bytes = x.to_le_bytes();
    let low = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    let high = u64::from_le_bytes([
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    ]);
    (low, high)
}

impl Uint256 {
    pub const ZERO: Self = Self([0; 4]);
    pub const ONE: Self = Self([1, 0, 0, 0]);
    /// `2^256 - 1`.
    pub const MAX: Self = Self([u64::MAX; 4]);

    pub const fn from_u64(value: u64) -> Self {
        Self([value, 0, 0, 0])
    }

    /// The integer whose little-endian bytes are `bytes`.
    pub fn from_le_bytes(bytes: &[u8; 32]) -> Self {
        let mut limbs = [0u64; 4];
        for i in 0..4 {
            let mut limb = [0u8; 8];
            for j in 0..8 {
                limb[j] = bytes[8 * i + j];
            }
            limbs[i] = u64::from_le_bytes(limb);
        }
        Self(limbs)
    }

    /// The little-endian bytes of the integer.
    pub fn to_le_bytes(self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        for i in 0..4 {
            let limb = self.0[i].to_le_bytes();
            for j in 0..8 {
                bytes[8 * i + j] = limb[j];
            }
        }
        bytes
    }

    /// The number of bits of the integer without its leading zeros: 0 for 0.
    pub fn bit_length(self) -> u32 {
        let mut i = 4;
        while i > 0 {
            i -= 1;
            if self.0[i] != 0 {
                let Ok(below) = u32::try_from(64 * i) else {
                    return 256;
                };
                return below + 64 - self.0[i].leading_zeros();
            }
        }
        0
    }

    /// `self + other`, or `None` above `2^256 - 1`.
    pub fn checked_add(self, other: Self) -> Option<Self> {
        let mut limbs = [0u64; 4];
        let mut carry = 0u64;
        for i in 0..4 {
            let (sum, first) = self.0[i].overflowing_add(other.0[i]);
            let (sum, second) = sum.overflowing_add(carry);
            limbs[i] = sum;
            carry = u64::from(first) + u64::from(second);
        }
        if carry != 0 {
            return None;
        }
        Some(Self(limbs))
    }

    /// `self - other`, or `None` below 0.
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        let mut limbs = [0u64; 4];
        let mut borrow = 0u64;
        for i in 0..4 {
            let (difference, first) = self.0[i].overflowing_sub(other.0[i]);
            let (difference, second) = difference.overflowing_sub(borrow);
            limbs[i] = difference;
            borrow = u64::from(first) + u64::from(second);
        }
        if borrow != 0 {
            return None;
        }
        Some(Self(limbs))
    }

    /// `self * factor`, or `None` above `2^256 - 1`.
    pub fn checked_mul_u64(self, factor: u64) -> Option<Self> {
        let mut limbs = [0u64; 4];
        let mut carry = 0u64;
        for i in 0..4 {
            // A 64-bit limb times a 64-bit factor plus a 64-bit carry fits in 128 bits.
            let product = u128::from(self.0[i]) * u128::from(factor) + u128::from(carry);
            let (low, high) = split(product);
            limbs[i] = low;
            carry = high;
        }
        if carry != 0 {
            return None;
        }
        Some(Self(limbs))
    }

    /// `(self / divisor, self % divisor)`.
    pub fn div_rem_u64(self, divisor: u64) -> Result<(Self, u64), ConsensusError> {
        if divisor == 0 {
            return Err(ConsensusError::DivisionByZero);
        }
        let mut limbs = [0u64; 4];
        let mut remainder = 0u64;
        let mut i = 4;
        while i > 0 {
            i -= 1;
            // The remainder is below the divisor, so the quotient of this step fits in 64
            // bits.
            let current = (u128::from(remainder) << 64) | u128::from(self.0[i]);
            let (quotient, _) = split(current / u128::from(divisor));
            let (rest, _) = split(current % u128::from(divisor));
            limbs[i] = quotient;
            remainder = rest;
        }
        Ok((Self(limbs), remainder))
    }

    /// `self * 2`, without the bit that leaves 256 bits.
    fn shl1(self) -> Self {
        let mut limbs = [0u64; 4];
        for i in 0..4 {
            limbs[i] = self.0[i] << 1;
            if i > 0 {
                limbs[i] |= self.0[i - 1] >> 63;
            }
        }
        Self(limbs)
    }

    /// The integer with bit `index` set.
    fn set_bit(self, index: u32) -> Self {
        let mut limbs = self.0;
        let Ok(limb) = usize::try_from(index / 64) else {
            return self;
        };
        if limb < 4 {
            limbs[limb] |= 1u64 << (index % 64);
        }
        Self(limbs)
    }

    /// `2^bits - 1`: the integer with the low `bits` bits set, for `bits` at most 256.
    fn ones(bits: u32) -> Self {
        let mut limbs = [0u64; 4];
        for i in 0..4 {
            let Ok(low) = u32::try_from(64 * i) else {
                continue;
            };
            if bits >= low + 64 {
                limbs[i] = u64::MAX;
            } else if bits > low {
                limbs[i] = (1u64 << (bits - low)) - 1;
            }
        }
        Self(limbs)
    }

    /// `((2^256 - 1) / divisor, (2^256 - 1) % divisor)`.
    ///
    /// A restoring division. The first `L - 1` steps of a long division by a divisor of `L`
    /// bits give quotient bits of 0, so the division starts with the remainder `2^(L-1) - 1`
    /// and runs `257 - L` steps.
    fn div_rem_max(divisor: Self) -> Result<(Self, Self), ConsensusError> {
        let length = divisor.bit_length();
        if length == 0 {
            return Err(ConsensusError::DivisionByZero);
        }
        let mut remainder = Self::ones(length - 1);
        let mut quotient = Self::ZERO;
        // The bit length of 4 limbs is at most 256.
        let Some(mut i) = 257u32.checked_sub(length) else {
            return Err(ConsensusError::Overflow);
        };
        while i > 0 {
            i -= 1;
            remainder = remainder.shl1();
            remainder.0[0] |= 1;
            if remainder >= divisor {
                let Some(rest) = remainder.checked_sub(divisor) else {
                    return Err(ConsensusError::Overflow);
                };
                remainder = rest;
                quotient = quotient.set_bit(i);
            }
        }
        Ok((quotient, remainder))
    }

    /// The target that the compact form `bits` encodes (zcashd `arith_uint256::SetCompact`).
    /// `None` when `bits` encode no target (negative, zero or overflow).
    ///
    /// Spec §7.7.4: `ToTarget`.
    pub fn from_compact(bits: u32) -> Option<Self> {
        let exponent = bits >> 24;
        let mantissa = bits & 0x007f_ffff;
        let negative = bits & 0x0080_0000 != 0;
        if negative || mantissa == 0 {
            return None;
        }
        // Overflow condition of arith_uint256::SetCompact.
        if exponent > 34
            || (mantissa > 0xff && exponent > 33)
            || (mantissa > 0xffff && exponent > 32)
        {
            return None;
        }
        let mut target = [0u8; 32];
        if exponent <= 3 {
            let shifted = mantissa >> (8 * (3 - exponent));
            if shifted == 0 {
                return None;
            }
            let bytes = shifted.to_le_bytes();
            for i in 0..4 {
                target[i] = bytes[i];
            }
        } else {
            let Ok(offset) = usize::try_from(exponent - 3) else {
                return None;
            };
            let bytes = mantissa.to_le_bytes();
            for i in 0..3 {
                // Bytes past the end are zero by the overflow condition above.
                if offset + i < 32 {
                    target[offset + i] = bytes[i];
                }
            }
        }
        Some(Self::from_le_bytes(&target))
    }

    /// The compact form of the integer (zcashd `arith_uint256::GetCompact`).
    ///
    /// Spec §7.7.4: `ToCompact`.
    pub fn to_compact(self) -> Result<u32, ConsensusError> {
        let bytes = self.to_le_bytes();
        let mut top: Option<usize> = None;
        for i in 0..32 {
            if bytes[i] != 0 {
                top = Some(i);
            }
        }
        let Some(top) = top else {
            return Ok(0);
        };
        let Ok(mut size) = u32::try_from(top + 1) else {
            return Err(ConsensusError::Overflow);
        };
        let mut mantissa = u32::from(bytes[top]) << 16;
        if top >= 1 {
            mantissa |= u32::from(bytes[top - 1]) << 8;
        }
        if top >= 2 {
            mantissa |= u32::from(bytes[top - 2]);
        }
        if mantissa & 0x0080_0000 != 0 {
            mantissa >>= 8;
            size += 1;
        }
        Ok(size << 24 | mantissa)
    }
}

impl Ord for Uint256 {
    fn cmp(&self, other: &Self) -> Ordering {
        let mut i = 4;
        while i > 0 {
            i -= 1;
            if self.0[i] != other.0[i] {
                return self.0[i].cmp(&other.0[i]);
            }
        }
        Ordering::Equal
    }
}

impl PartialOrd for Uint256 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

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
///
/// Spec §7.7.4: `ToTarget`.
pub fn target_from_compact(bits: u32) -> Option<Uint256> {
    Uint256::from_compact(bits)
}

/// The work of a block with target `bits`: `floor(2^256 / (target + 1))` (protocol
/// specification §7.7.5, the ZIP 221 field `nSubTreeTotalWork`). The cumulative work of a
/// chain is the sum of the work of its blocks. `None` when `bits` encode no target.
///
/// Spec §7.7.5: the work of a block is `floor(2^256 / (ToTarget(nBits) + 1))`.
pub fn block_work(bits: u32) -> Result<Option<Uint256>, ConsensusError> {
    let Some(target) = target_from_compact(bits) else {
        return Ok(None);
    };
    // `from_compact` bounds the target below 2^256 - 1, so `target + 1` does not overflow.
    let Some(divisor) = target.checked_add(Uint256::ONE) else {
        return Err(ConsensusError::Overflow);
    };
    // `floor(2^256 / d)` is `floor((2^256 - 1) / d)` plus 1 when `d` divides `2^256`.
    let (quotient, remainder) = Uint256::div_rem_max(divisor)?;
    let Some(next) = remainder.checked_add(Uint256::ONE) else {
        return Err(ConsensusError::Overflow);
    };
    if next != divisor {
        return Ok(Some(quotient));
    }
    let Some(work) = quotient.checked_add(Uint256::ONE) else {
        return Err(ConsensusError::Overflow);
    };
    Ok(Some(work))
}

/// The median of `times` as the specification defines it: the element at index
/// `floor(len / 2)` of the sorted list. `None` for an empty list.
///
/// Spec §7.7.3: `median(S)` is `sorted(S)` at the 1-based index `ceiling((len + 1) / 2)`.
/// The element at index `k` of the sorted list is the time `t` with fewer than `k + 1`
/// times below it and more than `k` times at or below it, so the function counts instead
/// of sorting: no allocation, and at most `len²` comparisons. The callers give at most
/// [`MEDIAN_TIME_SPAN`] times.
pub fn median_time(times: &[u32]) -> Option<u32> {
    let middle = times.len() / 2;
    let mut found: Option<u32> = None;
    for i in 0..times.len() {
        if let Some(_) = found {
            continue;
        }
        let candidate = times[i];
        let mut below = 0usize;
        let mut at_most = 0usize;
        for j in 0..times.len() {
            if times[j] < candidate {
                below += 1;
            }
            if times[j] <= candidate {
                at_most += 1;
            }
        }
        if below <= middle && middle < at_most {
            found = Some(candidate);
        }
    }
    found
}

/// The median-time-past of the header after `times` (newest first): the median of the
/// newest [`MEDIAN_TIME_SPAN`] times. `None` for an empty list.
pub fn median_time_past(times: &[u32]) -> Option<u32> {
    median_time(&times[..times.len().min(MEDIAN_TIME_SPAN)])
}

/// The times that the rule of the block at `height` reads: one per block before `height`,
/// at most `span`.
pub(crate) fn needed(height: u32, span: usize) -> usize {
    match usize::try_from(height) {
        Ok(height) => span.min(height),
        Err(_) => span,
    }
}

/// The `nBits` that the block at `chain.height` with time `time` must have on the chain of
/// `spec`. `rules` is the rule set of that height ([`crate::rule_sets::rules_at`]): the caller
/// selects it one time for every rule of the block.
///
/// Regtest has no such rule in hayai (`CoreSpec::disable_pow`): the header rules do not
/// call this function there.
pub fn expected_bits(
    spec: &CoreSpec,
    rules: &RuleSet,
    time: u32,
    chain: ParentChain<'_>,
) -> Result<u32, DifficultyError> {
    let height = chain.height;
    if height == 0 {
        return Err(DifficultyError::Genesis);
    }
    let params = &rules.difficulty;
    let short = |needed_times: usize, needed_bits: usize| ContextTooShort {
        times: chain.times.len(),
        needed_times,
        bits: chain.bits.len(),
        needed_bits,
    };

    // ZIP 205, ZIP 208, ZIP 218: from Testnet height 299,188, a block whose time is more
    // than 6 target spacings (18 from NU7) after its parent has `nBits` =
    // ToCompact(PoWLimit) (zcashd `nPowAllowMinDifficultyBlocksAfterHeight`).
    let min_difficulty = match spec.min_difficulty_start_height {
        Some(start) => height >= start,
        None => false,
    };
    if min_difficulty {
        if chain.times.len() == 0 {
            return Err(short(1, 0).into());
        }
        let parent_time = chain.times[0];
        let gap = i64::from(time) - i64::from(parent_time);
        let Some(allowed) = params
            .min_difficulty_gap_spacings
            .checked_mul(params.target_spacing)
        else {
            return Err(ConsensusError::Overflow.into());
        };
        if gap > i64::from(allowed) {
            return Ok(spec.pow_limit_bits);
        }
    }

    // Spec §7.7.3: `MeanTarget` is `PoWLimit` up to `PoWAveragingWindow`. hayai gives
    // `PoWLimit` as the threshold there, as zcashd and Zakura do (`adjusted_difficulty.rs:
    // 227-235`): the specification leaves `ActualTimespan` without a value at these heights.
    if height <= params.averaging_window {
        return Ok(spec.pow_limit_bits);
    }
    let Ok(window) = usize::try_from(params.averaging_window) else {
        return Err(ConsensusError::Overflow.into());
    };

    let needed_times = needed(height, window + MEDIAN_TIME_SPAN);
    if chain.times.len() < needed_times || chain.bits.len() < window {
        return Err(short(needed_times, window).into());
    }
    let times = &chain.times[..needed_times];
    // Spec §7.7.3: `MeanTarget` is the mean target of the `PoWAveragingWindow` blocks
    // before the height.
    let mean = mean_target(&chain.bits[..window])?;
    // Spec §7.7.3: `ActualTimespan` is `MedianTime(height) - MedianTime(height - W)`. The
    // height is above the window, so both spans hold a time.
    let (Some(newer), Some(older)) = (median_time_past(times), median_time(&times[window..]))
    else {
        return Err(short(needed_times, window).into());
    };
    let timespan = bounded_timespan(params, i64::from(newer) - i64::from(older))?;

    // Spec §7.7.3: `Threshold` is `min(PoWLimit, floor(MeanTarget /
    // AveragingWindowTimespan) * ActualTimespanBounded)`, and `ThresholdBits` its compact
    // form.
    let limit = Uint256::from_le_bytes(&spec.pow_limit);
    let (scaled, _) = mean.div_rem_u64(u64::from(averaging_window_timespan(params)?))?;
    // A product above 2^256 - 1 is above the limit.
    let target = match scaled.checked_mul_u64(timespan) {
        Some(target) => target.min(limit),
        None => limit,
    };
    Ok(target.to_compact()?)
}

/// `AveragingWindowTimespan`: the averaging window at the target spacing, in seconds.
fn averaging_window_timespan(params: &DifficultyParams) -> Result<u32, ConsensusError> {
    let Some(timespan) = params.averaging_window.checked_mul(params.target_spacing) else {
        return Err(ConsensusError::Overflow);
    };
    Ok(timespan)
}

/// `MeanTarget`: the mean of the targets of `bits`, rounded down.
///
/// The sum of the quotients plus the quotient of the sum of the remainders equals the
/// quotient of the sum, and no step exceeds 256 bits for any window length (Zakura
/// `mean_target_difficulty`).
fn mean_target(bits: &[u32]) -> Result<Uint256, DifficultyError> {
    let Ok(count) = u64::try_from(bits.len()) else {
        return Err(ConsensusError::Overflow.into());
    };
    let mut quotients = Uint256::ZERO;
    let mut remainders = Uint256::ZERO;
    for i in 0..bits.len() {
        let compact = bits[i];
        let Some(target) = target_from_compact(compact) else {
            return Err(DifficultyError::InvalidContextBits(compact));
        };
        let (quotient, remainder) = target.div_rem_u64(count)?;
        let Some(sum) = quotients.checked_add(quotient) else {
            return Err(ConsensusError::Overflow.into());
        };
        quotients = sum;
        let Some(sum) = remainders.checked_add(Uint256::from_u64(remainder)) else {
            return Err(ConsensusError::Overflow.into());
        };
        remainders = sum;
    }
    let (rest, _) = remainders.div_rem_u64(count)?;
    let Some(mean) = quotients.checked_add(rest) else {
        return Err(ConsensusError::Overflow.into());
    };
    Ok(mean)
}

/// `ActualTimespanBounded` of `actual` (`ActualTimespan`, in seconds).
///
/// Spec §7.7.3: `ActualTimespanDamped` with `trunc`, then the bounds `MinActualTimespan`
/// and `MaxActualTimespan` (84 % and 132 % of `AveragingWindowTimespan`, rounded down).
fn bounded_timespan(params: &DifficultyParams, actual: i64) -> Result<u64, ConsensusError> {
    let window = i64::from(averaging_window_timespan(params)?);
    // Rust's integer division truncates toward zero, as the specification requires.
    let Some(difference) = actual.checked_sub(window) else {
        return Err(ConsensusError::Overflow);
    };
    let Some(step) = difference.checked_div(i64::from(params.damping_factor)) else {
        return Err(ConsensusError::DivisionByZero);
    };
    let Some(damped) = window.checked_add(step) else {
        return Err(ConsensusError::Overflow);
    };
    let Some(down) = 100u32.checked_sub(params.max_adjust_up_percent) else {
        return Err(ConsensusError::Overflow);
    };
    let Some(up) = 100u32.checked_add(params.max_adjust_down_percent) else {
        return Err(ConsensusError::Overflow);
    };
    let (Some(min), Some(max)) = (
        window.checked_mul(i64::from(down)),
        window.checked_mul(i64::from(up)),
    ) else {
        return Err(ConsensusError::Overflow);
    };
    let (min, max) = (min / 100, max / 100);
    let bounded = if damped < min {
        min
    } else if damped > max {
        max
    } else {
        damped
    };
    // The lower bound is not negative.
    let Ok(bounded) = u64::try_from(bounded) else {
        return Err(ConsensusError::Overflow);
    };
    Ok(bounded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Uint256 {
        let mut bytes = [0u8; 32];
        let digits = text.as_bytes();
        assert_eq!(digits.len(), 64);
        for i in 0..32 {
            let value =
                u8::from_str_radix(core::str::from_utf8(&digits[2 * i..2 * i + 2]).unwrap(), 16)
                    .unwrap();
            bytes[31 - i] = value;
        }
        Uint256::from_le_bytes(&bytes)
    }

    #[test]
    fn median_is_the_upper_middle_element() {
        assert_eq!(median_time(&[]), None);
        assert_eq!(median_time(&[7]), Some(7));
        assert_eq!(median_time(&[9, 3]), Some(9));
        assert_eq!(median_time(&[5, 1, 3]), Some(3));
        assert_eq!(median_time(&[4, 2, 1, 3]), Some(3));
        // The median-time-past reads the newest 11 times only.
        let mut times: Vec<u32> = Vec::new();
        for i in 0..28 {
            times.push(27 - i);
        }
        assert_eq!(median_time_past(&times), Some(22));
        assert_eq!(median_time_past(&times[20..]), Some(4));
        assert_eq!(median_time_past(&[]), None);
    }

    /// The counting median equals the element at `len / 2` of the sorted list, with
    /// repeated times and every length up to 13.
    #[test]
    fn median_equals_the_middle_of_the_sorted_list() {
        let mut state = 0x9e37_79b9_u32;
        for len in 0..14 {
            for _ in 0..200 {
                let mut times = Vec::new();
                for _ in 0..len {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    times.push(state % 7);
                }
                let mut sorted = times.clone();
                sorted.sort_unstable();
                assert_eq!(
                    median_time(&times),
                    sorted.get(len / 2).copied(),
                    "{times:?}"
                );
            }
        }
    }

    #[test]
    fn timespan_bounds_of_both_spacings() {
        let pre = DifficultyParams::PRE_BLOSSOM;
        let post = DifficultyParams::POST_BLOSSOM;
        assert_eq!(averaging_window_timespan(&pre), Ok(2_550));
        assert_eq!(averaging_window_timespan(&post), Ok(1_275));
        // On target: no change.
        assert_eq!(bounded_timespan(&pre, 2_550), Ok(2_550));
        assert_eq!(bounded_timespan(&post, 1_275), Ok(1_275));
        // The damping divides the difference by 4 and truncates toward zero.
        assert_eq!(bounded_timespan(&pre, 2_550 + 7), Ok(2_551));
        assert_eq!(bounded_timespan(&pre, 2_550 - 7), Ok(2_549));
        assert_eq!(bounded_timespan(&pre, 2_550 - 3), Ok(2_550));
        // The bounds: 84 % and 132 %, rounded down.
        assert_eq!(bounded_timespan(&pre, 0), Ok(2_142));
        assert_eq!(bounded_timespan(&pre, -1_000_000), Ok(2_142));
        assert_eq!(bounded_timespan(&pre, 1_000_000), Ok(3_366));
        assert_eq!(bounded_timespan(&post, 0), Ok(1_071));
        assert_eq!(bounded_timespan(&post, 1_000_000), Ok(1_683));
        // The first actual timespans that reach each bound: damped = window + (a - w) / 4.
        assert_eq!(bounded_timespan(&pre, 2_550 - 4 * 408), Ok(2_142));
        assert_eq!(bounded_timespan(&pre, 2_550 - 4 * 407), Ok(2_143));
        assert_eq!(bounded_timespan(&pre, 2_550 + 4 * 816), Ok(3_366));
        assert_eq!(bounded_timespan(&pre, 2_550 + 4 * 815), Ok(3_365));
        // Parameters that no rule set has: the errors of the arithmetic.
        let zero_damping = DifficultyParams {
            damping_factor: 0,
            ..pre
        };
        assert_eq!(
            bounded_timespan(&zero_damping, 0),
            Err(ConsensusError::DivisionByZero)
        );
        let huge_window = DifficultyParams {
            averaging_window: u32::MAX,
            ..pre
        };
        assert_eq!(
            bounded_timespan(&huge_window, 0),
            Err(ConsensusError::Overflow)
        );
        let huge_up = DifficultyParams {
            max_adjust_up_percent: 101,
            ..pre
        };
        assert_eq!(bounded_timespan(&huge_up, 0), Err(ConsensusError::Overflow));
    }

    #[test]
    fn mean_target_is_the_floor_of_the_exact_mean() {
        // Nine targets of 2^216 and eight of 2^217: floor(25 * 2^216 / 17).
        let mut bits: Vec<u32> = Vec::new();
        for i in 0..17 {
            bits.push(if i % 2 == 0 { 0x1c01_0000 } else { 0x1c02_0000 });
        }
        let (expected, _) = Uint256::from_u64(25)
            .checked_mul_u64(1 << 56)
            .unwrap()
            .checked_mul_u64(1 << 56)
            .unwrap()
            .checked_mul_u64(1 << 56)
            .unwrap()
            .checked_mul_u64(1 << 48)
            .unwrap()
            .div_rem_u64(17)
            .unwrap();
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
        assert_eq!(
            mean_target(&[]),
            Err(DifficultyError::Rules(ConsensusError::DivisionByZero))
        );
    }

    /// Bitcoin's known values and the golden values of Zakura's
    /// `zakura-chain/src/work/difficulty/tests/vectors.rs` (`COMPACT_DIFFICULTY_CASES`).
    #[test]
    fn block_work_vectors() {
        assert_eq!(
            block_work(0x1d00_ffff),
            Ok(Some(Uint256::from_u64(0x1_0001_0001)))
        );
        assert_eq!(block_work(0x207f_ffff), Ok(Some(Uint256::from_u64(2))));
        assert_eq!(block_work(0x2000_7fff), Ok(Some(Uint256::from_u64(512))));
        // Mainnet limit 0x0007ffff << 216: floor(2^256 / (target + 1)) = 8192.
        assert_eq!(block_work(0x1f07_ffff), Ok(Some(Uint256::from_u64(8_192))));
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
            assert_eq!(block_work(bits), Ok(Some(hex(work))), "{bits:#010x}");
        }
        for invalid in [0x0100_3456, 0x0492_3456, 0x0100_0001, 0x1d80_0000, 0] {
            assert_eq!(block_work(invalid), Ok(None), "{invalid:#010x}");
        }
        // The smallest target, 1: the work is 2^255.
        assert_eq!(
            block_work(0x0101_0000),
            Ok(Some(Uint256([0, 0, 0, 1 << 63])))
        );
    }

    #[test]
    fn compact_round_trip_and_known_targets() {
        for bits in [
            0x1d00_ffff,
            0x1f07_ffff,
            0x2007_ffff,
            0x200f_0f0f,
            0x1c01_7878,
            0x2100_ff00,
        ] {
            let Some(target) = target_from_compact(bits) else {
                panic!("{bits:#x} decodes");
            };
            assert_eq!(target.to_compact(), Ok(bits));
        }
        // The compact form is not unique: the canonical form has no zero top byte.
        let Some(top_byte) = target_from_compact(0x2200_00ff) else {
            panic!("one byte fits at exponent 34");
        };
        assert_eq!(top_byte, target_from_compact(0x2100_ff00).unwrap());
        assert_eq!(top_byte.to_le_bytes()[31], 0xff);
        assert_eq!(
            target_from_compact(0x0300_1234),
            Some(Uint256::from_u64(0x1234))
        );
        assert_eq!(
            target_from_compact(0x0200_1234),
            Some(Uint256::from_u64(0x12))
        );
        assert_eq!(Uint256::from_u64(0x80).to_compact(), Ok(0x0200_8000));
        assert_eq!(Uint256::ZERO.to_compact(), Ok(0));
        assert_eq!(target_from_compact(0x1f80_0001), None, "negative");
        assert_eq!(target_from_compact(0x1f00_0000), None, "zero mantissa");
        assert_eq!(target_from_compact(0xff00_0001), None, "overflow");
        assert_eq!(
            target_from_compact(0x2200_01ff),
            None,
            "two bytes at exponent 34"
        );
        // The Mainnet limit: 0x0007ffff << 216.
        let mainnet = target_from_compact(0x1f07_ffff).unwrap();
        let mut expected = [0u8; 32];
        expected[28] = 0xff;
        expected[29] = 0xff;
        expected[30] = 0x07;
        assert_eq!(mainnet.to_le_bytes(), expected);
        assert_eq!(Uint256::from_le_bytes(&expected), mainnet);
    }

    #[test]
    fn the_arithmetic_of_uint256() {
        let max = Uint256::MAX;
        assert_eq!(max.checked_add(Uint256::ONE), None);
        assert_eq!(Uint256::ZERO.checked_sub(Uint256::ONE), None);
        assert_eq!(max.checked_sub(max), Some(Uint256::ZERO));
        let carry = Uint256([u64::MAX, 0, 0, 0]);
        assert_eq!(carry.checked_add(Uint256::ONE), Some(Uint256([0, 1, 0, 0])));
        assert_eq!(Uint256([0, 1, 0, 0]).checked_sub(Uint256::ONE), Some(carry));
        assert_eq!(
            carry.checked_mul_u64(2),
            Some(Uint256([u64::MAX - 1, 1, 0, 0]))
        );
        assert_eq!(max.checked_mul_u64(2), None);
        assert_eq!(max.checked_mul_u64(1), Some(max));
        assert_eq!(
            Uint256([0, 1, 0, 0]).div_rem_u64(3),
            Ok((Uint256([0x5555_5555_5555_5555, 0, 0, 0]), 1))
        );
        assert_eq!(max.div_rem_u64(1), Ok((max, 0)));
        assert_eq!(
            Uint256::ONE.div_rem_u64(0),
            Err(ConsensusError::DivisionByZero)
        );
        assert!(Uint256([0, 0, 0, 1]) > Uint256([u64::MAX, u64::MAX, u64::MAX, 0]));
        assert!(Uint256([5, 0, 0, 0]) < Uint256([6, 0, 0, 0]));
        assert_eq!(
            Uint256([5, 0, 0, 0]).cmp(&Uint256([5, 0, 0, 0])),
            Ordering::Equal
        );
        assert_eq!(Uint256::ZERO.bit_length(), 0);
        assert_eq!(Uint256::ONE.bit_length(), 1);
        assert_eq!(Uint256([0, 1, 0, 0]).bit_length(), 65);
        assert_eq!(max.bit_length(), 256);
        assert_eq!(Uint256::ones(0), Uint256::ZERO);
        assert_eq!(Uint256::ones(64), Uint256([u64::MAX, 0, 0, 0]));
        assert_eq!(Uint256::ones(65), Uint256([u64::MAX, 1, 0, 0]));
        assert_eq!(Uint256::ones(256), max);
        assert_eq!(Uint256::ZERO.set_bit(255), Uint256([0, 0, 0, 1 << 63]));
        assert_eq!(Uint256::ONE.shl1(), Uint256([2, 0, 0, 0]));
        assert_eq!(Uint256([1 << 63, 0, 0, 0]).shl1(), Uint256([0, 1, 0, 0]));
        assert_eq!(
            Uint256::div_rem_max(Uint256::ZERO),
            Err(ConsensusError::DivisionByZero)
        );
        assert_eq!(Uint256::div_rem_max(Uint256::ONE), Ok((max, Uint256::ZERO)));
        assert_eq!(
            Uint256::div_rem_max(Uint256::from_u64(2)),
            Ok((
                Uint256([u64::MAX, u64::MAX, u64::MAX, u64::MAX >> 1]),
                Uint256::ONE
            ))
        );
        assert_eq!(
            Uint256::div_rem_max(Uint256::from_u64(10)),
            Ok((max.div_rem_u64(10).unwrap().0, Uint256::from_u64(5)))
        );
        assert_eq!(Uint256::div_rem_max(max), Ok((Uint256::ONE, Uint256::ZERO)));
    }
}
