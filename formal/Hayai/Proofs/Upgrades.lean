/-
Bridge proofs for the upgrade selection of `hayai-consensus-core::chain_spec`.
-/
import Hayai.Core
import Hayai.Spec.Upgrades

open Aeneas Aeneas.Std Result
open HayaiCore
open Hayai.Spec.Upgrades

namespace Hayai.Proofs.Upgrades

instance : Inhabited chain_spec.Upgrade := ⟨chain_spec.Upgrade.Sprout⟩

/-- The activation heights of a spec, as natural numbers. -/
def acts (spec : chain_spec.CoreSpec) : List (Option ℕ) :=
  spec.activation_heights.val.map (Option.map (fun a => a.val))

theorem acts_length (spec : chain_spec.CoreSpec) : (acts spec).length = 12 := by
  simp [acts]

theorem upgrade_at_loop_spec (spec : chain_spec.CoreSpec) (height : U32) (i : Usize)
    (hi : i.val ≤ 12) :
    chain_spec.CoreSpec.upgrade_at_loop spec height i ⦃ r =>
      r = (latestActive (acts spec) height.val i.val).map (fun j => chain_spec.Upgrade.ALL.val[j]!) ⦄ := by
  unfold chain_spec.CoreSpec.upgrade_at_loop
  apply loop.spec_decr_nat (measure := fun (k : Usize) => k.val)
    (inv := fun (k : Usize) => k.val ≤ 12 ∧
      latestActive (acts spec) height.val k.val = latestActive (acts spec) height.val i.val)
  · rintro k ⟨hk, hlat⟩
    unfold chain_spec.CoreSpec.upgrade_at_loop.body
    split
    · step as ⟨ k1, hk1 ⟩
      step as ⟨ act, hact ⟩
      have hsucc : k.val = k1.val + 1 := by omega
      have hl := hlat
      rw [hsucc] at hl
      simp only [latestActive] at hl
      have hk1' : k1.val < spec.activation_heights.val.length := by simp; omega
      have hact' : (acts spec)[k1.val]? = some (act.map (fun a => a.val)) := by
        simp only [acts, List.getElem?_map, List.getElem?_eq_getElem hk1', hact]; rfl
      rcases act with _ | a
      · simp only [WP.spec_ok]
        have : activeAt (acts spec) height.val k1.val = false := by
          simp [activeAt, hact']
        simp only [this, Bool.false_eq_true, ↓reduceIte] at hl
        exact ⟨by omega, hl, by omega⟩
      · simp only
        split
        · rename_i hle
          step as ⟨ u, hu ⟩
          try simp only [WP.spec_ok]
          have : activeAt (acts spec) height.val k1.val = true := by
            have hx : (acts spec)[k1.val]? = some (some a.val) := by rw [hact']; rfl
            unfold activeAt; rw [hx]; exact decide_eq_true hle
          simp only [this, ↓reduceIte] at hl
          rw [← hl, hu]
          have hb : k1.val < chain_spec.Upgrade.ALL.val.length := by simp [chain_spec.Upgrade.ALL]; omega
          simp [List.getElem!_eq_getElem?_getD, List.getElem?_eq_getElem hb]
        · rename_i hgt
          simp only [WP.spec_ok]
          have : activeAt (acts spec) height.val k1.val = false := by
            have hx : (acts spec)[k1.val]? = some (some a.val) := by rw [hact']; rfl
            unfold activeAt; rw [hx]; exact decide_eq_false hgt
          simp only [this, Bool.false_eq_true, ↓reduceIte] at hl
          exact ⟨by omega, hl, by omega⟩
    · simp only [WP.spec_ok]
      have : k.val = 0 := by scalar_tac
      rw [this] at hlat
      simp [latestActive] at hlat
      rw [← hlat]
      simp
  · exact ⟨hi, rfl⟩

/-- ZIP 200: `upgrade_at` returns the upgrade of the epoch of `height`, and refuses a height
where no upgrade is active (a spec that `CoreSpec::checked` refuses). -/
theorem upgrade_at_spec (spec : chain_spec.CoreSpec) (height : U32) :
    chain_spec.CoreSpec.upgrade_at spec height ⦃ r =>
      r = match epochAt (acts spec) height.val with
        | some j => core.result.Result.Ok (chain_spec.Upgrade.ALL.val[j]!)
        | none => core.result.Result.Err ConsensusError.UncheckedSpec ⦄ := by
  unfold chain_spec.CoreSpec.upgrade_at
  step with upgrade_at_loop_spec as ⟨ found, hfound ⟩
  · simp [chain_spec.UPGRADES]
  · simp only [epochAt, acts_length]
    have h12 : chain_spec.UPGRADES.val = 12 := by simp [chain_spec.UPGRADES]
    rw [h12] at hfound
    rw [hfound]
    cases latestActive (acts spec) height.val 12 <;> simp

end Hayai.Proofs.Upgrades
