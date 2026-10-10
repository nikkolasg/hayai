/-
`Uint256` of `hayai-consensus-core::difficulty_rules`: 4 little-endian 64-bit limbs. `toNat`
gives the integer, and each operation of the translation computes the integer operation.
-/
import Hayai.Core

open Aeneas Aeneas.Std Result
open HayaiCore
open HayaiCore.difficulty_rules

namespace Hayai.Proofs.Uint256

/-- The integer of 4 little-endian limbs. -/
def toNat (u : Array U64 4#usize) : ℕ :=
  u.val[0]!.val + 2 ^ 64 * u.val[1]!.val + 2 ^ 128 * u.val[2]!.val + 2 ^ 192 * u.val[3]!.val

theorem toNat_lt (u : Array U64 4#usize) : toNat u < 2 ^ 256 := by
  unfold toNat
  scalar_tac

theorem from_u64_spec (v : U64) : Uint256.from_u64 v ⦃ u => toNat u = v.val ⦄ := by
  unfold Uint256.from_u64
  simp [toNat, Array.make]

theorem ZERO_toNat : toNat Uint256.ZERO = 0 := by
  simp [toNat, Uint256.ZERO, Array.repeat]

theorem ONE_toNat : toNat Uint256.ONE = 1 := by
  simp [toNat, Uint256.ONE, Array.make]

/-- The integer of the first `k` limbs. -/
def low (u : Array U64 4#usize) : ℕ → ℕ
  | 0 => 0
  | k + 1 => low u k + 2 ^ (64 * k) * u.val[k]!.val

theorem low_four (u : Array U64 4#usize) : low u 4 = toNat u := by
  simp [low, toNat]

theorem low_set_of_le (u : Array U64 4#usize) (j : Usize) (x : U64) (k : ℕ) (h : k ≤ j.val) :
    low (u.set j x) k = low u k := by
  induction k with
  | zero => simp [low]
  | succ k ih =>
    simp only [low]
    rw [ih (by omega)]
    have : j.val ≠ k := by omega
    simp [Array.set_val_eq, this]

theorem low_set_succ (u : Array U64 4#usize) (j : Usize) (x : U64) (h : j.val < 4) :
    low (u.set j x) (j.val + 1) = low u j.val + 2 ^ (64 * j.val) * x.val := by
  simp only [low]
  rw [low_set_of_le u j x j.val (le_refl _)]
  have hl : j.val < u.val.length := by simp; omega
  simp only [Array.set_val_eq, List.getElem!_eq_getElem?_getD, List.getElem?_set_self hl,
    Option.getD_some]

theorem getElem_eq_getElem! (u : Array U64 4#usize) (k : ℕ) (h : k < u.val.length) :
    u.val[k] = u.val[k]! := by
  simp [List.getElem!_eq_getElem?_getD, List.getElem?_eq_getElem h]

theorem u64_size : U64.size = 2 ^ 64 := by simp [U64.size, U64.numBits]
theorem u64_umax : UScalar.max UScalarTy.U64 = 2 ^ 64 - 1 := by simp [U64.max_eq]

/-- Two chained `overflowing_add` calls: the low limb and the carry. -/
theorem add_carry (x y c s1 s2 : U64) (f1 f2 : Bool) (hc : c.val ≤ 1)
    (h1 : if x.val + y.val > UScalar.max UScalarTy.U64 then s1.val + U64.size = x.val + y.val ∧ f1 = true
      else s1.val = x.val + y.val ∧ f1 = false)
    (h2 : if s1.val + c.val > UScalar.max UScalarTy.U64 then s2.val + U64.size = s1.val + c.val ∧ f2 = true
      else s2.val = s1.val + c.val ∧ f2 = false) :
    (if f1 = true then 1 else 0) + (if f2 = true then 1 else 0) ≤ 1 ∧
    s2.val + 2 ^ 64 * ((if f1 = true then 1 else 0) + (if f2 = true then 1 else 0)) =
      x.val + y.val + c.val := by
  rw [u64_size, u64_umax] at h1 h2
  have hx := x.hBounds; have hy := y.hBounds; have hs1 := s1.hBounds
  simp only [UScalarTy.numBits] at hx hy hs1
  split at h1 <;> split at h2 <;> simp_all <;> omega

theorem checked_add_loop_spec (iter : core.ops.range.Range Usize)
    (a b limbs : Array U64 4#usize) (carry : U64)
    (hend : iter.end.val = 4) (hstart : iter.start.val ≤ 4) (hcarry : carry.val ≤ 1)
    (hinv : low limbs iter.start.val + 2 ^ (64 * iter.start.val) * carry.val =
      low a iter.start.val + low b iter.start.val) :
    Uint256.checked_add_loop iter a b limbs carry ⦃ (limbs1 : Array U64 4#usize) (carry1 : U64) =>
      carry1.val ≤ 1 ∧ toNat limbs1 + 2 ^ 256 * carry1.val = toNat a + toNat b ⦄ := by
  unfold Uint256.checked_add_loop
  apply loop.spec_decr_nat (measure := fun s => 4 - s.1.start.val)
    (inv := fun s => s.1.end.val = 4 ∧ s.1.start.val ≤ 4 ∧ s.2.1 = a ∧ s.2.2.1 = b ∧
      s.2.2.2.2.val ≤ 1 ∧
      low s.2.2.2.1 s.1.start.val + 2 ^ (64 * s.1.start.val) * s.2.2.2.2.val =
        low a s.1.start.val + low b s.1.start.val)
  · rintro ⟨it, a', b', limbs', carry'⟩ ⟨hend', hstart', rfl, rfl, hcarry', hinv'⟩
    simp only at hend' hstart' hcarry' hinv'
    simp only [Uint256.checked_add_loop.body]
    step as ⟨ o, it1, ho, hit1 ⟩
    by_cases hlt : it.start.val < it.end.val
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      have hk : it.start.val < 4 := by omega
      step as ⟨ i1, hi1 ⟩
      step as ⟨ i2, hi2 ⟩
      step as ⟨ s1, f1, h1 ⟩
      step as ⟨ s2, f2, h2 ⟩
      step as ⟨ na, hna ⟩
      simp only [lift, bind_tc_ok, core.convert.num.FromU64Bool.from]
      step as ⟨ c1, hc1 ⟩
      · split <;> split <;> scalar_tac
      · subst hna
        rw [hi1, getElem_eq_getElem!] at h1
        rw [hi2, getElem_eq_getElem!] at h1
        have he1 : it1.end.val = 4 := by rw [hit1]; exact hend'
        refine ⟨he1, by omega, ?_, ?_, by omega⟩
        · have := (add_carry _ _ _ _ _ _ _ hcarry' h1 h2).1
          split at hc1 <;> split at hc1 <;> simp_all
        · rw [hstart1, low_set_succ _ _ _ hk]
          simp only [low]
          have hp : 2 ^ (64 * (it.start.val + 1)) = 2 ^ (64 * it.start.val) * 2 ^ 64 := by
            rw [Nat.mul_add, pow_add]
          rw [hp]
          have hc : s2.val + 2 ^ 64 * c1.val = a'.val[it.start.val]!.val +
              b'.val[it.start.val]!.val + carry'.val := by
            have := (add_carry _ _ _ _ _ _ _ hcarry' h1 h2).2
            have hc1' : c1.val = (if f1 = true then 1 else 0) + (if f2 = true then 1 else 0) := by
              split at hc1 <;> split at hc1 <;> simp_all
            rw [hc1']; exact this
          calc low limbs' it.start.val + 2 ^ (64 * it.start.val) * s2.val +
                2 ^ (64 * it.start.val) * 2 ^ 64 * c1.val
              = low limbs' it.start.val + 2 ^ (64 * it.start.val) * carry'.val +
                2 ^ (64 * it.start.val) * (a'.val[it.start.val]!.val +
                  b'.val[it.start.val]!.val) := by
                have e : ∀ P : ℕ, P * s2.val + P * 2 ^ 64 * c1.val =
                    P * (s2.val + 2 ^ 64 * c1.val) := fun P => by ring
                rw [add_assoc, e, hc]; ring
            _ = _ := by rw [hinv']; ring
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      have h4 : it.start.val = 4 := by omega
      simp only [WP.spec_ok]
      rw [h4] at hinv'
      rw [← low_four limbs', ← low_four a', ← low_four b']
      exact ⟨hcarry', by simpa using hinv'⟩
  · exact ⟨hend, hstart, rfl, rfl, hcarry, hinv⟩


/-- `checked_add`: `none` exactly when the sum is at least `2^256`, else the sum. -/
@[step]
theorem checked_add_spec (a b : Array U64 4#usize) :
    Uint256.checked_add a b ⦃ r =>
      (r = none ↔ 2 ^ 256 ≤ toNat a + toNat b) ∧
      ∀ c, r = some c → toNat c = toNat a + toNat b ⦄ := by
  unfold Uint256.checked_add
  step with checked_add_loop_spec as ⟨ limbs1, carry, hcarry, hsum ⟩
  · simp [low]
  · have hlt := toNat_lt limbs1
    split
    · rename_i hne
      simp only [WP.spec_ok, true_iff, reduceCtorEq, false_implies, implies_true, and_true]
      have : carry.val ≠ 0 := by
        intro h0; apply absurd hne; simp [bne_iff_ne, ne_eq, UScalar.eq_equiv, h0]
      omega
    · rename_i hne
      have : carry.val = 0 := by
        by_contra h0; apply hne; simp [bne_iff_ne, ne_eq, UScalar.eq_equiv, h0]
      simp only [WP.spec_ok, reduceCtorEq, false_iff, not_le, Option.some.injEq, forall_eq']
      omega

end Hayai.Proofs.Uint256
