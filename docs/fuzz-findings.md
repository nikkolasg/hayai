# Differential fuzzer: runs and findings

Fuzzer: `crates/hayai-fuzz`, in-process tier. Oracle and mutation classes:
`docs/conformance.md`, Differential fuzzer. Date of the runs: 2026-10-04. Machine: 32 threads.
Backend: default (`upstream`).

## Result

| Classification | Count |
|---|---|
| 1. hayai defect | 0 |
| 2. Policy difference, not consensus | 1 (K1) |
| 3. Oracle gap or harness error | 4 (K2, G1, G2, G3) |

No case file is open: no run ended with a finding that a known difference does not explain.

## Runs

A case count is the number of blocks (or headers) that both implementations checked.

### Run 1: seed 20261004, all classes, 200 s for each class

Tree: the workspace at 19:15 on 2026-10-04. Wall time: 2,606 s (43 min).

| Class | Cases | Seconds | Both accept | Both reject | No oracle | Known difference | Findings |
|---|---|---|---|---|---|---|---|
| `bytes` | 90,112 | 201 | 11,249 | 76,384 | 2 | 2,477 | 0 |
| `coinbase` | 158,720 | 200 | 28,787 | 129,933 | 0 | 0 | 0 |
| `commitments` | 88,064 | 201 | 8,637 | 79,427 | 0 | 0 | 0 |
| `header` | 51,712 | 202 | 25,120 | 13,396 | 4,792 | 8,404 | 0 |
| `header-context` | 164,020,224 | 200 | 65,962,583 | 98,057,641 | 0 | 0 | 0 |
| `height` | 42,597,376 | 200 | 25,468,774 | 17,128,602 | 0 | 0 | 0 |
| `limits` | 69,632 | 201 | 39,090 | 30,542 | 0 | 0 | 0 |
| `pow` | 55,463,936 | 200 | 5,937,031 | 49,526,905 | 0 | 0 | 0 |
| `script` | 121,856 | 200 | 13,824 | 108,032 | 0 | 0 | 0 |
| `shielded` | 84,992 | 201 | 888 | 83,776 | 0 | 328 | 0 |
| `spend` | 187,904 | 200 | 19,576 | 168,328 | 0 | 0 | 0 |
| `structure` | 139,264 | 200 | 14,287 | 113,310 | 0 | 11,667 | 0 |
| `txfields` | 183,808 | 200 | 58,824 | 124,984 | 0 | 0 | 0 |

"Both reject" includes the rejects with two different rule classes (for example 55,128 of
the `coinbase` cases). "No oracle" in `header`: hayai rejects a header whose `nBits` encode no
target at the history tree append, and the oracle has no rule for that append.

### Run 3: seed 20261006, 150 s for each class, on the tree at 21:30

The tree has the changes of the consensus crates of that day (header version with the high
bit, `ContextError::DuplicateTxid`, `PrepareError::SpentCoins`, `rules_at` at the Testnet NU7
height). The owner stopped the run after 3 classes.

| Class | Cases | Seconds | Both accept | Both reject | No oracle | Known difference | Findings |
|---|---|---|---|---|---|---|---|
| `header` | 37,376 | 150 | 18,163 | 9,562 | 3,492 | 6,159 | 0 |
| `structure` | 84,480 | 151 | 8,584 | 68,815 | 0 | 7,081 | 0 |
| `coinbase` | 103,936 | 150 | 18,845 | 85,091 | 0 | 0 | 0 |

Run 2 (seed 20261005) was stopped before it wrote a report. It gives no number.

### Smoke run

`cargo test -p hayai-fuzz --release`: seed `0x6861796169000c03`, 400 cases for each of the 13
classes, on both backends, on the current tree: no finding. Test time: 9 s on an idle machine.

## Known differences

### K1. Bytes after the block (classification 2)

- Case: seed fixture `Transparent`, raw operation `Append("00")`. Test:
  `bytes_after_the_block_are_a_known_difference`. First seen: class `structure`, case seed
  `0x6a40ceef1e4e7f7d`.
- hayai: reject, `ParseError::Trailing` (`hayai-wire/src/lib.rs`, `RawBlock::parse`).
- Reference: accept. The block message decoder of Zakura reads the block and ignores the
  bytes after it (`zakura-network/src/protocol/external/codec.rs:534`).
- Rule: none in the consensus rules. The block and its hash are the same with and without the
  bytes. The difference is in the wire policy, and hayai is the stricter side.
- Effect in hayaid: a block message with bytes after the block is a malformed body
  (`hayaid/src/sync.rs`, `parse_body`, `BodyError::Malformed`). The header does not become
  invalid. A node that forwards a block writes it again from the parsed form, so the bytes
  do not spread.
- Count: 14,468 cases in run 1, 7,081 in run 3. For each case the fuzzer removes the bytes
  after the block and checks that the two verdicts then agree.

### K2. Header version in a block case (classification 3, harness)

- Case: seed fixture `Transparent`, operation `HeaderVersion(3)` or `HeaderVersion(0x80000004)`.
  Test: `the_header_version_is_a_known_difference_of_the_block_cases`. First seen: class
  `header`, case seeds `0x9372e55a8ee54e70` (version 2) and `0xd0743df79ba49575` (version
  `0x80000002`).
- hayai: accept. Reference: reject at the parse of the header (version below 4, or the high
  bit set).
- Cause: the block cases run hayai with `HeaderPolicy::GeneratedBlocks`, which runs no header
  rule, because the generated headers have no proof of work. The rule of hayai is in
  `hayai_consensus::header::check_contextual`.
- The class `header-context` compares that rule: 164,020,224 cases in run 1, with versions 0,
  3, 4, 5, `0x7fffffff`, `0x80000000`, `0x80000004` and `0xffffffff`, no finding.
- Count: 8,408 cases in run 1, 6,159 in run 3. For each case the fuzzer sets the version to 4
  and checks that the two verdicts then agree.

## Harness errors found and corrected during development

These cases were findings of the first runs of 64 and 2,000 cases for each class. Each one
was an error of the harness. They are not in the runs above.

### G1. Deferred pool of the context (classification 3)

- Case: class `height`, case seed `0xbdf6b6797f24f61d`: the coinbase-only block at the Mainnet
  height 3,146,400 (NU6.1 activation, lockbox disbursement).
- hayai: reject, `CoinbaseError::NegativeDeferredPool`. Reference: accept.
- Cause: the context had a deferred pool of 0. The rule is a chain value pool rule, and the
  oracle has none (zakura-state).
- Correction: the context has a deferred pool of 100,000 ZEC. The error maps to the rule
  class `ValuePool`, which has no oracle.

### G2. Context above the money limit (classification 3)

- Case: class `txfields`, case seed `0xc0c3527ecaa24e70`: a spend of a coin of 21,000,000 ZEC.
- hayai: reject, `ContextError::ValueOverflow` (the chain value pools after the block hold
  more than the money limit). Reference: accept.
- Cause: the coins of the context held more than the money limit, which no chain state can.
- Correction: a coin of a context holds at most the money limit minus 400,000 ZEC.

### G3. Merkle root of a block without transactions (classification 3)

- Case: class `structure`, operation `StatedCount(0)`.
- `zakura_chain::block::merkle::Root` panics on an empty list (`merkle.rs:226`). The node does
  not reach that code: it rejects a block without a coinbase height first. The helper of
  the fuzzer that computes the root for the header correction called it.
- Correction: the helper returns no root for a block without transactions.

## Limits of the result

- Every finding count above is for the in-process oracle. Rules without an oracle: Sapling
  and Sprout proofs and signatures, anchors, chain value pools, the history tree append, NU7,
  checkpoints, the clock rule (`docs/conformance.md`).
- The shielded cases with valid proofs are the seed transactions. A mutation of a signed
  field makes the proof or a signature invalid, so the shielded rules after the proof are
  compared only on blocks that both implementations reject.
- The model rules of the oracle (transparent spends in a block, nullifier sets, parent and
  height) are code of the fuzzer, not of the reference.
- Run 3 covers 3 of the 13 classes on the current tree. The other 10 classes ran on the
  current tree only in the smoke run (400 cases each).
