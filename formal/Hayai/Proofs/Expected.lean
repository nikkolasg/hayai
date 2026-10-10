/-
Bridge proofs for `expected_bits` of `hayai-consensus-core::difficulty_rules`: the Testnet
minimum-difficulty rule, the low heights, and `ThresholdBits` together.
-/
import Hayai.Proofs.Difficulty

open Aeneas Aeneas.Std Result
open HayaiCore HayaiCore.difficulty_rules
open Hayai.Spec.Difficulty Hayai.Proofs.Scalars Hayai.Proofs.RuleSets Hayai.Proofs.Uint256
  Hayai.Proofs.Difficulty

namespace Hayai.Proofs.Expected

/-- ZIP 205, ZIP 208, ZIP 218: `min_difficulty_block` decides the Testnet minimum-difficulty
rule, from the time of the parent (the first time of the context). A context without the
parent is `ContextTooShort` when the rule applies. -/
theorem min_difficulty_block_spec (spec : chain_spec.CoreSpec) (p : rule_sets.DifficultyParams)
    (time : U32) (chain : ParentChain)
    (hg : p.min_difficulty_gap_spacings.val * p.target_spacing.val ≤ U32.max)
    (ht : (∃ s, spec.min_difficulty_start_height = some s ∧ s.val ≤ chain.height.val) →
      0 < chain.times.val.length) :
    min_difficulty_block spec p time chain ⦃ r => r = core.result.Result.Ok (decide
      (minDifficultyApplies (spec.min_difficulty_start_height.map (fun s => s.val))
        chain.height.val time.val chain.times.val[0]!.val
        p.min_difficulty_gap_spacings.val p.target_spacing.val)) ⦄ := by
  unfold min_difficulty_block
  rcases hs : spec.min_difficulty_start_height with _ | s
  · simp [minDifficultyApplies]
  simp only [Option.map_some]
  by_cases hle : s.val ≤ chain.height.val
  · have hge : chain.height ≥ s := hle
    simp only [hge, decide_true, ↓reduceIte]
    have hlen := ht ⟨s, hs, hle⟩
    simp only [Std.bind_ok, ↓reduceIte]
    rw [if_neg (by scalar_tac)]
    step as ⟨ pt, hpt ⟩
    step as ⟨ i1, hi1 ⟩
    step as ⟨ i2, hi2 ⟩
    step as ⟨ gap, hgap ⟩
    have hgs := U32.checked_mul_bv_spec p.min_difficulty_gap_spacings p.target_spacing
    simp only [lift, Std.bind_ok]
    split
    · rename_i h; rw [h] at hgs; simp at hgs; omega
    rename_i allowed h
    rw [h] at hgs
    obtain ⟨_, hal, _⟩ := hgs
    step as ⟨ i3, hi3 ⟩
    simp only [WP.spec_ok, core.result.Result.Ok.injEq, decide_eq_decide, minDifficultyApplies]
    have hpt' : pt.val = chain.times.val[0]!.val := by
      rw [hpt, getElem!_pos chain.times.val 0 hlen]
    constructor
    · intro h'; refine ⟨hle, ?_⟩
      have : i3 < gap := h'
      scalar_tac
    · intro ⟨_, h'⟩
      show i3 < gap
      scalar_tac
  · have hge : ¬ chain.height ≥ s := hle
    simp only [hge, decide_false, Bool.false_eq_true, ↓reduceIte, WP.spec_ok,
      minDifficultyApplies]
    simp [hle]


/-- §7.6 and §7.7.3: `expected_bits` returns the `nBits` of the block (`expectedBits`), on the
constants of §5.3, a context that holds what the rules read, and `nBits` of the context that
all encode a target. `hpl` is what `CoreSpec::checked` checks (`SpecError::PowLimitBits`). -/
theorem expected_bits_spec (spec : chain_spec.CoreSpec) (rules : rule_sets.RuleSet) (time : U32)
    (chain : ParentChain) (hp : specConstants rules.difficulty)
    (hpl : spec.pow_limit_bits.val = toCompact (Bytes.bytesVal spec.pow_limit.val))
    (h0 : 0 < chain.height.val)
    (hW : 1 ≤ rules.difficulty.averaging_window.val) (hS : 1 ≤ rules.difficulty.target_spacing.val)
    (hws : rules.difficulty.averaging_window.val * rules.difficulty.target_spacing.val ≤ U32.max)
    (hg : rules.difficulty.min_difficulty_gap_spacings.val * rules.difficulty.target_spacing.val
      ≤ U32.max)
    (hW11 : rules.difficulty.averaging_window.val + 11 ≤ Usize.max)
    (ht : min (rules.difficulty.averaging_window.val + 11) chain.height.val ≤
      chain.times.val.length)
    (hb : rules.difficulty.averaging_window.val ≤ chain.bits.val.length)
    (hv : ∀ i < rules.difficulty.averaging_window.val, validTarget chain.bits.val[i]!.val) :
    expected_bits spec rules time chain ⦃ r => ∃ v : U32, r = core.result.Result.Ok v ∧
      v.val = expectedBits (Bytes.bytesVal spec.pow_limit.val)
        (spec.min_difficulty_start_height.map (fun s => s.val)) chain.height.val time.val
        chain.times.val[0]!.val rules.difficulty.min_difficulty_gap_spacings.val
        rules.difficulty.target_spacing.val rules.difficulty.averaging_window.val
        (chain.times.val.map (fun x => x.val)) (chain.bits.val.map (fun x => x.val)) ⦄ := by
  unfold expected_bits
  rw [if_neg (by scalar_tac)]
  step with min_difficulty_block_spec as ⟨ r, hr ⟩
  rw [hr]
  simp only [core.result.Result.Insts.CoreOpsTry.branch, Std.bind_ok]
  unfold expectedBits
  split
  · rename_i hmin
    simp only [decide_eq_true_eq] at hmin
    simp only [hmin, ↓reduceIte, WP.spec_ok]
    exact ⟨_, rfl, hpl⟩
  · rename_i hmin
    simp only [decide_eq_true_eq] at hmin
    simp only [hmin, ↓reduceIte]
    by_cases hle : chain.height.val ≤ rules.difficulty.averaging_window.val
    · unfold threshold_bits
      rw [if_pos (by scalar_tac)]
      simp only [hle, ↓reduceIte, WP.spec_ok]
      exact ⟨_, rfl, hpl⟩
    · simp only [hle, ↓reduceIte]
      apply WP.spec_mono (threshold_bits_spec spec rules.difficulty chain hp hW hS hws hW11
        (by omega) ht hb hv)
      intro r hr
      obtain ⟨v, hv1, hv2⟩ := hr
      refine ⟨v, hv1, ?_⟩
      rw [hv2, List.map_take, List.map_take]

end Hayai.Proofs.Expected
