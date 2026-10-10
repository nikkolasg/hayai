/-
Facts about the scalar operations of the Aeneas library that the bridge proofs use and the
library does not state.
-/
import Hayai.Core

open Aeneas Aeneas.Std

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

end Hayai.Proofs.Scalars
