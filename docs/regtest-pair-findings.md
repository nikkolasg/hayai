# Regtest pair: findings

Date: 2026-10-05. Scope: each disagreement and each failure of the first runs of one hayaid
against one zakurad (`docs/regtest-pair.md`), with its cause, its owner and the steps to
reproduce it. Zakura commit `13779158253c`. Paths of Zakura are relative to its `crates`
directory.

## Summary

| Id | Subject | Wrong side | State |
|---|---|---|---|
| F1 | NU6.1 activation block without a lockbox disbursement | Configuration that hayai could not state (hayai-consensus) | Fixed |
| F2 | No answer to `getblocks` | hayai (hayai-net) | Fixed |
| F3 | More than 16 blocks in one `getdata` | hayai (hayai-sync) | Fixed |
| F4 | Announcement of a block to legacy peers before its validation | hayai (hayai-net, hayaid) | Fixed |
| F5 | `curtime` and `mintime` of `getblocktemplate` | hayai (hayai-rpc) | Fixed |
| F6 | No query method in the RPC server of hayaid | hayai (hayai-rpc, hayaid) | Fixed |
| F7 | No `mempool` request to a peer | hayai (hayai-net) | Fixed |
| F8 | Protocol version below the NU7 minimum | hayai (hayai-net) | Fixed |
| P1 | Mempool fee policy | Difference of policy, not of consensus | Fixed: hayai has the values of Zakura |
| Z1 | A Zakura node that stops loses its newest blocks | Zakura behaviour | Recorded |
| Z2 | No commit trace rows with the legacy stack | Zakura behaviour | Recorded |
| Z3 | `submitblock` verdict for an unknown parent | Wording | Recorded |

No block that both nodes validated had 2 verdicts, except the block of F1.

## Runs of 2026-10-05

Each run has the code with the fixes F2 to F7. `default` is the hayaid of the default
crypto backend. `zakura` is the hayaid of the Zakura backend.

| Scenario | Backend | Checks | Failed | Result |
|---|---|---|---|---|
| a (with f) | default | 69 | 0 | Same tip and state at each 20th height up to 320; hayaid resumes after SIGINT and SIGKILL; zakurad resumes |
| b | default | 73 | 0 | zakurad accepts each block of hayaid up to 321, with Orchard and Ironwood transactions |
| c | default | 28 | 0 | The other node mines the 4 relayed transactions; 9 policy cases |
| d | default | 14 | 0 | 6 reorgs of depth 1, 3 and 10, 3 for each winner |
| e | default | 28 | 0 | Both nodes refuse the 20 invalid blocks; no ban; both nodes accept the 2 valid blocks |
| nu61 | default | 4 | 0 | The result of F1 |
| g | default | 15 | 0 | 30 min, 622 blocks, no failed round, 12 equal state comparisons |
| a, b, e | zakura | 170 | 0 | The same result as the default backend |
| c, after the change of the unpaid action limit | default | 26 | 0 | P1 |
| nu61, after the fix of F1 | default | 14 | 0 | zakurad accepts block 100 of hayaid with a disbursement of 10 ZEC and the funding streams; the same state at the heights 99, 100 and 110 |
| e, with the funding streams | default | 30 | 0 | Both nodes refuse the 21 invalid blocks, one of them with a wrong funding stream output; both nodes accept the 2 valid blocks |
| c, after the fix of P1 | default | 25 | 0 | 4 relayed transactions; 9 policy cases with the same verdict on both nodes |
| nu7 | zakura | 56 | 0 | NU7 at height 250: hayaid follows zakurad across the activation; zakurad accepts 19 blocks of hayaid after it, 4 of them with 2 transactions of 20,000 zatoshis fee each; the same state at each compared height |

zakurad follows a series of 99 blocks of hayaid in 9 s to 12 s (scenario b). It reads them
in the rounds of its block sync. A single block of hayaid is the tip of zakurad after 2.4 ms
(median, scenario g).

## F1: NU6.1 activation block without a lockbox disbursement

- Zakura: `zakura-consensus/src/block/check.rs`, `subsidy_is_valid`. At the NU6.1 activation
  height of each network the list `lockbox_disbursements` of the network must not be empty.
  If the list is empty, the block is invalid
  (`missing lockbox disbursements for NU6.1 activation block`).
  Each entry must be an output of the coinbase. A block without a subsidy has no such rule.
- hayai before the fix: Regtest and a configured Regtest had no disbursement and no
  funding stream, and the activation block needed no output.
- Result before the fix, with the same activation heights and no disbursement in the
  configuration of Zakura: hayaid accepted block 100, zakurad refused each block at height
  100. zakurad cannot mine that block itself (`generate`: `block was rejected`). The chain
  of zakurad ends at 99.
- A second activation at the same height does not help: with NU6.1 and NU6.2 at height
  100 Zakura applies the same rule.
- Fix, hayai-consensus: `RegtestConfig::with_lockbox_disbursements` and
  `RegtestConfig::with_funding_streams` take the values with the meaning of
  `RegtestParameters` of Zakura. `CoinbaseTerms` has the outputs. Thus the coinbase check
  and the template use them. The deferred pool pays the disbursements.
- Fix, the rule of Zakura: for a network without a disbursement, the terms of the NU6.1
  activation height are the error `ConsensusError::NoLockboxDisbursement`. This applies
  while the block has a subsidy. The block validation refuses each block at that height,
  and the template has no coinbase for it. Both nodes refuse the same blocks.
- Fix, hayaid: `[regtest]` has the keys `lockbox_disbursements` and `funding_streams`. A
  network with an `nu6_1` height and no disbursement is a configuration error at the
  start. The chain of such a network ends below that height on each node. hayaid cannot
  make a template on the last block of that chain. zakurad starts with such a configuration
  and stops at the block before that height.
- Differences of the configuration that stay:
  - hayai needs a `height_range` and `recipients` in each funding stream entry. Zakura
    takes the Testnet values for an absent key.
  - hayai has no `extend_funding_stream_addresses_as_required`.
  - hayai refuses at its start a recipient with fewer addresses than its range has address
    periods. Zakura stops at the first block of a period without an address.
- Tests: `a_configured_regtest_pays_its_disbursements_at_nu6_1`,
  `a_regtest_without_a_disbursement_has_no_nu6_1_activation_block`,
  `a_configured_regtest_pays_its_funding_streams` (hayai-consensus),
  `the_coinbase_has_the_streams_and_the_disbursements_of_a_configured_regtest`
  (hayai-template), `a_chain_with_configured_funding_streams_crosses_nu6_1` (hayaid),
  scenario nu61 and the funding stream block of scenario e.
- Reproduce on the old code: `--scenario nu61` of the old harness.

## F2: no answer to `getblocks`

- Zakura reads the chain of a legacy peer with `getblocks` (`zakurad/src/components/sync.rs`,
  `obtain_tips`, 3 requests with a timeout of 6 s). hayai-net did not know the message.
- Result 1: zakurad did not get the blocks of hayaid. After `generate 5` on hayaid, zakurad
  was at height 1 after 50 s.
- Result 2: the connection of zakurad accepts no other request while it waits for the
  answer. Its block announcements wait in a queue
  (`zakura-network/src/peer_set/set.rs`, `broadcast_all`). hayaid got a block of zakurad
  late: 40 single blocks at random times, median 3.7 s, maximum 11.6 s.
- Fix: `hayai_net::relay` answers `getblocks` with one `inv` of the hashes of the validated
  blocks after the locator, at most 160. Without such a block the answer is the hash of the
  tip. An empty `inv` is not usable: Zakura counts it as a stall and disconnects the peer
  after 3 (`zakura-network/src/peer_set/stall_tracker.rs`). The run with an empty `inv` had
  one disconnect each 10 s.
- After the fix, the same 40 blocks: median 6 ms in both directions.
- Tests: `getblocks_is_answered_with_the_block_hashes_after_the_locator` (hayai-net),
  scenario b.
- Reproduce on the old code: scenario b; zakurad does not follow the first 99 blocks.

## F3: more than 16 blocks in one `getdata`

- Zakura answers at most 16 blocks and 1 MB for one `getdata` message. It never answers
  the other requests of the message (`zakurad/src/components/inbound.rs`,
  `GETDATA_MAX_BLOCK_COUNT`, `GETDATA_SENT_BYTES_LIMIT`). zcashd answers the others later.
- The scheduler of hayai-sync sent up to 64 hashes in one message. After 20 blocks of
  zakurad in one series, the request for the 17th block of the message had no answer.
- Result: a stall penalty for zakurad after 14 s, the block out of the fork choice after
  22 s, then a new request. In one run the second stall removed zakurad from the block
  download. hayaid then stayed one block below the tip for 90 s (the end of the run).
- Fix: `hayai_sync::download`. One message has at most 16 blocks, and fewer when the blocks
  before the last one reach 1 MB at 2 times the mean size of the recent blocks. The other
  requests of the message are free again without a penalty when both of these conditions
  are true:
  - the answers to one message reached 1 MB;
  - the peer is silent for the request timeout.
- Test: `the_answer_limits_of_a_zakura_peer_give_no_stall`.
- Reproduce on the old code: scenario a; rows `hayaid follows to N` of 22 s, or a failed
  row.

## F4: announcement of a block to legacy peers before its validation

- hayaid sent the `inv` of each block with a valid header to its legacy peers before the
  validation, and served the headers of such blocks. On Regtest a valid header costs no
  work.
- Zakura requests the block, finds it invalid and bans the address of the peer
  (`banned ip and removed banned peer addresses from address book`).
- Result in scenario e: after the invalid blocks through `submitblock` of hayaid, zakurad
  had no peer, and no block of one node reached the other one.
- On a network with proof of work the sender of such a block pays the work of one block.
  Each hayaid that forwards the block then loses its Zakura peers for the time of the ban.
- Fix: a legacy peer gets the `inv` after the commit (`Relay::block_validated`), and gets
  the headers and the `getblocks` hashes of validated blocks only. Its `getdata` for a
  block that the node did not validate yet has the answer `notfound`. A peer of the
  compact-relay extension gets the block after the header check, as before.
- Cost: the announcement to a legacy peer waits for the validation of the block.
- Tests: `a_legacy_peer_gets_no_block_before_its_validation` (hayai-net), scenario e (row
  `the two nodes are still connected`).

## F5: `curtime` and `mintime` of `getblocktemplate`

- hayai-rpc gave the clock of the node as `curtime` and the time of the template as
  `mintime`. On Regtest the newest blocks are old: each block is at the median-time-past
  plus 90 min at most. On such a chain `curtime` was above `maxtime`. A block with
  `curtime` as its time was invalid.
- zcashd and Zakura: `mintime` is the median-time-past plus 1 s, and `curtime` is a time
  that the header rules accept.
- Fix: `mintime` is the median-time-past plus 1 s. `curtime` is the clock, at least the
  time of the template and at most `maxtime`.
- Test: `getblocktemplate_has_zcashd_shape`; scenario b (the block on the template) and
  scenario e (the time cases use `mintime` and `maxtime`).

## F6: query methods of the RPC server

The RPC server of hayaid had no method that shows a block, the state or the mempool, and no
method that accepts a transaction. The server now has these methods (`docs/hayaid.md`):

- `getblockhash`;
- `getblock` (verbosity 0);
- `getblockchaininfo` (height, tip, value pools);
- `z_gettreestate` (the roots after the tip);
- `getrawmempool`;
- `sendrawtransaction`.

## F7: no `mempool` request to a peer

- Zakura announces a transaction one time, to the peers that are ready at that time. On
  Mainnet and Testnet these are 1/3 of the peers (`zakura-network/src/peer_set/set.rs`,
  `number_of_peers_to_broadcast`). Zakura waits 2 s between 2 announcements
  (`zakurad/src/components/mempool/gossip.rs`). A Zebra or Zakura node reads the mempools
  of 3 peers each 73 s for the other transactions (`mempool/crawler.rs`).
- hayai-net answered `mempool` and never sent it. A transaction whose announcement zakurad
  did not send reached hayaid only in a block.
- Result in scenario g: 5 of 39 transactions that zakurad accepted were not in the mempool
  of hayaid after 30 s.
- Fix: the relay sends `mempool` to each legacy peer after the handshake and then each
  60 s (`RelayConfig::mempool_poll`). The relay requests the transactions that it does not
  have.
- Test: `a_legacy_peer_gets_mempool_requests_and_its_answer_is_used`.

## F8: protocol version below the NU7 minimum

- From the NU7 activation Zakura disconnects a peer whose protocol version is below the
  NU7 version of the network: 170,180 on Testnet and Regtest, 170,190 on Mainnet
  (`zakura-network/src/protocol/external/types.rs`). hayai-net stated 170,160 on each
  build.
- Result in the first NU7 run: zakurad mined block 250 (the activation block), reset the
  connection of hayaid at each new handshake, and hayaid stayed at 249. hayaid had no
  warning in its log.
- Fix: a build with the NU7 rule set (the Zakura backend) states 170,190, the version of
  Zakura (`hayai_net::protocol::protocol_version`). A build without that rule set states
  170,160: it cannot follow the chain after NU7.
- Test: `protocol::tests` of hayai-net; scenario nu7.
- Not solved: hayaid logs a disconnect at the debug level only. A node that each peer
  disconnects after the handshake needs a warning.

## P1: mempool fee policy

| Rule | hayai in the first runs | Zakura | hayai now |
|---|---|---|---|
| Marginal fee for each logical action | 5,000 zatoshis (ZIP 317) | 400 zatoshis (`zakura-chain/src/transaction/unmined/zip317.rs`, `MARGINAL_FEE`) | 400 zatoshis |
| Unpaid actions | 50, then 0 | 0 (`BLOCK_UNPAID_ACTION_LIMIT`) | 0 |
| Upper bound of the minimum relay fee | 1,000 zatoshis | 800 zatoshis (`MEMPOOL_TX_FEE_REQUIREMENT_CAP`) | 800 zatoshis |
| Weight ratio cap of the template | 4 | 13 (`BLOCK_PRODUCTION_WEIGHT_RATIO_CAP`) | 13 |

- With a limit of 50 unpaid actions hayaid accepted and mined transactions that zakurad
  refused. With the limit 0 and the marginal fee of ZIP 317, zakurad accepted 4 of the 9
  transactions below that hayaid refused.
- Fix: the policy, the store and the template of hayai have the values of Zakura
  (`Zip317Params::ZAKURA`, `MIN_RELAY_FEE_CAP`; table in `docs/mempool-policy.md`).

Verdicts of scenario c, for a transaction with one input. The count of unpaid actions uses
the marginal fee of 400 zatoshis:

| Outputs | Fee (zatoshis) | Unpaid actions | hayaid in the first runs | zakurad | hayaid now |
|---|---|---|---|---|---|
| 1 | 0 | 2 | refused | refused | refused |
| 1 | 1,000 | 0 | refused | accepted | accepted |
| 1 | 9,999 | 0 | refused | accepted | accepted |
| 1 | 10,000 | 0 | accepted | accepted | accepted |
| 2 | 5,000 | 0 | refused | accepted | accepted |
| 40 | 5,000 | 26 | refused | refused | refused |
| 52 | 5,032 | 37 | refused | refused | refused |
| 60 | 5,020 | 45 | refused | refused | refused |
| 60 | 23,020 | 0 | refused | accepted | accepted |

hayaid mined each accepted transaction, and zakurad accepted the block. The test
`the_policy_cases_of_the_regtest_pair_have_the_verdict_of_zakura` (hayai-prepared) has the
same cases.

## Z1: a Zakura node that stops loses its newest blocks

zakurad writes its non-finalized blocks to disk at intervals. After SIGINT at height 240 it
started at height 227 in one run and at height 180 in another run. It then read the missing
blocks from hayaid again. A harness that uses a restart of zakurad as a disconnect makes a
fork at a height that it does not expect. Thus scenario d does not stop zakurad.

## Z2: no commit trace rows with the legacy stack

`commit_start`, `commit_finish` and `block_body_received` rows come from the block sync of
the Zakura stack (`zakura-network/src/zakura/trace.rs`). With `p2p_stack = "legacy"` the
trace directory has `legacy_sync.jsonl` and `legacy_peer_request.jsonl` only.
`scripts/join_traces.py` has no Zakura rows for the pair, so scenario g measures both nodes
from outside.

## Z3: `submitblock` verdict for an unknown parent

hayaid answers `rejected`. zakurad answers `inconclusive`. No node changes its tip.

## Measurements of scenario g

Regtest, loopback, one machine (32 threads), blocks of 2 transactions, 30 min, 622 blocks
(311 of each node) and 622 transactions. The method is in `docs/regtest-pair.md`. Other
builds ran on the machine at the same time. The values are not a benchmark result.

| Interval, measured from outside | n | Median | p90 | p99 | Maximum |
|---|---|---|---|---|---|
| Block of zakurad: hayaid has it as its tip | 311 | 35.1 ms | 41.8 ms | 6,423 ms | 8,149 ms |
| Block of zakurad: hayaid serves a template on it | 311 | 35.4 ms | 42.4 ms | 6,436 ms | 8,150 ms |
| Block of hayaid: zakurad has it as its tip | 311 | 2.6 ms | 12.7 ms | 204 ms | 418 ms |
| Block of hayaid: zakurad serves a template on it | 311 | 3.6 ms | 15.9 ms | 208 ms | 422 ms |
| Own block: hayaid serves a template on it, after the answer of `generate` | 311 | 0.3 ms | 2.2 ms | 5.0 ms | 210 ms |
| Own block: zakurad serves a template on it, after the answer of `generate` | 311 | 1.1 ms | 3.6 ms | 10.2 ms | 209 ms |
| Transaction sent to hayaid: in the mempool of zakurad | 312 | 6.2 ms | 126 ms | 7,869 ms | 8,003 ms |
| Transaction sent to zakurad: in the mempool of hayaid | 310 | 505 ms | 1,950 ms | 53,142 ms | 58,014 ms |

Inside hayaid, from its trace rows of the same run:

| Interval | n | Median | p90 | p99 | Maximum |
|---|---|---|---|---|---|
| Body of a block of zakurad received to `commit_finish` | 311 | 0.32 ms | 1.54 ms | 39.7 ms | 157 ms |
| Own block to `commit_finish` | 311 | 0.33 ms | 2.49 ms | 28.8 ms | 49.0 ms |
| Validation (`total_us`) | 622 | 0.06 ms | 0.14 ms | 4.6 ms | 29.4 ms |
| Tip change to `template_full` | 622 | 0.05 ms | 0.08 ms | 0.12 ms | 1.1 ms |

- The trace of zakurad has no row for these intervals (Z2).
- A block of zakurad needs 35 ms to become the tip of hayaid, and hayaid needs less than
  1 ms after it has the body. The run does not show which step uses the other time (the
  announcement of zakurad, the `getheaders` exchange or the `getdata` exchange). Issue:
  hayai-0dn.
- The run gives no cause for the values above 1 s. Zakura announces a transaction at most
  each 2 s. The values above 50 s are transactions that hayaid got with its `mempool`
  request (F7).
- A run before F7 and before the last form of F4 had the same medians (36.4 ms and 2.4 ms).
- Resident memory of hayaid at 0 %, 25 %, 50 %, 75 % and 100 % of the run: 51 MB, 18 MB,
  21 MB, 20 MB, 26 MB. The layer window holds the newest 1,000 blocks, and the chain had
  832 blocks at the end. zakurad: 183 MB, 68 MB, 80 MB, 67 MB, 100 MB.

## Verdict pairs of scenario e

Each row is one invalid block at height 113 (NU6.1 rules) on the pair with the funding
streams. Both nodes refuse each one.

| Block | hayaid | zakurad |
|---|---|---|
| Coinbase 1 zatoshi too much | coinbase pays 550000001 zatoshis and must pay 550000000 exactly | `Subsidy(InvalidMinerFees)` |
| Coinbase 1 zatoshi too little | coinbase pays 549999999 zatoshis and must pay 550000000 exactly | `Subsidy(InvalidMinerFees)` |
| Funding stream output 1 zatoshi too little | coinbase FundingStream(MajorGrants) output pays 49999999 zatoshis and must pay 50000000 | `Subsidy(FundingStreamNotFound)` |
| Wrong merkle root | merkle root does not match the transactions | `BadMerkleRoot` |
| Wrong header commitment | the header commitment does not match the parent's history tree | `InvalidBlockCommitment` |
| Time at the median-time-past | time is not after the median-time-past | `TimeTooEarly` |
| Time above the median-time-past plus 90 min | time is more than 90 min after the median-time-past | `TimeTooLate` |
| `bits` 0x207fffff | the target is above the proof-of-work limit | `TargetDifficultyLimit` |
| `bits` 0 | bits encode no target | `InvalidDifficulty` |
| `bits` 0x1f800001 | bits encode no target | `InvalidDifficulty` |
| Header version 3 | block version is below 4 | parse error: version must be at least 4 |
| Unknown parent | parent unknown | `inconclusive`, `MissingMinedParent` |
| 2 spends of one coin | input is spent twice in the block | refused |
| One transaction 2 times | duplicate txid | `DuplicateTransaction` |
| Spend of a missing coin | spends a coin that does not exist or was spent | `MissingTransparentInput` |
| Script that fails | input 0 script failed: evaluated to false | `Script(ScriptInvalid)` |
| Outputs above inputs | outputs exceed inputs | `IncorrectFee` |
| Coinbase spent after 8 blocks | spends a coinbase from height 105 at 113 | refused |
| Expired transaction | transaction expired at height 112 | `ExpiredTransaction` |
| Orchard binding signature, one bit changed | shielded bundles are not valid | `Halo2VerificationFailed` |
| Orchard proof, one bit changed | shielded bundles are not valid | `Halo2VerificationFailed` |

Controls: a block with one transparent and one Orchard transaction, and a block with the
harder `bits` 0x1f07ffff, are valid for both nodes.

## Faults of the harness

| Fault | Effect | Fix |
|---|---|---|
| Restart of zakurad as a disconnect | A fork at another height (Z1) | hayaid mines alone with a configuration without a peer |
| Restart of zakurad before hayaid had its blocks | Both nodes lost the blocks | The scenario waits for hayaid first |
| A block with one more transaction and the coinbase of the template | A second fault: from NU6 the coinbase pays the fees exactly | The block adds the fee to the coinbase |
| `mintime - 1` as a time that is too low, with the old `mintime` of hayaid | A valid block | F5; the case is the median-time-past |
| One expected tip for all the invalid blocks | False failures after a block that both nodes accepted | The harness reads the tip before each case |
| A shielding transaction with the coinbase of block 2 for block 101 | hayaid refused it: the coin is mature at height 102 | hayaid mines block 101 first |
| `generate` on hayaid at once after `sendrawtransaction` | A block without the transaction: the template gets a transaction after the mempool | The scenario waits for the template |
