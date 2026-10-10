/-
Bridge proofs for `hayai-consensus-core::difficulty_rules`: the translation computes the
functions of `Hayai.Spec.Difficulty`.
-/
import Hayai.Core
import Hayai.Spec.Difficulty
import Hayai.Proofs.Scalars
import Hayai.Proofs.Uint256
import Hayai.Proofs.RuleSets
import Hayai.Proofs.Compact

open Aeneas Aeneas.Std Result
open HayaiCore HayaiCore.difficulty_rules
open Hayai.Spec.Difficulty Hayai.Proofs.Scalars Hayai.Proofs.RuleSets Hayai.Proofs.Uint256
  Hayai.Proofs.Compact

namespace Hayai.Proofs.Difficulty

/-- §7.7.3, `ActualTimespanDamped` and `ActualTimespanBounded`: on the constants of §5.3, which
every rule set that `rules_at` selects carries (`RuleSets.rules_at_constants`),
`bounded_timespan` returns `ActualTimespanBounded` of the actual timespan, and none of its
overflow errors is reachable for a timespan of at most `2^32` seconds either way (the
difference of two `u32` median times). -/
theorem bounded_timespan_spec (p : rule_sets.DifficultyParams) (actual : I64)
    (hp : specConstants p)
    (hw : p.averaging_window.val * p.target_spacing.val ≤ U32.max)
    (ha : -(2 : ℤ) ^ 32 ≤ actual.val ∧ actual.val ≤ 2 ^ 32) :
    bounded_timespan p actual ⦃ r => ∃ v : U64, r = core.result.Result.Ok v ∧
      (v.val : ℤ) = actualTimespanBounded
        (averagingWindowTimespan p.averaging_window.val p.target_spacing.val) actual.val ⦄ := by
  obtain ⟨hd, hu, hdn⟩ := hp
  unfold bounded_timespan averaging_window_timespan
  step as ⟨ x, hx ⟩
  rcases x with _ | ts
  · simp at hx; omega
  obtain ⟨_, hts, _⟩ := hx
  simp only [core.result.Result.Insts.CoreOpsTry.branch, Std.bind_ok]
  step as ⟨ w, hw' ⟩
  try simp only [lift, Std.bind_ok]
  have h1 := I64.checked_sub_bv_spec actual w
  split
  · rename_i h; simp only [I64.checked_sub, I64.checked_div, I64.checked_add, I64.checked_mul] at h; rw [h] at h1; simp at h1; scalar_tac
  rename_i diff h; simp only [I64.checked_sub, I64.checked_div, I64.checked_add, I64.checked_mul] at h; rw [h] at h1; obtain ⟨_, _, hdiff, _⟩ := h1
  step as ⟨ df, hdf ⟩
  try simp only [lift, Std.bind_ok]
  have h2 := I64.checked_div_bv_spec diff df
  split
  · rename_i h; simp only [I64.checked_sub, I64.checked_div, I64.checked_add, I64.checked_mul] at h; rw [h] at h2; simp at h2; scalar_tac
  rename_i stp h; simp only [I64.checked_sub, I64.checked_div, I64.checked_add, I64.checked_mul] at h; rw [h] at h2; obtain ⟨_, _, hstp, _⟩ := h2
  have hsb := tdiv_bounds diff.val df.val
  have h3 := I64.checked_add_bv_spec w stp
  split
  · rename_i h; simp only [I64.checked_sub, I64.checked_div, I64.checked_add, I64.checked_mul] at h; rw [h] at h3; simp at h3; scalar_tac
  rename_i damped h; simp only [I64.checked_sub, I64.checked_div, I64.checked_add, I64.checked_mul] at h; rw [h] at h3; obtain ⟨_, _, hdamped, _⟩ := h3
  have h4 := U32.checked_sub_bv_spec 100#u32 p.max_adjust_up_percent
  split
  · rename_i h; rw [h] at h4; simp at h4; scalar_tac
  rename_i down h; rw [h] at h4; obtain ⟨_, hdown, _⟩ := h4
  have h5 := U32.checked_add_bv_spec 100#u32 p.max_adjust_down_percent
  split
  · rename_i h; rw [h] at h5; simp at h5; scalar_tac
  rename_i up h; rw [h] at h5; obtain ⟨_, hup, _⟩ := h5
  step as ⟨ i1, hi1 ⟩
  try simp only [lift, Std.bind_ok]
  step as ⟨ i2, hi2 ⟩
  try simp only [lift, Std.bind_ok]
  have h6 := I64.checked_mul_bv_spec w i1
  have h7 := I64.checked_mul_bv_spec w i2
  split
  · rename_i h; simp only [I64.checked_sub, I64.checked_div, I64.checked_add, I64.checked_mul] at h; rw [h] at h6; simp at h6; scalar_tac
  rename_i mn h; simp only [I64.checked_sub, I64.checked_div, I64.checked_add, I64.checked_mul] at h; rw [h] at h6; obtain ⟨_, _, hmn, _⟩ := h6
  split
  · rename_i h; simp only [I64.checked_sub, I64.checked_div, I64.checked_add, I64.checked_mul] at h; rw [h] at h7; simp at h7; scalar_tac
  rename_i mx h; simp only [I64.checked_sub, I64.checked_div, I64.checked_add, I64.checked_mul] at h; rw [h] at h7; obtain ⟨_, _, hmx, _⟩ := h7
  step as ⟨ mn1, hmn1 ⟩
  step as ⟨ mx1, hmx1 ⟩
  -- The values of the code, as integers of the specification.
  have hdf4 : df.val = 4 := by rw [hdf, hd]; rfl
  set W : ℕ := p.averaging_window.val * p.target_spacing.val with hW
  have hwv : w.val = (W : ℤ) := by rw [hw', hts]
  have hT : stp.val = Int.tdiv (actual.val - W) 4 := by rw [hstp, hdiff, hdf4, hwv]
  have hdv : damped.val = W + Int.tdiv (actual.val - W) 4 := by rw [hdamped, hwv, hT]
  have hd84 : down.val = 84 := by rw [hdown, hu]; rfl
  have hu132 : up.val = 132 := by rw [hup, hdn]; rfl
  have hmnv : mn1.val = ((W * 84 / 100 : ℕ) : ℤ) := by
    rw [hmn1, hmn, hi1, hd84, hwv]
    rw [Int.tdiv_eq_ediv_of_nonneg (by positivity)]
    push_cast; omega
  have hmxv : mx1.val = ((W * 132 / 100 : ℕ) : ℤ) := by
    rw [hmx1, hmx, hi2, hu132, hwv]
    rw [Int.tdiv_eq_ediv_of_nonneg (by positivity)]
    push_cast; omega
  have hspec : actualTimespanBounded (averagingWindowTimespan p.averaging_window.val
      p.target_spacing.val) actual.val = max mn1.val (min mx1.val damped.val) := by
    simp only [actualTimespanBounded, bound, minActualTimespan, maxActualTimespan,
      actualTimespanDamped, averagingWindowTimespan, powMaxAdjustUpPercent,
      powMaxAdjustDownPercent, powDampingFactor, ← hW, hmnv, hmxv, hdv]
  rw [hspec]
  have hmn0 : 0 ≤ mn1.val := by rw [hmnv]; positivity
  have hle : mn1.val ≤ mx1.val := by rw [hmnv, hmxv]; omega
  split
  · rename_i hlt
    have hlt' : damped.val < mn1.val := hlt
    step with u64_try_from_i64_spec as ⟨ r1, hr1 ⟩
    obtain ⟨v, hv1, hv⟩ := hr1 hmn0
    rw [hv1]; simp only [WP.spec_ok]
    refine ⟨v, rfl, ?_⟩
    rw [hv, min_eq_right (le_of_lt (lt_of_lt_of_le hlt' hle)), max_eq_left (le_of_lt hlt')]
  split
  · rename_i hge hgt
    have hgt' : mx1.val < damped.val := hgt
    step with u64_try_from_i64_spec as ⟨ r1, hr1 ⟩
    obtain ⟨v, hv1, hv⟩ := hr1 (le_trans hmn0 hle)
    rw [hv1]; simp only [WP.spec_ok]
    refine ⟨v, rfl, ?_⟩
    rw [hv, min_eq_left (le_of_lt hgt'), max_eq_right hle]
  · rename_i hge hgt
    have hge' : mn1.val ≤ damped.val := not_lt.mp hge
    have hgt' : damped.val ≤ mx1.val := not_lt.mp hgt
    step with u64_try_from_i64_spec as ⟨ r1, hr1 ⟩
    obtain ⟨v, hv1, hv⟩ := hr1 (le_trans hmn0 hge')
    rw [hv1]; simp only [WP.spec_ok]
    refine ⟨v, rfl, ?_⟩
    rw [hv, min_eq_right hgt', max_eq_right hge']

/-- The targets of the first `j` compact values. -/
def targetSum (bits : Slice U32) (j : ℕ) : ℕ :=
  ((bits.val.take j).map (fun b => toTarget b.val)).sum

theorem targetSum_succ (bits : Slice U32) (j : ℕ) (hj : j < bits.val.length) :
    targetSum bits (j + 1) = targetSum bits j + toTarget bits.val[j]!.val := by
  unfold targetSum
  rw [List.take_succ, List.getElem?_eq_getElem hj, getElem!_pos bits.val j hj]
  simp only [Option.toList_some, List.map_append, List.sum_append, List.map_cons, List.map_nil,
    List.sum_cons, List.sum_nil, add_zero]

/-- A compact value of the context encodes a target: nonzero and below `2^256`. -/
def validTarget (b : ℕ) : Prop := 0 < toTarget b ∧ toTarget b < 2 ^ 256

theorem targetSum_le (bits : Slice U32) :
    ∀ n, n ≤ bits.val.length → (∀ i < n, validTarget bits.val[i]!.val) →
      targetSum bits n ≤ n * (2 ^ 256 - 1)
  | 0, _, _ => by simp [targetSum]
  | n + 1, hn, hv => by
    rw [targetSum_succ bits n (by omega)]
    have ih := targetSum_le bits n (by omega) (fun i hi => hv i (by omega))
    have := (hv n (by omega)).2
    rw [add_mul, one_mul]; omega

theorem mean_target_loop_spec (bits : Slice U32) (count : U64) (hc : count.val = bits.val.length)
    (hc0 : 0 < count.val) (iter : core.ops.range.Range Usize) (Q R : Array U64 4#usize)
    (hend : iter.end.val = bits.val.length) (hs : iter.start.val ≤ bits.val.length)
    (hvalid : ∀ i < iter.start.val, validTarget bits.val[i]!.val)
    (hsum : count.val * toNat Q + toNat R = targetSum bits iter.start.val)
    (hR : toNat R ≤ iter.start.val * count.val) :
    mean_target_loop iter bits count Q R ⦃ r => match r with
      | core.result.Result.Ok m => (∀ i < bits.val.length, validTarget bits.val[i]!.val) ∧
          toNat m = targetSum bits bits.val.length / count.val
      | core.result.Result.Err e => ∃ i < bits.val.length,
          ¬ validTarget bits.val[i]!.val ∧ e = DifficultyError.InvalidContextBits bits.val[i]! ⦄ := by
  unfold mean_target_loop
  apply loop.spec_decr_nat
    (measure := fun (s : core.ops.range.Range Usize × Array U64 4#usize × Array U64 4#usize) =>
      bits.val.length - s.1.start.val)
    (inv := fun (s : core.ops.range.Range Usize × Array U64 4#usize × Array U64 4#usize) =>
      s.1.end.val = bits.val.length ∧ s.1.start.val ≤ bits.val.length ∧
      (∀ i < s.1.start.val, validTarget bits.val[i]!.val) ∧
      count.val * toNat s.2.1 + toNat s.2.2 = targetSum bits s.1.start.val ∧
      toNat s.2.2 ≤ s.1.start.val * count.val)
  · rintro ⟨it, Q', R'⟩ ⟨hend', hs', hvalid', hsum', hR'⟩
    simp only at hend' hs' hvalid' hsum' hR'
    unfold mean_target_loop.body
    step as ⟨ o, it1, ho, hit1 ⟩
    by_cases hlt : it.start.val < it.end.val
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      have he1 : it1.end.val = bits.val.length := by rw [hit1]; exact hend'
      have hj : it.start.val < bits.val.length := by omega
      step as ⟨ c, hc' ⟩
      have hcv : c = bits.val[it.start.val]! := by rw [hc', getElem!_pos bits.val _ hj]
      simp only [target_from_compact]
      step with from_compact_spec as ⟨ o1, ho1 ⟩
      rcases o1 with _ | T
      · simp only [WP.spec_ok]
        refine ⟨it.start.val, hj, ?_, by rw [hcv]⟩
        unfold validTarget; rw [← hcv]; omega
      obtain ⟨hT, hT0, hT1⟩ := ho1
      step with div_rem_u64_spec as ⟨ r, hr ⟩
      rcases r with ⟨q, rem⟩ | e
      · obtain ⟨_, hq, hrem⟩ := hr
        simp only [core.result.Result.Insts.CoreOpsTry.branch, Std.bind_ok]
        have hS := targetSum_succ bits it.start.val hj
        rw [← hcv] at hS
        have hq_le : count.val * toNat q ≤ toTarget c.val := by
          rw [hq, hT]; exact Nat.mul_div_le _ _
        have hrem_lt : rem.val < count.val := by rw [hrem]; exact Nat.mod_lt _ hc0
        have hsum2 : count.val * (toNat Q' + toNat q) + (toNat R' + rem.val) =
            targetSum bits (it.start.val + 1) := by
          rw [hS, ← hsum', ← hT, ← Nat.div_add_mod (toNat T) count.val, ← hq, ← hrem]; ring
        have hvalid2 : ∀ i < it.start.val + 1, validTarget bits.val[i]!.val := by
          intro i hi
          by_cases hie : i = it.start.val
          · subst hie; rw [← hcv]; exact ⟨hT0, hT1⟩
          · exact hvalid' i (by omega)
        have hts := targetSum_le bits (it.start.val + 1) (by omega) hvalid2
        have hcnt : it.start.val + 1 ≤ count.val := by omega
        have hQb : toNat Q' + toNat q < 2 ^ 256 := by
          have h1 : count.val * (toNat Q' + toNat q) ≤ count.val * (2 ^ 256 - 1) := by
            have := Nat.mul_le_mul_right (2 ^ 256 - 1) hcnt; omega
          have := Nat.le_of_mul_le_mul_left h1 hc0
          omega
        step with checked_add_spec as ⟨ o2, ho2a, ho2b ⟩
        rcases o2 with _ | Q2
        · exfalso; have := ho2a.mp rfl; omega
        have hQ2 := ho2b Q2 rfl
        simp only
        step with from_u64_spec as ⟨ u, hu ⟩
        have hcb := count.hBounds
        simp only [UScalarTy.numBits] at hcb
        have hRb : toNat R' + rem.val < 2 ^ 256 := by
          have : (it.start.val + 1) * count.val < 2 ^ 128 := by
            calc (it.start.val + 1) * count.val ≤ count.val * count.val := Nat.mul_le_mul_right _ hcnt
              _ < 2 ^ 64 * 2 ^ 64 := Nat.mul_lt_mul'' hcb hcb
              _ = 2 ^ 128 := by norm_num
          have : it.start.val * count.val ≤ (it.start.val + 1) * count.val :=
            Nat.mul_le_mul_right _ (by omega)
          omega
        step with checked_add_spec as ⟨ o3, ho3a, ho3b ⟩
        rcases o3 with _ | R2
        · exfalso; have := ho3a.mp rfl; rw [hu] at this; omega
        have hR2 := ho3b R2 rfl
        simp only [WP.spec_ok]
        refine ⟨he1, by omega, ?_, ?_, ?_, by omega⟩
        · rw [hstart1]; exact hvalid2
        · rw [hQ2, hR2, hu, hstart1, ← hsum2]
        · rw [hR2, hu, hstart1, add_mul, one_mul]; omega
      · simp only [core.result.Result.Insts.CoreOpsTry.branch, Std.bind_ok]
        obtain ⟨h0, _⟩ := hr; omega
    · simp only [hlt, ↓reduceIte] at ho
      obtain ⟨rfl, hstart1⟩ := ho
      simp only
      have hfull : it.start.val = bits.val.length := by omega
      rw [hfull] at hvalid' hsum' hR'
      step with div_rem_u64_spec as ⟨ r, hr ⟩
      rcases r with ⟨rest, _⟩ | e
      · obtain ⟨_, hrest, _⟩ := hr
        simp only [core.result.Result.Insts.CoreOpsTry.branch, Std.bind_ok]
        have hmean : toNat Q' + toNat rest = targetSum bits bits.val.length / count.val := by
          rw [hrest, ← hsum', Nat.mul_add_div hc0]
        have hts := targetSum_le bits bits.val.length (le_refl _) hvalid'
        have hmb : toNat Q' + toNat rest < 2 ^ 256 := by
          rw [hmean, Nat.div_lt_iff_lt_mul hc0]
          have : bits.val.length * (2 ^ 256 - 1) < 2 ^ 256 * count.val := by
            rw [hc, mul_comm]
            exact Nat.mul_lt_mul_of_pos_right (by norm_num) (by omega)
          omega
        step with checked_add_spec as ⟨ o2, ho2a, ho2b ⟩
        rcases o2 with _ | m
        · exfalso; have := ho2a.mp rfl; omega
        simp only [WP.spec_ok]
        exact ⟨hvalid', by rw [ho2b m rfl, hmean]⟩
      · simp only [core.result.Result.Insts.CoreOpsTry.branch, Std.bind_ok]
        obtain ⟨h0, _⟩ := hr; omega
  · exact ⟨hend, hs, hvalid, hsum, hR⟩

/-- §7.7.3, `MeanTarget`: on a nonempty context whose compact values all encode a target,
`mean_target` returns the mean of the targets, rounded down; otherwise it refuses the first
value that encodes no target. -/
theorem mean_target_spec (bits : Slice U32) (hne : 0 < bits.val.length) :
    mean_target bits ⦃ r => match r with
      | core.result.Result.Ok m => (∀ i < bits.val.length, validTarget bits.val[i]!.val) ∧
          toNat m = meanTarget (bits.val.map (fun b => b.val))
      | core.result.Result.Err e => ∃ i < bits.val.length,
          ¬ validTarget bits.val[i]!.val ∧ e = DifficultyError.InvalidContextBits bits.val[i]! ⦄ := by
  unfold mean_target
  step with u64_try_from_usize_spec as ⟨ r, count, hr, hcount ⟩
  rw [hr]
  simp only
  apply WP.spec_mono (mean_target_loop_spec bits count (by rw [hcount]; simp) (by rw [hcount]; simpa)
    _ _ _ (by simp) (by simp) (fun i hi => by simp at hi) (by simp [ZERO_toNat, targetSum])
    (by simp [ZERO_toNat]))
  intro res hres
  rcases res with m | e
  · obtain ⟨hv, hm⟩ := hres
    refine ⟨hv, ?_⟩
    rw [hm, meanTarget, mean, targetSum, List.take_length, List.map_map, List.length_map, hcount]
    simp; rfl
  · exact hres

end Hayai.Proofs.Difficulty
