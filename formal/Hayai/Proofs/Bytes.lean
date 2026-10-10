/-
Little-endian byte strings: the value of a list of bytes, and the specs of the conversions of
the standard library that the 256-bit arithmetic of the core uses.
-/
import Hayai.Core

open Aeneas Aeneas.Std Result

namespace Hayai.Proofs.Bytes

/-- The little-endian value of a list of bytes. -/
def leVal : List Byte → ℕ
  | [] => 0
  | b :: l => b.toNat + 256 * leVal l

theorem leVal_lt : ∀ l : List Byte, leVal l < 2 ^ (8 * l.length)
  | [] => by simp [leVal]
  | b :: l => by
    have := leVal_lt l
    have hb := b.isLt
    simp only [leVal, List.length_cons]
    rw [show 8 * (l.length + 1) = 8 * l.length + 8 by ring, pow_add]
    omega


theorem fromLEBytes_toNat : ∀ l : List Byte, (BitVec.fromLEBytes l).toNat = leVal l
  | [] => by simp [BitVec.fromLEBytes, leVal]
  | b :: l => by
    unfold BitVec.fromLEBytes
    have ih := fromLEBytes_toNat l
    have hlt := leVal_lt l
    have hb := b.isLt
    simp only [BitVec.toNat_or, BitVec.toNat_setWidth, BitVec.toNat_shiftLeft, ih, leVal,
      List.length_cons]
    have hw : 2 ^ (8 * l.length) * 256 = 2 ^ (8 * (l.length + 1)) := by
      rw [show 8 * (l.length + 1) = 8 * l.length + 8 by ring, pow_add]; rfl
    have h1 : b.toNat % 2 ^ (8 * (l.length + 1)) = b.toNat := Nat.mod_eq_of_lt (by
      have : 256 ≤ 2 ^ (8 * (l.length + 1)) := by rw [← hw]; have := Nat.one_le_two_pow (n := 8 * l.length); omega
      omega)
    have h2 : leVal l % 2 ^ (8 * (l.length + 1)) = leVal l := Nat.mod_eq_of_lt (by rw [← hw]; omega)
    have h3 : (leVal l <<< 8) % 2 ^ (8 * (l.length + 1)) = leVal l <<< 8 := Nat.mod_eq_of_lt (by
      rw [Nat.shiftLeft_eq, ← hw]; omega)
    rw [h1, h2, h3, Nat.or_comm, ← Nat.shiftLeft_add_eq_or_of_lt hb, Nat.shiftLeft_eq]
    ring

theorem leVal_append (l1 l2 : List Byte) :
    leVal (l1 ++ l2) = leVal l1 + 2 ^ (8 * l1.length) * leVal l2 := by
  induction l1 with
  | nil => simp [leVal]
  | cons b l ih =>
    simp only [List.cons_append, leVal, ih, List.length_cons]
    rw [show 8 * (l.length + 1) = 8 * l.length + 8 by ring, pow_add]
    ring

/-- The value of a list of `u8`. -/
abbrev bytesVal (l : List U8) : ℕ := leVal (l.map U8.bv)

theorem bytesVal_cons (b : U8) (l : List U8) : bytesVal (b :: l) = b.val + 256 * bytesVal l := by
  simp [bytesVal, leVal]

theorem bytesVal_nil : bytesVal [] = 0 := rfl

theorem u64_from_le_bytes_val (a : Array U8 8#usize) :
    (core.num.U64.from_le_bytes a).val = bytesVal a.val := by
  simp only [core.num.U64.from_le_bytes, UScalar.val]
  rw [BitVec.toNat_cast, fromLEBytes_toNat]

theorem leVal_toLEBytes {w : ℕ} (h : w % 8 = 0) (b : BitVec w) : leVal b.toLEBytes = b.toNat := by
  rw [← fromLEBytes_toNat, BitVec.fromLEBytes_toLEBytes h, BitVec.toNat_cast]

theorem bytesVal_eq_sum : ∀ l : List U8, bytesVal l = ∑ k ∈ Finset.range l.length, l[k]!.val * 256 ^ k
  | [] => by simp [bytesVal_nil]
  | b :: l => by
    rw [bytesVal_cons, bytesVal_eq_sum l, List.length_cons, Finset.sum_range_succ', Finset.mul_sum]
    simp only [List.getElem!_cons_succ, List.getElem!_cons_zero, pow_zero, mul_one, pow_succ]
    rw [add_comm]; congr 1
    apply Finset.sum_congr rfl; intro k _; ring

end Hayai.Proofs.Bytes
