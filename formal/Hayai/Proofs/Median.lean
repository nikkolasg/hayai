/-
`median_time` of `hayai-consensus-core::difficulty_rules` computes `median` of §7.7.3: the
element at index `length / 2` of the sorted list.

The code counts instead of sorting: the element at index `k` of the sorted list is the `t`
with `#{x < t} ≤ k < #{x ≤ t}`. `sorted_get_iff` is that fact for any sorted list, and the
counts do not change under permutation.
-/
import Hayai.Core
import Hayai.Spec.Difficulty

open Aeneas Aeneas.Std Result
open HayaiCore HayaiCore.difficulty_rules
open Hayai.Spec.Difficulty

namespace Hayai.Proofs.Median

/-- In a sorted list, a predicate closed downward holds at index `k` exactly when `k` is below
the number of elements that satisfy it. -/
theorem sorted_get_iff (p : ℕ → Bool) (hp : ∀ a b, a ≤ b → p b → p a) :
    ∀ (S : List ℕ), S.Pairwise (· ≤ ·) → ∀ k (hk : k < S.length),
      p S[k] ↔ k < S.countP p
  | [], _, k, hk => by simp at hk
  | a :: S, hs, k, hk => by
    rw [List.pairwise_cons] at hs
    obtain ⟨ha, hs⟩ := hs
    by_cases hpa : p a
    · rw [List.countP_cons_of_pos hpa]
      cases k with
      | zero => simp [hpa]
      | succ k =>
        simp only [List.getElem_cons_succ]
        rw [sorted_get_iff p hp S hs k (by simpa using hk)]
        omega
    · rw [List.countP_cons_of_neg hpa]
      have hall : ∀ x ∈ S, p x = false := fun x hx => by
        by_contra h; simp only [Bool.not_eq_false] at h; exact hpa (hp a x (ha x hx) h)
      rw [List.countP_eq_zero.mpr (fun x hx => by simp [hall x hx])]
      cases k with
      | zero => simp [hpa]
      | succ k =>
        simp only [List.getElem_cons_succ]
        have hx := hall _ (List.getElem_mem (l := S) (n := k) (by simpa using hk))
        simp [hx]

/-- The sorted list of §7.7.3. -/
abbrev sorted (L : List ℕ) : List ℕ := L.mergeSort (fun a b => decide (a ≤ b))

theorem sorted_pairwise (L : List ℕ) : (sorted L).Pairwise (· ≤ ·) := by
  have := List.sorted_mergeSort (le := fun a b => decide (a ≤ b))
    (fun a b c hab hbc => by simp at *; omega) (fun a b => by simp; omega) L
  simpa using this

theorem sorted_perm (L : List ℕ) : (sorted L).Perm L := List.mergeSort_perm L _

/-- The element at index `k` of the sorted list is the `t` with `#{x < t} ≤ k < #{x ≤ t}`. -/
theorem sorted_get_eq_iff (L : List ℕ) (k : ℕ) (hk : k < L.length) (t : ℕ) :
    (sorted L)[k]'(by rw [(sorted_perm L).length_eq]; exact hk) = t ↔
      L.countP (fun x => decide (x < t)) ≤ k ∧ k < L.countP (fun x => decide (x ≤ t)) := by
  have hk' : k < (sorted L).length := by rw [(sorted_perm L).length_eq]; exact hk
  have hlt := sorted_get_iff (fun x => decide (x < t)) (fun a b hab h => by simp at *; omega)
    (sorted L) (sorted_pairwise L) k hk'
  have hle := sorted_get_iff (fun x => decide (x ≤ t)) (fun a b hab h => by simp at *; omega)
    (sorted L) (sorted_pairwise L) k hk'
  rw [(sorted_perm L).countP_eq] at hlt hle
  simp only [decide_eq_true_eq] at hlt hle
  omega

/-- The times as natural numbers. -/
abbrev nats (times : Slice U32) : List ℕ := times.val.map (fun x => x.val)

theorem countP_take_succ (L : List ℕ) (p : ℕ → Bool) (j : ℕ) (h : j < L.length) :
    (L.take (j + 1)).countP p = (L.take j).countP p + if p L[j] then 1 else 0 := by
  rw [List.take_succ, List.getElem?_eq_getElem h, List.countP_append]
  split <;> simp_all

/-- The inner loop counts the times below `c` and the times at most `c`. -/
theorem count_loop_spec (times : Slice U32) (c : U32) (iter : core.ops.range.Range Usize)
    (below at_most : Usize) (hend : iter.end.val = (nats times).length)
    (hs : iter.start.val ≤ (nats times).length)
    (hb : below.val = ((nats times).take iter.start.val).countP (fun x => decide (x < c.val)))
    (ha : at_most.val = ((nats times).take iter.start.val).countP (fun x => decide (x ≤ c.val))) :
    median_time_loop0_loop0 iter times c below at_most ⦃ (b : Usize) (a : Usize) =>
      b.val = (nats times).countP (fun x => decide (x < c.val)) ∧
      a.val = (nats times).countP (fun x => decide (x ≤ c.val)) ⦄ := by
  unfold median_time_loop0_loop0
  apply loop.spec_decr_nat
    (measure := fun (s : core.ops.range.Range Usize × Usize × Usize) =>
      (nats times).length - s.1.start.val)
    (inv := fun (s : core.ops.range.Range Usize × Usize × Usize) =>
      s.1.end.val = (nats times).length ∧ s.1.start.val ≤ (nats times).length ∧
      s.2.1.val = ((nats times).take s.1.start.val).countP (fun x => decide (x < c.val)) ∧
      s.2.2.val = ((nats times).take s.1.start.val).countP (fun x => decide (x ≤ c.val)))
  · rintro ⟨it, b, a⟩ ⟨hend', hs', hb', ha'⟩
    simp only at hend' hs' hb' ha'
    have hlen : (nats times).length = times.val.length := by simp
    have hmax : times.val.length ≤ Usize.max := times.length_ineq
    unfold median_time_loop0_loop0.body
    step as ⟨ o, it1, ho, hit1 ⟩
    by_cases hlt : it.start.val < it.end.val
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      have hj : it.start.val < (nats times).length := by omega
      have hcb : ((nats times).take it.start.val).countP (fun x => decide (x < c.val)) ≤ it.start.val :=
        le_trans (List.countP_le_length) (by simp)
      have hca : ((nats times).take it.start.val).countP (fun x => decide (x ≤ c.val)) ≤ it.start.val :=
        le_trans (List.countP_le_length) (by simp)
      step as ⟨ x, hx ⟩
      have hxv : (nats times)[it.start.val] = x.val := by simp [hx]
      have he1 : it1.end.val = (nats times).length := by rw [hit1]; exact hend'
      have hb2 := countP_take_succ (nats times) (fun x => decide (x < c.val)) _ hj
      have ha2 := countP_take_succ (nats times) (fun x => decide (x ≤ c.val)) _ hj
      rw [hxv, ← hstart1] at hb2 ha2
      split
      · rename_i hxc
        have h1 : x.val < c.val := hxc
        step as ⟨ b1, hb1 ⟩
        split
        · step as ⟨ a1, ha1 ⟩
          simp only [h1, decide_true, ↓reduceIte, show x.val ≤ c.val by omega] at hb2 ha2
          exact ⟨he1, by omega, by omega, by omega, by omega⟩
        · rename_i hxc2; exfalso; exact hxc2 (by scalar_tac)
      · rename_i hxc
        have h1 : ¬ x.val < c.val := hxc
        split
        · rename_i hxc2
          have h2 : x.val ≤ c.val := hxc2
          step as ⟨ a1, ha1 ⟩
          simp only [h1, h2, decide_true, decide_false, Bool.false_eq_true, ↓reduceIte] at hb2 ha2
          exact ⟨he1, by omega, by omega, by omega, by omega⟩
        · rename_i hxc2
          have h2 : ¬ x.val ≤ c.val := hxc2
          simp only [h1, h2, decide_false, Bool.false_eq_true, ↓reduceIte] at hb2 ha2
          simp only [Std.bind_ok, WP.spec_ok]
          exact ⟨he1, by omega, by omega, by omega, by omega⟩
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      have hfull : it.start.val = (nats times).length := by omega
      simp only [WP.spec_ok]
      rw [hfull, List.take_length] at hb' ha'
      exact ⟨hb', ha'⟩
  · exact ⟨hend, hs, hb, ha⟩

/-- `t` is the element at index `length / 2` of the sorted list (`sorted_get_eq_iff`). -/
def isMedian (L : List ℕ) (t : ℕ) : Prop :=
  L.countP (fun x => decide (x < t)) ≤ L.length / 2 ∧ L.length / 2 < L.countP (fun x => decide (x ≤ t))

theorem isMedian_iff (L : List ℕ) (hL : L ≠ []) (t : ℕ) : isMedian L t ↔ median L = t := by
  have hk : L.length / 2 < L.length := by
    have : 0 < L.length := List.length_pos_iff.mpr hL
    omega
  have hk' : L.length / 2 < (sorted L).length := by rw [(sorted_perm L).length_eq]; exact hk
  rw [isMedian, ← sorted_get_eq_iff L _ hk t]
  simp only [median, getElem!_pos (sorted L) (L.length / 2) hk']

theorem median_mem (L : List ℕ) (hL : L ≠ []) : median L ∈ L := by
  have hk : L.length / 2 < L.length := by
    have : 0 < L.length := List.length_pos_iff.mpr hL
    omega
  have hk' : L.length / 2 < (sorted L).length := by rw [(sorted_perm L).length_eq]; exact hk
  simp only [median, getElem!_pos (sorted L) (L.length / 2) hk']
  exact (sorted_perm L).mem_iff.mp (List.getElem_mem _)

/-- §7.7.3, `median`: `median_time` returns the element at index `length / 2` of the sorted
list, and `None` for an empty list. -/
theorem median_time_spec (times : Slice U32) :
    median_time times ⦃ r => r.map (fun x => x.val) =
      if nats times = [] then none else some (median (nats times)) ⦄ := by
  unfold median_time median_time_loop0
  have hlen : (nats times).length = times.val.length := by simp
  have hmax : times.val.length ≤ Usize.max := times.length_ineq
  step as ⟨ middle, hmiddle ⟩
  apply loop.spec_decr_nat
    (measure := fun (s : core.ops.range.Range Usize × Option U32) =>
      (nats times).length - s.1.start.val)
    (inv := fun (s : core.ops.range.Range Usize × Option U32) =>
      s.1.end.val = (nats times).length ∧ s.1.start.val ≤ (nats times).length ∧
      (s.2 = none → ∀ i (h : i < s.1.start.val) (h' : i < (nats times).length),
        ¬ isMedian (nats times) (nats times)[i]) ∧
      (∀ t, s.2 = some t → nats times ≠ [] ∧ median (nats times) = t.val))
  · rintro ⟨it, found⟩ ⟨hend', hs', hnone, hsome⟩
    simp only at hend' hs' hnone hsome
    unfold median_time_loop0.body
    step as ⟨ o, it1, ho, hit1 ⟩
    by_cases hlt : it.start.val < it.end.val
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      have he1 : it1.end.val = (nats times).length := by rw [hit1]; exact hend'
      have hj : it.start.val < (nats times).length := by omega
      rcases found with _ | t
      · step as ⟨ c, hc ⟩
        have hcv : (nats times)[it.start.val] = c.val := by simp [hc]
        step with count_loop_spec as ⟨ below, at_most, hbelow, hat ⟩
        case hb => simp
        case ha => simp
        have hne : nats times ≠ [] := by intro h; simp [h] at hj
        split
        · split
          · rename_i h1 h2
            simp only [WP.spec_ok]
            refine ⟨he1, by omega, by simp, ?_, by omega⟩
            intro t' ht'
            simp only [Option.some.injEq] at ht'
            subst ht'
            refine ⟨hne, (isMedian_iff _ hne _).mp ⟨?_, ?_⟩⟩ <;> scalar_tac
          · rename_i h1 h2
            simp only [WP.spec_ok]
            refine ⟨he1, by omega, ?_, by simp, by omega⟩
            intro _ i hi hi'
            by_cases hie : i = it.start.val
            · subst hie; rw [hcv]; intro hm; exact h2 (by have := hm.2; scalar_tac)
            · exact hnone rfl i (by omega) hi'
        · rename_i h1
          simp only [WP.spec_ok]
          refine ⟨he1, by omega, ?_, by simp, by omega⟩
          intro _ i hi hi'
          by_cases hie : i = it.start.val
          · subst hie; rw [hcv]; intro hm; exact h1 (by have := hm.1; scalar_tac)
          · exact hnone rfl i (by omega) hi'
      · simp only [WP.spec_ok]
        exact ⟨he1, by omega, by simp, hsome, by omega⟩
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      have hfull : it.start.val = (nats times).length := by omega
      simp only [WP.spec_ok]
      rcases found with _ | t
      · split
        · rfl
        · rename_i hne
          exfalso
          obtain ⟨i, hi, hmi⟩ := List.getElem_of_mem (median_mem _ hne)
          exact hnone rfl i (by omega) hi (by rw [hmi]; exact (isMedian_iff _ hne _).mpr rfl)
      · obtain ⟨hne, hm⟩ := hsome t rfl
        simp [hne, hm]
  · refine ⟨by simp, by simp, fun _ i hi => by simp at hi, fun t ht => by simp at ht⟩

/-- §7.6 and §7.7.3, `MedianTime`: `median_time_past` returns the median of the newest
`PoWMedianBlockSpan` (11) times, or of all of them when fewer exist. -/
theorem median_time_past_spec (times : Slice U32) :
    median_time_past times ⦃ r => r.map (fun x => x.val) =
      if nats times = [] then none else some (medianTime (nats times)) ⦄ := by
  unfold median_time_past
  step*
  have hmin : i1.val = min times.val.length 11 := by
    rw [i1_post]; simp [core.cmp.impls.OrdUsize.min, MEDIAN_TIME_SPAN]; split <;> scalar_tac
  have hs : nats s = (nats times).take powMedianBlockSpan := by
    simp only [nats, s_post, List.slice, Nat.sub_zero, List.drop_zero, hmin, powMedianBlockSpan,
      List.map_take]
    rw [List.take_eq_take_iff]; simp [Nat.min_comm]
  apply WP.spec_mono (median_time_spec s)
  intro r hr
  rw [hr, hs, medianTime]
  simp [powMedianBlockSpan]

end Hayai.Proofs.Median
