/-
ZIP 200, the network upgrade mechanism: the rules of a block are those of the epoch of its
height, and the epoch of a height is the latest upgrade activated at or below it.

Written from ZIP 200. `acts` lists the activation heights of the upgrades in the order of the
protocol (Sprout, Overwinter, ..., NU7): `none` for an upgrade that the chain does not
activate.
-/
import Mathlib.Tactic

namespace Hayai.Spec.Upgrades

/-- The upgrade at index `i` is active at `height`: its activation height is at most `height`. -/
def activeAt (acts : List (Option ℕ)) (height i : ℕ) : Bool :=
  match acts[i]? with
  | some (some a) => decide (a ≤ height)
  | _ => false

/-- The latest upgrade among the first `n` that is active at `height`, if any. -/
def latestActive (acts : List (Option ℕ)) (height : ℕ) : ℕ → Option ℕ
  | 0 => none
  | i + 1 => if activeAt acts height i then some i else latestActive acts height i

/-- ZIP 200: the epoch of `height` is the latest upgrade activated at or below it; the block at
`ACTIVATION_HEIGHT − 1` is still in the epoch before. `none` when no upgrade is active, which
a checked chain never has: Sprout activates at 0. -/
def epochAt (acts : List (Option ℕ)) (height : ℕ) : Option ℕ :=
  latestActive acts height acts.length

/-- The upgrades in the order of the protocol, by index: Sprout 0, Overwinter 1, Sapling 2,
Blossom 3, Heartwood 4, Canopy 5, NU5 6, NU6 7, NU6.1 8, NU6.2 9, NU6.3 10, NU7 11. -/
def blossom : ℕ := 3
def nu7 : ℕ := 11

/-! ## The difficulty parameters of an epoch (ZIP 205, ZIP 208, ZIP 218) -/

/-- `PoWTargetSpacing`: 150 s (`PreBlossomPoWTargetSpacing`), 75 s from Blossom
(`PostBlossomPoWTargetSpacing`, ZIP 208), 25 s from NU7 (`PostNU7PoWTargetSpacing`, ZIP 218). -/
def targetSpacing (epoch : ℕ) : ℕ :=
  if epoch < blossom then 150 else if epoch < nu7 then 75 else 25

/-- `PoWAveragingWindow`: 17, and 102 from NU7 (`PostNU7PoWAveragingWindow`, ZIP 218). Blossom
does not change it (ZIP 208). -/
def averagingWindow (epoch : ℕ) : ℕ := if epoch < nu7 then 17 else 102

/-- The gap of the Testnet minimum-difficulty rule, in target spacings: 6 (ZIP 208: 6 ·
`PoWTargetSpacing`, so 15 minutes before Blossom as in ZIP 205), and 18 from NU7 (ZIP 218:
18 · 25 s = 450 s). -/
def minDifficultyGapSpacings (epoch : ℕ) : ℕ := if epoch < nu7 then 6 else 18

end Hayai.Spec.Upgrades
