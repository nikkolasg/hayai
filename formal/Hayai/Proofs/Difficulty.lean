/-
Bridge proofs for `hayai-consensus-core::difficulty_rules`: the translation computes the
functions of `Hayai.Spec.Difficulty`.
-/
import Hayai.Core
import Hayai.Spec.Difficulty
import Hayai.Proofs.Scalars
import Hayai.Proofs.Uint256
import Hayai.Proofs.RuleSets

open Aeneas Aeneas.Std Result
open HayaiCore HayaiCore.difficulty_rules
open Hayai.Spec.Difficulty Hayai.Proofs.Scalars Hayai.Proofs.RuleSets

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

end Hayai.Proofs.Difficulty
