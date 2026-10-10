/-
`Uint256` of `hayai-consensus-core::difficulty_rules`: 4 little-endian 64-bit limbs. `toNat`
gives the integer, and each operation of the translation computes the integer operation.
-/
import Hayai.Core
import Hayai.Proofs.Bytes
import Mathlib.Tactic

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

open Hayai.Proofs.Bytes in
/-- The inner loop of `from_le_bytes` copies the 8 bytes of limb `i`. -/
theorem from_le_bytes_inner_spec (bytes : Array U8 32#usize) (i : Usize) (hi : i.val < 4)
    (iter : core.ops.range.Range Usize) (limb : Array U8 8#usize)
    (hend : iter.end.val = 8) (hs : iter.start.val ≤ 8)
    (hl : ∀ k < 8, limb.val[k]! = if k < iter.start.val then bytes.val[8 * i.val + k]! else 0#u8) :
    Uint256.from_le_bytes_loop0_loop0 iter bytes i limb ⦃ limb1 =>
      ∀ k < 8, limb1.val[k]! = bytes.val[8 * i.val + k]! ⦄ := by
  unfold Uint256.from_le_bytes_loop0_loop0
  apply loop.spec_decr_nat
    (measure := fun (s : core.ops.range.Range Usize × Array U8 8#usize) => 8 - s.1.start.val)
    (inv := fun (s : core.ops.range.Range Usize × Array U8 8#usize) =>
      s.1.end.val = 8 ∧ s.1.start.val ≤ 8 ∧
      ∀ k < 8, s.2.val[k]! = if k < s.1.start.val then bytes.val[8 * i.val + k]! else 0#u8)
  · rintro ⟨it, lb⟩ ⟨hend', hs', hl'⟩
    simp only at hend' hs' hl'
    unfold Uint256.from_le_bytes_loop0_loop0.body
    step as ⟨ o, it1, ho, hit1 ⟩
    by_cases hlt : it.start.val < it.end.val
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      step as ⟨ i1, hi1 ⟩
      step as ⟨ i2, hi2 ⟩
      step as ⟨ x, hx ⟩
      step as ⟨ lb1, hlb1 ⟩
      have he1 : it1.end.val = 8 := by rw [hit1]; exact hend'
      refine ⟨he1, by omega, ?_, by omega⟩
      intro k hk
      rw [hlb1, Array.set_val_eq]
      by_cases hke : k = it.start.val
      · subst hke
        have hlen : it.start.val < lb.val.length := by simp; omega
        simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_self hlen, Option.getD_some]
        rw [if_pos (by omega), hx]
        have : 8 * i.val + it.start.val < bytes.val.length := by simp; omega
        simp [List.getElem?_eq_getElem this, hi2, hi1]
      · have hne : it.start.val ≠ k := fun h => hke h.symm
        simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_ne hne]
        rw [← List.getElem!_eq_getElem?_getD, hl' k hk]
        by_cases hkl : k < it.start.val
        · rw [if_pos hkl, if_pos (by omega)]; exact List.getElem!_eq_getElem?_getD
        · rw [if_neg hkl, if_neg (by omega)]
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only [WP.spec_ok]
      intro k hk
      rw [hl' k hk, if_pos (by omega)]
  · exact ⟨hend, hs, hl⟩

open Hayai.Proofs.Bytes in
/-- The value of limb `m` of the bytes `bytes`. -/
def limbVal (bytes : Array U8 32#usize) (m : ℕ) : ℕ :=
  ∑ k ∈ Finset.range 8, bytes.val[8 * m + k]!.val * 256 ^ k

open Hayai.Proofs.Bytes in
theorem from_le_bytes_outer_spec (bytes : Array U8 32#usize)
    (iter : core.ops.range.Range Usize) (limbs : Array U64 4#usize)
    (hend : iter.end.val = 4) (hs : iter.start.val ≤ 4)
    (hl : ∀ m < 4, limbs.val[m]!.val = if m < iter.start.val then limbVal bytes m else 0) :
    Uint256.from_le_bytes_loop0 iter bytes limbs ⦃ limbs1 =>
      ∀ m < 4, limbs1.val[m]!.val = limbVal bytes m ⦄ := by
  unfold Uint256.from_le_bytes_loop0
  apply loop.spec_decr_nat
    (measure := fun (s : core.ops.range.Range Usize × Array U64 4#usize) => 4 - s.1.start.val)
    (inv := fun (s : core.ops.range.Range Usize × Array U64 4#usize) =>
      s.1.end.val = 4 ∧ s.1.start.val ≤ 4 ∧
      ∀ m < 4, s.2.val[m]!.val = if m < s.1.start.val then limbVal bytes m else 0)
  · rintro ⟨it, lb⟩ ⟨hend', hs', hl'⟩
    simp only at hend' hs' hl'
    unfold Uint256.from_le_bytes_loop0.body
    step as ⟨ o, it1, ho, hit1 ⟩
    by_cases hlt : it.start.val < it.end.val
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      have hk4 : it.start.val < 4 := by omega
      step with from_le_bytes_inner_spec as ⟨ limb1, hlimb1 ⟩
      case hl => intro k hk; simp [Array.repeat_val, hk]; interval_cases k <;> rfl
      simp only [lift, Std.bind_ok]
      step as ⟨ a, ha ⟩
      have he1 : it1.end.val = 4 := by rw [hit1]; exact hend'
      have hv : (core.num.U64.from_le_bytes limb1).val = limbVal bytes it.start.val := by
        rw [u64_from_le_bytes_val, bytesVal_eq_sum]
        simp only [Array.length_eq, limbVal]
        apply Finset.sum_congr (by simp); intro k hk
        rw [hlimb1 k (by simpa using hk)]
      refine ⟨he1, by omega, ?_, by omega⟩
      intro m hm
      rw [ha, Array.set_val_eq]
      by_cases hme : m = it.start.val
      · subst hme
        have hlen : it.start.val < lb.val.length := by simp; omega
        simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_self hlen, Option.getD_some]
        rw [hv, if_pos (by omega)]
      · have hne : it.start.val ≠ m := fun h => hme h.symm
        simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_ne hne]
        rw [← List.getElem!_eq_getElem?_getD, hl' m hm]
        by_cases hml : m < it.start.val
        · rw [if_pos hml, if_pos (by omega)]
        · rw [if_neg hml, if_neg (by omega)]
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only [WP.spec_ok]
      intro m hm
      rw [hl' m hm, if_pos (by omega)]
  · exact ⟨hend, hs, hl⟩

open Hayai.Proofs.Bytes in
/-- `from_le_bytes`: the integer whose little-endian bytes are `bytes`. -/
@[step]
theorem from_le_bytes_spec (bytes : Array U8 32#usize) :
    Uint256.from_le_bytes bytes ⦃ u => toNat u = bytesVal bytes.val ⦄ := by
  unfold Uint256.from_le_bytes
  step with from_le_bytes_outer_spec as ⟨ limbs, hlimbs ⟩
  case hl => intro m hm; simp [Array.repeat_val, hm]; interval_cases m <;> rfl
  rw [bytesVal_eq_sum]
  simp only [toNat, hlimbs 0 (by omega), hlimbs 1 (by omega), hlimbs 2 (by omega),
    hlimbs 3 (by omega), limbVal, Array.length_eq]
  rw [show ((32#usize : Usize) : ℕ) = 32 from rfl]
  simp only [Finset.sum_range_succ, Finset.sum_range_zero, Nat.reduceMul, Nat.reduceAdd,
    Nat.reducePow, zero_add, mul_zero, pow_zero, mul_one]
  ring

/-- When the limbs above `m` are equal and limb `m` differs, limb `m` decides the order. -/
theorem compare_at (a b : Array U64 4#usize) (m : ℕ) (hm : m < 4)
    (heq : ∀ j, m < j → j < 4 → a.val[j]!.val = b.val[j]!.val)
    (hne : a.val[m]!.val ≠ b.val[m]!.val) :
    compare (toNat a) (toNat b) = compare a.val[m]!.val b.val[m]!.val := by
  have ha0 := a.val[0]!.hBounds; have ha1 := a.val[1]!.hBounds
  have ha2 := a.val[2]!.hBounds; have ha3 := a.val[3]!.hBounds
  have hb0 := b.val[0]!.hBounds; have hb1 := b.val[1]!.hBounds
  have hb2 := b.val[2]!.hBounds; have hb3 := b.val[3]!.hBounds
  simp only [UScalarTy.numBits] at *
  unfold toNat
  rcases Nat.lt_or_gt_of_ne hne with h | h
  · rw [(compare_lt_iff_lt).mpr h, compare_lt_iff_lt]
    interval_cases m <;> simp_all <;> omega
  · rw [(compare_gt_iff_gt).mpr h, compare_gt_iff_gt]
    interval_cases m <;> simp_all <;> omega

theorem cmp_loop_spec (a b : Array U64 4#usize) (i : Usize) (hi : i.val ≤ 4)
    (heq : ∀ j, i.val ≤ j → j < 4 → a.val[j]!.val = b.val[j]!.val) :
    Uint256.Insts.CoreCmpOrd.cmp_loop a b i ⦃ o => o = compare (toNat a) (toNat b) ⦄ := by
  unfold Uint256.Insts.CoreCmpOrd.cmp_loop
  apply loop.spec_decr_nat
    (measure := fun (s : Array U64 4#usize × Array U64 4#usize × Usize) => s.2.2.val)
    (inv := fun (s : Array U64 4#usize × Array U64 4#usize × Usize) =>
      s.1 = a ∧ s.2.1 = b ∧ s.2.2.val ≤ 4 ∧
      ∀ j, s.2.2.val ≤ j → j < 4 → a.val[j]!.val = b.val[j]!.val)
  · rintro ⟨a', b', k⟩ ⟨rfl, rfl, hk, hk'⟩
    simp only at hk hk'
    unfold Uint256.Insts.CoreCmpOrd.cmp_loop.body
    simp only
    split
    · step as ⟨ k1, hk1 ⟩
      step as ⟨ x, hx ⟩
      step as ⟨ y, hy ⟩
      have hl : k1.val < a'.val.length := by simp; omega
      have hx' : x.val = a'.val[k1.val]!.val := by rw [hx, getElem!_pos a'.val k1.val hl]
      have hl' : k1.val < b'.val.length := by simp; omega
      have hy' : y.val = b'.val[k1.val]!.val := by rw [hy, getElem!_pos b'.val k1.val hl']
      split
      · rename_i hne
        simp only [lift, Std.bind_ok, WP.spec_ok, core.cmp.impls.OrdU64.cmp]
        have hne' : x.val ≠ y.val := by
          intro h; apply absurd hne; simp [bne_iff_ne, ne_eq, UScalar.eq_equiv, h]
        rw [compare_at a' b' k1.val (by omega) (fun j hj hj' => hk' j (by omega) hj')
          (by rw [← hx', ← hy']; exact hne'), hx', hy']
      · rename_i heq'
        simp only [WP.spec_ok]
        have : x.val = y.val := by
          by_contra h; apply heq'; simp [bne_iff_ne, ne_eq, UScalar.eq_equiv, h]
        refine ⟨trivial, trivial, by omega, ?_, by omega⟩
        intro j hj hj'
        by_cases hje : j = k1.val
        · subst hje; rw [← hx', ← hy']; exact this
        · exact hk' j (by omega) hj'
    · simp only [WP.spec_ok]
      have h0 : k.val = 0 := by scalar_tac
      have : toNat a' = toNat b' := by
        unfold toNat
        rw [hk' 0 (by omega) (by omega), hk' 1 (by omega) (by omega),
          hk' 2 (by omega) (by omega), hk' 3 (by omega) (by omega)]
      rw [this]; exact ((compare_eq_iff_eq (α := ℕ)).mpr rfl).symm
  · exact ⟨rfl, rfl, hi, heq⟩

@[step]
theorem cmp_spec (a b : Array U64 4#usize) :
    Uint256.Insts.CoreCmpOrd.cmp a b ⦃ o => o = compare (toNat a) (toNat b) ⦄ :=
  cmp_loop_spec a b 4#usize (by simp) (fun j hj hj' => by simp at hj; omega)

@[step]
theorem gt_spec (a b : Array U64 4#usize) :
    core.cmp.PartialOrd.gt.trait_default Uint256.Insts.CoreCmpPartialOrdUint256 a b
      ⦃ r => r = decide (toNat b < toNat a) ⦄ := by
  simp only [core.cmp.PartialOrd.gt.trait_default, core.cmp.PartialOrd.gt.default,
    core.cmp.PartialOrd.gt_body, Uint256.Insts.CoreCmpPartialOrdUint256.partial_cmp]
  apply WP.spec_bind (Pₘ := fun c => c = some (compare (toNat a) (toNat b)))
  · apply WP.spec_bind (cmp_spec a b); intro o ho; simp [ho]
  intro c hc; subst hc
  simp [compare_gt_iff_gt]

@[step]
theorem ge_spec (a b : Array U64 4#usize) :
    core.cmp.PartialOrd.ge.trait_default Uint256.Insts.CoreCmpPartialOrdUint256 a b
      ⦃ r => r = decide (toNat b ≤ toNat a) ⦄ := by
  simp only [core.cmp.PartialOrd.ge.trait_default, core.cmp.PartialOrd.ge.default,
    core.cmp.PartialOrd.ge_body, Uint256.Insts.CoreCmpPartialOrdUint256.partial_cmp]
  apply WP.spec_bind (Pₘ := fun c => c = some (compare (toNat a) (toNat b)))
  · apply WP.spec_bind (cmp_spec a b); intro o ho; simp [ho]
  intro c hc; subst hc
  simp only [WP.spec_ok, Option.some.injEq]
  rcases lt_trichotomy (toNat a) (toNat b) with h | h | h
  · simp [(compare_lt_iff_lt).mpr h, Nat.not_le.mpr h]
  · simp [h]
  · simp [(compare_gt_iff_gt).mpr h, le_of_lt h]

end Hayai.Proofs.Uint256
