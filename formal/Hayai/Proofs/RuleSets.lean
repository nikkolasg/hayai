/-
The rule set tables of `hayai-consensus-core::rule_sets` carry the difficulty constants of
§5.3: `PoWDampingFactor` 4, `PoWMaxAdjustUp` 16 % and `PoWMaxAdjustDown` 32 %.
-/
import Hayai.Core
import Hayai.Proofs.Upgrades

open Aeneas Aeneas.Std Result
open HayaiCore

namespace Hayai.Proofs.RuleSets

/-- The difficulty parameters with the constants of §5.3. -/
def specConstants (p : rule_sets.DifficultyParams) : Prop :=
  p.damping_factor.val = 4 ∧ p.max_adjust_up_percent.val = 16 ∧
    p.max_adjust_down_percent.val = 32

/-- `TxVersions::of` never fails: it ignores the versions outside 1 to 6. -/
@[step]
theorem tx_versions_of_spec (v : Slice U32) : rule_sets.TxVersions.of v ⦃ _ => True ⦄ := by
  unfold rule_sets.TxVersions.of rule_sets.TxVersions.of_loop
  step with loop.spec_decr_nat (measure := fun (s : U8 × Usize) => v.length - s.2.val)
    (inv := fun (s : U8 × Usize) => s.2.val ≤ v.length) (post := fun _ => True)
  · rintro ⟨mask, i⟩ hi
    simp only at hi
    unfold rule_sets.TxVersions.of_loop.body
    simp only
    split
    · step as ⟨ x, hx ⟩
      split
      · split
        · step as ⟨ y, hy ⟩
          step as ⟨ j, hj ⟩
          scalar_tac
        · step as ⟨ j, hj ⟩
          scalar_tac
      · step as ⟨ j, hj ⟩
        scalar_tac
    · simp


theorem script_flags_spec : rule_sets.SCRIPT_FLAGS ⦃ _ => True ⦄ := by
  unfold rule_sets.SCRIPT_FLAGS rule_sets.SCRIPT_VERIFY_P2SH
    rule_sets.SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY
  step*

attribute [local step] script_flags_spec

/-- The difficulty parameters of the epoch of upgrade `j` (`Hayai.Spec.Upgrades`), with the
constants of §5.3. -/
def epochParams (j : ℕ) (d : rule_sets.DifficultyParams) : Prop :=
  specConstants d ∧ d.target_spacing.val = Hayai.Spec.Upgrades.targetSpacing j ∧
    d.averaging_window.val = Hayai.Spec.Upgrades.averagingWindow j ∧
    d.min_difficulty_gap_spacings.val = Hayai.Spec.Upgrades.minDifficultyGapSpacings j

macro "rule_set_tac" : tactic => `(tactic| (
  simp only [chain_spec.Upgrade.branch_id, lift]
  step*
  all_goals try simp_all [epochParams, specConstants, rule_sets.DifficultyParams.PRE_BLOSSOM,
    rule_sets.DifficultyParams.POST_BLOSSOM, rule_sets.DifficultyParams.POST_NU7,
    PRE_BLOSSOM_TARGET_SPACING, POST_BLOSSOM_TARGET_SPACING, POST_NU7_TARGET_SPACING,
    Hayai.Spec.Upgrades.targetSpacing, Hayai.Spec.Upgrades.averagingWindow,
    Hayai.Spec.Upgrades.minDifficultyGapSpacings, Hayai.Spec.Upgrades.blossom,
    Hayai.Spec.Upgrades.nu7]))

@[step] theorem sprout_spec : rule_sets.SPROUT ⦃ r => epochParams 0 r.difficulty ⦄ := by
  unfold rule_sets.SPROUT
  rule_set_tac

@[step] theorem overwinter_spec : rule_sets.OVERWINTER ⦃ r => epochParams 1 r.difficulty ⦄ := by
  unfold rule_sets.OVERWINTER
  rule_set_tac

@[step] theorem sapling_spec : rule_sets.SAPLING ⦃ r => epochParams 2 r.difficulty ⦄ := by
  unfold rule_sets.SAPLING
  rule_set_tac

@[step] theorem blossom_spec : rule_sets.BLOSSOM ⦃ r => epochParams 3 r.difficulty ⦄ := by
  unfold rule_sets.BLOSSOM
  rule_set_tac

@[step] theorem heartwood_spec : rule_sets.HEARTWOOD ⦃ r => epochParams 4 r.difficulty ⦄ := by
  unfold rule_sets.HEARTWOOD
  rule_set_tac

@[step] theorem canopy_spec : rule_sets.CANOPY ⦃ r => epochParams 5 r.difficulty ⦄ := by
  unfold rule_sets.CANOPY
  rule_set_tac

@[step] theorem nu5_spec : rule_sets.NU5 ⦃ r => epochParams 6 r.difficulty ⦄ := by
  unfold rule_sets.NU5
  rule_set_tac

@[step] theorem nu6_spec : rule_sets.NU6 ⦃ r => epochParams 7 r.difficulty ⦄ := by
  unfold rule_sets.NU6
  rule_set_tac

@[step] theorem nu6_1_spec : rule_sets.NU6_1 ⦃ r => epochParams 8 r.difficulty ⦄ := by
  unfold rule_sets.NU6_1
  rule_set_tac

@[step] theorem nu6_1_orchard_disabled_spec : rule_sets.NU6_1_ORCHARD_DISABLED ⦃ r => epochParams 8 r.difficulty ⦄ := by
  unfold rule_sets.NU6_1_ORCHARD_DISABLED
  rule_set_tac

@[step] theorem nu6_2_spec : rule_sets.NU6_2 ⦃ r => epochParams 9 r.difficulty ⦄ := by
  unfold rule_sets.NU6_2
  rule_set_tac

@[step] theorem nu6_3_spec : rule_sets.NU6_3 ⦃ r => epochParams 10 r.difficulty ⦄ := by
  unfold rule_sets.NU6_3
  rule_set_tac

@[step] theorem nu7_spec : rule_sets.NU7 ⦃ r => epochParams 11 r.difficulty ⦄ := by
  unfold rule_sets.NU7
  rule_set_tac

/-- Rule set `i` of the table has the difficulty parameters of the epoch of upgrade `i`. -/
@[step] theorem rule_sets_spec : rule_sets.RULE_SETS ⦃ a =>
    ∀ i rs, a.val[i]? = some rs → epochParams i rs.difficulty ⦄ := by
  unfold rule_sets.RULE_SETS
  step*
  intro i rs hrs
  simp only [Array.make] at hrs
  rcases (show i < 12 ∨ 12 ≤ i by omega) with hi | hi
  · interval_cases i <;> simp at hrs <;> subst hrs <;> assumption
  · rw [List.getElem?_eq_none (by simp; omega)] at hrs; cases hrs

@[step] theorem upgrade_eq_spec (a b : chain_spec.Upgrade) :
    chain_spec.Upgrade.Insts.CoreCmpPartialEqUpgrade.eq a b ⦃ r => r ↔ a = b ⦄ := by
  unfold chain_spec.Upgrade.Insts.CoreCmpPartialEqUpgrade.eq
  cases a <;> cases b <;> simp [chain_spec.Upgrade.read_discriminant]

@[step] theorem upgrade_index_spec (u : chain_spec.Upgrade) :
    chain_spec.Upgrade.index u ⦃ i => i.val < 12 ⦄ := by
  cases u <;> simp [chain_spec.Upgrade.index]

@[step] theorem orchard_disabled_spec (spec : chain_spec.CoreSpec) (height : U32) :
    chain_spec.CoreSpec.orchard_disabled spec height ⦃ _ => True ⦄ := by
  unfold chain_spec.CoreSpec.orchard_disabled chain_spec.CoreSpec.activation_height
  split <;> step* <;> split <;> (try split) <;> simp

theorem latestActive_lt (acts : List (Option ℕ)) (h : ℕ) :
    ∀ n j, Hayai.Spec.Upgrades.latestActive acts h n = some j → j < n
  | 0, j, hj => by simp [Hayai.Spec.Upgrades.latestActive] at hj
  | n + 1, j, hj => by
    simp only [Hayai.Spec.Upgrades.latestActive] at hj
    split at hj
    · simp at hj; omega
    · have := latestActive_lt acts h n j hj; omega

theorem all_index_spec (j : ℕ) (hj : j < 12) :
    (chain_spec.Upgrade.ALL.val[j]!).index ⦃ i => i.val = j ⦄ := by
  interval_cases j <;> simp [chain_spec.Upgrade.ALL, Array.make, chain_spec.Upgrade.index]

theorem all_eq_nu6_1 (j : ℕ) (hj : j < 12)
    (h : chain_spec.Upgrade.ALL.val[j]! = chain_spec.Upgrade.Nu6_1) : j = 8 := by
  interval_cases j <;> simp [chain_spec.Upgrade.ALL, Array.make] at h ⊢

/-- ZIP 200 and the difficulty parameters: `rules_at` selects a rule set with the difficulty
parameters of the epoch of the height (`Hayai.Spec.Upgrades`), and the constants of §5.3. -/
theorem rules_at_epoch (spec : chain_spec.CoreSpec) (height : U32) :
    rule_sets.rules_at spec height ⦃ r => ∀ rs, r = core.result.Result.Ok rs →
      ∃ j, Hayai.Spec.Upgrades.epochAt (Hayai.Proofs.Upgrades.acts spec) height.val = some j ∧
        epochParams j rs.difficulty ⦄ := by
  unfold rule_sets.rules_at
  step with Hayai.Proofs.Upgrades.upgrade_at_spec as ⟨ r, hr ⟩
  split at hr
  · rename_i j hj
    have hj12 : j < 12 := by
      have := latestActive_lt _ _ _ _ hj
      rwa [Hayai.Proofs.Upgrades.acts_length] at this
    rw [hr]
    simp only [core.result.Result.Insts.CoreOpsTry.branch, Std.bind_ok]
    step as ⟨ b, hb ⟩
    split
    · step with core.cmp.PartialEq.ne.trait_default.spec as ⟨ b1, hb1 ⟩
      · exact upgrade_eq_spec _ _
      split
      · simp
      · rename_i hne
        have heq : chain_spec.Upgrade.ALL.val[j]! = chain_spec.Upgrade.Nu6_1 := by
          by_contra h; exact hne (hb1.mpr h)
        have h8 := all_eq_nu6_1 j hj12 heq
        step as ⟨ rs, hrs ⟩
        try simp only [WP.spec_ok]
        intro rs1 h; cases h
        exact ⟨j, hj, by rw [h8]; exact hrs⟩
    · step as ⟨ a, ha ⟩
      step with all_index_spec j hj12 as ⟨ k, hk ⟩
      step as ⟨ rs, hrs ⟩
      try simp only [WP.spec_ok]
      intro rs1 h; cases h
      refine ⟨j, hj, ?_⟩
      rw [← hk]
      apply ha
      rw [hrs]
      exact List.getElem?_eq_getElem _
  · rw [hr]
    simp [core.result.Result.Insts.CoreOpsTry.branch,
      core.result.Result.Insts.CoreOpsTry_traitFromResidualResult.from_residual]

/-- Every rule set that `rules_at` selects carries the difficulty constants of §5.3. -/
theorem rules_at_constants (spec : chain_spec.CoreSpec) (height : U32) :
    rule_sets.rules_at spec height ⦃ r =>
      ∀ rs, r = core.result.Result.Ok rs → specConstants rs.difficulty ⦄ := by
  apply WP.spec_mono (rules_at_epoch spec height)
  intro r hr rs h
  obtain ⟨j, _, hp⟩ := hr rs h
  exact hp.1

end Hayai.Proofs.RuleSets
