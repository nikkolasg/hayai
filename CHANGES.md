# CHANGES

Design decisions and lessons, at the level of behaviour. The file does not record bug fixes.

## 2026-10-03 — Repository created

- Scope: the performance core of a miner node, not a complete node. The core has these parts:
  - prepared transactions
  - bulk validation
  - the coins cache
  - layered state
  - compact relay with lanes
  - live templates
  - a flat block store.

  Networking, RPC and full rule coverage come in a later stage.
- Cryptography comes from the upstream Zcash crates. `zakura-*` forks are allowed only as
  benchmark baselines inside `hayai-bench`, so a bug in the forks of Zakura cannot reach
  hayai. (The `hayai-crypto` facade, below, replaced this rule the same day.)
- Every performance claim has a benchmark against the code of Zakura or against a faithful port
  of its data layout. The report labels each estimate as an estimate.

## 2026-10-03 — hayai-sinsemilla and hayai-trees

- MerkleCRH^Orchard uses a position-weighted table (`[2^(51-i)] S(j)`, 4.8 MiB, position-major).
  With this table, a hash is 52 additions. Unlike Zakura, hayai keeps the exceptional cases of
  the incomplete addition:
  - Each table entry also stores `x([2] T)`, so `A_i = ±S[m_i]` is one comparison.
  - A zero chord denominator separates the doubling case from `⊥`.
  - The first word is a precomputed start point for each level.

  Tests force every case through a custom Q (`Table::new(q)`). This is the only reason for the
  Q parameter of the table.
- 2 evaluators:
  - a Jacobian scalar path, with its own mixed addition, so that the exceptional test shares
    the `Z²`
  - an affine lane path with one Montgomery-trick inversion for each column.

  Field inversion in upstream `pasta_curves` is a plain square-and-multiply (~380
  multiplications), and this cost sets the crossover. Lanes give a gain only from a few dozen
  lanes for each thread. Thus `merkle_crh_orchard_many` hashes small inputs with the scalar
  path in parallel, and cuts large inputs into one lane chunk for each rayon thread.
- Field inversion decided the numbers. Upstream `pasta_curves::Fp::invert` is
  square-and-multiply (4.0 µs). `invert_vartime` is a 62-bit-divstep Bernstein–Yang port of
  `modinv64` of libsecp256k1 (0.75 µs). One multiplication checks every result. On a failed
  check, the code uses the upstream inversion, and `invert_fallbacks()` counts these cases. Thus
  a bug in the fast path can only cost time, never correctness. The lane crossover moved from
  ~32 to ~12 lanes for each thread.
- The scheduler of tree appends makes one rayon task for each aligned block of `2^b` leaves
  (about 2 blocks for each thread, at most 64 leaves). That thread hashes the levels inside
  the block. Then the code hashes the block roots level-synchronously across subtrees.
  Level-synchronous batches over the whole block (10 pool barriers for each append) scaled
  poorly above 8 threads.
- The 4.8 MiB table does not fit L2, so both paths prefetch the entries of the next steps. The
  addresses depend only on the message words. The gain is −14 % on the scalar path with
  varied inputs. A benchmark of a hash must cycle through inputs, because a repeated input
  keeps its 51 entries in L1.
- Upstream `MerkleHashOrchard::combine` computes the Q of the domain again by hash-to-curve on
  each call. Thus the upstream baseline is ~120 µs for each hash, not the ~18 µs of a cached
  domain.
- `hayai-trees` keeps the upstream `Frontier`, node and anchor types. `append_many` does these
  steps:
  1. It expands the frontier into slots for each level.
  2. It hashes each level of the new perfect subtrees as one batch.
  3. It merges the carries.

  The fast hasher computes the root, not `Frontier::root`, which calls upstream `combine` 32
  times. Orchard nodes go into the hasher through their canonical bytes, because
  `MerkleHashOrchard` does not expose its field element.

## 2026-10-03 — hayai-wire, fixtures, hayai-blockstore

- The transaction boundaries do not need the parser. `scan::tx_wire_len` walks the v1–v6 wire
  layout with length arithmetic only (CompactSize counts, fixed component sizes, proof
  lengths). Thus `RawBlock::parse` first finds the limits of every transaction, and then runs
  upstream `Transaction::read` on the slices in parallel. `RawBlock::parse_sequential` stays as
  the reference implementation and the bench baseline. Upstream `Transaction::read` itself is
  slow (`Vector::read` reads scripts and proofs 1 byte at a time), and it always computes the
  txid. The parallel parse absorbs this cost. The code hashes the ZIP 244 authorizing digest
  from the scanned ranges, not from a second walk of the parsed form.
- Lesson: to drop a block that the code parsed in parallel costs more than the parse. glibc
  frees chunks that other threads allocated in their arenas: ~2.8 ms against ~1.1 ms for
  6,500 transparent transactions. A binary must choose its allocator for this cost. A
  benchmark that drops the result inside the timed region measures that cost.
- The upstream `test-dependencies` feature (proptest generators and ZIP 143/243/244 vectors)
  requires `proptest < 1.7`, but the workspace pins 1.11. Thus the vectors are copies under
  `crates/hayai-wire/tests/vectors/`. The tests assemble generated transactions at the byte
  level, with `Transaction::read` as the oracle.
- The fixture epoch is NU6.2 (`FIXTURE_HEIGHT` 3,400,000, v5 transactions). It is the last
  epoch in which an Orchard v5 bundle can contain only bare outputs that a transparent input
  funds. Thus the generator needs no Orchard note trees and no witnesses. NU6.3 moves to v6 and
  disables cross-address transfers for the Orchard pool. Orchard bundles use dummy spends and
  the empty-tree anchor. A validator that checks anchors must seed that anchor.
- The sighashes of a transaction with transparent inputs commit to the spent coins (amounts and
  scriptPubKeys). Thus a parsed `Transaction` cannot make its own sighash. The fixture tests
  attach the funding coins again through a custom `Authorization` marker. The validation code
  will need the same construction.
- The block store does not fsync each record for each block. The store writes the index after
  the data, so the index never points to bytes that are not written. After a crash, later
  appends simply follow a tail that no index entry points to.

## 2026-10-03 — hayai-coins

- The coins cache follows `CCoinsViewCache` of Bitcoin Core:
  - The key is the outpoint.
  - Coins that a block creates and spends before a flush never go to disk.
  - Each flush is one write batch.

  An index of dirty entries keeps the flush cost at O(block), not O(cache). The first
  measurement went from 84 ms to 51 ms for each block.
- hayai applies the RocksDB point-lookup settings by hand. `optimize_for_point_lookup` replaces
  the table factory and thus removes the Ribbon filter and the pinning settings. `multi_get`
  runs in chunks of 256 keys on the rayon pool, because the MultiGet of RocksDB uses one
  thread for each call.
- Coins and nullifiers flush in separate batches. If atomic block finalization becomes
  necessary, it is one trait method to add (`write_block_batch`).

## 2026-10-03 — hayai-relay and hayai-template

- A compact block points to a prefix of the non-prefilled positions by batch ids, and to the
  rest by 6-byte short ids. An unknown batch makes the whole reconstruction fail, because its
  length is unknown. The node then requests the batch and does not guess indexes. The code
  assembles the reconstructed `RawBlock` from retained bytes, and parses only the prefilled
  and the requested transactions. Lesson: test helpers that derive ids from the content make
  "different" test batches identical.
- The template selection is a deterministic greedy selection by ZIP 317 weight ratio
  (fixed-point, 32 fractional bits). Each pick records a rollback position, so incremental
  updates are exact. A randomized test compares them with a build from zero. The weighted
  random sample of ZIP 317 is a recommendation only. A deterministic selection gives identical
  templates across pool instances, and cache hits for own blocks. The caller supplies the
  subsidy and the funding streams.
- `Removed` for an unknown candidate does nothing, because tip events drop conflicts before the
  store does. `zcash_transparent 0.10` exposes `zcash_script 0.4` types, so that version sits
  next to 0.6 (the Rust interpreter that the verification uses).

## 2026-10-03 — hayai-crypto: swappable crypto backend

- Decision: the crypto stack is a feature choice, not a fixed decision. The reasons:
  - The upstream kernels are slower (halo2/orchard verification, Sinsemilla, field
    inversion).
  - The independence target is the node, not every field multiplication.
  - Upstream absorbs the kernel work of Zakura (equihash PRs librustzcash #3116/#3117/#3119).

  `hayai-crypto` re-exports the upstream crates (`upstream`, default) or the `zakura-*` forks
  (`zakura`, `=2.2.0`). No other crate names a crypto crate in its manifest.
- Feature plumbing: every crate has `default = ["upstream"]`, plus `upstream`/`zakura`
  features that forward to its hayai dependencies. Every edge between crates is
  `default-features = false`. Lesson: `--no-default-features` only disables the defaults of
  the packages that the command line selects. A dependency edge with default features enables
  them again, and this gives the "mutually exclusive" compile error. The forwarding tables are
  the cost of a working `cargo test -p <crate> --features zakura`.
- The forks have the same API for everything that hayai calls (same module paths,
  `Fp::to_repr` / `from_raw`, the Sinsemilla constants, `BatchValidator`, `Transaction::read`).
  The exception is the RNG line. ff/group 0.14 and the zakura builders and validators take
  rand_core 0.10 generators, and these traits have different names (`Rng` for the core trait,
  `SysRng` fallible by default). `hayai_crypto::rng` resolves that in one place. Code that
  mixes a backend RNG with draws in the style of `gen_range` keeps 2 generators (one for each
  rand line). It does not import 2 traits with the same name.
- `hayai-bench` keeps the renamed `zk_*` forks as baselines. Under the `zakura` feature, the
  hayai side and the baselines are the same crates. Benchmark ids carry
  `hayai_crypto::BACKEND_SUFFIX` (`hayai` vs `hayai-zk`, `upstream` vs `zakura-lib`), so both
  backends fit in one report.
- First numbers on the forks:
  - Cold validation of an Orchard-heavy block: 140 ms → 78 ms (halo2 batch verification).
  - MerkleCRH and tree appends: within noise, because the own kernels of hayai do that work on
    both backends.
  - `Fp::invert` of zakura-pasta is already a 62-bit divstep (0.63 µs against
    `invert_vartime` 0.71 µs). Thus the lane crossover argument applies only to upstream.

## 2026-10-03 — hayai-net and hayai-rpc

- The node negotiates the compact relay extension inside the legacy protocol (`zcmpctver`
  after `verack`, then relay frames inside `zcmpct`), not on a second stream or port. The
  results:
  - There is one framing and one size discipline.
  - With the extension disabled, the node is byte-for-byte a legacy node.
  - Every block uses the legacy path, so that path cannot decay.

  The service bit is `1 << 26`, because P2P v2 of Zakura uses `1 << 24`.
- The relay policy is "both paths, always". Every block, from any origin, goes through one
  path:
  1. dedup by hash
  2. header check
  3. `CompactBlock` to extension peers, `inv` to legacy peers
  4. the validator, one time.

  The node forwards a compact block before the reconstruction. It tells legacy peers when the
  body exists. Peers that never send `zcmpctver` stay legacy.
- The network layer uses `std::net` threads (one reader for each peer, a bounded outbound
  queue, one ticker), not tokio. The reasons:
  - The work of the relay for each message is small and synchronous.
  - The rest of the workspace is sync and uses rayon.
  - A miner node has tens of peers, not thousands.

  If that changes, the `Transport` trait is the seam.
- The `Codec` of Zebra is `pub(crate)` behind a private `protocol` module. Thus a differential
  test against it is not possible from outside. The tests check the codec against the byte
  vectors of Zebra (addr, MSG_WTX, version offsets) and against hand-built frames. zebra-network
  is not a dependency.
- `getblocktemplate` is a shim over the live template (`TemplateFeed`, which every
  `TemplateUpdate` feeds):
  - The node never builds a template again for a call.
  - `longpollid`/`workid` are the template id.
  - For `submitblock` with a `workid`, the node builds the block again from the stored
    template, so the own-block path applies.

  Long polls wake at once on tip events (the empty template first, then the full template).
  On set changes they wake after a delay, as the mempool poll of zcashd does.
- The HTTP server is hand-written (POST + Content-Length + keep-alive), not a crate. Pool
  software needs nothing more, and the dependency graph stays std-only.

## 2026-10-03 — CPU and memory review of the parallel paths

- The first system benchmarks overstated every parallel path. The counting allocator of
  `sysbench` updated 4 process-wide atomics for each allocation. 32 threads contended on that
  cache line. This changed the 6,500-transaction parse from 2.1 ms wall / 33 ms CPU to
  7.5 ms / 177 ms (IPC 0.24). The counters are now stripes for each thread (256 padded slots).
  Each thread adds its live bytes into the global peak every 64 KiB. Lesson: a measurement hook
  on the allocation path is itself a shared resource. When a parallel section shows an IPC far
  below the sequential one, check the harness before the code.
- Policy of the owner, after the corrected numbers: more CPU is acceptable where hayai gets a
  better wall time. No change gives wall time in exchange for CPU. A rayon pool of 16 threads
  (physical cores, `sysbench --threads 16`) decreases the CPU by 30–70 % on every scenario.
  But it loses 10–15 % wall time on the proof-heavy blocks, because halo2 batch verification
  scales across the SMT threads. Thus the pool stays at one thread for each logical CPU.
  tree_append_2048 at 2.18 vs 1.95 ms was noise: 3 runs of 100 iterations give 2.22–2.37 ms
  against 2.24–2.36 ms.
- mimalloc as the base allocator (`hayai-bench` feature `mimalloc`, off):
  - Wall time stays within the noise between runs on this machine (parse −11 %; template,
    tree_append and warm validation ±30 % in both directions across 2 runs).
  - RSS is +30 to +230 MiB on every row.

  On these numbers, hayai does not adopt mimalloc and does not recommend it for `hayaid`. The
  feature stays, so that a later run on a quiet machine can repeat the measurement.
- The default block cache of the coins store went from 1 GiB to 256 MiB. The 13,000-input
  lookup on the 2 M-coin store takes 3.3 ms at 256 MiB and 3.15 ms at 1 GiB. 32 MiB gives
  20 ms and 317 ms of CPU, because the filter, index and data blocks thrash. Thus the cache is
  not the place to save memory. The 333 MiB RSS of `coins_commit` is that cache with the whole
  160 MiB store in it, as designed.
- Allocation volume:
  - The code built the output map of the block 2 times for each block (validator and
    contextual check), and a third time into the layer. Now `block_outputs` builds it one time,
    and `contextual_check_with_outputs` moves it into the layer. Warm validation of the
    6,500-transaction block: 14.8 → 8.4 MiB, 26k → 13k allocations, 5.1 → 4.0 ms.
  - The code copied the spent scripts of a transaction 3 times into the sighash context. Now
    `SpentInputs` shares `Arc` slices. The upstream `TransparentAuthorizingContext` still
    gives owned vectors for each sighash.
  - The code decodes coins directly from the pinned slices of RocksDB, and one buffer serves a
    whole flush (`coins_commit` 52k → 27k allocations, `coins_lookup` 39k → 27k).

  The remaining allocations in cold validation come from the upstream APIs:
  - A `Draft` is 3.4 KiB (the transaction retyped with its spent coins for `signature_hash`,
    plus its digests).
  - `Transaction::read` and the script interpreter allocate for each push.
  - Orchard batch verification inside halo2 allocates ~450 MiB for each 330-action block.

## 2026-10-03 — Review fixes

- Lesson: an upstream parser can drop bytes that consensus needs. `Transaction::read_v4`
  replaces `valueBalanceSapling` with zero when a v4 transaction has no Sapling spends or
  outputs. Thus the parsed form cannot enforce the "MUST be 0" of §7.1.2. The wire scanner
  already walks every byte, so it is the place to recover such fields (`RawTx::
  v4_value_balance_without_components`). The rules are then checked in `draft`, not in the
  parser, so the parse step stays byte-compatible with upstream.
- The first implementation of the rules came from memory of the maturity check alone. But
  `CheckTxInputs` of zcashd has a second coinbase rule (spends only to shielded outputs). To
  port a check, read the whole upstream function. Then list every reject reason that it can
  give.
- The node forwards compact blocks after the reconstruction, with new short ids from a local
  nonce (BIP 152 / Bitcoin Core behaviour). It does not forward them before the
  reconstruction with the short ids of the sender. The reasons:
  - A relayed nonce lets one sender cause collisions in the whole network.
  - The receiver cannot compute short ids again for transactions that it has not resolved.

  A merkle mismatch is a short-id collision and costs the sender nothing. The node then gets
  the full block over `getdata` on the same connection. Each wait on a peer has a timestamp
  and moves to the next announcer.
- The conflict detection of the prepared store must mirror every uniqueness rule for each
  block that the contextual check enforces (outpoints and nullifiers). If not, the template
  can build a block that the node itself rejects.
- Data-structure candidates: measured on this machine with `benches/structures.rs` on the key
  shapes of hayai (random 32-byte txids and nullifiers, 36-byte outpoints, 64-byte wtxids,
  6-byte short ids). The comparison does not use advertised numbers. Kept:
  - `ahash`. foldhash fast/quality and rapidhash are 1.2–1.8x slower to build, and 1.5–4x
    slower on hits and misses at 13k and 1M keys. The AES path of ahash hashes these keys in
    1 or 2 rounds.
  - The ahash sets for each layer. A sorted `Vec` with binary search saves 23 % for each
    layer. But it is 7x slower to build, and 29x slower on the 100-layer walk of 13,000
    lookups that mostly miss.
  - `dashmap` for the prepared store. papaya is equal on one thread, which is the validation
    path. papaya is 1.9x faster under 32 readers plus a writer, but costs one heap allocation
    for each entry. scc is 1.3x slower on one thread.

  Rejected:
  - `SmallVec<[_; 4]>` for the lists of each transaction. It is 2.5–8x faster than `Vec` for
    1–8 items in isolation. But the lists are empty and not allocated for transparent
    transactions, and they are 3 of ~11 allocations for a shielded one. The inline storage is
    144 B against 24 B for each field on every stored `PreparedTx`.
  - `multitable` for the short-id index. It is 1.1–1.8x slower to build, 1.1–1.3x slower on
    hits and 1.8–2.3x slower on misses, for 4–34 % less memory. The table never grows. Thus
    about one build in 10,000 at 100,000 keys refuses an insert, and the code must build it
    again with a new hasher.

  The no-resize property of `multitable` fits only the short-id index for each block among
  the structures of hayai. The measured numbers exclude it there. It is never a candidate for
  consensus state.

## 2026-10-03 — Compact relay version 2: forward on the id list

- To forward after the reconstruction (the review fix above) costs one `BlockTxnRequest` round
  trip at each hop for every transaction that the hop does not have. The grinding attack that
  this fix closed targets short ids only. Batch ids and WtxIds are content-addressed, so a hop
  forwards them unchanged. A WtxId (`txid || auth_digest`) lets a node check both header roots
  from the ids alone. Thus version 2 adds a full-id section to `CompactBlock`. The node
  forwards the block when the id list matches the header:
  - the merkle root, always
  - `hashBlockCommitments`, when the history root of the parent is known
    (`HistoryRootSource`; `None` counts as "forwarded without auth root").

  A root mismatch forwards nothing, and the node gets the full block, as before. The node
  then requests the bytes with `TxRequest` from every announcer. The validation waits; the
  forward does not. `relay/forward_latency`: 21–22 ms for each hop at a 20 ms round trip in
  version 1, against 0.4–2.5 ms in version 2.
- Frame layout: the full-id section is at the end of the frame, and only when it is not empty.
  Thus one decoder reads both versions without state for each peer (the transport decodes
  `zcmpct` before it knows the version of the peer). Full ids carry their own index, as
  prefilled transactions do. Thus a new transaction in the middle of the block costs 58 bytes,
  and the positions around it do not change.
- A sender selects the form for each peer from what that peer announced, and when:
  - new (announced less than 3 s ago and not announced back) or never announced → full id
  - not in the local store → prefilled
  - all other cases → short id.

  One `CompactBuilder` computes the short ids one time for each block and selects for each
  peer. Version 1 peers keep the version 1 forms. They get the block after the body is
  complete.
- A node that forwarded on ids must send the bytes to its peers. The node answers a `TxRequest`
  for a transaction of a block that it still completes when the bytes arrive. It indexes
  retained bodies by WtxId for later requests. The node floods received batch announcements
  one time for each batch id to lane peers, after it holds every transaction of the batch.
- Lesson: the `TxRequest`/`Tx` path must feed pending blocks before the mempool sink decides.
  If not, a policy rejection can starve a block of bytes that the node already received.
- `block_commitments` (ZIP 244) is now in `hayai-wire` next to `auth_data_root`.
  `hayai-validate` keeps a copy until it uses the shared function.

## 2026-10-03 — Design review items 1, 5a and 6

- Window index (`hayai-state/src/window.rs`). The layer walk cost one probe for each layer and
  each key. Measured with the new `validate/block_windowed` bench (100 typical layers under the
  fixture block, `chain_with_layers`):

  | block | before | after |
  |---|---|---|
  | warm transparent-6500x1 | 13.5 ms | 4.4 ms |
  | cold transparent-6500x1 | 59.3 ms | 31.2 ms |
  | warm mixed-2000x1-100x2 | 5.2 ms | 2.4 ms |
  | warm transparent-1000x2 | 2.9 ms | 0.8 ms |

  The windowed numbers now equal the numbers with zero layers. `state/
  lookup_through_window` (13,000 lookups, 100 layers): walk 13.5 ms, index 0.36 ms.

  Design:
  - The index holds the contribution of the newest layer for each outpoint, and the oldest
    layer for each nullifier.
  - The maps of each layer stay the source of truth. Thus a pop derives the popped keys again
    from the remaining layers, and the index does not store shadowed entries.
  - The index carries its tip. A view with another tip walks its own layers, so a stale view
    never reads the entries of another tip.
  - The finalization absorbs a layer into the base before it drops the index entries of the
    layer. Thus a reader that misses the index finds the effect of the layer in the base.

  The walk stays public as the reference (`get_coins_by_walk`) for the bench and for the
  property test. The property test makes random push/pop/finalize histories and checks every
  view that it takes. Lesson from that test: distinct blocks need distinct hashes, also in a
  test. A pop and then a push at the same height with the same hash looks like the same tip to
  a stale view.
- The validator read the inputs of unknown transactions, and the contextual check read every
  input again. Now `resolve_inputs` runs one time, and the drafts and the check share the
  result. `check_parent` runs before the read, so a block for another tip costs no state round.
- Persistence off the read path. A flush held the write lock of the base during the RocksDB
  write. The flush now has 3 phases:
  1. `begin_flush`, under the lock, takes the dirty index as the generation in flight. The
     entries stay in the map, marked not fresh, so a spend during the write becomes a
     tombstone for the next flush.
  2. Outside the lock, the code writes the generation as one `WriteBatch` with the nullifiers
     and a `best_block` record.
  3. `end_flush`, under the lock, marks the entries clean. It skips the entries that the
     writer changed again.

  Reads during the write hit the map, so the coins side needs no lookup path for the
  generation. The nullifier sets probe the generation as a second set. The record is the
  recovery point. The batch is atomic, so the disk holds the state after exactly that block,
  and a restart replays the later blocks.
- Stage graph. After the drafts, 3 rayon tasks run: the scripts, the shielded batch, and the
  contextual check with the trees. `Draft` shares its `PreparedTx` through an `Arc`, so the
  check runs on it while the scripts still borrow the draft. The verdict is the first error in
  stage order, so it does not depend on the schedule. Cold measurements in the same minute,
  sequential vs concurrent, on a loaded machine (load 13–23):
  - mixed-2000x1-100x2: 104.2 → 94.6 ms
  - orchard-165x2: 150.5 → 148.4 ms.

  Against the quiet baseline of the morning (95.3 and 136.4 ms), the mixed block reads
  93.9 ms and the Orchard block 146.7 ms. This difference is the drift of the machine, not the
  change. The stages that overlap the shielded batch cost 5–6 ms on the mixed block, and this
  sets the limit of the gain. The 10–20 % of the review assumed a larger serial share.
- Eager keys. The build of the Orchard verifying key takes 0.6 s on 32 threads and 1.7 s on
  one thread, in a release build of upstream `orchard` 0.15.5. Nobody measured the "tens of
  seconds" in the old comment. `VerifyingKeys::prebuild` builds the keys of the active and the
  next epoch on a background thread, and `ready()` waits.

## 2026-10-03 — Design review item 5b: in-memory coins backing

- `MemBacking` keeps the coin set and the nullifier sets in memory. The persistence has 2
  parts:
  - an append-only log: one CRC32C-framed record for each write, synced for each record by
    default
  - snapshots of the whole set, renamed into place.

  Recovery = snapshot + log records with a later sequence number. The recovery forgives only a
  torn last record, and it reports that record.
- Coin layout, measured on 2,002,000 P2PKH coins with the counting allocator (heap bytes for
  each coin, parallel build, 13,000-key `get_many`):

  | layout | bytes for each coin | build | lookup |
  |---|---|---|---|
  | chosen: dense shards (69-byte entries in a vector + `HashTable<u32>` of positions, 256 shards) | 79.7 B | 57–81 ms | 0.67–0.96 ms |
  | rejected: `HashMap<OutPoint, Coin>` | 235 B | | 0.33–0.60 ms, no allocation for each lookup |
  | rejected: one compact `HashMap<[u8; 36], packed>` | 147 B | 0.34–0.71 s on one thread | |
  | rejected: the same, sharded | 147 B | | |

  Inline compact tables pay 70 bytes for each power-of-two bucket. At 2 M coins the load
  factor is 0.48. At mainnet size (27.3 M), every shard crosses the next doubling near 29.4 M
  coins at the same time. The dense layout pays 5 bytes for each bucket. The lower limit of a
  lookup is the script allocation that `Coin` requires.
- Nullifier layout (2 M nullifiers): a sorted run for each shard costs 32.1 B and 88–102 µs for
  each 1,000 lookups. Sharded ahash sets cost 69.1 B and 5 µs. hayai keeps the runs: they use
  2 GB less at mainnet size, and a block checks a few thousand nullifiers at most.
- Lesson: take one lock for each shard and each batch, not for each key. A lock is a locked
  read-modify-write. On x86 it orders the loads around it, so the misses of consecutive keys
  stop overlapping (13,000 lookups on one thread: 1.57 → 1.18 ms).
- Checksum: CRC32C 8.1–8.6 GB/s against BLAKE2b 0.93–1.0 GB/s on the 138 MB snapshot. The
  checksums detect damage. They do not stop an attacker who can write the files.
- `coins/commit_block/13000`: 22.6–28.7 ms with a sync for each record, 16.8–17.4 ms without,
  against 64–76 ms on RocksDB (loaded machine, load average 8–18; back-to-back runs).

## 2026-10-03 — hayaid, hayai-trace, /metrics

- The Regtest of Zakura does not require proof of work (`disable_pow`: solution shape and
  compact target only, no hash filter, no Equihash). Thus the Regtest producer sends a null
  solution, and hayaid needs no (48, 5) solver. zcashd Regtest verifies both and would need a
  solver. Lesson: hayai-wire accepts only 1344-byte solutions, so no hayai node can parse a
  Regtest header of Zakura or zcashd (36 bytes). The gap covers hayai-wire, hayai-relay,
  hayai-net and hayai-template (`docs/hayaid.md`, Regtest). Until the fix, hayaid pairs only
  with hayaid.
- The relay forwards a block after its header check and before its validation. Thus a child
  can arrive while its parent waits in the queue of the driver. The header index therefore
  holds pending headers (checked, not committed). The driver holds a block with a pending
  parent until that parent commits. Without this, a burst of blocks broke the follower at the
  second block, because hayai-net does not synchronize.
- Shadow mode reads coins from upstream with `getrawtransaction`, not with `gettxout`.
  `gettxout` answers for the upstream tip, which runs ahead of hayai, so the coin that the
  block under validation spends reads as spent. The node compares the tree roots of each block
  with the `z_gettreestate` of upstream. This makes "an unknown anchor is older than the start
  height" a fact, so the node accepts and counts such anchors. Every trust limit has a counter
  and a trace field. A disagreement stops the node.
- Metrics are a small atomic registry with a hand-written exposition in hayai-rpc, not the
  `metrics` crate. The names follow the exporter of Zakura where the meaning is the same.
- The trace writer follows zakura-jsonl-trace (bounded queue, count of dropped rows, flush and
  fsync every 1 s, one file for each table). It adds `unix_us`, so the traces of 2 processes
  join by time as well as by hash.

## 2026-10-03 — Design review item 2: speculative tip and ZIP 221 history tree

- The history tree uses upstream `zcash_history` 0.5 (the crate of the Zakura node) through the
  facade. `HistoryState` keeps only the MMR peaks, as ZIP 221 nodes, plus the node count and
  the upgrade. The peaks are sufficient to append and to compute the root. Each layer and the
  base hold one state, so a reorg needs no truncate. Tests:
  - the zcash-test-vectors V1 and V2 vectors (peaks, root and work after each of 16 appends)
  - the real mainnet headers after the Heartwood (903,000) and Canopy (1,046,400) activation
    blocks.

  An activation block starts a new tree, so the next header commits to a one-leaf tree that
  needs no earlier peaks.
- A ZIP 221 detail that the code follows: block `n` commits to the tree from the last
  activation before `n` up to `n - 1`. Thus an activation block commits to the whole tree of
  the previous upgrade. Only the Heartwood activation block holds all zeros.
- Seed: the peaks cannot come from headers, because a leaf holds the final note commitment
  roots of the block. Before Heartwood the tree is empty and known. After Heartwood a node
  seeds the base with `HistoryState::from_peaks`. Without a seed, the node does not check the
  header rule, and the layer records `history: None`. NU6.3 needs the Ironwood tree (tree
  version 3), so its append is an error.
- Correction to the brief: `TemplateEmpty` cannot go out at the header check. A template on
  block B commits to the history tree after B, and the leaf of B needs the body and the tree
  appends of B. Both protocol documents now have one rule:
  - `TemplateEmpty` and `TemplateFull` at the layer build (speculative tip)
  - `TemplateRevert` when `verify` fails.
- `build_layer` keeps the drafts of the unknown transactions, because the contextual check
  needs their fees, nullifiers and commitments. `verify` holds only the scripts and the
  shielded batch. `validate_block` still runs the context concurrently with them.
- Speculative layers never go into the window index. A view walks its layers above the tip of
  the index, then probes the index. Thus a view taken before a pop also uses the index and not
  a full walk.
- `template/switch_after_block`: the time from a parsed block to the full template on it, with
  8,000 candidates and a history tree of 4,095 leaves (load average 10–12 during the run):

  | block | `validate/block` cold | serial cold | speculative cold | serial warm | speculative warm |
  |---|---|---|---|---|---|
  | orchard-165x2 | 136.7 ms | 135.2 ms | 5.4 ms | 2.9 ms | 3.0 ms |
  | mixed-2000x1-100x2 | 91.6 ms | 92.9 ms | 10.0 ms | 4.2 ms | 4.0 ms |
  | transparent-6500x1 | 25.5 ms | 30.5 ms | 23.0 ms | 8.0 ms | 5.7 ms |

  The transparent block gains little. Its cold cost is the 6,500 drafts (sighash digests), and
  the layer build needs them.
- Lesson: the shared bench keys were built lazily inside `OnceLock::get_or_init` on a rayon
  worker, with the multicore keygen. That worker can take another task that waits on the same
  `OnceLock`. Parallel runs of `tests/validate.rs` stopped in 6 of 8 runs. The fixture now
  builds the key with `VerifyingKeys::prebuild` and `ready()` before any validation (0 of 8).
  The lazy path in hayai-prepared still has this hazard.

## 2026-10-03 — Zebra baselines

- Reference: the newest upstream releases, not the local Zebra checkout. (02f9648 is
  zebra-chain 10.0.0, which is older than the `zcash_primitives` transaction of zebra-chain
  13.) The real-code row uses `zebra-chain` 13.0.1, the newest release on the upstream Zcash
  crates of the workspace, so cargo duplicates nothing. Ports cite `zebra-state` 14.0.0,
  `zebra-consensus` 16.0.0 and `zebra-rpc` 18.0.0.
- `wire/parse_block` `zebra`: the real `zebra-chain` code. Its `Transaction` wraps a
  `zcash_primitives` transaction, so the parse path is different from the own structs of
  Zakura.
- `coins/lookup_block_inputs` `zebra`: the same UTXO layout as Zakura, with 7 gets for each
  input (verifier 2, contextual check 2, finalization 3). Zakura has an added
  `CheckParentInputs` round (2 gets) and reads 2 gets at finalization. The RocksDB options of
  Zebra do not have the 4 GiB WAL limit of Zakura.
- `state/push_block` `zebra-clone`: a model. Zebra deep-clones the blocks (with their output
  maps), the created UTXOs and the address index, which Zakura shares through `Arc`s. When the
  window is full, Zebra clones the chain 2 times for each block (commit and `finalize`).
- `template/*` `zebra-zip317`: the selection loop of Zakura is the loop of Zebra, line for
  line. Zebra adds a cached fake coinbase that it clones and parses on every selection (Zakura
  PR #1035 removed it).
- `validate/block` `zebra-model-cold`: the steps of the Zakura model with the 3 reads for each
  coin of Zebra. Zakura overlaps the lookups (64 for each transaction), and Zebra waits for
  them in sequence. On the in-memory view, both cost the same, so the 2 models run the same
  work.
- The `validate/block` model overstated both baselines (audit 2026-10-04). The model joined the
  scripts of each transaction before it prepared the next one. The real block task does not:
  it polls all transaction futures, and the `Buffer` worker only builds each future (`tower`
  `buffer/worker.rs:170-177`). For each transaction, the lookups, `CachedFfiTransaction::new`
  and the sighash run serially on the block task. Each input script runs in a `spawn_fifo`.
  This task starts when the block task polls it, and overlaps with the next transactions
  (Zakura `block.rs:589-615`, `transaction.rs:603`, `script.rs:71-76`; Zebra `block.rs:314-346`,
  `transaction.rs:327-346`, `script.rs:61`). The model now starts the scripts without a join,
  and joins after the last transaction. The unit test in `scenarios/validate.rs` checks that.
  Transparent-6500x1 cold:
  - criterion: 208 ms to 43 ms (Zakura), 206 ms to 43 ms (Zebra)
  - sysbench: 225 ms to 47 ms (both).

  The ratio to hayai (25 ms) is now about 1.8x, not 9x. The model still has no tokio or tower
  cost, so it stays a lower limit.
- `blockstore/get_block` `zebra`: the row layout of Zakura with the options of Zebra and the
  `zebra-chain` parse.
- `sysbench` has an `Impl::Zebra` for the triples that the catalogue lists
  (`validate_block_cold` on transparent fixtures). `scenarios::build` refuses all other
  triples.

## 2026-10-03 — Network Equihash parameters, eager verifying keys, mined parents

- The header knows the network without a network field. The parser accepts the solution
  lengths of the known parameter sets (1344 bytes for (200, 9), 36 bytes for (48, 5)). Each
  caller checks the length and runs Equihash with the `PowParams` of its network
  (hayai-net `Network::pow`, `StandardHeaderCheck::pow`, `TemplateConfig::pow`). The
  compact-relay frame needs no change. The header sets its own limit through its CompactSize
  solution length, so Mainnet frames stay byte-identical. hayaid Regtest now produces and
  parses the 36-byte headers of Zakura.
- Lazy key builds on the rayon pool are removed. Only `VerifyingKeys::prebuild` builds an
  Orchard key, on its own thread, and `ready()` waits off the pool. A batch rejects a bundle
  without a key (`PrepareError::Unsupported`) and does not build the key. Lesson: a `OnceLock`
  initializer that itself waits on the pool must never run on a pool worker (reproduced: the
  old code stopped in 1 of 3 runs of `tests/cold_keys.rs`). Cost: a node that crosses 2
  upgrades without a restart does not have the second key.
- Tip events separate `mined` from `conflicting`. A mined parent leaves the template, and its
  children lose the dependency in place. Conflicts still leave with their descendants. Lesson:
  "invalidated" mixed 2 meanings, and the template dropped the children of every mined
  transaction while the store kept them.

## 2026-10-03 — Deployment and CI

- Testnet in Docker needs a Zakura node. Thus the compose project runs zakurad, and
  hayaid-testnet joins the network namespace of zakurad. The addresses of `hayaid config`
  (127.0.0.1) stay valid, and the RPC of zakurad without cookie authentication stays on
  loopback. hayaid reads only `SocketAddr` values (no host names), so its configuration cannot
  use compose service names.
- Profiles select the network (`testnet` by default through `docker/.env`, `regtest`).
  Prometheus finds the node by DNS name. The absent profile gives no target, and the `network`
  label of the rules comes from the name.
- Restarts: hayaid refuses a non-empty `data_dir` (hayai-r5q), and a failed shadow seed leaves
  files there (hayai-qmj). The restart policies stay as they must be after the resume. The
  documented procedure resets `coins/` and `blocks/`, and nothing removes state automatically.
  `depends_on: service_healthy` on the `/ready` endpoint of zakurad prevents a hayaid seed
  from a node that still synchronizes.
- Lesson: the declared `rust-version` (1.85) does not build. hayaid needs 1.88 (upstream
  crates) or 1.91 (zakura-* forks), and zakura-chain 9.0.0 in hayai-bench needs 1.97. Thus the
  image and CI pin 1.97.1 (hayai-ecx). Check a declared MSRV with a build, not with the
  manifest.
- The image keeps the symbol table and drops the DWARF sections of the release profile (binary
  480 MB → 21 MB, image 268 MB). It carries the Sapling parameters, because hayaid does not
  download them.
- cargo-deny ignores 3 rustls-webpki advisories, with a reason. They come through the
  `download-params` feature of zcash_proofs, which only hayai-bench uses (hayai-ehj).

## 2026-10-04 — Design review items 4 and 7: template as lane, prebuilt bodies

- Block order. A batch reference covers a run of consecutive positions. The template ordered
  its block by weight ratio, so a new high-ratio transaction broke every batch after it. The
  selection stays by weight ratio. The block order is now canonical (depth over in-block
  parents, then txid), so a block is a function of its set. The template reads the parents
  from the inputs (`Candidate::spends`), as a receiver does. Lesson: the test fixtures
  declared `depends_on` without a spend of the parent, and the order showed it.
- Template as lane, as feature bit 2 and not as version 3. Only lane owners publish, any
  version can carry it, and a peer without the bit sees nothing new. Each template change
  sends 2 messages:
  - a `BatchAnnounce` of its additions (`seq` = template id)
  - a `CandidateAnnounce` that names every batch of the candidate and the removed positions.

  Each announcement is complete, so a missed announcement costs nothing. `CandidateBlock` =
  header, coinbase, `(lane, seq)`, removed positions, short and full ids of the additions. A
  block equal to its candidate costs 61 bytes in addition to the header and the coinbase
  (`relay/bytes_on_wire`: 1,652 bytes in total for the 2,001-transaction block, 13,614 with
  short ids). Reconstruction (`relay/reconstruct`, load 13–25): 0.67 ms on
  `transparent-2000x2` (short ids 0.77 ms, batch 0.46 ms), 0.19 ms on `orchard-200x2`. If a
  candidate block does not resolve, the node gets the full block. The node forwards it when
  it holds the bytes, because the canonical order needs every input.
- Prebuilt bodies. All the work that the contextual check does with the body after the
  coinbase depends on the parent only. Thus `prebuild_body` does it early, and
  `PrebuiltBody::commit` adds the header and coinbase rules and moves the maps into the
  layer. The full check and the prebuild share one implementation of each rule. The merkle and
  auth roots come from the branches of position 0. A shielded coinbase is a mismatch, because
  its commitments come before the commitments of the body. Measurements:
  - `state/commit_prebuilt`: the swap takes 0.04–0.36 ms, against 0.72–2.74 ms for warm
    `validate_block`. The prebuild itself costs 0.69–2.80 ms.
  - `template/own_block_commit` (to the full template on 8,000 candidates): 2.3–3.9 ms
    against 3.1–9.3 ms.
- Own blocks: when idle, hayaid prebuilds the body of the newest template, at most every
  200 ms (`mining.prebuild_own`, on). Candidates of the lanes of peers: off by default
  (`network.prebuilt_candidates = 0`). The reasons:
  - A prebuild costs as much as the warm validation that it saves.
  - A lane publishes again on every template change.
  - Only an exact match gives a gain.
- Not done: reuse of verdicts relative to the tip for competitor blocks. One missed
  invalidation of that cache accepts a double spend. After the window index, the contextual
  checks cost less than 3 ms.

## 2026-10-04 — Restart, Mainnet, toolchain and dependency cleanup

- hayaid resumes from `data_dir`. The coins store records its best block. For each coins
  flush, `state.log` holds a checksummed record of the base (frontiers, value pools, history
  peaks, block times, new anchors), and the node writes it before the flush. A restart does
  these steps:
  1. It takes the record of the best block.
  2. It drops the later records.
  3. It replays the block files above the base with full validation.

  Decision: an append-only log, not one snapshot next to the coins snapshot. The anchor sets
  grow with the chain, and a snapshot for each flush would write them again. The shadow node
  keeps its set of spent outpoints in `spent.log` (records by generation height, cut at the
  best block). A first start seeds before it creates any file. If it fails before the start
  record, it removes the files that it created. Lesson: the shadow trust state (`spent`) was in
  memory only, so a restart would have accepted a spent pre-start coin again.
- Fixture cache: parallel writers shared one temporary file. Each write now uses a unique name
  (process id and counter), and every caller reads the file back, because Orchard proofs are
  not deterministic.
- `cold_keys` failed at random with `pthread lock: Invalid argument` and SIGABRT after the test
  printed `ok` (1 run in 10 on the zakura backend). The helper thread of the test dropped the
  RocksDB handle of the harness while the main thread ended the process. At the same time, the
  static destructors of the C++ runtime of RocksDB ran. The test now joins the helper (0 aborts
  in 40 runs). The 600 s timeout was not the cause: 6 runs on 2 cores with 8 CPU burners took
  12 to 26 s. Lesson: a test must join every thread that owns a RocksDB handle.
- Mainnet is a network kind (`NetworkKind::Mainnet`). Shadow and full mode accept it, with no
  code guard. `docs/hayaid.md` and `docs/install.md` list the rules that hayaid does not
  enforce. Full mode on Mainnet starts at genesis and cannot synchronize.
- `rust-version`: 1.91 for the node crates (both backends; upstream alone builds on 1.88), and
  1.97 for hayai-bench (zakura-chain). The CI `msrv` job checks the node crates on 1.91.1.
- `zcash_proofs` no longer has `download-params`, because nothing used the only caller in
  hayai-bench. `minreq`, `rustls-webpki` and `webpki-roots` left the graph, together with the
  advisory ignores and the MPL-2.0 exception of deny.toml. The `multitable` bench candidate
  stays a hayai-bench feature that is off by default. deny.toml still excludes it, because
  `all-features` pulls the unlicensed git dependency into the graph. The measured result stays
  in the review section.
- Metrics for operations: process memory and CPU, template latency (tip change to template),
  coins cache size, build info. Logs go to stderr, with colour only on a terminal.

## 2026-10-04 — hayai-consensus, finality depth 1,000, state record version 2

- `hayai-consensus` is the one home of the network parameters, and of one rule set for each
  network upgrade. `rules_at(network, height)` selects the rule set. hayaid, hayai-validate and
  hayai-state get the branch, the epoch and the block limits from it. Lesson: the driver had a
  fixed `BlockLimits::PRE_NU7`. A rule that depends on the height must come from the height,
  not from a constant at the call site.
- NU7 has no rule set (owner decision). On the zakura backend, `zcash_protocol` selects
  `BranchId::Nu7` from Testnet height 4,465,026, and the branch predicates of hayai-prepared
  treated it as NU6.3. `rules_at` now returns `UnsupportedUpgrade` there, and the node stops.
  The only `cfg` for NU7 is in hayai-crypto (`nu7_branch`, `nu7_activation`).
- The layer window is the finality depth: 1,000 blocks (was 100), as in Zebra and Zakura. A
  restart replays up to 1,000 blocks plus the blocks since the last flush, with full
  validation. The window holds 1,000 layers in memory.
- The Testnet proof-of-work limit is `0x07ff…ff` (compact `0x2007ffff`). hayaid had the
  Mainnet value `0x1f07ffff` for Testnet.
- `Layer`, `Base`, `Anchors`, `ValuePools` and the `state.log` record (version 2) have the
  fields that the difficulty rule and Ironwood need (`bits`, 28 block times, Ironwood
  frontier, anchor and pool, transparent, Sprout and deferred pools). No rule uses them yet. A
  version 1 record still loads, with empty new fields. All the fields came in one change, so
  that the parallel work items do not edit the same structs.

## 2026-10-04 — Conformance harness on published vectors

- `hayai-bench/tests/conformance_blocks.rs` runs the 90 Zebra block vectors through
  `validate_block`. `conformance_txs.rs` runs the 1,046 `zcash_script` vectors through
  `Draft::check_input`. `docs/conformance.md` holds the inventory, the design and the outcomes.
- The outcome of each vector is in `tests/vectors/expected-*.json`, with the reason and the
  plan item. A test fails when an outcome changes in either direction. A work item that makes a
  vector pass must update the file. A rejection of a valid vector is a defect. It goes to
  `docs/conformance.md` and to a bd issue, never to the expected file.
- The chain context comes from the vector set only (owner decision Q7): the state that hayai
  built from the earlier vectors, empty trees before an activation, and published roots. A
  block that reads other state stops as `context_free`, with the list of the missing state. No
  `mkcontext` binary exists, because its source was a reference node.
- Lesson: an `Unsupported` error must not end a stage. The transaction stage runs every
  transaction, and reports a rejection before an unsupported rule. Without that order, a block
  with one Sapling bundle hides an invalid Orchard proof in the same block.
- Lesson: random script vectors cannot show a sighash value through the public prepare path.
  The ZIP 143, 243 and 244 sighash values need a test inside `hayai-prepared` (bd hayai-xya).

## 2026-10-04 — Embedded Sapling verifying keys, ZIP 213

- The 2 Sapling Groth16 verifying keys are files in `hayai-prepared/src/sapling_vk/` (1,636
  and 1,444 bytes): the start of the official parameter files. hayaid reads no parameter file,
  `sapling_params_dir` is removed, and the image has no parameters. `VerifyingKeys::new()` and
  `prebuild(epoch, next)` take no Sapling argument.
- Provenance: `scripts/extract-sapling-vk.sh` writes the files from hash-checked parameters. A
  test compares them with `wagyu-zcash-parameters` (a test dependency, the official files in a
  crate). Thus the test needs no file on the machine and never skips.
- Lesson: `sapling-crypto` has no public constructor of its verifying key types from a
  `groth16::VerifyingKey`. The public path is `SpendParameters::read`, so the loader appends
  5 empty prover vectors to the key. The encoding loads on both backends.
- ZIP 213 is in `hayai-prepared/src/coinbase.rs`, and `draft` calls it for a coinbase. The
  decrypt functions are generic over the authorization, so the tests use builder output
  without proofs.
- The Sprout Groth16 verifying key is not embedded, because `sprout-groth16.params` (725 MB)
  is not on the machine. Zebra and Zakura ship it as `sprout-groth16.vk` (plan item A5).

## 2026-10-04 — Subsidy, funding streams, lockbox, coinbase terms (W3a)

- `hayai_consensus::coinbase::CoinbaseTerms::at(network, height)` is the one source of what a
  coinbase must pay:
  - the founders' reward
  - the funding stream outputs
  - the NU6.1 lockbox disbursement
  - the deferred part
  - the ZIP 236 flag of the rule set.

  The validator (`check`, `deferred_pool_after`) and the template
  (`hayai_template::consensus_subsidy`) read the same terms, so they cannot disagree.
  `validate_block` still uses `SubsidyRule` until W3b.
- The constants (address lists, ranges, numerators) are copies of the constants of Zakura. The
  test `hayai-bench/tests/conformance_subsidy.rs` compares every schedule with `zakura-chain`
  and `zebra-chain`. It also runs the coinbase of each block vector through the check.
- The node also checks the founders' reward addresses, although Zakura and Zebra reach that
  code only above their checkpoints. Thus the check does not depend on the checkpoint range.
- Lesson: the value rule needs the lockbox disbursement. In the NU6.1 activation block, the
  coinbase pays 78,750 ZEC more than subsidy − deferred + fees. A rule with only `total` and
  `deferred` rejects that block.
- Lesson: a height of an upgrade without a rule set (NU7) has no subsidy and no terms. The
  functions return `UnsupportedUpgrade`. They never apply the schedule of an earlier upgrade.

## 2026-10-04 — hayai-sync: fork-aware header chain (W6)

- The header chain is a tree in memory (96 bytes for each entry) and a checksummed append-only
  header log on disk. The position of an entry is its first-seen order and its order in the
  log. Thus a start that applies the log in order gives the same chain. Removed entries keep
  their position until the next start.
- Tie on equal work: the first-seen entry stays the best tip (Bitcoin Core, zcashd), not the
  larger hash (Zebra, Zakura).
- The finalized height comes from the best header tip (minus 1,000) and from the last
  checkpoint that the best chain reached. It is not monotonic: an invalid block on the best
  chain moves it back. The chain refuses and removes the branches that leave the best chain
  below it.
- The context-free rules and the work function come from hayai-consensus
  (`check_proof_of_work`, `block_work`). The contextual rules go through the `HeaderRules`
  trait, and its context has the fields of `hayai_consensus::ParentChain`. The chain reads the
  context from the branch of the header, not from the best chain.
- The start does not run proof of work or the contextual rules again. A record checksum shows
  that the chain wrote the header after these checks. Only `Invalid` is in the log. The node
  sets the other body states from its block state.
- Lesson: a model-based property test (random forks, duplicates, invalid blocks, finality
  depth 1 to 7) found no fault that the example tests missed. But 4 deliberate faults in the
  tie and finality rules each made it fail at once. Keep the test when the chain changes.

## 2026-10-04 — Ironwood (NU6.3), history tree version 3 (W1)

- One code path for the Orchard and the Ironwood bundle. A v6 transaction has 2 slots of the
  Orchard protocol. `draft`, the batch and the contextual check name the pool
  (`Pool::Orchard`, `Pool::Ironwood`). The implementation is the same on both backends. The
  Ironwood tree has the node type and the hash of the Orchard tree (upstream
  `MERKLE_CRH_PERSONALIZATION`; Zakura `ironwood.rs` re-exports `orchard::tree`).
- The Ironwood state is not optional. Before NU6.3 the tree is empty, its root is in the anchor
  set of every base, and the pool is zero. A `state.log` record without the Ironwood frontier
  loads with the empty tree, because no build accepted a block from NU6.3 before this change.
- `draft` takes the rule set of the epoch (`RuleSet::of_branch`). Versions, pools and coinbase
  rules come from hayai-consensus. Lesson: the "some source, some sink" rule counted Orchard
  actions without the `enableSpends` and `enableOutputs` flags.
- The upstream parser applies the flag-bit rules and the canonical proof length. Tests prove
  this for each bundle version, because hayai has no second check of the bits.
- Lesson: no input reaches a rule through the parser when the transaction format already gives
  that rule (a pool that is not active). Such a rule is a function of its own (`check_pools`),
  with a test on a changed rule set.
- Shadow mode: from NU6.3, a `z_gettreestate` answer without the Ironwood tree, or a start
  block without the Ironwood pool, is an error. hayai does not replace the state of upstream
  with an empty tree.
- Fixtures: generated blocks of the NU6.3 epoch (owner decision: no blocks from the network).
  An Orchard bundle of NU6.3 has padding actions only, because the pool takes no value and no
  cross-address transfer. The 2 backends share the fixture cache, so each backend also
  verifies the proofs that the other backend made.
- Not done: the Orchard soft fork of NU6.1 (no Orchard bundle from Mainnet 3,363,426 until
  NU6.2) needs the network and the height in the contextual check. The total shielded cost of
  ZIP 218 belongs to the NU7 rule set.

## 2026-10-04 — Peer management: address book, connection manager, misbehaviour score

- The score type is in hayai-sync (`score`), and hayai-net depends on hayai-sync. The block
  download needs the same reasons and must not depend on hayai-net.
- The node holds scores and bans for each IP address, not for each connection. A peer that
  connects again keeps its score. Points decay (1 each 60 s), so rare small faults never reach
  a threshold.
- In the relay, only context-free faults get a score:
  - a frame that does not decode
  - a message that the negotiated protocol does not permit
  - a header without valid proof of work.

  A failure that depends on the local stores or on the local view of the chain costs nothing.
  An invalid block from a compact-relay peer costs nothing, because that peer forwards before
  validation.
- The tests inject the clock and the DNS resolver (`PeerEnv`), and the address book takes the
  time as a parameter. No test reads the system time for a decision or resolves a name.
- The node checks the limits under the lock of the peer set, before the `version` message.
- Review fixes:
  - The node never replaces an address that responded at any time, and dials it before gossip
    addresses. One failure after a local outage must not expose good addresses to a flood.
  - A `getaddr` answer holds responded addresses only.
  - For IPv6, the key of bans, scores and the limit for each IP is the /64.
  - `Source::Peer` has the IP address, so the node can still score a peer that left before the
    end of the validation.
- The node sends an address on to other peers only when the address is new to the book.
  Without that rule, the same announcement goes in circles between nodes for 10 min.
- ZIP 155 has no `sendaddrv2`: a Zcash peer sends `addrv2` without negotiation. The node
  decodes `addrv2` and sends `addr` only, as Zebra does.
- `TxSink::accept_tx` still returns `bool`. The node reports an invalid transaction or block
  with `Relay::misbehaved`. The verdict enum of plan item B5 waits for the driver item, which
  owns the hayaid sinks.
- Lesson: `zcash_encoding::CompactSize::read` refuses values above 0x02000000. The service bits
  of `addrv2` need a reader for the full `u64` range.

## 2026-10-04 — Difficulty adjustment and header rules in one place

- `hayai_consensus::header::check_header` is the one function for the header rules. It has 3
  parts:
  - `check_contextual` (version, target limit, time rules, expected `bits`)
  - `check_local_time` (only with a clock; a replay gives none)
  - `check_proof_of_work`.

  The relay check and the hayaid header check call the whole function. Block validation calls
  `check_contextual` on the context of the view. The replay adds `check_proof_of_work`.
  Decision: block validation does not repeat Equihash. Equihash costs 0.16 ms for each header
  on this machine, and an own block commits in 0.04 to 0.36 ms.
- A context that is too short for a rule gives the result `HeaderVerdict::ContextTooShort`,
  with the rules that did not run. It is never a pass. The caller decides:
  - A full node rejects (`HeaderPolicy::Enforce`).
  - A shadow node trusts and counts (`HeaderPolicy::TrustShortContext`).

  The context is 2 lists, times and `bits`, because a seed gives 11 times and no `bits`. The
  time rules then run from the first block.
- Regtest follows Zakura (`disable_pow`): no hash filter, no Equihash, no expected `bits`.
  zcashd Regtest keeps the `bits` of the parent instead. A pair with zcashd needs that rule.
- Lesson: `check_pow` had no proof-of-work limit, and `expand_target` accepted a mantissa that
  a small exponent shifts to zero. Only the vectors of the `arith_uint256` tests of Bitcoin
  showed these faults. Port the vectors of the reference together with the function.
- Lesson: the Testnet `MTP + 90 min` rule starts at height 653,606, not at genesis. A network
  parameter that is a start height belongs in `NetworkParams`, not in a `match` of the node.
- The difficulty tests compare 36 generated chains with a reference implementation in the test
  (`num-bigint`, blocks indexed by height, its own compact encoding). The published vectors
  cannot test the adjustment: the longest range is 11 blocks, and the rule reads 28.

## 2026-10-04 — Compact block with full ids is type 12; `Malformed` is 50 points

- The version 2 layout marked its full-id section by its presence at the end of the type 6
  payload. A payload cut before the section was a valid version 1 payload, and a hop could
  remove the section. The property test `truncation_never_decodes` found it. Only the frame
  length and the legacy checksum caught a cut.
- Type 12 (`CompactBlockV2`) now carries the full-id section. Type 6 is the version 1 layout
  only. The encoder uses type 12 if and only if the block has full ids. A zero count in type 12
  is malformed. Each message has one encoding, and the codec needs no state of the connection.
  `Message::CompactBlock` is still one variant. A draft 2 or 3 node and a draft 4 node
  disconnect on a block with full ids.
- Lesson: never infer an optional section from the end of a payload. The message type (or an
  explicit field) must name the layout, and every field of a layout is always present.
- The truncation test now cuts each generated frame at every byte position. A second test does
  the same for one message of each type code.
- `Misbehaviour::Malformed` is 50 points. The first one disconnects, and the second one before
  the decay bans. An honest peer with a newer protocol can send a message that the decoder does
  not know. Zebra only disconnects.

## 2026-10-04 — Mempool policy, ZIP 401 store (W11)

- Owner decision Q8: the node relays as the public network does. `hayai-prepared/src/policy.rs`
  holds the admission rules (`MempoolPolicy::admit`), with one `PolicyReject` for each rule.
  `docs/mempool-policy.md` gives the source of each rule, and names the constants that no local
  source confirms.
- The store evicts by ZIP 401, not by the lowest weight ratio. The random number generator is a
  constructor argument (`PreparedStore::with_rng`), and the victim scan is in insertion order.
  Thus a seed gives a reproducible eviction. The review fix stays: an ancestor of the new
  transaction is not a candidate, and descendants leave with their parent.
- The store limit is a ZIP 401 cost (`max(size, 10,000)`), not bytes. A test that sets the size
  of a store in bytes of small transactions now refuses every insert.
- The unpaid action limit is 50 (ZIP 317, zcashd). Zebra and Zakura use 0. It is a field of
  `MempoolPolicy`.
- No rebroadcast from the mempool, because zcashd and Zebra do not have one.
- Lesson: the clock of a time rule is an argument (`insert_at(tx, now)`), so a test of "60 min"
  needs no sleep.

## 2026-10-04 — hayai-sync: block download scheduler (B2)

- `download::Scheduler` is a pure state machine: events in, actions out, and the time is an
  argument. It holds no bodies and nothing on disk. The node driver maps the actions to
  `getdata`, to its body store and to the validator.
- The scheduler fills the window from `best_chain_from`, not from `next_blocks_to_download`. A
  body in memory is not `BodyKnown` in the header chain, because that state cannot go back
  after a reorg. Thus that function would return the window again at each call.
- The memory limit counts each request as one largest block. Only the first block of the
  window can use the last 2 MB of the budget. An exception for the lowest missing block (the
  text of the plan) lets the memory grow without a limit when the validation is slow.
- Rescue and stall are 2 rules. Rescue (2 s) moves the requests without a penalty. Stall (8 s)
  gives the penalty. A penalty at the rescue time disconnects honest peers whose rate changes.
- The progress of a peer is "a body arrived". A fixed deadline from the rate at the time of the
  request penalized a peer that became slow. A peer that answers a later request first makes no
  progress for the earlier one.
- `BestHeaderTipChanged` has the committed tip of the node, because the header chain does not
  give the fork point of a committed block that left the best chain.
- Lesson: a run of 18 deliberate faults against the tests. The simulation missed 3 (budget of a
  late body, order of answers, peer selection by speed). Each one needed a test with single
  events and exact expected actions.

## 2026-10-04 — Phase 1 integration: coinbase terms, value pools, seed context, Orchard soft fork

- Block validation gets the coinbase terms from `CoinbaseTerms::at(network, height)`.
  `SubsidyRule` is removed from hayai-state, hayai-validate, hayaid and hayai-template, and
  shadow mode sends no `getblocksubsidy` request. `CheckConfig` and `ValidateConfig` have the
  network. `HeaderPolicy` has no network of its own. `CoinbaseSpec` has the network, and
  `build` returns the error of a height without a rule set.
- The contextual check keeps the transparent, Sapling, Orchard, Ironwood and deferred pools up
  to date. No pool can be negative, and the total is at most `MAX_MONEY`. A prebuilt body keeps
  its totals, and the commit computes the pools with the coinbase.
- Lesson: a pool that the node does not know is not zero. The shadow seed fails without a pool
  that a rule reads (a `lockbox` value above zero from NU6). `state.log` record version 3 marks
  a record with every pool. The node refuses a record of version 1 or 2 above the genesis
  block, with the instruction to remove the data directory.
- The shadow seed holds 28 blocks with time and `bits`, in the header index and in the base.
  The state record holds the same context. No header rule waits for context, and
  `hayai_shadow_trusted_bits_total` reads 0. A seed without the 28 blocks fails.
- The template `bits` on Mainnet and Testnet are `expected_bits` for the template time. On
  Testnet the value applies to that time only (minimum-difficulty rule).
- A rule that depends on the height inside one upgrade is a rule set of its own, and only
  `rules_at` selects it. Thus the Orchard soft fork is the NU6.1 rule set with the Orchard pool
  off. The epoch of hayai-prepared has no height, so hayai-state checks the pools of the rule
  set of the height. Lesson: `RuleSet::of_branch` is not the rule set of a block.
- Lesson: a test double of a consensus input hides a wrong fixture. The bench fixtures paid
  the whole subsidy to the miner. They now pay the Mainnet terms of their height (generator
  version 2), and tests change the coinbase with `Fixture::with_coinbase`.
- The template candidate counts Ironwood actions for ZIP 317 and for the block limit.

## 2026-10-04 — Checkpoints and the checkpoint path (A9, B7)

- Owner decision: do as Zakura does. The Mainnet and Testnet checkpoint lists are the files of
  Zakura (clone revision `1377915`). The node verifies a block at or below the last checkpoint
  by its hash. The header chain still applies every header rule to every header.
- The checkpoint path (`apply_checkpointed`) builds the layer from the parsed transactions. It
  makes no prepared transaction, because a draft has the sighash digests that only the scripts
  read. It keeps the checks that guard the state:
  - parent
  - missing input
  - double spend and duplicate nullifier in the block
  - negative pool
  - header commitment.

  It drops the rules that the hash replaces.
- The path alone cannot prove that a block between 2 checkpoints is on the checkpointed chain.
  The header chain is that proof. The caller gives the hash of the header chain (`expected`),
  and the header commitment of the next block binds the tree roots.
- Full validation refuses a block at or below the mandatory checkpoint (Canopy − 1). Thus the
  conformance harness runs the published vectors of these heights on the checkpoint path.
- `ChainConfig::checkpoints` stays a field, because a test of a generated chain needs its own
  list. The type is `hayai_consensus::Checkpoints`, so the lookup code exists one time only.

## 2026-10-04 — Sprout (A5, W7)

- Owner decision: do as Zakura does. Full validation verifies the Groth16 JoinSplits of v4
  transactions and the Ed25519 JoinSplit signature (ZIP 215 rules at every height). It has no
  verifier for BCTV14 proofs and no sighash for v1 and v2. `draft` returns `Unsupported` for
  v1 to v3, and only the checkpoint path applies these blocks.
- The Sprout verifying key is in the binary (`hayai-prepared/src/sprout_vk/`, 1,828 bytes): the
  start of `sprout-groth16.params`. `scripts/extract-sprout-vk.sh` writes it from the
  hash-checked file. No crate ships the 725 MB parameters. Thus the test pins the hash of the
  key file, and a test verifies real JoinSplit proofs of the published block vectors.
- A Sprout anchor needs the tree, not only the root. A JoinSplit continues the tree of its
  anchor, and a later JoinSplit of the same transaction can use that output treestate. Thus
  the base keeps the frontier of every final Sprout treestate by root, in memory and in
  `state.log` (record version 4). `PreparedTx::anchors` holds no Sprout anchor.
- A base that starts above the genesis block has no Sprout treestates (shadow seed, state
  record before version 4). It does not know the Sprout state, and a block with a JoinSplit is
  `SproutStateUnknown`. Lesson: the rule is the same as for the value pools. A state that the
  node does not know is not the empty state.
- The state update of a JoinSplit is the same on both paths. `Commitments::sprout` goes through
  `append_leaves`, and the nullifiers and the value go through the totals. The checkpoint path
  reads no proof, so BCTV14 and Groth16 JoinSplits have one code path.
- Lesson: upstream `write_frontier_v1` takes a tree of depth 32 only. The Sprout frontier
  (depth 29) has its own `write` and `read` over the 2 public parts of that function.
- Lesson: the conformance stage that runs `draft` on every transaction stopped every block
  before Sapling. The stage now counts a transaction that only the checkpoint path applies,
  and does not run it.
- Not done: a sighash for v1 and v2 transactions and a BCTV14 verifier. No vector on this
  machine can test them, and no block that needs them has full validation.

## 2026-10-04 — hayaid full mode: synchronization, fork choice, reorg, speculative tip (B3)

- One path for every block in full mode:
  1. The header chain has the header.
  2. The download scheduler delivers the block in height order.
  3. The driver validates it.

  A block of the relay (compact relay, own block) is a body from another source of that path,
  not a second path. The relay keeps its header check on the committed window (`HeaderIndex`,
  pending headers). The header chain checks the header again when the body arrives.
- The relay sends no block `getdata` when the node gives it a `SyncSink`. An announced block
  and a failed compact block become a `getheaders` of the node.
- A body that a peer can change without a change of the block hash never makes a header
  invalid. From NU5 the authorizing data are outside the block hash. Thus the driver checks
  `hashBlockCommitments` before the validation. A mismatch is a wrong body (penalty, another
  peer), not an invalid block. The invalid mark is in the header log and stays after a start.
- Reorg: the driver disconnects only when the delivered blocks of the new branch meet one of
  these conditions:
  - They end at the best header tip.
  - They have more work than the tip.
  - They fill the validation lookahead.

  Lesson: "more work than the tip" alone stopped the return to the first chain after an
  invalid branch, because the header chain keeps the first-seen chain on equal work.
- After a reorg, the whole mempool goes through the admission again, with the transactions of
  the disconnected blocks first. A check of the returned transactions alone leaves transactions
  whose inputs the reorg removed.
- The node serves the headers of the blocks that it can send, not only of the committed ones.
  Lesson: a node forwards a block before its validation. A legacy peer then asks for the
  header, and an answer without it loses the block until the next one.
- A block at or below the mandatory checkpoint waits until the header chain reaches a
  checkpoint above it. The replay of a full node uses the checkpoint path at or below the last
  checkpoint.
- The block store keeps every block by hash. The newest append at a height is the block of that
  height. `DuplicateHeight` is removed.
- The clock of the scheduler is the arrival time of each message at the relay. A tick runs only
  when the queue of the driver is empty. A tick at the time of the driver gives a stall to a
  peer whose block waits in the queue.
- Not solved: the best header chain alone drives the download. Thus a header chain with more
  work and no bodies keeps the node at its tip. The relay parses transactions with the branch
  of the start height, and the verifying keys are the keys of the start epoch.

## 2026-10-04 — Template: ZIP 317 block production, branch id and limits of the height (B3)

- The selection is the ZIP 317 block production algorithm with one difference: the order by
  weight ratio replaces the random pick, because the template lane needs one selection for each
  set. In the order, pass 1 (fee >= conventional fee) comes before pass 2. Pass 2 stops at 50
  unpaid actions in the block. `Candidate::unpaid_actions` and `SetEvent::Repriced` carry the
  count. `docs/mempool-policy.md`, Template selection, lists the differences from the ZIP.
- The weight ratio uses `max(1, fee)`. The sigop budget subtracts the coinbase sigops. The
  sigop and shielded limits are `BlockLimits` of the rule set of the height. Before this change,
  the NU7 shielded limits applied at every height. `TemplateConfig` has no limit field for
  them.
- No branch id is fixed at start. `CoinbaseSpec` has no `branch_id`. The build gets it from
  `rules_at(network, height)`, and `CoinbaseTx::branch_id` carries it. `rebuild_block` takes no
  branch. `RpcConfig` holds the network. `submitblock` parses with the branch of the height
  that the coinbase states. A height without a rule set is an error.
- The `getblocktemplate` `maxtime` is the median-time-past plus 90 min
  (`Tip::median_time_past`), from `max_time_start_height`. Lesson: a value derived from
  `mintime` was correct only by accident, because of how the node sets the template time.

## 2026-10-04 — Review fixes: header version, duplicate txid, peer-reachable panics

- For zcashd, the header version is a signed 32-bit integer. `check_version` is the one
  function for the rule, and the header chain calls it. Lesson: a field that the reference
  reads as signed needs the comparison of the reference, not only its constant.
- A body with a txid 2 times can have the merkle root of a valid block. Each function that
  compares the merkle root also refuses a txid that occurs 2 times (`hayai_wire::duplicate_txid`),
  with `ContextError::DuplicateTxid`. The error names a fault of the body, so the node driver
  must not mark the header invalid for it.
- Lesson: an assertion on an argument is a panic that a peer can reach when one caller takes
  the argument from a peer. `draft` returns `PrepareError::SpentCoins`, and `commit_prebuilt`
  checks the shape of the first transaction before the draft.
- The NU7 height is a constant of hayai-consensus, not a value of the crypto backend. A backend
  that does not know an upgrade must not make the node use older rules.
- Regtest does not apply the shielding rule for coinbase spends (`coinbase_must_be_shielded`),
  as in Zakura and zcashd. Coinbase maturity applies on every network.
- The founders' reward check has no caller in a node. Its heights are at or below the
  mandatory checkpoint, and Regtest has no founders' reward. The code stays for
  `CoinbaseTerms::at`, the template tests and the conformance tests.

## 2026-10-04 — hayai-fuzz: differential fuzzer, in-process tier (C3, W6c)

- A case is a seed block, a recipe of mutations and a chain context. hayai (`validate_bytes`)
  and the Zakura library code each give a verdict. A finding is a different outcome (accept
  against reject) or a panic. The fuzzer counts a different rule class on 2 rejects, and it is
  not a finding.
- zakura-consensus and zakura-state cannot be dependencies. Their `rocksdb` 0.24 and the
  `rocksdb` 0.25 of hayai-coins both link the native library `rocksdb`. The oracle links
  zakura-chain, zakura-header-chain, zakura-script and zakura-orchard. It holds a copy of the
  check files of zakura-consensus (`src/reference/zakura_consensus`, with the commit in its
  `mod.rs`). The set rules of zakura-state are a model in the oracle. Anchors, chain value
  pools and the history tree have no oracle (`docs/conformance.md`).
- The context of a case is a `CoinsBacking` over a map. Thus each case has its own coins,
  nullifiers, height and network, and no case writes state.
- Lesson: a fixture must exist before the rayon pool runs cases. A lock around a fixture
  generation that uses the pool stops the pool.
- Lesson: after a mutation, the header must commit to the new body. If not, every case stops
  at the merkle root. When the reference does not parse the block, hayai computes the roots,
  so a block that only hayai parses still reaches the rules of hayai.
- Lesson: the context must be a possible chain state. A deferred pool of 0 at the NU6.1
  activation block and coins above the money limit gave rejects of hayai that the oracle
  cannot judge.

## 2026-10-04 — Sync gaps and review fixes: upgrades, checkpoints in the node, withheld bodies, faults

- No value that depends on the height is fixed at start. The relay asks the node for the
  branch (`ChainSource::tx_branch`, `block_branch`). The keys follow the tip
  (`VerifyingKeys::prebuild_more` after each commit, `ready()` on the driver thread before a
  batch). At an activation, the store drops the transactions of the old epoch, because their
  signature hash commits to the old branch id.
- A generated chain gets upgrades and checkpoints through `Network::ConfiguredRegtest`
  (`[regtest]` in the configuration). The value is `Copy` and lives until the process ends.
  Lesson: a process-wide setting was not possible, because the tests of one binary run nodes
  with different values.
- 3 faults, not 2 (`node/fault.rs`): wrong body, invalid, local. The invalid mark in
  `headers.log` is only for a fault that the header hash commits to. Lesson: a restart does not
  remove the mark. Thus these causes must never reach it:
  - a capability of the node (a missing key, an unknown Sprout state)
  - a body that a peer changed (authorizing data, a merkle mutation).

  The body check runs before each commit path, also before the prebuilt path.
- Withheld bodies: the fork choice of the header chain stays work-only. The node takes a block
  that no peer sends out of the fork choice for a time (`mark_unavailable`, in memory). The
  scheduler does not ask a peer on another branch (`PeerForkPoint`). Lesson: a request by
  reported height to a peer on another branch gives a stall to an honest zcashd peer, because
  zcashd does not answer `notfound` for a block.
- The driver handles one batch for each turn of its loop. The tick runs on elapsed time, and
  its clock is the arrival time of the oldest queued message. Lesson: a tick only on an empty
  queue never ran under load. A clock that is the time of the driver gives a stall to a peer
  whose block waits in the queue.
- No template while the tip is more than 100 blocks below the best header tip. Measured with
  2,000 generated blocks from one peer, flush interval 100, 3 runs each:
  - 3,315 to 3,430 blocks/s with a template after each batch
  - 3,900 blocks/s without.

  With the flush interval 8 of the tests, each flush has 2 `fsync` calls, and the rate is
  about 300 blocks/s.
- The mempool insert checks a tip count under the lock that the driver holds while it cleans
  the store. Lesson: "write the view, then clean the store" lets an admission on the old tip
  insert after the clean.
- Not done:
  - compaction of `state.log` and `headers.log`
  - a Sprout seed for shadow mode (no RPC of zakurad or zebrad gives the Sprout tree)
  - a request without a penalty to a peer whose chain is not known.

## 2026-10-05 — Setup of Zakura: command line, configuration keys, metrics, sync race (hayai-90w, hayai-hg9)

- One name for one setting. Where Zakura has the concept, the configuration of hayaid has the
  section, the key and the value format of `zakurad`, and the old name is removed.
  `docs/zakura-compat.md` has the table of each key.
- A key of `zakurad` that hayaid does not use is not an unknown key. The parser takes it out
  before serde reads the file:
  - a tuning key gives a warning
  - a key of the consensus rules, the network or a data location gives an error, unless its
    value is the behaviour of hayaid.

  One report has all lines.
- hayaid reads no `ZAKURA_*` variable. It reads `XDG_CONFIG_HOME` and `HOME` for the default
  configuration path only, which is the rule of `zakurad`.
- A metric has a name of Zakura only with the meaning of Zakura. Lesson from a run of the real
  zakurad: its Docker build exports no `process_*` metric, and its duration metrics are
  summaries, not histograms. Read the `/metrics` output of the reference, not only its
  dashboards.
- The race files use the host network for each container and no mount of the host root.
  node-exporter reads the data file system through the 2 data volumes.
- Lesson: a test must not assert on a row or a metric that only a timer writes. The
  `sync_progress` row comes from the tick of the block synchronization. A node that reaches its
  tip and stops in less than one tick has no such row. The tests wait, with a time limit, for
  the metric that the same report sets (`wait_sync_report`).
- Lesson: bash starts a background job with SIGINT ignored, and a Python child keeps that state
  until it sets its handler. Stop such a job with SIGTERM and a time limit.
- Not done: a run on the public Testnet, `terraform apply`, a run of `scripts/race_deploy.sh`
  against real hosts.

## 2026-10-05 — NU7 rule set (hayai-sm3)

- NU7 is one more rule set (`rules::nu7`). It exists only when the crypto backend has the NU7
  branch id. The upstream `BranchId` has no value `0x77190ad9`, and its parser refuses such a
  transaction. Thus the default backend keeps the stop at the NU7 height.
- The schedule (halving, subsidy, scheduled issuance, funding streams) reads a table of target
  spacing eras, not the rule set. It gives the NU7 values on each backend, and the comparison
  with `zakura-chain` runs on each backend.
- The NSM value balance is not a stored value. It is the scheduled issuance up to the height,
  minus the total of the chain value pools. Lesson: Zakura adds one step for each block from a
  seed that it derives in the same way. Thus the sum has a closed form, and the state format
  does not change.
- An NSM error is a `ConsensusError` inside `CoinbaseError`. Thus the node driver treats it as
  a local fault (stop, no invalid mark). The coinbase value rule already limits the balance. A
  failure of the NSM rule shows wrong pools of this node.
- `CoinbaseTerms::at` has no chain value pools and fails from the NSM reissuance height
  (Testnet 7,305,222). Block validation and the template use `CoinbaseTerms::after`. The node
  gives the total of the pools after the parent to the template in the tip event
  (`Tip::issued_supply`). The template does not read the state. From that height, a tip
  without the value is an error.
- `getblocktemplate` `coinbasetxn.fee` is the negated miner share of the fees, as in Zakura.
  Lesson: a value field of the shim that repeats a value of the coinbase must come from the
  coinbase (`CoinbaseTx::miner_fees`), not from the fee total.
- By the rules, Regtest has no reissuance height. `RegtestConfig::with_test_reissuance_height`
  names one for tests, as the test-only parameter of Zakura does. No configuration file sets
  it.
- The header context is 113 blocks (the window of 102 blocks of ZIP 218, plus 11). A state of
  an older build holds 28 blocks. The difficulty rule then gives `ContextTooShort` until the
  node has 113 blocks.
- Lesson: the fuzz cases must stay below the block before NU7. That block has a rule on the
  total of the pools, and the pools of a case are not the pools of the chain.
- Not done:
  - ZIP 2008 (Mainnet has no NU7 height; a test fails when it gets one)
  - the seed setting of a Zakura Regtest configuration.

## 2026-10-05 — Fast CI: `baselines` feature and `slow` modules (hayai-t1r)

- `hayai-bench` has a default feature `baselines`. The feature contains:
  - the zakura-* and `zebra-chain` dependencies
  - the modules and bench targets that use them
  - `sysbench`
  - each test that compares hayai with these libraries.

  The fixtures, the functional tests and the published vectors need no feature.
- The CI of each push selects by cargo feature and by test filter only: `--workspace
  --exclude hayai-fuzz --no-default-features --features upstream` and `-- --skip slow::`. The
  local gate and `full.yml` stay the full run.
- Lesson: `--no-default-features --features zakura` also removes `baselines`. The Zakura
  backend with the comparisons is `--features zakura,baselines`. Without it, cargo skips the
  bench targets and `conformance_nu7` with no message.
- Lesson: a dev-dependency cannot be optional. `zakura-header-chain` and `chrono` are optional
  dependencies of the feature.
- Measured with warm fixtures: the whole test run is about 560 CPU-s, and the fuzz smoke run is
  226 CPU-s of it. Only 4 tests of `hayaid` are in a `slow` module (the stops at random points,
  and 3 scenarios that wait for the timeout of a peer).

## 2026-10-05 — First pair with a real zakurad: legacy peers of the Zebra family

- Until now, a legacy peer was a zcashd in the tests. Zebra and Zakura are different in 4
  points, and each one was a defect of hayaid (`docs/regtest-pair-findings.md`).
- They read the chain of a peer with `getblocks`, and the connection waits 6 s for the answer.
  The node answers with the hashes after the locator, or with its tip hash. Lesson: an empty
  `inv` is worse than no answer, because Zakura counts it as a stall and disconnects after 3.
- They answer at most 16 blocks and 1 MB for one `getdata` message, and send nothing for the
  other requests. The scheduler sets the size of a message below these limits. Lesson: a
  request without an answer is not always a stall of the peer. First compare with the answer
  limits of the reference.
- They ban a peer that sends an invalid block. A legacy peer now gets the `inv` and the headers
  of a block after its validation. The compact-relay peers still get the block after the
  header check. Lesson: "forward before validation" is a rule of the extension only. On
  Regtest a valid header costs nothing. On a network with proof of work, it costs one block to
  cut each forwarding node from its Zakura peers.
- They drop inbound requests under load. The free-again rule of the scheduler covers this.
- They announce a transaction one time, to a part of their peers, and read the mempools of
  their peers for the rest. The node now sends `mempool` to its legacy peers each 60 s.
  Lesson: an answer to a request is not sufficient. Find out which requests the reference
  expects from its peers.
- They disconnect a peer below the protocol version of the active upgrade. The NU7 rule set
  came without the NU7 protocol version, and only a run across NU7 with a real peer showed it.
  Lesson: a new rule set has a peer-to-peer part (version, minimum peer version). Test an
  upgrade against the reference node, not only against its library.
- `getblocktemplate`: `curtime` must be a valid header time. Lesson: the clock of the node is
  not valid on a chain whose newest blocks are old.
- The RPC server has query methods (`NodeQuery`), because a comparison of 2 nodes needs the
  state of both through the same interface.
- Zakura with the legacy stack writes no commit trace rows, so the pair measures both nodes
  from outside. A Zakura node that stops loses its newest non-finalized blocks. Thus a test
  must not use its restart as a disconnect.

## 2026-10-05 — Unpaid action limit 0 (owner decision)

- `BLOCK_UNPAID_ACTION_LIMIT` is 0, the value of Zakura and Zebra (ZIP 317 gives 50 as the
  default). Reason: equal relay behaviour with the other nodes. The mempool refuses a
  transaction with an unpaid action, as `mempool_checks` of Zakura does, and the second pass
  of the template adds none.
- The minimum relay fee rule and the second pass stay in the code, as in Zakura. They decide
  only under a policy with a higher limit, and their tests set such a limit.
- Lesson: a test transaction needs the conventional fee of its logical actions (15,000
  zatoshis for one transparent input and one Orchard or Ironwood output).

## 2026-10-05 — Fee policy values of Zakura (owner decision, hayai-dh9)

- The policy, the store and the template use `Zip317Params::ZAKURA`: marginal fee 400
  zatoshis, 2 grace actions, weight ratio cap 13. `MIN_RELAY_FEE_CAP` is 800. Reason: equal
  relay behaviour with the other nodes. `docs/mempool-policy.md` has the table.
- `Zip317Params::ZIP317` stays for the arithmetic tests of hayai-template and hayai-rpc. The
  fixtures of hayai-bench still pay 5,000 zatoshis for each action.
- The node names the parameter set in 3 places (`MempoolPolicy::of`, and 2
  `PreparedStore::new` calls in `hayaid/src/node.rs`). They must name the same set.
- Lesson: a policy test must state its fee as a multiple of the marginal fee of the policy.
  The tests with a literal fee of 15,000 zatoshis needed a change.

## 2026-10-05 — Regtest funding streams and lockbox disbursements (F1, hayai-op5, hayai-40v)

- `RegtestConfig` takes lockbox disbursements and funding streams with the meaning of the
  Regtest parameters of Zakura. `CoinbaseTerms` is the one place that reads them, so the
  coinbase check and the template agree without a change.
- The rule of Zakura on an NU6.1 height without a disbursement is in `CoinbaseTerms`
  (`ConsensusError::NoLockboxDisbursement`): the block is not valid. hayaid also refuses such a
  `[regtest]` section at its start, because a node stops when it cannot make a template on
  its tip.
- A check that needs the activation heights (address periods) runs on a `Network`.
  `with_funding_streams` makes one from a copy of the configuration, which stays in memory. The
  stream tables also stay in memory.
- Regtest takes a P2SH address of any network, as Zakura does, because the script has the hash
  only.
- Lesson: the reference has its own rule for an absent configuration value. Compare the
  behaviour of both nodes on the empty configuration, not only on the full one.

## 2026-10-05 — Lane publication and private transactions (owner decision, hayai-m7h)

- `[mining] lane_publication`: `all` (default), `public`, `none`. The key decides only which
  part of its own template the node publishes. The feature bits do not change. A bit states
  what a node can receive, and no peer waits for a candidate.
- A private transaction (`sendprivatetransaction`) is in the store and in the template. The
  relay reads the store through `mempool::PublicTxs`, which does not have the private
  transactions. This one place covers `inv`, `TxAnnounce`, `getdata`, `TxRequest`, `mempool`
  and the id form of a compact block (prefilled). The driver keeps the private ids out of the
  lane.
- A separate method, and not a parameter of `sendrawtransaction`. A Zakura or zcashd node
  ignores an unknown parameter and publishes the transaction. An unknown method fails.
- A node with `all` refuses the method. A silent public relay is worse than an error.
- Lesson: in a test with 2 nodes, the second node publishes its own lane, and the first node
  sends it on. A test that reads "no batch on the wire" needs a second node with `none`, or no
  second node.

## 2026-10-05 — Operator RPC methods with the fields of Zakura (hayai-m7h)

- `hayai-rpc/src/info.rs` has the shapes, and `hayaid/src/query.rs` has the node state
  (`NodeQuery`). Scenario `rpc` of the Regtest pair compares each answer with zakurad.
- `stop` and `addnode` work on Regtest only, as the bodies of Zakura do.
- The node holds the state after a block (tree roots, tree sizes, value pools) for the layers
  and the base only. For an older block, `getblockheader` and `getblock` leave out the fields
  of that state. The node does not store the genesis block.
- Not served: `getblock` with verbosity 2 (the transaction object of `getrawtransaction`), and
  `errors` of `getinfo`.
- Lessons from the comparison with zakurad:
  - Zakura answers a parameter error with code -1, not -32602.
  - The difficulty limit of Zakura is the target of the compact form of the limit. The full
    Regtest limit gives 1.0000000596, not 1.0.
  - 2 Regtest producers with the same coinbase script make the same block in the same second.
    A fork test needs 2 scripts.
  - A test address must come from a key. A made-up Sapling or Unified address is not valid,
    and both nodes then agree on "not valid".

## 2026-10-05 — Cookie authentication of the RPC server (hayai-8g4)

- The RPC server has the cookie of Zakura:
  - file `.cookie` with `__cookie__:<secret>`, mode 0600
  - HTTP Basic
  - keys `enable_cookie_auth` (default on) and `cookie_dir`.

  The default directory is `data_dir`. `hayai-rpc/src/cookie.rs` owns the file and the header
  rule. `HttpServer` removes the file at its shutdown.
- A request without the credentials gets the status 401, as in zcashd. zakurad closes the
  connection without an answer. `stop` and `addnode` stay Regtest only, as the bodies of Zakura
  do. The `/metrics` server stays open, as in Zakura.
- Each client in the repository reads the cookie file:
  - the tests of hayaid
  - the Regtest pair (both nodes have the cookie, with one client code)
  - `scripts/regtest_pair.sh`.

  The tests of `hayai-rpc` that have another subject start the server without a cookie.
- The workspace has no base64 crate, so `cookie.rs` has the 2 functions.
- Lessons:
  - `curl -u ""` asks the terminal for a password and blocks a script. Read the cookie file
    first, and stop when it is absent.
  - The server reads the body of a request before it answers 401. An answer before the read
    can be lost when the server closes a connection with data that it did not read.

## 2026-10-05 — `tip-height` prints the restart tip (owner decision, hayai-l91)

- `hayaid::node::stored_tip` is the rule of the restart without the validation:
  1. the best block of the coins store
  2. the record of `state.log` for it
  3. the stored blocks that extend it.

  `replay` and `stored_tip` share `replay_end` and `stored_child`, and `StateLog::open` and
  `StateLog::resume_point` share `select`. A change of the restart rule goes into these
  functions.
- Each store has a read that writes nothing: `hayai_coins::stored_best_block` (header of the
  snapshot and scan of the log, or a read-only open of RocksDB), and
  `BlockStore::open_read_only`.
- Lessons:
  - A read-only open of RocksDB 11.8 makes no `LOG` file and takes no `LOCK`. A test that
    compares the files before and after showed it, so no logger option is necessary.
  - `zakurad tip-height` writes its error to stdout and exits with the status 0, and it does
    not take `regtest` as a network. hayaid keeps stderr and the status 1.

## 2026-10-05 — Comparison of zakurad and hayaid for each block (race package, version 2)

- `docs/zakura-measurements.md` is the reference for what each node measures. A panel or a
  table pairs 2 metrics only when that document gives the verdict CLOSE, and the panel states
  the difference. A quantity that one node does not measure has no pair.
- A metric with a name of Zakura must have the definition of Zakura. 3 of them did not
  (`state_finalized_block_height`, `sync_block_verify_duration_seconds`,
  `sync_downloads_in_flight`), and `mining_template_rebuilt` counted another event. Lesson:
  read the code point of the Zakura metric before the use of its name.
- The block clock (`docs/hayaid.md`): one `Instant` for the reception of a block goes with the
  block from the reader thread or the relay to the commit and to the first template. A trace
  field and the gauge of the same quantity come from one clock reading, so a test can compare
  them for equality.
- Gauges of the last block have the height as a value, never as a label. Thus Prometheus
  cannot join 2 nodes on the height. The dashboard has one panel for each node (Grafana trend
  panel, X = height), and `scripts/race_blocks.py` makes the joined table.
- zakurad exports summaries. Only `_sum` and `_count` are comparable with a histogram of
  hayaid. A rule for "the value of the last block" reads the increase of both over 2 samples,
  one scrape after the commit (`race:contextual_commit_seconds:last`).
- Lessons:
  - Prometheus 3 has range windows that are open on the left: `[10s]` at a scrape interval of
    5 s has 2 samples, not 3.
  - A series with one sample for each block needs `last_over_time(...[$__interval])` in a
    panel. Without it, a step above the scrape interval misses most samples.
  - The Grafana xychart panel showed "Err" for each mapping in a headless browser. The trend
    panel works with a frame that has one row for each height (join on the time, then group by
    the height).
  - zakurad with `[tracing] log_file` writes no log to the output of its container.

## 2026-10-05 — RPC caller of the race (`scripts/race_rpc_caller.py`)

- One Python program (standard library) runs next to each node in a pinned public image. No
  node image has Python, and a second compose service needs no image build. The program reads
  the RPC address from the configuration of the node, and reads the cookie for each call.
- The long poll is off by default. Both nodes count the wait of a long poll in
  `rpc_request_duration_seconds`, so a held long poll makes the dashboard mean useless.
  `blocks.md` has the client-side mean of the calls without `longpollid` in each case.
- "Template served" uses the commit time of each machine (trace row of hayaid, log line of
  zakurad) and the wall clock of that machine. No value crosses 2 machines.
- Lessons:
  - hayaid holds a `getblocktemplate` call only with the capability `longpoll`. With the
    `longpollid` alone, it answers at once: the first caller made 190,000 calls in 50 s. A
    client loop on a long poll needs a wait when the answer comes back at once.
  - A wrong cookie: hayaid answers 401, and zakurad closes the connection. hayaid removes the
    cookie file at its stop, and zakurad keeps it.
  - A container that reads a file of mode 0600 of another user needs root with
    `DAC_READ_SEARCH` only (`cap_drop: ALL`).
  - Python as process 1 of a container ignores SIGTERM without a handler.

## 2026-10-05 — Header sync: silence and the idle poll (`crates/hayaid/src/sync.rs`)

- Zakura, Zebra and zcashd send no `headers` message when they have no header after the
  locator. The stall rule of the header sync disconnected each such peer after
  `header_timeout_ms`, also the only peer of the node.
- The role of the header sync, with its stall rule, goes only to a peer with evidence of more
  headers:
  - a reported height above the best header
  - a full `headers` message with a new header.

  Each other peer gets `getheaders` and no role.
- Idle poll: one `getheaders` to one peer in rotation, with a delay that doubles from
  `header_poll_ms` to `header_poll_max_ms`. A new block sets the delay back.
- Lesson: silence is a stall only with evidence that the peer has more than the node. Before a
  timeout becomes a penalty, read what the other implementations send for an empty result.

## 2026-10-05 — First sync of Testnet: lost peers, the withheld rule, the header log

- Chain of causes of the stop at height 4,393,339:
  1. A row of 2 MB blocks near 4,308,000 followed small blocks.
  2. A Zakura peer answers at most 1 MB of one `getdata` message.
  3. When the peer answered a later message, the scheduler gave a stall for the other 15
     requests.
  4. 2 stalls disconnect a peer, and the node refuses it for 10 min for each stall.
  5. The peer that stayed answers `notfound` for each block.
- The scheduler frees the requests after the answered blocks of a message that reached 1 MB
  when the peer answers a later message. A request before an answered block of its message
  keeps the stall rule, because the peer keeps that block back.
- The withheld rule ended an exclusion at each `headers` message of the excluded chain. The
  exclusion itself sends `getheaders`, so the node did one exclusion each second. Now only a
  peer that connected after the exclusion ends it with its headers.
- An exclusion moved more than 65,536 headers of the best chain into the side set. The limit
  then removed the newest of them, and the chain wrote a second record when a peer sent them
  again. The limit does not count excluded headers.
- The header log is an operation log: a start must get the entries of the run. A header that
  the chain removed and accepts again gets a mark record, not a second header record. Without
  the mark, a start does not know that the header came back.
- Lessons:
  - A state in memory only (the exclusion) must not change what the log replay does. Each
    removal that a run does and a start does not do gives a log that the start refuses.
  - A rule for the limits of another implementation needs a test with a change of the block
    size, not only with one size.
  - A rule that sends a request must not take the answer to that request as news.
  - `peers = 1` in the warning shows the cause in the first line. A debug log was necessary to
    find it.

## 2026-10-06 — Race sidecar: one method on both machines

- A compared quantity comes from one program with the same method on both machines (the
  sidecar, `scripts/race_sidecar.py`), not from the metric of each node. Node metrics with one
  name had different meanings. `rpc_request_duration_seconds` contains the wait of each long
  poll on both nodes, and zakurad has no process metric.
- The resources of a node come from the cgroup v2 of its container. A fixed cgroup parent
  (`race-<node>.slice`) gives a known path. A read-only mount of `/sys/fs/cgroup` reads it
  without privilege and without the Docker socket. `io.stat` lists a device-mapper device and
  its disk, so count only the devices without `slaves`.
- The live Zakura value uses the calibration of `scripts/race_blocks.py` again, on a limited
  number of recent rows. 2 programs with one function cannot give different results.
- A textfile with its own label `node` needs `honor_labels` on the node-exporter job. If not,
  Prometheus renames the label to `exported_node`.

## 2026-10-06 — Wallet index (hayai-1bd)

- Optional, one key (`[state] wallet_index`), off by default. Zakura has no such key, because
  its archive mode always writes the indexes. The index needs the chain from the genesis block,
  so a node turns it on with an empty `cache_dir`.
- The block is its own access list. The validation keeps the coins of the inputs in the layer
  (`Layer::spent_coins`), and the driver takes them out before the push. The index reads no
  coin, and the balances are merge operands, so no write reads first.
- A writer thread with a bounded queue keeps the work off the driver. The waiting blocks go in
  one write batch. Each block has an undo record, so a reorg and a start undo without the
  block files.
- Consistency: the durable index holds the base block before the coins store names it. A start
  undoes the index to the base, and the replay indexes the blocks again.
- The first version drained the queue and synced the index log before each coins flush, and
  the driver waited. This took 144 s of a 24 min Testnet sync, almost all of it in the sync
  (one block for each write batch, 15 µs for each write). The sync now runs in the background
  from one flush to the next. A sync holds the next base when 2 conditions are true:
  - Its tip is at or above the base.
  - No undo after its request went to or below the base.

  The finality depth is above the flush interval, so this is the usual case. Rule: an order of
  2 writes needs only the end of the first write before the start of the second, not a wait in
  the driver.
- Open: the write-ahead log is on. The variant without the log (an atomic memtable flush of all
  column families at each persist) is not measured yet.

## Consensus trace (2026-10-06)

- `docs/consensus.md` is the one place for the consensus coverage: each normative rule of a
  ZIP or of the specification is a row with 3 columns, `Rule | Code | Test`. A missing or
  untested rule shows as such; nothing is summarized away.
- Lesson (owner feedback on the first version): a trace with rule ids, group codes, a status
  legend, a Zakura column and code given as file names is hard to read. The Code column links
  to the line of the check at a fixed commit, and a script checks that each link lands on its
  identifier. A rule without code says "Not implemented" and why the node is correct without
  it. Narrative about validation paths belongs in `docs/architecture.md`.
- Code marks: `// ZIP <n>: ...` and `// Spec §<section>: ...` at each check and constant.
  Search for them to find the code of a rule.
- Lesson: an upstream function that computes a digest is not a consensus check. The upstream
  v5 sighash hashes an empty output list for `SIGHASH_SINGLE` without an output, and ZIP 244
  requires a failure. The deployed nodes (zcashd, Zebra, Zakura) apply it as a failed signature
  check, not as a failed transaction, so `<pk> OP_CHECKSIG OP_NOT` passes; hayai does the same. A rule that an upstream crate seems to enforce needs a refused-input
  test in hayai.
- Lesson: a setter that nothing calls hides a missing rule (`Relay::set_min_peer_version`,
  `PreparedStore::relay_ids`). A rule needs a test at the level of the node, not only of the
  helper.

## 2026-10-07 — hayai-fixtures

- The fixture generator moves from `hayai_bench::fixtures` to the new crate `hayai-fixtures`.
  The crate depends only on `hayai-crypto`, `hayai-wire`, `hayai-consensus` and 4 small
  crates (`secp256k1`, `rayon`, `sha2`, `bytes`).
- Reason: `hayai-fuzz` used `hayai-bench` only for the fixtures. Through `hayai-bench` it
  built the benchmark scenarios, the system metrics, `perf-event2` and the benchmark baselines.
- The chain fixture and `scratch_dir` stay in `hayai-bench`. Only `hayai-bench` uses them, and
  the chain fixture needs the RocksDB coins store and the block shapes of the benchmark models.
- The test vectors under `crates/hayai-bench/tests/vectors/` stay. Other crates read them by
  path and do not depend on `hayai-bench`.
- Rule: a crate that needs test data or a test helper of another crate depends on a support
  crate, not on a benchmark crate.

## 2026-10-07 — RocksDB optional in hayai-coins (hayai-dny)

- The RocksDB backing of `hayai-coins` (`RocksBacking`, `Config`, the error variants `Rocks` and
  `MissingColumnFamily`) is behind the cargo feature `rocksdb`.
- Reason: `hayai-state`, `hayai-prepared`, `hayai-validate` and `hayai-fuzz` use only the types,
  the cache and `MemBacking`. Before, each of them built the RocksDB C++ library. Another node
  has its own database, and a WASM target cannot build RocksDB.
- `hayaid` and `hayai-bench` construct a `RocksBacking` and turn the feature on at their edge.
- `rocksdb` is in the default features of `hayai-coins`. Thus `cargo test -p hayai-coins` runs
  every test. The workspace edges set `default-features = false`, so the default does not reach
  a dependent crate.
- The test target `store` has `required-features = ["rocksdb"]`. In `tests/mem.rs`, only the
  tests that use `RocksBacking` need the feature, so the `MemBacking` tests run without it. The
  CI command (`--workspace --no-default-features`) still runs the RocksDB tests: feature
  unification takes `rocksdb` from the edge of `hayaid`.
- Without the feature, `stored_best_block` reads only the memory backend. A directory without
  its log is an error.

## 2026-10-07 — Chain parameters as data (hayai-wku)

- A `ChainSpec` holds each value of a network that the rules read: the activation heights,
  `NetworkParams`, the checkpoints, the funding streams, the lockbox disbursements, the
  founders' addresses, the NSM seed and the `zcash_protocol` network type. A fork clones a
  built-in spec, changes its fields and calls `ChainSpec::network()`. The fork does not edit
  hayai-consensus.
- Decision: `Network::Custom(CheckedSpec)` replaces `Network::ConfiguredRegtest`. Only
  `ChainSpec::network()` makes a `CheckedSpec`, and two of them are equal when they are the
  same spec in memory. Mainnet, Testnet and Regtest stay variants with a `static` spec.
  `Network::spec()` is one `const` match, `Network` stays `Copy` and 16 bytes, and no function
  of the crate matches on the network.
- Rejected: a struct `Network(&'static ChainSpec)`, which changes each `Network::Mainnet` and
  each pattern. Rejected: a parameter trait, which adds a generic or a `dyn` call on each
  rule path for one implementation.
- Rule: `ChainSpec::network()` refuses each value that a rule would later meet as a panic or
  as a rule without code (17 reasons, one `ChainSpecError` variant each). The checks of a
  Regtest configuration and of a spec are the same functions. Lesson: a review found 6 such
  values after the first version, from a division by 0 to an NU7 height on Mainnet without
  the ZIP 2008 rule.
- `Upgrade` stays a closed enum, because the rules of an upgrade are code. The magic bytes
  and the ports stay in hayai-net and hayaid. hayai gives no `Parameters` value to an
  upstream crate: `rules_at` gives the branch id, and the network type gives the address
  encodings. The upstream `NetworkType` has 3 values, so a chain uses the address encodings
  of Mainnet, Testnet or Regtest.
- The Mainnet and Testnet activation heights are constants of the specs. A test compares
  them with the `zcash_protocol` of the backend. The compiler decodes the embedded checkpoint
  lists, and the first halving height is a private field that `ChainSpec::network()` derives.
  Thus a lookup costs what the old `match` cost. The validate benchmark shows no change
  above the noise of the machine.
- Lesson: the Mainnet and Testnet tables hold one address for a stream that repeats it (ZIP
  214 `[a] * n`). A Regtest configuration needs one address for each period, as in Zakura.
  Thus the check of a spec accepts one address for many periods, and the check of a Regtest
  configuration does not. The strict check needs the network that `ChainSpec::network()`
  makes, so it runs after the lenient check. When two streams have too few addresses, the
  `StreamAddresses` error can now name another stream than before.

## 2026-10-07 — Pure consensus core (hayai-szv, M12 stage 1)

- Decision: the consensus rules of hayai-consensus move to the new crate
  `hayai-consensus-core`. The crate stays in the Rust subset that Charon and Aeneas
  translate to Lean:
  - index loops, no iterator chain, no `HashMap`, no `as` cast, no panic;
  - checked arithmetic on each amount and each height, with the `MAX_MONEY` bound.

  Reason: the owner wants a Lean specification of the rules, with bridge proofs that this
  crate satisfies it (`~/prog/zcash/hayai-formal-verification.md`). hayai-consensus is the
  adapter: the networks, the address decoding, the checkpoints and the upstream types.
- `CoreSpec` is plain data with scripts, not addresses. The three built-in specs hold it as
  constant data. A `const fn` Base58 decoder gives the scripts at compile time, and a test
  compares each script with the upstream decoder. A custom chain gets its core from
  `ChainSpec::network()`, which decodes the addresses at run time.
- The core returns an error for every value that `CoreSpec::checked` refuses
  (`UncheckedSpec`, `DivisionByZero`), never a panic. The adapter maps such an error to
  `unreachable!` in the wrappers whose result cannot fail on a checked spec. One
  `ConsensusError` enum serves both crates. Only the adapter builds `UnsupportedUpgrade` and
  `NoRuleSet`: the core has a rule set for every upgrade, and the backend decides which
  branch ids it knows.
- Each path selects the rule set of a height one time. The adapter selects it with
  `rules_at`, which also refuses an upgrade without a backend branch id. It passes the rule
  set to the rules of the core: `expected_bits`, `check_contextual`, `CoinbaseTerms::at` and
  `CoinbaseTerms::after` take `rules: &RuleSet`. A core function that selected the rule set
  again would double that cost on the per-header path.
- The 256-bit arithmetic of §7.7 is `Uint256`: 4 limbs, with the operations of the rule
  only. The work of a block is a restoring division that starts below the bit length of the
  divisor: 57 steps for a Mainnet target. hayai-consensus tests it against
  `primitive_types::U256` on 2,000 random values and 20,000 random compact forms.
- Behaviour that changed, on inputs that no valid chain reaches:
  - `miner_fee_share`, `reissuance_bonus`, `CoinbaseTerms::miner_fees` before NU7,
    `deferred_pool_after` and `funding_streams` refuse an amount above `MAX_MONEY`. Before,
    the arithmetic wrapped or the value passed.
  - A slow start of 1 block or a halving interval of 0 is an error of the schedule, not a
    wrap.
  - `ChainSpec::network()` runs its checks in another order. A spec with two faults can
    report another error than before.
  - `CoinbaseTerms::check` matches the required outputs in output order. When two
    unmatched outputs share a script or a value, the `found` detail of `WrongAmount` or
    `WrongScript` can name another output than before.
  - The header rules refuse an upgrade without a backend branch id (NU7 on the upstream
    backend) before the version, target, time and `nBits` rules. Before, that refusal came
    after the time rules and before the `nBits` rule.
- Lesson: the compact form of a target keeps its top 3 bytes. A chain whose blocks state
  the compact limit gets one unit below the limit as its expected `nBits` on an on-target
  window. A test that expects the limit there is wrong, not the rule.
- Lesson: the funding stream tables exist two times: with addresses for the configuration
  and the RPC, and with scripts for the core. A const fn cannot build a slice of scripts
  whose length varies by stream, so a test compares the two tables field by field.

## 2026-10-07 — hayai-mempool out of hayai-prepared (hayai-0sm)

- Decision: `PreparedStore`, `MempoolPolicy` and the admission order go to the new crate
  `hayai-mempool`. `hayai-prepared` keeps the preparation only. Reason: the store pulled
  hayai-template into hayai-prepared, and thus into hayai-state and hayai-validate.
- `PreparedTx::candidate` is removed. The store makes the candidate with
  `Candidate::from_raw` from the facts of the `PreparedTx`. Thus the preparation needs no ZIP
  317 type, and no ZIP 317 code moves.
- hayai-validate reads the known transactions through the trait `PreparedLookup` of
  hayai-prepared, as a generic parameter. The lookup of each transaction is a static call.
  hayai-validate and hayai-state do not depend on hayai-mempool.
- The first version made the whole validation body generic, so each caller crate compiled
  its own copy of the body. Now only the lookup loop is generic. The body is not generic and
  takes the loop as one `dyn` call for each block. The wall time of the first version read
  5 % slower in 3 runs on a machine with a load average above 8, which is not a valid
  measurement; the counter A/B of the final version shows equal instructions and cycles.
- Lesson: wall time on a loaded machine is noise. The decision measure of a refactor is the
  instruction and cycle count of an interleaved A/B of the old and the new binary.
- The split of the admission: `hayai_mempool::Mempool` holds the order of the checks, the
  insert on the tip of the checks, the retries after a tip change and the readmission after
  a reorg. hayaid keeps the peer score, the gauges, the private transactions and the
  transaction sink of the relay. A node with another relay reuses the crate as it is.
- The Regtest spend builder of the node tests moves from hayaid to `hayai_fixtures::regtest`,
  so the admission tests move with the admission.

## 2026-10-08 — State persistence into hayai-state (hayai-tlq, M6)

- `hayaid/src/persist.rs` moves to `hayai_state::persist`: `RecordLog`, `StateRecord`,
  `StateLog`, `Recovered`, `ResumePoint` and `PersistError`. The record format does not
  change. Reason: the record is the encoding of `BaseState`, and a node that embeds the
  state crate needs the same restart rule as hayaid.
- The record names the mode of the node that wrote it. `hayai_state::persist::Mode` (full
  or shadow) replaces `hayaid::config::Mode` in the record; hayaid converts its
  configuration value. The network of the record is `hayai_consensus::Network`, as before
  (`NetworkKind` is an alias of it).
- `hayai-bench` builds on macOS: the `perf_event_open` counters are behind
  `cfg(target_os = "linux")`, and on another system every hardware event is unavailable
  (the `Meter` then reports no counter, as on a kernel that refuses them). The tests that
  read `/proc` stay Linux-only.

## 2026-10-08 — Layering guard (hayai-yq7, M11)

- `scripts/layering.sh` fails when a crate depends on a crate after it in the crate list of
  `docs/architecture.md`, when a pure crate names `std::fs`, `std::net`, `std::thread`, a
  clock or a lock outside its tests, when an in-memory crate names `std::fs` or `std::net`
  outside its tests or pulls RocksDB or tokio, and when a crate outside hayai-net, hayai-rpc
  and hayaid names `std::net`. CI runs it as the job `layering`, without a build.
- The scan is by token, as `hayai-consensus-core/tests/subset.rs`: the scan of a file stops
  at its `#[cfg(test)] mod tests`, and the files named `tests.rs`, `*_tests.rs`,
  `test_support.rs` and `test_util.rs` do not count. Lesson: a probe appended at the end of
  a file lands after the test module and does not count; a check of the guard puts the
  probe at the top of the file.
- `hayai_state::persist` is the one file of an in-memory crate that names `std::fs`. The
  exception is a list in the script, with the reason beside each entry.

## 2026-10-08 — One header index component (hayai-vfx, M4)

- `hayaid::headers::HeaderIndex` and `SeedBlock` move to `hayai_sync::index`. The index
  implements `hayai_relay::HeaderContext` (`has_block`, `parent`, the system clock), and
  `hayai_relay::StandardHeaderCheck` is the one implementation of the header check: it
  gains `trust_short_context` and `verify`, which returns the height and the rules that a
  trusted short context left unchecked (`Verified`). hayaid's `NodeHeaderCheck` keeps only
  the trace row, the `trusted_bits` counter and the pending entry of the relay path. The
  inline copy of the rules in hayaid is gone. `HeaderIndex::median_time_past` had no
  caller and is removed.
- Not done: in full mode the `HeaderChain` and the index stay two structures, linked by the
  commit order of the node. The relay's check reads the index, so a relayed block whose
  parent is a header-only entry of the chain is `ParentUnknown` and goes to the sync as a
  `BlockInv`, and a relayed block is checked twice (relay, then `Sync::relayed`). The
  merge (the chain as the relay's context, a seeded chain for shadow mode) changes the sync
  and needs the node network tests, which bind `127.0.0.x` and do not run on macOS. It is
  the follow-up issue of M4.
- Measurement, on this machine (macOS, wall time, 3 runs each): `parent_context` 150 to
  158 ns before and 157 to 170 ns after, `verify` on Regtest 730 to 750 ns before and 719
  to 773 ns after. The bands overlap: no change above the noise. The bench
  `hayai-bench/benches/headers.rs` stays for the gate of M13 on Linux (instruction counts).

## 2026-10-08 — Sans-IO relay policy (hayai-set, M3)

- `hayai-net/src/relay.rs` splits into `policy` and `relay`. `RelayPolicy` holds the peer
  set and every decision of the both-paths relay; it names no socket and no thread. Every
  message that leaves and every connection that ends goes through the trait `Io` (`send`,
  `queued_bytes`, `close`) that the caller passes with each event, and the tick takes its
  `now`. `Relay` is the shell: the TCP transports, the acceptor, the dialler and the
  ticker; it implements `Io` with the transport of each peer and keeps the public API, so
  no caller changes.
- Decision: the policy keeps the locks of the state (15 mutexes over the peers, the recent
  blocks, the pending compact blocks, the lanes, the candidates), one `Io` call replaces
  each transport call (one indirect call), and the bodies of the handlers do not change.
  Reason: one lock in place of the fine-grained ones would serialize the reader threads of
  the peers behind the header check and the parsing, and this machine cannot measure that
  (no perf counters, and the peer tests bind `127.0.0.x`). A state machine that returns
  its messages in place of calling `Io` is the next step when the gate can measure it.
- Performance: no algorithm, lock, loop or thread changes. Each transport call becomes one
  call through `&dyn Io` (an indirect call that the shell answers with one map lookup under
  the transports lock, the send after its release); the handler bodies and the lock order
  are the same. The relay bench `forward_latency` of the baseline (v2 forward on ids 260 µs
  to 1.0 ms, v1 26 to 31 ms with the simulated 20 ms round trip) is in the M13 gate.
- The policy tests drive it with a recording `Io` and the stores of an empty node: the
  handshake, an `inv` answered with `getdata` and the bytes announced onwards, a block
  announced to the legacy peers after its validation except to its source, a peer whose
  queue is full closed through `Io`. The loopback tests (24) and the peer tests are
  unchanged.

## 2026-10-09 — The node as a library (hayai-341, M1)

- The library of hayaid moves to the crate `hayai-node`: `Config` with its TOML form,
  `Node`, the driver, the sync, the shadow follower, the wallet index attachment, the RPC
  and the metrics. `hayaid` is the binary: `main.rs` reads the configuration file, the
  signals and the logs, and has the CLI (`start`, `generate`, `tip-height`). The module
  paths do not change (`hayai_node::node`, `hayai_node::config`, ...).
- Decision: `Config` goes with the library. The node tests build nodes from TOML through
  `Config::parse`, and another node constructs the same struct; the TOML derive is its
  serialization, not a file format of the binary. hayaid keeps the file lookup and the
  defaults of `generate`.
- `NodeBuilder` takes components of the caller in place of the ones that the
  configuration names: the tracer, the metrics registry, the coins store with its best
  block, and the block store. `Node::start(&config)` is `NodeBuilder::new(config).start()`.
  A store of the caller skips the empty-directory check of its path, and takes no
  snapshot. The test starts a Regtest node on a memory store and a block store of the
  caller, mines 3 blocks, and restarts on the same stores.
- Owner decisions of 2026-10-09: the driver is in the library; shadow mode is a library
  component; the wallet index is an optional library component. Shadow mode is in the
  library by `Config` (`[shadow]`), not yet behind a trait of the block source: that trait
  is the follow-up of M1, with the relay transport (the `Io` of M3) as its second
  implementation.
- `scripts/check_metric_names.py` and the docs point at `crates/hayai-node/src/`; the
  Zakura configuration fixtures of the config tests move to `crates/hayai-node/tests/fixtures`.

## 2026-10-09 — Smaller splits (hayai-vif, M10)

- `hayai-template-messages`: the messages of the template push protocol with their
  binary-frame and JSON-lines encodings, out of `hayai-template`. A miner decodes the push
  without the live template, the consensus rules or the trees. `hayai_template::messages`
  re-exports the crate, so no caller changes.
- `hayai-http`: the HTTP/1.1 server side (one request read with its bounds, one response
  written) and the cookie authentication, out of `hayai-rpc`. `hayai-metrics`: the
  Prometheus registry and the `/metrics` endpoint, out of `hayai-rpc`. `hayai-rpc` keeps
  the JSON-RPC methods and its HTTP front end on `hayai-http`, and counts its requests in a
  `hayai_metrics::Registry`. A node that serves no RPC takes the metrics alone.
- `hayai-shadow`: the upstream JSON-RPC client, the seed, the follower and the
  upstream-backed coins, out of `hayai-node`. The couplings to the node are cut without a
  copy: `parse_hash` is `hayai_wire::header::parse_hash`; the follower and the seed take
  `hayai_consensus::Network` in place of `NetParams`; the follower reports to the trait
  `UpstreamSink` (the node implements it with its event channel) in place of the driver's
  `Event`; `UpstreamBacking` takes the two counters of the trusted coins and nullifiers in
  place of `NodeMetrics`. Static dispatch everywhere (`Follower<S: UpstreamSink>`): no new
  call on the block path.

## 2026-10-09 — The links of docs/consensus.md follow the split

- The 1,883 links of the Code and Test columns pointed at `nikkolasg/hayai` at the commit
  `163279a`, before the crate split and before the consensus core. They now point at
  `zodl-inc/hayai/blob/main` and the files of the split. `scripts/consensus_links.py`
  rewrites them: it reads the text of each linked line in the old commit and finds it in
  the current tree (1,441 in the same file, 246 in the file that the split moved it to, 8
  elsewhere), then the definition of the symbol of the label (119), then a table of
  overrides for the rules that the core rewrote with other text (69, each read by hand).
  None is unresolved: no linked rule is gone. The script checks that each new link names
  an existing line, and a second run changes nothing.
- Lesson: a link pinned to a commit stays valid and goes stale. The script keeps the
  table on `main` and reports the links whose code moved without its text, which is the
  list of rules to read after a refactor of the rules.

## 2026-10-08 — Race deployment: private secrets, enabled units

- Lesson: Docker Compose outside swarm mounts a `secrets:` file with its owner and mode of
  the host, and ignores `uid`, `gid` and `mode`. A 0600 file of the operator is unreadable
  for Grafana (uid 472). The directory of the secret is 0700; the daemon resolves the bind
  mount as root, so the file can stay 0644. `deploy/terraform/aws` already did this.
- Lesson: `systemctl enable` on a unit without an `[Install]` section only warns, and the
  unit does not start at the next boot. A unit that must survive a reboot has
  `WantedBy=multi-user.target`.

## 2026-10-09 — Formal verification, stage A: the core translates to Lean (hayai-7yq.1, M14)

- `formal/` is a Lake project. A reader checks the proofs with `elan` and `lake build`
  only: the translation of `hayai-consensus-core` is committed (`formal/Hayai/Core`), and
  Charon and Aeneas (pinned by commit in `formal/TOOLCHAIN`) only regenerate it
  (`formal/scripts/extract.sh`). The CI job `formal-extract` regenerates and diffs; the job
  `formal` builds the proofs. Decision of the owner: the pins do not drive the design and a
  reader never builds the tools.
- The whole core is in the Aeneas subset. The first extraction refused 89 sites; each form
  was reduced to a probe crate and the core rewritten (`docs/formal-verification.md`,
  section "The subset in practice"). The forms with a cost in the hot path:
  - `CoreSpec` owns its tables (`Vec`), so `hayai-consensus` builds the core of each
    built-in network once (`OnceLock`) and of a custom network at `ChainSpec::network`.
    `Network::core()` is one atomic load; before, the tables were `&'static` slices.
  - `CoinbaseTerms::check` takes `&[CoinbaseOutput]` with an owned script: one small
    `Vec<u8>` per coinbase output per block check (a coinbase has 2 to 6 outputs).
  - `rules_at` returns the `RuleSet` by value (a `Copy` of about 100 bytes) once per
    block-level call; the adapter keeps its `&'static` rule sets.
  - `ParentChain` by value: it is `Copy`, three words.
  - A loop keeps its failure in a local and `break`s; the number of operations is the same.
- Six modules of the core are renamed, because Lean reads `spec.CoreSpec.checked` as a field
  of a local variable `spec`: `chain_spec`, `rule_sets`, `block_limits`,
  `subsidy_schedule`, `header_rules`, `difficulty_rules`. Rule: a module of the core has no
  name that a local variable would have (`tests/subset.rs`).
- The `Display`, `Debug` and `Error::source` bodies of `thiserror` are excluded from the
  extraction (their bodies use `dyn` and the formatter); no rule reads them. The
  standard-library items without a model in the Aeneas library are defined by hand in
  `formal/Hayai/Core/FunsExternal.lean` (13 definitions; 17 axioms for hashing and
  formatting).
- Lesson: `charon --exclude` matches an impl method with the pattern
  `{core::error::Error<_>}::source`, not with `core::error::Error::source`; and
  `--start-from-pub` is the way to leave the derived impls of private items out.
- Lesson: Aeneas refuses a function that borrows an argument and returns a `&'static`
  (the two lifetimes do not unify in its borrow model), but accepts the same function with
  a by-value result, and a function without borrowed arguments that returns `&TABLE[i]`.

## 2026-10-10 — Formal verification, stage B starts: specs, first proofs, progress table (hayai-ncv)

- `formal/Hayai/Spec/Difficulty.lean` and `Header.lean` transcribe §7.6 and §7.7 from the
  LaTeX source of the protocol specification (`zcash/zips`, `protocol/protocol.tex`), one
  definition per item, each named after it. They are the trusted part of a proof: a reader
  checks them against the document. Where the specification is not an integer formula
  (`256^(e−3)` for `e < 3`, the percentages of `PoWMaxAdjust*`) the file says how it reads it.
- `formal/proven.tsv` maps each proven rule of `docs/consensus.md` to its theorems;
  `formal/scripts/progress.py` generates `formal/PROGRESS.md` (the status of each of the 763
  rules) and checks with `#print axioms` that each theorem exists and uses no `sorryAx`. The
  CI job `formal` fails on a stale file. A theorem with a `sorry` never counts as a proof.
- First proofs: `check_version` and `check_local_time` decide their §7.6 rules exactly
  (the saturating addition of the code does not change the local-time rule), and
  `Uint256::checked_add` returns the sum or `None` at `2^256` and above.
- Lesson: a loop of the translation is `loop body x`; `loop.spec_decr_nat` with an invariant
  over the state tuple and the measure `4 - start` proves a limb loop without unrolling it.
- Lesson: `partial` is a keyword of Lean; and the `lean_lib` glob `.submodules` leaves out the
  root module, so `import Hayai` needs `.andSubmodules`.

## 2026-10-10 — The median of the core counts instead of sorting (hayai-ncv)

- `median_time` returns the time `t` with `#{x < t} ≤ len / 2 < #{x ≤ t}`: the element at
  index `len / 2` of the sorted list, with no copy and no sort. The old version copied the
  times into a `Vec` and sorted it by swaps. For the at most 11 times of a call, the new one
  makes no allocation and at most 121 comparisons. The proof (`formal/Hayai/Proofs/Median.lean`)
  needs one fact about sorted lists, where a proof of the swap sort needs an invariant of each
  swap. A test compares the result with a real sort on repeated values and lengths 0 to 13.
- Proven: `median_time` is `median` of §7.7.3, `median_time_past` is `MedianTime`; ZIP 200
  epochs (`upgrade_at`); every rule set that `rules_at` selects has the constants of §5.3;
  `bounded_timespan` is `ActualTimespanBounded`.
- Lesson: in this Aeneas version `Result` is a coinductive tree, so `x = ok v` cannot be split
  by cases; a fact about a constant of the translation is a weakest-precondition theorem
  (`RULE_SETS ⦃ a => … ⦄`), which also shows that the constant evaluates without error.
