# Design review, 2026-10-03

Subject: hayai at revision 3eff6f6, with the numbers in `bench-results/summary.json`.
Reviewer: a second-pass abstract review after the local optimizations and the correctness fixes.
The mark *est.* shows an estimate. Paths are relative to `crates/`.

## Summary

- The validation benchmarks use a chain with 0 layers and keep every coin in memory. In
  production, each input and each nullifier probes up to 100 layer maps. 1 index over the
  window removes this cost. It is the cheapest gain. It applies to every block.
- The template waits for validation, and validation is 1 serial step. Split it into a cheap
  layer build (5–10 ms) and a verification half (scripts and proofs). Build the template on the
  speculative layer. Validate the next block on the speculative layer. Commit when the verdict
  arrives.
- Forwarding waits for local reconstruction. Reconstruction waits for missing transactions.
  Batch ids and full WtxIds are content-addressed, so a node can forward them unchanged. A node
  can then forward when the id list matches the header roots, before it has the bytes.
- Lanes help only 1 hop. A lane becomes unusable when the template changes its order. Make the
  template the lane. The lane is a log of template deltas for each parent, with set semantics.
  Peers flood it by batch id.
- Persistence holds the base write lock during RocksDB writes. Persistence has no recovery
  point. Move the write outside the lock. Then keep the coin set in memory. Persist it with a
  snapshot plus a replay of the block files.

## 1. Window index for layered lookups

Now:

- `ChainView::get_coins` (`hayai-state/src/lib.rs:421-447`) and `contains_nullifier_many`
  (`:362-388`) walk every layer, newest first. Each outpoint costs 2 probes for each layer.
- The window is 100 layers.
- Each layer map uses its own random `ahash` key, so each probe hashes the key again.
- The code reads the inputs of unknown transactions 2 times: 1 time at
  `hayai-validate/src/lib.rs:169-202`, and again for all inputs at
  `hayai-state/src/check.rs:342-343`.
- The bench harness builds a chain with no layers and funds every coin in memory
  (`hayai-bench/src/chain_fixture.rs:64-102`). The warm results (0.8–4.5 ms) do not include
  the walk.

Change:

- Keep 1 index over the window: `OutPoint → (height, Created(coin) | Spent)`, plus 1
  `nullifier → height` index for each pool.
- A push inserts the entries of the layer. A pop removes them. Finalization removes the
  entries at the finalized height.
- A lookup is then 1 probe in the index, then the base.
- Keep the maps of each layer for reorgs.
- Read every input of the block 1 time. Pass the result to the contextual check.

Gain (*est.*):

- Typical block (200 inputs, 50 nullifiers): about 45,000 cached probes, 0.5–1 ms. After the
  change: under 0.1 ms.
- 13,000-input block after a run of full blocks: 2.6 M probes, mostly DRAM misses,
  100–250 ms. After the change: about 3 ms. A spam campaign makes exactly this case.

Cost and risk: small. The index must always equal the layer walk. Test this with random
push/pop/finalize histories. The change needs no peers. It makes no consensus change.

Measure first: warm and cold `validate_block` on a chain that holds 100 layers of typical
shape, then 100 full layers.

## 2. Speculative tip

Now:

- `validate_block` runs lookup, drafts, scripts, the shielded batch, the contextual check and
  the tree appends in sequence (`hayai-validate/src/lib.rs:169-276`).
- `LiveTemplate::on_tip` runs only after the layer push (`hayai-template/src/live.rs:363-383`).
- The template needs the ZIP 221 history root after the parent (`Tip::history_root`,
  `live.rs:70-79`). No code computes it yet.
- The code does not implement principle 8 (pipeline across blocks): `validate_block` only
  accepts a view of committed layers.

A Zcash fact sets the design. A ZIP 221 leaf holds the Sapling and Orchard roots after the
block, plus counts from the body. A template on block B needs the body of B and its tree
appends. The header alone is not sufficient.

The 2 protocol documents also disagree on the time of `TemplateEmpty`.
`docs/protocol-template-push.md` sends `TemplateEmpty` after the layer push.
`docs/protocol-compact-relay.md` step 5 sends it after reconstruction.

Change: split validation into 2 parts. Run the 2 parts at the same time:

- `build_layer`: roots, drafts of unknown transactions, 1 state read, contextual rules, tree
  appends, and the history-tree append (store the peaks in `Layer`).
- `verify`: scripts and the shielded batch.

When `build_layer` finishes, publish its layer as a speculative tip. The template then removes
the conflicts of the block and pushes a full template. The validator of the next block can take
a view that includes the speculative layer. Commit when `verify` succeeds.

If `verify` fails:

1. Remove the layer and its descendants.
2. Restore the parent template.
3. Penalize the sender.

Gain (*est.*):

- Template switch after a cold competitor block: `orchard-165x2` decreases from 140 ms (78 ms
  on the zakura backend) to 5–10 ms. `transparent-6500x1` decreases from 34.5 ms to about
  10 ms.
- Warm blocks: no change.
- The full template replaces the coinbase-only window, so the first seconds on a new tip earn
  fees.

Cost and risk: medium. The work is a speculative state in hayai-state, a revert path in the
template, and the history tree. Miners hash on an invalid parent for at most the verify time
(under 140 ms within NU7 bounds). That time is shorter than the documented 5 s timeout of the
empty template. The change needs no peers. It makes no consensus change, because the commit
still waits for full validation.

Measure first: the fraction of mainnet and testnet blocks that contain unknown transactions,
and the count of these transactions (`Timings::unknown` in Phase 1 shadow mode).

## 3. Forwarding after a complete id list

Now:

- A node forwards a received compact block only after `reconstruct` succeeds
  (`hayai-net/src/relay.rs:1050-1076`, `:935-987`). Each hop that does not have a transaction
  adds 1 `BlockTxnRequest` round trip before it forwards.
- A forwarder references only its own batches (`relay.rs:1000-1004`).
- A node never floods again a batch announcement that it receives (`relay.rs:780-839`). Lanes
  reach only the direct peers of the publisher.

Analysis: the grinding attack that CHANGES.md records targets short ids only. Batch ids and
WtxIds are content-addressed, so a node that forwards them unchanged makes no new attack
possible. A WtxId is `txid || auth_digest`. Thus a node can check both the merkle root and the
ZIP 244 root of the auth data from the ids alone.

Change (protocol v2):

- Add a full-id section to `CompactBlock`. A sender uses full WtxIds for fresh transactions. A
  fresh transaction has an announcement less than a few seconds ago, and the peer has not
  announced it back. The sender uses short ids for the other transactions.
- A receiver resolves the short ids, computes both roots from the id list, and forwards when
  they match the header. It re-keys the short ids and copies the batch refs and full ids
  unchanged.
- It requests missing bytes from every announcer with `TxRequest`, at the same time as it
  forwards the block. Validation waits for the bytes. Forwarding does not.
- Flood each `BatchAnnounce` 1 time for each batch id, within the existing lane limits.

Rejected alternatives:

- A 2-phase announce helps only while a node waits for data. Full ids remove that wait.
- Keys for each link still need a re-key and a local resolution. They add handshake state and
  save no latency.

Gain (*est.*): for a block with transactions that the path does not have, h hops cost h round
trips today and about 1 round trip after the change. With 3 hops at 100 ms RTT, the time
decreases from about 300 ms to 100 ms. The size cost is 58 bytes for each fresh transaction. A
block that already reconstructs locally gains about 1 ms for each hop.

Cost and risk: medium. A node forwards a block whose bytes it does not yet hold. Proof of work
limits abuse, as it does for header-first relay. The change needs peers. It makes no consensus
change.

Measure first: the number of `BlockTxnRequest`s for each compact block, and their cost, on the
Phase 2 testbed and on testnet.

## 4. Template as lane

Now:

- The block order follows the ZIP 317 weight (`live.rs:1-10`).
- A batch reference covers only an exact run of consecutive positions
  (`hayai-relay/src/compact.rs:58-78`). A new transaction with a high ratio goes into the
  middle of the order. It makes every batch after it unusable.
- No code publishes lanes from the template.

Change:

- Keep the selection by weight ratio. Order the block by the selection time within 1 parent,
  or by a canonical topological order (parents first, ties by txid). Consensus fixes only the
  parent-before-child order.
- Publish each template delta as 1 batch. A candidate is `(lane_id, seq)`.
- Encode a solved block as header + coinbase + candidate + diff: short ids for additions,
  indexes for removals, and a flag for canonical order.
- A receiver can prebuild the layer of each candidate against the parent. A block equal to a
  candidate then commits as a pointer swap.

Churn: most Zcash blocks are not full. In those blocks the template holds the whole mempool
and changes only by additions. The additions are about a few ids every 2 s, and under 1 kB/s
for each lane for each peer (*est.*).

Gain (*est.*): announcements have a constant size of about 1.7 kB. No hop that holds the lane
misses a short id. The latency gain is the round trip from section 3, for blocks that contain
private or late transactions. The saved bytes are worth under 1 ms.

Cost and risk: medium to high. Pools must publish lanes. Peers must flood them. An order by
selection time loses history independence ("same set gives the same bytes"). The canonical
order keeps it. The change makes no consensus change.

Measure first: the share of the transactions of a block that the receiver does not have when
the block arrives, split into "never announced" and "announced less than 2 s before".

## 5. Persistence off the read path

Now:

- `Chain::flush` holds the base write lock while RocksDB writes (`hayai-state/src/lib.rs:286-291`,
  `hayai-coins/src/cache.rs:219-255`). Every base read waits for it.
- A 13,000-input block takes 51 ms to commit. A flush that covers many blocks takes longer.
- The code persists no best-block height, frontier, anchor set or value pool. Thus no recovery
  point exists.

Change:

1. Under the lock, move the dirty entries into an immutable flush generation. Write it
   outside the lock. Then remove it under the lock. A read goes to the cache, then to the
   generation, then to the disk. Write a best-block record in the same `WriteBatch` as the
   coins and nullifiers.
2. Keep the coin set and the nullifier sets in memory. Persist them with a periodic snapshot
   at a finalized height, plus the block files that hayai-blockstore already keeps. The
   recovery loads the snapshot and replays the later blocks without proofs. Nullifiers become
   an append-only log for each block. RocksDB keeps only the block index.

Gain (*est.*):

- Step 1 removes a 50–500 ms stall for any block that arrives during a flush.
- Step 2 removes LSM compaction on uniform keys and the CPU time of the flush for each block.
  Every coin lookup becomes a memory hit. The measured 13,000-input lookup decreases from
  3.3 ms to 0.2 ms. The reorg depth stays the size of the layer window.

Cost and risk: step 1 is small. Step 2 is medium. The memory increases with the size of the
set. The change needs no peers. It makes no consensus change.

Measure first: the count and the bytes of the mainnet UTXOs, the nullifier counts, and the
write and load times of the snapshot.

## 6. Stage graph in one block

Now:

- Scripts run first, then each Orchard group in sequence, then Sapling, then the context and
  tree steps (`hayai-validate/src/lib.rs:227-276`, `hayai-prepared/src/shielded.rs:194-229`).
  Upstream Orchard batches have a serial part (about 36 % in the audit).
- The code builds each verifying key at its first use. Each build takes tens of seconds
  (`shielded.rs:61-63`).

Change: run `check_scripts`, each shielded group, and the context plus the trees as concurrent
tasks. All of them depend only on the drafts. Build every key of the active epoch at startup
and before each activation height.

Gain (*est.*): 10–20 % on cold mixed blocks (99.5 ms today). A prebuild of the keys removes a
stall of tens of seconds on the first shielded block after a start or an upgrade.

Cost and risk: small. The change needs no peers. It makes no consensus change.

## 7. Own-block and shared-transaction reuse

Now: an own block takes the warm validation path (1–4.5 ms). Then `on_tip` rebuilds the
selection (2.6 ms).

Change: after each template change, precompute the next template (candidates minus the
selection) and the layer of the template. A submission that matches commits as a pointer swap.

For competitor blocks on the same parent, reuse the tip-relative verdicts of the store: inputs
unspent and nullifiers absent. `remove_conflicting` (`hayai-prepared/src/store.rs:299-325`)
already keeps that invariant.

Gain (*est.*): 3–7 ms after own blocks, and about 1 ms for each competitor block after the
change of section 1.

Risk: the reuse of verdicts is a consensus-critical cache. A single missed invalidation accepts
a double spend. Do it only if contextual checks still cost more than a few ms after section 1.

## 8. Zcash-specific points

- NU7 limits a block to 330 actions and 300 Sapling I/O, so cold verification stays between
  80 and 140 ms. Section 2 removes that time from the template path.
- Most blocks are small. For a typical block, the window walk (section 1) and network round
  trips (section 3) cost more than the cryptography.
- The coinbase is transparent only (`hayai-template/src/coinbase.rs:1-6`). If pools need a
  shielded coinbase, prove the subsidy-only output in advance.
- For M4, a SwiftSync bitmap of the outputs still unspent at a checkpoint can build the
  snapshot of section 5. The trees need only a frontier and the anchor set at the checkpoint.
- The 2 benchmarks disagree by up to 15x: `relay/full_block_parse` (10–14 ms) and
  `wire/parse_block` (0.8–4.7 ms). Resolve this difference before a relay decision uses either
  benchmark.

## Parts that are right

- Deferral of the Orchard MSM across blocks: halo2_proofs 0.3.5 exports `verify_proof` with a
  strategy and `MSM::add_msm`, so accumulation is possible. But the node already verified the
  known transactions. A block cannot commit before its proofs. Keep block-scoped batches.
- Script parallelism for each input: a split by transaction stalls on 1 large transaction.
- Copy of the coin scripts out of the block (`check.rs:230`): a slice keeps a 2 MB block in
  memory for each coin that survives.
- Erlay-style reconciliation: at Zcash transaction rates, announce traffic is tens of kB/s.
  The gain is smaller than the cost of the complexity.
- A nullifier filter at the tip: pinned Ribbon filters already make negative lookups cheap,
  under 1 ms for each block.

## Table

| Opportunity | Metric | Est. gain | Effort | Needs peers | Consensus risk |
|---|---|---|---|---|---|
| 1. Window index | M1, M3 | 0.5–1 ms typical; 100–250 ms worst | S | No | None |
| 2. Speculative tip | M3, effective M1 | 25–135 ms on cold blocks | M | No | None (commit after verify) |
| 3. Id-complete forwarding | M2 | (h−1) RTT per block with missing txs | M | Yes | None |
| 4. Template as lane | M2 | missing-tx RTT for lane publishers | M–L | Yes (pools) | None |
| 5. Persistence off the read path | M1 tail, M4 | 50–500 ms stall removed; 3 ms/lookup | S, then M | No | None |
| 6. Stage graph and eager keys | M1 cold | 10–20 %; seconds at start | S | No | None |
| 7. Own-block and verdict reuse | M3 | 1–7 ms | M | No | Cache invalidation must be exact |
