/-
The difficulty rules of the Zcash protocol specification, §7.7.3 (difficulty adjustment),
§7.7.4 (nBits conversion) and §7.7.5 (work), as functions on natural numbers and integers.

Written from the specification, not from the code. Each definition names the item of the
specification it transcribes. The proofs of `Hayai.Proofs.Difficulty` show that the Lean
translation of `hayai-consensus-core::difficulty_rules` computes these functions.

Conventions:
- A target and a compact value are natural numbers. The specification writes
  `256^(size − 3)` as a rational power; for `size < 3` the product is rounded down
  (zcashd shifts right), see `toTarget`.
- The constants `PoWMedianBlockSpan = 11`, `PoWDampingFactor = 4`, `PoWMaxAdjustDown = 32/100`
  and `PoWMaxAdjustUp = 16/100` are those of §5.3.
-/
import Mathlib.Data.List.Sort
import Mathlib.Data.Nat.Log
import Mathlib.Tactic

namespace Hayai.Spec.Difficulty

/-! ## §5.3: constants -/

/-- `PoWMedianBlockSpan`. -/
def powMedianBlockSpan : ℕ := 11
/-- `PoWDampingFactor`. -/
def powDampingFactor : ℤ := 4
/-- `PoWMaxAdjustDown`, in percent (32/100). -/
def powMaxAdjustDownPercent : ℕ := 32
/-- `PoWMaxAdjustUp`, in percent (16/100). -/
def powMaxAdjustUpPercent : ℕ := 16

/-! ## §7.7.4: nBits conversion -/

/-- `bitlength(x)`: the number of bits of `x` without its leading zeros, 0 for 0. -/
def bitLength (x : ℕ) : ℕ := if x = 0 then 0 else Nat.log2 x + 1

/-- `size(x) := ⌈bitlength(x) / 8⌉`. -/
def size (x : ℕ) : ℕ := (bitLength x + 7) / 8

/-- `mantissa(x) := ⌊x · 256^(3 − size(x))⌋`. -/
def mantissa (x : ℕ) : ℕ :=
  if size x ≤ 3 then x * 256 ^ (3 - size x) else x / 256 ^ (size x - 3)

/-- `ToCompact(x)`. -/
def toCompact (x : ℕ) : ℕ :=
  if mantissa x < 2 ^ 23 then mantissa x + 2 ^ 24 * size x
  else mantissa x / 256 + 2 ^ 24 * (size x + 1)

/-- `ToTarget(x)`: 0 when the sign bit is set, else `(x ∧ (2^23 − 1)) · 256^(⌊x / 2^24⌋ − 3)`.
For an exponent below 3 the product is rounded down. -/
def toTarget (x : ℕ) : ℕ :=
  if x &&& 2 ^ 23 = 2 ^ 23 then 0
  else
    let m := x &&& (2 ^ 23 - 1)
    let e := x / 2 ^ 24
    if 3 ≤ e then m * 256 ^ (e - 3) else m / 256 ^ (3 - e)

/-! ## §7.7.3: difficulty adjustment -/

/-- `mean(S) := (Σ S_i) / length(S)`, rounded down (the target is an integer). -/
def mean (s : List ℕ) : ℕ := s.sum / s.length

/-- `median(S) := sorted(S)_⌈(length(S)+1)/2⌉` (1-based), that is the element at the 0-based
index `length(S) / 2`: for an even length, the element that begins the second half. -/
def median (s : List ℕ) : ℕ := (s.mergeSort (· ≤ ·))[s.length / 2]!

/-- The 1-based index `⌈(n + 1) / 2⌉` of §7.7.3 is the 0-based index `n / 2`. -/
theorem median_index (n : ℕ) : (n + 1 + 1) / 2 - 1 = n / 2 := by omega

/-- `bound_Lower^Upper(x) := max(Lower, min(Upper, x))`. -/
def bound (lower upper x : ℤ) : ℤ := max lower (min upper x)

/-- `AveragingWindowTimespan := PoWAveragingWindow · PoWTargetSpacing`, for the window and the
spacing of the height. -/
def averagingWindowTimespan (window spacing : ℕ) : ℕ := window * spacing

/-- `MinActualTimespan := ⌊AveragingWindowTimespan · (1 − PoWMaxAdjustUp)⌋`. -/
def minActualTimespan (awt : ℕ) : ℕ := awt * (100 - powMaxAdjustUpPercent) / 100

/-- `MaxActualTimespan := ⌊AveragingWindowTimespan · (1 + PoWMaxAdjustDown)⌋`. -/
def maxActualTimespan (awt : ℕ) : ℕ := awt * (100 + powMaxAdjustDownPercent) / 100

/-- `MedianTime(height)`: the median of the `nTime` of the `PoWMedianBlockSpan` blocks before
`height`, or of all of them when fewer exist. `times` lists the `nTime` of the blocks before
`height`, newest first. -/
def medianTime (times : List ℕ) : ℕ := median (times.take powMedianBlockSpan)

/-- `ActualTimespan(height) := MedianTime(height) − MedianTime(height − PoWAveragingWindow)`,
with `times` as in `medianTime`: the blocks before `height − PoWAveragingWindow` start at
index `window`. -/
def actualTimespan (window : ℕ) (times : List ℕ) : ℤ :=
  (medianTime times : ℤ) - (medianTime (times.drop window) : ℤ)

/-- `ActualTimespanDamped := AveragingWindowTimespan + trunc((ActualTimespan −
AveragingWindowTimespan) / PoWDampingFactor)`. `trunc` rounds toward zero: `Int.tdiv`. -/
def actualTimespanDamped (awt : ℕ) (actual : ℤ) : ℤ :=
  awt + Int.tdiv (actual - awt) powDampingFactor

/-- `ActualTimespanBounded := bound_MinActualTimespan^MaxActualTimespan(ActualTimespanDamped)`. -/
def actualTimespanBounded (awt : ℕ) (actual : ℤ) : ℤ :=
  bound (minActualTimespan awt) (maxActualTimespan awt) (actualTimespanDamped awt actual)

/-- `MeanTarget(height)` for `height > PoWAveragingWindow`: the mean of `ToTarget(nBits)` of the
`PoWAveragingWindow` blocks before `height`. `bits` lists the `nBits` of those blocks. -/
def meanTarget (bits : List ℕ) : ℕ := mean (bits.map toTarget)

/-- `Threshold(height)` for `height > PoWAveragingWindow`: `min(PoWLimit,
⌊MeanTarget / AveragingWindowTimespan⌋ · ActualTimespanBounded)`. The bounded timespan is not
negative (its lower bound is a natural number), so the product is a natural number. -/
def threshold (powLimit awt meanTarget : ℕ) (bounded : ℤ) : ℕ :=
  min powLimit (meanTarget / awt * bounded.toNat)

/-- `ThresholdBits(height) := ToCompact(Threshold(height))`, for `height > PoWAveragingWindow`,
from the `nTime` of the `PoWAveragingWindow + PoWMedianBlockSpan` blocks before `height`
(newest first) and the `nBits` of the `PoWAveragingWindow` blocks before `height`. -/
def thresholdBits (powLimit window spacing : ℕ) (times bits : List ℕ) : ℕ :=
  let awt := averagingWindowTimespan window spacing
  let bounded := actualTimespanBounded awt (actualTimespan window times)
  toCompact (threshold powLimit awt (meanTarget bits) bounded)

/-! ## §7.7.5: work -/

/-- The work of a block: `⌊2^256 / (ToTarget(nBits) + 1)⌋`. -/
def work (nBits : ℕ) : ℕ := 2 ^ 256 / (toTarget nBits + 1)

end Hayai.Spec.Difficulty
