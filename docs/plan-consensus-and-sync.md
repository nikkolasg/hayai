# hayai: plan for consensus completeness (A), sync and node completeness (B), conformance testing (C)

Read-only work. No file was changed. No cargo command was run.

## 0. Findings that change the brief

1. **Upstream verifies Ironwood.** `orchard` 0.15.5 has `BundleVersion::ironwood_v3()`, `ValuePool::Ironwood`, `OrchardCircuitVersion::PostNu6_3`, `Flags::CROSS_ADDRESS_DISABLED`, `note_encryption::IronwoodDomain`. `zcash_primitives` 0.30.1 has `TransactionData::ironwood_bundle()`, `sighash_v6.rs`, the Ironwood txid digest. `zcash_history` 0.5.0 has `V3` / `NodeDataV3` with `start_ironwood_root`, `end_ironwood_root`, `ironwood_tx`. hayai already builds the `PostNu6_3` key (`crates/hayai-prepared/src/shielded.rs:43-47`). The `Unsupported("ironwood bundle")` in `prepare.rs` is a hayai gap, not a backend gap.
2. **NU6.3 is active on both networks.** `zcash_protocol` 0.10.5 `consensus.rs:502,535`: Mainnet 3,428,143, Testnet 4,134,000. hayai at the Mainnet tip (3,505,115) stops at the first v6 Ironwood block. Ironwood is the first blocker, before sync.
3. **NU7 exists only in the Zakura forks.** `zakura-protocol` 2.2.0: `BranchId::Nu7 = 0x7719_0ad9`, Testnet activation 4,465,026, Mainnet `None`. Upstream 0.10.5 and 0.10.6: `Nu7` is behind `cfg(zcash_unstable = "nu7")`, branch `0xffff_ffff`, no heights. `zakura-primitives` 2.2.0 has no `zip233_amount`: its NU7 v6 format equals the NU6.3 v6 format.
4. **Newer upstream releases exist but are not in the local registry.** `zebra-chain` 14.0.0 pins `orchard 0.16.0`, `sapling-crypto 0.9`, `zcash_primitives =0.31.0-pre.0`, `zcash_protocol =0.11.0-pre.0`. Their sources are absent locally, so their NU7 content is unknown.
5. **Reference versions.** `zebra-consensus` 16.0.0 and `zebra-state` 14.0.0 are not in `~/.cargo/registry`. Present: `zebra-consensus` 11.0.0, `zebra-state` 10.1.0, `zebra-chain` 11.1.0 / 13.0.1 / 14.0.0, and the checkout (02f9648: zebra-consensus 9.0.0). Zebra line numbers below are from the checkout. Zakura line numbers are from HEAD 1377915 (2026-10-02). An implementer must confirm against 16.0.0 if exact parity with that release matters.
6. **Rules that hayai applies wrongly today (not only absent):**
   - Coinbase value uses `paid > allowed` at every height (`hayai-state/src/check.rs:623`). From NU6 the rule is equality (ZIP 236).
   - "Some source of funds" counts Orchard actions without the `enableSpends` flag (`prepare.rs:306-311`). The spec counts actions only when the flag is 1.
   - `check_pow` (`hayai-wire/src/header.rs:214`) does not compare the target with `PoWLimit` on Mainnet and Testnet.
   - `NetParams::max_time_enforced` returns true for every Testnet height. The rule starts at Testnet height 653,606.
   - The header time rules run only in the relay path (`hayaid/src/headers.rs:245`), not in `validate_block`. Restart replay and any future sync path skip them.
   - The driver always uses `BlockLimits::PRE_NU7`.
   - Value pools hold Sapling and Orchard only.
   - The coinbase-spend rule (no transparent outputs) applies on Regtest too. Zakura waives it there (`zakura-chain/src/transaction.rs:557`, `should_allow_unshielded_coinbase_spends`).

Path abbreviations: `ZC` = `../zakura-src/zakura/crates/zakura-consensus/src`, `ZS` = `.../zakura-state/src/service`, `ZCH` = `.../zakura-chain/src`, `ZH` = `.../zakura-header-chain/src`, `ZB` = `../zebra`.

---

## 1. Rule checklist (deliverable A.8)

Status: P = present, PART = partial, ABS = absent, WRONG = applied with a different result, PARSER = enforced by the upstream parser (needs a test that proves it).

### 1.1 Header

| # | Rule | Reference | hayai | Item |
|---|---|---|---|---|
| H1 | Header encoding, version >= 4 | `ZH/validation/context_free/encoding.rs:26` | P | — |
| H2 | Solution length of the network (no (48,5) on Mainnet) | `ZC/block/check.rs:158` | P | — |
| H3 | Equihash solution valid | same; `ZB/zebra-consensus/src/block/check.rs:143` | P (Regtest waiver as Zakura) | — |
| H4 | `nBits` decodes; target <= PoWLimit | `ZC/block/check.rs:105`; `ZH/.../target.rs:25`; `ZB .../block/check.rs:76` | ABS on Mainnet/Testnet | A1 |
| H5 | Difficulty filter: hash <= target | `ZC/block/check.rs:127` | P | — |
| H6 | `nBits` = ThresholdBits(height) (§7.7.3) | `ZH/validation/contextual/adjusted_difficulty.rs:204-260`; `ZS/check.rs:396`; `ZB/zebra-state/src/service/check.rs:267` | ABS (bits trusted) | A1 |
| H7 | Testnet minimum difficulty: height >= 299,188 and gap > 6 x spacing gives PoWLimit (ZIP 205/208) | `ZCH/parameters/network_upgrade.rs` (`minimum_difficulty_spacing_for_height`, `is_testnet_min_difficulty_block`) | ABS | A1 |
| H8 | NU7: spacing 25 s, averaging window 102, Testnet gap 18 x spacing (ZIP 218) | `network_upgrade.rs:257-336, 584` | ABS | A7 |
| H9 | time > median-time-past (11 blocks) | `ZH/.../validate.rs` (`TimeTooEarly`) | PART: relay path only | A1 |
| H10 | time <= MTP + 90 min; Mainnet from height 2; Testnet from 653,606 | `validate.rs` (`TimeTooLate`), `ZCH/parameters/network.rs:246` | WRONG on Testnet below 653,606; relay path only | A1 |
| H11 | time <= local clock + 2 h (not deterministic) | `ZC/block/check.rs:404` | P (relay path) | A1 keeps it out of replay |
| H12 | Header field at offset 68: Sapling root / history root / `hashBlockCommitments` | `ZS/check.rs:265` | P to NU6.2; ABS from NU6.3; skipped when the parent history is unknown | A6, B8 |
| H13 | Height = parent height + 1; coinbase height | `ZS/check.rs:371` | P | — |
| H14 | Genesis hash of the network | — | P Mainnet, Regtest; ABS Testnet | A0 |
| H15 | Below the last checkpoint: hash equals the checkpoint | `ZC/checkpoint.rs` | ABS | B7 |
| H16 | Not below the finalized tip | `ZS/check.rs:355` | P by design | — |

### 1.2 Block

| # | Rule | Reference | hayai | Item |
|---|---|---|---|---|
| B1 | At least one transaction; coinbase first and only first | `ZC/block/check.rs:68` | P | — |
| B2 | Merkle root | `ZC/block/check.rs:523` | P | — |
| B3 | No duplicate txid (CVE-2012-2459) | same | P | — |
| B4 | Block size <= 2,000,000 | parser | P | — |
| B5 | v5+ `nConsensusBranchId` equals the block's branch | `ZCH/block.rs:158`; `ZC/transaction/check.rs:930` | P | — |
| B6 | Sigops <= 20,000 (legacy + P2SH) | `ZC/block.rs:303` and the block verifier | P | — |
| B7 | ZIP 218 limits from NU7: Orchard <= 330, Ironwood <= 330, Sapling I/O <= 300, JoinSplits <= 0, total cost <= 330 | `ZC/block/check.rs:450` | PART: Orchard and Sapling fields exist; Ironwood, global budget absent; driver never selects NU7 | A7 |
| B8 | Block subsidy schedule (slow start, Blossom, halvings) | `ZCH/parameters/network/subsidy.rs` (`halving`, `halving_block_subsidy`) | PART: Mainnet only, no NU7 era | A2 |
| B9 | Founders' reward output, heights 1 .. first halving, pre-Canopy | `ZC/block/check.rs:178-232` | ABS | A2 |
| B10 | Funding stream outputs, Canopy onward (ZIP 214, 1014, 1015; ZIP 2008 address rotation at NU7) | `ZC/block/check.rs:233-320`; constants in `ZCH/parameters/network/subsidy/constants/{mainnet,testnet}.rs` | ABS | A2 |
| B11 | NU6.1 activation block: ten equal lockbox disbursement outputs (ZIP 271) | `ZC/block/check.rs:268-290` | ABS | A2 |
| B12 | Pre-NU6: coinbase output <= subsidy + fees | `ZC/block/check.rs:326` | P | — |
| B13 | NU6 onward: coinbase output + deferred = subsidy + fees (ZIP 236) | same, lines 388-396 | WRONG (`>` only) | A2 |
| B14 | Coinbase output includes `-valueBalanceIronwood` | same, lines 341-366 | ABS | A6 |
| B15 | NU7: miner share = fees - floor(6 x fees / 10) | `ZCH/parameters/network/subsidy/fees.rs:21` | ABS | A7 |
| B16 | ZIP 234: subsidy = halving subsidy + ceil(NSM(parent) x 1375 / 10^10) from the reissuance height | `subsidy.rs` (`block_subsidy`, `reissuance_bonus`, `nsm_reissuance_height`) | ABS | A7 |
| B17 | NU7: NSM value balance >= 0 after the block | `ZS/check.rs:65` | ABS | A7 |
| B18 | ZIP 213: coinbase Sapling/Orchard/Ironwood outputs decrypt with the zero OVK; lead byte 0x02 from Canopy | `ZC/transaction/check.rs:503`; `ZCH/primitives/zcash_note_encryption.rs:12` | ABS | A3 |
| B19 | NU6.3: coinbase has no Orchard bundle | `ZC/transaction/check.rs:367` | ABS | A6 |
| B20 | Pre-Heartwood: coinbase has no shielded outputs | spec §7.1.2 (Zebra and Zakura skip it: checkpoint) | ABS | A3 |
| B21 | Chain value pools >= 0: transparent, Sprout, Sapling, Orchard, Ironwood, deferred; total <= MAX_MONEY | `ZCH/value_balance.rs:360` | PART: Sapling, Orchard | A2, A5, A6 |
| B22 | Transparent inputs exist, unspent, in block order, no double spend | `ZS/check/utxo.rs:45,133` | P | — |
| B23 | Coinbase maturity 100 | `utxo.rs:200` | P | — |
| B24 | Spend of a coinbase output has no transparent outputs (waived on Regtest in Zakura) | `utxo.rs:200`; `ZCH/transaction.rs:552` | P; Regtest differs | A0 |
| B25 | Remaining transparent value >= 0 | `utxo.rs:241` | P (fee) | — |
| B26 | Nullifiers unique in block and chain: Sprout, Sapling, Orchard, Ironwood | `ZS/check/nullifier.rs:37,148,237,254` | PART: Sapling, Orchard (store has four pools) | A5, A6 |
| B27 | Sapling, Orchard, Ironwood anchors are final treestates of earlier blocks | `ZS/check/anchors.rs:24` | PART: Sapling, Orchard | A6 |
| B28 | Sprout anchors: an earlier final treestate or an interstitial treestate of the same transaction | `anchors.rs:230` | ABS | A5 |
| B29 | History tree append; new tree at each activation; V3 leaf from NU6.3 | `ZCH/history_tree.rs:142,241` | P V1, V2; ABS V3 | A6 |

### 1.3 Transaction

| # | Rule | Reference | hayai | Item |
|---|---|---|---|---|
| T1 | Version allowed per epoch: v4 Sapling..NU6.3; v5 from NU5; v6 from NU6.3 | `ZC/transaction.rs:1021,1167,1235` | P; v1–v3 `Unsupported` | A5 |
| T2 | NU7: version is 5 or 6 (ZIP 2003) | `ZC/transaction.rs:1032` | ABS | A7 |
| T3 | Some input: `tx_in > 0` or Sapling spends or (Orchard actions and enableSpends) or (Ironwood actions and enableSpends) or JoinSplit (v4) | `ZC/transaction/check.rs:131` | WRONG: ignores the flags | A0 |
| T4 | Some output, same form with enableOutputs | same | WRONG: ignores the flags | A0 |
| T5 | Orchard actions need one of the two flags | `check.rs:150` | P | — |
| T6 | Ironwood actions need one of the two flags | `check.rs:165` | ABS | A6 |
| T7 | Orchard `enableCrossAddress` is 0 from NU6.3 | `check.rs:179` | PARSER (`Flags::from_byte` under `orchard_v3`) | A6 test |
| T8 | Sapling `cv`, `epk`, `rk` are not of small order | `check.rs:209` | PARSER | C2 test |
| T9 | Orchard/Ironwood proof length is canonical | `check.rs:218` | PARSER for Ironwood; confirm for v5 Orchard | A6 test |
| T10 | Temporary soft fork: no Orchard bundle from Mainnet 3,363,426 (Testnet 4,048,500) until NU6.2 | `ZC/transaction.rs:478`; `ZCH/parameters/network.rs:26,31,373` | ABS | A0 |
| T11 | Coinbase: no JoinSplit, no Sapling spend, Orchard enableSpends = 0, Ironwood enableSpends = 0 | `check.rs:251` | PART: Sapling, Orchard | A5, A6 |
| T12 | Non-coinbase input has a non-null prevout; coinbase scriptSig 2..=100 bytes; height push | `ZC/block/check.rs:68`, parser | P | — |
| T13 | Expiry < 500,000,000; non-coinbase not mined after expiry; coinbase expiry = height from NU5 | `check.rs:542,592` | P | — |
| T14 | JoinSplit: one of `vpub_old`, `vpub_new` is zero | `check.rs:279` | ABS | A5 |
| T15 | Canopy onward: `vpub_old` = 0 (ZIP 211) | `check.rs:304` | ABS | A5 |
| T16 | NU6.3 onward: `valueBalanceOrchard` >= 0 | `check.rs:338` | ABS | A6 |
| T17 | No duplicate outpoint or nullifier in a transaction (all four pools) | `check.rs:403` | PART: transparent, Sapling, Orchard | A5, A6 |
| T18 | Lock time (`IsFinalTx` with block height and time) | `check.rs:72` | P | — |
| T19 | Transparent scripts, flags P2SH and CLTV, sighash ZIP 143/243/244 and v6 | `ZC/script.rs` | P (v6 sighash through upstream; add a vector test) | C1 |
| T20 | Sapling spend and output proofs, spendAuthSig, bindingSig | `ZC/transaction.rs:1348`; `ZC/primitives/sapling.rs` | PART: needs parameters on disk | A4 |
| T21 | Orchard proof and signatures under the circuit of the block epoch (a v5 bundle at NU6.3 uses the NU6.3 key) | `ZC/transaction.rs:1438`; `ZC/primitives/halo2.rs:405` | P (grouped by `bundle_version().circuit_version()`); add a test for v5 at NU6.3 | A6 test |
| T22 | Ironwood proof and signatures | `ZC/transaction.rs:1219-1228` | ABS | A6 |
| T23 | Sprout: Groth16 JoinSplit proof, Ed25519 `joinSplitSig`, `h_sig` | `ZC/transaction.rs:1284`; `ZC/primitives/groth16.rs` | ABS | A5 |
| T24 | Sprout: BCTV14 (PHGR13) proofs before Sapling | not in Zebra or Zakura | ABS | A5b (optional) |
| T25 | v4 without Sapling components: `valueBalanceSapling` = 0 | spec §7.1.2 | P | — |
| T26 | Value ranges, sums <= MAX_MONEY, including `vpub` and Ironwood balance | `ZCH/transaction.rs` value balance | PART | A5, A6 |
| T27 | Mempool only: no coinbase; coinbase maturity at next height; lock time against next MTP; standard inputs; ZIP 317 | `ZC/transaction.rs:513,585-606`; `check.rs:872` | ABS (see B10) | B10 |

Not consensus and not planned: Zakura's `legacy_chain` check (`ZS/check.rs:434`), the NU6.3 branch-id misbehaviour grace period (`ZC/transaction.rs:94`, a peer-scoring matter, used in B5).

---

## 2. Work package A: design per item

### A0. New crate `hayai-consensus` and the small fixes (M)

- **Why a new crate.** The network parameters are in `hayaid/src/params.rs`. hayai-state receives them through `SubsidyRule`. Difficulty, subsidy, funding streams, checkpoints and limits need one home below hayai-state. Position: after hayai-wire. Dependencies: hayai-crypto, hayai-wire.
- **Modules.** `network.rs` (`Network { Mainnet, Testnet, Regtest(RegtestConfig) }`, genesis hash and time per network, PoW limit, `Upgrade` enum of hayai's own that includes `Nu7`, `activation(Upgrade) -> Option<u32>`, `branch_at(height)`, `spacing_at`, `max_time_start`, `min_difficulty_start`, `allows_unshielded_coinbase_spend`, `orchard_disabled(height)`), `difficulty.rs`, `subsidy.rs`, `funding.rs`, `limits.rs`, `checkpoints.rs`, `nsm.rs`.
- **NU7 on two backends.** `BranchId::Nu7` exists only on the zakura backend. Put the single `cfg` in `hayai-crypto/src/lib.rs`: `pub fn nu7_branch() -> Option<BranchId>` and `pub fn nu7_activation(network_type) -> Option<u32>` (both `None` on upstream). No other crate uses a `cfg` for NU7.
- **Fixes in this item.** T3/T4 (flags) in `hayai-prepared/src/prepare.rs::draft`. T10 (soft fork) in `draft` with a height-derived field on `RuleEpoch` or a contextual check in `check_txs` (contextual is simpler: `RuleEpoch` has no height). B24 Regtest waiver through `CheckConfig`. H14 Testnet genesis (`05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38`, confirm) and removal of the "full mode refuses Testnet" check in `hayaid/src/config.rs:264`.
- **Interface change.** `hayai_state::CheckConfig { limits, subsidy: &dyn SubsidyRule }` becomes `CheckConfig { rules: &hayai_consensus::Rules }`. `hayaid::params::NetParams` becomes a thin wrapper.
- **Tests.** Unit tests per fix. T3: a v5 transaction with Orchard actions, `enableSpends = 0`, no other input fails.

### A1. Difficulty and header time (M)

- **Rule.** Spec §7.7.3 (ThresholdBits, MeanTarget, ActualTimespanBounded, MedianTime), §7.6 (time rules), ZIP 205/208 (Testnet minimum difficulty), ZIP 218 (NU7 window).
- **Constants.** PoWAveragingWindow 17 (102 from NU7), PoWMedianBlockSpan 11, damping 4, max adjust up 16 %, down 32 %, spacing 150 / 75 / 25 s, Testnet rule from height 299,188 with gap strictly greater than 6 x spacing (18 x from NU7).
- **Design.** `hayai_consensus::difficulty::expected_bits(network, height, candidate_time, context: &[(bits, time)]) -> u32`, context newest first, length `min(height, window + 11)`. `median_time_past(context)`. Arithmetic: mean = sum of (target / n) + (sum of (target % n)) / n, as Zakura (`adjusted_difficulty.rs` `mean_target_difficulty`). This avoids the 256-bit overflow of 102 Testnet targets (limit 2^251). Then `mean / AveragingWindowTimespan * bounded_timespan`, capped at PoWLimit. Use `primitive_types::U256` (in the facade). Add `compact_from_target` to `hayai-wire/src/header.rs` beside `expand_target`.
- **`check_header_context(network, header, height, context, now: Option<u32>)`**: H4, H6, H7, H9, H10, and H11 when `now` is given. One function, used by the relay header check (`hayaid/src/headers.rs`, fills `ParentInfo::expected_bits`), by header sync (B1), and by `validate_block` through a new `ChainView::header_context()` (replay passes `now = None`).
- **State.** A `Layer` gains `bits: u32`. `Base` keeps the last 113 `(bits, time)` pairs (replaces `times: VecDeque<u32>`). `state.log` record version 2 stores them.
- **Tests.** (a) Zebra block vectors: for each contiguous run the expected bits of block n equal the header bits (needs 28 headers; use the header fixtures of C2). (b) Differential against `zakura_header_chain::AdjustedDifficulty::expected_difficulty_threshold` on random contexts (C3). (c) Testnet vectors 299,187–299,202 exercise the minimum-difficulty start. (d) The first 28 blocks: PoWLimit for height <= 17.
- **Size.** M.

### A2. Subsidy, funding streams, lockbox, coinbase outputs (L)

- **Rules.** Spec §7.8 (subsidy), §7.9 (founders' reward), §7.10 (funding streams), ZIP 207, 214 (revisions 1–3), 1014, 1015, 271, 1016, 236, 2001, 2008.
- **Data to port** from `ZCH/parameters/network/subsidy/constants/{mainnet,testnet}.rs`: 48 founder addresses per network, ECC/ZF/MG address lists (48 Mainnet, 51 Testnet), FPF addresses, numerators 7/5/8 (Canopy streams), 12 deferred + 8 FPF (NU6 and NU6.1 streams), ranges Mainnet 1,046,400..2,726,400, 2,726,400..3,146,400, 3,146,400..4,406,400; Testnet 1,028,500..2,796,000, 2,976,000..3,396,000, 3,536,500..4,476,000; the NU6.1 disbursement (10 outputs of 7,875 ZEC to `t3ev37Q2uL1sfTsiJQJiWJoFzQpDhmnUwYo` on Mainnet; Testnet address in the Testnet file); first halving Testnet 1,116,000; address change interval = post-Blossom halving interval / 48.
- **Design.** `hayai_consensus::subsidy::{halving, halving_block_subsidy, founders_reward, founders_reward_script}` and `funding::{streams_at(height) -> Vec<(Receiver, amount, Option<script>)>, lockbox_disbursements(height)}`. Decode the Base58Check addresses to scripts once at start (P2SH for `t3`/`t2`, P2PKH for the ZIP 2008 `t1` address). `hayai_consensus::coinbase::check(coinbase outputs, height, fees, nsm_parent) -> Result<DeferredChange, _>`: match each required output by exact `(value, script)` with multiplicity (Zakura `UnmatchedCoinbaseOutputs`), then B12/B13.
- **State.** `ValuePools` gains `transparent`, `sprout`, `deferred` (and `ironwood` in A6, `nsm: i64` in A7). `value_pools_after` updates all and checks each >= 0 and the total <= MAX_MONEY. Transparent pool change = coinbase outputs + outputs − spent inputs. Deferred change = lockbox share − disbursement.
- **Placement.** `hayai-state/src/check.rs`: `check_coinbase_value` is replaced by a call into `hayai_consensus::coinbase::check`. `hayai-template/src/coinbase.rs` takes the required outputs from the same functions, so the template and the validator agree.
- **Tests.** Zebra vectors with real coinbases at each boundary (Mainnet 1, 395/396, 653,600, 1,046,400, 1,180,900, 1,687,106; Testnet 1,028,500, 1,116,000, 1,326,100, 1,842,421). Differential against `zakura_chain::parameters::subsidy::{block_subsidy, funding_stream_values, founders_reward, miner_fee_share}` (public, `zakura-chain` 9.0.0 is already a hayai-bench dependency) for every height that is a boundary plus random heights. NU6, NU6.1 blocks: no local vector; fetch from the shadow node (C2).
- **Size.** L (mostly data and tests).

### A3. ZIP 213 shielded coinbase (S)

- **Rule.** Heartwood onward: every Sapling, Orchard, Ironwood output of a coinbase decrypts with OVK = 32 zero bytes. Canopy onward: lead byte 0x02 (`Zip212Enforcement::On`). Pre-Heartwood: a coinbase has no shielded outputs.
- **Design.** `hayai-prepared/src/coinbase.rs` (new file): `check_shielded_coinbase(tx, branch)` with `sapling_crypto::note_encryption::try_sapling_output_recovery`, `zcash_note_encryption::try_output_recovery_with_ovk` over `OrchardDomain::for_action` / `IronwoodDomain::for_action`. Add `zcash_note_encryption` to the facade. Call it from `draft` when `is_coinbase`. Also B19 and B20 here.
- **Tests.** Zakura's `ORCHARD_NOTE_ENCRYPTION_ZERO_VECTOR` (`zakura-test/src/vectors/orchard_note_encryption.rs`, also in the Zebra checkout). A real shielded coinbase block from the shadow node. A mutated ciphertext fails.
- **Size.** S.

### A4. Sapling parameters and verification (S)

- **Gap.** `SaplingKeys::load` needs the 48 MB + 3.6 MB parameter files. Without them a Sapling bundle is `Unsupported`. Zakura uses `LocalTxProver::bundled()` (`ZC/primitives/sapling.rs:43`).
- **Design.** Verification needs only the two verifying keys (about 1–2 kB each). Extract them once from the hash-checked parameter files, commit them as `hayai-prepared/src/sapling-spend.vk`, `sapling-output.vk`, load with `bellman::groth16::VerifyingKey::read`, and test that they equal the keys of `zcash_proofs::load_parameters` when the files are present. `VerifyingKeys.sapling` becomes non-optional. Remove `sapling_params_dir` as a requirement. Alternative: the `bundled-prover` feature of `zcash_proofs` (50 MB in the binary).
- **Other Sapling gaps.** None found in the contextual rules. Small-order checks and canonical encodings come from the parser (prove with C3). Both backends use `sapling_crypto::BatchValidator` with the same API.
- **Tests.** Mainnet blocks 419,201/419,202, 434,873 (Sapling transactions, in the Zebra vectors). `sapling-treestate-main-0-419-201.txt` for the tree.
- **Size.** S.

### A5. Sprout (L) and A5b BCTV14 (L, optional)

- **What the reference nodes do.** Zebra and Zakura verify below the mandatory checkpoint (Canopy − 1: Mainnet 1,046,399, Testnet 1,028,499) by hash only (`ZCH/parameters/network.rs:271`). They verify Groth16 JoinSplits (v4, from Sapling) with an embedded 1,828-byte key (`ZC/primitives/groth16/sprout-groth16.vk`) and the Ed25519 signature. They have no BCTV14 verifier. They keep the Sprout tree, anchors (with the tree per anchor, for interstitial treestates) and nullifiers for every block, also below the checkpoint, because the state needs them.
- **What a hayai node from genesis must do.** State: always. Proofs: only above the checkpoint. This equals the reference nodes.
- **Design.**
  - Wire: `hayai-wire` already delimits v1–v4 with JoinSplits (`scan.rs`). `check_version` accepts v1–v3 in their epochs (v1/v2 before Overwinter, v3 in Overwinter, v4 from Sapling; the upstream parser needs the right `BranchId`).
  - Tree: `hayai-trees/src/sprout.rs` (new): depth 29, SHA-256 compression function without padding, `incrementalmerkletree::Frontier<_, 29>`. Sprout anchors need the frontier per root, not only the root: `Base` keeps `sprout_trees: Map<root, Arc<SproutFrontier>>` (about 1 kB each; Mainnet has well under a million distinct Sprout roots; confirm the count), persisted in `state.log`.
  - Contextual (`check_txs`): Sprout nullifiers (two per JoinSplit) into `Pool::Sprout`; anchor chain inside a transaction (B28); pool balance; T14, T15.
  - Proofs (`hayai-prepared/src/sprout.rs`, new): `zcash_proofs::sprout::verify_proof` with the embedded key, `h_sig = BLAKE2b-256("ZcashComputehSig", randomSeed || nf1 || nf2 || joinSplitPubKey)`, Ed25519 with ZIP 215 rules from Canopy and the pre-Canopy rules before it (use `ed25519-zebra`; confirm the pre-Canopy acceptance set).
  - NU7: v4 is disallowed (T2), so the ZIP 218 JoinSplit limit of zero needs no counter. Keep a counter anyway for the error message.
- **A5b.** A no-checkpoint full verification of heights below Sapling activation needs a BCTV14 verifier on alt_bn128 (zcashd uses libsnark; a Rust port exists in old librustzcash as `sprout::verify` only for Groth16, so BCTV14 means new code with the `bn` crate and the Sprout verifying key). Recommendation: do not build it; see A9.
- **Tests.** Zebra vectors with JoinSplits: Mainnet 396, 347,499–347,501, 415,000, 419,199–419,202; Testnet 2,259, 141,042, 207,499–207,501. `ZC/primitives/groth16/vectors.rs`. zcash-test-vectors has Sprout note-commitment-tree vectors (not local; fetch).
- **Size.** L.

### A6. Ironwood, NU6.3 (L)

- **Transaction rules.** T6, T7, T9, T11, T16, T17, T22, B14, B19, the Ironwood terms of T3/T4 and of the fee.
- **Design.**
  - `hayai-prepared/src/prepare.rs::draft`: read `tx.ironwood_bundle()`; nullifiers into `Pool::Ironwood`; `commitments.ironwood`; anchor `(Pool::Ironwood, bundle.anchor())`; `ironwood_actions` counter; fee adds the Ironwood value balance.
  - `hayai-prepared/src/shielded.rs`: `ScopedBatch::add` queues the Ironwood bundle in the `PostNu6_3` group with the same sighash (Zakura `transaction.rs:1219-1228`). `verify_orchard` takes a closure that selects the bundle.
  - Tree: the Ironwood tree uses the Orchard node type and MerkleCRH^Orchard (`ZCH/ironwood.rs` re-exports `orchard::tree`). `hayai-trees`: `pub type IronwoodFrontier = OrchardFrontier`. `Layer`, `Base`, `Anchors`, `ValuePools`, `BaseState`, `StateRecord` gain the Ironwood field. `append_trees` appends three trees (run them with `rayon::join`).
  - History: `hayai-state/src/history.rs`: `TreeVersion::V3` with `NodeDataV3`; `HistoryLeaf` gains `ironwood_root`, `ironwood_tx`. At the NU6.3 activation block a new V3 tree starts.
  - Coinbase: Ironwood value balance in `coinbase_value_out`; `PrebuiltBody::commit` refuses a coinbase with an Ironwood bundle (as it does for Sapling/Orchard).
  - Wire `Anchors::get`, `Base::insert_anchor`, `has_anchor`, shadow `trust_anchors`, `compare_roots` (Zakura `z_gettreestate` returns the Ironwood tree; confirm the field name).
- **Tests.** `zcash_history` V3 test vectors (in the crate, `test_vectors.rs`); copy them as `zip_0221_v3.rs` beside the V1/V2 files in `hayai-state/tests/vectors`. Fixture generator: a v6 bundle with `BundleVersion::ironwood_v3()` in `hayai-bench/src/fixtures.rs`. Real blocks: Mainnet 3,428,142–3,428,144 and Testnet 4,133,999–4,134,001 from the shadow node (C2). Shadow run across the Mainnet tip is the acceptance test.
- **Size.** L. It is the first item to do: Mainnet shadow mode stops without it.

### A7. NU7 (M, zakura backend only today)

Rules in Zakura's code:
1. ZIP 218 spacing 25 s, window 102, Testnet gap multiplier 18 (A1 parameters).
2. ZIP 218 shielded limits including Ironwood and the global budget of 330 (`BlockLimits` gains `ironwood_actions`, `sprout_joinsplits`, `shielded_budget`; `hayai_consensus::limits::at(height)`; the driver stops using `PRE_NU7`).
3. Halving index by elapsed block seconds over three eras (`subsidy.rs` `halving`), subsidy `12.5 ZEC x spacing / 150 / 2^halvings`.
4. Funding stream end height stretched: `end' = nu7 + 3 x (end − nu7)` (`nu7_adjusted_funding_stream_height`); address period formula with ratio 3 (`funding_stream_address_period`); ZIP 2008 FPF address rotation.
5. ZIP 2003: v4 disallowed.
6. Fee split: 60 % of the aggregate fees to the NSM, rounded once per block in the miner's favour.
7. NSM balance: seed at NU7 − 1 = scheduled issuance − issued supply, checked against the constant (Mainnet 36,858,445,520, Testnet 55,768,414,957 zatoshis); per block `nsm += halving_subsidy − (change of the six pools)`; must stay >= 0.
8. ZIP 234 bonus from the reissuance height (derived: first block after the third halving and after NU7 where the reserve condition holds; `nsm_reissuance_crossing_height`).
9. Protocol version 170,180 (Testnet) / 170,190 (Mainnet) (`zakura-network/src/protocol/external/types.rs:130`).
10. Coinbase maturity stays 100 and the expiry threshold stays 500,000,000 in Zakura's consensus code (confirm against the ZIP 218 text: the brief says they scale; Zakura does not scale them in consensus; the default expiry delta of wallets and the template is policy).
- **Design.** All in `hayai-consensus` (`nsm.rs`, `subsidy.rs`, `limits.rs`), port of the Zakura functions with differential tests against `zakura-chain`. `ValuePools.nsm: i64`. The template (`hayai-template/src/coinbase.rs`) uses `miner_fee_share`.
- **Dependencies.** A0, A1, A2, A6.

### A9. Checkpoints (M, with B7)

- **Reference.** Zebra and Zakura embed a list (Zakura: 14,385 Mainnet entries to height 3,499,045; 10,059 Testnet entries to 4,023,200; gap <= 400 blocks and <= 32 MB). Below the last checkpoint: hash chain only, plus the state updates. Canopy − 1 is mandatory.
- **Recommendation for hayai.**
  - A checkpoint list per network in `hayai-consensus/src/checkpoints/{main,test}.txt`, same text format as Zakura (`height hash`), so the file is diffable against Zakura's and Zebra's.
  - Obtain: `hayai checkpoints` tool that reads the header chain of the node. Verify: (1) byte comparison with Zakura's and Zebra's lists over the common range; (2) a full shadow-verified run of hayai itself above Canopy (C4); (3) header-chain work: the list must be on the best-work header chain that B1 validates (PoW, difficulty). hayai therefore does not trust the list for proof of work: header sync validates every header in full (the cost is one Equihash per header, about 3.5 M x a few ms spread over the cores). The list then only selects which script and proof checks to skip.
  - Modes: `checkpoint_sync = true` (default): below the last checkpoint skip scripts, Sapling/Orchard/Ironwood/Sprout proofs and signatures; keep every state rule and every header rule; keep the header commitment check (B8). `checkpoint_sync = false`: full verification from Canopy; below Canopy the mandatory checkpoint stays (hash-only for proofs), as in the reference nodes.
  - A true no-checkpoint verification needs: A5b (BCTV14), v1–v3 script rules of the early chain (the same interpreter flags; zcashd applied P2SH and CLTV from genesis; confirm), pre-Canopy Ed25519 rules, pre-Heartwood coinbase rule (B20), the Sprout pool rules. Size L+. Not recommended.

---

## 3. Work package B: sync and node completeness

New crate **`hayai-sync`**: pure state machines, no sockets, no threads. Position: after hayai-validate, before hayai-net is not possible (hayai-net is above); so hayai-sync depends on hayai-wire, hayai-consensus, hayai-state (types only) and exposes traits that hayai-net and hayaid implement. hayai-net keeps the wire work.

### B1. Fork-aware header chain (L) — `hayai-sync/src/headers.rs`
- Replaces `hayaid/src/headers.rs::HeaderIndex` (a single chain plus pending).
- Structure: arena of header nodes `{hash, prev, height, time, bits, cumulative_work: U256, status}` with `HashMap<BlockHash, NodeId>`; the best-work tip; per node the status `HeaderValid | BodyStored | Validated | Invalid`. Below the finality depth the tree collapses to one chain: a flat file `headers.dat` (fixed 1,487-byte records, height-indexed, about 5.2 GB for Mainnet) or only `(hash, time, bits)` in memory (40 bytes x 3.5 M = 140 MB) with the full headers on disk. Recommendation: keep `(hash, time, bits, work)` in memory, full headers in the file.
- Validation of each header: A1 `check_header_context` with the context read along the node's own branch; H1–H5; checkpoint match (H15); reject a fork that leaves the chain below the last checkpoint or below the finality depth.
- Protocol: `getheaders` with a locator (tip, then exponential steps), 160 headers per message (`hayai-net/src/codec.rs:43`), from one sync peer at a time with a 2 minute stall rule; announce handling by `inv` then `getheaders`. Equihash runs on the rayon pool in batches of 160.
- Serves `getheaders` from the same structure (also fixes the limit "serves headers from the restart point").
- Tests: property test with random forks against a naive model; Zebra block vectors' contiguous headers; reorg across the finality boundary is refused; fuzz of out-of-order and duplicate `headers`.

### B2. Block download with an ordered commit window (L) — `hayai-sync/src/download.rs`
- Window of heights `[committed + 1, committed + W]` on the best header chain. Each slot: `Missing | Requested(peer, deadline) | Received(Arc<RawBlock>) | Prepared`. Requests: `getdata(MSG_BLOCK)` in batches of 16 per peer, at most 2 batches in flight per peer, slots assigned to the peer with the lowest latency estimate; a slot that passes its deadline goes to another peer and the first peer loses score.
- **Memory budget.** `sync.memory_budget_bytes` (default 1 GiB) bounds the sum of the bytes of `Received` and `Prepared` slots; W adapts (`budget / recent mean block size`, capped at 2,000). The requester does not ask for a slot that would pass the budget, except the lowest missing slot (no deadlock).
- **Pipeline.** Parse and context-free preparation run on arrival, out of order, in parallel (principle 8). Coins of inputs are not known out of order, so the out-of-order stage is: parse, txids, merkle root against the header, auth root. The in-order stage is `build_layer` on `Chain::view_speculative` and `verify` on the pool: the existing speculative tip gives a pipeline depth > 1 (hayaid does not use it yet; B2 makes the driver use `build_layer` / `push_speculative` / `confirm`).
- Merkle root mismatch or a block that does not match its header: the peer is banned; the slot is asked again.
- Tests: simulated peers (slow, silent, lying, out-of-order) on the loopback transport of `hayai-net/tests/loopback.rs`; the committed chain equals the source chain; the memory high-water mark stays under the budget.

### B3. Driver integration, fork choice and reorg (M) — `hayaid/src/node.rs`
- Fork choice: best cumulative work among header-valid chains whose blocks are not invalid; ties: the first seen (zcashd) — Zebra uses the hash; say which in the docs (open question 7).
- Reorg: when a better chain forks at depth d <= the layer window: store the side-branch bodies (block store keyed by hash, B9), then `Chain::pop` d times, validate the new branch with `validate_block`; if a block of the new branch is invalid, mark it and its descendants `Invalid`, pop back and re-apply the old branch from the stored bodies. Transactions of disconnected blocks return to the prepared store (current restriction in `docs/hayaid.md`).
- **Finality depth.** The layer window is 100 (`LAYER_WINDOW`). Zebra and Zakura use 1,000 (`MAX_BLOCK_REORG_HEIGHT`, raised in June 2026). At 25 s spacing 100 blocks are 42 minutes. Recommendation: window 1,000 on Mainnet/Testnet. The window index makes lookups independent of the window length (`state/lookup_through_window`: 0.28 ms); memory grows with 1,000 layers (measure; estimate 100–300 MB at full blocks).
- During initial sync the window is not needed below the last checkpoint: finalize at once (window 0) to cut memory and flush work.
- Tests: regtest pair with a forced fork (two producers, partition, heal); property test on `Chain` with pop/re-apply.

### B4. Address book and discovery (M) — `hayai-net/src/addrbook.rs` (new), `relay.rs`
- `addr` / `getaddr` handling (messages decode today and are ignored: `relay.rs:904-909`), `addrv2` optional. Address book with new/tried buckets keyed by source group (/16), persisted as `peers.dat`; at most 1,000 addresses per `addr`, at most one `getaddr` answer per connection, 23 % sample (zcashd).
- DNS seeders: Mainnet `dnsseed.z.cash`, `dnsseed.str4d.xyz`, `mainnet.seeder.zfnd.org`, `mainnet.seeder.shieldedinfra.net` (port 8233); Testnet `dnsseed.testnet.z.cash`, `testnet.seeder.zfnd.org` (18233) (`zakura-network/src/config.rs:874-885`). hayaid config reads `SocketAddr` only: add host-name resolution for seeders.
- Outbound target (default 8 full-relay), inbound limit, one connection per /16 outbound, feeler connections.
- Protocol version: raise `PROTOCOL_VERSION` from 170,150 to 170,160 (NU6.3) and 170,180/190 with NU7; minimum peer version per epoch (`hayai-net/src/protocol.rs:21-23`).

### B5. Misbehaviour scoring and peer management (M) — `hayai-sync/src/score.rs`, `hayai-net/src/relay.rs`
- Score per peer, ban at 100 for 24 h by IP. Table: invalid header PoW 100; header that fails contextual rules 100; block that does not match its header 100; block invalid by consensus 100 (not for compact-relay reconstruction faults: they cost nothing, as today); unconnected headers 20; unsolicited large messages 20; oversize or malformed frame 100; invalid transaction: 10, or 100 for a failed proof; NU6.2 branch id within the NU6.3 grace period: 0 (`ZC/transaction.rs:94`); stall on a requested block: 0 score, disconnect after 2 stalls.
- `TxSink::accept_tx` and `BlockSink` return a verdict enum (`Accepted | Known | Policy | Invalid(score)`) in place of `bool`.
- Eviction of inbound peers when full: protect by netgroup, latency, recent block and transaction relay (Bitcoin Core rule).

### B6. Restart and resume during sync (S–M) — `hayaid/src/node.rs`, `persist.rs`
- The coins best block and `state.log` already define the resume point. Add: header chain file (B1) with its own tail check; the download window is not persisted (bodies above the committed tip that are in the block store are re-read; others are requested again).
- Replay rule during sync below the checkpoint: the replay uses the same fast path as sync (B7), not full validation.
- Block store must accept bodies out of height order or the node stores a body only at commit (simpler; recommended: store at commit; the window holds bodies in memory).
- Test: kill -9 at random points of a regtest sync of 2,000 blocks; the final state root equals an uninterrupted run.

### B7. Checkpoint-range fast path (M) — `hayai-validate/src/lib.rs`
- `ValidateConfig.verify: VerifyLevel { Full, StateOnly }`. `StateOnly` (height <= last checkpoint and the header chain contains the checkpoint): `draft` without the sighash context (the 3.4 kB `Draft` and the digests are the cold cost of transparent blocks), no `check_scripts`, no shielded batch, no ZIP 213 decryption; everything in `contextual_check_with_outputs` stays, including the header commitment and the trees.
- This is an explicit mode with a typed result, not a silent skip (CLAUDE.md rule): the layer records `verified: StateOnly`, the trace row has the field, and a metric counts the blocks.

### B8. Verified roots: VCT or simpler (S) 
- Zakura's VCT exists because its per-block tree rebuild is about 70 % of its checkpoint commit time. hayai's batched appends cost 0.97 ms per 330 Orchard leaves and 2.4 ms per 330 Sapling leaves (`trees/*_append`). For the whole Mainnet history (50.5 M Orchard leaves, 0.67 M Ironwood; Sapling outputs unknown, estimate below 100 M): about 150 s of Orchard appends and about 12 minutes of Sapling appends per 100 M leaves. This is small against download time.
- Recommendation: no VCT. Compute the trees locally. The simpler equivalent already exists: from Heartwood the header of block n+1 commits to the roots of block n through the history tree, and hayai checks it for every block (H12). Below the checkpoint the headers are fixed by hash, so a wrong tree fails at the next block. Before Heartwood: Sapling/Blossom headers commit to the final Sapling root directly; Sprout has no commitment (as in the reference nodes).
- Requirement: the history state is never `None` in full mode (true from genesis).

### B9. Block store by hash, side branches (S) — `hayai-blockstore/src/lib.rs`
- Today: the first block of a height wins (`DuplicateHeight`). Needed for B3: `hash -> Loc` as the primary index and `height -> hash` for the best chain, rewritten at a reorg. Optional pruning later.

### B10. Mempool policy (M) — `hayai-prepared/src/store.rs`, `hayaid/src/node.rs::Mempool`
- Consensus-in-mempool: no coinbase; coinbase maturity at tip + 1; expiry (`expiry == 0 || expiry > tip`, and zcashd's "expiring soon" threshold of 3 blocks); lock time against the next block's MTP (not the block time); anchors and nullifiers against the tip (present through `prepare` on the view? nullifiers and anchors are not checked at admission today: add `tx_no_duplicates_in_chain` and anchor checks as `ZS/check/anchors.rs:476`, `nullifier.rs:148`); ZIP 218 per-transaction limit from NU7.
- Policy: ZIP 317 (conventional fee, unpaid actions limit 0 (was 50 until 2026-10-05), `mempool_checks`: `ZCH` `transaction::zip317`), standard scripts (`are_inputs_standard`, scriptSig <= 1,650 bytes and push-only, P2SH sigops <= 15: `ZC/transaction/check.rs:701-909`), dust and OP_RETURN rules (confirm zcashd values), maximum transaction size.
- Eviction: ZIP 401: cost limit 80,000,000, weighted random eviction, 60 minute memory of evicted ids (`zakurad/src/components/mempool/config.rs:70-72`). hayai's store evicts by cost limit; make it ZIP 401.
- Revalidation at a tip change: drop expired, conflicting, and (at an epoch change) all entries (present); re-add transactions of disconnected blocks.
- Rebroadcast: own (RPC-submitted) transactions every 10–30 minutes until mined; `mempool` message answer exists.
- `sendrawtransaction` RPC (needed for a "feature-complete node"; out of the two packages' strict scope; listed in open questions).

### B11. Trusted snapshot start (M) — `hayaid`, `hayai-coins/src/mem/snapshot.rs`
- A snapshot = `coins.snapshot` (exists: 1,280 CRC32C sections) + one `state.log` record (frontiers of Sprout, Sapling, Orchard, Ironwood; anchor sets; Sprout tree map; value pools; history peaks; the last 113 `(bits, time)`) + the header chain to that height.
- What a trusted format needs:
  1. One manifest: network, height, block hash, format version, backend-independent encodings, SHA-256 (not CRC32C: the CRC detects damage, not an attacker) of every section and of the manifest.
  2. Binding to the chain: the block hash is on the best-work header chain that the node validates (B1) and at or below a checkpoint. The frontier roots are bound by the history root in the header of height + 1. The value pools and the coin set are not committed by any header: they are trusted. Zcash has no UTXO commitment.
  3. Trust statement: the manifest hash is either embedded in the release (as Bitcoin Core's assumeutxo) or given by the operator in the configuration. No default download.
  4. Background validation (optional, recommended): a second chain syncs from genesis and compares its coin-set hash at the snapshot height with the manifest; a mismatch stops the node. Needs a canonical coin-set hash: hash of the 256 shards in key order (the shard order is by the first txid byte; inside a shard sort by outpoint).
  5. Anchor sets are large (one root per block per pool): ship them, or ship only the frontier and accept that anchors older than the snapshot are unknown (a consensus gap: reject). Ship them.
- `hayaid snapshot export|import` commands. Test: export at height h on Regtest, import in a fresh directory, sync to the tip, equal state hash.

### B12. Throughput estimate (labelled estimate)
Measured unit costs (`bench-results/summary.json`, Ryzen 9 9950X, 32 threads): parse 0.3–0.9 ms per real or full block; contextual check 0.26–1.8 ms per full block; state push 0.44 ms; coins commit 17–29 ms per 13,000 inputs + 13,000 outputs on the memory backing (about 1 µs per coin); cold full validation 25 ms (6,500 transparent txs), 92 ms mixed, 136 ms (330 Orchard actions, upstream) / 78 ms (zakura backend); warm 2.4–5 ms.
- Checkpoint range (B7): per full block about 1 ms parse + 2 ms state + trees. Most of the 3.5 M Mainnet blocks are small. Estimate 0.3 ms mean: about 20 minutes of in-order CPU, plus the spam period (many full blocks: estimate 500,000 blocks x 5 ms = 40 minutes), plus Equihash for 3.5 M headers on 32 threads (measure; no benchmark exists). Download: the chain size is several hundred GB (confirm); at 50 MB/s this is the bound: 1.5–3 hours. Expected total: 2–4 hours, network-bound.
- Full verification above Canopy without the checkpoint list: Orchard 50.5 M actions x 0.41 ms (upstream) = 5.7 h, x 0.24 ms (zakura) = 3.3 h; scripts about 4 µs per input in parallel; Sapling: no measurement exists (no parameters on this machine): add a benchmark in A4.
- Tip: one block per 75 s (25 s at NU7) at 2–5 ms warm: no constraint.

---

## 4. Work package C: consensus conformance testing

### C1. Inventory of vector sets (S)

| Set | Location | hayai uses today | Missing |
|---|---|---|---|
| Zebra block vectors (93 files) | `../zebra/zebra-test/src/vectors/` (same set in `zakura-src/zakura/crates/zakura-test/src/vectors/`; crate `zebra-test` 4.0.0 on crates.io, not in the local registry) | 6 blocks in `hayai-wire/tests/vectors` (Mainnet 0, 1, 1,687,106–108; Regtest 0), 2 blocks + 2 headers in `hayai-state/tests/vectors` (903,000/001, 1,046,400/401) | all others |
| Content of the Zebra set | Mainnet: 0–10, 202 (+ `202-bad`), 395, 396, 347,499–501 (Overwinter), 415,000, 419,199–202 (Sapling), 434,873, 653,599–601 (Blossom), 902,999–903,001 (Heartwood), 949,496, 975,066, 982,681, 1,046,399–401 (Canopy), 1,180,900, 1,687,106–108, 113, 118, 121 (NU5). Testnet: 0–10, 2,259, 141,042, 207,499–501, 279,999–280,001, 299,187–189, 299,201–202, 583,999–584,001, 903,799–801, 914,678, 925,483, 1,028,499–501, 1,095,000, 1,101,629, 1,115,999–1,116,001, 1,326,100, 1,599,199, 1,842,421, 432, 462, 467, 468. Also `sapling-treestate-main-0-419-201.txt`, `orchard_note_encryption.rs`. | | **No vector after NU5: none for NU6, NU6.1, NU6.2, NU6.3, NU7.** |
| ZIP 143 / 243 / 244 sighash vectors | `zebra-test/src/zip0143.rs`, `zip0243.rs`, `zip0244.rs`; hayai copies in `hayai-wire/tests/vectors/tx-zip0143.hex`, `tx-zip0243.hex`, `tx-zip0244.hex` | yes (parse, txid, auth digest) | sighash values against hayai's `SighashContext`; v6 sighash vectors (zcash-test-vectors `zip_0244`/v6; confirm the file name) |
| ZIP 221 history | `zcash_history` 0.5 `test_vectors.rs`; hayai `tests/vectors/zip_0221_v1.rs`, `v2.rs` | V1, V2 | V3 (in the crate) |
| zcash-test-vectors (github.com/zcash/zcash-test-vectors) | not local | indirect | Sprout/Sapling/Orchard merkle trees, note encryption (ZIP 212/213 relevant), `f4jumble` not needed, ZIP 316 not needed, Orchard key components not needed, `orchard_merkle_tree`, `sapling_note_encryption`, `orchard_note_encryption`, `zip_0143/0243/0244`, Ironwood vectors if published (confirm) |
| zcashd `script_tests.json`, `tx_valid.json`, `tx_invalid.json`, `sighash.json` (`src/test/data/`) | not local | no | all four. `sighash.json` is Sprout-era (pre-Overwinter); `tx_valid/invalid` are Bitcoin-derived with Zcash edits |
| `zcash_script` vectors | `~/.cargo/registry/.../zcash_script-0.6.0/src/test_vectors.rs` (14,705 lines, the port of `script_tests.json` without CSV, DERSIG, MINIMALIF, NULLFAIL, WITNESS cases) | no (hayai relies on the crate's own tests) | run them through `Draft::check_input` to test hayai's glue (flags, sighash callback) |
| Zakura Groth16 Sprout vectors | `ZC/primitives/groth16/vectors.rs` | no | for A5 |

### C2. Block-vector harness (M)
- New test crate file `crates/hayai-bench/tests/conformance_blocks.rs` plus fixtures under `crates/hayai-bench/tests/context/`.
- **State per block.** A block needs: parent tip (hash, height), the last 28 (113 at NU7) `(bits, time)` pairs, the coins its inputs spend, the Sprout/Sapling/Orchard/Ironwood frontiers and anchors that its transactions reference, the nullifier sets (emptiness of the block's nullifiers), value pools, history peaks at the parent.
- **Three classes.**
  1. From genesis: Mainnet 0–10 and Testnet 0–10 run with no seed. This tests the slow start, founders' reward (from height 1), difficulty for height <= 17, the genesis rules.
  2. Contiguous triples at boundaries (h−1, h, h+1; 1,687,106–108; 419,199–202; 299,187–189): seed a context fixture at the parent of the first block, then run the run. Most are coinbase-only or nearly so.
  3. Isolated blocks (202, 395, 396, 415,000, 434,873, 949,496, 975,066, 982,681, 1,180,900, 1,687,113/118/121, the Testnet singles): one fixture each.
- **Context fixture.** One JSON file per run: the fields above. Generator: `hayai-bench/src/bin/mkcontext.rs` that calls a synced reference node (`getblock`, `z_gettreestate`, `getrawtransaction`, history peaks). hayai's shadow seed (`hayaid/src/shadow.rs`, `upstream.rs`) already reads the same items: reuse its client. Pre-seed nullifiers as empty and mark anchors as "trusted from the fixture". History peaks: Zakura has no RPC for peaks (confirm); for V1/V2 trees compute them by replaying headers + roots from the activation height (roots from `z_gettreestate`), or start runs at activation blocks where the tree is empty (the boundary triples at Heartwood, Canopy, NU5 already are).
- **Assertions.** Every vector passes `validate_block` with `VerifyLevel::Full`; `202-bad` fails; the resulting roots equal the next header's commitment where the next block is in the set.
- **New vectors to add** (from the shadow node, checked into `hayai-bench/tests/vectors/`): triples at NU6, NU6.1 (lockbox disbursement block), NU6.2, the Orchard soft-fork height, NU6.3 on both networks; a v6 Ironwood block; a shielded-coinbase block; Testnet NU7 when it exists.
- Size M. Depends on A0–A6 for all vectors to pass; the harness itself can land first with an expected-failure list that shrinks.

### C3. Differential fuzzer for negative tests (L)
- No official set of invalid blocks exists. Design: `crates/hayai-fuzz` (new crate, not in the default workspace build of the node; `cargo-fuzz`/libFuzzer targets plus a proptest mode for stable CI).
- **Seeds.** The Zebra vectors with their contexts (C2), hayai-bench synthetic fixtures (real proofs), ZIP 244 vector transactions.
- **Mutators.**
  - Byte level: bit flips and splices inside transaction and header byte ranges (ranges from `hayai-wire/src/scan.rs`), then fix-up of the merkle root and auth root so the mutation reaches the rule and not only the root check; optional fix-up of PoW on Regtest parameters.
  - Structure-aware: Orchard/Ironwood flag byte (all 256 values); value balances (sign, ±1, MAX_MONEY edges); expiry (0, height−1, height, 499,999,999, 500,000,000); lock time and sequence; coinbase: height push forms (minimal/non-minimal), scriptSig length 1, 2, 100, 101, output values ±1 around the allowed total, missing/duplicated/reordered funding outputs, shielded outputs; duplicated inputs and nullifiers in a transaction and across the block; transaction order (child before parent); two coinbases; sigops at 20,000 and 20,001; block size at the limit; version/group id/branch id of each epoch in each epoch; header: version, time around MTP and MTP + 90 min, bits ±1, commitment field; ZIP 218 counts at 330/331, 300/301.
- **Oracles (what is callable as a library).**
  - `zebra-consensus` 11.0.0: `mod block` is private. Public: `transaction::check::*` (pure functions on `zebra_chain::Transaction`: `has_inputs_and_outputs`, `has_enough_orchard_flags`, `coinbase_tx_no_prevout_joinsplit_spend`, `joinsplit_has_vpub_zero`, `disabled_add_to_sprout_pool`, `spend_conflicts`, `coinbase_outputs_are_decryptable`, `coinbase_expiry_height`, `non_coinbase_expiry_height`, `lock_time_has_passed`, `consensus_branch_id`), `difficulty_is_valid`, `funding_stream_address`, the batch verifiers (`groth16`, `halo2`, `redjubjub`, `redpallas`, `ed25519`). Block-level `check::subsidy_is_valid`, `miner_fees_are_valid`, `merkle_root_validity` are not reachable.
  - `zebra-state` 10.1.0: `check` is re-exported (`lib.rs:61-63`) but most functions are `pub(crate)`. Public: `check::utxo::transparent_coinbase_spend`, `remaining_transaction_value`; `transparent_spend` needs a `ZebraDb`.
  - Co-resolution: `zebra-consensus` 11.0.0 and `zebra-state` 10.1.0 need `zebra-chain` 11.1.0 and `zcash_primitives` 0.29 (both in the registry). They do not share types with hayai's 0.30.1 pins, but cargo builds 0.29 and 0.30 side by side; the fuzzer passes bytes, so type identity is not needed. They predate NU6.3: usable for rules up to NU6.2 only. `zebra-chain` 13.0.1 co-resolves with hayai's pins but has no matching `zebra-consensus` in the registry (confirm which `zebra-consensus` release pairs with 13.0.1 and add it; 16.0.0 is the candidate named in CHANGES.md).
  - Zakura: `zakura-consensus` 10.0.0 is not on the local registry (only `zakura-chain` 9.0.0); use a path or git dependency on `../zakura-src/zakura`. Same visibility as Zebra: `transaction::check::*` public (adds the Ironwood, cross-address, Orchard-pool, proof-size functions), `block::check` private. `zakura-header-chain` is public for difficulty and time (`validate_contextual_difficulty_and_time`, `AdjustedDifficulty`, `validate_compact_target`, `validate_hash_filter`). `zakura_chain::parameters::subsidy::*` is public for subsidy, funding streams, fee share, NSM.
  - **Whole-block oracle.** Because the block-level functions are private in both, use the node as a process: `zakurad` (and `zebrad`) on Regtest with `getblocktemplate` in proposal mode (`{"mode":"proposal","data":hex}`): Zakura runs every block rule except proof of work for a proposal (`ZC/block.rs:407-412, 690-720`). hayai's `scripts/regtest_pair.sh --zakurad` already starts the pair. The fuzzer builds the chain on both nodes, submits each mutant as a proposal to the reference and to hayai's `validate_block`, and compares accept/reject. Reasons differ in text: compare the verdict, and keep a hand-made map from hayai error variants to reference reject classes for triage.
- **Verdict rule.** Any disagreement is a finding. hayai `Unsupported` counts as a disagreement once A-items are done.
- **CI.** (1) Deterministic proptest mode, fixed seeds, in-process oracles only, 2–5 minutes, on every PR. (2) Nightly job, time-boxed (30–60 minutes), libFuzzer with the process oracle. (3) Corpus and every found disagreement (minimized) under version control in `crates/hayai-fuzz/corpus/` and `regressions/`; each regression becomes a unit test.
- Size L. The in-process tier (transaction rules, subsidy, difficulty) is M and can start as soon as A0 lands.

### C4. Full-history replay as the acceptance gate (M, after B)
- Mode: hayai syncs from genesis with `checkpoint_sync = false` above Canopy (full verification) while a reference node at the same height range supplies the comparison data: per block the Sprout, Sapling, Orchard, Ironwood roots (`z_gettreestate`), the value pools (`getblock` `valuePools`), the subsidy split (`getblocksubsidy`), and at intervals the coin count. This is shadow mode without trust limits: every counter of `docs/hayaid.md` "Trust limits" must be zero.
- A second pass: `checkpoint_sync = true`; the final state hash (coin-set hash of B11, frontiers, pools, history peaks) equals the first pass.
- **Done means:** zero disagreements and zero `Unsupported` over the whole Mainnet and Testnet history on both crypto backends (NU7 Testnet range on the zakura backend); every rule row of section 1 is P with a named test; the fuzzer nightly has no open finding; `hayai_shadow_trusted_*` and `hayai_block_commitments_unchecked_total` are zero; a 7-day tip-following run on each network agrees with the reference on every block and on every rejected block that the reference rejects (inject the fuzzer's mutants at the tip on Regtest for the reject side).

---

## 5. Ordered implementation plan

Sizes: S <= 2 days, M <= 1 week, L > 1 week (one agent). "Files" lists the files an item owns, so parallel items do not collide.

**Phase 0 (serial, first):**
- **W0 = A0** (M). New `crates/hayai-consensus/*`; `hayai-crypto/src/lib.rs` (NU7 helpers, `zcash_note_encryption`, `bellman`/`bls12_381` re-exports); `hayai-state/src/check.rs` (`CheckConfig`); `hayaid/src/params.rs`, `config.rs`. Everything else depends on it.
- **W0c = C1 + C2 harness skeleton** (S + M). `hayai-bench/tests/conformance_blocks.rs`, `hayai-bench/src/bin/mkcontext.rs`, vector copies. Parallel to W0 (no shared files). Lands with an expected-failure list.

**Phase 1 (four agents in parallel after W0):**
- **W1 = A6 Ironwood** (L). `hayai-prepared/src/{prepare.rs,shielded.rs,lib.rs}`, `hayai-trees/src/lib.rs`, `hayai-state/src/{lib.rs,history.rs,check.rs (trees, anchors, pools)}`, `hayaid/src/{persist.rs,shadow.rs,node.rs (roots compare)}`. Highest priority.
- **W2 = A1 difficulty and time** (M). `hayai-consensus/src/difficulty.rs`, `hayai-wire/src/header.rs`, `hayai-relay/src/header_check.rs`, `hayaid/src/headers.rs`. Touches `hayai-state/src/lib.rs` (`Base` context) — coordinate with W1 by landing the `Base`/`Layer` field additions of W1 and W2 in one preliminary commit (**W0b**, S: add `bits`, Ironwood fields, extended `ValuePools`, `state.log` version 2).
- **W3 = A2 subsidy/funding/coinbase** (L). `hayai-consensus/src/{subsidy.rs,funding.rs,coinbase.rs}`, data files, `hayai-template/src/coinbase.rs`; one call site in `hayai-state/src/check.rs::check_coinbase_value` (after W1 merges its part, or a separate function file `hayai-state/src/coinbase.rs`).
- **W4 = A4 Sapling keys** (S) then **A3 ZIP 213** (S). `hayai-prepared/src/shielded.rs` (Sapling part; after W1's edit of the same file, or split `shielded.rs` into `orchard.rs` and `sapling.rs` in W0b), new `hayai-prepared/src/coinbase.rs`.
- **W5 = B4 address book + B5 scoring** (M + M). `hayai-net/src/{addrbook.rs,relay.rs,session.rs,protocol.rs}`, `hayai-sync/src/score.rs`. Independent of all A items.
- **W6 = B1 header chain** (L). `crates/hayai-sync/src/headers.rs`. Depends on W2 for the contextual check; can start with a trait for it.
- **W6c = C3 in-process tier** (M). `crates/hayai-fuzz`. Depends on W0.

**Phase 2:**
- **W7 = A5 Sprout** (L). `hayai-trees/src/sprout.rs`, `hayai-prepared/src/sprout.rs`, `check_version`, `check_txs` Sprout part, `Base` Sprout tree map. After W1 (same structs).
- **W8 = A7 NU7** (M). `hayai-consensus/src/{nsm.rs,limits.rs}`, additions in `subsidy.rs`, `funding.rs`. After W2, W3, W1.
- **W9 = B9 block store by hash** (S), **B2 download window** (L), **B7 fast path** (M), **A9 checkpoints** (M). `hayai-blockstore/src/lib.rs`, `hayai-sync/src/download.rs`, `hayai-validate/src/lib.rs`, `hayai-consensus/src/checkpoints.rs`. B2 after W6. B7 after W1.
- **W10 = B3 driver, fork choice, reorg, speculative pipeline** (M–L). `hayaid/src/node.rs` (single owner: no other item edits `node.rs` in this phase; earlier items keep their `node.rs` edits to call-site changes). After W6, W9.
- **W11 = B10 mempool** (M). `hayai-prepared/src/store.rs`, new `hayai-prepared/src/policy.rs`, `hayaid` `Mempool`. After W0; parallel to W9.
- **W11c = C3 process-oracle tier and nightly CI** (M). After W3, W1.

**Phase 3:**
- **W12 = B6 restart during sync** (S–M). After W10.
- **W13 = B11 snapshot** (M). After W1, W7 (the state record must be final).
- **W14 = C2 completion** (new vectors NU6–NU6.3) (S) and **C4 full-history replay** (M + machine time). After W10.
- **W15 = docs** (S): `docs/consensus-rules.md` regenerated from section 1, `docs/architecture.md` (hayai-consensus, hayai-sync), `docs/hayaid.md` limits, `CHANGES.md`.

Dependency summary: W0 → {W1, W2, W3, W4, W6c}; W2 → W6 → W9(B2) → W10 → {W12, W14}; W1 → {W7, W8, W9(B7), W13}; W3 → W8; W5 and W11 are independent.

---

## 6. Ironwood backend decision

**Evidence.**
- Upstream supports the full NU6.3 verification surface (section 0, finding 1). `BatchValidator::add_bundle` is byte-identical between `orchard` 0.15.5 and `zakura-orchard` 2.2.0 except the RNG trait (diff of `src/bundle/batch.rs`). hayai already compiles `OrchardCircuitVersion::PostNu6_3` on both backends.
- Upstream does not support NU7: no branch id, no heights (0.10.5, 0.10.6, and the librustzcash checkout 330e4c0 of 2026-08-23). Newer upstream (`orchard` 0.16.0, `zcash_primitives` 0.31.0-pre.0, `zcash_protocol` 0.11.0-pre.0) exists on crates.io (pinned by `zebra-chain` 14.0.0) but is not available locally to inspect.

**Options.**
1. Require the zakura backend from NU6.3. Not needed: upstream verifies NU6.3. Loses the independent backend on the live rules.
2. Bump the upstream pins to 0.16 / 0.31-pre. Possible later; pre-release pins (`=0.31.0-pre.0`) and `sapling-crypto` 0.9 change APIs; no evidence that they carry NU7 heights.
3. Implement per backend. Not needed for NU6.3: the code is backend-neutral.

**Recommendation.** Implement Ironwood once, backend-neutral, on the current pins (W1). Keep NU7 behind the facade helpers (`nu7_branch()`, `nu7_activation()`), which return `None` on upstream. On upstream a node at or after a network's NU7 height refuses to start with a clear error ("NU7 needs the zakura backend"), never a silent rule skip. Revisit option 2 when upstream publishes NU7 parameters. Both backends stay in CI for every epoch up to NU6.3; the differential between them is a free conformance check.

---

## 7. Open questions for the project owner

1. **NU7 authority.** Testnet NU7 at 4,465,026 and branch `0x77190ad9` come from the Zakura forks and the "valargroup deploy-nu7" draft. Is this the public Testnet's NU7, or a Zakura-only schedule? If public, is the zakura backend acceptable as the only NU7-capable build until upstream follows?
2. **Default backend.** Should Mainnet/Testnet binaries default to `upstream` (independence) or `zakura` (speed, NU7)?
3. **Checkpoint policy.** Confirm: checkpoint list as a speed option, full header validation always, mandatory hash-only range below Canopy (no BCTV14). Or is a true genesis verification (A5b, L+) a goal?
4. **Sapling keys.** Embed the two verifying keys in the repository (recommended), or the `bundled-prover` feature, or keep the parameter files?
5. **Finality depth.** 100 (today) or 1,000 (Zebra/Zakura since June 2026)?
6. **Snapshot trust.** Manifest hash embedded in releases, operator-supplied only, or both? Is background validation from genesis required?
7. **Fork-choice tie-break.** First seen (zcashd) or lowest hash (Zebra/Zakura)?
8. **Reference for parity.** Zebra 16.0.0 sources are not local. Is Zakura HEAD the primary reference, with Zebra confirmed later? Should the plan add `zebra-consensus` 16.0.0 as a dev-dependency of the fuzzer (needs a download)?
9. **Regtest rules.** Follow Zakura's Regtest waivers (unshielded coinbase spends, PoW waiver) as the Regtest definition?
10. **Mempool policy.** The architecture document says mempool policy is "deliberately incompatible". For a general node: adopt ZIP 317/401 and zcashd standardness exactly, or keep a miner-oriented policy?
11. **RPC scope.** "Feature-complete node" beyond mining RPC (`sendrawtransaction`, `getblock`, `getrawtransaction`, wallet-facing lightwalletd RPCs) is outside packages A and B. In scope?
12. **Details that need the spec or ZIP text:** Testnet genesis hash constant; coinbase maturity and expiry at NU7 (Zakura keeps 100 and 500,000,000); pre-Canopy Ed25519 acceptance rules; whether the Orchard v5 parser enforces canonical proof length before NU6.3; the Zakura RPC field for the Ironwood treestate; the count of distinct Sprout roots; the Mainnet chain size for the sync estimate.

---

### Critical Files for Implementation
- crates/hayai-state/src/check.rs
- crates/hayai-prepared/src/prepare.rs
- crates/hayai-prepared/src/shielded.rs
- crates/hayai-state/src/history.rs
- crates/hayaid/src/node.rs

Supporting files: crates/hayai-state/src/lib.rs, crates/hayaid/src/params.rs, crates/hayaid/src/headers.rs, crates/hayai-relay/src/header_check.rs, crates/hayai-net/src/relay.rs, crates/hayai-crypto/src/lib.rs. Reference sources: ../zakura-src/zakura/crates/zakura-consensus/src/block/check.rs, .../zakura-consensus/src/transaction/check.rs, .../zakura-consensus/src/transaction.rs, .../zakura-state/src/service/check.rs, .../zakura-header-chain/src/validation/contextual/adjusted_difficulty.rs, .../zakura-chain/src/parameters/network/subsidy.rs, .../zakura-chain/src/parameters/network_upgrade.rs, ../zebra/zebra-test/src/vectors/.
---

## Owner decisions (2026-10-04)

- **Q1, Q2 (NU7).** No NU7 work in Phase 1. `hayai-consensus` holds one rule set per network upgrade behind one interface, so that NU7 is one more rule set when a backend provides it. At an NU7 activation height that the build does not support, the node stops with a clear error. W8 and W9 stay in the plan after Phase 1. The default backend does not change.
- **Q3, Q4 (checkpoints and pre-Sapling history).** Do as Zakura does: a dense checkpoint list, and blocks below the last checkpoint are verified by hash.
- **Q5 (finality depth).** 1,000 blocks, as Zebra and Zakura.
- **Q6 (Sapling keys).** Embed the two verifying keys in the binary.
- **Q7 (fixtures from the network).** No. Do not store real Mainnet or Testnet blocks as fixtures. Tests for NU6 and later use generated data, the `zcash_history` vectors and shadow runs.
- **Q8 (mempool policy).** Apply every relevant ZIP (ZIP 317, ZIP 401) and zcashd standardness.
- Q9 to Q12 are open. The plan's recommendation applies until the owner decides.
