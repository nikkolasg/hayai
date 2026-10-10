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

theorem limb_of_toNat (a : Array U64 4#usize) (i : ℕ) (hi : i < 4) :
    toNat a / 2 ^ (64 * i) % 2 ^ 64 = a.val[i]!.val := by
  have h0 := a.val[0]!.hBounds; have h1 := a.val[1]!.hBounds
  have h2 := a.val[2]!.hBounds; have h3 := a.val[3]!.hBounds
  simp only [UScalarTy.numBits] at *
  unfold toNat
  interval_cases i <;> simp only [Nat.mul_zero, Nat.mul_one, Nat.reduceMul, Nat.reducePow, Nat.div_one] <;> omega

theorem digit_of_toNat (a : Array U64 4#usize) (i k : ℕ) (hi : i < 4) (hk : k < 8) :
    toNat a / 256 ^ (8 * i + k) % 256 = a.val[i]!.val / 256 ^ k % 256 := by
  have e1 : (256 : ℕ) ^ (8 * i + k) = 2 ^ (64 * i) * 256 ^ k := by
    rw [pow_add, show (256 : ℕ) = 2 ^ 8 by norm_num, ← pow_mul, ← pow_mul]; ring_nf
  rw [e1, ← Nat.div_div_eq_div_mul, ← limb_of_toNat a i hi]
  generalize toNat a / 2 ^ (64 * i) = y
  interval_cases k <;> simp only [pow_zero, Nat.div_one, Nat.reducePow] <;> omega

open Hayai.Proofs.Bytes in
theorem to_le_bytes_inner_spec (i : Usize) (hi : i.val < 4) (limb : Array U8 8#usize)
    (iter : core.ops.range.Range Usize) (b0 bytes : Array U8 32#usize)
    (hend : iter.end.val = 8) (hs : iter.start.val ≤ 8)
    (hb : ∀ n < 32, bytes.val[n]! =
      if 8 * i.val ≤ n ∧ n < 8 * i.val + iter.start.val then limb.val[n - 8 * i.val]! else b0.val[n]!) :
    Uint256.to_le_bytes_loop0_loop0 iter bytes i limb ⦃ bytes1 =>
      ∀ n < 32, bytes1.val[n]! =
        if 8 * i.val ≤ n ∧ n < 8 * i.val + 8 then limb.val[n - 8 * i.val]! else b0.val[n]! ⦄ := by
  unfold Uint256.to_le_bytes_loop0_loop0
  apply loop.spec_decr_nat
    (measure := fun (s : core.ops.range.Range Usize × Array U8 32#usize) => 8 - s.1.start.val)
    (inv := fun (s : core.ops.range.Range Usize × Array U8 32#usize) =>
      s.1.end.val = 8 ∧ s.1.start.val ≤ 8 ∧
      ∀ n < 32, s.2.val[n]! =
        if 8 * i.val ≤ n ∧ n < 8 * i.val + s.1.start.val then limb.val[n - 8 * i.val]!
        else b0.val[n]!)
  · rintro ⟨it, bt⟩ ⟨hend', hs', hb'⟩
    simp only at hend' hs' hb'
    unfold Uint256.to_le_bytes_loop0_loop0.body
    step as ⟨ o, it1, ho, hit1 ⟩
    by_cases hlt : it.start.val < it.end.val
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      have he1 : it1.end.val = 8 := by rw [hit1]; exact hend'
      step as ⟨ x, hx ⟩
      step as ⟨ p1, hp1 ⟩
      step as ⟨ p2, hp2 ⟩
      step as ⟨ bt1, hbt1 ⟩
      refine ⟨he1, by omega, ?_, by omega⟩
      intro n hn
      rw [hbt1, Array.set_val_eq]
      by_cases hne : n = p2.val
      · subst hne
        have hlen : p2.val < bt.val.length := by simp; omega
        simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_self hlen, Option.getD_some]
        rw [if_pos (by omega), hx, show p2.val - 8 * i.val = it.start.val by omega]
        have : it.start.val < limb.val.length := by simp; omega
        simp [List.getElem?_eq_getElem this]
      · have hne' : p2.val ≠ n := fun h => hne h.symm
        simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_ne hne']
        rw [← List.getElem!_eq_getElem?_getD, hb' n hn]
        by_cases hl : 8 * i.val ≤ n ∧ n < 8 * i.val + it.start.val
        · rw [if_pos hl, if_pos (by omega)]; exact List.getElem!_eq_getElem?_getD
        · rw [if_neg hl, if_neg (by omega)]; exact List.getElem!_eq_getElem?_getD
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only [WP.spec_ok]
      intro n hn
      rw [hb' n hn, show it.start.val = 8 by omega]
  · exact ⟨hend, hs, hb⟩

open Hayai.Proofs.Bytes in
theorem to_le_bytes_outer_spec (a : Array U64 4#usize) (iter : core.ops.range.Range Usize)
    (bytes : Array U8 32#usize) (hend : iter.end.val = 4) (hs : iter.start.val ≤ 4)
    (hb : ∀ n < 32, bytes.val[n]!.val =
      if n < 8 * iter.start.val then toNat a / 256 ^ n % 256 else 0) :
    Uint256.to_le_bytes_loop0 iter a bytes ⦃ bytes1 =>
      ∀ n < 32, bytes1.val[n]!.val = toNat a / 256 ^ n % 256 ⦄ := by
  unfold Uint256.to_le_bytes_loop0
  apply loop.spec_decr_nat
    (measure := fun (s : core.ops.range.Range Usize × Array U64 4#usize × Array U8 32#usize) =>
      4 - s.1.start.val)
    (inv := fun (s : core.ops.range.Range Usize × Array U64 4#usize × Array U8 32#usize) =>
      s.1.end.val = 4 ∧ s.1.start.val ≤ 4 ∧ s.2.1 = a ∧
      ∀ n < 32, s.2.2.val[n]!.val = if n < 8 * s.1.start.val then toNat a / 256 ^ n % 256 else 0)
  · rintro ⟨it, a', bt⟩ ⟨hend', hs', rfl, hb'⟩
    simp only at hend' hs' hb'
    unfold Uint256.to_le_bytes_loop0.body
    step as ⟨ o, it1, ho, hit1 ⟩
    by_cases hlt : it.start.val < it.end.val
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      have he1 : it1.end.val = 4 := by rw [hit1]; exact hend'
      have hi4 : it.start.val < 4 := by omega
      step as ⟨ x, hx ⟩
      simp only [lift, Std.bind_ok]
      step with to_le_bytes_inner_spec (b0 := bt) as ⟨ bt1, hbt1 ⟩
      case hb => intro n hn; rw [if_neg (by simp)]
      try simp only [WP.spec_ok]
      refine ⟨he1, by omega, ?_, by omega⟩
      intro n hn
      rw [hbt1 n hn]
      by_cases hl : 8 * it.start.val ≤ n ∧ n < 8 * it.start.val + 8
      · rw [if_pos hl, if_pos (by omega)]
        have hk : n - 8 * it.start.val < 8 := by omega
        rw [u64_to_le_bytes_digit x _ hk, hx]
        have hl4 : it.start.val < a'.val.length := by simp; omega
        rw [← getElem!_pos a'.val it.start.val hl4, ← digit_of_toNat a' _ _ hi4 hk,
          show 8 * it.start.val + (n - 8 * it.start.val) = n by omega]
      · rw [if_neg hl, hb' n hn]
        by_cases hn' : n < 8 * it.start.val
        · rw [if_pos hn', if_pos (by omega)]
        · rw [if_neg hn', if_neg (by omega)]
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only [WP.spec_ok]
      intro n hn
      rw [hb' n hn, if_pos (by omega)]
  · exact ⟨hend, hs, rfl, hb⟩

/-- `to_le_bytes`: byte `n` is digit `n` of the integer in base 256. -/
@[step]
theorem to_le_bytes_spec (a : Array U64 4#usize) :
    Uint256.to_le_bytes a ⦃ bytes => ∀ n < 32, bytes.val[n]!.val = toNat a / 256 ^ n % 256 ⦄ := by
  unfold Uint256.to_le_bytes
  step with to_le_bytes_outer_spec
  case hb => intro n hn; simp [Array.repeat_val, hn]; interval_cases n <;> rfl
  exact bytes_post n n_post

open Hayai.Proofs.Bytes in
section
/-- The sum of the first `n` base-256 digits of `x` is `x mod 256^n`. -/
theorem digits_sum (x : ℕ) : ∀ n, ∑ k ∈ Finset.range n, x / 256 ^ k % 256 * 256 ^ k = x % 256 ^ n
  | 0 => by simp [Nat.mod_one]
  | n + 1 => by
    rw [Finset.sum_range_succ, digits_sum x n, pow_succ, Nat.mod_mul]
    ring

theorem u128_to_le_bytes_digit (x : U128) (k : ℕ) (hk : k < 16) :
    (core.num.U128.to_le_bytes x).val[k]!.val = x.val / 256 ^ k % 256 := by
  have h := leVal_digit x.bv.toLEBytes k
  rw [leVal_toLEBytes (by simp)] at h
  have hl : k < x.bv.toLEBytes.length := by simp [BitVec.toLEBytes_length]; omega
  simp only [core.num.U128.to_le_bytes, Array.from_val, List.getElem!_eq_getElem?_getD,
    List.getElem?_map, List.getElem?_eq_getElem hl, Option.map_some, Option.getD_some] at h ⊢
  exact h.symm

theorem bytesVal8 (l : List U8) (h : l.length = 8) :
    bytesVal l = ∑ k ∈ Finset.range 8, l[k]!.val * 256 ^ k := by
  rw [bytesVal_eq_sum, h]

/-- `split`: the low and the high 64 bits of a `u128`. -/
theorem split_spec (x : U128) :
    difficulty_rules.split x ⦃ p => p.1.val = x.val % 2 ^ 64 ∧ p.2.val = x.val / 2 ^ 64 ⦄ := by
  unfold difficulty_rules.split
  simp only [lift, Std.bind_ok]
  have hd := fun k (hk : k < 16) => u128_to_le_bytes_digit x k hk
  have hxb := x.hBounds
  simp only [UScalarTy.numBits] at hxb
  step*
  have hv : ∀ k (hk : k < 16), ((core.num.U128.to_le_bytes x).val[k]'(by simp; omega)).val =
      x.val / 256 ^ k % 256 := fun k hk => by
    rw [← getElem!_pos (core.num.U128.to_le_bytes x).val k (by simp; omega)]; exact hd k hk
  simp only [i_post, i1_post, i2_post, i3_post, i4_post, i5_post, i6_post, i7_post, i8_post,
    i9_post, i10_post, i11_post, i12_post, i13_post, i14_post, i15_post]
  rw [u64_from_le_bytes_val, u64_from_le_bytes_val]
  simp only [Array.make, Array.from_val, bytesVal_cons, bytesVal_nil]
  rw [hv 0 (by omega), hv 1 (by omega), hv 2 (by omega), hv 3 (by omega), hv 4 (by omega),
    hv 5 (by omega), hv 6 (by omega), hv 7 (by omega), hv 8 (by omega), hv 9 (by omega),
    hv 10 (by omega), hv 11 (by omega), hv 12 (by omega), hv 13 (by omega), hv 14 (by omega),
    hv 15 (by omega)]
  have hq : x.val / 256 ^ 8 % 256 ^ 8 = x.val / 256 ^ 8 := Nat.mod_eq_of_lt (by
    rw [Nat.div_lt_iff_lt_mul (by positivity), ← pow_add]; norm_num; omega)
  constructor
  · calc _ = ∑ k ∈ Finset.range 8, x.val / 256 ^ k % 256 * 256 ^ k := by
          simp only [Finset.sum_range_succ, Finset.sum_range_zero]; ring
      _ = x.val % 256 ^ 8 := digits_sum _ 8
      _ = x.val % 2 ^ 64 := by norm_num
  · calc _ = ∑ k ∈ Finset.range 8, x.val / 256 ^ 8 / 256 ^ k % 256 * 256 ^ k := by
          simp only [Finset.sum_range_succ, Finset.sum_range_zero, Nat.div_div_eq_div_mul,
            ← pow_add]
          ring
      _ = x.val / 256 ^ 8 % 256 ^ 8 := digits_sum _ 8
      _ = x.val / 256 ^ 8 := hq
      _ = x.val / 2 ^ 64 := by norm_num

end

/-- The value of the limbs from `i` up. -/
def hiV (a : Array U64 4#usize) (i : ℕ) : ℕ := toNat a / 2 ^ (64 * i)

theorem hiV_step (a : Array U64 4#usize) (i : ℕ) (hi : i < 4) :
    hiV a i = hiV a (i + 1) * 2 ^ 64 + a.val[i]!.val := by
  unfold hiV
  rw [← limb_of_toNat a i hi, show 64 * (i + 1) = 64 * i + 64 by ring, pow_add,
    ← Nat.div_div_eq_div_mul]
  exact (Nat.div_add_mod' _ _).symm

theorem hiV_four (a : Array U64 4#usize) : hiV a 4 = 0 := by
  unfold hiV; rw [Nat.div_eq_of_lt]; have := toNat_lt a; norm_num at this ⊢; omega

theorem hiV_zero (a : Array U64 4#usize) : hiV a 0 = toNat a := by simp [hiV]

/-- `hiV` reads only the limbs from `i` up. -/
theorem hiV_set (a : Array U64 4#usize) (k : Usize) (v : U64) (i : ℕ) (hk : k.val < i) (hi : i ≤ 4) :
    hiV (a.set k v) i = hiV a i := by
  rcases (show i = 4 ∨ i < 4 by omega) with h | h
  · subst h; rw [hiV_four, hiV_four]
  · -- Induction from 4 down.
    have key : ∀ m, m ≤ 4 - i → hiV (a.set k v) (4 - m) = hiV a (4 - m) := by
      intro m
      induction m with
      | zero => intro _; rw [hiV_four, hiV_four]
      | succ m ih =>
        intro hm
        have e : 4 - (m + 1) + 1 = 4 - m := by omega
        rw [hiV_step _ _ (by omega), hiV_step a _ (by omega), e, ih (by omega)]
        congr 1
        rw [Array.set_val_eq]
        have hne : k.val ≠ 4 - (m + 1) := by omega
        simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_ne hne]
    have := key (4 - i) (le_refl _)
    rwa [show 4 - (4 - i) = i by omega] at this

theorem div_rem_u64_loop_spec (a : Array U64 4#usize) (d : U64) (hd : d.val ≠ 0)
    (q : Array U64 4#usize) (rem : U64) (i : Usize) (hi : i.val ≤ 4) (hrem : rem.val < d.val)
    (hinv : hiV a i.val = d.val * hiV q i.val + rem.val) :
    Uint256.div_rem_u64_loop a d q rem i ⦃ (q1 : Array U64 4#usize) (r1 : U64) =>
      toNat a = d.val * toNat q1 + r1.val ∧ r1.val < d.val ⦄ := by
  unfold Uint256.div_rem_u64_loop
  apply loop.spec_decr_nat
    (measure := fun (s : Array U64 4#usize × Array U64 4#usize × U64 × Usize) => s.2.2.2.val)
    (inv := fun (s : Array U64 4#usize × Array U64 4#usize × U64 × Usize) =>
      s.1 = a ∧ s.2.2.2.val ≤ 4 ∧ s.2.2.1.val < d.val ∧
      hiV a s.2.2.2.val = d.val * hiV s.2.1 s.2.2.2.val + s.2.2.1.val)
  · rintro ⟨a', q', r', k⟩ ⟨rfl, hk, hr', hinv'⟩
    simp only at hk hr' hinv'
    unfold Uint256.div_rem_u64_loop.body
    simp only
    split
    · rename_i hk0
      have hk1 : 1 ≤ k.val := by scalar_tac
      have hdb := d.hBounds; have hrb := r'.hBounds
      simp only [UScalarTy.numBits] at hdb hrb
      step as ⟨ k1, hk1v ⟩
      simp only [lift, Std.bind_ok]
      step as ⟨ i3, hi3 ⟩
      step as ⟨ x, hx ⟩
      have hxb := x.hBounds
      simp only [UScalarTy.numBits] at hxb
      have h128 : U128.size = 2 ^ 128 := by simp [U128.size, U128.numBits]
      have hi3v : i3.val = r'.val * 2 ^ 64 := by
        rw [hi3, core.convert.num.FromU128U64.from_val_eq, Nat.shiftLeft_eq, h128]
        apply Nat.mod_eq_of_lt; omega
      have hcur : (i3 ||| core.convert.num.FromU128U64.from x).val = r'.val * 2 ^ 64 + x.val := by
        rw [UScalar.val_or, hi3v, core.convert.num.FromU128U64.from_val_eq, ← Nat.shiftLeft_eq,
          ← Nat.shiftLeft_add_eq_or_of_lt (by omega), Nat.shiftLeft_eq]
      have hdv : (core.convert.num.FromU128U64.from d).val = d.val :=
        core.convert.num.FromU128U64.from_val_eq d
      have hcur_lt : r'.val * 2 ^ 64 + x.val < d.val * 2 ^ 64 := by nlinarith
      step as ⟨ i7, hi7 ⟩
      step with split_spec as ⟨ quot, hi7' , hquot, _ ⟩
      step as ⟨ i9, hi9 ⟩
      step with split_spec as ⟨ rest, hi9', hrest, _ ⟩
      step as ⟨ q2, hq2 ⟩
      try simp only [WP.spec_ok]
      have hqv : quot.val = (r'.val * 2 ^ 64 + x.val) / d.val := by
        rw [hquot, hi7, hcur, hdv]; apply Nat.mod_eq_of_lt
        rw [Nat.div_lt_iff_lt_mul (by omega)]; linarith
      have hrv : rest.val = (r'.val * 2 ^ 64 + x.val) % d.val := by
        rw [hrest, hi9, hcur, hdv]; apply Nat.mod_eq_of_lt
        have := Nat.mod_lt (r'.val * 2 ^ 64 + x.val) (show 0 < d.val by omega); omega
      have hxk : x.val = a'.val[k1.val]!.val := by
        rw [hx, getElem!_pos a'.val k1.val (by simp; omega)]
      have hk1k : k1.val + 1 = k.val := by omega
      refine ⟨by omega, ?_, ?_, by omega⟩
      · rw [hrv]; exact Nat.mod_lt _ (by omega)
      · rw [hiV_step a' _ (by omega), hiV_step q2 _ (by omega), hk1k, hq2,
          hiV_set _ _ _ _ (by omega) hk, hinv', Array.set_val_eq]
        have hlen : k1.val < q'.val.length := by simp; omega
        simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_self hlen, Option.getD_some]
        rw [← List.getElem!_eq_getElem?_getD, ← hxk, hqv, hrv]
        have hdm := Nat.div_add_mod (r'.val * 2 ^ 64 + x.val) d.val
        generalize hiV q' k.val = H
        generalize (r'.val * 2 ^ 64 + x.val) / d.val = Q at hdm ⊢
        generalize (r'.val * 2 ^ 64 + x.val) % d.val = R at hdm ⊢
        rw [mul_add, add_assoc, hdm]; ring
    · rename_i hk0
      have h0 : k.val = 0 := by scalar_tac
      simp only [WP.spec_ok, WP.uncurry']
      rw [h0, hiV_zero, hiV_zero] at hinv'
      exact ⟨hinv', hr'⟩
  · exact ⟨rfl, hi, hrem, hinv⟩

/-- `div_rem_u64`: the quotient and the remainder of the integer by a `u64`, and
`DivisionByZero` for 0. -/
@[step]
theorem div_rem_u64_spec (a : Array U64 4#usize) (d : U64) :
    Uint256.div_rem_u64 a d ⦃ r => match r with
      | core.result.Result.Ok (q, rem) => d.val ≠ 0 ∧ toNat q = toNat a / d.val ∧
          rem.val = toNat a % d.val
      | core.result.Result.Err e => d.val = 0 ∧ e = ConsensusError.DivisionByZero ⦄ := by
  unfold Uint256.div_rem_u64
  split
  · rename_i h0
    simp only [WP.spec_ok]
    exact ⟨by rw [h0]; rfl, trivial⟩
  · rename_i h0
    have hd : d.val ≠ 0 := by intro h; apply h0; apply UScalar.eq_of_val_eq; simp [h]
    step with div_rem_u64_loop_spec as ⟨ q, rem, hq, hrem ⟩
    case hinv => simp [hiV_four]
    refine ⟨hd, ?_, ?_⟩
    · rw [hq, Nat.mul_add_div (by omega), Nat.div_eq_of_lt hrem]; simp
    · rw [hq, Nat.mul_add_mod, Nat.mod_eq_of_lt hrem]

theorem checked_mul_u64_loop_spec (iter : core.ops.range.Range Usize)
    (a limbs : Array U64 4#usize) (f carry : U64)
    (hend : iter.end.val = 4) (hstart : iter.start.val ≤ 4)
    (hinv : low limbs iter.start.val + 2 ^ (64 * iter.start.val) * carry.val =
      low a iter.start.val * f.val) :
    Uint256.checked_mul_u64_loop iter a f limbs carry ⦃ (limbs1 : Array U64 4#usize) (carry1 : U64) =>
      toNat limbs1 + 2 ^ 256 * carry1.val = toNat a * f.val ⦄ := by
  unfold Uint256.checked_mul_u64_loop
  apply loop.spec_decr_nat
    (measure := fun (s : core.ops.range.Range Usize × Array U64 4#usize × Array U64 4#usize × U64) =>
      4 - s.1.start.val)
    (inv := fun (s : core.ops.range.Range Usize × Array U64 4#usize × Array U64 4#usize × U64) =>
      s.1.end.val = 4 ∧ s.1.start.val ≤ 4 ∧ s.2.1 = a ∧
      low s.2.2.1 s.1.start.val + 2 ^ (64 * s.1.start.val) * s.2.2.2.val =
        low a s.1.start.val * f.val)
  · rintro ⟨it, a', limbs', c'⟩ ⟨hend', hs', rfl, hinv'⟩
    simp only at hend' hs' hinv'
    unfold Uint256.checked_mul_u64_loop.body
    step as ⟨ o, it1, ho, hit1 ⟩
    by_cases hlt : it.start.val < it.end.val
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      have hk : it.start.val < 4 := by omega
      have he1 : it1.end.val = 4 := by rw [hit1]; exact hend'
      step as ⟨ x, hx ⟩
      have hxb := x.hBounds; have hfb := f.hBounds; have hcb := c'.hBounds
      simp only [UScalarTy.numBits] at hxb hfb hcb
      simp only [lift, Std.bind_ok]
      have hfx := core.convert.num.FromU128U64.from_val_eq x
      have hff := core.convert.num.FromU128U64.from_val_eq f
      have hfc := core.convert.num.FromU128U64.from_val_eq c'
      have hprod : x.val * f.val + c'.val < 2 ^ 128 := by
        have : x.val * f.val ≤ (2 ^ 64 - 1) * (2 ^ 64 - 1) :=
          Nat.mul_le_mul (by omega) (by omega)
        omega
      step as ⟨ p1, hp1 ⟩
      step as ⟨ p2, hp2 ⟩
      step with split_spec as ⟨ lo, hi, hlo, hhi ⟩
      step as ⟨ l2, hl2 ⟩
      try simp only [WP.spec_ok]
      refine ⟨he1, by omega, ?_, by omega⟩
      have hxv : x.val = a'.val[it.start.val]!.val := by
        rw [hx, getElem!_pos a'.val it.start.val (by simp; omega)]
      have hpv : p2.val = x.val * f.val + c'.val := by rw [hp2, hp1, hfx, hff, hfc]
      rw [hl2, hstart1, low_set_succ _ _ _ hk]
      simp only [low]
      have hp : 2 ^ (64 * (it.start.val + 1)) = 2 ^ (64 * it.start.val) * 2 ^ 64 := by
        rw [Nat.mul_add, pow_add]
      rw [hp]
      have hc : lo.val + 2 ^ 64 * hi.val = x.val * f.val + c'.val := by
        rw [hlo, hhi, hpv]; omega
      have e : ∀ P : ℕ, P * lo.val + P * 2 ^ 64 * hi.val = P * (lo.val + 2 ^ 64 * hi.val) :=
        fun P => by ring
      rw [add_assoc, e, hc, ← hxv]
      calc low limbs' it.start.val + 2 ^ (64 * it.start.val) * (x.val * f.val + c'.val)
          = (low limbs' it.start.val + 2 ^ (64 * it.start.val) * c'.val) +
            2 ^ (64 * it.start.val) * x.val * f.val := by ring
        _ = _ := by rw [hinv']; ring
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      have h4 : it.start.val = 4 := by omega
      simp only [WP.spec_ok]
      rw [h4] at hinv'
      rw [← low_four limbs', ← low_four a']
      simpa using hinv'
  · exact ⟨hend, hstart, rfl, hinv⟩

/-- `checked_mul_u64`: `None` exactly when the product is `2^256` or more, else the product. -/
@[step]
theorem checked_mul_u64_spec (a : Array U64 4#usize) (f : U64) :
    Uint256.checked_mul_u64 a f ⦃ r =>
      (r = none ↔ 2 ^ 256 ≤ toNat a * f.val) ∧
      ∀ c, r = some c → toNat c = toNat a * f.val ⦄ := by
  unfold Uint256.checked_mul_u64
  step with checked_mul_u64_loop_spec as ⟨ limbs1, carry, hsum ⟩
  · simp [low]
  · have hlt := toNat_lt limbs1
    split
    · rename_i hne
      simp only [WP.spec_ok, true_iff, reduceCtorEq, false_implies, implies_true, and_true]
      have : carry.val ≠ 0 := by
        intro h0; apply absurd hne; simp [bne_iff_ne, ne_eq, UScalar.eq_equiv, h0]
      have : 2 ^ 256 ≤ 2 ^ 256 * carry.val := Nat.le_mul_of_pos_right _ (by omega)
      omega
    · rename_i hne
      have : carry.val = 0 := by
        by_contra h0; apply hne; simp [bne_iff_ne, ne_eq, UScalar.eq_equiv, h0]
      simp only [WP.spec_ok, reduceCtorEq, false_iff, not_le, Option.some.injEq, forall_eq']
      rw [this] at hsum
      omega

/-- `Ord::min` of two integers. -/
@[step]
theorem min_spec (a b : Array U64 4#usize) :
    core.cmp.Ord.min.trait_default Uint256.Insts.CoreCmpOrd a b ⦃ c =>
      toNat c = min (toNat a) (toNat b) ⦄ := by
  simp only [core.cmp.Ord.min.trait_default, core.cmp.Ord.min.default, core.cmp.Ord.min_body,
    Uint256.Insts.CoreCmpOrd, Uint256.Insts.CoreCmpPartialOrdUint256]
  apply WP.spec_bind (Pₘ := fun l => l = decide (toNat b < toNat a))
  · simp only [core.cmp.PartialOrd.lt_body, Uint256.Insts.CoreCmpPartialOrdUint256.partial_cmp]
    apply WP.spec_bind (Pₘ := fun c => c = some (compare (toNat b) (toNat a)))
    · apply WP.spec_bind (cmp_spec b a); intro o ho; simp [ho]
    intro c hc; subst hc; simp [compare_lt_iff_lt]
  intro l hl; subst hl
  by_cases h : toNat b < toNat a
  · simp [h, le_of_lt h]
  · simp [h]; omega

end Hayai.Proofs.Uint256
