/-
The block header rules of the Zcash protocol specification, §7.6 (block header consensus)
and §7.7.2 (difficulty filter), that read no hash and no Equihash solution: the version,
the time against the median-time-past, `nBits` against `ThresholdBits`, and the target
against `PoWLimit`.

Written from the specification, not from the code. The proofs of `Hayai.Proofs.Header` show
that the Lean translation of `hayai-consensus-core::header_rules` decides these rules.
-/
import Hayai.Spec.Difficulty

namespace Hayai.Spec.Header

open Hayai.Spec.Difficulty

/-- §7.6: the block version number must be at least 4. The version is read as a signed
32-bit integer (zcashd): a value with the high bit set is negative. -/
def minBlockVersion : ℕ := 4

/-- §7.6: the version, as a signed 32-bit integer, is at least `minBlockVersion`. -/
def versionValid (version : ℕ) : Prop := version < 2 ^ 31 ∧ minBlockVersion ≤ version

instance (version : ℕ) : Decidable (versionValid version) := by
  unfold versionValid; infer_instance

/-- §7.6: the median-time-past of a block is the median of the `nTime` of the preceding
`PoWMedianBlockSpan` blocks, or of all preceding blocks when fewer exist. `times` lists the
`nTime` of the preceding blocks, newest first. -/
def medianTimePast (times : List ℕ) : ℕ := medianTime times

/-- §7.6: for each block other than the genesis block, `nTime` is strictly greater than the
median-time-past. -/
def timeAfterMedianTimePast (time mtp : ℕ) : Prop := mtp < time

/-- §7.6: 90 · 60 seconds. -/
def maxFutureBlockTimeMtp : ℕ := 90 * 60

/-- §7.6: from height 2 on Mainnet and from height 653,606 on Testnet (`maxTimeStart`),
`nTime` is at most the median-time-past plus 90 · 60 seconds. -/
def timeWithinMedianTimePast (maxTimeStart height time mtp : ℕ) : Prop :=
  maxTimeStart ≤ height → time ≤ mtp + maxFutureBlockTimeMtp

/-- §7.7.2 and §7.7.3: `nBits` encodes a target, and the target is at most `PoWLimit`, the
largest threshold of the chain. A zero target accepts no hash. -/
def targetWithinLimit (powLimit nBits : ℕ) : Prop :=
  0 < toTarget nBits ∧ toTarget nBits ≤ powLimit

/-- §7.6: `nBits` equals `ThresholdBits(height)`. -/
def bitsAsExpected (nBits expected : ℕ) : Prop := nBits = expected

/-- The contextual rules of a header at `height > 0`, with `expected` the `ThresholdBits` of
the height and `times` the `nTime` of the preceding blocks, newest first. -/
def contextualRules (powLimit maxTimeStart height version time nBits expected : ℕ)
    (times : List ℕ) : Prop :=
  versionValid version ∧
  targetWithinLimit powLimit nBits ∧
  timeAfterMedianTimePast time (medianTimePast times) ∧
  timeWithinMedianTimePast maxTimeStart height time (medianTimePast times) ∧
  bitsAsExpected nBits expected

/-- §7.6: two hours. -/
def maxFutureBlockTimeLocal : ℕ := 2 * 60 * 60

/-- §7.6: a full validator does not accept a block whose `nTime` is more than two hours after
its clock `now`. This is not a consensus rule: its value changes with the clock. -/
def localTimeRule (time now : ℕ) : Prop := time ≤ now + maxFutureBlockTimeLocal

instance (time now : ℕ) : Decidable (localTimeRule time now) := by
  unfold localTimeRule; infer_instance

end Hayai.Spec.Header
