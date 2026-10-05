# Consensus conformance: vector inventory and harness

Scope: the published test vectors on this machine, the block and transaction harness of
`hayai-bench`, and the outcomes on 2026-10-04. Rule numbers (H1, B1, T1) are those of
`docs/plan-consensus-and-sync.md`, section 1. Rule rows are those of `docs/consensus-rules.md`.

Owner decision Q7 applies: the repository holds no block that a node or the network supplied.
Every vector below is a copy of a published vector set.

## Vector sets

Registry paths are below `~/.cargo/registry/src/index.crates.io-*/`. "In-crate" means that the
vectors are `#[cfg(test)]` or `pub(crate)` items: a test of hayai must copy them.

| Set | Location | Content | Cases | Use in hayai | Rules |
|---|---|---|---|---|---|
| Zebra block vectors | `zebra/zebra-test/src/vectors/block-*.txt`, `*.bin` (checkout 02f9648). The same 93 files are in `zakura-src/zakura/crates/zakura-test/src/vectors/` | Whole blocks, Mainnet and Testnet, genesis to NU5 | 90 blocks: 42 Mainnet, 47 Testnet, 1 invalid (Mainnet 202, `-bad`) | All 90 in `hayai-bench/tests/conformance_blocks.rs` (copies in `hayai-bench/tests/vectors/`). 6 in `hayai-wire/tests/vectors.rs`, 2 in `hayai-state/tests/history.rs` | H1–H5, H12, H13, H14; B1–B6, B12, B22–B27, B29; T1, T3–T5, T11–T13, T17–T21, T25. Sprout: T23 on the 5 Groth16 JoinSplits of 4 transactions without a transparent input (`hayai-bench/tests/sprout.rs`); B28 on Mainnet 396 and Testnet 2,259 |
| Zebra final roots | `zebra-test/src/vectors/block.rs` | Final Sprout, Sapling and Orchard roots after a block | 70 roots: 7 Sprout, 52 Sapling, 11 Orchard | Sprout, Sapling and Orchard roots in `conformance_blocks.rs` (`final-roots.json`). The Sprout roots of Mainnet 0 and 396 and of Testnet 0 and 2,259 in `hayai-bench/tests/sprout.rs`. Two Sapling roots in `hayai-state/tests/history.rs` | H12, B27, B28, B29 |
| Zebra Sapling tree state | `zebra-test/src/vectors/sapling-treestate-main-0-419-201.txt` | The Sapling commitment tree after Mainnet 419,201 (`zcashd` encoding) | 1 | Seed of Mainnet 419,202 in `conformance_blocks.rs` | H12, B27 |
| Zebra Orchard note encryption | `zebra-test/src/vectors/orchard_note_encryption.rs` | Orchard note encryption vectors | 20 | None | B18 (ZIP 213, plan item A3) |
| ZIP 143, 243, 244 transactions | `zcash_primitives-0.30.1/src/transaction/tests/data.rs` (in-crate). The same vectors are in `zebra-test/src/zip0143.rs`, `zip0243.rs`, `zip0244.rs` | Generated transactions, their ids, digests and sighash values | 10 + 10 + 10 | Parse, transaction id and authorizing digest in `hayai-wire/tests/scan.rs`. `draft` in `hayai-bench/tests/conformance_txs.rs`. The sighash values are not tested (bd hayai-xya) | T19 |
| v4 transaction of Testnet 280,003 | `zcash_primitives-0.30.1/src/transaction/tests/data.rs` (`tx_read_write`) | One real v4 transaction | 1 | `hayai-wire/tests/scan.rs` | Parse |
| ZIP 233 transactions | `zcash_primitives-0.30.1/src/transaction/tests/data/zip_0233.rs` (in-crate) | v6 transactions of the NU7 format with `zip233_amount`, with sighash values | 10 | None: the parser needs `cfg(zcash_unstable = "nu7")` | T19 for NU7 |
| Script vectors | `zcash_script-0.6.0/src/test_vectors.rs` (public with the feature `test-dependencies`) | The port of zcashd `script_tests.json` without the CSV, DERSIG, MINIMALIF, NULLFAIL and WITNESS cases: scriptSig, scriptPubKey, flags, result, sigop count | 1,046 | All in `conformance_txs.rs` through `Draft::check_input` and `draft` | T19, B6 |
| ZIP 221 history tree | `zcash_history-0.5.0/src/test_vectors/zip_0221_v1.rs`, `v2.rs`, `v3.rs` (in-crate) | Leaves, peaks and roots of a growing tree: Heartwood (V1), NU5 (V2), NU6.3 (V3) | 16 + 16 + 16 | V1, V2 and V3 in `hayai-state/tests/history.rs` | H12, B29 |
| Equihash | `equihash-0.3.0/src/test_vectors/valid.rs`, `invalid.rs` (in-crate) | Solutions for small parameters | 21 valid, 9 invalid | None: hayai calls `equihash::is_valid_solution`, and the crate tests these vectors | H2, H3 |
| Orchard | `orchard-0.15.5/src/test_vectors/` (in-crate) | Commitment tree (1), keys (10), merkle paths (16), note encryption (10), ZIP 32 (4) | 41 | None. `hayai-sinsemilla` and `hayai-trees` test against the upstream implementation | B27 (tree), B18 (note encryption) |
| Sapling | `sapling-crypto-0.7.0/src/test_vectors/`, `src/pedersen_hash/test_vectors.rs` (in-crate) | Note encryption (10), signatures (10), Pedersen hash (37) | 57 | None | B18, T20 |
| Sprout circuit | `zcash_proofs-0.30.0/src/circuit/sprout/test_vectors.dat` | Witness data of the Sprout circuit | 1 file | None | T23 (plan item A5) |
| Sprout `h_sig` and Groth16 | `zebra-consensus-11.0.0/src/primitives/groth16/vectors.rs`, `zakura-consensus/src/primitives/groth16/vectors.rs` | `h_sig` inputs and results | 4 | `hayai-prepared/src/sprout.rs` | T23 |
| Zebra chain vectors | `zebra-chain-13.0.1/src/**/tests/vectors*.rs`, `test_vectors.rs` (also 14.0.0 and `zakura-chain-9.0.0`) | Sprout tree roots (16), Sapling Pedersen and commitment vectors (12), Orchard tree roots (10), Sinsemilla, group hash, compact difficulty encoding | Tests of Zebra's own types on the block vectors above, plus the listed constants | The Sprout empty roots (30) and tree roots (16) in `hayai-trees/src/sprout.rs` | B27, B28, H4 |
| Transparent keys | `zcash_transparent-0.10.0/src/test_vectors.rs`, `zip_0048.rs` (in-crate) | ZIP 316 transparent OVK, ZIP 48 keys | 20 + 20 | None | None: not consensus |
| Addresses and keys | `zcash_address-0.13.0`, `zcash_keys-0.16.1`, `f4jumble-0.1.1` | Unified addresses, key derivation, F4Jumble | — | None | None: not consensus |

## Heights of the Zebra block vectors

Activation heights: Mainnet Overwinter 347,500, Sapling 419,200, Blossom 653,600, Heartwood
903,000, Canopy 1,046,400, NU5 1,687,104. Testnet Overwinter 207,500, Sapling 280,000, Blossom
584,000, Heartwood 903,800, Canopy 1,028,500, NU5 1,842,420.

| Network | Upgrade | Blocks | Heights |
|---|---|---|---|
| Mainnet | Sprout | 16 | 0–10, 202, 202 (`-bad`, invalid), 395, 396, 347,499 |
| Mainnet | Overwinter | 4 | 347,500, 347,501, 415,000, 419,199 |
| Mainnet | Sapling | 5 | 419,200–419,202, 434,873, 653,599 |
| Mainnet | Blossom | 3 | 653,600, 653,601, 902,999 |
| Mainnet | Heartwood | 6 | 903,000, 903,001, 949,496, 975,066, 982,681, 1,046,399 |
| Mainnet | Canopy | 3 | 1,046,400, 1,046,401, 1,180,900 |
| Mainnet | NU5 | 6 | 1,687,106–1,687,108, 1,687,113, 1,687,118, 1,687,121 |
| Testnet | Sprout | 14 | 0–10, 2,259, 141,042, 207,499 |
| Testnet | Overwinter | 3 | 207,500, 207,501, 279,999 |
| Testnet | Sapling | 8 | 280,000, 280,001, 299,187–299,189, 299,201, 299,202, 583,999 |
| Testnet | Blossom | 3 | 584,000, 584,001, 903,799 |
| Testnet | Heartwood | 5 | 903,800, 903,801, 914,678, 925,483, 1,028,499 |
| Testnet | Canopy | 9 | 1,028,500, 1,028,501, 1,095,000, 1,101,629, 1,115,999–1,116,001, 1,326,100, 1,599,199 |
| Testnet | NU5 | 5 | 1,842,421, 1,842,432, 1,842,462, 1,842,467, 1,842,468 |

Final Sapling roots exist for every vector from Sapling activation, except Testnet 1,599,199.
Final Orchard roots exist for the 11 NU5 vectors.

## Missing vectors

- The newest block vector is Mainnet 1,687,121 and Testnet 1,842,468. No block vector exists
  for NU6, NU6.1, NU6.2, NU6.3 or NU7. The NU6.1 lockbox disbursement block, the Orchard
  soft-fork height, a v6 Ironwood block and a block with a shielded coinbase under NU6.3 have
  no vector. The NU6.3 rules are tested on generated blocks (`hayai-bench/tests/ironwood.rs`,
  fixtures `nu6_3_block`): v6 transactions with Ironwood bundles and Orchard bundles of NU6.3
  with proofs under the NU6.3 circuit.
- No vector set of invalid blocks exists. The one invalid block (Mainnet 202, `-bad`) fails at
  the merkle root.
- The only NU6.3 vectors are the 16 ZIP 221 V3 history vectors. The only v6 transaction vectors
  are the ZIP 233 vectors of the NU7 format, which the upstream parser reads only under
  `zcash_unstable = "nu7"`. No v6 sighash vector of the NU6.3 format is on this machine.
- zcashd `tx_valid.json`, `tx_invalid.json`, `sighash.json` and the original
  `script_tests.json` are not on this machine. No crate in the registry ships them.
  `zcash_script` 0.6 ships the port of `script_tests.json` only.
- `zcash-test-vectors` is not on this machine. The crates above hold the parts that the
  upstream crates copied.
- The longest contiguous range is 11 blocks (heights 0 to 10). The difficulty adjustment
  (H6) needs 28 blocks of context above height 17, so no vector can test the adjustment
  itself: the tests of hayai-consensus compare generated chains with a reference
  implementation. The vectors test the parts of H6 and H7 that need less context: the limit
  up to height 17 (heights 1 to 10 of both networks, with the time rules), and the Testnet
  minimum-difficulty blocks (299,188, 299,189, 299,202, 584,000, 903,800, 903,801,
  1,028,500), which need the time of the parent only. For the other vectors the context
  stage trusts the rules whose context is too short (`HeaderPolicy::TrustShortContext`).
- Context that the set does not hold: the spent coins of most transactions with transparent
  inputs, the Sapling and Orchard frontiers and anchors away from the activation heights
  (except the Sapling tree state of Mainnet 419,201), the nullifier sets, the chain value
  pools, the history tree peaks.

## Block harness

`crates/hayai-bench/tests/conformance_blocks.rs`, with `tests/conformance/vectors.rs` (the
vector set), `context.rs` (the chain context) and `expected.rs` (outcomes and the expected
file).

### Stages

A vector runs the stages in order and stops at the first stage that fails.

| Stage | Check | Code under test |
|---|---|---|
| `parse` | The block parses under the branch of its height. The layout scanner and the sequential parser give the same transactions | `RawBlock::parse`, `RawBlock::parse_sequential` |
| `header` | The genesis hash of the network. The next vector builds on the hash. The hash is at most the target. The Equihash solution is valid | `BlockHeader::hash`, `check_pow`, `check_equihash`, `NetworkParams` |
| `merkle_root` | The header root equals the root of the transaction ids | `merkle_root` |
| `transactions` | Each transaction whose spent coins the harness holds: the structural rules, the scripts, the shielded batch (with the JoinSplit proofs and signature of a v4 transaction). Coins come from the block and from the chain context. A v1, v2 or v3 transaction in a block at or below the mandatory checkpoint does not run: hayai has no verification for it, and the result counts it in `checkpoint_only` | `draft`, `check_scripts`, `Draft::add_shielded`, `ScopedBatch::finalize` |
| `context` | The whole block on a chain whose tip is the parent | `hayai_validate::validate_block` with `hayai_consensus::rules_at`. At or below the mandatory checkpoint of the network: `hayai_validate::apply_checkpointed` with the checkpoint list of the network |
| `final_roots` | The Sprout, Sapling and Orchard roots of the layer equal the published final roots | `Layer::anchors`, `Layer::sprout_frontier` |

The `transactions` stage runs every transaction. A rejection has priority over an
`Unsupported` error, so an unsupported transaction cannot hide an invalid one.

The genesis block has no parent state. It stops after `transactions`.

### Classes

| Class | Definition | Vectors |
|---|---|---|
| `genesis` | Height 0 | 2 |
| `range` | The parent or the child of the block is a vector | 66 |
| `isolated` | No neighbour of the block is a vector | 22 |

### Chain context

The vectors of a network run in height order. A valid block is pushed on the chain, so the
next block of a contiguous range runs on the state that hayai built. Every other block gets a
new base at its parent with these seeds:

| State | Seed |
|---|---|
| Parent hash and height | The `prev_hash` of the block and its height minus 1 |
| Coins | The unspent outputs of the earlier vectors of the network. A chain that starts at height 1 holds every coin |
| Sapling and Orchard trees | Empty when the parent is before the activation of the pool: the harness then knows the whole pool. Else the published tree state (Mainnet 419,201). Else the published final root of the parent, or of the block when the block has no output for the pool: the root only |
| Sprout state | The empty state (tree, nullifier set, pool) when the parent is before the first block with a JoinSplit, which `zebra-chain` publishes with the Sprout roots (Mainnet 396, Testnet 2,259). Else unknown (`Base::set_sprout_unknown`): hayai then refuses a block with a JoinSplit |
| Nullifier sets | Empty |
| Value pools | Zero |
| History tree | The empty tree when the parent is before Heartwood. Else unknown: hayai then does not check the header commitment |
| Median time | The time of the parent vector, or the block time minus 1. `validate_block` does not read it |

After the first block of an upgrade from Heartwood, the tree of the new upgrade has one leaf.
The harness sets that tree on the layer when the parent tree was unknown and the roots are
known. The next block is then checked against a real commitment (Canopy activation: Mainnet
1,046,400, Testnet 1,028,500).

The `context` stage does not run when the block reads state that the base does not hold. The
result lists that state in `missing`:

- spent coins that no earlier vector created;
- the frontier of a tree to which the block appends, when only the root is known;
- the root of a tree that the header or the history leaf commits to, when it is unknown;
- an anchor that the chain does not hold, and a value that leaves a pool, when the harness
  does not know the whole pool.

The result lists in `assumed` the context taken on trust: an unknown history tree (the header
commitment is not checked) and nullifiers that are assumed not revealed.

The harness uses the Sapling and Sprout verifying keys that hayai embeds.

The harness gives the network to `validate_block`, which takes the coinbase terms of each
block from `hayai_consensus::coinbase::CoinbaseTerms` (founders' reward, funding stream
outputs, value rule, deferred pool). `conformance_subsidy.rs` runs the same check on the
coinbase of each valid block vector. With the `baselines` feature of `hayai-bench` (a
default feature), it also compares the schedules of hayai-consensus with `zakura-chain` and
`zebra-chain`.

Commands:

- `cargo test -p hayai-bench --release`: the vector tests and the comparisons with
  `zakura-chain`, `zakura-header-chain` and `zebra-chain` (`conformance_nu7.rs`, the module
  `baselines` of `conformance_subsidy.rs`, the Sprout tree comparison of `sprout.rs`).
- `cargo test -p hayai-bench --release --no-default-features --features upstream`: the
  vector tests without the comparisons. The build has no zakura-* crate and no
  `zebra-chain`. The CI of each push uses this feature set.
- On the Zakura backend the two feature sets are `--no-default-features --features
  zakura,baselines` and `--no-default-features --features zakura`.

### Verdicts and the expected file

| Verdict | Meaning |
|---|---|
| `valid` | Every stage passed |
| `context_free` | Every stage before `context` passed. The `context` stage needs state that the set cannot supply |
| `unsupported` | hayai returned an `Unsupported` error |
| `rejected` | hayai rejected the vector |

`tests/vectors/expected-blocks.json` holds, for each vector, the stage, the verdict and the
error of today. An entry that is not `valid` also holds the reason and the plan item that
changes it. The test fails when an outcome differs from the file in either direction, when a
vector has no entry, and when an entry has no vector.

Procedure after a change of an outcome:

1. Run `cargo test -p hayai-bench --release --test conformance_blocks`.
2. Read the differences that the test prints.
3. Copy `target/conformance/expected-blocks.json` to
   `crates/hayai-bench/tests/vectors/expected-blocks.json`. Unchanged entries keep their reason
   and plan item.
4. Write the reason and the plan item of each changed entry that is not `valid`.

A `rejected` verdict on a vector that is published as valid is a consensus defect. It must not
go into the expected file: it goes into "Defects found" below and into a bd issue.

Output files in `target/conformance/`: `blocks.results.json` (one record for each vector:
network, height, upgrade, class, transaction counts, stage, verdict, error, `missing`,
`assumed`), `blocks.summary.txt` (the table below), `expected-blocks.json`.

A second test changes one vector (Testnet 1,842,467) at three places and checks that the
harness stops at the stage of the change: the nonce (`header`), the coinbase lock time
(`merkle_root`), the Orchard binding signature (`transactions`).

### Seeding binary

The plan describes `hayai-bench/src/bin/mkcontext.rs`, a generator that reads the context of a
block from a synced reference node. Owner decision Q7 forbids that source, and the seeds above
come from the vector set inside the test. The binary does not exist.

## Block outcomes

| Class | `valid` | `context_free` | `unsupported` | `rejected` | Total |
|---|---|---|---|---|---|
| `genesis` | 2 | 0 | 0 | 0 | 2 |
| `range` | 49 | 17 | 0 | 0 | 66 |
| `isolated` | 6 | 15 | 0 | 1 | 22 |
| All | 57 | 32 | 0 | 1 | 90 |

- 46 of the 57 valid blocks have no assumed context. 11 run with an unknown history tree.
- 47 of the 57 valid blocks are at or below the mandatory checkpoint: they pass the
  checkpoint path (`apply_checkpointed`), which runs no script, no proof and no coinbase
  rule. 28 of them are before Sapling activation, and their 28 transactions (coinbase
  transactions of v1 and v3) have no verification. The 2 genesis blocks stop after the
  `transactions` stage.
- The 10 valid blocks above the mandatory checkpoint are coinbase-only blocks. They test the
  header rules, the coinbase rules and the coinbase terms (funding streams, value limit) in
  full validation.
- The 19 valid blocks from Sapling activation to the mandatory checkpoint test the Sapling
  root in the header (Sapling and Blossom), the Heartwood activation commitment (Testnet
  903,800, 903,801) and the Canopy activation commitment (Testnet 1,028,501). Their
  `transactions` stage runs `draft` on the coinbase.
- No block stops at an `Unsupported` error.
- The 32 context-free blocks have transactions that spend coins from outside the set, or that
  use tree frontiers, anchors or pool values from outside the set. Testnet 1,842,467 has one
  Orchard transaction without transparent inputs: its proof and signatures pass.
- 9 context-free blocks have JoinSplits on a Sprout state that the set does not hold. In 4 of
  them (Mainnet 419,201, 419,202 and 903,000, Testnet 925,483) the JoinSplits have Groth16
  proofs: the `transactions` stage verifies the proofs and the JoinSplit signature of every
  such transaction that has no transparent input from outside the set. The other 5 are
  before Sapling activation (BCTV14 proofs, no verification).
- Mainnet 396 and Testnet 2,259 hold the first JoinSplit of their network. Each spends one
  coin from outside the set, so the block stage does not run. `hayai-bench/tests/sprout.rs`
  appends their commitments to the empty Sprout tree and compares the root with the published
  final root.
- The embedded verifying keys accept the Sapling proofs and signatures of every transaction
  that the `transactions` stage prepares: the coinbase transactions and the transactions
  without transparent inputs from outside the set.
- 5 blocks have a coinbase with Sapling outputs: Mainnet 949,496, 975,066 and 982,681 and
  Testnet 914,678 (Heartwood, lead byte 0x01), Testnet 1,101,629 (Canopy, lead byte 0x02).
  Each output decrypts with the zero outgoing viewing key (ZIP 213).
- The rejected block is Mainnet 202 `-bad`, which Zebra publishes as invalid.

| Network | Upgrade | `valid` | `context_free` | `unsupported` | `rejected` |
|---|---|---|---|---|---|
| Mainnet | Sprout | 13 | 2 | 0 | 1 |
| Mainnet | Overwinter | 2 | 2 | 0 | 0 |
| Mainnet | Sapling | 2 | 3 | 0 | 0 |
| Mainnet | Blossom | 2 | 1 | 0 | 0 |
| Mainnet | Heartwood | 1 | 5 | 0 | 0 |
| Mainnet | Canopy | 2 | 1 | 0 | 0 |
| Mainnet | NU5 | 0 | 6 | 0 | 0 |
| Testnet | Sprout | 12 | 2 | 0 | 0 |
| Testnet | Overwinter | 1 | 2 | 0 | 0 |
| Testnet | Sapling | 8 | 0 | 0 | 0 |
| Testnet | Blossom | 3 | 0 | 0 | 0 |
| Testnet | Heartwood | 3 | 2 | 0 | 0 |
| Testnet | Canopy | 8 | 1 | 0 | 0 |
| Testnet | NU5 | 0 | 5 | 0 | 0 |

The outcomes are the same on the `upstream` and the `zakura` crypto backend.

## Transaction harness

`crates/hayai-bench/tests/conformance_txs.rs`.

### Script vectors

Each of the 1,046 `zcash_script` cases spends a coin with the scriptPubKey of the case in a v4
transaction with the scriptSig of the case. `Draft::check_input` evaluates the input under the
flags of the case (`RuleEpoch::script_flags`). The result must equal the published result. The
sigop count of the scriptPubKey, read from `PreparedTx::sigops` of a transaction with that
output script, must equal the published count.

`check_input` reports an error as text. The test therefore also evaluates each case with the
interpreter of the crate, and requires the two results to be equal. No signature of the
vectors is valid for the transaction of the test, so a signature check fails, as it does in
the tests of the crate (no sighash).

Outcome: 1,046 of 1,046 cases agree.

### ZIP 143, 243 and 244 transactions

`draft` runs on each vector transaction whose spent coins the vector holds: every ZIP 244
vector, and the ZIP 143 and ZIP 243 vectors with no transparent input or with one signed
input. The vectors are generated transactions that test digests, so a rejection is a recorded
outcome and not a defect. `tests/vectors/expected-txs.json` holds the outcomes.

| Set | Accepted | Unsupported | Rejected | Not run |
|---|---|---|---|---|
| ZIP 143 (v3) | 0 | 4 | 0 | 6 |
| ZIP 243 (v4) | 2 | 0 | 1 | 7 |
| ZIP 244 (v5) | 2 | 0 | 8 | 0 |

The 4 unsupported transactions are v3: full validation has no verification for a version
before Sapling (`docs/consensus-rules.md`, Checkpoints). The 9 rejections are correct: 5
transactions spend more than `MAX_MONEY`, 1 v4 transaction with two JoinSplits has
transparent outputs of more than `MAX_MONEY`, 2 transactions have no output and no shielded
component, and 1 coinbase has Sapling outputs that do not decrypt with the zero outgoing
viewing key (ZIP 213).

The sighash values of the three sets are not tested. hayai computes a sighash only inside
`hayai-prepared` (`SighashContext`, private), and the vector scripts cannot verify a signature,
so `check_input` cannot show the value. `tests/vectors/tx-sighash-inputs.json` holds the
inputs and the published values for that test (bd hayai-xya).

## Differential fuzzer

`crates/hayai-fuzz` runs hayai and the Zakura library code on the same block in one process
and compares the two verdicts. A case is a seed block, a list of mutations and a chain
context. The seeds are the generated fixture blocks of `hayai-bench` (real signatures, real
Orchard and Ironwood proofs) and generated coinbase-only blocks at a chosen height of Mainnet
or Testnet. `docs/fuzz-findings.md` has the runs and the findings.

Entry points:

- `cargo test -p hayai-fuzz --release`: the smoke run (fixed seed, 400 cases for each class,
  about 10 s on 32 threads) and the cases with a known verdict. `cargo test --workspace` runs it.
  The CI of each push does not build the crate; the full workflow runs it.
- `cargo run --release -p hayai-fuzz -- --seed N --seconds S [--class NAME]`: a long run on
  all cores. Case files go to `target/fuzz/findings/`. `--class NAME --case-seed N` makes one
  case again. `--replay FILE` runs the recipe of a case file.

A case seed and a class name give the case. The fuzzer has its own random generator
(SplitMix64), so a seed gives the same case with every version of every dependency. The
run loop is a plain loop on the rayon pool. It has no libFuzzer target: the valid seeds have
proofs and signatures, so coverage feedback on bytes does not reach the rules behind them,
and the structured mutations do.

### Comparison

| Outcome | Meaning |
|---|---|
| both accept, both reject | Agreement. The rule class of each reject is counted. |
| both reject, other rule class | Not a finding. A block can break two rules, and the two implementations order their rules differently. |
| hayai accepts, reference rejects | Finding. |
| hayai rejects, reference accepts | Finding, except when hayai rejects with a rule that the oracle does not have (anchor, chain value pool, history tree). |
| a panic | Finding. |
| known difference | Counted, not written. `src/known.rs` removes the cause from the block and checks that the two implementations then agree. |

### Oracle

The reference verdict follows `SemanticBlockVerifier::call` and the transaction `Verifier::call`
of zakura-consensus 10.0.0 in the same order, with the context of the case in place of the
state service.

zakura-consensus and zakura-state do not link into the process: their `rocksdb` 0.24 and the
`rocksdb` 0.25 of `hayai-coins` both link the native library `rocksdb`, and cargo allows one.
`src/reference/zakura_consensus/` is a copy of `transaction/check.rs`, `block/check.rs`,
`block/subsidy.rs`, the error types and one function of zakura-state, from the Zakura source
tree at commit 1377915. The copy compiles against the published zakura-chain 9.0.0,
zakura-header-chain 4.0.0 and zakura-script 4.0.0.

Kind of code: **L** linked reference crate, **C** copied reference file, **M** model written for
the fuzzer after the rule of zakura-state, **none** no rule in the oracle.

| Rule class | Rules (plan section 1) | Code | Kind |
|---|---|---|---|
| Parse | block and transaction encoding, B4 block size, T8, T9 | `zakura_chain` `Block::zcash_deserialize` | L |
| Header encoding | H1 version | `zakura_header_chain::validate_encoding_version_hash` | L |
| Header context | H4, H6, H7, H9, H10 on generated chains of times and `nBits` | `validate_compact_target`, `AdjustedDifficulty`, `validate_contextual_difficulty_and_time` | L |
| Proof of work | H2, H3, H5 on changed headers of 2 published Mainnet vectors | `difficulty_is_valid`, `equihash_solution_is_valid` | C over L |
| Merkle root | B2, B3, B5 | `merkle_root_validity` | C |
| Coinbase form | B1, T12, H13 (height on the parent) | `coinbase_is_first`, `Block::coinbase_height`; parent and height | C, L, M |
| Coinbase terms | B8, B10, B11, B12, B13, B14, B18, B19 | `block_subsidy`, `subsidy_is_valid`, `miner_fees_are_valid`, `coinbase_outputs_are_decryptable` | L, C |
| Transaction structure | T1, T3 to T7, T11, T16, T17 | `transaction_check::*`, the version rules of `transaction.rs` | C |
| Expiry and lock time | T13, T18 | `coinbase_expiry_height`, `non_coinbase_expiry_height`, `lock_time_has_passed` | C |
| Transparent inputs | B22 | inputs exist, no double spend, order in the block | M |
| Coinbase spends | B23, B24 | `transparent_coinbase_spend` of zakura-state | C |
| Values | B25, T26 | `Transaction::value_balance`, `remaining_transaction_value` | L |
| Scripts | T19 | `zakura_script::CachedFfiTransaction::is_valid` | L |
| Signature operations | B6 | `Sigops::sigops`, `p2sh_sigops`, the limit of `block.rs` | L |
| Orchard and Ironwood proofs and signatures | T21, T22 | `zakura_orchard` `BatchValidator` with the key of the upgrade, sighash of zakura-chain | L |
| Nullifiers | B26 (Orchard, Ironwood) | no nullifier two times in the block or in the chain of the case | M |
| Header commitments | H12 from NU5 | parse of the field, authorizing data root and hash of the two roots: zakura-chain. The history root is a value of the case. | L, M |
| Sapling and Sprout proofs and signatures | T20, T23 | none: a block with such a part is "not covered" | none |
| Anchors | B27, B28 | none | none |
| Chain value pools | B21 | none | none |
| History tree append | B29 | none | none |
| ZIP 218 limits, NU7 rules | B7, B15 to B17, T2, H8 | none: the cases stay below the block before NU7. `tests/conformance_nu7.rs` of hayai-bench (feature `baselines`) compares the NU7 schedule, the NSM values, the limits and the difficulty with `zakura-chain` and `zakura-header-chain` | none |
| Checkpoints, finality, clock rule | H11, H15, H16 | none | none |

The block cases run hayai with `HeaderPolicy::GeneratedBlocks`: the generated headers have no
proof of work, so the contextual header rules do not run in them. The classes `header-context`
and `pow` compare the header rules on headers alone.

On the `zakura` backend, hayai and the oracle use the same `zakura-*` cryptography crates. The
proof and signature rows then compare the code of hayai around the verifier, not two
verifiers. On the default backend they compare upstream `orchard` with `zakura-orchard`.

### Mutation classes

| Class | Mutations |
|---|---|
| `header` | version, time, `nBits`, nonce, parent hash, solution length and bits |
| `structure` | swap, copy and remove transactions; copies that keep the merkle root; stated count and its encoding; truncation; bytes after the block |
| `coinbase` | output values (1 zatoshi more or less), scripts, removed, copied and added outputs, required outputs, height in other encodings, script length 1, 2, 100, 101, lock time, sequence, expiry, more inputs, version, group and branch |
| `height` | a coinbase-only block at or near each upgrade height, halving and funding stream limit of Mainnet and Testnet, with 0 or 1 coinbase change |
| `txfields` | a spend of an `OP_1` coin with lock time, sequence, expiry, version, branch and output values at their limits; fields of the seed transactions |
| `script` | generated pairs of locking and unlocking scripts: random opcodes, pay to script hash, `OP_CHECKLOCKTIMEVERIFY`, signatures and keys in valid and broken encodings, the size limits of the interpreter, encodings of zero |
| `spend` | missing coin, coinbase maturity at 99, 100, 101 blocks, double spend in a transaction and in the block, parent and child in the block in both orders, changed coins of the context |
| `shielded` | flags (256 values), value balance, anchor, copied nullifier, nullifier in the chain, proof length, bit flips in actions, proofs and signatures |
| `limits` | 20,000 signature operations and 2,000,000 bytes, and the values around them |
| `commitments` | bits of the merkle root and of the commitments field; changed authorizing data with and without the header correction |
| `bytes` | bit flips, set, insert and delete on the bytes of one transaction (the header then commits to the new body) and on the bytes of the block |
| `header-context` | a header on a generated chain: time around the median-time-past and its limit, `nBits` at and near the expected value, gaps for the Testnet minimum difficulty rule |
| `pow` | bit flips in the headers of the Mainnet blocks 1 and 1,687,106 |

After the mutations, the header gets the merkle root and the commitments of the new body,
unless the class tests these fields. The reference code computes the two values. When the
reference does not parse the block, the code of hayai computes them, so a block that only
hayai parses reaches the rules of hayai.

## Defects found

None. No vector that is published as valid is rejected by hayai at any stage.

Limits of this statement:

- 32 block vectors stop at missing context. The rules after that point did not run on them.
- 47 valid blocks pass the checkpoint path only: their scripts, proofs, signatures and
  coinbase rules did not run.
- A transaction with transparent inputs from outside the set is not prepared, so its Sapling
  and JoinSplit proofs are not verified. In the 32 context-free blocks, the `transactions`
  stage prepares 64 transactions and does not prepare 165: 110 without their coins, and 55 of
  a version before Sapling.
- No vector holds a JoinSplit block whose whole context is in the set. The Sprout state rules
  (anchors, interstitial treestates, nullifiers, pool) run on generated blocks
  (`hayai-bench/tests/sprout.rs`).
- Mainnet 202 `-bad` has a coinbase height that is not in the canonical encoding. hayai stops
  at the merkle root, because the changed coinbase has another transaction id. The coinbase
  height rule itself (`ContextError::CoinbaseHeight`) did not run on the vector.
