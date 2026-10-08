# hayai: plan for consensus completeness (A), sync and node completeness (B), conformance testing (C)

The plan comes from read-only work. The work changed no file and ran no cargo command.

## 0. Findings that change the brief

1. Upstream verifies Ironwood. `orchard` 0.15.5 has `BundleVersion::ironwood_v3()`, `ValuePool::Ironwood`, `OrchardCircuitVersion::PostNu6_3`, `Flags::CROSS_ADDRESS_DISABLED`, `note_encryption::IronwoodDomain`. `zcash_primitives` 0.30.1 has `TransactionData::ironwood_bundle()`, `sighash_v6.rs` and the Ironwood txid digest. `zcash_history` 0.5.0 has `V3` / `NodeDataV3` with `start_ironwood_root`, `end_ironwood_root`, `ironwood_tx`. hayai already builds the `PostNu6_3` key (`crates/hayai-prepared/src/shielded.rs:43-47`). The `Unsupported("ironwood bundle")` in `prepare.rs` is a hayai gap, not a backend gap.
2. NU6.3 is active on both networks. The activation heights in `zcash_protocol` 0.10.5 (`consensus.rs:502,535`) are Mainnet 3,428,143 and Testnet 4,134,000. hayai at the Mainnet tip (3,505,115) stops at the first v6 Ironwood block. Ironwood is the first blocker, before sync.
3. NU7 exists only in the Zakura forks. `zakura-protocol` 2.2.0 has `BranchId::Nu7 = 0x7719_0ad9`, Testnet activation 4,465,026 and Mainnet `None`. In upstream 0.10.5 and 0.10.6, `Nu7` is behind `cfg(zcash_unstable = "nu7")`, with branch `0xffff_ffff` and no heights. `zakura-primitives` 2.2.0 has no `zip233_amount`: its NU7 v6 format is the same as the NU6.3 v6 format.
4. Newer upstream releases exist, but the local registry does not have them. `zebra-chain` 14.0.0 pins `orchard 0.16.0`, `sapling-crypto 0.9`, `zcash_primitives =0.31.0-pre.0`, `zcash_protocol =0.11.0-pre.0`. Their sources are not available locally, so their NU7 content is unknown.
5. Reference versions. `zebra-consensus` 16.0.0 and `zebra-state` 14.0.0 are not in `~/.cargo/registry`. Present locally: `zebra-consensus` 11.0.0, `zebra-state` 10.1.0, `zebra-chain` 11.1.0 / 13.0.1 / 14.0.0, and the checkout (02f9648: zebra-consensus 9.0.0). The Zebra line numbers below come from the checkout. The Zakura line numbers come from HEAD 1377915 (2026-10-02). If exact parity with 16.0.0 is necessary, an implementer must confirm the rules against that release.
6. Rules that hayai applies with a wrong result today (not only absent rules):
   - The coinbase value check uses `paid > allowed` at every height (`hayai-state/src/check.rs:623`). From NU6 the rule is equality (ZIP 236).
   - The rule "some source of funds" counts Orchard actions without the `enableSpends` flag (`prepare.rs:306-311`). The spec counts actions only when the flag is 1.
   - `check_pow` (`hayai-wire/src/header.rs:214`) does not compare the target with `PoWLimit` on Mainnet and Testnet.
   - `NetParams::max_time_enforced` returns true for every Testnet height. The rule starts at Testnet height 653,606.
   - The header time rules run only in the relay path (`hayaid/src/headers.rs:245`), not in `validate_block`. The replay at a restart and any future sync path skip them.
   - The driver always uses `BlockLimits::PRE_NU7`.
   - Value pools hold Sapling and Orchard only.
   - The coinbase-spend rule (no transparent outputs) applies on Regtest too. Zakura does not apply it there (`zakura-chain/src/transaction.rs:557`, `should_allow_unshielded_coinbase_spends`).

Path abbreviations:

- `ZC` = `../zakura-src/zakura/crates/zakura-consensus/src`;
- `ZS` = `.../zakura-state/src/service`;
- `ZCH` = `.../zakura-chain/src`;
- `ZH` = `.../zakura-header-chain/src`;
- `ZB` = `../zebra`.

---

## 1. Rule checklist (deliverable A.8)

Status values:

- P = present;
- PART = partial;
- ABS = absent;
- WRONG = hayai applies the rule with a different result;
- PARSER = the upstream parser enforces the rule (a test must prove it).

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
| B16 | ZIP 237: subsidy = halving subsidy + ceil(NSM(parent) x 1375 / 10^10) from the reissuance height | `subsidy.rs` (`block_subsidy`, `reissuance_bonus`, `nsm_reissuance_height`) | ABS | A7 |
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

Rules that are not consensus rules and that the plan does not include:

- the `legacy_chain` check of Zakura (`ZS/check.rs:434`);
- the grace period of the NU6.3 branch id for misbehaviour (`ZC/transaction.rs:94`, a peer-scoring matter, used in B5).

---

## 2. Work package A: design for each item

### A0. New crate `hayai-consensus` and the small fixes (M)

- **Reason for a new crate.** The network parameters are in `hayaid/src/params.rs`. hayai-state receives them through `SubsidyRule`. Difficulty, subsidy, funding streams, checkpoints and limits need one crate below hayai-state. The new crate comes after hayai-wire. Its dependencies are hayai-crypto and hayai-wire.
- **Modules.**
  - `network.rs`: `Network { Mainnet, Testnet, Regtest(RegtestConfig) }`, the genesis hash and time for each network, the PoW limit, an `Upgrade` enum of hayai's own that includes `Nu7`, `activation(Upgrade) -> Option<u32>`, `branch_at(height)`, `spacing_at`, `max_time_start`, `min_difficulty_start`, `allows_unshielded_coinbase_spend`, `orchard_disabled(height)`.
  - `difficulty.rs`, `subsidy.rs`, `funding.rs`, `limits.rs`, `checkpoints.rs`, `nsm.rs`.
- **NU7 on 2 backends.** `BranchId::Nu7` exists only on the zakura backend. Put the single `cfg` in `hayai-crypto/src/lib.rs`, with the functions `pub fn nu7_branch() -> Option<BranchId>` and `pub fn nu7_activation(network_type) -> Option<u32>`. Both functions return `None` on upstream. No other crate uses a `cfg` for NU7.
- **Fixes in this item.**
  - Fix T3/T4 (flags) in `hayai-prepared/src/prepare.rs::draft`.
  - Fix T10 (soft fork) in `draft` with a height-derived field on `RuleEpoch`, or with a contextual check in `check_txs`. The contextual check is simpler, because `RuleEpoch` has no height.
  - Fix B24 (Regtest waiver) through `CheckConfig`.
  - Fix H14 with the Testnet genesis hash (`05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38`, confirm).
  - Remove the "full mode refuses Testnet" check in `hayaid/src/config.rs:264`.
- **Interface change.** `hayai_state::CheckConfig { limits, subsidy: &dyn SubsidyRule }` becomes `CheckConfig { rules: &hayai_consensus::Rules }`. `hayaid::params::NetParams` becomes a thin wrapper.
- **Tests.** Each fix has unit tests. For T3, a v5 transaction with Orchard actions, `enableSpends = 0` and no other input fails.

### A1. Difficulty and header time (M)

- **Rules.**
  - spec §7.7.3 (ThresholdBits, MeanTarget, ActualTimespanBounded, MedianTime);
  - spec §7.6 (time rules);
  - ZIP 205/208 (Testnet minimum difficulty);
  - ZIP 218 (NU7 window).
- **Constants.**
  - PoWAveragingWindow 17 (102 from NU7);
  - PoWMedianBlockSpan 11;
  - damping 4;
  - maximum adjustment up 16 %, down 32 %;
  - spacing 150 / 75 / 25 s;
  - the Testnet rule from height 299,188, with a gap strictly greater than 6 x spacing (18 x from NU7).
- **Design.** The function is `hayai_consensus::difficulty::expected_bits(network, height, candidate_time, context: &[(bits, time)]) -> u32`. The context is newest first, with the length `min(height, window + 11)`. Also add `median_time_past(context)`. Use `primitive_types::U256` (in the facade). Add `compact_from_target` to `hayai-wire/src/header.rs` beside `expand_target`.
- **Arithmetic.** Compute the mean as Zakura does (`adjusted_difficulty.rs` `mean_target_difficulty`): mean = sum of (target / n) + (sum of (target % n)) / n. This method prevents the 256-bit overflow of 102 Testnet targets (limit 2^251). Then compute `mean / AveragingWindowTimespan * bounded_timespan`, with PoWLimit as the maximum.
- **`check_header_context(network, header, height, context, now: Option<u32>)`.** The function checks H4, H6, H7, H9 and H10, and H11 when the caller gives `now`. One function serves 3 callers:
  - the relay header check (`hayaid/src/headers.rs`, fills `ParentInfo::expected_bits`);
  - the header sync (B1);
  - `validate_block`, through a new `ChainView::header_context()`. The replay gives `now = None`.
- **State.** A `Layer` gets `bits: u32`. `Base` keeps the last 113 `(bits, time)` pairs, in place of `times: VecDeque<u32>`. Version 2 of the `state.log` record stores them.
- **Tests.**
  - (a) Zebra block vectors: for each contiguous run, the expected bits of block n are equal to the bits of the header. The test needs 28 headers: use the header fixtures of C2.
  - (b) A differential test against `zakura_header_chain::AdjustedDifficulty::expected_difficulty_threshold` on random contexts (C3).
  - (c) The Testnet vectors 299,187–299,202 test the start of the minimum difficulty.
  - (d) The first 28 blocks: PoWLimit for height <= 17.
- **Size.** M.

### A2. Subsidy, funding streams, lockbox, coinbase outputs (L)

- **Rules.** Spec §7.8 (subsidy), §7.9 (founders' reward), §7.10 (funding streams), ZIP 207, 214 (revisions 1–3), 1014, 1015, 271, 1016, 236, 2001, 2008.
- **Data to port.** The source is `ZCH/parameters/network/subsidy/constants/{mainnet,testnet}.rs`:
  - 48 founder addresses for each network;
  - ECC/ZF/MG address lists (48 Mainnet, 51 Testnet);
  - FPF addresses;
  - numerators 7/5/8 (Canopy streams), 12 deferred + 8 FPF (NU6 and NU6.1 streams);
  - ranges: Mainnet 1,046,400..2,726,400, 2,726,400..3,146,400, 3,146,400..4,406,400; Testnet 1,028,500..2,796,000, 2,976,000..3,396,000, 3,536,500..4,476,000;
  - the NU6.1 disbursement: 10 outputs of 7,875 ZEC to `t3ev37Q2uL1sfTsiJQJiWJoFzQpDhmnUwYo` on Mainnet. The Testnet address is in the Testnet file;
  - the first halving on Testnet: 1,116,000;
  - the address change interval = post-Blossom halving interval / 48.
- **Design.**
  - Add `hayai_consensus::subsidy::{halving, halving_block_subsidy, founders_reward, founders_reward_script}` and `funding::{streams_at(height) -> Vec<(Receiver, amount, Option<script>)>, lockbox_disbursements(height)}`.
  - Decode the Base58Check addresses to scripts one time at the start (P2SH for `t3`/`t2`, P2PKH for the ZIP 2008 `t1` address).
  - Add `hayai_consensus::coinbase::check(coinbase outputs, height, fees, nsm_parent) -> Result<DeferredChange, _>`. It matches each required output by exact `(value, script)` with multiplicity (Zakura `UnmatchedCoinbaseOutputs`). Then it applies B12/B13.
- **State.** `ValuePools` gets `transparent`, `sprout`, `deferred` (and `ironwood` in A6, `nsm: i64` in A7). `value_pools_after` updates all pools. It checks that each pool is >= 0 and that the total is <= MAX_MONEY. Transparent pool change = coinbase outputs + outputs − spent inputs. Deferred change = lockbox share − disbursement.
- **Placement.** In `hayai-state/src/check.rs`, a call to `hayai_consensus::coinbase::check` replaces `check_coinbase_value`. `hayai-template/src/coinbase.rs` takes the required outputs from the same functions, so the template and the validator agree.
- **Tests.**
  - Zebra vectors with real coinbases at each boundary: Mainnet 1, 395/396, 653,600, 1,046,400, 1,180,900, 1,687,106; Testnet 1,028,500, 1,116,000, 1,326,100, 1,842,421.
  - A differential test against `zakura_chain::parameters::subsidy::{block_subsidy, funding_stream_values, founders_reward, miner_fee_share}` for each boundary height and for random heights. These functions are public, and `zakura-chain` 9.0.0 is already a dependency of hayai-bench.
  - No local vector exists for NU6 and NU6.1 blocks: get them from the shadow node (C2).
- **Size.** L (mostly data and tests).

### A3. ZIP 213 shielded coinbase (S)

- **Rule.** From Heartwood, every Sapling, Orchard and Ironwood output of a coinbase decrypts with OVK = 32 zero bytes. From Canopy, the lead byte is 0x02 (`Zip212Enforcement::On`). Before Heartwood, a coinbase has no shielded outputs.
- **Design.** Add `check_shielded_coinbase(tx, branch)` in the new file `hayai-prepared/src/coinbase.rs`. The function uses `sapling_crypto::note_encryption::try_sapling_output_recovery` and `zcash_note_encryption::try_output_recovery_with_ovk` over `OrchardDomain::for_action` / `IronwoodDomain::for_action`. Add `zcash_note_encryption` to the facade. Call the function from `draft` when `is_coinbase`. B19 and B20 also go in this file.
- **Tests.**
  - Zakura's `ORCHARD_NOTE_ENCRYPTION_ZERO_VECTOR` (`zakura-test/src/vectors/orchard_note_encryption.rs`, also in the Zebra checkout);
  - a real shielded coinbase block from the shadow node;
  - a mutated ciphertext fails.
- **Size.** S.

### A4. Sapling parameters and verification (S)

- **Gap.** `SaplingKeys::load` needs the 48 MB + 3.6 MB parameter files. Without them a Sapling bundle is `Unsupported`. Zakura uses `LocalTxProver::bundled()` (`ZC/primitives/sapling.rs:43`).
- **Design.** The verification needs only the 2 verifying keys (about 1–2 kB each). Do these steps:
  1. Extract the keys one time from the hash-checked parameter files.
  2. Commit them as `hayai-prepared/src/sapling-spend.vk` and `sapling-output.vk`.
  3. Load them with `bellman::groth16::VerifyingKey::read`.
  4. Test that they are equal to the keys of `zcash_proofs::load_parameters` when the files are present.

  `VerifyingKeys.sapling` becomes non-optional. Remove `sapling_params_dir` as a requirement. Alternative: the `bundled-prover` feature of `zcash_proofs` (50 MB in the binary).
- **Other Sapling gaps.** The review found no other gap in the contextual rules. The parser does the small-order checks and the canonical encodings, and C3 must prove this. Both backends use `sapling_crypto::BatchValidator` with the same API.
- **Tests.** Mainnet blocks 419,201/419,202 and 434,873 (Sapling transactions, in the Zebra vectors). For the tree, use `sapling-treestate-main-0-419-201.txt`.
- **Size.** S.

### A5. Sprout (L) and A5b BCTV14 (L, optional)

- **Method of the reference nodes.** Zebra and Zakura verify below the mandatory checkpoint (Canopy − 1: Mainnet 1,046,399, Testnet 1,028,499) by hash only (`ZCH/parameters/network.rs:271`). They verify Groth16 JoinSplits (v4, from Sapling) with an embedded 1,828-byte key (`ZC/primitives/groth16/sprout-groth16.vk`) and the Ed25519 signature. They have no BCTV14 verifier. They keep the Sprout tree, the anchors and the nullifiers for every block, also below the checkpoint, because the state needs them. For each anchor they keep the tree, for interstitial treestates.
- **Requirement for a hayai node from genesis.** The node must always keep the state. It must verify proofs only above the checkpoint. The reference nodes do the same.
- **Design.**
  - Wire: `hayai-wire` already delimits v1–v4 with JoinSplits (`scan.rs`). `check_version` accepts v1–v3 in their epochs: v1/v2 before Overwinter, v3 in Overwinter, v4 from Sapling. The upstream parser needs the right `BranchId`.
  - Tree: the new file `hayai-trees/src/sprout.rs` has depth 29, the SHA-256 compression function without padding, and `incrementalmerkletree::Frontier<_, 29>`. Sprout anchors need the frontier for each root, not only the root. `Base` keeps `sprout_trees: Map<root, Arc<SproutFrontier>>` (about 1 kB each), and `state.log` persists it. Mainnet has much fewer than 1 million distinct Sprout roots: confirm the count.
  - Contextual checks (`check_txs`): the Sprout nullifiers (2 for each JoinSplit) go into `Pool::Sprout`; the anchor chain inside a transaction (B28); the pool balance; T14, T15.
  - Proofs, in the new file `hayai-prepared/src/sprout.rs`:
    - `zcash_proofs::sprout::verify_proof` with the embedded key;
    - `h_sig = BLAKE2b-256("ZcashComputehSig", randomSeed || nf1 || nf2 || joinSplitPubKey)`;
    - Ed25519 with the ZIP 215 rules from Canopy, and the pre-Canopy rules before Canopy. Use `ed25519-zebra`. Confirm the pre-Canopy acceptance set.
  - NU7: NU7 does not allow v4 (T2), so the ZIP 218 JoinSplit limit of 0 needs no counter. Keep a counter all the same, for the error message.
- **A5b.** A full verification without checkpoints of the heights below the Sapling activation needs a BCTV14 verifier on alt_bn128. zcashd uses libsnark. Old librustzcash has a Rust port as `sprout::verify`, but only for Groth16. Thus BCTV14 needs new code with the `bn` crate and the Sprout verifying key. Recommendation: do not build it (see A9).
- **Tests.**
  - Zebra vectors with JoinSplits: Mainnet 396, 347,499–347,501, 415,000, 419,199–419,202; Testnet 2,259, 141,042, 207,499–207,501;
  - `ZC/primitives/groth16/vectors.rs`;
  - the vectors of the Sprout note commitment tree in zcash-test-vectors (not local: get them).
- **Size.** L.

### A6. Ironwood, NU6.3 (L)

- **Transaction rules.** T6, T7, T9, T11, T16, T17, T22, B14, B19, the Ironwood terms of T3/T4 and of the fee.
- **Design.**
  - `hayai-prepared/src/prepare.rs::draft`: read `tx.ironwood_bundle()`; the nullifiers go into `Pool::Ironwood`; `commitments.ironwood`; the anchor `(Pool::Ironwood, bundle.anchor())`; the `ironwood_actions` counter; the fee adds the Ironwood value balance.
  - `hayai-prepared/src/shielded.rs`: `ScopedBatch::add` queues the Ironwood bundle in the `PostNu6_3` group with the same sighash (Zakura `transaction.rs:1219-1228`). `verify_orchard` takes a closure that selects the bundle.
  - Tree: the Ironwood tree uses the Orchard node type and MerkleCRH^Orchard (`ZCH/ironwood.rs` re-exports `orchard::tree`). In `hayai-trees`: `pub type IronwoodFrontier = OrchardFrontier`. `Layer`, `Base`, `Anchors`, `ValuePools`, `BaseState`, `StateRecord` get the Ironwood field. `append_trees` appends 3 trees: run the 3 appends with `rayon::join`.
  - History: in `hayai-state/src/history.rs`, `TreeVersion::V3` with `NodeDataV3`. `HistoryLeaf` gets `ironwood_root` and `ironwood_tx`. At the NU6.3 activation block a new V3 tree starts.
  - Coinbase: the Ironwood value balance goes into `coinbase_value_out`. `PrebuiltBody::commit` refuses a coinbase with an Ironwood bundle (as it does for Sapling/Orchard).
  - Add Ironwood to `Anchors::get`, `Base::insert_anchor`, `has_anchor`, the shadow `trust_anchors` and `compare_roots`. Zakura `z_gettreestate` returns the Ironwood tree: confirm the field name.
- **Tests.**
  - the `zcash_history` V3 test vectors (in the crate, `test_vectors.rs`). Copy them as `zip_0221_v3.rs` beside the V1/V2 files in `hayai-state/tests/vectors`;
  - a fixture generator: a v6 bundle with `BundleVersion::ironwood_v3()` in `hayai-fixtures/src/lib.rs`;
  - real blocks: Mainnet 3,428,142–3,428,144 and Testnet 4,133,999–4,134,001 from the shadow node (C2);
  - the acceptance test: a shadow run across the Mainnet tip.
- **Size.** L. It is the first item to do: the Mainnet shadow mode stops without it.

### A7. NU7 (M, zakura backend only today)

Rules in Zakura's code:

1. ZIP 218 spacing 25 s, window 102, Testnet gap multiplier 18 (A1 parameters).
2. ZIP 218 shielded limits, with Ironwood and the global budget of 330 (`BlockLimits` gets `ironwood_actions`, `sprout_joinsplits`, `shielded_budget`; `hayai_consensus::limits::at(height)`; the driver no longer uses `PRE_NU7`).
3. Halving index from the elapsed seconds of the blocks over 3 eras (`subsidy.rs` `halving`); subsidy `12.5 ZEC x spacing / 150 / 2^halvings`.
4. Stretched end height of a funding stream: `end' = nu7 + 3 x (end − nu7)` (`nu7_adjusted_funding_stream_height`); the address period formula with ratio 3 (`funding_stream_address_period`); the rotation of the FPF address (ZIP 2008).
5. ZIP 2003: v4 is not allowed.
6. Fee split: 60 % of the aggregate fees go to the NSM. The rounding occurs one time for each block, in favour of the miner.
7. NSM balance:
   - the seed at NU7 − 1 = scheduled issuance − issued supply. A check compares the seed with the constant (Mainnet 36,858,445,520, Testnet 55,768,414,957 zatoshis);
   - for each block, `nsm += halving_subsidy − (change of the six pools)`;
   - the balance must stay >= 0.
8. ZIP 237 bonus from the reissuance height. The height is a derived value: the first block after the third halving and after NU7 at which the reserve condition is true (`nsm_reissuance_crossing_height`).
9. Protocol version 170,180 (Testnet) / 170,190 (Mainnet) (`zakura-network/src/protocol/external/types.rs:130`).
10. In Zakura's consensus code, the coinbase maturity stays 100 and the expiry threshold stays 500,000,000. Confirm this against the ZIP 218 text: the brief says that they scale, but Zakura does not scale them in consensus. The default expiry delta of wallets and of the template is policy.

- **Design.** All rules go in `hayai-consensus` (`nsm.rs`, `subsidy.rs`, `limits.rs`). They are a port of the Zakura functions, with differential tests against `zakura-chain`. `ValuePools.nsm: i64` is a new field. The template (`hayai-template/src/coinbase.rs`) uses `miner_fee_share`.
- **Dependencies.** A0, A1, A2, A6.

### A9. Checkpoints (M, with B7)

- **Reference.** Zebra and Zakura embed a list (Zakura: 14,385 Mainnet entries to height 3,499,045; 10,059 Testnet entries to 4,023,200; gap <= 400 blocks and <= 32 MB). Below the last checkpoint they check the hash chain only, plus the state updates. Canopy − 1 is mandatory.
- **Recommendation for hayai.**
  - A checkpoint list for each network in `hayai-consensus/src/checkpoints/{main,test}.txt`. The text format is the same as Zakura's (`height hash`), so a diff against the lists of Zakura and Zebra is possible.
  - Source of the list: a `hayai checkpoints` tool that reads the header chain of the node.
  - Verification of the list:
    1. a byte comparison with the lists of Zakura and Zebra over the common range;
    2. a full shadow-verified run of hayai itself above Canopy (C4);
    3. header-chain work: the list must be on the best-work header chain that B1 validates (PoW, difficulty).
  - Thus hayai does not trust the list for proof of work. The header sync validates every header in full. The cost is one Equihash for each header: about 3.5 M x a few ms, spread over the cores. The list then only selects the script and proof checks to skip.
  - Modes:
    - `checkpoint_sync = true` (default): below the last checkpoint, skip scripts and the Sapling/Orchard/Ironwood/Sprout proofs and signatures. Keep every state rule, every header rule and the header commitment check (B8).
    - `checkpoint_sync = false`: full verification from Canopy. Below Canopy the mandatory checkpoint stays (hash-only for proofs), as in the reference nodes.
  - A true verification without checkpoints needs these items:
    - A5b (BCTV14);
    - the v1–v3 script rules of the early chain (the same interpreter flags; zcashd applied P2SH and CLTV from genesis; confirm);
    - the pre-Canopy Ed25519 rules;
    - the pre-Heartwood coinbase rule (B20);
    - the Sprout pool rules.

    Size: L+. The plan does not recommend it.

---

## 3. Work package B: sync and node completeness

The new crate `hayai-sync` has pure state machines, no sockets and no threads. It comes after hayai-validate. A position before hayai-net is not possible, because hayai-net is above. Thus hayai-sync depends on hayai-wire, hayai-consensus and hayai-state (types only). It exposes traits that hayai-net and hayaid implement. hayai-net keeps the wire work.

### B1. Fork-aware header chain (L) — `hayai-sync/src/headers.rs`

- The new chain replaces `hayaid/src/headers.rs::HeaderIndex` (a single chain plus pending headers).
- Structure: an arena of header nodes `{hash, prev, height, time, bits, cumulative_work: U256, status}` with `HashMap<BlockHash, NodeId>`, the tip with the best work, and for each node the status `HeaderValid | BodyStored | Validated | Invalid`. Below the finality depth the tree becomes one chain. 2 options exist for that chain:
  - a flat file `headers.dat` (fixed 1,487-byte records, indexed by height, about 5.2 GB for Mainnet);
  - only `(hash, time, bits)` in memory (40 bytes x 3.5 M = 140 MB), with the full headers on disk.

  Recommendation: keep `(hash, time, bits, work)` in memory, and the full headers in the file.
- Validation of each header:
  - A1 `check_header_context`, with the context that the check reads along the own branch of the node;
  - H1–H5;
  - the checkpoint match (H15);
  - the rejection of a fork that leaves the chain below the last checkpoint or below the finality depth.
- Protocol:
  - `getheaders` with a locator (tip, then exponential steps);
  - 160 headers for each message (`hayai-net/src/codec.rs:43`);
  - one sync peer at a time, with a stall rule of 2 minutes;
  - the node handles an announcement with `inv`, then `getheaders`.

  Equihash runs on the rayon pool in batches of 160.
- The node serves `getheaders` from the same structure. This also removes the limit "serves headers from the restart point".
- Tests:
  - a property test with random forks against a naive model;
  - the contiguous headers of the Zebra block vectors;
  - a reorg across the finality boundary fails;
  - a fuzz test of out-of-order and duplicate `headers`.

### B2. Block download with an ordered commit window (L) — `hayai-sync/src/download.rs`

- The window of heights is `[committed + 1, committed + W]` on the best header chain. Each slot has one state: `Missing | Requested(peer, deadline) | Received(Arc<RawBlock>) | Prepared`. Requests:
  - `getdata(MSG_BLOCK)` in batches of 16 for each peer;
  - at most 2 batches in flight for each peer;
  - the downloader assigns slots to the peer with the lowest latency estimate;
  - a slot that passes its deadline goes to another peer, and the first peer loses score.
- **Memory budget.** `sync.memory_budget_bytes` (default 1 GiB) bounds the sum of the bytes of `Received` and `Prepared` slots. W adapts to `budget / recent mean block size`, with a maximum of 2,000. The requester does not ask for a slot that would pass the budget, except the lowest missing slot. This exception prevents a deadlock.
- **Pipeline.** Parse and context-free preparation run on arrival, out of order, in parallel (principle 8). The coins of the inputs are not known out of order. Thus the out-of-order stage is: parse, txids, merkle root against the header, auth root. The in-order stage is `build_layer` on `Chain::view_speculative` and `verify` on the pool. The existing speculative tip gives a pipeline depth > 1. hayaid does not use it yet: B2 makes the driver use `build_layer` / `push_speculative` / `confirm`.
- When the merkle root does not match, or a block does not match its header, the node bans the peer. Then it requests the slot again.
- Tests:
  - simulated peers (slow, silent, with false data, out-of-order) on the loopback transport of `hayai-net/tests/loopback.rs`;
  - the committed chain is equal to the source chain;
  - the peak memory stays under the budget.

### B3. Driver integration, fork choice and reorg (M) — `hayaid/src/node.rs`

- Fork choice: the best cumulative work among the header-valid chains whose blocks are not invalid. For a tie, zcashd selects the first chain that it sees, and Zebra uses the hash. The docs must state the choice (open question 7).
- Reorg, when a better chain forks at depth d <= the layer window:
  1. Store the bodies of the side branch (block store keyed by hash, B9).
  2. Do `Chain::pop` d times.
  3. Validate the new branch with `validate_block`.
  4. If a block of the new branch is invalid, mark it and its descendants `Invalid`. Then pop back, and apply the old branch again from the stored bodies.

  Transactions of disconnected blocks return to the prepared store (current restriction in `docs/hayaid.md`).
- **Finality depth.** The layer window is 100 (`LAYER_WINDOW`). Zebra and Zakura use 1,000 (`MAX_BLOCK_REORG_HEIGHT`, the value since June 2026). At a spacing of 25 s, 100 blocks are 42 minutes. Recommendation: a window of 1,000 on Mainnet/Testnet. The window index makes the time of a lookup independent of the window length (`state/lookup_through_window`: 0.28 ms). The memory grows with 1,000 layers: measure it (estimate: 100–300 MB at full blocks).
- During the initial sync, the node needs no window below the last checkpoint. There, finalize each block at once (window 0) to decrease the memory and the flush work.
- Tests:
  - a regtest pair with a forced fork (2 producers, partition, reconnection);
  - a property test on `Chain` with pop/re-apply.

### B4. Address book and discovery (M) — `hayai-net/src/addrbook.rs` (new), `relay.rs`

- Handle `addr` / `getaddr`. Today the node decodes these messages and ignores them (`relay.rs:904-909`). `addrv2` is optional. The address book has new/tried buckets keyed by source group (/16), and the node persists it as `peers.dat`. Limits (zcashd):
  - at most 1,000 addresses for each `addr`;
  - at most one `getaddr` answer for each connection;
  - a sample of 23 %.
- DNS seeders: Mainnet `dnsseed.z.cash`, `dnsseed.str4d.xyz`, `mainnet.seeder.zfnd.org`, `mainnet.seeder.shieldedinfra.net` (port 8233); Testnet `dnsseed.testnet.z.cash`, `testnet.seeder.zfnd.org` (18233) (`zakura-network/src/config.rs:874-885`). The hayaid configuration reads `SocketAddr` only. Add the resolution of host names for seeders.
- Also add an outbound target (default 8 full-relay), an inbound limit, one outbound connection for each /16, and feeler connections.
- Protocol version: set `PROTOCOL_VERSION` to 170,160 (NU6.3), from 170,150, and to 170,180/190 with NU7. Add a minimum peer version for each epoch (`hayai-net/src/protocol.rs:21-23`).

### B5. Misbehaviour scoring and peer management (M) — `hayai-sync/src/score.rs`, `hayai-net/src/relay.rs`

- Each peer has a score. At 100 the node bans the IP of the peer for 24 h. The score of each fault is in the table below.
- `TxSink::accept_tx` and `BlockSink` return a verdict enum (`Accepted | Known | Policy | Invalid(score)`) in place of `bool`.
- When the inbound slots are full, the node evicts an inbound peer. It protects peers by netgroup, by latency, and by recent relay of blocks and transactions (Bitcoin Core rule).

| Fault | Score |
|---|---|
| Invalid header PoW | 100 |
| Header that fails contextual rules | 100 |
| Block that does not match its header | 100 |
| Block invalid by consensus | 100 (not for compact-relay reconstruction faults: they cost nothing, as today) |
| Unconnected headers | 20 |
| Unsolicited large messages | 20 |
| Oversize or malformed frame | 100 |
| Invalid transaction | 10, or 100 for a failed proof |
| NU6.2 branch id within the NU6.3 grace period | 0 (`ZC/transaction.rs:94`) |
| Stall on a requested block | 0, disconnect after 2 stalls |

### B6. Restart and resume during sync (S–M) — `hayaid/src/node.rs`, `persist.rs`

- The coins best block and `state.log` already define the resume point. Add the header chain file (B1), with its own tail check. The node does not persist the download window. At a restart, the node reads again the bodies above the committed tip that are in the block store, and it requests the other bodies again.
- Replay rule during the sync below the checkpoint: the replay uses the same fast path as the sync (B7), not full validation.
- 2 options exist: the block store accepts bodies out of height order, or the node stores a body only at the commit. The second option is simpler and is the recommendation: the window holds the bodies in memory.
- Test: send kill -9 at random points of a regtest sync of 2,000 blocks. The final state root is equal to the root of a run without interruption.

### B7. Checkpoint-range fast path (M) — `hayai-validate/src/lib.rs`

- New field: `ValidateConfig.verify: VerifyLevel { Full, StateOnly }`. `StateOnly` applies when the height is <= the last checkpoint and the header chain contains the checkpoint. In this mode:
  - `draft` runs without the sighash context (the 3.4 kB `Draft` and the digests are the cold cost of transparent blocks);
  - no `check_scripts`;
  - no shielded batch;
  - no ZIP 213 decryption;
  - everything in `contextual_check_with_outputs` stays, with the header commitment and the trees.
- `StateOnly` is an explicit mode with a typed result, not a silent skip (CLAUDE.md rule). The layer records `verified: StateOnly`, the trace row has the field, and a metric counts the blocks.

### B8. Verified roots: VCT or a simpler method (S)

- Zakura's VCT exists because the rebuild of the trees for each block is about 70 % of its commit time in the checkpoint range. The batched appends of hayai cost 0.97 ms per 330 Orchard leaves and 2.4 ms per 330 Sapling leaves (`trees/*_append`).
- The whole Mainnet history has 50.5 M Orchard leaves and 0.67 M Ironwood leaves. The count of Sapling outputs is unknown (estimate: below 100 M). The cost is about 150 s of Orchard appends, and about 12 minutes of Sapling appends for each 100 M leaves. This cost is small compared with the download time.
- Recommendation: no VCT. Compute the trees locally. A simpler equivalent already exists. From Heartwood, the header of block n+1 commits to the roots of block n through the history tree, and hayai checks this for every block (H12). Below the checkpoint the checkpoint hashes fix the headers, so a wrong tree fails at the next block.
- Before Heartwood, the Sapling/Blossom headers commit to the final Sapling root directly. Sprout has no commitment (as in the reference nodes).
- Requirement: the history state must never be `None` in full mode. This is true from genesis.

### B9. Block store by hash, side branches (S) — `hayai-blockstore/src/lib.rs`

- Today the store keeps the first block of a height (`DuplicateHeight`). B3 needs `hash -> Loc` as the primary index and `height -> hash` for the best chain. The store writes `height -> hash` again at a reorg. Pruning is optional, later.

### B10. Mempool policy (M) — `hayai-prepared/src/store.rs`, `hayaid/src/node.rs::Mempool`

- Consensus rules in the mempool:
  - no coinbase;
  - coinbase maturity at tip + 1;
  - expiry (`expiry == 0 || expiry > tip`), and the "expiring soon" threshold of zcashd of 3 blocks;
  - lock time against the MTP of the next block (not the block time);
  - anchors and nullifiers against the tip. Does `prepare` on the view check them? Today the node does not check nullifiers and anchors at admission. Add `tx_no_duplicates_in_chain` and anchor checks, as in `ZS/check/anchors.rs:476` and `nullifier.rs:148`;
  - the ZIP 218 limit for each transaction from NU7.
- Policy:
  - ZIP 317: the conventional fee; the limit of unpaid actions 0 (50 until 2026-10-05); `mempool_checks`: `ZCH` `transaction::zip317`;
  - standard scripts: `are_inputs_standard`, scriptSig <= 1,650 bytes and push-only, P2SH sigops <= 15 (`ZC/transaction/check.rs:701-909`);
  - dust and OP_RETURN rules (confirm the zcashd values);
  - the maximum transaction size.
- Eviction (ZIP 401): cost limit 80,000,000, weighted random eviction, and a memory of 60 minutes for evicted ids (`zakurad/src/components/mempool/config.rs:70-72`). The store of hayai evicts by cost limit. Change it to ZIP 401.
- Revalidation at a tip change: remove the expired and the conflicting entries, and all entries at an epoch change (present). Add the transactions of disconnected blocks again.
- Rebroadcast: send the own transactions (submitted through RPC) again each 10–30 minutes, until a block includes them. The answer to the `mempool` message exists.
- `sendrawtransaction` RPC: a "feature-complete node" needs it. It is out of the strict scope of the 2 packages, and the open questions list it.

### B11. Trusted snapshot start (M) — `hayaid`, `hayai-coins/src/mem/snapshot.rs`

- A snapshot has these parts:
  - `coins.snapshot` (exists: 1,280 CRC32C sections);
  - one `state.log` record (frontiers of Sprout, Sapling, Orchard, Ironwood; anchor sets; Sprout tree map; value pools; history peaks; the last 113 `(bits, time)`);
  - the header chain to that height.
- Needs of a trusted format:
  1. One manifest: network, height, block hash, format version, backend-independent encodings, and the SHA-256 of every section and of the manifest. Use SHA-256 and not CRC32C: the CRC detects damage, not an attacker.
  2. Binding to the chain: the block hash is on the best-work header chain that the node validates (B1), and at or below a checkpoint. The history root in the header of height + 1 binds the frontier roots. No header commits to the value pools and the coin set: the node trusts them. Zcash has no UTXO commitment.
  3. Trust statement: the release embeds the manifest hash (as Bitcoin Core's assumeutxo), or the operator gives it in the configuration. There is no default download.
  4. Background validation (optional, recommended): a second chain synchronizes from genesis. It compares its coin-set hash at the snapshot height with the manifest, and a mismatch stops the node. This needs a canonical hash of the coin set: the hash of the 256 shards in key order. The shard order is by the first byte of the txid. Inside a shard, the order is by outpoint.
  5. Anchor sets are large (one root for each block and each pool). 2 options exist: ship them, or ship only the frontier and accept that anchors older than the snapshot are unknown. The second option is a consensus gap: reject it. Ship the anchor sets.
- Commands: `hayaid snapshot export|import`. Test:
  1. Export at height h on Regtest.
  2. Import in a new directory.
  3. Synchronize to the tip.
  4. Compare the state hashes: they must be equal.

### B12. Throughput estimate (labelled estimate)

Measured unit costs (`bench-results/summary.json`, Ryzen 9 9950X, 32 threads):

| Operation | Cost |
|---|---|
| Parse | 0.3–0.9 ms per real or full block |
| Contextual check | 0.26–1.8 ms per full block |
| State push | 0.44 ms |
| Coins commit | 17–29 ms per 13,000 inputs + 13,000 outputs on the memory backing (about 1 µs per coin) |
| Cold full validation | 25 ms (6,500 transparent txs), 92 ms mixed, 136 ms (330 Orchard actions, upstream) / 78 ms (zakura backend) |
| Warm | 2.4–5 ms |

- Checkpoint range (B7), CPU: for each full block, about 1 ms parse + 2 ms state + trees. Most of the 3.5 M Mainnet blocks are small. With an estimated mean of 0.3 ms, the in-order CPU time is about 20 minutes. Add the spam period (many full blocks: estimate 500,000 blocks x 5 ms = 40 minutes). Add Equihash for 3.5 M headers on 32 threads (measure it: no benchmark exists).
- Checkpoint range (B7), download: the chain size is several hundred GB (confirm). At 50 MB/s the download is the bound: 1.5–3 hours. Expected total: 2–4 hours, network-bound.
- Full verification above Canopy without the checkpoint list:
  - Orchard: 50.5 M actions x 0.41 ms (upstream) = 5.7 h, x 0.24 ms (zakura) = 3.3 h;
  - scripts: about 4 µs for each input, in parallel;
  - Sapling: no measurement exists, because this machine has no parameters. Add a benchmark in A4.
- Tip: one block each 75 s (25 s at NU7), at 2–5 ms warm. This is no constraint.

---

## 4. Work package C: consensus conformance testing

### C1. Inventory of vector sets (S)

| Set | Location | hayai uses today | Missing |
|---|---|---|---|
| Zebra block vectors (93 files) | `../zebra/zebra-test/src/vectors/` (same set in `zakura-src/zakura/crates/zakura-test/src/vectors/`; crate `zebra-test` 4.0.0 on crates.io, not in the local registry) | 6 blocks in `hayai-wire/tests/vectors` (Mainnet 0, 1, 1,687,106–108; Regtest 0), 2 blocks + 2 headers in `hayai-state/tests/vectors` (903,000/001, 1,046,400/401) | all others |
| Content of the Zebra set | Mainnet: 0–10, 202 (+ `202-bad`), 395, 396, 347,499–501 (Overwinter), 415,000, 419,199–202 (Sapling), 434,873, 653,599–601 (Blossom), 902,999–903,001 (Heartwood), 949,496, 975,066, 982,681, 1,046,399–401 (Canopy), 1,180,900, 1,687,106–108, 113, 118, 121 (NU5). Testnet: 0–10, 2,259, 141,042, 207,499–501, 279,999–280,001, 299,187–189, 299,201–202, 583,999–584,001, 903,799–801, 914,678, 925,483, 1,028,499–501, 1,095,000, 1,101,629, 1,115,999–1,116,001, 1,326,100, 1,599,199, 1,842,421, 432, 462, 467, 468. Also `sapling-treestate-main-0-419-201.txt`, `orchard_note_encryption.rs`. | | No vector after NU5: none for NU6, NU6.1, NU6.2, NU6.3, NU7. |
| ZIP 143 / 243 / 244 sighash vectors | `zebra-test/src/zip0143.rs`, `zip0243.rs`, `zip0244.rs`; hayai copies in `hayai-wire/tests/vectors/tx-zip0143.hex`, `tx-zip0243.hex`, `tx-zip0244.hex` | yes (parse, txid, auth digest) | sighash values against hayai's `SighashContext`; v6 sighash vectors (zcash-test-vectors `zip_0244`/v6; confirm the file name) |
| ZIP 221 history | `zcash_history` 0.5 `test_vectors.rs`; hayai `tests/vectors/zip_0221_v1.rs`, `v2.rs` | V1, V2 | V3 (in the crate) |
| zcash-test-vectors (github.com/zcash/zcash-test-vectors) | not local | indirect | Sprout/Sapling/Orchard merkle trees, note encryption (ZIP 212/213 relevant), `f4jumble` not needed, ZIP 316 not needed, Orchard key components not needed, `orchard_merkle_tree`, `sapling_note_encryption`, `orchard_note_encryption`, `zip_0143/0243/0244`, Ironwood vectors if published (confirm) |
| zcashd `script_tests.json`, `tx_valid.json`, `tx_invalid.json`, `sighash.json` (`src/test/data/`) | not local | no | all four. `sighash.json` is Sprout-era (pre-Overwinter); `tx_valid/invalid` are Bitcoin-derived with Zcash edits |
| `zcash_script` vectors | `~/.cargo/registry/.../zcash_script-0.6.0/src/test_vectors.rs` (14,705 lines, the port of `script_tests.json` without CSV, DERSIG, MINIMALIF, NULLFAIL, WITNESS cases) | no (hayai relies on the crate's own tests) | run them through `Draft::check_input` to test hayai's glue (flags, sighash callback) |
| Zakura Groth16 Sprout vectors | `ZC/primitives/groth16/vectors.rs` | no | for A5 |

### C2. Block-vector harness (M)

- New test file `crates/hayai-bench/tests/conformance_blocks.rs`, with fixtures under `crates/hayai-bench/tests/context/`.
- **State for each block.** A block needs:
  - the parent tip (hash, height);
  - the last 28 (113 at NU7) `(bits, time)` pairs;
  - the coins that its inputs spend;
  - the Sprout/Sapling/Orchard/Ironwood frontiers and anchors that its transactions reference;
  - the nullifier sets (the nullifiers of the block are not in them);
  - the value pools;
  - the history peaks at the parent.
- **3 classes.**
  1. From genesis: Mainnet 0–10 and Testnet 0–10 run with no seed. This class tests the slow start, the founders' reward (from height 1), the difficulty for height <= 17 and the genesis rules.
  2. Contiguous triples at boundaries (h−1, h, h+1; 1,687,106–108; 419,199–202; 299,187–189). Seed a context fixture at the parent of the first block. Then run the triple. Most of these blocks have only a coinbase, or almost only a coinbase.
  3. Isolated blocks (202, 395, 396, 415,000, 434,873, 949,496, 975,066, 982,681, 1,180,900, 1,687,113/118/121, the Testnet singles): one fixture each.
- **Context fixture.** One JSON file for each run, with the fields above. The generator `hayai-bench/src/bin/mkcontext.rs` calls a synced reference node (`getblock`, `z_gettreestate`, `getrawtransaction`, history peaks). The shadow seed of hayai (`hayaid/src/shadow.rs`, `upstream.rs`) already reads the same items: use its client again. Pre-seed nullifiers as empty, and mark anchors as "trusted from the fixture".
- **History peaks.** Zakura has no RPC for peaks (confirm). For V1/V2 trees, compute the peaks with a replay of headers + roots from the activation height (roots from `z_gettreestate`). Alternatively, start runs at activation blocks, where the tree is empty. The boundary triples at Heartwood, Canopy and NU5 already start there.
- **Assertions.**
  - every vector passes `validate_block` with `VerifyLevel::Full`;
  - `202-bad` fails;
  - the resulting roots are equal to the commitment of the next header, when the next block is in the set.
- **New vectors.** The source is the shadow node, and the place is `hayai-bench/tests/vectors/`:
  - triples at NU6, NU6.1 (lockbox disbursement block), NU6.2, the Orchard soft-fork height, and NU6.3 on both networks;
  - a v6 Ironwood block;
  - a shielded-coinbase block;
  - Testnet NU7 when it exists.
- Size: M. All vectors pass only after A0–A6. The harness can merge first, with a list of expected failures that becomes shorter.

### C3. Differential fuzzer for negative tests (L)

- No official set of invalid blocks exists. Design: the new crate `crates/hayai-fuzz`, outside the default workspace build of the node. It has `cargo-fuzz`/libFuzzer targets and a proptest mode for stable CI.
- **Seeds.** The Zebra vectors with their contexts (C2), the synthetic fixtures of hayai-fixtures (real proofs), the transactions of the ZIP 244 vectors.
- **Mutators.**
  - Byte level: bit flips and splices inside the byte ranges of transactions and headers (ranges from `hayai-wire/src/scan.rs`). Then a fix-up of the merkle root and the auth root, so that the mutation reaches the rule and not only the root check. Optional: a fix-up of PoW on Regtest parameters.
  - Structure-aware:
    - the Orchard/Ironwood flag byte (all 256 values);
    - value balances (sign, ±1, MAX_MONEY limits);
    - expiry (0, height−1, height, 499,999,999, 500,000,000);
    - lock time and sequence;
    - coinbase: height push forms (minimal/non-minimal), scriptSig length 1, 2, 100, 101, output values ±1 around the allowed total, missing/duplicated/reordered funding outputs, shielded outputs;
    - duplicated inputs and nullifiers in a transaction and across the block;
    - transaction order (child before parent);
    - 2 coinbases;
    - sigops at 20,000 and 20,001;
    - block size at the limit;
    - version/group id/branch id of each epoch in each epoch;
    - header: version, time around MTP and MTP + 90 min, bits ±1, commitment field;
    - ZIP 218 counts at 330/331, 300/301.
- **Oracles (functions that a library call can reach).**
  - `zebra-consensus` 11.0.0: `mod block` is private. The public items are `transaction::check::*` (pure functions on `zebra_chain::Transaction`: `has_inputs_and_outputs`, `has_enough_orchard_flags`, `coinbase_tx_no_prevout_joinsplit_spend`, `joinsplit_has_vpub_zero`, `disabled_add_to_sprout_pool`, `spend_conflicts`, `coinbase_outputs_are_decryptable`, `coinbase_expiry_height`, `non_coinbase_expiry_height`, `lock_time_has_passed`, `consensus_branch_id`), `difficulty_is_valid`, `funding_stream_address` and the batch verifiers (`groth16`, `halo2`, `redjubjub`, `redpallas`, `ed25519`). A test cannot reach the block-level functions `check::subsidy_is_valid`, `miner_fees_are_valid` and `merkle_root_validity`.
  - `zebra-state` 10.1.0: the crate re-exports `check` (`lib.rs:61-63`), but most functions are `pub(crate)`. The public functions are `check::utxo::transparent_coinbase_spend` and `remaining_transaction_value`. `transparent_spend` needs a `ZebraDb`.
  - Co-resolution: `zebra-consensus` 11.0.0 and `zebra-state` 10.1.0 need `zebra-chain` 11.1.0 and `zcash_primitives` 0.29 (both in the registry). They do not share types with the 0.30.1 pins of hayai, but cargo builds 0.29 and 0.30 side by side. The fuzzer sends bytes, so it does not need type identity. These crates are older than NU6.3, so they apply only to rules up to NU6.2.
  - `zebra-chain` 13.0.1 co-resolves with the pins of hayai, but the registry has no matching `zebra-consensus`. Confirm the `zebra-consensus` release that pairs with 13.0.1, and add it. CHANGES.md names 16.0.0 as the candidate.
  - Zakura: `zakura-consensus` 10.0.0 is not in the local registry (only `zakura-chain` 9.0.0). Use a path or git dependency on `../zakura-src/zakura`. The visibility is the same as in Zebra: `transaction::check::*` is public (with the added Ironwood, cross-address, Orchard-pool and proof-size functions), and `block::check` is private. `zakura-header-chain` is public for difficulty and time (`validate_contextual_difficulty_and_time`, `AdjustedDifficulty`, `validate_compact_target`, `validate_hash_filter`). `zakura_chain::parameters::subsidy::*` is public for subsidy, funding streams, fee share and NSM.
  - **Whole-block oracle.** The block-level functions are private in both crates. Thus use the node as a process: `zakurad` (and `zebrad`) on Regtest, with `getblocktemplate` in proposal mode (`{"mode":"proposal","data":hex}`). For a proposal, Zakura runs every block rule except proof of work (`ZC/block.rs:407-412, 690-720`). The script `scripts/regtest_pair.sh --zakurad` of hayai already starts the pair. The fuzzer builds the chain on both nodes. It sends each mutant as a proposal to the reference and to `validate_block` of hayai, and compares accept/reject.
  - The reasons differ in text. Compare the verdict. For triage, keep a hand-made map from the error variants of hayai to the reject classes of the reference.
- **Verdict rule.** Any disagreement is a finding. After the A items are done, a hayai `Unsupported` counts as a disagreement.
- **CI.**
  1. A deterministic proptest mode: fixed seeds, in-process oracles only, 2–5 minutes, on every PR.
  2. A nightly job with a time limit (30–60 minutes): libFuzzer with the process oracle.
  3. The corpus and every disagreement found (minimized), under version control in `crates/hayai-fuzz/corpus/` and `regressions/`. Each regression becomes a unit test.
- Size: L. The in-process tier (transaction rules, subsidy, difficulty) is M. It can start when A0 merges.

### C4. Full-history replay as the acceptance gate (M, after B)

- Mode: hayai synchronizes from genesis with `checkpoint_sync = false` above Canopy (full verification). At the same time, a reference node at the same height range supplies the comparison data:
  - for each block, the Sprout, Sapling, Orchard and Ironwood roots (`z_gettreestate`);
  - the value pools (`getblock` `valuePools`);
  - the subsidy split (`getblocksubsidy`);
  - at intervals, the coin count.

  This mode is shadow mode without trust limits: every counter of `docs/hayaid.md` "Trust limits" must be 0.
- A second pass uses `checkpoint_sync = true`. Its final state hash (coin-set hash of B11, frontiers, pools, history peaks) is equal to the hash of the first pass.
- **Completion criteria.**
  - 0 disagreements and 0 `Unsupported` over the whole Mainnet and Testnet history on both crypto backends (NU7 Testnet range on the zakura backend);
  - every rule row of section 1 is P, with a named test;
  - the nightly fuzzer job has no open finding;
  - `hayai_shadow_trusted_*` and `hayai_block_commitments_unchecked_total` are 0;
  - a run of 7 days at the tip on each network agrees with the reference on every block, and on every block that the reference rejects. For the reject side, inject the mutants of the fuzzer at the tip on Regtest.

---

## 5. Ordered implementation plan

Sizes: S <= 2 days, M <= 1 week, L > 1 week (one agent). Each item lists the files that it owns, so parallel items do not edit the same files.

**Phase 0 (serial, first):**
- **W0 = A0** (M). New `crates/hayai-consensus/*`; `hayai-crypto/src/lib.rs` (NU7 helpers, `zcash_note_encryption`, `bellman`/`bls12_381` re-exports); `hayai-state/src/check.rs` (`CheckConfig`); `hayaid/src/params.rs`, `config.rs`. All other items depend on it.
- **W0c = C1 + C2 harness skeleton** (S + M). `hayai-bench/tests/conformance_blocks.rs`, `hayai-bench/src/bin/mkcontext.rs`, vector copies. It runs in parallel to W0 (no shared files). It merges with a list of expected failures.

**Phase 1 (4 agents in parallel after W0):**
- **W1 = A6 Ironwood** (L). `hayai-prepared/src/{prepare.rs,shielded.rs,lib.rs}`, `hayai-trees/src/lib.rs`, `hayai-state/src/{lib.rs,history.rs,check.rs (trees, anchors, pools)}`, `hayaid/src/{persist.rs,shadow.rs,node.rs (roots compare)}`. It has the highest priority.
- **W2 = A1 difficulty and time** (M). `hayai-consensus/src/difficulty.rs`, `hayai-wire/src/header.rs`, `hayai-relay/src/header_check.rs`, `hayaid/src/headers.rs`. W2 also changes `hayai-state/src/lib.rs` (`Base` context). Coordinate with W1: merge the `Base`/`Layer` field additions of W1 and W2 in one preliminary commit (**W0b**, S: add `bits`, the Ironwood fields, the extended `ValuePools` and `state.log` version 2).
- **W3 = A2 subsidy/funding/coinbase** (L). `hayai-consensus/src/{subsidy.rs,funding.rs,coinbase.rs}`, data files, `hayai-template/src/coinbase.rs`; one call site in `hayai-state/src/check.rs::check_coinbase_value` (after W1 merges its part, or in a separate file `hayai-state/src/coinbase.rs`).
- **W4 = A4 Sapling keys** (S), then **A3 ZIP 213** (S). `hayai-prepared/src/shielded.rs` (Sapling part; after the edit of W1 to the same file, or split `shielded.rs` into `orchard.rs` and `sapling.rs` in W0b), new `hayai-prepared/src/coinbase.rs`.
- **W5 = B4 address book + B5 scoring** (M + M). `hayai-net/src/{addrbook.rs,relay.rs,session.rs,protocol.rs}`, `hayai-sync/src/score.rs`. It does not depend on an A item.
- **W6 = B1 header chain** (L). `crates/hayai-sync/src/headers.rs`. It depends on W2 for the contextual check. It can start with a trait for that check.
- **W6c = C3 in-process tier** (M). `crates/hayai-fuzz`. It depends on W0.

**Phase 2:**
- **W7 = A5 Sprout** (L). `hayai-trees/src/sprout.rs`, `hayai-prepared/src/sprout.rs`, `check_version`, the Sprout part of `check_txs`, the Sprout tree map of `Base`. It comes after W1 (same structs).
- **W8 = A7 NU7** (M). `hayai-consensus/src/{nsm.rs,limits.rs}`, additions in `subsidy.rs`, `funding.rs`. It comes after W2, W3 and W1.
- **W9 = B9 block store by hash** (S), **B2 download window** (L), **B7 fast path** (M), **A9 checkpoints** (M). `hayai-blockstore/src/lib.rs`, `hayai-sync/src/download.rs`, `hayai-validate/src/lib.rs`, `hayai-consensus/src/checkpoints.rs`. B2 comes after W6. B7 comes after W1.
- **W10 = B3 driver, fork choice, reorg, speculative pipeline** (M–L). `hayaid/src/node.rs` (single owner: no other item edits `node.rs` in this phase, and earlier items limit their `node.rs` edits to call-site changes). It comes after W6 and W9.
- **W11 = B10 mempool** (M). `hayai-prepared/src/store.rs`, new `hayai-prepared/src/policy.rs`, `hayaid` `Mempool`. It comes after W0, in parallel to W9.
- **W11c = C3 process-oracle tier and nightly CI** (M). It comes after W3 and W1.

**Phase 3:**
- **W12 = B6 restart during sync** (S–M). It comes after W10.
- **W13 = B11 snapshot** (M). It comes after W1 and W7 (the state record must be final).
- **W14 = C2 completion** (new vectors NU6–NU6.3) (S) and **C4 full-history replay** (M + machine time). It comes after W10.
- **W15 = docs** (S): `docs/consensus.md` regenerated from section 1, `docs/architecture.md` (hayai-consensus, hayai-sync), `docs/hayaid.md` limits, `CHANGES.md`.

Dependency summary: W0 → {W1, W2, W3, W4, W6c}; W2 → W6 → W9(B2) → W10 → {W12, W14}; W1 → {W7, W8, W9(B7), W13}; W3 → W8; W5 and W11 are independent.

---

## 6. Ironwood backend decision

**Evidence.**
- Upstream supports all of the NU6.3 verification (section 0, finding 1). `BatchValidator::add_bundle` is byte-identical between `orchard` 0.15.5 and `zakura-orchard` 2.2.0, except the RNG trait (diff of `src/bundle/batch.rs`). hayai already compiles `OrchardCircuitVersion::PostNu6_3` on both backends.
- Upstream does not support NU7: no branch id, no heights (0.10.5, 0.10.6, and the librustzcash checkout 330e4c0 of 2026-08-23). A newer upstream (`orchard` 0.16.0, `zcash_primitives` 0.31.0-pre.0, `zcash_protocol` 0.11.0-pre.0) exists on crates.io, and `zebra-chain` 14.0.0 pins it. Its source is not available locally for inspection.

**Options.**
1. Require the zakura backend from NU6.3. This is not necessary, because upstream verifies NU6.3. This option loses the independent backend on the live rules.
2. Increase the upstream pins to 0.16 / 0.31-pre. This is possible later. The pre-release pins (`=0.31.0-pre.0`) and `sapling-crypto` 0.9 change APIs. No evidence shows that they have NU7 heights.
3. Implement Ironwood for each backend. This is not necessary for NU6.3, because the code is backend-neutral.

**Recommendation.** Implement Ironwood one time, backend-neutral, on the current pins (W1). Keep NU7 behind the facade helpers (`nu7_branch()`, `nu7_activation()`), which return `None` on upstream. On upstream, a node at or after the NU7 height of a network refuses to start with a clear error ("NU7 needs the zakura backend"), never with a silent rule skip. Examine option 2 again when upstream publishes NU7 parameters. Both backends stay in CI for every epoch up to NU6.3, and the differential between them is a conformance check at no cost.

---

## 7. Open questions for the project owner

1. **NU7 authority.** Testnet NU7 at 4,465,026 and branch `0x77190ad9` come from the Zakura forks and the "valargroup deploy-nu7" draft. Is this the NU7 of the public Testnet, or a schedule of Zakura only? If it is public, is the zakura backend acceptable as the only build with NU7 until upstream has NU7?
2. **Default backend.** Which default backend do the Mainnet/Testnet binaries use: `upstream` (independence) or `zakura` (speed, NU7)?
3. **Checkpoint policy.** Confirm the policy: a checkpoint list as a speed option, full header validation always, and a mandatory hash-only range below Canopy (no BCTV14). Or is a true verification from genesis (A5b, L+) a goal?
4. **Sapling keys.** Which option applies: embed the 2 verifying keys in the repository (recommended), use the `bundled-prover` feature, or keep the parameter files?
5. **Finality depth.** 100 (today) or 1,000 (Zebra/Zakura since June 2026)?
6. **Snapshot trust.** Does the release embed the manifest hash, does only the operator supply it, or both? Is background validation from genesis necessary?
7. **Fork-choice tie-break.** First seen (zcashd) or lowest hash (Zebra/Zakura)?
8. **Reference for parity.** Zebra 16.0.0 sources are not local. Is Zakura HEAD the primary reference, with a later confirmation against Zebra? Does the plan add `zebra-consensus` 16.0.0 as a dev-dependency of the fuzzer (this needs a download)?
9. **Regtest rules.** Do the Regtest rules follow the Regtest waivers of Zakura (unshielded coinbase spends, PoW waiver)?
10. **Mempool policy.** The architecture document says that the mempool policy is "deliberately incompatible". For a general node, does hayai adopt ZIP 317/401 and zcashd standardness exactly, or keep a miner-oriented policy?
11. **RPC scope.** A "feature-complete node" beyond the mining RPC (`sendrawtransaction`, `getblock`, `getrawtransaction`, wallet-facing lightwalletd RPCs) is outside packages A and B. Is it in scope?
12. **Details that need the spec or ZIP text:**
    - the Testnet genesis hash constant;
    - the coinbase maturity and expiry at NU7 (Zakura keeps 100 and 500,000,000);
    - the pre-Canopy Ed25519 acceptance rules;
    - whether the Orchard v5 parser enforces the canonical proof length before NU6.3;
    - the Zakura RPC field for the Ironwood treestate;
    - the count of distinct Sprout roots;
    - the Mainnet chain size for the sync estimate.

---

### Critical Files for Implementation
- crates/hayai-state/src/check.rs
- crates/hayai-prepared/src/prepare.rs
- crates/hayai-prepared/src/shielded.rs
- crates/hayai-state/src/history.rs
- crates/hayaid/src/node.rs

Related files:

- crates/hayai-state/src/lib.rs
- crates/hayaid/src/params.rs
- crates/hayaid/src/headers.rs
- crates/hayai-relay/src/header_check.rs
- crates/hayai-net/src/relay.rs
- crates/hayai-crypto/src/lib.rs

Reference sources:

- ../zakura-src/zakura/crates/zakura-consensus/src/block/check.rs
- .../zakura-consensus/src/transaction/check.rs
- .../zakura-consensus/src/transaction.rs
- .../zakura-state/src/service/check.rs
- .../zakura-header-chain/src/validation/contextual/adjusted_difficulty.rs
- .../zakura-chain/src/parameters/network/subsidy.rs
- .../zakura-chain/src/parameters/network_upgrade.rs
- ../zebra/zebra-test/src/vectors/

---

## Owner decisions (2026-10-04)

- **Q1, Q2 (NU7).** Phase 1 has no NU7 work. `hayai-consensus` holds one rule set for each network upgrade behind one interface, so that NU7 is one more rule set when a backend provides it. At an NU7 activation height that the build does not support, the node stops with a clear error. W8 and W9 stay in the plan after Phase 1. The default backend does not change.
- **Q3, Q4 (checkpoints and pre-Sapling history).** Do as Zakura does: use a dense checkpoint list, and verify the blocks below the last checkpoint by hash.
- **Q5 (finality depth).** 1,000 blocks, as in Zebra and Zakura.
- **Q6 (Sapling keys).** Embed the 2 verifying keys in the binary.
- **Q7 (fixtures from the network).** No. Do not store real Mainnet or Testnet blocks as fixtures. Tests for NU6 and later use generated data, the `zcash_history` vectors and shadow runs.
- **Q8 (mempool policy).** Apply every relevant ZIP (ZIP 317, ZIP 401) and zcashd standardness.
- Q9 to Q12 are open. The recommendation of the plan applies until the owner decides.
