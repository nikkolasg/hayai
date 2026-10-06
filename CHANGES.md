# CHANGES

Design decisions and lessons, behaviour level. Bug fixes are not recorded here.

## 2026-10-03 — Repository created

- Scope: the performance core of a miner node (prepared transactions, bulk validation, coins
  cache, layered state, compact relay with lanes, live templates, flat block store), not a
  complete node. Networking, RPC and full rule coverage come later.
- Cryptography comes from the upstream Zcash crates; `zakura-*` forks are allowed only as
  benchmark baselines inside `hayai-bench`, so a bug in Zakura's forks cannot reach hayai.
  (Superseded the same day by the `hayai-crypto` facade, below.)
- Every performance claim has a benchmark against Zakura's code or a faithful port of its data
  layout. Estimates are labelled as estimates in the report.

## 2026-10-03 — hayai-sinsemilla and hayai-trees

- MerkleCRH^Orchard uses a position-weighted table (`[2^(51-i)] S(j)`, 4.8 MiB, position-major)
  so a hash is 52 additions. Unlike Zakura, the incomplete-addition exceptional cases are kept:
  each table entry also stores `x([2] T)` so `A_i = ±S[m_i]` is one comparison, the chord
  denominator being zero distinguishes the doubling case from `⊥`, and the first word is a
  precomputed start point per level. Tests force every case through a custom Q
  (`Table::new(q)`), which is why the table is parameterised by Q at all.
- Two evaluators: a Jacobian scalar path (own mixed addition, so the `Z²` is shared with the
  exceptional test) and an affine lane path with one Montgomery-trick inversion per column.
  Field inversion in upstream `pasta_curves` is a plain square-and-multiply (~380
  multiplications), which sets the crossover: lanes only pay off from a few dozen lanes per
  thread, so `merkle_crh_orchard_many` hashes small inputs with the scalar path in parallel and
  cuts large inputs into one lane chunk per rayon thread.
- Field inversion decided the numbers: upstream `pasta_curves::Fp::invert` is square-and-multiply
  (4.0 µs); `invert_vartime` is a 62-bit-divstep Bernstein–Yang port of libsecp256k1's
  `modinv64` (0.75 µs) and every result is checked with one multiplication, falling back to the
  upstream inversion (counted by `invert_fallbacks()`), so a bug in the fast path can only cost
  time, never correctness. The lane crossover moved from ~32 to ~12 lanes per thread.
- Tree appends are scheduled as one rayon task per aligned block of `2^b` leaves (about two
  blocks per thread, at most 64 leaves) with the levels inside a block hashed on that thread,
  then the block roots level-synchronously across subtrees. Level-synchronous batches over the
  whole block (ten pool barriers per append) scaled poorly beyond eight threads.
- The 4.8 MiB table does not fit L2, so both paths prefetch the entries of the next steps
  (addresses depend only on the message words): −14 % on the scalar path with varied inputs.
  Benchmarks of a hash must cycle through inputs; a repeated input keeps its 51 entries in L1.
- Upstream `MerkleHashOrchard::combine` recomputes the domain's Q by hash-to-curve on every
  call, so the upstream baseline is ~120 µs per hash, not the ~18 µs of a cached domain.
- `hayai-trees` keeps the upstream `Frontier`, node and anchor types; `append_many` expands the
  frontier into per-level slots, hashes each level of the new perfect subtrees as one batch and
  merges carries, and the root is computed with the fast hasher rather than `Frontier::root`
  (which would call upstream `combine` 32 times). Orchard nodes cross into the hasher through
  their canonical bytes because `MerkleHashOrchard` does not expose its field element.

## 2026-10-03 — hayai-wire, fixtures, hayai-blockstore

- Transaction boundaries do not need the parser: `scan::tx_wire_len` walks the v1–v6 wire
  layout with length arithmetic only (CompactSize counts, fixed component sizes, proof
  lengths), so `RawBlock::parse` delimits every transaction first and runs upstream
  `Transaction::read` on the slices in parallel. `RawBlock::parse_sequential` is kept as the
  reference implementation and bench baseline. Upstream `Transaction::read` itself is slow
  (byte-at-a-time `Vector::read` for scripts and proofs) and computes the txid unconditionally;
  parallelism absorbs it. The ZIP 244 authorizing digest is hashed from the scanned ranges
  instead of a second traversal of the parsed form.
- Lesson: dropping a parallel-parsed block costs more than parsing it (glibc frees of chunks
  allocated on other threads' arenas: ~2.8 ms vs ~1.1 ms for 6,500 transparent transactions).
  A binary must pick its allocator for this; benchmarks that drop the result inside the timed
  region measure that cost.
- Upstream's `test-dependencies` feature (proptest generators and ZIP 143/243/244 vectors)
  requires `proptest < 1.7` while the workspace pins 1.11, so the vectors are copied under
  `crates/hayai-wire/tests/vectors/` and generated transactions are assembled at the byte
  level with `Transaction::read` as the oracle.
- Fixture epoch is NU6.2 (`FIXTURE_HEIGHT` 3,400,000, v5 transactions): it is the last epoch in
  which an Orchard v5 bundle may consist of bare outputs funded by a transparent input, which
  keeps the generator free of Orchard note trees and witnesses. NU6.3 moves to v6 and disables
  cross-address transfers for the Orchard pool. Orchard bundles use dummy spends and the
  empty-tree anchor; a validator that checks anchors must seed that anchor.
- Sighashes of a transaction with transparent inputs commit to the spent coins (amounts and
  scriptPubKeys), so a parsed `Transaction` cannot produce its own sighash; the fixture tests
  re-attach the funding coins through a custom `Authorization` marker. Validation code will
  need the same construction.
- Block store records are not fsynced per block; the index is written after the data so it
  never references unwritten bytes, and an unreferenced tail after a crash is simply followed
  by later appends.

## 2026-10-03 — hayai-coins

- The coins cache follows Bitcoin Core's `CCoinsViewCache`: outpoint-keyed, coins created and
  spent before a flush never reach disk, one write batch per flush. A dirty-entry index keeps
  flush O(block) rather than O(cache) (first measurement: 84 ms → 51 ms per block).
- RocksDB point-lookup settings are applied by hand because `optimize_for_point_lookup`
  replaces the table factory and would discard the Ribbon filter and pinning settings.
  `multi_get` runs in 256-key chunks on the rayon pool since RocksDB's MultiGet is
  single-threaded per call.
- Coins and nullifiers flush in separate batches; atomic block finalization, if needed, is a
  single trait method to add (`write_block_batch`).

## 2026-10-03 — hayai-relay and hayai-template

- Compact blocks reference a prefix of the non-prefilled positions by batch ids and the rest by
  6-byte short ids; an unknown batch fails the whole reconstruction (its length is unknown) and
  is requested, rather than guessing indexes. The reconstructed `RawBlock` is assembled from
  retained bytes; only prefilled and requested transactions are parsed. Lesson: test helpers
  that derive ids from content make "different" test batches identical.
- Template selection is deterministic greedy by ZIP 317 weight ratio (fixed-point, 32
  fractional bits) with per-pick rollback positions, so incremental updates are exact (checked
  against a from-scratch build in a randomized test). ZIP 317's weighted random sampling is a
  recommendation; determinism buys identical templates across pool instances and own-block
  cache hits. Subsidy and funding streams are supplied by the caller.
- `Removed` for an unknown candidate is a no-op because tip events drop conflicts before the
  store does. `zcash_transparent 0.10` exposes `zcash_script 0.4` types, so that version sits
  beside 0.6 (the Rust interpreter used for verification).

## 2026-10-03 — hayai-crypto: swappable crypto backend

- Decision: the upstream kernels are slower (halo2/orchard verification, Sinsemilla, field
  inversion) and the independence target is the node, not every field multiplication; upstream
  is absorbing Zakura's kernel work (equihash PRs librustzcash #3116/#3117/#3119). So the
  crypto stack became a feature choice instead of a fixed decision: `hayai-crypto` re-exports
  the upstream crates (`upstream`, default) or the `zakura-*` forks (`zakura`, `=2.2.0`), and
  no other crate names a crypto crate in its manifest.
- Feature plumbing: every crate has `default = ["upstream"]` plus `upstream`/`zakura` features
  forwarding to its hayai dependencies, and every inter-crate edge is `default-features =
  false`. Lesson: `--no-default-features` only disables the defaults of the packages selected
  on the command line; a dependency edge with default features re-enables them, which turns
  into the "mutually exclusive" compile error. The forwarding tables are the price of
  `cargo test -p <crate> --features zakura` working.
- The forks are API-identical for everything hayai calls (same module paths, `Fp::to_repr` /
  `from_raw`, the Sinsemilla constants, `BatchValidator`, `Transaction::read`) except the RNG
  line: ff/group 0.14 and the zakura builders and validators take rand_core 0.10 generators,
  whose traits are named differently (`Rng` for the core trait, `SysRng` fallible by
  default). `hayai_crypto::rng` resolves that once; code that mixes a backend RNG with
  `gen_range`-style draws keeps two generators (one per rand line) rather than importing two
  traits of the same name.
- `hayai-bench` keeps the `zk_*` renamed forks as baselines. Under the `zakura` feature the
  hayai side and the baselines are the same crates; benchmark ids carry
  `hayai_crypto::BACKEND_SUFFIX` (`hayai` vs `hayai-zk`, `upstream` vs `zakura-lib`) so both
  backends fit in one report.
- First numbers on the forks: Orchard-heavy cold block validation 140 ms → 78 ms (halo2
  batch verification), MerkleCRH and tree appends within noise (hayai's own kernels do that
  work on both backends), and zakura-pasta's `Fp::invert` is already a 62-bit divstep
  (0.63 µs vs `invert_vartime` 0.71 µs), so the lane crossover argument only holds upstream.

## 2026-10-03 — hayai-net and hayai-rpc

- The compact relay extension is negotiated inside the legacy protocol (`zcmpctver` after
  `verack`, then relay frames inside `zcmpct`) rather than on a second stream or port. One
  framing and one size discipline; with the extension disabled the node is byte-for-byte a
  legacy node; the legacy path is exercised by every block, so it cannot rot. Service bit
  `1 << 26` because Zakura's P2P v2 uses `1 << 24`.
- Relay policy is "both paths, always": every block, whatever its origin, enters one path
  (dedup by hash, header check, `CompactBlock` to extension peers, `inv` to legacy peers,
  then the validator once). A compact block is forwarded before reconstruction; legacy
  peers are told once the body exists. Peers that never send `zcmpctver` stay legacy.
- `std::net` threads (one reader per peer, bounded outbound queue, one ticker) instead of
  tokio: the relay's work per message is small and synchronous, the rest of the workspace
  is sync and rayon-based, and the peer count of a miner node is tens, not thousands. The
  `Transport` trait is the seam if that changes.
- Zebra's `Codec` is `pub(crate)` behind a private `protocol` module, so a differential
  test against it is not possible from outside; the codec is tested against Zebra's byte
  vectors (addr, MSG_WTX, version offsets) and hand-built frames instead, and zebra-network
  is not a dependency.
- `getblocktemplate` is a shim over the live template (`TemplateFeed` fed by every
  `TemplateUpdate`): templates are never rebuilt per call, `longpollid`/`workid` are the
  template id, and `submitblock` with a `workid` is rebuilt from the stored template so the
  own-block path applies. Long polls wake at once on tip events (empty template first,
  then full) and after a delay on set changes, as zcashd's mempool poll does.
- The HTTP server is hand-rolled (POST + Content-Length + keep-alive) rather than a crate:
  pool software needs nothing more and the dependency graph stays std-only.


## 2026-10-03 — CPU and memory review of the parallel paths

- The first system benchmarks overstated every parallel path: the counting allocator of
  `sysbench` updated four process-wide atomics per allocation, and 32 threads contending on
  that cache line turned the 6,500-transaction parse from 2.1 ms wall / 33 ms CPU into
  7.5 ms / 177 ms (IPC 0.24). Counters are now per-thread stripes (256 padded slots, live
  bytes folded into the global peak every 64 KiB). Lesson: a measurement hook on the
  allocation path is itself a shared resource; check the harness before the code when a
  parallel section shows an IPC far below the sequential one.
- Policy from the owner, after the corrected numbers: more CPU is acceptable where hayai
  wins wall time; no change trades wall time for CPU. A rayon pool of 16 threads (physical
  cores, `sysbench --threads 16`) cuts CPU 30–70 % on every scenario but loses 10–15 % wall
  time on the proof-heavy blocks (halo2 batch verification scales across the SMT threads),
  so the pool stays at one thread per logical CPU. tree_append_2048 at 2.18 vs 1.95 ms was
  noise: three runs of 100 iterations give 2.22–2.37 ms against 2.24–2.36 ms.
- mimalloc as the base allocator (`hayai-bench` feature `mimalloc`, off): wall time within
  the run-to-run noise of this machine (parse −11 %, template, tree_append and warm
  validation ±30 % in both directions across two runs), RSS +30 to +230 MiB on every row.
  Not adopted and not recommended for `hayaid` on these numbers; the feature remains so the
  measurement can be repeated on a quiet machine.
- Coins block cache default 1 GiB → 256 MiB: the 13,000-input lookup on the 2 M-coin store
  is 3.3 ms at 256 MiB and 3.15 ms at 1 GiB; 32 MiB gives 20 ms and 317 ms of CPU (filter,
  index and data blocks thrash), so the cache is not where memory should be saved. The
  333 MiB RSS of `coins_commit` is that cache holding the whole 160 MiB store, which is the
  design.
- Allocation volume. The block's output map was built twice per block (validator and
  contextual check) and rebuilt a third time into the layer; `block_outputs` builds it once
  and `contextual_check_with_outputs` moves it into the layer (warm validation of the
  6,500-transaction block: 14.8 → 8.4 MiB, 26k → 13k allocations, 5.1 → 4.0 ms). The spent
  scripts of a transaction were copied three times into the sighash context
  (`SpentInputs` now shares `Arc` slices; the upstream `TransparentAuthorizingContext` still
  hands out owned vectors per sighash). Coins are decoded straight from RocksDB's pinned
  slices and one buffer serves a whole flush (`coins_commit` 52k → 27k allocations,
  `coins_lookup` 39k → 27k). What remains in cold validation is inherent to the upstream
  APIs: a `Draft` is 3.4 KiB (the transaction retyped with its spent coins for
  `signature_hash`, plus its digests), `Transaction::read` and the script interpreter
  allocate per push, and Orchard batch verification allocates ~450 MiB per 330-action block
  inside halo2.

## 2026-10-03 — Review fixes

- Lesson: an upstream parser can drop consensus-relevant bytes. `Transaction::read_v4`
  replaces `valueBalanceSapling` with zero when a v4 transaction has no Sapling spends or
  outputs, so the parsed form cannot enforce §7.1.2's "MUST be 0". The wire scanner already
  walks every byte, so it is the place to recover such fields (`RawTx::
  v4_value_balance_without_components`); rules are then checked in `draft`, not in the parser,
  so the parse step stays byte-compatible with upstream.
- Rules were implemented from memory of the maturity check alone; zcashd's
  `CheckTxInputs` has a second coinbase rule (spends only to shielded outputs). When porting
  a check, read the whole upstream function and list every reject reason it can emit.
- Compact blocks are forwarded after reconstruction, re-keyed with a local nonce (BIP 152 /
  Bitcoin Core behaviour), not before reconstruction with the sender's short ids: a relayed
  nonce would let one sender cause collisions network-wide, and the receiver cannot recompute
  short ids for transactions it has not resolved. A merkle mismatch is a short-id collision
  and costs the sender nothing; the full block is fetched over `getdata` on the same
  connection. Waits on a peer are timestamped and move to the next announcer.
- The prepared store's conflict detection has to mirror every per-block uniqueness rule the
  contextual check enforces (outpoints and nullifiers), or the template can build a block the
  node itself rejects.
- Data-structure candidates, measured on this machine with `benches/structures.rs` on hayai's
  key shapes (random 32-byte txids and nullifiers, 36-byte outpoints, 64-byte wtxids, 6-byte
  short ids); advertised numbers were not used. Kept: `ahash` (foldhash fast/quality and
  rapidhash are 1.2–1.8x slower to build and 1.5–4x slower on hits and misses at 13k and
  1M keys; ahash's AES path hashes these keys in one or two rounds); the per-layer ahash sets
  (a sorted `Vec` with binary search saves 23 % per layer but is 7x slower to build and 29x
  slower on the 100-layer walk of 13,000 mostly missing lookups); `dashmap` for the prepared
  store (papaya is equal single-threaded, which is the validation path, 1.9x faster under
  32 readers plus a writer, at one heap allocation per entry; scc is 1.3x slower
  single-threaded). Rejected: `SmallVec<[_; 4]>` for the per-transaction lists (2.5–8x
  faster than `Vec` for 1–8 items in isolation, but the lists are empty and unallocated for
  transparent transactions and three of ~11 allocations for a shielded one, while the inline
  storage is 144 B against 24 B per field on every stored `PreparedTx`); `multitable` for
  the short-id index (1.1–1.8x slower build, 1.1–1.3x slower hits, 1.8–2.3x slower misses
  for 4–34 % less memory, and the table never grows, so about one build in 10,000 at
  100,000 keys refuses an insert and has to be rebuilt with a fresh hasher). Its no-resize
  property fits only the per-block short-id index among hayai's structures; the measured
  numbers rule it out there, and it is never a candidate for consensus state.

## 2026-10-03 — Compact relay version 2: forward on the id list

- Forwarding after reconstruction (the review fix above) costs one `BlockTxnRequest` round
  trip per hop for every transaction a hop lacks. The grinding attack it closed targets short
  ids only. Batch ids and WtxIds are content-addressed, so a hop forwards them unchanged,
  and a WtxId (`txid || auth_digest`) lets a node check both header roots from the ids alone.
  Version 2 therefore adds a full-id section to `CompactBlock` and forwards once the id list
  matches the header: the merkle root always, `hashBlockCommitments` when the parent's
  history root is known (`HistoryRootSource`, `None` counts as "forwarded without auth
  root"). A root mismatch forwards nothing and fetches the full block, as before. Bytes are
  then requested with `TxRequest` from every announcer; validation waits, forwarding does
  not. `relay/forward_latency`: 21–22 ms per hop at a 20 ms round trip in version 1 against
  0.4–2.5 ms in version 2.
- Frame layout: the full-id section sits at the end of the frame, present only when
  non-empty, so one decoder reads both versions without per-peer state (the transport
  decodes `zcmpct` before it knows the peer's version). Full ids carry their own index, like
  prefilled transactions, so a fresh transaction in the middle of the block costs 58 bytes
  and leaves the positions around it untouched.
- Senders decide the form per peer from what they announced and when: fresh (announced
  less than 3 s ago and not announced back) or never announced → full id; outside the local
  store → prefilled; otherwise short id. One `CompactBuilder` computes the short ids once per
  block and selects per peer. Version 1 peers keep the version 1 forms and receive the
  block after the body is complete.
- A node that forwarded on ids owes its peers the bytes: a `TxRequest` for a transaction of
  a block it is still completing is answered when the bytes arrive, and retained bodies
  are indexed by WtxId for later requests. Received batch announcements are flooded once
  per batch id to lane peers after the node holds every transaction of the batch.
- Lesson: the `TxRequest`/`Tx` path must feed pending blocks before the mempool sink
  decides, or a policy rejection would starve a block of bytes the node already received.
- `block_commitments` (ZIP 244) now lives in `hayai-wire` beside `auth_data_root`;
  `hayai-validate` keeps a copy until it is switched to the shared function.

## 2026-10-03 — Design review items 1, 5a and 6

- Window index (`hayai-state/src/window.rs`). The layer walk cost one probe per layer per
  key. Measured with the new `validate/block_windowed` bench (100 typical layers under the
  fixture block, `chain_with_layers`): warm transparent-6500x1 13.5 ms → 4.4 ms, cold
  59.3 → 31.2 ms; warm mixed-2000x1-100x2 5.2 → 2.4 ms; warm transparent-1000x2
  2.9 → 0.8 ms. The windowed numbers now equal the zero-layer ones. `state/
  lookup_through_window` (13,000 lookups, 100 layers): walk 13.5 ms, index 0.36 ms.
  Design: the index holds the newest layer's contribution per outpoint and the oldest
  layer per nullifier; the per-layer maps stay the source of truth, so a pop derives the
  popped keys again from the remaining layers instead of storing shadowed entries; the
  index carries its tip, and a view with another tip walks its own layers, so a stale view
  never reads another tip's entries; finalization absorbs a layer into the base before it
  drops the layer's index entries, so a reader that misses the index finds the layer's
  effect in the base. The walk stays public as the reference (`get_coins_by_walk`) for the
  property test (random push/pop/finalize histories, checked on every view ever taken)
  and for the bench. Lesson from that test: distinct blocks need distinct hashes even in a
  test; a pop followed by a push at the same height with the same hash looks like the same
  tip to a stale view.
- The validator read the inputs of unknown transactions and the contextual check read
  every input again. `resolve_inputs` runs once; the drafts and the check share the result.
  `check_parent` runs before the read, so a block for another tip costs no state round.
- Persistence off the read path. A flush held the base write lock during the RocksDB
  write. The flush now has three phases: `begin_flush` under the lock takes the dirty
  index as the generation in flight (the entries stay in the map, marked not fresh, so a
  spend during the write becomes a tombstone for the next flush), the generation is
  written outside the lock as one `WriteBatch` with the nullifiers and a `best_block`
  record, and `end_flush` under the lock marks the entries clean, skipping the ones the
  writer changed again. Reads during the write hit the map, so the coins side needs no
  generation lookup path; the nullifier sets probe the generation as a second set. The
  record is the recovery point: the batch is atomic, so the disk is the state after
  exactly that block, and a restart replays the later blocks.
- Stage graph. Scripts, the shielded batch and the contextual check with the trees run as
  three rayon tasks after the drafts. `Draft` shares its `PreparedTx` through an `Arc`, so
  the check runs on it while the scripts still borrow the draft. The verdict is the first
  error in stage order, so it does not depend on the schedule. Measured cold, same
  minute, sequential vs concurrent on a loaded machine (load 13–23): mixed-2000x1-100x2
  104.2 → 94.6 ms, orchard-165x2 150.5 → 148.4 ms; against the morning's quiet baseline
  (95.3 and 136.4 ms) the mixed block reads 93.9 ms and the Orchard block 146.7 ms, which
  is the machine's drift, not the change. The stages that overlap the shielded batch cost
  5–6 ms on the mixed block, which bounds the gain; the review's 10–20 % assumed a larger
  serial share.
- Eager keys. The Orchard verifying key build is 0.6 s on 32 threads and 1.7 s on one
  thread in a release build of upstream `orchard` 0.15.5; the "tens of seconds" in the old
  comment was not measured. `VerifyingKeys::prebuild` builds the active and next epoch's
  keys on a background thread and `ready()` waits.

## 2026-10-03 — Design review item 5b: in-memory coins backing

- `MemBacking` keeps the coin set and the nullifier sets in memory; persistence is an
  append-only log (one CRC32C-framed record per write, synced per record by default) and
  whole-set snapshots renamed into place. Recovery = snapshot + log records with a later
  sequence number; only a torn last record is forgiven, and it is reported.
- Coin layout, measured on 2,002,000 P2PKH coins with the counting allocator (heap bytes
  per coin, parallel build, 13,000-key `get_many`): chosen dense shards (69-byte entries
  in a vector + `HashTable<u32>` of positions, 256 shards) 79.7 B, 57–81 ms load,
  0.67–0.96 ms. Rejected: `HashMap<OutPoint, Coin>` 235 B (0.33–0.60 ms, no allocation
  per lookup), one compact `HashMap<[u8; 36], packed>` 147 B and a 0.34–0.71 s
  single-threaded build, the same sharded 147 B. Inline compact tables pay 70 bytes per
  power-of-two bucket: at 2 M coins the load factor is 0.48, and at mainnet size (27.3 M)
  every shard crosses the next doubling near 29.4 M coins at once. The dense layout pays
  5 bytes per bucket. The lookup floor is the script allocation that `Coin` requires.
- Nullifier layout (2 M nullifiers): sorted run per shard 32.1 B and 88–102 µs per 1,000
  lookups; sharded ahash sets 69.1 B and 5 µs. Runs kept: 2 GB less at mainnet size, and
  a block checks a few thousand nullifiers at most.
- Lesson: take one lock per shard per batch, not per key. A lock is a locked
  read-modify-write, which on x86 orders the loads around it, so the misses of
  consecutive keys stop overlapping (13,000 lookups on one thread: 1.57 → 1.18 ms).
- Checksum: CRC32C 8.1–8.6 GB/s against BLAKE2b 0.93–1.0 GB/s on the 138 MB snapshot.
  The checksums detect damage, not an attacker who can write the files.
- `coins/commit_block/13000`: 22.6–28.7 ms with a sync per record, 16.8–17.4 ms without,
  against 64–76 ms on RocksDB (loaded machine, load average 8–18; back-to-back runs).

## 2026-10-03 — hayaid, hayai-trace, /metrics

- Zakura's Regtest waives proof of work (`disable_pow`: solution shape and compact target
  only, no hash filter, no Equihash), so the Regtest producer sends a null solution and
  hayaid needs no (48, 5) solver. zcashd Regtest verifies both and would need one.
  Lesson: hayai-wire accepts only 1344-byte solutions, so no hayai node can parse a Regtest
  header of Zakura or zcashd (36 bytes). The gap spans hayai-wire, hayai-relay, hayai-net
  and hayai-template (`docs/hayaid.md`, Regtest); hayaid pairs with hayaid until then.
- The relay forwards a block after its header check and before its validation, so a child
  can arrive while its parent waits in the driver's queue. The header index therefore
  holds pending headers (checked, not committed), and the driver holds a block whose
  parent is pending until that parent commits. Without this, a burst of blocks broke the
  follower at the second block, because hayai-net does not synchronize.
- Shadow mode reads coins from upstream with `getrawtransaction`, not `gettxout`:
  `gettxout` answers for the upstream tip, which runs ahead of hayai, so the coin that the
  block under validation spends reads as spent. Each block's tree roots are compared with
  upstream's `z_gettreestate`; this makes "an unknown anchor is older than the start
  height" a fact, so such anchors are accepted and counted. Every trust limit has a
  counter and a trace field. A disagreement stops the node.
- Metrics are a small atomic registry with a hand-written exposition in hayai-rpc, not the
  `metrics` crate. Names follow Zakura's exporter where the meaning is the same.
- The trace writer follows zakura-jsonl-trace (bounded queue, count of dropped rows, flush
  and fsync every 1 s, one file per table) and adds `unix_us`, so traces of two processes
  join by time as well as by hash.

## 2026-10-03 — Design review item 2: speculative tip and ZIP 221 history tree

- The history tree uses upstream `zcash_history` 0.5 (the crate of Zakura's node) through
  the facade. `HistoryState` keeps only the MMR peaks, as ZIP 221 nodes, plus the node count
  and the upgrade. The peaks are sufficient to append and to compute the root. Each layer
  and the base hold one state, so a reorg needs no truncate. Tests: the zcash-test-vectors
  V1 and V2 vectors (peaks, root and work after each of 16 appends), and the real mainnet
  headers after the Heartwood (903,000) and Canopy (1,046,400) activation blocks. An
  activation block starts a new tree, so the next header commits to a one-leaf tree that
  needs no earlier peaks.
- ZIP 221 detail that the code follows: block `n` commits to the tree from the last
  activation before `n` up to `n - 1`. An activation block thus commits to the whole tree of
  the previous upgrade. Only the Heartwood activation block holds all zeros.
- Seeding: peaks cannot come from headers, because a leaf holds the block's final note
  commitment roots. Before Heartwood the tree is empty and known. After Heartwood a node
  seeds the base with `HistoryState::from_peaks`. Without a seed the header rule is not
  checked and the layer records `history: None`. NU6.3 needs the Ironwood tree (tree
  version 3), so its append is an error.
- Correction to the brief: `TemplateEmpty` cannot go out at the header check. A template
  on block B commits to the history tree after B, and B's leaf needs B's body and tree
  appends. One rule now holds in both protocol documents: `TemplateEmpty` and
  `TemplateFull` at the layer build (speculative tip), `TemplateRevert` when `verify` fails.
- `build_layer` keeps the drafts of the unknown transactions, because the contextual
  check needs their fees, nullifiers and commitments. `verify` holds only the scripts and
  the shielded batch. `validate_block` still runs the context concurrently with them.
- Speculative layers never enter the window index. A view walks its layers above the
  index's tip, then probes the index. This also makes a view taken before a pop use the
  index instead of a full walk.
- `template/switch_after_block`, time from a parsed block to the full template on it,
  8,000 candidates, history tree of 4,095 leaves (load average 10–12 during the run):

  | block | `validate/block` cold | serial cold | speculative cold | serial warm | speculative warm |
  |---|---|---|---|---|---|
  | orchard-165x2 | 136.7 ms | 135.2 ms | 5.4 ms | 2.9 ms | 3.0 ms |
  | mixed-2000x1-100x2 | 91.6 ms | 92.9 ms | 10.0 ms | 4.2 ms | 4.0 ms |
  | transparent-6500x1 | 25.5 ms | 30.5 ms | 23.0 ms | 8.0 ms | 5.7 ms |

  The transparent block gains little: its cold cost is the 6,500 drafts (sighash
  digests), and the layer build needs them.
- Lesson: the shared bench keys were built lazily inside `OnceLock::get_or_init` on a rayon
  worker, with the multicore keygen. That worker can take another task that waits on the
  same `OnceLock`. Parallel runs of `tests/validate.rs` stopped in 6 of 8 runs. The
  fixture now builds the key with `VerifyingKeys::prebuild` and `ready()` before any
  validation (0 of 8). The lazy path in hayai-prepared still has this hazard.

## 2026-10-03 — Zebra baselines

- Reference: the newest upstream releases, not the local Zebra checkout (02f9648 is
  zebra-chain 10.0.0, which predates the `zcash_primitives` transaction of zebra-chain 13).
  The real-code row uses `zebra-chain` 13.0.1, the newest release on the workspace's upstream
  Zcash crates, so cargo duplicates nothing. Ports cite `zebra-state` 14.0.0,
  `zebra-consensus` 16.0.0 and `zebra-rpc` 18.0.0.
- `wire/parse_block` `zebra`: the real `zebra-chain` code. Its `Transaction` wraps a
  `zcash_primitives` transaction, so the parse path differs from Zakura's own structs.
- `coins/lookup_block_inputs` `zebra`: the same UTXO layout as Zakura; 7 gets per input
  (verifier 2, contextual check 2, finalization 3). Zakura has an added `CheckParentInputs`
  round (2 gets) and reads 2 gets at finalization. Zebra's RocksDB options lack Zakura's
  4 GiB WAL bound.
- `state/push_block` `zebra-clone`: a model. Zebra deep-clones the blocks (with their output
  maps), the created UTXOs and the address index, which Zakura shares through `Arc`s, and
  clones the chain twice per block when the window is full (commit and `finalize`).
- `template/*` `zebra-zip317`: Zakura's selection loop is Zebra's line for line; Zebra adds a
  cached fake coinbase that it clones and parses on every selection (Zakura PR #1035 removed
  it).
- `validate/block` `zebra-model-cold`: the Zakura model's steps with Zebra's three reads per
  coin. Lookups that Zakura overlaps (64 per transaction) and Zebra awaits in sequence cost
  the same on the in-memory view, so the two models run the same work.
- `validate/block` model overstated both baselines (audit 2026-10-04). The model joined the
  scripts of each transaction before it prepared the next. The real block task does not: it
  polls all transaction futures, and the `Buffer` worker only builds each future
  (`tower` `buffer/worker.rs:170-177`). Per transaction, the lookups, `CachedFfiTransaction::new`
  and the sighash run serially on the block task. Each input script runs in a `spawn_fifo`
  that starts when the task polls it and overlaps with the next transactions
  (Zakura `block.rs:589-615`, `transaction.rs:603`, `script.rs:71-76`; Zebra `block.rs:314-346`,
  `transaction.rs:327-346`, `script.rs:61`). The model now starts the scripts without a
  join and joins after the last transaction; the unit test in `scenarios/validate.rs`
  checks that. Transparent-6500x1 cold: criterion 208 ms to 43 ms (Zakura), 206 ms to 43 ms
  (Zebra); sysbench 225 ms to 47 ms (both). The comparison with hayai (25 ms) is now about
  1.8x, not 9x. The model still has no tokio or tower cost, so it stays a lower bound.
- `blockstore/get_block` `zebra`: Zakura's row layout with Zebra's options and the
  `zebra-chain` parse.
- `sysbench` has an `Impl::Zebra` for the triples that the catalogue lists
  (`validate_block_cold` on transparent fixtures); `scenarios::build` refuses other ones.

## 2026-10-03 — Network Equihash parameters, eager verifying keys, mined parents

- The header is network-aware without a network field: the parser accepts the solution
  lengths of the known parameter sets (1344 bytes for (200, 9), 36 bytes for (48, 5)) and
  each caller checks the length and runs Equihash with the `PowParams` of its network
  (hayai-net `Network::pow`, `StandardHeaderCheck::pow`, `TemplateConfig::pow`). The
  compact-relay frame needs no change: the header delimits itself through its CompactSize
  solution length, so Mainnet frames stay byte-identical. hayaid Regtest now produces and
  parses Zakura's 36-byte headers.
- Lazy key builds on the rayon pool are gone. Only `VerifyingKeys::prebuild` builds an
  Orchard key, on its own thread, and `ready()` waits off the pool; a batch rejects a bundle
  whose key is absent (`PrepareError::Unsupported`) instead of building it. Lesson: a
  `OnceLock` initializer that itself waits on the pool must never run on a pool worker
  (reproduced: the old code hung in 1 of 3 runs of `tests/cold_keys.rs`). Cost: a node that
  crosses two upgrades without a restart lacks the second key.
- Tip events separate `mined` from `conflicting`. A mined parent leaves the template and its
  children lose the dependency in place; conflicts still leave with their descendants.
  Lesson: "invalidated" mixed two meanings, and the children of every mined transaction
  were dropped from the template while the store kept them.

## 2026-10-03 — Deployment and CI

- Testnet in Docker needs a Zakura node, so the compose project runs zakurad, and
  hayaid-testnet joins the network namespace of zakurad. The addresses of `hayaid config`
  (127.0.0.1) stay valid, and the RPC of zakurad without cookie authentication stays on
  loopback. hayaid reads `SocketAddr` values only (no host names), which rules out compose
  service names in its configuration.
- Profiles select the network (`testnet` by default through `docker/.env`, `regtest`).
  Prometheus finds the node by DNS name: the absent profile gives no target, and the
  `network` label of the rules comes from the name.
- Restarts: hayaid refuses a non-empty `data_dir` (hayai-r5q), and a failed shadow seed
  leaves files there (hayai-qmj). The restart policies stay as they must be after resume,
  the documented procedure resets `coins/` and `blocks/`, and nothing wipes state
  automatically. `depends_on: service_healthy` on the `/ready` endpoint of zakurad keeps
  hayaid from seeding from a node that still synchronizes.
- Lesson: the declared `rust-version` (1.85) does not build. hayaid needs 1.88 (upstream
  crates) or 1.91 (zakura-* forks), and zakura-chain 9.0.0 in hayai-bench needs 1.97, so
  the image and CI pin 1.97.1 (hayai-ecx). Check a declared MSRV with a build, not with
  the manifest.
- The image keeps the symbol table and drops the DWARF sections of the release profile
  (binary 480 MB → 21 MB, image 268 MB). It carries the Sapling parameters, because
  hayaid does not download them.
- cargo-deny ignores three rustls-webpki advisories, with a reason. They come through the
  `download-params` feature of zcash_proofs, which only hayai-bench uses (hayai-ehj).

## 2026-10-04 — Design review items 4 and 7: template as lane, prebuilt bodies

- Block order. A batch reference covers a run of consecutive positions, and the template
  ordered its block by weight ratio, so a new high-ratio transaction broke every batch
  after it. The selection stays by weight ratio; the block order is now canonical (depth
  over in-block parents, then txid), so a block is a function of its set. The template
  reads the parents from the inputs (`Candidate::spends`), as a receiver does. Lesson: the
  test fixtures declared `depends_on` without spending the parent; the order exposed it.
- Template as lane, as feature bit 2 and not version 3: only lane owners publish, any
  version can carry it, and a peer without the bit sees nothing new. Each template change
  is a `BatchAnnounce` of its additions (`seq` = template id) and a `CandidateAnnounce`
  that names every batch of the candidate and the removed positions. Each announcement is
  complete, so a missed one costs nothing. `CandidateBlock` = header, coinbase,
  `(lane, seq)`, removed positions, short and full ids of the additions. A block equal to
  its candidate costs 61 bytes besides the header and the coinbase (`relay/bytes_on_wire`:
  1,652 bytes in total for the 2,001-transaction block, 13,614 with short ids). Rebuild
  (`relay/reconstruct`, load 13–25): 0.67 ms (short ids 0.77 ms, batch 0.46 ms) on
  `transparent-2000x2`, 0.19 ms on `orchard-200x2`. A candidate block that does not resolve
  falls back to the full block; it is forwarded once its bytes are held, because the
  canonical order needs every input.
- Prebuilt bodies. Everything the contextual check does with the body after the coinbase
  depends on the parent only, so `prebuild_body` runs it early and
  `PrebuiltBody::commit` adds the header and coinbase rules and moves the maps into the
  layer. The full check and the prebuild share one implementation of each rule; the
  merkle and auth roots come from the branches of position 0. A shielded coinbase is a
  mismatch (its commitments precede the body's). `state/commit_prebuilt`: swap 0.04–0.36
  ms against warm `validate_block` 0.72–2.74 ms; the prebuild itself costs 0.69–2.80 ms.
  `template/own_block_commit` (to the full template on 8,000 candidates): 2.3–3.9 ms
  against 3.1–9.3 ms.
- Own blocks: hayaid prebuilds the newest template's body while idle, at most every
  200 ms (`mining.prebuild_own`, on). Candidates of peers' lanes: off by default
  (`network.prebuilt_candidates = 0`): a prebuild costs as much as the warm validation it
  saves, a lane republishes on every template change, and only an exact match pays.
- Not done: reuse of tip-relative verdicts for competitor blocks. One missed invalidation
  of that cache accepts a double spend, and after the window index the contextual checks
  cost under 3 ms.


## 2026-10-04 — Restart, Mainnet, toolchain and dependency cleanup

- hayaid resumes from `data_dir`. The coins store records its best block. `state.log` holds a
  checksummed record of the base (frontiers, value pools, history peaks, block times, new
  anchors) per coins flush, written before the flush. A restart takes the record of the best
  block, drops later records and replays the block files above the base with full
  validation. Decision: an append-only log, not one snapshot next to the coins snapshot,
  because the anchor sets grow with the chain and a snapshot per flush would rewrite them.
  The shadow node keeps its spent-outpoint set in `spent.log` (records by generation height,
  cut at the best block). A first start seeds before it creates any file and removes what it
  created if it fails before the start record. Lesson: shadow trust state (`spent`) was memory
  only, so a restart would have accepted a spent pre-start coin again.
- Fixture cache: parallel writers shared one temporary file. Each write uses a unique name
  (process id and counter) and every caller reads the file back, because Orchard proofs are
  not deterministic.
- `cold_keys` flaked with `pthread lock: Invalid argument` and SIGABRT after the test printed
  `ok` (1 run in 10 on the zakura backend). The helper thread of the test dropped the
  RocksDB handle of the harness while the main thread ended the process, and the static
  destructors of RocksDB's C++ runtime ran meanwhile. The test now joins the helper (0 aborts
  in 40 runs). The 600 s timeout was not the cause: 6 runs on 2 cores with 8 CPU burners took
  12 to 26 s. Lesson: a test must join every thread that owns a RocksDB handle.
- Mainnet is a network kind (`NetworkKind::Mainnet`), accepted in shadow and full mode with no
  code guard. Rules that hayaid does not enforce are listed in `docs/hayaid.md` and
  `docs/install.md`. Full mode on Mainnet starts at genesis and cannot synchronize.
- `rust-version`: 1.91 for the node crates (both backends; upstream alone builds on 1.88),
  1.97 for hayai-bench (zakura-chain). The CI `msrv` job checks the node crates on 1.91.1.
- `zcash_proofs` no longer has `download-params`: the only caller in hayai-bench was unused.
  `minreq`, `rustls-webpki` and `webpki-roots` left the graph, with the advisory ignores and
  the MPL-2.0 exception of deny.toml. The `multitable` bench candidate stays an off-by-default
  hayai-bench feature; deny.toml still excludes it, because `all-features` pulls the
  unlicensed git dependency into the graph. The measured result stays in the review section.
- Metrics for operations: process memory and CPU, template latency (tip change to template),
  coins cache size, build info. Logs go to stderr, with colour only on a terminal.

## 2026-10-04 — hayai-consensus, finality depth 1,000, state record version 2

- `hayai-consensus` is the one home of network parameters and of one rule set per network
  upgrade. `rules_at(network, height)` selects the rule set. hayaid, hayai-validate and
  hayai-state take the branch, the epoch and the block limits from it. Lesson: the driver
  had a fixed `BlockLimits::PRE_NU7`, so a rule that depends on the height must come from
  the height, not from a constant at the call site.
- NU7 has no rule set (owner decision). On the zakura backend `zcash_protocol` selects
  `BranchId::Nu7` from Testnet height 4,465,026, and the branch predicates of hayai-prepared
  treated it as NU6.3. `rules_at` now returns `UnsupportedUpgrade` there and the node stops.
  The only `cfg` for NU7 is in hayai-crypto (`nu7_branch`, `nu7_activation`).
- The layer window is the finality depth: 1,000 blocks (was 100), as Zebra and Zakura. A
  restart replays up to 1,000 blocks plus the blocks since the last flush, with full
  validation. The window holds 1,000 layers in memory.
- The Testnet proof-of-work limit is `0x07ff…ff` (compact `0x2007ffff`). hayaid had the
  Mainnet value `0x1f07ffff` for Testnet.
- `Layer`, `Base`, `Anchors`, `ValuePools` and the `state.log` record (version 2) have the
  fields that the difficulty rule and Ironwood need (`bits`, 28 block times, Ironwood
  frontier, anchor and pool, transparent, Sprout and deferred pools). No rule uses them yet.
  A version 1 record still loads: the new fields are empty. The fields landed in one change
  so that the parallel work items do not edit the same structs.

## 2026-10-04 — Conformance harness on published vectors

- `hayai-bench/tests/conformance_blocks.rs` runs the 90 Zebra block vectors through
  `validate_block`. `conformance_txs.rs` runs the 1,046 `zcash_script` vectors through
  `Draft::check_input`. `docs/conformance.md` holds the inventory, the design and the outcomes.
- The outcome of each vector is in `tests/vectors/expected-*.json` with the reason and the
  plan item. A test fails when an outcome changes in either direction. A work item that makes a
  vector pass must update the file. A rejection of a valid vector is a defect: it goes to
  `docs/conformance.md` and a bd issue, never to the expected file.
- The chain context comes from the vector set only (owner decision Q7): the state that hayai
  built from the earlier vectors, empty trees before an activation, published roots. A block
  that reads other state stops as `context_free` with the list of the missing state. No
  `mkcontext` binary exists, because its source was a reference node.
- Lesson: an `Unsupported` error must not end a stage. The transaction stage runs every
  transaction and reports a rejection before an unsupported rule. Without that order, a block
  with one Sapling bundle hides an invalid Orchard proof in the same block.
- Lesson: random script vectors cannot show a sighash value through the public prepare path.
  The ZIP 143, 243 and 244 sighash values need a test inside `hayai-prepared` (bd hayai-xya).

## 2026-10-04 — Embedded Sapling verifying keys, ZIP 213

- The two Sapling Groth16 verifying keys are files in `hayai-prepared/src/sapling_vk/`
  (1,636 and 1,444 bytes): the start of the official parameter files. hayaid reads no
  parameter file, `sapling_params_dir` is gone, and the image has no parameters.
  `VerifyingKeys::new()` and `prebuild(epoch, next)` take no Sapling argument.
- Provenance: `scripts/extract-sapling-vk.sh` writes the files from hash-checked parameters.
  A test compares them with `wagyu-zcash-parameters` (a test dependency, the official
  files in a crate), so the test needs no file on the machine and is never skipped.
- Lesson: `sapling-crypto` has no public constructor of its verifying key types from a
  `groth16::VerifyingKey`. The public path is `SpendParameters::read`, so the loader appends
  five empty prover vectors to the key. The encoding loads on both backends.
- ZIP 213 is in `hayai-prepared/src/coinbase.rs`, called from `draft` for a coinbase. The
  decrypt functions are generic over the authorization, so the tests use builder output
  without proofs.
- The Sprout Groth16 verifying key is not embedded: `sprout-groth16.params` (725 MB) is not
  on the machine. Zebra and Zakura ship it as `sprout-groth16.vk` (plan item A5).

## 2026-10-04 — Subsidy, funding streams, lockbox, coinbase terms (W3a)

- `hayai_consensus::coinbase::CoinbaseTerms::at(network, height)` is the one source of what
  a coinbase must pay: founders' reward, funding stream outputs, the NU6.1 lockbox
  disbursement, the deferred part, and the ZIP 236 flag of the rule set. The validator
  (`check`, `deferred_pool_after`) and the template (`hayai_template::consensus_subsidy`)
  read the same terms, so they cannot disagree. `validate_block` still uses `SubsidyRule`
  until W3b.
- The constants (address lists, ranges, numerators) are copies of Zakura's. The test
  `hayai-bench/tests/conformance_subsidy.rs` compares every schedule with `zakura-chain` and
  `zebra-chain`, and runs the coinbase of each block vector through the check.
- The founders' reward addresses are checked too, although Zakura and Zebra reach that code
  only above their checkpoints: the check then does not depend on the checkpoint range.
- Lesson: the value rule needs the lockbox disbursement. In the NU6.1 activation block the
  coinbase pays 78,750 ZEC more than subsidy − deferred + fees. A rule with only
  `total` and `deferred` rejects that block.
- Lesson: a height of an upgrade without a rule set (NU7) has no subsidy and no terms. The
  functions return `UnsupportedUpgrade`. They never apply the schedule of an earlier upgrade.

## 2026-10-04 — hayai-sync: fork-aware header chain (W6)

- The header chain is a tree in memory (96 bytes for each entry) and a checksummed
  append-only header log on disk. The position of an entry is its first-seen order and its
  order in the log, so a start that applies the log in order gives the same chain. Removed
  entries keep their position until the next start.
- Tie on equal work: the first-seen entry stays the best tip (Bitcoin Core, zcashd), not the
  larger hash (Zebra, Zakura).
- The finalized height comes from the best header tip (minus 1,000) and the last checkpoint
  that the best chain reached. It is not monotonic: an invalid block on the best chain
  moves it back. Branches that leave the best chain below it are refused and removed.
- The context-free rules and the work function come from hayai-consensus
  (`check_proof_of_work`, `block_work`). The contextual rules go through the `HeaderRules`
  trait, whose context has the fields of `hayai_consensus::ParentChain`. The chain reads the
  context from the branch of the header, not from the best chain.
- The start does not run proof of work or the contextual rules again: a record checksum
  shows that the chain wrote the header after these checks. Only `Invalid` is in the log.
  The node sets the other body states from its block state.
- Lesson: a model-based property test (random forks, duplicates, invalid blocks, finality
  depth 1 to 7) found no fault that the example tests missed, but four deliberate faults in
  the tie and finality rules each failed it at once. Keep it when the chain changes.

## 2026-10-04 — Ironwood (NU6.3), history tree version 3 (W1)

- One code path for the Orchard and the Ironwood bundle: a v6 transaction has two slots of
  the Orchard protocol, and `draft`, the batch and the contextual check name the pool
  (`Pool::Orchard`, `Pool::Ironwood`). The implementation is the same on both backends. The
  Ironwood tree has the node type and the hash of the Orchard tree (upstream
  `MERKLE_CRH_PERSONALIZATION`, Zakura `ironwood.rs` re-exports `orchard::tree`).
- The Ironwood state is not optional. Before NU6.3 the tree is empty, its root is in the
  anchor set of every base, and the pool is zero. A `state.log` record without the Ironwood
  frontier loads with the empty tree: no build accepted a block from NU6.3 before this change.
- `draft` takes the rule set of the epoch (`RuleSet::of_branch`): versions, pools and
  coinbase rules come from hayai-consensus. Lesson: the "some source, some sink" rule
  counted Orchard actions without the `enableSpends` and `enableOutputs` flags.
- The upstream parser applies the flag-bit rules and the canonical proof length. Tests
  prove this for each bundle version, because hayai has no second check of the bits.
- Lesson: a rule that the transaction format already gives (a pool that is not active) has
  no input that reaches it through the parser. Such a rule is a function of its own
  (`check_pools`) with a test on a changed rule set.
- Shadow mode: from NU6.3, a `z_gettreestate` answer without the Ironwood tree, or a start
  block without the Ironwood pool, is an error. hayai does not replace the state of upstream
  with an empty tree.
- Fixtures: generated blocks of the NU6.3 epoch (owner decision: no blocks from the
  network). An Orchard bundle of NU6.3 has padding actions only, because the pool takes no
  value and no cross-address transfer. The fixture cache is shared by the two backends, so
  each backend also verifies proofs that the other one made.
- Not done: the Orchard soft fork of NU6.1 (no Orchard bundle from Mainnet 3,363,426 until
  NU6.2) needs the network and the height in the contextual check. The total shielded cost
  of ZIP 218 belongs to the NU7 rule set.

## 2026-10-04 — Peer management: address book, connection manager, misbehaviour score

- The score type is in hayai-sync (`score`), and hayai-net depends on hayai-sync. The block
  download needs the same reasons and must not depend on hayai-net.
- Scores and bans are held for each IP address, not for each connection: a peer that
  connects again keeps its score. Points decay (1 each 60 s), so that rare small faults
  never reach a threshold.
- Only context-free faults score in the relay: a frame that does not decode, a message that
  the negotiated protocol does not permit, a header without valid proof of work. A failure
  that depends on the local stores or on the local view of the chain costs nothing. An
  invalid block from a compact-relay peer costs nothing, because that peer forwards before
  validation.
- The clock and the DNS resolver are injected (`PeerEnv`), and the address book takes the
  time as a parameter. No test reads the system time for a decision or resolves a name.
- The limits are checked under the lock of the peer set, before the `version` message.
- Review fixes: an address that responded at any time is never replaced and is dialled
  before gossip (one failure after a local outage must not expose good addresses to a
  flood); a `getaddr` answer holds responded addresses only; the key of bans, scores and the
  limit per IP is the /64 for IPv6; `Source::Peer` has the IP address, so that a peer that
  left before the validation ended is still scored.
- An address goes on to other peers only when it is news to the book. Without that rule the
  same announcement travels in circles between nodes for 10 minutes.
- ZIP 155 has no `sendaddrv2`: a Zcash peer sends `addrv2` without negotiation. The node
  decodes `addrv2` and sends `addr` only, as Zebra does.
- `TxSink::accept_tx` still returns `bool`. The node reports an invalid transaction or block
  with `Relay::misbehaved`. The verdict enum of plan item B5 waits for the driver item,
  which owns the hayaid sinks.
- Lesson: `zcash_encoding::CompactSize::read` refuses values above 0x02000000. The service
  bits of `addrv2` need a reader for the full `u64` range.

## 2026-10-04 — Difficulty adjustment and header rules in one place

- `hayai_consensus::header::check_header` is the one function for the header rules. Its
  parts are `check_contextual` (version, target limit, time rules, expected `bits`),
  `check_local_time` (only with a clock; a replay gives none) and `check_proof_of_work`.
  The relay check and the hayaid header check call the whole function. Block validation
  calls `check_contextual` on the context of the view. The replay adds
  `check_proof_of_work`. Decision: block validation does not repeat Equihash. It costs
  0.16 ms for each header on this machine, and an own block commits in 0.04 to 0.36 ms.
- A context that is too short for a rule is the result `HeaderVerdict::ContextTooShort`,
  with the rules that did not run. It is never a pass. The caller decides: a full node
  rejects (`HeaderPolicy::Enforce`), a shadow node trusts and counts
  (`HeaderPolicy::TrustShortContext`). The context is two lists, times and `bits`, because
  a seed gives 11 times and no `bits`: the time rules then run from the first block.
- Regtest follows Zakura (`disable_pow`): no hash filter, no Equihash, no expected `bits`.
  zcashd Regtest keeps the `bits` of the parent instead. A pair with zcashd needs that rule.
- Lesson: `check_pow` had no proof-of-work limit, and `expand_target` accepted a mantissa
  that a small exponent shifts to zero. Both were visible only against the vectors of
  Bitcoin's `arith_uint256` tests. Port the vectors of the reference with the function.
- Lesson: the Testnet `MTP + 90 min` rule starts at height 653,606, not at genesis. A
  network parameter that is a start height belongs in `NetworkParams`, not in a `match` of
  the node.
- The difficulty tests compare 36 generated chains with a reference implementation in the
  test (`num-bigint`, blocks indexed by height, its own compact encoding). The published
  vectors cannot test the adjustment: the longest range is 11 blocks and the rule reads 28.

## 2026-10-04 — Compact block with full ids is type 12; `Malformed` is 50 points

- The version 2 layout marked its full-id section by presence at the end of the type 6
  payload. A payload cut before the section was a valid version 1 payload, and a hop could
  remove the section. The property test `truncation_never_decodes` found it. Only the frame
  length and the legacy checksum caught a cut.
- Type 12 (`CompactBlockV2`) now carries the full-id section. Type 6 is the version 1
  layout only. The encoder uses type 12 if and only if the block has full ids, and a zero
  count in type 12 is malformed: each message has one encoding, and the codec needs no
  state of the connection. `Message::CompactBlock` is still one variant. A draft 2 or 3
  node and a draft 4 node disconnect on a block with full ids.
- Lesson: never infer an optional section from the end of a payload. The message type (or
  an explicit field) must name the layout, and every field of a layout is always present.
- The truncation test now cuts each generated frame at every byte position, and a second
  test does the same for one message of each type code.
- `Misbehaviour::Malformed` is 50 points: the first one disconnects, the second one before
  the decay bans. An honest peer with a newer protocol can send a message that the decoder
  does not know. Zebra only disconnects.

## 2026-10-04 — Mempool policy, ZIP 401 store (W11)

- Owner decision Q8: the node relays as the public network. `hayai-prepared/src/policy.rs`
  holds the admission rules (`MempoolPolicy::admit`), one `PolicyReject` for each rule.
  `docs/mempool-policy.md` gives the source of each rule and says which constants no local
  source confirms.
- The store evicts by ZIP 401, not by the lowest weight ratio. The random number generator
  is a constructor argument (`PreparedStore::with_rng`) and the victim scan is in insertion
  order, so a seed gives a reproducible eviction. The review fix stays: an ancestor of the
  new transaction is not a candidate, and descendants leave with their parent.
- The store limit is a ZIP 401 cost (`max(size, 10,000)`), not bytes. A test that sizes a
  store in bytes of small transactions now refuses every insert.
- The unpaid action limit is 50 (ZIP 317, zcashd). Zebra and Zakura use 0. It is a field of
  `MempoolPolicy`.
- No rebroadcast from the mempool: zcashd and Zebra do not have one.
- Lesson: the clock of a time rule is an argument (`insert_at(tx, now)`), so a test of
  "60 min" needs no sleep.

## 2026-10-04 — hayai-sync: block download scheduler (B2)

- `download::Scheduler` is a pure state machine: events in, actions out, the time is an
  argument. It holds no bodies and nothing on disk. The node driver maps the actions to
  `getdata`, to its body store and to the validator.
- The window is filled from `best_chain_from`, not from `next_blocks_to_download`: a body in
  memory is not `BodyKnown` in the header chain (that state cannot go back after a reorg),
  so that function would return the window again at each call.
- The memory bound counts each request as one largest block. Only the first block of the
  window can use the last 2 MB of the budget. An exception for the lowest missing block
  (the plan's text) lets memory grow without bound when validation is slow.
- Rescue and stall are two rules. Rescue (2 s) moves the requests without a penalty. Stall
  (8 s) gives the penalty. A penalty at the rescue time disconnects honest peers whose rate
  changes.
- Progress of a peer is "a body arrived". A fixed deadline from the rate at the time of the
  request penalized a peer that became slow. A peer that answers a later request first makes
  no progress for the earlier one.
- `BestHeaderTipChanged` has the committed tip of the node, because the header chain does
  not give the fork point of a committed block that left the best chain.
- Lesson: 18 deliberate faults were run against the tests. The simulation missed three
  (budget of a late body, order of answers, peer selection by speed). Each needed a test
  with single events and exact expected actions.

## 2026-10-04 — Phase 1 integration: coinbase terms, value pools, seed context, Orchard soft fork

- Block validation takes the coinbase terms from `CoinbaseTerms::at(network, height)`.
  `SubsidyRule` is gone from hayai-state, hayai-validate, hayaid and hayai-template, and
  shadow mode makes no `getblocksubsidy` request. `CheckConfig` and `ValidateConfig` have
  the network. `HeaderPolicy` has no network of its own. `CoinbaseSpec` has the network,
  and `build` returns the error of a height without a rule set.
- The contextual check maintains the transparent, Sapling, Orchard, Ironwood and deferred
  pools. No pool can be negative, and the total is at most `MAX_MONEY`. A prebuilt body
  keeps its totals, and the commit computes the pools with the coinbase.
- Lesson: a pool that the node does not know is not zero. The shadow seed fails without a
  pool that a rule reads (a `lockbox` value above zero from NU6). `state.log` record
  version 3 marks a record with every pool. A record of version 1 or 2 above the genesis
  block is refused with the instruction to remove the data directory.
- The shadow seed holds 28 blocks with time and `bits`, in the header index and in the
  base. The state record holds the same context. No header rule waits for context, and
  `hayai_shadow_trusted_bits_total` reads 0. A seed without the 28 blocks fails.
- The template `bits` on Mainnet and Testnet are `expected_bits` for the template time.
  On Testnet the value belongs to that time only (minimum-difficulty rule).
- A rule that depends on the height inside one upgrade is a rule set of its own that only
  `rules_at` selects: the Orchard soft fork is the NU6.1 rule set with the Orchard pool
  off. The epoch of hayai-prepared has no height, so hayai-state checks the pools of the
  rule set of the height. Lesson: `RuleSet::of_branch` is not the rule set of a block.
- Lesson: a test double of a consensus input hides a wrong fixture. The bench fixtures
  paid the whole subsidy to the miner. They now pay the Mainnet terms of their height
  (generator version 2), and tests change the coinbase with `Fixture::with_coinbase`.
- The template candidate counts Ironwood actions for ZIP 317 and for the block limit.

## 2026-10-04 — Checkpoints and the checkpoint path (A9, B7)

- Owner decision: do as Zakura does. The Mainnet and Testnet checkpoint lists are Zakura's
  files (clone revision `1377915`), and a block at or below the last checkpoint is verified
  by its hash. The header chain still applies every header rule to every header.
- The checkpoint path (`apply_checkpointed`) builds the layer from the parsed transactions.
  It makes no prepared transaction, because a draft has the sighash digests that only the
  scripts read. It keeps the checks that guard the state (parent, missing input, double
  spend and duplicate nullifier in the block, negative pool, header commitment) and drops
  the rules that the hash replaces.
- The path cannot prove alone that a block between two checkpoints is on the checkpointed
  chain: that proof is the header chain. The caller gives the hash of the header chain
  (`expected`), and the header commitment of the next block binds the tree roots.
- Full validation refuses a block at or below the mandatory checkpoint (Canopy − 1). The
  conformance harness therefore runs the published vectors of these heights on the
  checkpoint path.
- `ChainConfig::checkpoints` stays a field: a test of a generated chain needs its own list.
  The type is `hayai_consensus::Checkpoints`, so the lookup code exists once.

## 2026-10-04 — Sprout (A5, W7)

- Owner decision: do as Zakura does. Full validation verifies the Groth16 JoinSplits of v4
  transactions and the Ed25519 JoinSplit signature (ZIP 215 rules at every height). It has
  no verifier for BCTV14 proofs and no sighash for v1 and v2: `draft` returns `Unsupported`
  for v1 to v3, and only the checkpoint path applies these blocks.
- The Sprout verifying key is in the binary (`hayai-prepared/src/sprout_vk/`, 1,828 bytes):
  the start of `sprout-groth16.params`, written by `scripts/extract-sprout-vk.sh` from the
  hash-checked file. No crate ships the 725 MB parameters, so the test pins the hash of the
  key file, and a test verifies real JoinSplit proofs of the published block vectors.
- A Sprout anchor needs the tree, not only the root: a JoinSplit continues the tree of its
  anchor, and a later JoinSplit of the same transaction can use that output treestate. The
  base therefore keeps the frontier of every final Sprout treestate by root, in memory and
  in `state.log` (record version 4). `PreparedTx::anchors` holds no Sprout anchor.
- A base that starts above the genesis block has no Sprout treestates (shadow seed, state
  record before version 4). It does not know the Sprout state, and a block with a JoinSplit
  is `SproutStateUnknown`. Lesson: the same rule as for the value pools, a state that the
  node does not know is not the empty state.
- The state update of a JoinSplit is the same on both paths: `Commitments::sprout` goes
  through `append_leaves`, and the nullifiers and the value go through the totals. The
  checkpoint path reads no proof, so BCTV14 and Groth16 JoinSplits have one code path.
- Lesson: upstream `write_frontier_v1` takes a tree of depth 32 only. The Sprout frontier
  (depth 29) has its own `write` and `read` over the two public parts of that function.
- Lesson: the conformance stage that runs `draft` on every transaction stopped every block
  before Sapling. A transaction that only the checkpoint path applies is counted, not run.
- Not done: a sighash for v1 and v2 transactions and a BCTV14 verifier (no vector on this
  machine can test them, and no block that needs them has full validation).

## 2026-10-04 — hayaid full mode: synchronization, fork choice, reorg, speculative tip (B3)

- One path for every block in full mode. The header chain has the header, the download
  scheduler delivers the block in height order, the driver validates it. A block of the
  relay (compact relay, own block) is a body from another source of that path, not a second
  path. The relay keeps its header check on the committed window (`HeaderIndex`, pending
  headers); the header chain checks the header again when the body arrives.
- The relay sends no block `getdata` when the node gives it a `SyncSink`. An announced block
  and a failed compact block become a `getheaders` of the node.
- A body that a peer can change without a change of the block hash never makes a header
  invalid. From NU5 the authorizing data are outside the block hash, so the driver checks
  `hashBlockCommitments` before the validation: a mismatch is a wrong body (penalty, another
  peer), not an invalid block. The invalid mark is in the header log and survives a start.
- Reorg: the driver disconnects only when the delivered blocks of the new branch end at the
  best header tip, have more work than the tip, or fill the validation lookahead. Lesson:
  "more work than the tip" alone stopped the return to the first chain after an invalid
  branch, because the header chain keeps the first-seen chain on equal work.
- After a reorg the whole mempool passes the admission again, with the transactions of the
  disconnected blocks first. A check of the returned transactions alone leaves transactions
  whose inputs the reorg removed.
- The node serves the headers of the blocks that it can send, not only of the committed
  ones. Lesson: a node forwards a block before its validation; a legacy peer then asks for
  the header, and an answer without it loses the block until the next one.
- A block at or below the mandatory checkpoint waits until the header chain reaches a
  checkpoint above it. The replay of a full node uses the checkpoint path at or below the
  last checkpoint.
- The block store keeps every block by hash; the newest append at a height is the block of
  that height. `DuplicateHeight` is gone.
- The clock of the scheduler is the arrival time of each message at the relay, and a tick
  runs only when the queue of the driver is empty. A tick at the time of the driver gives a
  stall to a peer whose block waits in the queue.
- Not solved: the best header chain alone drives the download, so a header chain with more
  work and no bodies keeps the node at its tip. The relay parses transactions with the
  branch of the start height, and the verifying keys are the keys of the start epoch.

## 2026-10-04 — Template: ZIP 317 block production, branch id and limits of the height (B3)

- The selection is the ZIP 317 block production algorithm with one difference: the order by
  weight ratio replaces the random pick (the template lane needs one selection per set).
  The order has pass 1 (fee >= conventional fee) before pass 2. Pass 2 stops at 50 unpaid
  actions in the block. `Candidate::unpaid_actions` and `SetEvent::Repriced` carry the count.
  `docs/mempool-policy.md`, Template selection, lists the differences from the ZIP.
- The weight ratio uses `max(1, fee)`. The sigop budget subtracts the coinbase sigops. The
  sigop and shielded limits are `BlockLimits` of the rule set of the height: before, the
  NU7 shielded limits applied at every height. `TemplateConfig` has no limit field for them.
- No branch id is fixed at start. `CoinbaseSpec` has no `branch_id`: the build takes it from
  `rules_at(network, height)` and `CoinbaseTx::branch_id` carries it. `rebuild_block` takes no
  branch. `RpcConfig` holds the network. `submitblock` parses with the branch of the height
  that the coinbase states. A height without a rule set is an error.
- `getblocktemplate` `maxtime` is the median-time-past plus 90 min (`Tip::median_time_past`),
  from `max_time_start_height`. Lesson: a value derived from `mintime` was right only by
  accident of how the node sets the template time.

## 2026-10-04 — Review fixes: header version, duplicate txid, peer-reachable panics

- The header version is a signed 32-bit integer for zcashd. `check_version` is the one
  function for the rule, and the header chain calls it. Lesson: a field that the reference
  reads as signed needs the comparison of the reference, not only its constant.
- A body with a txid twice can have the merkle root of a valid block. Each function that
  compares the merkle root also refuses a txid twice (`hayai_wire::duplicate_txid`), with
  `ContextError::DuplicateTxid`. The error names a fault of the body, so the node driver
  must not mark the header invalid for it.
- Lesson: an assertion on an argument is a panic that a peer can reach when one caller
  takes the argument from a peer. `draft` returns `PrepareError::SpentCoins`, and
  `commit_prebuilt` checks the shape of the first transaction before the draft.
- The NU7 height is a constant of hayai-consensus, not a value of the crypto backend. A
  backend that does not know an upgrade must not make the node use older rules.
- Regtest waives the shielding rule for coinbase spends (`coinbase_must_be_shielded`), as
  Zakura and zcashd. Coinbase maturity applies on every network.
- The founders' reward check has no caller in a node: its heights are at or below the
  mandatory checkpoint, and Regtest has no founders' reward. The code stays for
  `CoinbaseTerms::at`, the template tests and the conformance tests.

## 2026-10-04 — hayai-fuzz: differential fuzzer, in-process tier (C3, W6c)

- A case is a seed block, a recipe of mutations and a chain context. hayai
  (`validate_bytes`) and the Zakura library code each give a verdict. A finding is a
  different outcome (accept against reject) or a panic. A different rule class on two
  rejects is counted and is not a finding.
- zakura-consensus and zakura-state cannot be dependencies: their `rocksdb` 0.24 and the
  `rocksdb` 0.25 of hayai-coins both link the native library `rocksdb`. The oracle links
  zakura-chain, zakura-header-chain, zakura-script and zakura-orchard, and holds a copy of
  the check files of zakura-consensus (`src/reference/zakura_consensus`, commit in its
  `mod.rs`). Rules of zakura-state that are set rules are a model in the oracle; anchors,
  chain value pools and the history tree have no oracle (`docs/conformance.md`).
- The context of a case is a `CoinsBacking` over a map, so each case has its own coins,
  nullifiers, height and network, and no case writes state.
- Lesson: a fixture must exist before the rayon pool runs cases. A lock around a fixture
  generation that uses the pool stops the pool.
- Lesson: after a mutation the header must commit to the new body, or every case stops at
  the merkle root. When the reference does not parse the block, hayai computes the roots,
  so a block that only hayai parses still reaches the rules of hayai.
- Lesson: the context must be a possible chain state. A deferred pool of 0 at the NU6.1
  activation block and coins above the money limit gave rejects of hayai that the oracle
  cannot judge.

## 2026-10-04 — Sync gaps and review fixes: upgrades, checkpoints in the node, withheld bodies, faults

- No value that depends on the height is fixed at start. The relay asks the node for the
  branch (`ChainSource::tx_branch`, `block_branch`), and the keys follow the tip
  (`VerifyingKeys::prebuild_more` after each commit, `ready()` on the driver thread before
  a batch). The store drops the transactions of the old epoch at an activation: their
  signature hash commits to the old branch id.
- A generated chain gets upgrades and checkpoints through `Network::ConfiguredRegtest`
  (`[regtest]` in the configuration). The value is `Copy` and lives until the process
  ends. Lesson: a process-wide setting was not possible, because the tests of one binary
  run nodes with different values.
- Three faults, not two (`node/fault.rs`): wrong body, invalid, local. The invalid mark in
  `headers.log` is only for a fault that the header hash commits to. Lesson: a restart
  does not remove the mark, so a capability of the node (a missing key, an unknown Sprout
  state) and a body that a peer changed (authorizing data, a merkle mutation) must never
  reach it. The body check runs before each commit path, the prebuilt path too.
- Withheld bodies: the fork choice of the header chain stays work-only, and the node takes
  a block that no peer sends out of it for a time (`mark_unavailable`, in memory). The
  scheduler does not ask a peer on another branch (`PeerForkPoint`). Lesson: a request by
  reported height to a peer on another branch gives a stall to an honest zcashd peer,
  which does not answer `notfound` for a block.
- The driver handles one batch for each turn of its loop, and the tick runs on elapsed
  time with the arrival time of the oldest queued message as its clock. Lesson: a tick
  only on an empty queue never ran under load, and a clock that is the time of the driver
  gives a stall to a peer whose block waits in the queue.
- No template while the tip is more than 100 blocks below the best header tip. Measured,
  2,000 generated blocks from one peer, flush interval 100, 3 runs each: 3,315 to
  3,430 blocks/s with a template after each batch, 3,900 blocks/s without. With the flush interval 8 of the tests each
  flush has two `fsync` calls and the rate is about 300 blocks/s.
- The mempool insert checks a tip count under the lock that the driver holds while it
  cleans the store. Lesson: "write the view, then clean the store" lets an admission on
  the old tip insert after the clean.
- Not done: compaction of `state.log` and `headers.log`; a Sprout seed for shadow mode
  (no RPC of zakurad or zebrad gives the Sprout tree); a penalty-free request to a peer
  whose chain is not known.

## 2026-10-05 — Setup of Zakura: command line, configuration keys, metrics, sync race (hayai-90w, hayai-hg9)

- One name for one setting: where Zakura has the concept, the configuration of hayaid has
  the section, the key and the value format of `zakurad`, and the old name is gone.
  `docs/zakura-compat.md` has the table of each key.
- A key of `zakurad` that hayaid does not use is not an unknown key. The parser takes it
  out before serde reads the file: a warning for a tuning key, an error for a key of the
  consensus rules, the network or a data location unless its value is the behaviour of
  hayaid. One report has all lines.
- hayaid reads no `ZAKURA_*` variable. It reads `XDG_CONFIG_HOME` and `HOME` for the
  default configuration path only, which is the rule of `zakurad`.
- A metric has a name of Zakura only with the meaning of Zakura. Lesson from a run of
  the real zakurad: its Docker build exports no `process_*` metric, and its duration
  metrics are summaries, not histograms. Read the `/metrics` output of the reference, not
  only its dashboards.
- The race files use the host network for each container and no mount of the host root:
  node-exporter reads the data file system through the two data volumes.
- Lesson: a test must not assert on a row or a metric that only a timer writes. The
  `sync_progress` row comes from the tick of the block synchronization; a node that
  reaches its tip and stops in less than one tick has none. The tests wait, with a bound,
  for the metric that the same report sets (`wait_sync_report`).
- Lesson: bash starts a background job with SIGINT ignored, and a Python child keeps
  that until it sets its handler. Stop such a job with SIGTERM and a bound.
- Not done: a run on the public Testnet, `terraform apply`, a run of
  `scripts/race_deploy.sh` against real hosts.

## 2026-10-05 — NU7 rule set (hayai-sm3)

- NU7 is one more rule set (`rules::nu7`) and exists only when the crypto backend has the
  NU7 branch id. The upstream `BranchId` has no value `0x77190ad9` and its parser refuses
  such a transaction, so the default backend keeps the stop at the NU7 height.
- The schedule (halving, subsidy, scheduled issuance, funding streams) reads a table of
  target spacing eras, not the rule set. It gives the NU7 values on each backend, and the
  comparison with `zakura-chain` runs on each backend.
- The NSM value balance is not a stored value: it is the scheduled issuance up to the
  height minus the total of the chain value pools. Lesson: Zakura adds one step for each
  block from a seed that it derives the same way, so the sum has a closed form and the
  state format does not change.
- An NSM error is a `ConsensusError` inside `CoinbaseError`, so the node driver treats it
  as a local fault (stop, no invalid mark). The coinbase value rule already bounds the
  balance; a failure of the NSM rule shows wrong pools of this node.
- `CoinbaseTerms::at` has no chain value pools and fails from the NSM reissuance height
  (Testnet 7,305,222). Block validation and the template use `CoinbaseTerms::after`. The
  node gives the total of the pools after the parent to the template in the tip event
  (`Tip::issued_supply`); the template does not read the state. A tip without the value
  is an error from that height.
- `getblocktemplate` `coinbasetxn.fee` is the negated miner share of the fees, as in
  Zakura. Lesson: a value field of the shim that repeats a value of the coinbase must
  come from the coinbase (`CoinbaseTx::miner_fees`), not from the fee total.
- Regtest has no reissuance height by the rules. `RegtestConfig::with_test_reissuance_height`
  names one for tests, as Zakura's test-only parameter does. No configuration file sets it.
- The header context is 113 blocks (the window of 102 blocks of ZIP 218 plus 11). A state
  of an older build holds 28 blocks; the difficulty rule then gives `ContextTooShort`
  until the node has 113 blocks.
- Lesson: the fuzz cases must stay below the block before NU7. That block has a rule on
  the total of the pools, and the pools of a case are not those of the chain.
- Not done: ZIP 2008 (Mainnet has no NU7 height; a test fails when it gets one), the seed
  setting of a Zakura Regtest configuration.

## 2026-10-05 — Fast CI: `baselines` feature and `slow` modules (hayai-t1r)

- `hayai-bench` has a default feature `baselines`: the zakura-* and `zebra-chain`
  dependencies, the modules and bench targets that use them, `sysbench`, and each test
  that compares hayai with these libraries. The fixtures, the functional tests and the
  published vectors need no feature.
- The CI of each push selects by cargo feature and by test filter only: `--workspace
  --exclude hayai-fuzz --no-default-features --features upstream` and `-- --skip slow::`.
  The local gate and `full.yml` stay the full run.
- Lesson: `--no-default-features --features zakura` removes `baselines` too. The Zakura
  backend with the comparisons is `--features zakura,baselines`. Without it cargo skips
  the bench targets and `conformance_nu7` with no message.
- Lesson: a dev-dependency cannot be optional. `zakura-header-chain` and `chrono` are
  optional dependencies of the feature.
- Measured with warm fixtures: the whole test run is about 560 CPU-s, of which the fuzz
  smoke run is 226 CPU-s. Only 4 tests of `hayaid` are in a `slow` module (the stops at
  random points and 3 scenarios that wait for the timeout of a peer).

## 2026-10-05 — First pair with a real zakurad: legacy peers of the Zebra family

- Until now a legacy peer was a zcashd in the tests. Zebra and Zakura differ in four
  points, and each one was a defect of hayaid (`docs/regtest-pair-findings.md`).
- They read the chain of a peer with `getblocks`, and the connection waits 6 s for the
  answer. The node answers with the hashes after the locator, or with its tip hash.
  Lesson: an empty `inv` is worse than no answer, because Zakura counts it as a stall and
  disconnects after 3.
- They answer at most 16 blocks and 1 MB for one `getdata` message and are silent for the
  other requests. The scheduler sizes a message below these limits. Lesson: a request
  without an answer is not always a stall of the peer; first compare with the answer
  limits of the reference.
- They ban a peer that sends an invalid block. A legacy peer now gets the `inv` and the
  headers of a block after its validation. The compact-relay peers still get the block
  after the header check. Lesson: "forward before validation" is a rule of the extension
  only; on Regtest a valid header costs nothing, and on a network with proof of work it
  costs one block to cut each forwarding node from its Zakura peers.
- They drop inbound requests under load. The free-again rule of the scheduler covers it.
- They announce a transaction one time, to a part of their peers, and read the mempools of
  their peers for the rest. The node now sends `mempool` to its legacy peers each 60 s.
  Lesson: an answer to a request is not enough; find out which requests the reference
  expects its peers to send.
- They disconnect a peer below the protocol version of the active upgrade. The NU7 rule
  set came without the NU7 protocol version, and only a run across NU7 with a real peer
  showed it. Lesson: a new rule set has a peer-to-peer part (version, minimum peer
  version); test an upgrade against the reference node, not only against its library.
- `getblocktemplate`: `curtime` must be a valid header time. Lesson: the clock of the node
  is not valid on a chain whose newest blocks are old.
- The RPC server has query methods (`NodeQuery`): a comparison of two nodes needs the
  state of both through the same interface.
- Zakura with the legacy stack writes no commit trace rows, so the pair measures both
  nodes from outside. A Zakura node that stops loses its newest non-finalized blocks: a
  test must not use its restart as a disconnect.

## 2026-10-05 — Unpaid action limit 0 (owner decision)

- `BLOCK_UNPAID_ACTION_LIMIT` is 0, the value of Zakura and Zebra (ZIP 317 gives 50 as
  the default). Reason: equal relay behaviour with the other nodes. The mempool refuses
  a transaction with an unpaid action, as Zakura's `mempool_checks` does, and the second
  pass of the template adds none.
- The minimum relay fee rule and the second pass stay in the code, as in Zakura: they
  decide only under a policy with a higher limit, and their tests set such a limit.
- Lesson: a test transaction needs the conventional fee of its logical actions (15,000
  zatoshis for one transparent input and one Orchard or Ironwood output).

## 2026-10-05 — Fee policy values of Zakura (owner decision, hayai-dh9)

- The policy, the store and the template use `Zip317Params::ZAKURA`: marginal fee 400
  zatoshis, 2 grace actions, weight ratio cap 13. `MIN_RELAY_FEE_CAP` is 800. Reason:
  equal relay behaviour with the other nodes. `docs/mempool-policy.md` has the table.
- `Zip317Params::ZIP317` stays for the arithmetic tests of hayai-template and hayai-rpc.
  The fixtures of hayai-bench still pay 5,000 zatoshis for each action.
- The node names the parameter set in three places (`MempoolPolicy::of`, and two
  `PreparedStore::new` calls in `hayaid/src/node.rs`). They must name the same set.
- Lesson: a policy test must state its fee as a multiple of the marginal fee of the
  policy. The tests with a literal fee of 15,000 zatoshis needed a change.

## 2026-10-05 — Regtest funding streams and lockbox disbursements (F1, hayai-op5, hayai-40v)

- `RegtestConfig` takes lockbox disbursements and funding streams with the meaning of
  Zakura's Regtest parameters. `CoinbaseTerms` is the one place that reads them, so the
  coinbase check and the template agree without a change.
- The rule of Zakura on an NU6.1 height without a disbursement is in `CoinbaseTerms`
  (`ConsensusError::NoLockboxDisbursement`): the block is not valid. hayaid also refuses
  such a `[regtest]` section at its start, because a node stops when it cannot make a
  template on its tip.
- A check that needs the activation heights (address periods) runs on a `Network`.
  `with_funding_streams` makes one from a copy of the configuration, which stays in
  memory. The stream tables stay in memory too.
- Regtest takes a P2SH address of any network, as Zakura: the script has the hash only.
- Lesson: the reference has its own rule for an absent configuration value. Compare the
  behaviour of both nodes on the empty configuration, not only on the full one.

## 2026-10-05 — Lane publication and private transactions (owner decision, hayai-m7h)

- `[mining] lane_publication`: `all` (default), `public`, `none`. The key decides only
  what the node publishes of its own template. The feature bits do not change: a bit
  states what a node can receive, and no peer waits for a candidate.
- A private transaction (`sendprivatetransaction`) is in the store and in the template.
  The relay reads the store through `mempool::PublicTxs`, which does not have the private
  transactions. This one place covers `inv`, `TxAnnounce`, `getdata`, `TxRequest`,
  `mempool` and the id form of a compact block (prefilled). The driver keeps the private
  ids out of the lane.
- A separate method, and not a parameter of `sendrawtransaction`: a Zakura or zcashd node
  ignores an unknown parameter and publishes the transaction. An unknown method fails.
- A node with `all` refuses the method. A silent public relay is worse than an error.
- Lesson: in a test with two nodes, the second node publishes its own lane, and the first
  node sends it on. A test that reads "no batch on the wire" needs a second node with
  `none`, or no second node.

## 2026-10-05 — Operator RPC methods with the fields of Zakura (hayai-m7h)

- `hayai-rpc/src/info.rs` has the shapes, `hayaid/src/query.rs` has the node state
  (`NodeQuery`). Scenario `rpc` of the Regtest pair compares each answer with zakurad.
- `stop` and `addnode` work on Regtest only, as the bodies of Zakura.
- The node holds the state after a block (tree roots, tree sizes, value pools) for the
  layers and the base only. `getblockheader` and `getblock` leave the fields of that
  state out for an older block. The node does not store the genesis block.
- Not served: `getblock` with verbosity 2 (the transaction object of
  `getrawtransaction`), `errors` of `getinfo`.
- Lessons from the comparison with zakurad:
  - Zakura answers a parameter error with code -1, not -32602.
  - The difficulty limit of Zakura is the target of the compact form of the limit. The
    full Regtest limit gives 1.0000000596, not 1.0.
  - Two Regtest producers with the same coinbase script make the same block in the same
    second. A fork test needs two scripts.
  - A test address must come from a key. A made-up Sapling or Unified address is not
    valid, and both nodes then agree on "not valid".

## 2026-10-05 — Cookie authentication of the RPC server (hayai-8g4)

- The RPC server has the cookie of Zakura: file `.cookie` with `__cookie__:<secret>`,
  mode 0600, HTTP Basic, keys `enable_cookie_auth` (default on) and `cookie_dir`. The
  default directory is `data_dir`. `hayai-rpc/src/cookie.rs` owns the file and the
  header rule. `HttpServer` removes the file at its shutdown.
- A request without the credentials gets the status 401, as zcashd. zakurad closes the
  connection without an answer. `stop` and `addnode` stay Regtest only, as the bodies of
  Zakura. The `/metrics` server stays open, as in Zakura.
- Each client in the repository reads the cookie file: the tests of hayaid, the Regtest
  pair (both nodes have the cookie, with one client code), `scripts/regtest_pair.sh`.
  The tests of `hayai-rpc` that have another subject start the server without a cookie.
- The workspace has no base64 crate: `cookie.rs` has the two functions.
- Lessons:
  - `curl -u ""` asks the terminal for a password and blocks a script. Read the cookie
    file first and stop when it is absent.
  - The server reads the body of a request before it answers 401. An answer before the
    read can be lost when the server closes a connection with data that it did not read.

## 2026-10-05 — `tip-height` prints the restart tip (owner decision, hayai-l91)

- `hayaid::node::stored_tip` is the rule of the restart without the validation: best block
  of the coins store, record of `state.log` for it, then the stored blocks that extend it.
  `replay` and `stored_tip` share `replay_end` and `stored_child`, and `StateLog::open` and
  `StateLog::resume_point` share `select`. A change of the restart rule goes into these.
- Each store has a read that writes nothing: `hayai_coins::stored_best_block` (header of the
  snapshot and scan of the log, or a read-only open of RocksDB), `BlockStore::open_read_only`.
- Lessons:
  - A read-only open of RocksDB 11.8 makes no `LOG` file and takes no `LOCK`. A test that
    compares the files before and after showed it: no logger option is necessary.
  - `zakurad tip-height` writes its error to stdout and exits with the status 0, and it does
    not take `regtest` as a network. hayaid keeps stderr and the status 1.

## 2026-10-05 — Comparison of zakurad and hayaid for each block (race package, version 2)

- `docs/zakura-measurements.md` is the reference for what each node measures. A panel or
  a table pairs two metrics only when that document gives the verdict CLOSE, and the
  panel states the difference. A quantity that one node does not measure has no pair.
- A metric with a name of Zakura must have the definition of Zakura. Three of them did
  not (`state_finalized_block_height`, `sync_block_verify_duration_seconds`,
  `sync_downloads_in_flight`), and `mining_template_rebuilt` counted another event.
  Lesson: read the code point of the Zakura metric before the use of its name.
- The block clock (`docs/hayaid.md`): one `Instant` for the reception of a block goes
  with the block from the reader thread or the relay to the commit and to the first
  template. A trace field and the gauge of the same quantity come from one clock reading,
  so a test can compare them for equality.
- Gauges of the last block have the height as a value, never as a label. Prometheus then
  cannot join two nodes on the height: the dashboard has one panel for each node (Grafana
  trend panel, X = height), and `scripts/race_blocks.py` makes the joined table.
- zakurad exports summaries. Only `_sum` and `_count` are comparable with a histogram of
  hayaid. A rule for "the value of the last block" reads the increase of both over 2
  samples, one scrape after the commit (`race:contextual_commit_seconds:last`).
- Lessons:
  - Prometheus 3 has range windows that are open on the left: `[10s]` at a scrape
    interval of 5 s has 2 samples, not 3.
  - A series with one sample for each block needs `last_over_time(...[$__interval])` in a
    panel. Without it a step above the scrape interval misses most samples.
  - The Grafana xychart panel showed "Err" for each mapping in a headless browser. The
    trend panel works with a frame that has one row for each height (join on the time,
    then group by the height).
  - zakurad with `[tracing] log_file` writes no log to the output of its container.

## 2026-10-05 — RPC caller of the race (`scripts/race_rpc_caller.py`)

- One Python program (standard library) runs beside each node in a pinned public image:
  no node image has Python, and a second compose service needs no image build. It reads
  the RPC address from the configuration of the node and the cookie for each call.
- The long poll is off by default. Both nodes count the wait of a long poll in
  `rpc_request_duration_seconds`, so a held long poll makes the dashboard mean useless.
  `blocks.md` has the client-side mean of the calls without `longpollid` in each case.
- "Template served" uses the commit time of each machine (trace row of hayaid, log line
  of zakurad) and the wall clock of that machine. No value crosses two machines.
- Lessons:
  - hayaid holds a `getblocktemplate` call only with the capability `longpoll`. With the
    `longpollid` alone it answers at once: the first caller made 190,000 calls in 50 s.
    A client loop on a long poll needs a wait when the answer comes back at once.
  - A wrong cookie: hayaid answers 401, zakurad closes the connection. hayaid removes the
    cookie file at its stop, zakurad keeps it.
  - A container that reads a file of mode 0600 of another user needs root with
    `DAC_READ_SEARCH` only (`cap_drop: ALL`).
  - Python as process 1 of a container ignores SIGTERM without a handler.

## 2026-10-05 — Header sync: silence and the idle poll (`crates/hayaid/src/sync.rs`)

- Zakura, Zebra and zcashd send no `headers` message when they have no header after the
  locator. The stall rule of the header sync disconnected each such peer after
  `header_timeout_ms`, also the only peer of the node.
- The role of the header sync, with its stall rule, goes only to a peer with evidence of
  more headers: a reported height above the best header, or a full `headers` message with a
  new header. Each other peer gets `getheaders` and no role.
- Idle poll: one `getheaders` to one peer in rotation, with a delay that doubles from
  `header_poll_ms` to `header_poll_max_ms`. A new block sets the delay back.
- Lesson: silence is a stall only with evidence that the peer has more than the node. Read
  what the other implementations send for an empty result before a timeout becomes a
  penalty.

## 2026-10-05 — First sync of Testnet: lost peers, the withheld rule, the header log

- Cause chain of the stop at height 4,393,339. A row of 2 MB blocks near 4,308,000 followed
  small blocks. A Zakura peer answers at most 1 MB of one `getdata` message, and the
  scheduler gave a stall for the other 15 requests when the peer answered a later
  message. Two stalls disconnect a peer, and the node refuses it for 10 min for each
  stall. The peer that stayed answers `notfound` for each block.
- The scheduler frees the requests after the answered blocks of a message that reached
  1 MB when the peer answers a later message. A request before an answered block of its
  message keeps the stall rule: the peer keeps that block back.
- The withheld rule ended an exclusion at each `headers` message of the excluded chain.
  The exclusion itself sends `getheaders`, so the node did one exclusion each second.
  Only a peer that connected after the exclusion ends it with its headers.
- An exclusion moved more than 65,536 headers of the best chain into the side set. The
  bound then removed the newest of them, and the chain wrote a second record when a peer
  sent them again. The bound does not count excluded headers.
- The header log is an operation log: a start must get the entries of the run. A header
  that the chain removed and accepts again gets a mark record, not a second header
  record. Without the mark a start does not know that the header came back.
- Lessons:
  - A state in memory only (the exclusion) must not change what the log replay does. Each
    removal that a run does and a start does not do gives a log that the start refuses.
  - A rule for the limits of another implementation needs a test with a change of the
    block size, not only with one size.
  - A rule that sends a request must not take the answer to that request as news.
  - `peers = 1` in the warning shows the cause in the first line. A debug log was
    necessary to find it.

## 2026-10-06 — Race sidecar: one method on both machines

- A compared quantity comes from one program with the same method on both machines (the
  sidecar, `scripts/race_sidecar.py`), not from the metric of each node. Node metrics
  with one name had different meanings: `rpc_request_duration_seconds` contains the wait
  of each long poll on both nodes, and zakurad has no process metric.
- Resources of a node come from the cgroup v2 of its container. A fixed cgroup parent
  (`race-<node>.slice`) gives a known path, and a read-only mount of `/sys/fs/cgroup`
  reads it without privilege and without the Docker socket. `io.stat` lists a
  device-mapper device and its disk: count only devices without `slaves`.
- The live Zakura value reuses the calibration of `scripts/race_blocks.py` on bounded
  recent rows. Two programs with one function cannot drift apart.
- A textfile with its own label `node` needs `honor_labels` on the node-exporter job, or
  Prometheus renames the label to `exported_node`.

## 2026-10-06 — Wallet index (hayai-1bd)

- Optional, one key (`[state] wallet_index`), off by default. Zakura has no such key: its
  archive mode always writes the indexes. The index needs the chain from the genesis block,
  so a node turns it on with an empty `cache_dir`.
- The block is its own access list: the validation keeps the coins of the inputs in the
  layer (`Layer::spent_coins`), and the driver takes them out before the push. The index
  reads no coin, and the balances are merge operands, so no write reads first.
- A writer thread with a bounded queue keeps the work off the driver. The waiting blocks go
  in one write batch. Each block has an undo record, so a reorg and a start undo without
  the block files.
- Consistency: the durable index holds the base block before the coins store names it; a
  start undoes the index to the base, and the replay indexes the blocks again.
- The first version drained the queue and synced the index log before each coins flush,
  and the driver waited: 144 s of a 24 min Testnet sync, almost all of it the sync (one
  block for each write batch, 15 µs for each write). The sync now runs in the background
  from one flush to the next: a sync holds the next base when its tip is at or above the
  base and no undo after its request went to or below the base. The finality depth above
  the flush interval makes that the usual case. Rule: an order of two writes needs only
  the end of the first write before the start of the second, not a wait in the driver.
- Open: the write-ahead log is on. The variant without the log (an atomic memtable flush of
  all column families at each persist) is not measured yet.
