/-
Bridge proof for `check_contextual` of `hayai-consensus-core::header_rules`: the contextual
header rules of §7.6.
-/
import Hayai.Proofs.Expected
import Hayai.Proofs.Header

open Aeneas Aeneas.Std Result
open HayaiCore HayaiCore.header_rules HayaiCore.difficulty_rules
open Hayai.Spec.Difficulty Hayai.Spec.Header Hayai.Proofs.Scalars Hayai.Proofs.RuleSets
  Hayai.Proofs.Uint256 Hayai.Proofs.Difficulty Hayai.Proofs.Expected Hayai.Proofs.Header

namespace Hayai.Proofs.Contextual

/-- §7.6: on a chain with the proof-of-work rules, a context that holds what the rules read,
the constants of §5.3 and `nBits` of the context that all encode a target, `check_contextual`
accepts the header exactly when it passes the contextual rules of §7.6 (version, target at
most `PoWLimit`, `nTime` above the median-time-past and at most 90 minutes after it, and
`nBits = ThresholdBits(height)`), and returns `Checked`: no rule is left unchecked. -/
theorem check_contextual_spec (spec : chain_spec.CoreSpec) (rules : rule_sets.RuleSet)
    (header : HeaderFields) (chain : ParentChain) (hnp : spec.disable_pow = false)
    (hp : specConstants rules.difficulty)
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
    check_contextual spec rules header chain ⦃ r => match r with
      | core.result.Result.Ok verdict => verdict = HeaderVerdict.Checked ∧
          contextualRules (Bytes.bytesVal spec.pow_limit.val) spec.max_time_start_height.val
            chain.height.val header.version.val header.time.val header.bits.val
            (expectedBits (Bytes.bytesVal spec.pow_limit.val)
              (spec.min_difficulty_start_height.map (fun s => s.val)) chain.height.val
              header.time.val chain.times.val[0]!.val
              rules.difficulty.min_difficulty_gap_spacings.val
              rules.difficulty.target_spacing.val rules.difficulty.averaging_window.val
              (chain.times.val.map (fun x => x.val)) (chain.bits.val.map (fun x => x.val)))
            (chain.times.val.map (fun x => x.val))
      | core.result.Result.Err _ =>
          ¬ contextualRules (Bytes.bytesVal spec.pow_limit.val) spec.max_time_start_height.val
            chain.height.val header.version.val header.time.val header.bits.val
            (expectedBits (Bytes.bytesVal spec.pow_limit.val)
              (spec.min_difficulty_start_height.map (fun s => s.val)) chain.height.val
              header.time.val chain.times.val[0]!.val
              rules.difficulty.min_difficulty_gap_spacings.val
              rules.difficulty.target_spacing.val rules.difficulty.averaging_window.val
              (chain.times.val.map (fun x => x.val)) (chain.bits.val.map (fun x => x.val)))
            (chain.times.val.map (fun x => x.val)) ⦄ := by
  unfold check_contextual
  rw [if_neg (by scalar_tac)]
  step with check_version_spec as ⟨ r1, hr1 ⟩
  rw [hr1]
  split
  swap
  · rename_i hver
    simp [core.result.Result.Insts.CoreOpsTry.branch,
      core.result.Result.Insts.CoreOpsTry_traitFromResidualResult.from_residual,
      contextualRules, hver]
  rename_i hver
  simp only [core.result.Result.Insts.CoreOpsTry.branch, Std.bind_ok]
  step with check_target_spec as ⟨ r2, hr2 ⟩
  rcases r2 with tgt | e
  swap
  · simp [core.result.Result.Insts.CoreOpsTry_traitFromResidualResult.from_residual,
      contextualRules, hr2.1]
  obtain ⟨htgt, _⟩ := hr2
  simp only
  have hMTS : MEDIAN_TIME_SPAN.val = 11 := by simp [MEDIAN_TIME_SPAN]
  step with needed_spec as ⟨ nd, hnd ⟩
  rw [hMTS] at hnd
  rw [if_pos (by scalar_tac)]
  step with Hayai.Proofs.Median.median_time_past_spec as ⟨ o, ho ⟩
  have hnl : (Hayai.Proofs.Median.nats chain.times).length = chain.times.val.length := by simp
  have hne : Hayai.Proofs.Median.nats chain.times ≠ [] := by
    intro h; rw [h, List.length_nil] at hnl; omega
  rw [if_neg hne] at ho
  rcases o with _ | mtp
  · simp at ho
  simp only [Option.map_some, Option.some.injEq] at ho
  simp only
  have hmtp : mtp.val = medianTimePast (chain.times.val.map (fun x => x.val)) := by
    rw [ho]; rfl
  have htb := header.time.hBounds
  simp only [UScalarTy.numBits] at htb
  split
  · rename_i hle
    have hle' : header.time.val ≤ mtp.val := hle
    simp only [WP.spec_ok, contextualRules, timeAfterMedianTimePast]
    omega
  rename_i hgt
  have hgt' : mtp.val < header.time.val := by have := hgt; scalar_tac
  unfold MAX_FUTURE_BLOCK_TIME_MTP
  step as ⟨ c5400, hc ⟩
  simp only [lift, Std.bind_ok]
  have hlim := u32_saturating_add_val mtp c5400
  have hexp := expected_bits_spec spec rules header.time chain hp hpl h0 hW hS hws hg hW11 ht hb hv
  simp only [hnp, Bool.false_eq_true, ↓reduceIte]
  split
  · rename_i hge
    split
    · rename_i hlate
      have : mtp.val + 5400 < header.time.val := by
        have := hlate; rw [hc] at hlim; scalar_tac
      simp only [WP.spec_ok, contextualRules, timeWithinMedianTimePast, maxFutureBlockTimeMtp]
      rw [hmtp] at this
      intro ⟨_, _, _, h4, _⟩; have := h4 hge; omega
    · rename_i hok
      have : header.time.val ≤ mtp.val + 5400 := by
        have := hok; rw [hc] at hlim; scalar_tac
      apply WP.spec_bind hexp
      rintro r ⟨v, rfl, hv3⟩
      simp only
      split
      · rename_i heq
        simp only [WP.spec_ok, true_and, contextualRules, timeAfterMedianTimePast,
          timeWithinMedianTimePast, maxFutureBlockTimeMtp, bitsAsExpected]
        refine ⟨hver, htgt, by rw [← hmtp]; exact hgt', fun _ => by
          rw [← hmtp]; exact this, by rw [← hv3, heq]⟩
      · rename_i hne3
        simp only [WP.spec_ok, contextualRules, bitsAsExpected]
        intro ⟨_, _, _, _, h5⟩; apply hne3; apply UScalar.eq_of_val_eq; rw [hv3, h5]
  · rename_i hge
    apply WP.spec_bind hexp
    rintro r ⟨v, rfl, hv3⟩
    simp only
    split
    · rename_i heq
      simp only [WP.spec_ok, true_and, contextualRules, timeAfterMedianTimePast,
        timeWithinMedianTimePast, maxFutureBlockTimeMtp, bitsAsExpected]
      refine ⟨hver, htgt, by rw [← hmtp]; exact hgt', fun h => by
        exfalso; exact hge h, by rw [← hv3, heq]⟩
    · rename_i hne3
      simp only [WP.spec_ok, contextualRules, bitsAsExpected]
      intro ⟨_, _, _, _, h5⟩; apply hne3; apply UScalar.eq_of_val_eq; rw [hv3, h5]

end Hayai.Proofs.Contextual
