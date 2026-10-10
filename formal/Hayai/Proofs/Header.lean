/-
Bridge proofs for `hayai-consensus-core::header_rules`: the translation decides the rules of
`Hayai.Spec.Header`.
-/
import Hayai.Core
import Hayai.Spec.Header
import Hayai.Proofs.Scalars
import Hayai.Proofs.Compact

open Aeneas Aeneas.Std Result
open HayaiCore HayaiCore.header_rules
open Hayai.Spec.Header Hayai.Spec.Difficulty Hayai.Proofs.Scalars Hayai.Proofs.Bytes
  Hayai.Proofs.Uint256 Hayai.Proofs.Compact

namespace Hayai.Proofs.Header

/-- §7.6, the version rule: `check_version` accepts `version` exactly when the version, read
as a signed 32-bit integer, is at least 4, and refuses it with `HeaderError.Version`
otherwise. A version above 4 passes: it has the rules of version 4. -/
theorem check_version_spec (version : U32) :
    check_version version ⦃ r =>
      r = if versionValid version.val then core.result.Result.Ok ()
        else core.result.Result.Err (HeaderError.Version version) ⦄ := by
  unfold check_version
  step as ⟨ i, hi ⟩
  have hb := version.hBounds
  simp only [UScalarTy.numBits] at hb
  simp only [versionValid, minBlockVersion, MIN_BLOCK_VERSION]
  split
  · rename_i hne
    rw [u32_bne_zero] at hne
    have : ¬ version.val < 2 ^ 31 := by
      intro hlt; apply hne; rw [hi, Nat.shiftRight_eq_div_pow]; omega
    rw [if_neg (by omega)]; simp
  · rename_i hne
    rw [u32_bne_zero, not_not] at hne
    have hlt : version.val < 2 ^ 31 := by
      rw [hi, Nat.shiftRight_eq_div_pow] at hne; omega
    split
    · have : ¬ 4 ≤ version.val := by scalar_tac
      rw [if_neg (by omega)]; simp
    · have : 4 ≤ version.val := by scalar_tac
      rw [if_pos (by omega)]; simp

/-- §7.6, the local rule: `check_local_time` accepts `time` exactly when it is at most two
hours after `now`, and refuses it with `HeaderError.TimeTooFarAhead` otherwise. The saturating
addition of the code does not change the rule: a `u32` time is never above the saturated
limit. -/
theorem check_local_time_spec (time now : U32) :
    check_local_time time now ⦃ r =>
      (r = core.result.Result.Ok () ↔ localTimeRule time.val now.val) ∧
      ∀ e, r = core.result.Result.Err e → ∃ limit, e = HeaderError.TimeTooFarAhead time limit ⦄ := by
  unfold check_local_time MAX_FUTURE_BLOCK_TIME_LOCAL
  step*
  have ht := time.hBounds
  simp only [UScalarTy.numBits] at ht
  simp only [lift, Std.bind_ok]
  have hl := u32_saturating_add_val now i
  split
  · rename_i hgt
    simp only [WP.spec_ok, reduceCtorEq, false_iff, localTimeRule, maxFutureBlockTimeLocal]
    refine ⟨by scalar_tac, fun e he => ⟨_, (core.result.Result.Err.inj he).symm⟩⟩
  · rename_i hgt
    simp only [WP.spec_ok, true_iff, localTimeRule, maxFutureBlockTimeLocal, reduceCtorEq,
      false_implies, implies_true, and_true]
    scalar_tac

/-- §7.7.2 and §7.7.3: `check_target` accepts `bits` exactly when its target is nonzero and
at most `PoWLimit` (the little-endian value of `pow_limit`), and returns that target. It
refuses `bits` that encode no target with `InvalidBits` and a target above the limit with
`TargetAboveLimit`. -/
theorem check_target_spec (bits : U32) (pow_limit : Array U8 32#usize) :
    check_target bits pow_limit ⦃ r => match r with
      | core.result.Result.Ok t =>
        targetWithinLimit (bytesVal pow_limit.val) bits.val ∧ toNat t = toTarget bits.val
      | core.result.Result.Err e =>
        ¬ targetWithinLimit (bytesVal pow_limit.val) bits.val ∧
        (e = HeaderError.InvalidBits bits ∨ e = HeaderError.TargetAboveLimit bits) ⦄ := by
  unfold check_target
  have hpl : bytesVal pow_limit.val < 2 ^ 256 := by
    have := leVal_lt (pow_limit.val.map U8.bv); simpa using this
  step with from_compact_spec as ⟨ o, ho ⟩
  rcases o with _ | t
  · simp only [WP.spec_ok, targetWithinLimit]
    refine ⟨?_, Or.inl trivial⟩
    rcases ho with h | h <;> omega
  · obtain ⟨ht, hpos, hlt⟩ := ho
    step with from_le_bytes_spec as ⟨ u, hu ⟩
    step with gt_spec as ⟨ b, hb' ⟩
    split
    · rename_i hgt
      simp only [WP.spec_ok, targetWithinLimit]
      rw [hb'] at hgt
      simp only [decide_eq_true_eq] at hgt
      rw [← ht, ← hu]
      exact ⟨by omega, Or.inr trivial⟩
    · rename_i hgt
      simp only [WP.spec_ok, targetWithinLimit]
      rw [hb'] at hgt
      simp only [decide_eq_true_eq, not_lt] at hgt
      rw [← ht, ← hu]
      exact ⟨⟨by omega, hgt⟩, rfl⟩

end Hayai.Proofs.Header
