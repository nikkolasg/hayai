/-
§7.7.4: `Uint256::from_compact` of `hayai-consensus-core::difficulty_rules` is `ToTarget`.
-/
import Hayai.Core
import Hayai.Spec.Difficulty
import Hayai.Proofs.Bytes
import Hayai.Proofs.Uint256
import Hayai.Proofs.Scalars
import Mathlib.Tactic

open Aeneas Aeneas.Std Result
open HayaiCore HayaiCore.difficulty_rules
open Hayai.Spec.Difficulty Hayai.Proofs.Bytes Hayai.Proofs.Uint256 Hayai.Proofs.Scalars

namespace Hayai.Proofs.Compact

/-- The first loop copies the 4 bytes of the shifted mantissa to the start of the target. -/
theorem copy4_spec (iter : core.ops.range.Range Usize) (t0 t : Array U8 32#usize)
    (bytes : Array U8 4#usize) (hend : iter.end.val = 4) (hs : iter.start.val ≤ 4)
    (ht : ∀ n < 32, t.val[n]! = if n < iter.start.val then bytes.val[n]! else t0.val[n]!) :
    Uint256.from_compact_loop0 iter t bytes ⦃ t1 =>
      ∀ n < 32, t1.val[n]! = if n < 4 then bytes.val[n]! else t0.val[n]! ⦄ := by
  unfold Uint256.from_compact_loop0
  apply loop.spec_decr_nat
    (measure := fun (s : core.ops.range.Range Usize × Array U8 32#usize) => 4 - s.1.start.val)
    (inv := fun (s : core.ops.range.Range Usize × Array U8 32#usize) =>
      s.1.end.val = 4 ∧ s.1.start.val ≤ 4 ∧
      ∀ n < 32, s.2.val[n]! = if n < s.1.start.val then bytes.val[n]! else t0.val[n]!)
  · rintro ⟨it, tt⟩ ⟨hend', hs', ht'⟩
    simp only at hend' hs' ht'
    unfold Uint256.from_compact_loop0.body
    step as ⟨ o, it1, ho, hit1 ⟩
    by_cases hlt : it.start.val < it.end.val
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      step as ⟨ x, hx ⟩
      step as ⟨ tt1, htt1 ⟩
      have he1 : it1.end.val = 4 := by rw [hit1]; exact hend'
      refine ⟨he1, by omega, ?_, by omega⟩
      intro n hn
      rw [htt1, Array.set_val_eq]
      by_cases hne : n = it.start.val
      · subst hne
        have hlen : it.start.val < tt.val.length := by simp; omega
        simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_self hlen, Option.getD_some]
        rw [if_pos (by omega), hx]
        have : it.start.val < bytes.val.length := by simp; omega
        simp [List.getElem?_eq_getElem this]
      · have hne' : it.start.val ≠ n := fun h => hne h.symm
        simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_ne hne']
        rw [← List.getElem!_eq_getElem?_getD, ht' n hn]
        by_cases hl : n < it.start.val
        · rw [if_pos hl, if_pos (by omega)]; exact List.getElem!_eq_getElem?_getD
        · rw [if_neg hl, if_neg (by omega)]; exact List.getElem!_eq_getElem?_getD
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only [WP.spec_ok]
      intro n hn
      rw [ht' n hn]
      have : it.start.val = 4 := by omega
      rw [this]
  · exact ⟨hend, hs, ht⟩

/-- The second loop copies the 3 bytes of the mantissa from byte `offset` on, and drops a byte
past the end. -/
theorem copy3_spec (iter : core.ops.range.Range Usize) (t0 t : Array U8 32#usize)
    (offset : Usize) (bytes : Array U8 4#usize) (ho32 : offset.val ≤ 32)
    (hend : iter.end.val = 3) (hs : iter.start.val ≤ 3)
    (ht : ∀ n < 32, t.val[n]! =
      if offset.val ≤ n ∧ n < offset.val + iter.start.val then bytes.val[n - offset.val]!
      else t0.val[n]!) :
    Uint256.from_compact_loop1 iter t offset bytes ⦃ t1 =>
      ∀ n < 32, t1.val[n]! =
        if offset.val ≤ n ∧ n < offset.val + 3 then bytes.val[n - offset.val]! else t0.val[n]! ⦄ := by
  unfold Uint256.from_compact_loop1
  apply loop.spec_decr_nat
    (measure := fun (s : core.ops.range.Range Usize × Array U8 32#usize) => 3 - s.1.start.val)
    (inv := fun (s : core.ops.range.Range Usize × Array U8 32#usize) =>
      s.1.end.val = 3 ∧ s.1.start.val ≤ 3 ∧
      ∀ n < 32, s.2.val[n]! =
        if offset.val ≤ n ∧ n < offset.val + s.1.start.val then bytes.val[n - offset.val]!
        else t0.val[n]!)
  · rintro ⟨it, tt⟩ ⟨hend', hs', ht'⟩
    simp only at hend' hs' ht'
    unfold Uint256.from_compact_loop1.body
    step as ⟨ o, it1, ho, hit1 ⟩
    by_cases hlt : it.start.val < it.end.val
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      have he1 : it1.end.val = 3 := by rw [hit1]; exact hend'
      step as ⟨ p, hp ⟩
      split
      · rename_i hp32
        have hp32' : p.val < 32 := hp32
        step as ⟨ x, hx ⟩
        step as ⟨ tt1, htt1 ⟩
        refine ⟨he1, by omega, ?_, by omega⟩
        intro n hn
        rw [htt1, Array.set_val_eq]
        by_cases hne : n = p.val
        · subst hne
          have hlen : p.val < tt.val.length := by simp; omega
          simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_self hlen, Option.getD_some]
          rw [if_pos (by omega), hx]
          have : it.start.val < bytes.val.length := by simp; omega
          rw [show p.val - offset.val = it.start.val by omega]
          simp [List.getElem?_eq_getElem this]
        · have hne' : p.val ≠ n := fun h => hne h.symm
          simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set_ne hne']
          rw [← List.getElem!_eq_getElem?_getD, ht' n hn]
          by_cases hl : offset.val ≤ n ∧ n < offset.val + it.start.val
          · rw [if_pos hl, if_pos (by omega)]; exact List.getElem!_eq_getElem?_getD
          · rw [if_neg hl, if_neg (by omega)]; exact List.getElem!_eq_getElem?_getD
      · rename_i hp32
        have hp32' : ¬ p.val < 32 := hp32
        simp only [WP.spec_ok]
        refine ⟨he1, by omega, ?_, by omega⟩
        intro n hn
        rw [ht' n hn]
        by_cases hl : offset.val ≤ n ∧ n < offset.val + it.start.val
        · rw [if_pos hl, if_pos (by omega)]
        · rw [if_neg hl, if_neg (by omega)]
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only [WP.spec_ok]
      intro n hn
      rw [ht' n hn]
      have : it.start.val = 3 := by omega
      rw [this]
  · exact ⟨hend, hs, ht⟩

/-- The value of the target that `copy3_spec` builds from zero: the mantissa at byte `o`. The
bytes past the end are zero when the mantissa fits, which the overflow condition ensures. -/
theorem shifted_val (bytes : Array U8 4#usize) (t1 : Array U8 32#usize) (o : ℕ)
    (ho : 1 ≤ o ∧ o ≤ 31)
    (ht1 : ∀ n < 32, t1.val[n]! = if o ≤ n ∧ n < o + 3 then bytes.val[n - o]! else 0#u8)
    (hm : bytesVal bytes.val < 2 ^ 23)
    (h30 : o = 30 → bytesVal bytes.val < 65536) (h31 : o = 31 → bytesVal bytes.val < 256) :
    bytesVal t1.val = bytesVal bytes.val * 256 ^ o := by
  have hb0 := bytes.val[0]!.hBounds; have hb1 := bytes.val[1]!.hBounds
  have hb2 := bytes.val[2]!.hBounds; have hb3 := bytes.val[3]!.hBounds
  simp only [UScalarTy.numBits] at hb0 hb1 hb2 hb3
  rw [bytesVal_eq_sum t1.val, bytesVal_eq_sum bytes.val] at *
  simp only [Array.length_eq] at *
  rw [show ((32#usize : Usize) : ℕ) = 32 from rfl]
  rw [show ((4#usize : Usize) : ℕ) = 4 from rfl] at *
  rw [Finset.sum_congr rfl (fun n hn => by rw [ht1 n (Finset.mem_range.mp hn)])]
  simp only [Finset.sum_range_succ, Finset.sum_range_zero] at *
  obtain ⟨ho1, ho2⟩ := ho
  interval_cases o <;> simp at * <;> omega

theorem zero32 : ∀ n < 32, (Array.repeat 32#usize 0#u8).val[n]! = 0#u8 := by
  intro n hn; interval_cases n <;> rfl

/-- `ToTarget` with the sign bit, the mantissa and the exponent of `bits` read apart. -/
theorem toTarget_eq (b : ℕ) :
    toTarget b = if b.testBit 23 then 0 else
      (if 3 ≤ b / 2 ^ 24 then b % 2 ^ 23 * 256 ^ (b / 2 ^ 24 - 3)
       else b % 2 ^ 23 / 256 ^ (3 - b / 2 ^ 24)) := by
  unfold toTarget
  rw [Nat.and_two_pow_sub_one_eq_mod, Nat.and_two_pow]
  cases b.testBit 23 <;> simp

/-- The value of the little-endian bytes of a `u32`. -/
theorem u32_to_le_bytes_val (x : U32) :
    bytesVal (x.bv.toLEBytes.map (@UScalar.mk UScalarTy.U8)) = x.val := by
  simp only [bytesVal, List.map_map]
  have : (U8.bv ∘ @UScalar.mk UScalarTy.U8) = id := by funext y; rfl
  rw [this, List.map_id, leVal_toLEBytes (by simp)]
  rfl

theorem u32_to_le_bytes_arr_val (x : U32) :
    bytesVal (core.num.U32.to_le_bytes x).val = x.val := by
  simp only [core.num.U32.to_le_bytes, Array.from_val]
  exact u32_to_le_bytes_val x

/-- §7.7.4, `ToTarget`: `from_compact` returns the target that `bits` encodes, and `None`
exactly when that target is 0 (the sign bit, or a zero mantissa) or `2^256` and above (the
overflow condition of zcashd `SetCompact`). -/
theorem from_compact_spec (bits : U32) :
    Uint256.from_compact bits ⦃ r => match r with
      | none => toTarget bits.val = 0 ∨ 2 ^ 256 ≤ toTarget bits.val
      | some t => toNat t = toTarget bits.val ∧ 0 < toTarget bits.val ∧
          toTarget bits.val < 2 ^ 256 ⦄ := by
  unfold Uint256.from_compact
  have hb := bits.hBounds
  simp only [UScalarTy.numBits] at hb
  step as ⟨ e, he ⟩
  step as ⟨ m, hm ⟩
  step as ⟨ i, hi ⟩
  rw [toTarget_eq]
  have he' : e.val = bits.val / 2 ^ 24 := by rw [he, Nat.shiftRight_eq_div_pow]
  have hm' : m.val = bits.val % 2 ^ 23 := by
    rw [hm, UScalar.val_and, show (8388607#u32 : U32).val = 2 ^ 23 - 1 from rfl,
      Nat.and_two_pow_sub_one_eq_mod]
  have hi' : i.val = (bits.val.testBit 23).toNat * 2 ^ 23 := by
    rw [hi, UScalar.val_and, show (8388608#u32 : U32).val = 2 ^ 23 from rfl, Nat.and_two_pow]
  have hE : e.val < 256 := by rw [he']; omega
  have hM : m.val < 2 ^ 23 := by rw [hm']; omega
  rw [← he', ← hm']
  split
  · -- The sign bit: `ToTarget` is 0.
    rename_i hne
    have hne' : i.val ≠ 0 := by
      intro h; apply absurd hne; simp [bne_iff_ne, ne_eq, UScalar.eq_equiv, h]
    have : bits.val.testBit 23 = true := by
      cases h : bits.val.testBit 23
      · rw [h] at hi'; simp at hi'; exact absurd hi' hne'
      · rfl
    simp [this]
  rename_i hne
  have hi0 : i.val = 0 := by
    by_contra h; apply hne; simp [bne_iff_ne, ne_eq, UScalar.eq_equiv, h]
  have htb : bits.val.testBit 23 = false := by
    cases h : bits.val.testBit 23
    · rfl
    · rw [h] at hi'; simp at hi'; omega
  simp only [htb, Bool.false_eq_true, ↓reduceIte]
  split
  · -- A zero mantissa: `ToTarget` is 0.
    rename_i hm0
    have : m.val = 0 := by rw [hm0]; rfl
    simp [this]
  rename_i hm0
  have hm1 : 1 ≤ m.val := by
    by_contra h; apply hm0; apply UScalar.eq_of_val_eq; simp; omega
  split
  · -- The overflow condition: `ToTarget` is `2^256` or more.
    rename_i hov
    simp only [Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq] at hov
    have hE4 : 3 ≤ e.val := by
      rcases hov with (h | ⟨_, h⟩) | ⟨_, h⟩ <;> scalar_tac
    simp only [WP.spec_ok, hE4, ↓reduceIte]
    right
    rcases hov with (h | ⟨h1, h⟩) | ⟨h1, h⟩
    · have h1 : 32 ≤ e.val - 3 := by scalar_tac
      calc 2 ^ 256 = 256 ^ 32 := by norm_num
        _ ≤ 256 ^ (e.val - 3) := Nat.pow_le_pow_right (by norm_num) h1
        _ ≤ m.val * 256 ^ (e.val - 3) := Nat.le_mul_of_pos_left _ (by omega)
    · have h2 : 31 ≤ e.val - 3 := by scalar_tac
      have h3 : 256 ≤ m.val := by scalar_tac
      calc 2 ^ 256 = 256 * 256 ^ 31 := by norm_num
        _ ≤ m.val * 256 ^ (e.val - 3) :=
          Nat.mul_le_mul h3 (Nat.pow_le_pow_right (by norm_num) h2)
    · have h2 : 30 ≤ e.val - 3 := by scalar_tac
      have h3 : 65536 ≤ m.val := by scalar_tac
      calc 2 ^ 256 = 65536 * 256 ^ 30 := by norm_num
        _ ≤ m.val * 256 ^ (e.val - 3) :=
          Nat.mul_le_mul h3 (Nat.pow_le_pow_right (by norm_num) h2)
  rename_i hov
  simp only [Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq, not_or, not_and] at hov
  have hov1 : e.val ≤ 34 := by have := hov.1.1; scalar_tac
  have hov2 : 256 ≤ m.val → e.val ≤ 33 := fun h => by have := hov.1.2 (by scalar_tac); scalar_tac
  have hov3 : 65536 ≤ m.val → e.val ≤ 32 := fun h => by have := hov.2 (by scalar_tac); scalar_tac
  split
  · -- An exponent of at most 3: the mantissa shifted right.
    rename_i hle
    have hle' : e.val ≤ 3 := hle
    step as ⟨ i1, hi1 ⟩
    step as ⟨ i2, hi2 ⟩
    step as ⟨ sh, hsh ⟩
    have hshv : sh.val = m.val / 256 ^ (3 - e.val) := by
      rw [hsh, Nat.shiftRight_eq_div_pow, hi2, hi1, pow_mul]; rfl
    have hV : (if 3 ≤ e.val then m.val * 256 ^ (e.val - 3) else m.val / 256 ^ (3 - e.val))
        = sh.val := by
      rw [hshv]; split
      · have : e.val = 3 := by omega
        simp [this]
      · rfl
    rw [hV]
    split
    · rename_i hsh0
      have : sh.val = 0 := by rw [hsh0]; rfl
      simp [this]
    rename_i hsh0
    have hsh1 : 1 ≤ sh.val := by
      by_contra h; apply hsh0; apply UScalar.eq_of_val_eq; simp; omega
    try simp only [lift, Std.bind_ok]
    step with copy4_spec (t0 := Array.repeat 32#usize 0#u8) as ⟨ t1, ht1 ⟩
    step with from_le_bytes_spec as ⟨ u, hu ⟩
    try simp only [WP.spec_ok]
    have hsh32 : sh.val < 2 ^ 32 := sh.hBounds
    refine ⟨?_, by omega, by omega⟩
    rw [hu, ← u32_to_le_bytes_arr_val sh]
    rw [bytesVal_eq_sum t1.val, bytesVal_eq_sum]
    simp only [Array.length_eq, List.length_map, BitVec.toLEBytes_length]
    rw [show ((32#usize : Usize) : ℕ) = 32 from rfl]
    rw [Finset.sum_congr rfl (fun n hn => by rw [ht1 n (Finset.mem_range.mp hn)])]
    simp [Finset.sum_range_succ, Array.repeat_val]
    done
  · -- An exponent above 3: the mantissa at byte `exponent − 3`.
    rename_i hle
    have hle' : 3 < e.val := by scalar_tac
    step as ⟨ i1, hi1 ⟩
    step with usize_try_from_u32_spec as ⟨ r, off, hr, hoff ⟩
    rw [hr]
    try simp only [lift, Std.bind_ok]
    step with copy3_spec (t0 := Array.repeat 32#usize 0#u8) as ⟨ t1, ht1 ⟩
    case ht => intro n hn; simp [Array.repeat_val]
    step with from_le_bytes_spec as ⟨ u, hu ⟩
    simp only [WP.spec_ok, show 3 ≤ e.val by omega, ↓reduceIte]
    have hbytes := u32_to_le_bytes_arr_val m
    have hval := shifted_val _ t1 off.val (by scalar_tac)
      (fun n hn => by rw [ht1 n hn, zero32 n hn])
      (by rw [hbytes]; omega) (fun h => by rw [hbytes]; by_contra h'; have := hov3 (by omega); scalar_tac)
      (fun h => by rw [hbytes]; by_contra h'; have := hov2 (by omega); scalar_tac)
    rw [hbytes] at hval
    have hoff' : off.val = e.val - 3 := by scalar_tac
    rw [← hoff']
    refine ⟨by rw [hu, hval], ?_, ?_⟩
    · have := Nat.one_le_two_pow (n := 8 * off.val)
      have h256 : 256 ^ off.val = 2 ^ (8 * off.val) := by rw [pow_mul]; norm_num
      rw [h256]; nlinarith
    · have hoff31 : off.val ≤ 31 := by omega
      rcases (show off.val ≤ 29 ∨ off.val = 30 ∨ off.val = 31 by omega) with h | h | h
      · calc m.val * 256 ^ off.val < 2 ^ 23 * 256 ^ off.val :=
              Nat.mul_lt_mul_of_pos_right hM (by positivity)
          _ ≤ 2 ^ 23 * 256 ^ 29 := Nat.mul_le_mul_left _ (Nat.pow_le_pow_right (by norm_num) h)
          _ < 2 ^ 256 := by norm_num
      · have : m.val < 65536 := by by_contra h'; have := hov3 (by omega); scalar_tac
        rw [h]; norm_num; omega
      · have : m.val < 256 := by by_contra h'; have := hov2 (by omega); scalar_tac
        rw [h]; norm_num; omega

end Hayai.Proofs.Compact
