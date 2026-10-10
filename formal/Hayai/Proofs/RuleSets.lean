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

macro "rule_set_tac" : tactic => `(tactic| (
  simp only [chain_spec.Upgrade.branch_id, lift]
  step*
  all_goals try simp [specConstants, rule_sets.DifficultyParams.PRE_BLOSSOM,
    rule_sets.DifficultyParams.POST_BLOSSOM, rule_sets.DifficultyParams.POST_NU7]))

@[step] theorem sprout_spec : rule_sets.SPROUT ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.SPROUT
  rule_set_tac

@[step] theorem overwinter_spec : rule_sets.OVERWINTER ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.OVERWINTER
  rule_set_tac

@[step] theorem sapling_spec : rule_sets.SAPLING ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.SAPLING
  rule_set_tac

@[step] theorem blossom_spec : rule_sets.BLOSSOM ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.BLOSSOM
  rule_set_tac

@[step] theorem heartwood_spec : rule_sets.HEARTWOOD ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.HEARTWOOD
  rule_set_tac

@[step] theorem canopy_spec : rule_sets.CANOPY ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.CANOPY
  rule_set_tac

@[step] theorem nu5_spec : rule_sets.NU5 ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.NU5
  rule_set_tac

@[step] theorem nu6_spec : rule_sets.NU6 ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.NU6
  rule_set_tac

@[step] theorem nu6_1_spec : rule_sets.NU6_1 ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.NU6_1
  rule_set_tac

@[step] theorem nu6_1_orchard_disabled_spec : rule_sets.NU6_1_ORCHARD_DISABLED ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.NU6_1_ORCHARD_DISABLED
  rule_set_tac

@[step] theorem nu6_2_spec : rule_sets.NU6_2 ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.NU6_2
  rule_set_tac

@[step] theorem nu6_3_spec : rule_sets.NU6_3 ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.NU6_3
  rule_set_tac

@[step] theorem nu7_spec : rule_sets.NU7 ⦃ r => specConstants r.difficulty ⦄ := by
  unfold rule_sets.NU7
  rule_set_tac

@[step] theorem rule_sets_spec : rule_sets.RULE_SETS ⦃ a =>
    ∀ rs ∈ a.val, specConstants rs.difficulty ⦄ := by
  unfold rule_sets.RULE_SETS
  step*
  intro rs hrs
  simp [Array.make] at hrs
  rcases hrs with h | h | h | h | h | h | h | h | h | h | h | h <;> subst h <;> assumption

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

/-- Every rule set that `rules_at` selects carries the difficulty constants of §5.3. -/
theorem rules_at_constants (spec : chain_spec.CoreSpec) (height : U32) :
    rule_sets.rules_at spec height ⦃ r =>
      ∀ rs, r = core.result.Result.Ok rs → specConstants rs.difficulty ⦄ := by
  unfold rule_sets.rules_at
  step with Hayai.Proofs.Upgrades.upgrade_at_spec as ⟨ r, hr ⟩
  split at hr
  · rw [hr]
    simp only [core.result.Result.Insts.CoreOpsTry.branch, Std.bind_ok]
    step*
    all_goals try exact upgrade_eq_spec _ _
    intro rs1 h
    cases h
    rw [rs_post]
    exact a_post _ (List.getElem_mem _)
  · rw [hr]
    simp [core.result.Result.Insts.CoreOpsTry.branch,
      core.result.Result.Insts.CoreOpsTry_traitFromResidualResult.from_residual]

end Hayai.Proofs.RuleSets
