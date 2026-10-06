# Mempool policy

The node stores and relays the transactions that the public network relays. `prepare`
(`hayai-prepared`) rejects a transaction that breaks a context-free consensus rule.
`MempoolPolicy::admit` rejects a valid transaction that the next block cannot contain or that
is not standard. `PreparedStore` applies ZIP 401.

The node applies them in `crates/hayaid/src/mempool.rs` to each transaction of a peer, of
this node, and of a block that a reorg disconnected.

Code: `crates/hayaid/src/mempool.rs`, `crates/hayai-prepared/src/policy.rs`,
`crates/hayai-prepared/src/store.rs`,
`crates/hayai-template/src/zip317.rs` (shared fee constants),
`crates/hayai-template/src/live.rs` (template selection).

Local sources: `ZB` = `../zebra` (commit 02f9648), `ZK` =
`../zakura-src/zakura/crates`. No zcashd source is on this machine. A
zcashd name or reject string with "no" in the last column comes from memory of the zcashd
source and needs a check against zcashd.

## Admission order (`Mempool::admit`)

| Step | Check | Refusal (`Reject`) | Score of the peer |
|---|---|---|---|
| 1 | The store does not hold the transaction | `Known` | none |
| 2 | The txid is not on the recently-evicted list (ZIP 401) | `Store(RecentlyEvicted)` | none |
| 3 | `prepare` on the committed tip: context-free consensus rules, inputs, scripts | `Prepare` | 10 points for an invalid script |
| 4 | `MempoolPolicy::admit` (next section), with the next height, the median-time-past of the tip and the rule set of the next height | `Policy` | none |
| 5 | No Orchard bundle in the Orchard-disabled range (`Network::orchard_disabled` of the next height) | `OrchardDisabled` | none |
| 6 | No nullifier of the transaction is in the chain | `NullifierInChain` | none |
| 7 | Each anchor is the tree state of an earlier block (Sprout: or an interstitial state of the transaction) | `UnknownAnchor` | none |
| 8 | Proofs and signatures of the shielded bundles | `Proof` | 100 points |
| 9 | Insert, with the ZIP 401 eviction and the conflict rules | `Store` | none |

The proofs run after the cheap rules. At each new tip the node removes the mined
transactions, the transactions that conflict with the block, and the expired ones
(`remove_expired` with the next height). After a reorg the node empties the store and
admits the transactions of the disconnected blocks, then the transactions that the store
held, through the same steps on the new tip. A transaction whose parent is later in the list
passes in a later round.

## Admission rules (`MempoolPolicy::admit`)

The order is the order of zcashd `AcceptToMemoryPool`.

| Rule | Reject (`PolicyReject`) | Source | Constant | Local source |
|---|---|---|---|---|
| No coinbase | `Coinbase` | zcashd `coinbase` | — | yes: `ZB/zebra-consensus/src/transaction.rs:448` |
| The epoch is the epoch of the next block | `Epoch` | ZIP 244 branch id; hayai store rule | — | hayai rule |
| Not expired: next height <= expiry height, or expiry 0 | `Expired` | ZIP 203; zcashd `tx-overwinter-expired` | — | yes: `ZB/zebrad/src/components/mempool/storage.rs:885` |
| Does not expire soon: expiry height >= next height + 3 | `ExpiringSoon` | zcashd `TX_EXPIRING_SOON_THRESHOLD`, `tx-expiring-soon` | 3 blocks | no. Zebra and Zakura do not have the rule |
| scriptSig size | `ScriptSigSize` | zcashd `IsStandardTx`, `scriptsig-size` | 1,650 bytes | yes: `ZB/zebrad/src/components/mempool/storage/policy.rs` |
| scriptSig is push-only | `ScriptSigNotPushOnly` | zcashd `IsStandardTx`, `scriptsig-not-pushonly` | — | yes: `ZB .../mempool/storage.rs` (`reject_if_non_standard_tx`) |
| Output script is P2PKH, P2SH, P2PK, multisig or `OP_RETURN` | `ScriptPubKey` | zcashd `IsStandard`, `scriptpubkey` | multisig: at most 3 keys | yes: same function |
| `OP_RETURN` script size | `DataCarrierSize` | zcashd `MAX_OP_RETURN_RELAY` (`-datacarriersize`) | 83 bytes | yes: `ZB .../mempool/config.rs` |
| No bare multisig output | `BareMultisig` | zcashd `bare-multisig` | `permit_bare_multisig = false` | Zebra: yes. zcashd default of `-permitbaremultisig`: no (Bitcoin Core permits by default) |
| No dust output (not for `OP_RETURN`) | `Dust` | zcashd `CTxOut::IsDust`, `dust` | 3 x (100 x (output size + 148) / 1000) zatoshis; 54 for P2PKH | yes: `ZB/zebra-chain/src/transparent.rs:369` |
| At most 1 `OP_RETURN` output | `MultiOpReturn` | zcashd `multi-op-return` | 1 | yes: `ZB .../mempool/storage.rs` |
| Lock time is final at the next height and at the median-time-past of the tip | `NonFinal` | zcashd `CheckFinalTx` with `LOCKTIME_MEDIAN_TIME_PAST`, `non-final` | threshold 500,000,000 | yes: `ZB/zebra-consensus/src/transaction.rs:480-493` |
| A spent coinbase output has 100 confirmations at the next height | `ImmatureCoinbase` | protocol §7.1.2; zcashd `bad-txns-premature-spend-of-coinbase` | 100 blocks | yes: `ZB/zebra-state/src/service/check/utxo.rs:200` |
| A spend of a coinbase output has no transparent output | `UnshieldedCoinbaseSpend` | protocol §7.1.2 | — | yes: same file. Regtest does not have the rule, as Zakura (`MempoolPolicy::coinbase_must_be_shielded`, from `NetworkParams`) |
| Inputs are standard spends | `NonStandardInput` | zcashd `AreInputsStandard`, `bad-txns-nonstandard-inputs` | redeem script that is not standard: at most 15 sigops | yes: `ZB .../mempool/storage/policy.rs` (`are_inputs_standard`) |
| Sigops (legacy plus P2SH) | `TooManySigops` | zcashd `MAX_STANDARD_TX_SIGOPS`, `bad-txns-too-many-sigops` | 4,000 | yes: same file |
| ZIP 317 unpaid actions | `UnpaidActions` | ZIP 317 `block_unpaid_action_limit`; zcashd `-txunpaidactionlimit`, `tx-unpaid-action-limit-exceeded` | 0: a transaction pays the marginal fee of 400 zatoshis for each logical action, and for 2 actions at least. The unpaid action count is max(0, max(2, logical actions) - floor(fee / 400)) | yes: `ZK/zakura-chain/src/transaction/unmined/zip317.rs:81-98` (`unpaid_actions`) and `:166-175` (`mempool_checks` refuses a transaction with an unpaid action) |
| Minimum relay fee | `FeeBelowMinimumRelay` | zcashd `CFeeRate::GetFeeForRelay` | clamp(100 x size / 1000, 100, 800) zatoshis. With the unpaid action limit of 0 the lowest fee is 800 zatoshis, so the rule above decides first | yes: `ZK/zakura-chain/src/transaction/unmined/zip317.rs:177-200` (`mempool_checks`) |
| Shielded counts fit in a block of the next rule set | `AboveBlockLimit` | ZIP 218 (NU7 block limits) | Orchard 330, Ironwood 330, Sapling 300, the three together 330; none before NU7 | yes: `ZK/zakura-consensus/src/block/check.rs:450` |

`MempoolPolicy::of(Network)` differs by network in one value: Regtest does not require
standard transactions (zcashd `fRequireStandard`; not confirmed by a local source). The
script, dust and standard input rules then do not apply.

## Fee and relay constants

The node has the values of Zakura. Reason: equal relay behaviour with the other nodes. A
transaction that a Zakura node relays and mines is a transaction that this node relays and
mines, and the node relays no transaction that a Zakura node refuses for its fee. The
policy, the store and the template use the same `Zip317Params::ZAKURA` and the same
`logical_actions`. `ZKF` = `ZK/zakura-chain/src/transaction/unmined/zip317.rs`, `ZKU` =
`ZK/zakura-chain/src/transaction/unmined.rs`.

| Constant | ZIP 317 value | Zakura value | hayai value |
|---|---|---|---|
| Marginal fee, zatoshis for each logical action | 5,000 | 400 (`ZKF:25`, `MARGINAL_FEE`) | 400 (`Zip317Params::ZAKURA.marginal_fee`) |
| Grace actions | 2 | 2 (`ZKF:28`, `GRACE_ACTIONS`) | 2 (`Zip317Params::ZAKURA.grace_actions`) |
| Standard size of a transparent input, bytes | 150 | 150 (`ZKF:31`) | 150 (`P2PKH_STANDARD_INPUT_SIZE`) |
| Standard size of a transparent output, bytes | 34 | 34 (`ZKF:34`) | 34 (`P2PKH_STANDARD_OUTPUT_SIZE`) |
| Weight ratio cap of the block production | 4 | 13 (`ZKF:37`, `BLOCK_PRODUCTION_WEIGHT_RATIO_CAP`) | 13 (`Zip317Params::ZAKURA.weight_ratio_cap`) |
| Fee that the weight ratio uses for a lower fee, zatoshis | 1 | 1 (`ZKF:43`) | 1 (`Zip317Params::weight_ratio`) |
| Unpaid action limit of a block and of a mempool transaction | 50 | 0 (`ZKF:48`, `BLOCK_UNPAID_ACTION_LIMIT`) | 0 (`BLOCK_UNPAID_ACTION_LIMIT`) |
| Minimum relay fee rate, zatoshis for each 1,000 bytes | none (zcashd: 100) | 100 (`ZKF:56`, `MIN_MEMPOOL_TX_FEE_RATE`) | 100 (`MIN_RELAY_FEE_RATE`) |
| Upper bound of the minimum relay fee, zatoshis | none (zcashd and Zebra: 1,000) | 800 (`ZKF:62`, `MEMPOOL_TX_FEE_REQUIREMENT_CAP`) | 800 (`MIN_RELAY_FEE_CAP`) |
| ZIP 401 cost threshold of a transaction | 10,000 (ZIP 401) | 10,000 (`ZKU:65`) | 10,000 (`MEMPOOL_COST_THRESHOLD`) |
| ZIP 401 low fee penalty, for a fee below the conventional fee | 40,000 (ZIP 401) | 40,000 (`ZKU:73`) | 40,000 (`LOW_FEE_PENALTY`) |
| ZIP 401 cost limit of the mempool | 80,000,000 (ZIP 401) | 80,000,000 (`ZK/zakurad/src/components/mempool/config.rs:70`) | 80,000,000 (`MEMPOOL_TX_COST_LIMIT`) |

The rules with these constants are the rules of Zakura:

- Conventional fee: 400 x max(2, logical actions) zatoshis (`ZKF:67-76`,
  `Zip317Params::conventional_fee`). The logical actions are those of ZIP 317, with the
  Ironwood actions (`ZKF:131-163`, `logical_actions`).
- Unpaid actions: max(0, max(2, logical actions) - floor(fee / 400)) (`ZKF:81-98`,
  `Zip317Params::unpaid_actions`).
- Admission: first the unpaid action rule, then the minimum relay fee (`ZKF:166-200`,
  `MempoolPolicy::check_fee`).
- Eviction weight: the cost plus the low fee penalty when the fee is below the
  conventional fee (`ZKU:484-529`, `PreparedStore::insert`).

A transaction with 1 or 2 logical actions needs a fee of 800 zatoshis.

The Regtest pair measured these cases against zakurad, for one input and outputs of 32
bytes each. Each node gives the same verdict (`the_policy_cases_of_the_regtest_pair_have_
the_verdict_of_zakura` in `policy.rs`, scenario c of the pair):

| Outputs | Logical actions | Fee (zatoshis) | Conventional fee | Verdict |
|---|---|---|---|---|
| 1 | 1 | 0 | 800 | refused: 2 unpaid actions |
| 1 | 1 | 1,000 | 800 | accepted |
| 1 | 1 | 9,999 | 800 | accepted |
| 1 | 1 | 10,000 | 800 | accepted |
| 2 | 2 | 5,000 | 800 | accepted |
| 40 | 38 | 5,000 | 15,200 | refused: 26 unpaid actions |
| 52 | 49 | 5,032 | 19,600 | refused: 37 unpaid actions |
| 60 | 57 | 5,020 | 22,800 | refused: 45 unpaid actions |
| 60 | 57 | 23,020 | 22,800 | accepted |

Rules without a Zcash value:

- Maximum standard transaction size. zcashd `IsStandardTx` has no size rule (not confirmed
  by a local source). Zebra has none. Zakura has a local limit of 250,000 bytes
  (`ZK/zakurad/src/components/mempool/config.rs`). The value 100,000 was the consensus
  limit of a transaction before Sapling. The node has no size rule.
- Replace-by-fee. Zcash has none. A second spend of an outpoint or a second reveal of a
  nullifier is a conflict (`InsertError::Conflict`, `InsertError::NullifierConflict`).
- Orphans. `prepare` fails with `MissingInput` when the coins view does not hold an input.
  The node keeps no orphan pool, as Zebra and Zakura.

Differences from zcashd:

- `AreInputsStandard` in zcashd stops at the first P2SH input whose redeem script is not
  standard and accepts the transaction when that script has at most 15 sigops. The node
  checks every input, as Zebra.
- The transaction version rule of `IsStandardTx` is not in the policy: `prepare` checks
  the version against the rule set.

## Store rules (`PreparedStore`, ZIP 401)

| Rule | Source | Constant | Local source |
|---|---|---|---|
| Cost limit of the store | ZIP 401 `mempooltxcostlimit` | 80,000,000 (`MEMPOOL_TX_COST_LIMIT`) | yes: `ZB/zebrad/src/components/mempool/config.rs` |
| Cost of a transaction: max(serialized size, threshold) | ZIP 401 | 10,000 | yes: `ZB/zebra-chain/src/transaction/unmined.rs:67` |
| Eviction weight: cost + penalty when fee < conventional fee (400 zatoshis for each action) | ZIP 401, ZIP 317 | 40,000 | yes: `ZK/zakura-chain/src/transaction/unmined.rs:484-529` |
| Eviction: weighted random selection, the new transaction is a candidate | ZIP 401 `EvictTransaction` | — | yes: `ZB .../mempool/storage/verified_set.rs` (`evict_one`) |
| Recently evicted txids are refused | ZIP 401 `RecentlyEvicted` | 60 min, 40,000 entries | yes: `ZB .../mempool/storage.rs:51`, `eviction_list.rs` |
| The list holds the txid, not the wtxid | ZIP 401 | — | yes: `ZB .../mempool/storage.rs` (`RandomlyEvicted`) |
| Expiry at each new tip: remove when next height > expiry height | ZIP 203 | — | yes: `storage.rs:885` |

Differences from ZIP 401:

- An ancestor of the new transaction is not a candidate of the selection. Its eviction
  removes the new transaction too.
- The descendants of a victim leave with it. Only the selected victim goes on the
  recently-evicted list, as in Zakura.

## Template selection (`LiveTemplate`, ZIP 317 block production)

| Step of ZIP 317 | Template | Local source |
|---|---|---|
| `weight_ratio` = min(max(1, fee) / conventional fee, `weight_ratio_cap`) | `Zip317Params::weight_ratio` with the cap 13 of Zakura, fixed point with 32 fractional bits (Zakura: `f32`) | yes: `ZK/zakura-chain/src/transaction/unmined/zip317.rs` (`conventional_fee_weight_ratio`) |
| Pass 1: each candidate that pays the conventional fee, one time. Add it when the block stays in the size limit and the sigop limit | The candidates with a weight ratio of 1 or more come first in the order | yes: `ZK/zakura-rpc/src/methods/types/get_block_template/zip317.rs` |
| Pass 2: each other candidate, one time. Add it when the block stays in the two limits and holds at most `block_unpaid_action_limit` unpaid actions | The candidates with a weight ratio below 1 follow. The budget is 0 unpaid actions (`BLOCK_UNPAID_ACTION_LIMIT`), so the pass adds no candidate with an unpaid action; the mempool admits none | yes: same file (`BlockTemplateLimits::try_add`), with the value 0 of Zakura and Zebra. ZIP 317 gives 50 as the default |
| Size limit | 2,000,000 bytes minus the header, the transaction count and the coinbase with the largest scriptSig | yes: same file (`block_template_overhead_bytes`) |
| Sigop limit | 20,000 (`BlockLimits::sigops` of the rule set of the height) minus the sigops of the coinbase | yes: same file (`BlockTemplateLimits::initial`) |
| A candidate that does not fit | It leaves the candidates. The pass continues with the next one | yes: same file |

Differences from ZIP 317:

- The pick is not random. ZIP 317 picks the next candidate of a pass at random, with a
  probability in proportion to its weight ratio. The template takes the candidates of a
  pass in the order of the weight ratio, highest first, then smallest size, then wtxid. The
  compact relay and the template lane need the same selection for the same set on every
  instance (`docs/protocol-template-push.md`, Selection).
- Unmined parents. ZIP 317 does not specify them. A transaction is a candidate only when
  the block holds all its unmined parents. When its turn in the order comes before that,
  the template tries it immediately after the last parent, in either pass, with the same
  limits. Zakura adds such a child only when it pays the conventional fee.
- Limits of the rule set. ZIP 317 names the size limit and the sigop limit. The template
  also applies the shielded limits of the rule set of the height (`BlockLimits`; ZIP 218
  from NU7: Orchard 330, Ironwood 330, Sapling 300, the three together 330). A rule set
  before NU7 has none.

## Relay

| Behaviour | Source | Local source |
|---|---|---|
| Announce a new transaction once, when the store accepts it | zcashd, Zebra | yes: `ZB/zebrad/src/components/mempool/gossip.rs` |
| Answer a `mempool` message with the ids of the store, without the transactions that expire soon (`TxLookup::for_each_relay_id`, ZIP 204) | zcashd | answer: yes (`ZB/zebrad/src/components/inbound.rs:560`). Filter on expiry: no |
| v5 and later transactions are announced by wtxid (`MSG_WTX`), earlier ones by txid | ZIP 239 | hayai-net `tx_inv_item` |
| No rebroadcast from the mempool | zcashd rebroadcasts only the transactions of its wallet; Zebra has no rebroadcast | Zebra: yes. zcashd: no |
