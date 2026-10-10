import Lake
open Lake DSL

/-- The Lean library of Aeneas: the models of the Rust primitives (`Aeneas.Std`), the
`progress` tactic and the scalar lemmas. Pinned to the commit that generated `Hayai/Core`. -/
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "d119a474" / "backends/lean"

package «hayai-formal» where
  leanOptions := #[⟨`autoImplicit, false⟩]

/-- The translation of `hayai-consensus-core` (generated, `scripts/extract.sh`), the
specification of the consensus rules, and the proofs that the translation satisfies it. -/
@[default_target] lean_lib «Hayai» where
  globs := #[.andSubmodules `Hayai]
