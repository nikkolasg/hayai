/-
Facts about the scalar operations of the Aeneas library that the bridge proofs use and the
library does not state.
-/
import Hayai.Core

open Aeneas Aeneas.Std Result

namespace Hayai.Proofs.Scalars

theorem u32_bne_zero (x : U32) : (x != 0#u32) = true ↔ x.val ≠ 0 := by
  simp [bne_iff_ne, ne_eq, UScalar.eq_equiv]

theorem u64_bne_zero (x : U64) : (x != 0#u64) = true ↔ x.val ≠ 0 := by
  simp [bne_iff_ne, ne_eq, UScalar.eq_equiv]

/-- `u32::saturating_add`: the sum, at most `2^32 - 1`. -/
theorem u32_saturating_add_val (x y : U32) :
    (core.num.U32.saturating_add x y).val = min (2 ^ 32 - 1) (x.val + y.val) := by
  simp only [core.num.U32.saturating_add, UScalar.saturating_add, UScalar.val, BitVec.toNat_ofNat,
    UScalar.max, UScalarTy.numBits]
  have : min (2 ^ 32 - 1) (x.bv.toNat + y.bv.toNat) < 2 ^ 32 := by omega
  rw [Nat.mod_eq_of_lt (by simpa using this)]

/-- `i64::from(u32)` (`FunsExternal.lean`): the same integer. -/
@[step] theorem i64_from_u32_spec (x : U32) :
    I64.Insts.CoreConvertFromU32.from x ⦃ y => y.val = x.val ⦄ := by
  unfold I64.Insts.CoreConvertFromU32.from
  simp only [WP.spec_ok, UScalar.hcast_val_eq]
  have := x.hBounds
  simp only [UScalarTy.numBits, IScalarTy.numBits] at *
  rw [Int.bmod_eq_of_le] <;> omega

/-- `u64::try_from(i64)` (`FunsExternal.lean`): `Ok` of the same integer when it is not
negative. -/
@[step] theorem u64_try_from_i64_spec (x : I64) :
    U64.Insts.CoreConvertTryFromI64TryFromIntError.try_from x ⦃ r =>
      (0 ≤ x.val → ∃ y : U64, r = core.result.Result.Ok y ∧ (y.val : ℤ) = x.val) ∧
      (x.val < 0 → ∃ e, r = core.result.Result.Err e) ⦄ := by
  unfold U64.Insts.CoreConvertTryFromI64TryFromIntError.try_from tryFromIScalar
  have := x.hBounds
  split
  · rename_i h
    simp only [WP.spec_ok]
    refine ⟨fun _ => ⟨_, rfl, ?_⟩, fun hn => by omega⟩
    simp only [IScalar.hcast_val_eq, IScalarTy.numBits, UScalarTy.numBits] at *
    rw [Int.emod_eq_of_lt] <;> simp at h <;> omega
  · rename_i h
    simp only [WP.spec_ok]
    refine ⟨fun h0 => ?_, fun _ => by simp⟩
    exfalso; apply h; refine ⟨h0, ?_⟩
    simp only [UScalar.max, UScalarTy.numBits, IScalarTy.numBits] at *; omega

/-- `Int.tdiv` never grows the absolute value (Mathlib `Int.natAbs_tdiv_le_natAbs`). -/
theorem tdiv_bounds (x d : ℤ) : -(x.natAbs : ℤ) ≤ x.tdiv d ∧ x.tdiv d ≤ x.natAbs := by
  have := Int.natAbs_tdiv_le_natAbs x d
  omega

end Hayai.Proofs.Scalars
