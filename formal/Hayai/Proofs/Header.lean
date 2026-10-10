/-
Bridge proofs for `hayai-consensus-core::header_rules`: the translation decides the rules of
`Hayai.Spec.Header`.
-/
import Hayai.Core
import Hayai.Spec.Header
import Hayai.Proofs.Scalars

open Aeneas Aeneas.Std Result
open HayaiCore HayaiCore.header_rules
open Hayai.Spec.Header Hayai.Proofs.Scalars

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

end Hayai.Proofs.Header
