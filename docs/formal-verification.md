# Formal verification of the consensus rules

## Abstract

The goal is a machine-checked guarantee that hayai applies the Zcash consensus rules. The
method has three parts:

1. A Lean 4 specification of every consensus rule of the protocol specification and the ZIPs.
2. A Rust consensus core, `hayai-consensus-core`, in the Rust subset that Charon and Aeneas
   translate to Lean.
3. Bridge proofs: the Lean translation of each core function equals the specification for
   every input.

The core holds the decisions. The other crates (the shell) read the store, run rayon and
batch the cryptography. The proofs cover the core. The shell stays outside the proofs and
has stated contracts.

## Terminology

- **Specification (spec)**: the Lean model of a rule, written from the protocol
  specification and the ZIPs. It is not derived from hayai code.
- **Core**: `crates/hayai-consensus-core`. Pure functions that decide valid or invalid and
  compute the state delta.
- **Shell**: `hayai-state`, `hayai-validate`, `hayai-prepared`. They fetch the data that a
  block reads, run the cryptography, and call the core.
- **Bridge proof**: a Lean proof that the Aeneas translation of a Rust function equals the
  spec for every input.
- **Spec-reading error**: hayai implements a reading of the protocol specification, and the
  network follows another reading. No proof against a hand-written spec finds this class.
- **Trusted base**: the code that a result assumes correct and does not check.

## Motivation

A consensus defect in a miner node causes a chain split (the node mines on a chain that the
network refuses) or a crash (a peer reaches a panic). The review of 2026-10-04 found defects
of these classes: a signed version compared as unsigned, a duplicate txid with the same
merkle root, a peer-reachable `assert`, a work overflow (`docs/reviews/`). The differential
fuzzer against Zakura (`docs/fuzz-findings.md`) found none of them. It also has no oracle for
the chain value pools, the history tree append, NU7 and checkpoints.

## The guarantee

```
∀ state block,
  core_check(ctx(state, block), block) = Ok(delta)  ↔  BlockValid (abs state) block
  ∧ abs(state + delta) = apply (abs state) block
```

- `BlockValid` and `apply` are the spec. `abs` maps the hayai state to the abstract state of
  the spec.
- The theorem holds on the Rust code through the Aeneas translation of the core.
- The shell must satisfy these contracts:
  - The context holds the coin of each input, the presence of each nullifier and anchor,
    the value pools and the history state of the parent.
  - A parallel map of a pure core function equals the sequential map.
  - The cryptographic verdicts are those of the upstream crates.

### Trusted base

- The Lean kernel, the Rust compiler, Charon and Aeneas.
- The cryptography as opaque functions in the spec: Groth16, Halo2, RedJubjub and
  RedPallas, Ed25519 (ZIP 215), the ZIP 213 note decryption, Equihash, BLAKE2b, SHA-256,
  the Pedersen and Sinsemilla hashes.
- The transparent script interpreter (`zcash_script`), until the spec models Script.
- The transaction parser (`zcash_primitives`), until the spec models the encodings.
- The checkpoint list, if the owner keeps the checkpoint path (section Open decisions).

### Validation of the spec

A proof shows that hayai equals the spec. It does not show that the spec equals the network.
Two checks validate the spec:

- Replay of the whole Mainnet and Testnet chains through the compiled spec. The spec must
  accept every block and reach the same value pools and tree roots.
- The compiled spec as a second oracle in `hayai-fuzz`, compared with Zakura on mutated
  blocks.

## Design rules of the core

Every change to `hayai-consensus-core` must keep these rules. `tests/subset.rs` checks the
token rules; the Charon and Aeneas extraction (bd M14) is the real check.

1. Pure and closed. No IO, no clock (the caller passes `now`), no rayon, no locks, no `Arc`,
   no `&dyn`, no async, no global state (`LazyLock`, `OnceLock`, `Box::leak`). Tables are
   `const`. The core takes `&ChainSpec`, `&RuleSet` and plain values; it never sees
   `Network`.
2. Context in, delta out. The shell fetches the data that the block reads. The core returns
   `Result<Delta, RuleError>`.
3. Cryptography and parsing outside. The core takes the verdicts of scripts and proofs as
   inputs, and a hayai struct of facts for each transaction, not
   `zcash_primitives::Transaction`. A hash that a rule needs is a precomputed input or a
   generic trait with static dispatch.
4. Rust subset:
   - Index loops (`while`, `for i in 0..n`), no iterator adapters, no `impl Iterator`.
   - No `HashMap`, `HashSet`, `BTreeMap`. For uniqueness in a block, the shell sorts and
     the core checks in one pass that the input is sorted and has no duplicate.
   - No closure that captures `&mut`. No `return`, `break` or `continue` out of a nested
     loop. No `&mut` as a generic argument. No nested associated types. No serde.
5. Arithmetic. Every money and height operation is checked and returns a typed error.
   `try_from`, not `as`. The 256-bit arithmetic of §7.7.3 uses the own type `Uint256`
   (`[u64; 4]`), not `primitive_types::U256`. Release builds have `overflow-checks` off, so
   only an explicit check catches a wrap.
6. No panics: no `unwrap`, `expect`, `assert!`, `unreachable!`, `panic!`, and no index that
   can fail. Each failure is an error variant.
7. Structure that mirrors the spec. One module for each section of the protocol
   specification or ZIP, one function for each rule, a doc comment that names the section,
   one error variant for each rule, named after its row in `docs/consensus.md`. The Lean
   spec uses the same names.
8. One path. `validate_block`, `commit_prebuilt` and the restart replay call the same core
   functions, so one proof covers all of them.
9. Build. Few dependencies, derives only (`thiserror`), stable Rust, no nightly features,
   no `no_std`. Module paths stay stable, because the extraction starts from them.

## Scope of the other code

| Code | Method |
|---|---|
| Optimized algorithms: `hayai-trees::append_many`, `hayai-wire::merkle_root`, `tx_wire_len`, `canonical_order`, the window index, the header tree `Dag`, `CoinsCache` | Lean proofs over an abstract hash and an abstract map: the algorithm equals its simple reference. Each has a proptest reference today, except `CoinsCache`, which needs one. |
| Concurrent state: the speculative stack, the 3-phase flush, the crash recovery, the download scheduler | Quint (TLA+) models, model checking of the invariants, and Quint traces replayed on the Rust code. No Rust tool verifies locks, rayon and IO in place. |
| The Nakamoto protocol | No work. Published proofs exist (Garay, Kiayias and Leonardos; Pass, Seeman and Shelat). The node obligations are fork choice by most work (the `Dag`) and the work function (`difficulty::block_work`). |

## Plan

| Step | Work | State |
|---|---|---|
| 1 | Core, stage 1: chain parameters, rule sets, subsidy, funding streams, lockbox, founders' reward, NSM, difficulty, header rules, coinbase value (bd M12) | done |
| 2 | Core, stage 2: the contextual block rules of `hayai-state/src/check.rs` (bd M2, hand-off note in `bd show hayai-rlg`) | open |
| 3 | Charon and Aeneas extraction of the core, then in CI (bd M14) | done: `formal/`, CI jobs `formal` and `formal-extract` |
| 4 | Lean spec, by area: header and difficulty, then subsidy and value pools, then the transaction structure, then the contextual rules | open |
| 5 | Replay of the chains through the compiled spec; the spec as a `hayai-fuzz` oracle | open |
| 6 | Bridge proofs from the core to the spec, by area | open |
| 7 | Proofs of the optimized algorithms; Quint models of the concurrent state | open |

The toolchain: Lean 4.31, Aeneas and Charon at the commits of `formal/TOOLCHAIN`,
`formal/scripts/extract.sh` regenerates the translation, and the CI job `formal-extract`
fails on drift. A reader checks the proofs with `elan` and `lake build` only
(`formal/README.md`).

## The subset in practice

The first extraction of the core (2026-10-09) refused 89 sites. Each refusal was reduced
to a small probe crate and fixed in the core; the forms that Aeneas at `formal/TOOLCHAIN`
accepts and refuses:

| Refused | Used instead |
|---|---|
| A `&'static [T]` field in a struct; a struct reached through a reference that holds a reference (`&sets[s]` with `sets: &[Set<'_>]`) | Owned tables: `Vec<StreamSet>`, `Vec<Disbursement>`, `Vec<P2shScript>` in `CoreSpec`; the adapter builds each core once |
| `&ParentChain` (a reference to a struct of slices) | `ParentChain` by value: it is `Copy`, three words |
| `&[(u64, &[u8])]` (a slice of tuples with a reference) | `&[CoinbaseOutput]` with an owned script |
| `return` or `?` inside a loop | A `failure: Option<E>` variable, `break`, and the return after the loop |
| `let Some(x) = array[i] else` | `let item = array[i]; let Some(x) = item else` |
| A function that borrows an argument and returns a `&'static` | The value: `rules_at` returns `RuleSet` |
| A `const fn` that returns a `&'static` element of a const table | A plain `fn` |
| The `Display`, `Debug` and `Error::source` bodies of `thiserror` | Excluded from the extraction (`--exclude`): no rule reads them |
| A local variable with the name of a module (`spec`, `subsidy`, `header`): Lean reads `spec.CoreSpec.checked` as a field of the variable | Modules named as no variable is: `chain_spec`, `rule_sets`, `block_limits`, `subsidy_schedule`, `header_rules`, `difficulty_rules` |

The standard-library items that the Aeneas library has no model of (integer conversions,
`checked_shr`, the `Option` methods, hashing and formatting) are in
`formal/Hayai/Core/FunsExternal.lean`: definitions for the ones that a rule computes with,
axioms for hashing and formatting.

## Decisions

- Checkpoint path (owner, 2026-10-08). `apply_checkpointed` computes the delta with the
  core, but it runs no rule function for a block at or below the last checkpoint, as Zebra
  and Zakura. The proof takes the checkpoint list as an axiom. The node does not check
  these blocks fully: that would slow the first synchronization.
- Pause (owner, 2026-10-08). Steps 2 to 7 of the plan wait. The crate split of the epic
  "Modular crates" goes on and keeps the design rules of the core, so that the plan starts
  from the same core when it resumes.
- Resume (owner, 2026-10-09). The formal work starts with the core only, in stages:
  A, the extraction in CI (done); B, the Lean spec and the bridge proofs of the header and
  difficulty rules; C, the other areas of the core; D, step 2 of the plan. Tool versions
  are pinned by commit, but the pins do not drive the design: a reader builds the proofs
  with `elan` and `lake` only, and the translation is committed.

## Effort

These are estimates. Published ratios are 5 to 14 proof lines for each Rust line (Verus case
studies; SymCrypt on Aeneas with AI-written proofs). `multisig-formal` has about 2,800
hand-written Lean lines for about 300 translated Rust lines.

| Part | Estimate |
|---|---|
| Lean spec of all consensus rules (about 700 rows in `docs/consensus.md`) | 10,000 to 20,000 Lean lines |
| Bridge proofs for the core (about 20,000 Rust lines when M2 is done) | 100,000 to 300,000 Lean lines |

Each network upgrade changes the spec and the proofs together.

## Prior art

| Project | Method |
|---|---|
| Cardano ledger (Agda) | executable formal spec of the ledger rules; conformance tests run the spec and the Haskell node on random states. About 10,000 Agda lines for about 200,000 Haskell lines. |
| Ethereum Beacon Chain (Dafny, ConsenSys) | verified reference implementation, no runtime errors |
| EVMYulLean (Nethermind, Lean) | Lean EVM model that passes 22,330 of 22,332 conformance tests |
| btc-verified (Lean 4) | verified codecs, SHA-256 and chainstate accounting; checks Mainnet blocks at build time |
| SymCrypt (Aeneas, Lean) | functional correctness of 16,700 Rust lines with 237,000 Lean lines, written with AI agents |
| Malachite, CometBFT (Quint) | model-based tests drive the Rust code from Quint traces |
