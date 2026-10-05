# Consensus rules: coverage

Status values: **done** (implemented and tested), **partial** (implemented with a listed gap),
**deferred** (not in this phase; the validator returns an `Unsupported` error and does not
accept the block).

## Header

| Rule | Crate | Status |
|---|---|---|
| Header serialization (1487 bytes on Mainnet and Testnet, 177 bytes on Regtest), block hash | hayai-wire | done |
| `bits` encode a target (not negative, not zero, no overflow) and the target is at most the proof-of-work limit of the network | hayai-wire (`check_target`), hayai-consensus (`header`) | done |
| Hash ≤ target (difficulty filter) | hayai-wire (`check_pow`), hayai-consensus (`header::check_proof_of_work`) | done. Regtest waives it (Zakura's `disable_pow`) |
| Equihash solution with the network's parameters: (200, 9) on Mainnet and Testnet, (48, 5) on Regtest (`PowParams`) | hayai-wire (upstream `equihash`), hayai-consensus (`header::check_proof_of_work`) | done. Regtest checks the solution length only (Zakura's `disable_pow`) |
| Difficulty adjustment: `bits` equals `ThresholdBits` (specification §7.7.3: mean target of 17 blocks, median times of 11 blocks, damping factor 4, bounds 16 % up and 32 % down, target spacing 150 s before Blossom and 75 s from Blossom, the limit up to height 17) | hayai-consensus (`difficulty::expected_bits`) | done. Regtest has no expected value: any target at or below the limit (Zakura's `disable_pow`) |
| Testnet minimum difficulty (ZIP 205, ZIP 208): from height 299,188 a block more than 6 target spacings after its parent has the limit | hayai-consensus (`difficulty::expected_bits`) | done |
| Time > median-time-past of the previous 11 blocks | hayai-consensus (`header::check_contextual`) | done |
| Time ≤ median-time-past + 90 min, from height 2 on Mainnet and Regtest and from height 653,606 on Testnet | hayai-consensus (`header::check_contextual`) | done |
| Time ≤ clock of the node + 2 h (a local rule, not a consensus rule) | hayai-consensus (`header::check_local_time`) | done: the header checks of hayai-relay and hayaid apply it; block validation and the replay at a restart do not |
| Version ≥ 4 as a signed 32-bit integer: a version below 4 or with the high bit set is not valid (zcashd reads `nVersion` as `int32_t`; Zakura `zakura-chain/src/block/serialize.rs:51`) | hayai-consensus (`header::check_version`, called by `header::check_contextual` and by the header chain of hayai-sync) | done |
| A header at a checkpoint height has the checkpoint hash; no branch leaves the best chain below the last checkpoint that the best chain reached | hayai-consensus (`Network::checkpoints`), hayai-sync (`HeaderChain`) | done (section Checkpoints) |
| Header field at offset 68: final Sapling root (Sapling, Blossom), ZIP 221 `hashChainHistoryRoot` of the parent's tree (Heartwood, Canopy; all zeros in the Heartwood activation block), ZIP 244 `hashBlockCommitments` (from NU5) | hayai-state (`history`) | done: checked whenever the history tree after the parent is known. The tree (MMR peaks, upstream `zcash_history`) is in every layer and in the base, and a new tree starts at each upgrade activation. The rule set names the tree version (`RuleSet::history`): version 1 (Heartwood, Canopy), version 2 with the Orchard root and count (NU5 to NU6.2), version 3 with the Ironwood root and count (from NU6.3). Seeding rule: before Heartwood the tree is empty and needs no seed; a base at or after Heartwood starts with an unknown tree, and the node seeds it with `HistoryState::from_peaks` (peaks cannot be derived from headers). A block on an unknown tree is not checked and its layer records `history: None`. An upgrade without a rule set returns `HistoryError::Unsupported` |

Where the header rules run. `hayai_consensus::header::check_header` is the one function: the
contextual rules (`check_contextual`), the local time rule when the caller gives a clock, and
the proof of work (`check_proof_of_work`).

| Path | Rules | Context |
|---|---|---|
| Relay header check (`hayai_relay::StandardHeaderCheck`) | `check_header` with the clock | `HeaderContext::parent` |
| hayaid header check (`NodeHeaderCheck`: relay, `submitblock`, shadow follower) | `check_header` with the clock | the header index: committed and pending headers |
| Block validation (`validate_block`, `build_layer`, `commit_prebuilt`) | `check_contextual` (`hayai_validate::check_block_header`) | the view: `ChainView::recent_times`, `difficulty_context` |
| Header chain (`hayai_sync::HeaderChain::accept_headers`) | `check_version`, `check_proof_of_work`, then the `HeaderRules` of the node. A chain whose work is 2^256 or more is `HeaderRuleError::WorkOverflow` (possible on Regtest only) | the ancestors of the branch |
| Replay at a restart | `check_proof_of_work`, then block validation | the view |
| Checkpoint path (`apply_checkpointed`) | none: the header chain applied the rules to the header before the download | none |

The contextual rules read the times of the 28 blocks before the header and the `bits` of the
17 blocks before it (fewer near the genesis block). A context that holds fewer blocks than a
rule reads gives the result `HeaderVerdict::ContextTooShort` with the rules that did not run.
That result is never a pass:

- A full node starts at the genesis block and has the whole context. It rejects such a
  header (`HeaderPolicy::Enforce`).
- A shadow node starts from the state of upstream. Its seed holds the time and the `bits`
  of the start block and of the 27 blocks before it, in the header index and in the base of
  the view. Every header rule therefore runs from the first block after the start, in the
  header check and in block validation. A seed without these blocks fails. The policy of a
  shadow node is `HeaderPolicy::TrustShortContext`: a header whose context is too short
  passes the rules that did not run and is counted in `hayai_shadow_trusted_bits_total`.
  With a whole seed the counter reads 0.
- `HeaderPolicy::GeneratedBlocks` runs no header rule. Only the generated blocks of
  hayai-bench use it: their headers have no proof of work.

## Block body

| Rule | Crate | Status |
|---|---|---|
| Merkle root matches transactions | hayai-wire, hayai-validate | done |
| Block size ≤ 2,000,000 bytes | hayai-wire | done |
| Coinbase first and only first; height in coinbase scriptSig | hayai-state | done |
| Block subsidy schedule: slow start, Blossom, halvings (Mainnet, Testnet, Regtest) | hayai-consensus (`subsidy`) | done |
| Founders' reward output before Canopy; funding stream outputs from Canopy (exact value and script, one coinbase output for each); ten lockbox disbursement outputs in the NU6.1 activation block (ZIP 271) | hayai-consensus (`coinbase::CoinbaseTerms::check`), called by hayai-state for every block of full validation | done. No block reaches the founders' reward check in a node: each height with a founders' reward is at or below the mandatory checkpoint (Mainnet and Testnet), and Regtest has no founders' reward. The conformance tests compare the amounts and the addresses with Zakura and Zebra |
| Coinbase value from NU6 = subsidy − deferred + lockbox disbursement + fees (ZIP 236); before NU6 at most that value. The value is the transparent outputs minus the Sapling, Orchard and Ironwood value balances | hayai-consensus (`coinbase::CoinbaseTerms::check`), called by hayai-state for every block | done |
| Deferred pool: plus the lockbox share, minus the disbursement, never negative | hayai-consensus (`coinbase::CoinbaseTerms::deferred_pool_after`), hayai-state | done. A node that starts above the genesis block takes the pool from its start state: a shadow seed without a pool above zero from NU6 fails, and a state record without the pool is refused |
| Sigop limit 20,000 (legacy count plus P2SH redeem-script count, as zcashd) | hayai-prepared (count), hayai-state (limit) | done |
| Block limits of the rule set of the block's height (`hayai_consensus::rules_at`): sigops, Orchard actions, Ironwood actions, Sapling spends and outputs | hayai-consensus, hayai-state | done: every rule set up to NU6.3 has the sigop limit only |
| NU7 per-block shielded limits (ZIP 218: 330 Orchard actions, 330 Ironwood actions, 300 Sapling spends and outputs, a total shielded cost of 330) | hayai-consensus (`BlockLimits::NU7`), hayai-state (`add_totals`) | done with the NU7 rule set (section NU7) |
| No txid twice in a block (CVE-2012-2459: a body with a txid twice can have the merkle root of a valid block; Zakura `zakura-consensus/src/block/check.rs:523-559`) | hayai-wire (`duplicate_txid`), hayai-validate (with the merkle root, full path and checkpoint path), hayai-state (prebuilt path and `contextual_check`) | done: `ContextError::DuplicateTxid`. The error names a fault of the body: the header can be the header of a valid block |
| Parent hash is the view's tip | hayai-state | done |

## Transaction, context-free

| Rule | Crate | Status |
|---|---|---|
| Parse with the branch id of the block height | hayai-wire | done |
| Transaction version and consensus branch id valid for the epoch | hayai-prepared | done: the versions come from `RuleSet::tx_versions` (v4 from Sapling, v5 from NU5, v6 from NU6.3). v1 to v3 in their epochs are `Unsupported` in full validation, as in Zakura's verifier (`WrongVersion`): every block that holds one is at or below the mandatory checkpoint, and the checkpoint path applies it (Checkpoints) |
| A bundle only for a pool of the epoch (`RuleSet::pools`) | hayai-prepared | done |
| Orchard soft fork: no Orchard bundle from Mainnet height 3,363,426 and Testnet height 4,048,500 until the NU6.2 activation (Zakura `zakura-chain/src/parameters/network.rs:26,31,373-378`, `zakura-consensus/src/transaction.rs:484-493`) | hayai-consensus (`rules_at` gives the NU6.1 rule set with the Orchard pool off), hayai-state (`check_pools` on the rule set of the height) | done for blocks. The mempool admission of hayaid uses the rule set of the branch and does not apply the range (plan item B10, the node driver item) |
| Some source of funds: a transparent input, a JoinSplit, a Sapling spend, or Orchard or Ironwood actions with `enableSpends = 1`. Some sink of funds: a transparent output, a JoinSplit, a Sapling output, or Orchard or Ironwood actions with `enableOutputs = 1`. No duplicate inputs | hayai-prepared | done |
| Coinbase has no JoinSplit and no shielded spends (Sapling spends, Orchard `enableSpends`, Ironwood `enableSpends`) | hayai-prepared | done |
| Coinbase has no Orchard bundle from NU6.3 (`RuleSet::coinbase.orchard_bundle`): a shielded coinbase output is an Ironwood output | hayai-prepared | done |
| Coinbase scriptSig length 2..=100; non-coinbase inputs have non-null prevouts | hayai-prepared | done |
| Coinbase shielded outputs (ZIP 213): none before Heartwood; from Heartwood every Sapling, Orchard and Ironwood output decrypts with the zero outgoing viewing key and gives the note commitment; Sapling lead byte 0x01 in Heartwood and 0x02 from Canopy (ZIP 212, no grace period), Orchard 0x02, Ironwood 0x03 | hayai-prepared (`coinbase.rs`, upstream `zcash_note_encryption`) | done |
| Expiry height below `TX_EXPIRY_HEIGHT_THRESHOLD` | hayai-prepared | done |
| Transparent script and signature verification (ZIP 244 / 243 sighash; flags P2SH, CHECKLOCKTIMEVERIFY) | hayai-prepared (upstream `zcash_script` 0.6 Rust interpreter) | done |
| Transparent value sums, no overflow, fee ≥ 0 | hayai-prepared | done |
| Orchard bundle: proofs, spend-auth and binding signatures under the circuit of the epoch (a v5 bundle of NU6.3 uses the NU6.3 key) | hayai-prepared (upstream `orchard` BatchValidator, bisection on failure) | done |
| Orchard flags: actions need `enableSpends` or `enableOutputs`; reserved bits are 0; bit 2 is 0 in every Orchard bundle (`enableCrossAddress = 0` from NU6.3) | hayai-prepared; the upstream parser for the bit rules (`Flags::from_byte`) | done |
| Orchard pool from NU6.3: `valueBalanceOrchard` ≥ 0 (no value enters the pool) | hayai-prepared | done |
| Ironwood bundle (v6, from NU6.3): proofs, spend-auth and binding signatures under the NU6.3 circuit, with the v6 sighash; a bundle with `enableCrossAddress = 0` is verified against the restricted instance | hayai-prepared (upstream `orchard` BatchValidator, the group of the NU6.3 key) | done |
| Ironwood flags: actions need `enableSpends` or `enableOutputs`; bits 0 to 2 are valid, the other bits are 0 | hayai-prepared; the upstream parser for the bit rules | done |
| Orchard and Ironwood proof of the canonical length for the action count (from NU6.2) | upstream parser (`Bundle::try_from_parts`) | done (test in hayai-prepared) |
| Fee: the Ironwood value balance is part of the transaction value | hayai-prepared | done |
| Sapling bundle: Groth16 proofs, spend-auth and binding signatures; `cv`, `rk`, `epk` not of small order | hayai-prepared (upstream `sapling-crypto` BatchValidator; `cv` in the upstream parser) | done: the two verifying keys are in the binary (`sapling_vk/`), and no parameter file is read |
| No duplicate nullifier within a transaction (Sprout, Sapling, Orchard, Ironwood; each pool has its own set) | hayai-prepared | done |
| JoinSplit values: `vpub_old` or `vpub_new` is zero. From Canopy `vpub_old` is zero (ZIP 211, `RuleSet::sprout_deposit`). Each value is in `0..=MAX_MONEY` (the upstream parser) | hayai-prepared (`sprout.rs`) | done |
| Fee: `vpub_new` of a JoinSplit is an input of the transaction value and `vpub_old` is an output. The outputs with every `vpub_old`, and every `vpub_new`, each sum to at most `MAX_MONEY` | hayai-prepared | done |
| JoinSplit of a v4 transaction (from Sapling): Groth16 proof with the public inputs anchor, `hSig`, nullifiers, MACs, commitments, `vpub_old`, `vpub_new` | hayai-prepared (upstream `zcash_proofs::sprout::verify_proof`, one proof at a time as Zakura) | done: the verifying key is in the binary (`sprout_vk/`) |
| JoinSplit signature: `joinSplitPubKey` is the encoding of a point, and `joinSplitSig` is a valid Ed25519 signature of the shielded sighash (ZIP 243, `SIGHASH_ALL`), by the ZIP 215 rules | hayai-prepared (`ed25519-zebra`, the crate of Zakura) | done. ZIP 215 applies at every height, as in Zebra and Zakura: ZIP 215 activates with Canopy, and no block before Canopy has full validation |
| JoinSplit of a v2 or v3 transaction (before Sapling): BCTV14 proof | hayai-prepared | no verifier, as Zebra and Zakura. `ScopedBatch::add` returns `Unsupported` for a BCTV14 proof, and `draft` returns `Unsupported` for v1 to v3. The checkpoint path applies these blocks |

## Transaction, contextual

| Rule | Crate | Status |
|---|---|---|
| Inputs exist and are unspent in the chain view (including in-block ordering) | hayai-state | done |
| Spent coin matches the coin the transaction was prepared against (value, script) | hayai-state | done |
| Coinbase maturity (100 blocks, `hayai_consensus::COINBASE_MATURITY`) | hayai-state | done |
| A transaction spending a coinbase output has no transparent outputs (zcashd `bad-txns-coinbase-spend-has-transparent-outputs`), on Mainnet and Testnet. Regtest does not have the rule (`NetworkParams::coinbase_must_be_shielded`; Zakura `zakura-chain/src/transaction.rs:552-564`, `parameters/network/testnet.rs:1426`) | hayai-state | done |
| Expiry height: `expiry == 0 || height <= expiry`; coinbase expiry equals the height from NU5 (ZIP 203) | hayai-state | done |
| Lock time (zcashd `IsFinalTx` with the block's height and time) | hayai-state | done |
| No duplicate nullifier within the block, the non-finalized layers, or the finalized set (Sprout, Sapling, Orchard, Ironwood) | hayai-state | done |
| Anchors (Sapling, Orchard, Ironwood) are roots of some earlier block's final treestate; the empty-tree root is always valid | hayai-state | done |
| Sprout anchors: the anchor of a JoinSplit is the final Sprout treestate of an earlier block, the empty tree, or the output treestate of an earlier JoinSplit of the same transaction (Zakura `sprout_anchors_refer_to_treestates`, `zakura-state/src/service/check/anchors.rs:230`) | hayai-state (`check_sprout_anchors`) | done. A base that does not know the Sprout state (a shadow seed, a state record before version 4 above the genesis block) refuses a block with a JoinSplit: `ContextError::SproutStateUnknown` |
| Note commitment trees: the commitments of the block, in block order, go to the Sprout, Sapling, Orchard and Ironwood trees. The Ironwood tree has the hash of the Orchard tree (MerkleCRH^Orchard). The Sprout tree has depth 29 and the SHA-256 compression function (`hayai_trees::SproutFrontier`) | hayai-state, hayai-trees | done |
| Chain value pool balances non-negative after each block: transparent, Sprout, Sapling, Orchard, Ironwood, deferred; total of the pools ≤ `MAX_MONEY` (Zakura `ValueBalance::add_chain_value_pool_change`). `vpub_old` enters the Sprout pool and `vpub_new` leaves it | hayai-state | done |
| Coinbase value includes the value that enters the Ironwood pool (`-valueBalanceIronwood`) | hayai-state | done |
| NSM of NU7: fee share of the miner, NSM value balance, seed, reissuance bonus | hayai-consensus (`nsm`, `coinbase`), hayai-state (`block_pools_after`) | done with the NU7 rule set (section NU7) |

## NU7

Reference: the Zakura source (`zakura-chain` 9.0.0, `zakura-consensus`, `zakura-state`,
`zakura-protocol` 2.2.0). Paths are relative to `zakura/crates/`.

Backends. The NU7 rule set exists when the crypto backend has the NU7 branch id
`0x77190ad9`. The `zakura` backend has it. The default `upstream` backend does not:
`zcash_protocol` 0.10.5 `BranchId` has no such value (`src/consensus.rs`,
`impl TryFrom<u32> for BranchId`; the `Nu7` variant is behind `cfg(zcash_unstable = "nu7")`
with the value `0xffffffff`), and `zcash_primitives` 0.30.1 `Transaction::read`
(`src/transaction/mod.rs`, the header read of `read_v5` and `read_v6`) refuses a v5 or v6
transaction with another branch id. The txid and the signature hash also take a
`BranchId`. A node of the default backend stops with `ConsensusError::UnsupportedUpgrade`
when its next block is the first block of NU7.

Heights. Testnet: 4,465,026 (`zakura-chain/src/parameters/constants.rs:80`). Mainnet: none
(`constants.rs:83-112`; `zakura-protocol` `src/consensus.rs:503`). Regtest: none, or the
`nu7` value of `[regtest] activation_heights`.

State values: **done** (implemented, with the test named), **none** (no code; the row
says why no block needs it).

| Rule | ZIP | Zakura | hayai | Test | State |
|---|---|---|---|---|---|
| Consensus branch id `0x77190ad9` | ZIP 259 | `zakura-chain/src/parameters/network_upgrade.rs:242-243` | `hayai_crypto::nu7_branch`, `rules::nu7` | `rules::tests::every_upgrade_with_a_branch_id_has_one_rule_set_with_its_branch`, hayaid `params::tests::the_rules_at_the_nu7_height` | done on `zakura`; stop on `upstream` |
| Transaction version is 5 or 6 | ZIP 2003 | `zakura-consensus/src/transaction.rs:1023-1040` | `RuleSet::tx_versions`, hayai-prepared `check_version` | hayai-prepared `prepare::tests::a_v4_transaction_is_refused_from_nu7` | done |
| No JoinSplit (`SproutBlockJoinSplitLimit` = 0) | ZIP 218, ZIP 2003 | `network_upgrade.rs:309-316`, `zakura-consensus/src/block/check.rs:488-493` | `RuleSet::pools.sprout` is false; `check_pools` in hayai-prepared and hayai-state | `rules::tests::the_nu7_rule_set_changes_these_rules` | done: the version rule refuses each format with a JoinSplit |
| Block limits: 330 Orchard actions, 330 Ironwood actions, 300 Sapling spends and outputs, shielded cost 330 | ZIP 218 | `network_upgrade.rs:301-327`, `zakura-consensus/src/block/check.rs:451-503` | `BlockLimits::NU7`, hayai-state `add_totals` | hayai-bench `conformance_nu7` (`the_limits_and_the_difficulty_parameters_match_zakura_chain`), `ironwood` (`ironwood_actions_count_for_the_block_limit`), `state` (`block_totals`) | done |
| The same limits on one transaction of the mempool | ZIP 218 (Zakura policy) | `zakura-consensus/src/transaction.rs:474-482` | hayai-prepared `policy::check_block_limits` | `policy::tests::block_limits_boundary` | done |
| The same limits on the template | ZIP 218 | `zakura-rpc/src/methods/types/get_block_template/zip317.rs:300-430` | hayai-template `live::Budget` | `live::tests::shielded_limits_bound_the_selection`, `the_template_follows_the_rules_at_the_nu7_boundary` | done |
| Target spacing 25 s | ZIP 218 | `network_upgrade.rs:257,481` | `DifficultyParams::POST_NU7`, `subsidy::SPACING_ERAS` | `tests/difficulty.rs` (`the_window_and_the_spacing_change_at_nu7`) | done |
| Averaging window 102 blocks; the context of a header is 113 blocks | ZIP 218 | `network_upgrade.rs:285,585`, `zakura-header-chain/src/validation/contextual/constants.rs:26,37-42` | `DifficultyParams::POST_NU7`, `DIFFICULTY_CONTEXT_BLOCKS` | `tests/difficulty.rs` (`generated_chains_across_nu7_match_the_reference`), hayai-bench `conformance_nu7` (`the_expected_bits_across_nu7_match_zakura_header_chain`) | done |
| Testnet minimum difficulty: gap of 18 spacings (450 s) | ZIP 208, ZIP 218 | `network_upgrade.rs:336,522-543` | `DifficultyParams::min_difficulty_gap_spacings` | `tests/difficulty.rs` (`the_window_and_the_spacing_change_at_nu7`) | done |
| Subsidy of one block: `floor(1,250,000,000 * 25 / 150)` after the halvings | ZIP 218 | `zakura-chain/src/parameters/network/subsidy.rs:948-984` | `subsidy::total_subsidy` | `subsidy::tests::the_testnet_schedule_follows_the_nu7_spacing`, hayai-bench `conformance_nu7` (`the_schedule_across_nu7_matches_zakura_chain`) | done |
| Halving index with the 25 s era: an interval has 3 times the blocks | ZIP 218 | `subsidy.rs:523-563` | `subsidy::halving` | the same two tests | done |
| The last funding stream set ends at the third halving: an end above the NU7 height `A` moves to `A + 3 * (end - A)` | ZIP 214 revision 3, ZIP 1016 | `subsidy.rs:388-402`, `network/testnet.rs:1167-1183`, `subsidy/constants/mainnet.rs:288` | `funding::nu7_adjusted_end` | `funding::tests::the_last_testnet_streams_follow_nu7`, hayai-bench `conformance_nu7` (`the_testnet_funding_streams_across_nu7_match_zakura_chain`) | done |
| An address period has 3 times the blocks from NU7 | ZIP 218 | `subsidy.rs:341-370` | `funding::address_period` | `funding::tests::an_address_period_has_three_times_the_blocks_from_nu7`, the same hayai-bench test | done |
| Mainnet recipient of the last stream set from the first address period at or after NU7 (a P2PKH address) | ZIP 2008 | `subsidy/constants/mainnet.rs:196-221`, `zakura-consensus/src/block/check.rs:602-609` | no code | `funding::tests::zip_2008_has_no_code_while_mainnet_has_no_nu7_height` fails when Mainnet gets a height | none: Mainnet has no NU7 height |
| The coinbase gets the fees minus `floor(6 * fees / 10)`; the value rule of ZIP 236 is exact on that share | NU7 deployment draft (`draft-valargroup-deploy-nu7`), ZIP 236 | `subsidy/fees.rs:20-41`, `zakura-consensus/src/block/check.rs:365-381` | `nsm::miner_fee_share`, `CoinbaseTerms::miner_fees` | `coinbase::tests::the_terms_at_the_nu7_boundary`, hayai-bench `conformance_nu7` (`the_nsm_values_match_zakura_chain`, `the_coinbase_terms_across_nu7_match_zakura_chain`) | done |
| NSM value balance: the scheduled issuance minus the total of the chain value pools | zips#1354 | `zakura-chain/src/block.rs:359-416`, `zakura-chain/src/value_balance.rs:416-433` | `nsm::balance`, `subsidy::scheduled_issuance`; no stored value | `nsm::tests::the_balance_is_the_scheduled_issuance_minus_the_pools`, hayai-bench `conformance_nu7` (the scheduled issuance) | done |
| The balance in the block before NU7 is the seed of the network: Testnet 55,768,414,957, Mainnet 36,858,445,520, Regtest any | zips#1354 | `value_balance.rs:377-414`, `subsidy/constants/testnet.rs:27`, `mainnet.rs:44` | `nsm::check_balance`, hayai-state `block_pools_after` | hayai-state `check::tests::the_nsm_rules_apply_from_the_block_before_nu7` | done. Zakura lets a Regtest configuration set a seed; hayai has no such setting |
| From NU7 the balance is not negative | zips#1354 | `zakura-state/src/service/check.rs:46-98` | the same functions | the same test | done |
| NSM reissuance height: the first height after the third halving at which the bonus of a full reserve is below the subsidy. Testnet 7,305,222; Regtest and Mainnet none | halving-preserving NSM draft (`draft-judah-nsm-halving-preserving-issuance`) | `subsidy.rs:609-720` | `nsm::reissuance_height` | hayai-bench `conformance_nu7` (`the_nsm_values_match_zakura_chain`) | done |
| From that height the subsidy is the halving subsidy plus `ceil(balance * 1,375 / 10,000,000,000)` of the balance after the parent | ZIP 234, the same draft | `subsidy.rs:741-797,927-946`, `zakura-consensus/src/block.rs:504-545` | `CoinbaseTerms::after`, `nsm::reissuance_bonus` | `coinbase::tests::the_subsidy_has_the_reissuance_bonus_from_the_reissuance_height`, hayai-bench `conformance_nu7` (`the_subsidy_with_the_reissuance_bonus_matches_zakura_chain`) | done. The template takes the total of the chain value pools after the parent from its tip event (`Tip::issued_supply`), and a tip without it is `ConsensusError::IssuedSupplyUnknown` from that height. Tests: hayai-template `coinbase::tests::the_coinbase_has_the_reissuance_bonus_from_the_reissuance_height`, `live::tests::the_template_has_the_reissuance_bonus_from_the_reissuance_height`, hayai-rpc `getblocktemplate_gives_the_coinbase_of_the_nsm_rules` |
| History tree version 3, `hashBlockCommitments`, Orchard and Ironwood circuit, script flags: those of NU6.3 | ZIP 221, ZIP 244 | `zakura-chain/src/history_tree.rs:142,241`, `zakura-chain/src/block/commitment.rs:142`, `zakura-consensus/src/primitives/halo2.rs:405` | `rules::nu7` takes them from the NU6.3 rule set | `rules::tests::the_nu7_rule_set_changes_these_rules`, hayai-prepared `orchard::tests::circuit_version_follows_the_branch` | done |
| Coinbase maturity (100 blocks), expiry and lock time rules | — | no NU7 change in the Zakura source | no change | — | no rule to add |
| Minimum protocol version of a peer: 170,180 on Testnet and Regtest, 170,190 on Mainnet | ZIP 204 | `zakura-network/src/protocol/external/types.rs:127-131` | hayai-net `protocol::min_peer_version` | hayai-net `protocol` tests | done before this work |

The chain crosses NU7 and the NSM reissuance height in one node test: hayaid
`sync_tests::a_chain_crosses_nu7_at_the_tip_and_during_the_synchronization` (Regtest,
NU6.3 at 104, NU7 at 108, reissuance at 110). The producer builds each block from its
template, one node follows at the tip, and one node synchronizes the chain.

The rules give Regtest no reissuance height. A test names one with
`RegtestConfig::with_test_reissuance_height`, as Zakura does for its tests
(`ParametersBuilder::with_test_nsm_reissuance_height`,
`zakura-chain/src/parameters/network/testnet.rs:1095-1101`). No configuration file sets
the value.

Node values of Zakura that follow the spacing and are not consensus rules have no code in
hayai: the stall interval and the checkpoint lag of the sync progress
(`zakurad/src/components/sync/progress.rs:178,377-392`), the download window
(`zakurad/src/components/sync.rs:164-200`), the mempool crawler and gossip intervals
(`zakurad/src/components/mempool/crawler.rs:82`, `gossip.rs:28`), the `nsm` entry of
`getblockchaininfo` (`zakura-rpc/src/methods.rs:4416`), and the averaging window of
`getnetworksolps` (`zakura-rpc/src/methods.rs:759`).

## Checkpoints

hayai follows Zebra and Zakura: a dense checkpoint list, and a block at or below the last
checkpoint is verified by its hash.

- Lists (`hayai_consensus::Network::checkpoints`): the files `main-checkpoints.txt` and
  `test-checkpoints.txt` of the Zakura repository
  (`crates/zakura-chain/src/parameters/checkpoint/`, revision
  `13779158253cfe315f73eadffb9b4c93c25e82a5`), copied to
  `crates/hayai-consensus/src/checkpoints/`. Mainnet: 14,385 checkpoints, last height
  3,499,045. Testnet: 10,059 checkpoints, last height 4,023,200. The gap between two
  checkpoints is at most 400 blocks. Regtest: the genesis block only. A test compares the
  copies with the files of a Zakura clone beside the repository.
- Header chain: a header at a checkpoint height with another hash is `CheckpointMismatch`.
  The finalized height is at least the last checkpoint at or below the best tip, so a branch
  that leaves the best chain below it is `ForkBelowFinalized`. Each header has the full
  header rules (proof of work, Equihash, difficulty, time): the list does not replace them.
- Checkpoint path (`hayai_validate::apply_checkpointed`), for a block at or below the last
  checkpoint whose header is on the best header chain below a checkpoint that the chain
  reached.

  | Checked | Not checked |
  |---|---|
  | The parent is the tip of the state | Scripts and transparent signatures |
  | The block hash is `expected`, and it is the checkpoint hash at a checkpoint height | Sapling, Orchard, Ironwood and Sprout proofs and signatures |
  | The merkle root of the header matches the transactions, and no txid is in the block twice | Equihash and the contextual header rules (the header chain applied them) |
  | The header commitment (offset 68) to the Sapling root, to the history tree of the parent, and from NU5 to the authorizing data | Coinbase rules and terms, coinbase maturity, ZIP 213 |
  | Each transparent input spends a coin that exists; no outpoint is spent twice in the block | The order of a parent and its child in the block |
  | No nullifier is revealed twice in the block | Nullifiers against earlier blocks, anchors |
  | No value pool is negative, and the total is at most `MAX_MONEY` | Expiry, lock time, the pools of the height, the block limits, the context-free transaction rules |

  The caller supplies `expected`: the hash of the best header chain at the height of the
  block. The function compares the block hash with `expected` and with the checkpoint of
  the height, when the height has one. It does not read the header chain. A height
  between two checkpoints has no checkpoint, so `expected` is the only bond between the
  block and the checkpointed chain there. A caller that passes the hash of the block
  itself as `expected` removes that comparison.

  The state update is complete: coins, nullifiers, the note commitment trees, the history
  tree, the value pools and the header context. A test runs a generated chain through both
  paths and compares the states. A JoinSplit adds its nullifiers, its note
  commitments and its value to the Sprout state, and the path reads no proof of it.
- Mandatory checkpoint (`Network::mandatory_checkpoint_height`): the last block before
  Canopy (Mainnet 1,046,399, Testnet 1,028,499, Regtest 0), as Zakura. Full validation
  (`validate_block`, `build_layer`, `commit_prebuilt`) refuses a block at or below it with
  `BlockError::BelowMandatoryCheckpoint`: such a block has only the checkpoint path.

## Notes

- Script verification uses the Rust interpreter in `zcash_script` 0.6, and not the C++
  `zcash_script` library. ECC maintains the Rust interpreter and tests it against the C++
  implementation. hayai's differential tests against Zakura (C++ interpreter) are the acceptance
  gate for consensus parity. The flags are those of zcashd's `ConnectBlock` and Zakura's verifier
  (`zakura-script/src/lib.rs:173`): `P2SH | CHECKLOCKTIMEVERIFY`.
- The Sapling verifying keys are the first 1,636 bytes of `sapling-spend.params` and the first
  1,444 bytes of `sapling-output.params` (`crates/hayai-prepared/src/sapling_vk/`).
  `scripts/extract-sapling-vk.sh` writes them from files with the BLAKE2b-512 hashes of
  `zcash_proofs`. A test compares them with the parameters of the `wagyu-zcash-parameters`
  crate.
- The Sprout verifying key is the first 1,828 bytes of `sprout-groth16.params`
  (`crates/hayai-prepared/src/sprout_vk/`). `scripts/extract-sprout-vk.sh` writes it from a
  file with the size and the BLAKE2b-512 hash of `zcash_proofs` (`scripts/fetch-params.sh
  --sprout` downloads the file). The file is equal to `sprout-groth16.vk` of Zakura. A test
  pins its hash, and `hayai-bench/tests/sprout.rs` verifies the 5 Groth16 JoinSplits of the
  published block vectors that need no spent coin (Mainnet 419,201 and 903,000, Testnet
  925,483) with it.
- No header commits to the Sprout root. Before Sapling the header field at offset 68 is
  reserved, and hayai does not check it, as Zakura (`zakura-state/src/service/check.rs:272`,
  `PreSaplingReserved`).
- The Sprout treestates are in memory: the base holds the frontier of the final treestate of
  every block that changed the tree, by root (about 1 kB each). hayaid writes the new
  treestates of each flush to `state.log` and reads all of them at a restart.
- The upstream Sapling batch validator applies the canonical point encodings of ZIP 216 at
  every height. ZIP 216 activates with Canopy. Zebra and Zakura do the same, because no block
  before Canopy has a non-canonical encoding.
- The sigop count follows zcashd (`GetLegacySigOpCount` plus `GetP2SHSigOpCount`). Zebra counts
  only the legacy sigops.
- Block validation evaluates lock times against the height and header time of the block itself
  (zcashd `ContextualCheckBlock` with `nLockTimeFlags = 0`). The median-time-past rule applies
  to mempool admission only.
- The cache of context-free results has one entry set per `RuleEpoch { branch_id, script_flags }`.
  An epoch change drops the cache.
- The finalized anchors are an in-memory set per pool (Sapling, Orchard, Ironwood). The set starts with the empty-tree root
  (zcashd's `GetSaplingAnchorAt` / `GetOrchardAnchorAt` treat it as always present). hayaid
  persists the set in `state.log` (the new anchors of each flush) and rebuilds it at a restart
  (`docs/hayaid.md`, Restart).
