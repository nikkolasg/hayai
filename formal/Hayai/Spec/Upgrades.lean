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

end Hayai.Spec.Upgrades
