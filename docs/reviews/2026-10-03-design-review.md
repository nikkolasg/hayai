# Design review, 2026-10-03

Subject: hayai at revision 3eff6f6, with the numbers in `bench-results/summary.json`.
Reviewer: a second-pass abstract review after the local optimizations and the correctness fixes.
Estimates carry *est.*. Paths are relative to `crates/`.

## Summary

- The validation benchmarks use a chain with zero layers and keep every coin in memory. In
  production, each input and each nullifier probes up to 100 layer maps. One index over the
  window removes this cost. It is the cheapest gain and it affects every block.
- Validation is one serial gate in front of the template. Split it into a cheap layer build
  (5–10 ms) and a verification half (scripts and proofs). Build the template, and validate the
  next block, on the speculative layer. Commit when the verdict arrives.
- Forwarding waits for local reconstruction, and reconstruction waits for missing transactions.
  Batch ids and full WtxIds are content-addressed, so a node can forward them unchanged. A node
  can then forward when the id list matches the header roots, before it has the bytes.
- Lanes help only one hop, and they break when the template changes order. Make the template
  the lane: a log of template deltas for each parent, flooded by batch id, with set semantics.
- Persistence holds the base write lock during RocksDB writes, and it has no recovery point.
  Move the write outside the lock. Then keep the coin set in memory and persist with a snapshot
  plus replay of the block files.

## 1. Window index for layered lookups

Now:

- `ChainView::get_coins` (`hayai-state/src/lib.rs:421-447`) and `contains_nullifier_many`
  (`:362-388`) walk every layer, newest first. Each outpoint costs two probes per layer.
- The window is 100 layers.
- Each layer map uses its own random `ahash` key, so each probe hashes the key again.
- Inputs of unknown transactions are read twice: once at `hayai-validate/src/lib.rs:169-202`,
  and again for all inputs at `hayai-state/src/check.rs:342-343`.
- The bench harness builds a chain with no layers and funds every coin in memory
  (`hayai-bench/src/chain_fixture.rs:64-102`). The warm results (0.8–4.5 ms) do not include
  the walk.

Change: keep one index over the window: `OutPoint → (height, Created(coin) | Spent)`, plus
one `nullifier → height` index for each pool. A push inserts the layer's entries. A pop removes
them. Finalization removes the entries at the finalized height. A lookup is then one probe in
the index, then the base. Keep the per-layer maps for reorgs. Read every input of the block
once, and pass the result to the contextual check.

Gain (*est.*):

- Typical block (200 inputs, 50 nullifiers): about 45,000 cached probes, 0.5–1 ms. After the
  change: under 0.1 ms.
- 13,000-input block after a run of full blocks: 2.6 M probes, mostly DRAM misses,
  100–250 ms. After the change: about 3 ms. A spam campaign produces exactly this case.

Cost and risk: small. The index must always equal the layer walk. Test this with random
push/pop/finalize histories. No peers. No consensus change.

Measure first: warm and cold `validate_block` on a chain that holds 100 layers of typical
shape, then 100 full layers.

## 2. Speculative tip

Now:

- `validate_block` runs lookup, drafts, scripts, the shielded batch, the contextual check and
  the tree appends one after another (`hayai-validate/src/lib.rs:169-276`).
- `LiveTemplate::on_tip` runs only after the layer push (`hayai-template/src/live.rs:363-383`).
- The template needs the ZIP 221 history root after the parent (`Tip::history_root`,
  `live.rs:70-79`). No code computes it yet.
- Principle 8 (pipeline across blocks) is not implemented: `validate_block` only accepts a view
  of committed layers.

A Zcash fact sets the shape: a ZIP 221 leaf holds the Sapling and Orchard roots after the
block, plus counts from the body. A template on block B needs B's body and its tree appends.
The header alone is not enough. The two protocol documents also disagree on timing:
`docs/protocol-template-push.md` sends `TemplateEmpty` after the layer push, and
`docs/protocol-compact-relay.md` step 5 sends it after reconstruction.

Change: split validation into two parts and run them at the same time:

- `build_layer`: roots, drafts of unknown transactions, one state read, contextual rules, tree
  appends, and the history-tree append (store the peaks in `Layer`).
- `verify`: scripts and the shielded batch.

When `build_layer` finishes, publish its layer as a speculative tip. The template then drops
the block's conflicts and pushes a full template. The validator of the next block may take a
view that includes the speculative layer. Commit when `verify` succeeds. On failure, drop the
layer and its descendants, restore the parent template and penalize the sender.

Gain (*est.*):

- Template switch after a cold competitor block: `orchard-165x2` drops from 140 ms (78 ms on
  the zakura backend) to 5–10 ms. `transparent-6500x1` drops from 34.5 ms to about 10 ms.
- Warm blocks: no change.
- The full template replaces the coinbase-only window, so the first seconds on a new tip earn
  fees.

Cost and risk: medium. The work is a speculative state in hayai-state, a revert path in the
template, and the history tree. Hashing on an invalid parent lasts at most the verify time
(under 140 ms within NU7 bounds). That exposure is smaller than the documented 5 s
empty-template timeout. No peers. No consensus change, because commit still waits for full
validation.

Measure first: the fraction of mainnet and testnet blocks that contain unknown transactions,
and how many (`Timings::unknown` in Phase 1 shadow mode).

## 3. Forwarding once the id list is complete

Now:

- A received compact block is forwarded only after `reconstruct` succeeds
  (`hayai-net/src/relay.rs:1050-1076`, `:935-987`). Each hop that lacks a transaction adds
  one `BlockTxnRequest` round trip before it forwards.
- A forwarder references only its own batches (`relay.rs:1000-1004`).
- A node never re-floods a batch announcement it receives (`relay.rs:780-839`). Lanes reach
  only the publisher's direct peers.

Analysis: the grinding attack recorded in CHANGES.md targets short ids only. Batch ids and
WtxIds are content-addressed, so forwarding them unchanged opens no new attack. A WtxId is
`txid || auth_digest`, so a node can check both the merkle root and the ZIP 244 auth data
root from the ids alone.

Change (protocol v2):

- Add a full-id section to `CompactBlock`. Senders use full WtxIds for fresh transactions:
  those announced less than a few seconds ago that the peer has not announced back. Short
  ids cover the rest.
- A receiver resolves the short ids, computes both roots from the id list, and forwards when
  they match the header. It re-keys the short ids and copies the batch refs and full ids
  unchanged.
- It requests missing bytes from every announcer with `TxRequest`, in parallel with
  forwarding. Validation waits for the bytes. Forwarding does not.
- Flood each `BatchAnnounce` once per batch id, within the existing lane limits.

Alternatives rejected:

- Two-phase announce helps only while a node waits for data. Full ids remove that wait.
- Per-link keys still need re-keying and local resolution. They add handshake state and save
  no latency.

Gain (*est.*): for a block with transactions the path lacks, h hops cost h round trips today
and about one after the change. With 3 hops at 100 ms RTT, that is about 300 ms down to
100 ms. The size cost is 58 bytes per fresh transaction. Blocks that already reconstruct
locally gain about 1 ms per hop.

Cost and risk: medium. A node forwards a block whose bytes it does not yet hold. Proof of work
limits abuse, as it does for header-first relay. Needs peers. No consensus change.

Measure first: the number of `BlockTxnRequest`s per compact block, and their cost, on the
Phase 2 testbed and on testnet.

## 4. Template as lane

Now:

- Block order follows ZIP 317 weight (`live.rs:1-10`).
- A batch reference covers only an exact run of consecutive positions
  (`hayai-relay/src/compact.rs:58-78`). A new high-ratio arrival lands mid-order and breaks
  every batch after it.
- No code publishes lanes from the template.

Change:

- Keep selection by weight ratio. Order the block by selection time within one parent, or by
  a canonical topological order (parents first, ties by txid). Consensus fixes only
  parent-before-child order.
- Publish each template delta as one batch. A candidate is `(lane_id, seq)`.
- Encode a solved block as header + coinbase + candidate + diff: short ids for additions,
  indexes for removals, and a flag for canonical order.
- A receiver can prebuild each candidate's layer against the parent. A block equal to a
  candidate then commits as a pointer swap.

Churn: most Zcash blocks are not full. In those blocks the template holds the whole mempool
and changes only by additions, about a few ids every 2 s and under 1 kB/s per lane per peer
(*est.*).

Gain (*est.*): announcements have a constant size of about 1.7 kB, and no hop that holds the
lane misses a short id. The latency gain is the round trip from section 3, for blocks that
contain private or late transactions. The bytes saved are worth under 1 ms.

Cost and risk: medium to high. Pools must publish lanes and peers must flood them. Ordering
by selection time gives up history independence ("same set gives the same bytes"). Canonical
order keeps it. No consensus change.

Measure first: the share of block transactions that the receiver lacks when the block arrives,
split into "never announced" and "announced less than 2 s before".

## 5. Persistence off the read path

Now:

- `Chain::flush` holds the base write lock while RocksDB writes (`hayai-state/src/lib.rs:286-291`,
  `hayai-coins/src/cache.rs:219-255`). Every base read waits for it.
- One 13,000-input block takes 51 ms to commit, and a flush that covers many blocks takes
  longer.
- No best-block height, frontier, anchor set or value pool is persisted, so no recovery point
  exists.

Change:

1. Under the lock, move the dirty entries into an immutable flush generation. Write it
   outside the lock, then drop it under the lock. Reads go to the cache, then the generation,
   then the disk. Write a best-block record in the same `WriteBatch` as the coins and
   nullifiers.
2. Keep the coin set and the nullifier sets in memory. Persist with a periodic snapshot at a
   finalized height, plus the block files that hayai-blockstore already keeps. Recovery loads
   the snapshot and replays the later blocks without proofs. Nullifiers become an append-only
   log for each block. RocksDB keeps only the block index.

Gain (*est.*):

- Step 1 removes a 50–500 ms stall for any block that arrives during a flush.
- Step 2 removes LSM compaction on uniform keys and the per-block flush CPU. Every coin lookup
  becomes a memory hit: the measured 13,000-input lookup drops from 3.3 ms to 0.2 ms. Reorg
  depth stays the size of the layer window.

Cost and risk: step 1 is small. Step 2 is medium, and memory grows with the set size. No
peers. No consensus change.

Measure first: the mainnet UTXO count and bytes, the nullifier counts, and the snapshot write
and load times.

## 6. Stage graph inside one block

Now:

- Scripts run first, then each Orchard group in turn, then Sapling, then the context and tree
  steps (`hayai-validate/src/lib.rs:227-276`, `hayai-prepared/src/shielded.rs:194-229`).
  Upstream Orchard batches have a serial part (about 36 % in the audit).
- Verifying keys are built on first use, and each takes tens of seconds (`shielded.rs:61-63`).

Change: run `check_scripts`, each shielded group, and context plus trees as concurrent tasks.
All of them depend only on the drafts. Build every key of the active epoch at startup and
before each activation height.

Gain (*est.*): 10–20 % on cold mixed blocks (99.5 ms today). Key prebuilding removes a stall
of tens of seconds on the first shielded block after a start or an upgrade.

Cost and risk: small. No peers. No consensus change.

## 7. Own-block and shared-transaction reuse

Now: an own block takes the warm validation path (1–4.5 ms), and then `on_tip` rebuilds the
selection (2.6 ms).

Change: after each template change, precompute the next template (candidates minus the
selection) and the template's layer. A submission that matches commits as a pointer swap.
For competitor blocks on the same parent, reuse the store's tip-relative verdicts: inputs
unspent and nullifiers absent. `remove_conflicting` (`hayai-prepared/src/store.rs:299-325`)
already keeps that invariant.

Gain (*est.*): 3–7 ms after own blocks, and about 1 ms per competitor block once section 1 is
done.

Risk: verdict reuse is a consensus-critical cache, and one missed invalidation accepts a
double spend. Do it only if contextual checks still cost more than a few ms after section 1.

## 8. Zcash-specific points

- NU7 caps a block at 330 actions and 300 Sapling I/O, so cold verification stays between
  80 and 140 ms. Section 2 removes that time from the template path.
- Most blocks are small. For a typical block, the window walk (section 1) and network round
  trips (section 3) cost more than the cryptography.
- The coinbase is transparent only (`hayai-template/src/coinbase.rs:1-6`). If pools need a
  shielded coinbase, prove the subsidy-only output ahead of time.
- For M4, a SwiftSync bitmap of the outputs still unspent at a checkpoint can build the
  snapshot of section 5. The trees need only a frontier and the anchor set at the checkpoint.
- The two benchmarks disagree by up to 15x: `relay/full_block_parse` (10–14 ms) and
  `wire/parse_block` (0.8–4.7 ms). Resolve this before using either for relay decisions.

## Parts that are right

- Deferring the Orchard MSM across blocks: halo2_proofs 0.3.5 exports `verify_proof` with a
  strategy and `MSM::add_msm`, so accumulation is possible. But known transactions are already
  verified, and a block cannot commit before its proofs. Keep block-scoped batches.
- Per-input script parallelism: one large transaction would stall a per-transaction split.
- Copying coin scripts out of the block (`check.rs:230`): a slice would pin a 2 MB block for
  each surviving coin.
- Erlay-style reconciliation: at Zcash transaction rates, announce traffic is tens of kB/s.
  The gain does not pay for the complexity.
- A nullifier filter at the tip: pinned Ribbon filters already make negative lookups cheap,
  under 1 ms per block.

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
