# Review of consensus and synchronization code, 2026-10-04

Scope: `hayai-consensus`, `hayai-state` (check, checkpoint), `hayai-validate`, `hayai-prepared`,
`hayai-sync`, `hayai-net`, `hayaid`, `hayai-template`. Reference: Zakura clone at
`../zakura-src` (revision `1377915`), the ZIPs and the protocol specification.
Method: code reading only. No build and no test ran. A finding is verified against the code
unless it has the mark "unverified".

Path abbreviations: `ZC` = `zakura/crates/zakura-consensus/src`, `ZCH` = `zakura/crates/zakura-chain/src`,
`ZS` = `zakura/crates/zakura-state/src/service`.

## Summary

| Severity | Count |
|---|---|
| Critical | 6 |
| High | 8 |
| Medium | 11 |
| Low | 14 |

- One consensus divergence in the header rules (C1).
- Three remote stops of the node (C2, C3, C4).
- Two paths that mark an honest block invalid in the header log, which a restart does not repair (C5, C6).
- No divergence in difficulty, subsidy, funding streams, lockbox, value pools, anchors, expiry, lock time or sigops.

## Critical findings

### C1. Header version with the high bit

- Location: `crates/hayai-consensus/src/header.rs:124`, `crates/hayai-sync/src/headers.rs:378`, `crates/hayai-wire/src/header.rs:133`.
- Reference: `ZCH/block/serialize.rs:51` rejects `version >> 31 != 0`. zcashd reads `nVersion` as a signed integer, so the value is below 4.
- Fault: hayai compares the version as `u32` with 4 only.
- Input: a header with `version = 0x8000_0004`, valid `bits`, valid Equihash and valid times. hayai accepts the header. Zakura and zcashd reject it.
- Cost of the input: the work of one block.
- Fix: reject when `version >> 31 != 0 || version < 4` in the two functions. Add the case to `crates/hayai-consensus/tests/header.rs`.

### C2. Body with a duplicated transaction on the checkpoint path

- Location: `crates/hayai-wire/src/lib.rs:350` (`merkle_root` has no mutation detection), `crates/hayaid/src/sync.rs:170-180` (`parse_body`), `crates/hayai-validate/src/lib.rs:319-345`, `crates/hayai-state/src/check/checkpoint.rs:63-72`, `crates/hayaid/src/node/full.rs:232-246`.
- Reference: `ZC/block/check.rs:523-559` (duplicate transaction hashes are a block error, CVE-2012-2459).
- Fault: a checkpointed job treats each error other than `BlockCommitments` as a fatal `NodeError`. The duplicate check of the state runs before the history check.
- Input: a checkpointed block with transactions `[cb, a, b]`. A peer answers `getdata` with `[cb, a, b, b]`. The block hash and the merkle root are equal. `checkpoint_layer` returns `DoubleSpend` or `DuplicateNullifier`, and the driver stops.
- The peer can send the input again after each restart. The input works before and after NU5.
- Fix: refuse a body with a duplicate txid in `parse_body` as `BodyError` (wrong body). Map `ContextError::DuplicateTxid` to `Refusal::WrongBody` in `refusal_of`.

### C3. Panic on a first transaction that is not a coinbase (prebuilt path)

- Location: `crates/hayai-validate/src/lib.rs:494` (`draft(raw.txs[0].clone(), .., Vec::new())`), `crates/hayai-prepared/src/prepare.rs:433` (`assert_eq!(spent.len(), vin_len)`), `crates/hayai-state/src/check.rs:1132` (`matches` reads `txs[1..]` only).
- Reference: `ZC/block/check.rs:68` (`coinbase_is_first`) returns an error.
- Fault: `commit_prebuilt` does not check the shape of `txs[0]` before `draft`. A transaction with a transparent input and an empty coin list reaches the assertion.
- Input: a header on the committed tip with valid proof of work. The body has `txs[1..]` equal to a prebuilt body of the node and a `txs[0]` with one transparent input (prevout not null).
- Precondition: a prebuilt body matches. `mining.prebuild_own` is on by default (`crates/hayaid/src/config.rs:200`). With an empty mempool the own body has zero transactions (`crates/hayaid/tests/regtest_pair.rs:380`). The trace of `prebuild` for an empty list is unverified.
- Cost of the input: the work of one block. On Testnet the minimum-difficulty rule makes the cost small.
- Fix: return `ContextError::NoCoinbase` in `commit_prebuilt` when `txs[0]` is not a coinbase. Change the two assertions in `draft` to a `PrepareError`.

### C4. Lock order inversion in the relay

- Location: `crates/hayai-net/src/relay.rs:1487-1488` (`BatchRequest`: `lanes`, then `own_batches`) and `crates/hayai-net/src/relay.rs:2136-2137` (`send_compact_block`: `own_batches`, then `lanes`, held during the send loop).
- Input: a peer sends `BatchRequest` messages in a loop. One message can hold about 250,000 ids (8 MB payload limit, `crates/hayai-relay/src/message.rs:37`). The next forwarded block takes the locks in the opposite order.
- Result: the reader thread and the sender thread wait for each other. The ticker stops at `sweep_pending` (`relay.rs:1043`), and the driver stops at `announce_batch` (`relay.rs:1960`) or in `forward_block`.
- Fix: take `lanes` before `own_batches` at lines 2136-2137, or copy the batch list and release the two locks before the send loop.

### C5. Changed coinbase script on the prebuilt path

- Location: `crates/hayaid/src/node/full.rs:223-250` (a job that is not class `full` goes to `commit_on_tip`), `crates/hayaid/src/node/full.rs:296` (only `commit_speculative` calls `auth_data_matches`), `crates/hayaid/src/node.rs:1141-1153`, `crates/hayaid/src/sync.rs:1014-1019` (`mark_invalid`), `crates/hayai-sync/src/headers.rs:847` (the mark goes to the header log).
- Reference: ZIP 244. The scriptSig of a v5 transaction is in the authorizing digest, not in the txid.
- Fault: the prebuilt path has no wrong-body check. A body that a peer changed gives `Refusal::Invalid`.
- Input: an honest block with a v5 coinbase whose `txs[1..]` match a prebuilt body. A peer changes one byte of the coinbase scriptSig after the height push and sends the body first. The txid and the merkle root are equal. The result is `BlockCommitments`, then the invalid mark.
- Result: the honest block and its descendants stay invalid after a restart. The node leaves the chain of the network.
- Precondition: the same as C3.
- Fix: call `auth_data_matches` for each job that is not checkpointed, before each commit path. Map `ContextError::BlockCommitments` to `WrongBody` on each path.

### C6. Missing Orchard verifying key and the invalid mark

- Location: `crates/hayaid/src/node.rs:1918-1921` (keys of the start epoch and of the next upgrade only), `crates/hayai-prepared/src/shielded.rs:197`, `crates/hayaid/src/node/full.rs:81-86` (`refusal_of`), `crates/hayaid/src/sync.rs:1014-1019`.
- Fault: `PrepareError::Unsupported` is a property of the node, and `refusal_of` maps it to `Refusal::Invalid`. The mark is in `headers.log`.
- Input 1: a first start on Mainnet. The keys are the keys of the Sprout epoch. The first block above the last checkpoint with an Orchard or Ironwood bundle gets the invalid mark. No peer action is necessary.
- Input 2: a node that runs across an upgrade.
- The peer that sent the block gets 100 points and a ban.
- `docs/hayaid.md:440-449` states that a restart resumes the synchronization. The statement is wrong: the mark stays after the restart, and no command removes it.
- The same mapping applies to `SproutStateUnknown` and `UnsupportedUpgrade`.
- Fix: make each local-capability error a fatal `NodeError`. Build the key of the epoch before the first full validation of that epoch.

## High findings

### H1. Duplicated transaction on the full path before NU5

- Location: `crates/hayai-state/src/check.rs:1012-1014` (`DuplicateTxid`), `crates/hayaid/src/node/full.rs:60-63` (`auth_data_matches` returns true before NU5), `crates/hayaid/src/node/full.rs:81-86`.
- Input: the body of C2 for a block before NU5 that takes the full path. The honest block gets the invalid mark.
- Reach: each block before NU5 on a network without checkpoints (Regtest). On Mainnet the block must be above 1,046,399, below 1,687,104 and not yet checkpointed in the header chain. The Mainnet case is unverified.
- Fix: the fix of C2.

### H2. Ban for a rule of the local clock

- Location: `crates/hayaid/src/sync.rs:729-731` (each `RejectReason::Rule` is `InvalidHeader`, 100 points), `crates/hayaid/src/sync.rs:82` (`check_local_time` is part of the rules).
- Reference: `ZC/error.rs:788-806` gives 0 points. `crates/hayai-net/src/relay.rs:967-984` scores context-free faults only.
- Input: a miner sets the block time to the true time plus 7,195 s. The local clock is 10 s slow. Each peer that sends the header gets a ban of 24 h.
- Fix: score only `Version`, `SolutionLength`, `Pow`, `Equihash` and `CheckpointMismatch`.

### H3. Header synchronization peer without progress

- Location: `crates/hayaid/src/sync.rs:524-529` (selection by the reported `start_height`), `crates/hayaid/src/sync.rs:692-708` (160 headers refresh `since_ms` with no check of `accepted.added`), `crates/hayaid/src/sync.rs:709-713`.
- Input: a peer reports the height `0xffffffff` and answers each `getheaders` with the same 160 known headers. The timeout of 120 s does not fire. No penalty applies. Other peers get no continuation.
- Result: the header chain grows only through announcements. A node that is far behind does not reach the tip.
- Fix: continue and refresh the timer only when `added > 0`. Set a minimum header rate. Do not select by the reported height.

### H4. Outbound queue without a byte limit

- Location: `crates/hayai-net/src/transport.rs:69,136` (1,024 frames), `crates/hayai-net/src/relay.rs:1404-1422` (`on_getdata` ignores the result of `send` and continues).
- Reference: zcashd stops `getdata` when the send buffer is full.
- Input: a peer sends `getdata` for 1,024 blocks of 2 MB and does not read. About 2 GB stays in the queue of one connection. After `QueueFull`, the loop reads each remaining block from disk.
- The queue size and the loop are verified. The write timeout and the 20 min ping timeout are unverified.
- Fix: a byte budget for each queue. Stop the loop at the first failed send.

### H5. Scheduler preference for a peer without a measured rate

- Location: `crates/hayai-sync/src/download.rs:968-975` (`delivery_ms.unwrap_or(0)` gives the key 0; the reported height selects the set), `crates/hayai-sync/src/score.rs:70` (a stall has 0 points).
- Input: a peer connects, reports the height `0xffffffff`, sends no body and connects again after the disconnection. The peer gets the lowest missing blocks each time. Each round delays the synchronization by about 16 s.
- The rescue rule for a peer without a rate (`download.rs:881`) is unverified.
- Fix: give a peer without a rate the median delivery time of the measured peers. Refuse a new connection for some time after the stall limit.

### H6. Driver queue without a bound

- Location: `crates/hayaid/src/node.rs:1972` (`unbounded()`), `crates/hayaid/src/node.rs:618` (the tick runs only when the queue is empty), `crates/hayaid/src/sync.rs:182-199`.
- Input: several connections send known `headers` messages (160 headers, no penalty) while the driver validates or flushes. Memory grows at the rate of the network. No stall rule and no header timeout runs while the queue has events.
- Fix: a bound for each peer in `SyncInbox`. Run the tick on elapsed time.

### H7. Race between mempool admission and commit

- Location: `crates/hayaid/src/mempool.rs:112` (copy of the view) to `:164` (insert), `crates/hayaid/src/node.rs:1036-1042` (the driver writes the view, then cleans the store).
- Reference: ZIP 317 block production needs valid transactions. Zakura checks against the state at the insert.
- Input: transaction T spends an outpoint that block N+1 also spends. A relay thread prepares T on tip N. The driver commits N+1 and cleans the store. The relay thread then inserts T.
- Result: no later step removes T before its expiry height. With expiry 0, T stays. Each template that selects T is an invalid block.
- Fix: a tip generation in the store. `admit` reads it before the view, and the insert refuses a changed generation.

### H8. Block store durability before the coins flush

- Location: `crates/hayaid/src/node.rs:753-775` (`flush_coins` has no `blocks.sync()`), `crates/hayaid/src/node.rs:745` (the sync is in `close` only).
- Sequence: a coins flush at base B, then a power loss before the kernel writes the block files. At the next start the block store ends below B, and the start fails. The replay code for this case is unverified.
- Fix: call `self.blocks.sync()` in `flush_coins` before `chain.flush()`.

## Medium findings

| # | Subject | Location | Input or condition | Fix |
|---|---|---|---|---|
| M1 | Testnet NU7 height is unknown to the default backend. `rules_at` returns the NU6.3 rule set from Testnet 4,465,026 with no error. The owner decision requires a stop. | `crates/hayai-crypto/src/lib.rs:93-98`, `crates/hayai-consensus/src/rules.rs:331`; `ZCH/parameters/constants.rs` | A Testnet block at 4,465,026. Which side is right is unverified. | Give `hayai-consensus` the height on both backends and return `UnsupportedUpgrade`. |
| M2 | `apply_checkpointed` gets `raw.hash()` as `expected`. The comparison does nothing. `docs/consensus-rules.md` states the comparison as done. | `crates/hayaid/src/node.rs:1086,1713` | A stale delivered block between two checkpoints. Exploit unverified. | Pass the hash of the best header chain at the height. |
| M3 | The coinbase-spend rule applies on Regtest. Zakura waives it. | `crates/hayai-state/src/check.rs:486-492`; `ZCH/transaction.rs:552-564` | A Regtest transaction that spends a coinbase output to a transparent output. | A network flag in `check_txs`. |
| M4 | `readmit` verifies each mempool transaction again on the driver thread. | `crates/hayaid/src/mempool.rs:179-200` | A full store and a reorg of 1 block. | Use the stored `PreparedTx`. |
| M5 | The template on a speculative tip keeps expired transactions until the driver handles the removal. | `crates/hayaid/src/node/full.rs:393-425`; ZIP 203 | A stored transaction with expiry height H and a speculative block at H. | Add these ids to `conflicting`. |
| M6 | The replay of the header log depends on the checkpoint list of the binary. | `crates/hayai-sync/src/headers.rs:586-597`, `crates/hayaid/src/node/full.rs:459` | A log with a stale side header; a new release adds a checkpoint at that height. The start fails. Unverified in a run. | Skip and count a record that the list refuses. |
| M7 | No bound on side headers. The log has no compaction. | `crates/hayai-sync/src/headers.rs:385-418` | Testnet headers at the limit target more than 450 s after the parent, many siblings for each block down to tip − 1,000. | A limit for each fork height and peer. |
| M8 | The scheduler clock is stale after a long driver step. | `crates/hayaid/src/sync.rs:971-1038`, `crates/hayai-sync/src/download.rs:875` | A pause above 8 s gives a `Stall` to honest peers. Pause duration unverified. | Set the clock before each `step`. |
| M9 | A compact-relay peer gets no penalty for a wrong body. | `crates/hayai-net/src/relay.rs:951-953` | A `getdata` answer with a wrong merkle root. | Report it as `Malformed`. |
| M10 | A downloaded tip block goes to peers before the authorizing data check. | `crates/hayaid/src/sync.rs:420-429` | A body with changed authorizing data. | Forward after `auth_data_matches`. |
| M11 | `state.log` has no bound, and the start reads all of it. | `crates/hayaid/src/persist.rs:100-101,578-582` | Size at Mainnet height unverified. | Keep the last records only. |

## Low findings

| # | Subject | Location |
|---|---|---|
| L1 | `ParentUnknown` passes the header commitment on the checkpoint path. | `crates/hayai-state/src/check.rs:880-890` |
| L2 | `prepare` runs scripts and ZIP 213 decryption of a coinbase-shaped peer transaction before the policy. | `crates/hayaid/src/mempool.rs` (`admit`) |
| L3 | `min_relay_fee` clamps to 100 zatoshis below 1,000 bytes (equal to Zakura; zcashd unverified). | `crates/hayai-prepared/src/policy.rs` |
| L4 | A child of an unmined parent is always `MissingInput`; the ancestor code of the store has no user. | `crates/hayaid/src/mempool.rs` |
| L5 | `rebuild_block` does not count the sigops of a coinbase override. | `crates/hayai-template/src/submission.rs` |
| L6 | `PeerDisconnected` can reach the driver before `PeerConnected`. | `crates/hayai-net/src/relay.rs:1198-1202,900-906` |
| L7 | Regtest: cumulative work addition can overflow (unverified). | `crates/hayai-sync/src/headers.rs:412` |
| L8 | Inbound slots have no eviction. | `crates/hayai-net/src/connect.rs:243-248` |
| L9 | `mempool` requests have no rate limit. | `crates/hayai-net/src/relay.rs:1424-1430` |
| L10 | A transaction that does not parse is dropped with a debug line and no score. | `crates/hayai-net/src/relay.rs:1435-1441` |
| L11 | The address source limit uses the full IPv6 address, not the /64. | `crates/hayai-net/src/addrbook.rs:388,403` |
| L12 | `headers_after` reads 160 headers from disk under the header chain mutex. | `crates/hayaid/src/node.rs:344-351` |
| L13 | `open_header_chain` applies the local time rule to committed headers. | `crates/hayaid/src/node/full.rs:482-490` |
| L14 | A body that arrives after 64 s is `Unsolicited` (20 points). | `crates/hayai-sync/src/download.rs:642-645` |

## Checkpoint path

- Checked: parent, hash at a checkpoint height, merkle root, coin existence, double spend in the block, duplicate nullifier in the block, value pools, header commitment.
- Not checked: scripts, proofs, signatures, coinbase rules and terms, maturity, the coinbase-spend rule, parent order in the block, nullifiers of earlier blocks, anchors, expiry, lock time, block limits, duplicate txids, each context-free transaction rule.
- Before NU5 the txid binds the whole transaction. From NU5 `hashBlockCommitments` binds the authorizing data, and a mismatch is a wrong body.
- The only body that a peer can change under the same hash is the duplicate of C2.
- A block at or below the mandatory checkpoint waits for a checkpoint above it (`crates/hayaid/src/node/full.rs:175`). A false header chain below a checkpoint does not change the state.
- The founders' reward check has no caller on Mainnet or Testnet: full validation refuses these heights.

## Rules compared and found equal

| Rule | hayai | Reference |
|---|---|---|
| H1 version >= 4 (except C1) | `hayai-consensus/src/header.rs:124` | `ZCH/block/serialize.rs:58` |
| H2, H3 solution length, Equihash, Regtest waiver | `header.rs:186-213` | `ZC/block/check.rs:158` |
| H4 target at or below the limit | `hayai-wire/src/header.rs:186-266` | `ZC/block/check.rs:105` |
| H5 hash at or below the target | `hayai-wire/src/header.rs:272` | `ZC/block/check.rs:127` |
| H6 expected `bits` (window 17, median 11, damping, bounds) | `difficulty.rs:118-205` | `adjusted_difficulty.rs` |
| H7 Testnet minimum difficulty (299,188, gap above 6 spacings) | `difficulty.rs:139-147` | `ZCH/parameters/network_upgrade.rs:522-575` |
| H9 time above median-time-past | `header.rs:132-137` | `validate.rs` |
| H10 time at or below median-time-past + 90 min (Mainnet 2, Testnet 653,606) | `header.rs:138-144` | `ZCH/parameters/network.rs:246` |
| H11 local clock + 2 h | `header.rs:216` | `ZC/block/check.rs:404` |
| H12 header commitment for each epoch, V1 to V3 leaves | `hayai-state/src/history.rs` | `ZS/check.rs:263-352` |
| H13 height and coinbase height | `check.rs:343-372` | `ZS/check.rs:371` |
| H14 genesis hashes | `network.rs` | checkpoint files |
| H15 checkpoint hash, mandatory checkpoint Canopy − 1 | `hayai-validate/src/lib.rs:322-339`, `checkpoints.rs:143` | `ZC/checkpoint.rs`, `network.rs:271` |
| B1 coinbase first and only first (full path; see C3) | `check.rs:920-924` | `ZC/block/check.rs:68` |
| B2, B3 merkle root, duplicate txid verdict | `check.rs:1012` | `ZC/block/check.rs:523` |
| B6 sigops above 20,000 rejected | `check.rs:663-666` | `ZC/block.rs` |
| B8 subsidy and halvings | `subsidy.rs:61-110` | `ZCH/parameters/network/subsidy.rs` |
| B9 founders' reward | `founders.rs:29-60` | `ZC/block/check.rs:204-224` |
| B10 funding streams (ranges, numerators, address lists, address period) | `funding.rs` | `constants/{mainnet,testnet}.rs` |
| B11 NU6.1 disbursement (10 outputs of 7,875 ZEC, multiset match) | `lockbox.rs`, `coinbase.rs:143-195` | `ZC/block/check.rs:268-290` |
| B12, B13, B14 coinbase value (`<=` before NU6, `==` from NU6, Ironwood balance) | `coinbase.rs:166-216` | `ZC/block/check.rs:326-398` |
| B19 no Orchard bundle in a coinbase from NU6.3 | `prepare.rs:424` | `ZC/transaction/check.rs:367` |
| B18, B20 ZIP 213, lead byte, no grace period | `hayai-prepared/src/coinbase.rs` | `ZCH/primitives/zcash_note_encryption.rs` |
| B21 six value pools, total at or below `MAX_MONEY` | `check.rs:741-785` | `ZCH/value_balance.rs:360-376` |
| B22, B23, B25 inputs, maturity 100, remaining value | `check.rs` | `ZS/check/utxo.rs` |
| B24 coinbase-spend rule on Mainnet and Testnet (Regtest: M3) | `check.rs:486-492` | `ZCH/transaction.rs:552` |
| B26 nullifiers, four pools | `check.rs` | `ZS/check/nullifier.rs` |
| B27 Sapling, Orchard, Ironwood anchors | `check.rs` | `ZS/check/anchors.rs:24` |
| B28 Sprout anchors with interstitial roots | `check.rs:586-613` | `ZS/check/anchors.rs:230-345` |
| B29 history append | `history.rs` | `ZCH/history_tree.rs` |
| T1 versions for each epoch | `prepare.rs` | `ZC/transaction.rs:1021-1260` |
| T3 to T7 sources, sinks and flags | `prepare.rs` (`draft`) | `ZC/transaction/check.rs:131-179` |
| T10 Orchard-disabled range (Mainnet 3,363,426 to 3,364,600; Testnet 4,048,500 to 4,052,000) | `network.rs:291`, `rules.rs:326-339` | `ZCH/parameters/network.rs:26,31,373` |
| T11, T12 coinbase rules, script length | `prepare.rs:413-431` | `ZC/transaction/check.rs:251` |
| T13 expiry | `prepare.rs`, `check.rs:384,552` | `ZC/transaction/check.rs:535-640` |
| T14, T15 `vpub` rules, ZIP 211 | `sprout.rs` | `ZC/transaction/check.rs:279,304` |
| T16 `valueBalanceOrchard` >= 0 from NU6.3 | `prepare.rs:407-411` | `ZC/transaction/check.rs:338` |
| T17 duplicates in a transaction | `prepare.rs`, `sprout.rs` | `ZC/transaction/check.rs:403` |
| T18 lock time (block time, threshold 500,000,000) | `check.rs:202` | `ZC/transaction/check.rs:60-86` |
| T21, T22 circuit by branch | `shielded.rs` | `ZC/transaction.rs:1200-1228` |
| T23 JoinSplit proof, `h_sig`, Ed25519 | `sprout.rs` | `ZC/primitives/groth16.rs:112` |
| T25, T26 value balance and ranges | `prepare.rs` | specification §7.1.2 |
| T27, ZIP 317, ZIP 401, ZIP 203 mempool constants | `policy.rs`, `store.rs`, `zip317.rs` | the ZIPs |

Rules with no code, as the plan states: H8, B15, B16, B17, T2 (NU7), T24 (BCTV14).

hayai checks the Sapling root commitment of the header from Sapling to Heartwood. Zakura does not. zcashd does, so hayai agrees with the specification.

## Rules and behaviours with no test

- Header version with the high bit (C1).
- Body with a duplicated transaction on each path (C2, H1).
- `commit_prebuilt` with a first transaction that is not a coinbase (C3).
- `BatchRequest` during a block forward (C4).
- A block of a peer on the prebuilt path with changed authorizing data (C5).
- A missing verifying key at an epoch change (C6).
- The checkpoint path of the driver.
- A header that fails only the local time rule, and its score (H2).
- A header peer with no progress (H3). A `getdata` to a peer that does not read (H4).
- A peer without a rate against measured peers (H5). A queue flood with no tick (H6).
- An admission during a commit (H7). A power loss of the block store (H8).
- Lock time as a time, and the boundaries `lock == height` and `lock == block time`.
- Sigops at 20,000 exactly.
- Duplicate nullifier in a block on the checkpoint path (Sapling, Orchard).
- `PrepareError::ExpiryTooHigh`, `PrepareError::V4ValueBalance` in `draft`, `Reject::OrchardDisabled`, `Reject::NullifierInChain`.
- A JoinSplit with valid proofs and a wrong `joinSplitSig`.
- Header log replay with another checkpoint list (M6). Testnet minimum-difficulty headers in the header chain (M7).
- Crash during a reorg, and a restart after a reorg.
