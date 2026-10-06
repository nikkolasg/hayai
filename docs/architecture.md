# hayai architecture

hayai is the performance core of a Zcash miner node. It covers everything between a transaction
or block that appears on the wire and a miner that works on it. It is consensus-compatible
on the chain rules it implements. Everywhere else, it is deliberately incompatible with existing
node software (relay protocol, template delivery, storage).

The mempool policy is the policy of the public network (`docs/mempool-policy.md`). The
cryptographic primitives come from the upstream Zcash crates. The `zakura-*` forks appear only
as a benchmark baseline.

## Terminology

- **Wire bytes**: the exact serialized form of a block or transaction as the node received it.
  The node keeps the wire bytes for the life of the object and never serializes the object
  again.
- **Prepared transaction**: a transaction plus every context-free result that the node computed
  for it. The node stores it once. Mempool admission, compact-block reconstruction, block
  validation and the template build use it again.
- **Context-free**: a result that depends only on the transaction bytes, the contents of the
  outputs it spends, and the consensus-rule epoch. Txids, sighashes, script and signature
  results, proof results, fee, sigops, nullifiers and note commitments are context-free.
- **Contextual**: a result that depends on the chain the node applies the transaction to.
  Unspentness of inputs, coinbase maturity, nullifier uniqueness, anchor validity, expiry and
  lock time are contextual.
- **Rule epoch**: the consensus branch id plus the set of script flags in force. The key of a
  cached context-free result includes the rule epoch.
- **Layer**: the state delta of one block over its parent: coins created, outpoints spent,
  nullifiers added, tree frontiers and anchors after the block.
- **Coins**: the transparent UTXO set, keyed by outpoint.

## Metrics

- M1: block received → validated and committed.
- M2: block found → accepted locally and propagated.
- M3: tip or mempool change → new template at the pool.
- M4: sync and restart time.

The block spacing decreases to 25 s at NU7. Then 1 s of M1 or M2 delay costs ~4 % of blocks.

## Principles

1. **Verify once, carry the result.** Every context-free result stays with the transaction from
   first sight to block inclusion. A block made of known transactions costs contextual checks only.
2. **A block is its own access list.** The body of a block names every input, nullifier and
   anchor that the block reads. The node therefore does all reads in one parallel round before
   any check runs.
3. **State commits in memory; disk follows.** A block commit is the push of one layer. The flush
   to disk is batched and asynchronous.
4. **Relay before commit.** The node forwards a block after its header checks pass. The decision
   to build on the block waits for validation. The forward does not wait for validation.
5. **Bulk arrays through one pool.** The node lays out the validation work as flat arrays
   (inputs, bundles, leaves). A core-sized pool processes the arrays. No item has its own async
   step.
6. **Lane-oriented kernels.** Hash and curve kernels take arrays and run lanes in lockstep.
   Batch inversion, SIMD and later backends therefore apply at the same call sites.
7. **Disseminate before ordering.** Peers send transactions and the candidate batches of miners
   through the network continuously. A block announcement carries digests. DAG mempools
   (Narwhal, Bullshark, Autobahn, Quorum Store) use this separation, so throughput does not
   depend on consensus latency. Under proof of work, the header is the decision on the order,
   and no quorum is necessary.
8. **Pipeline across blocks.** Ingest, preparation, contextual check and persistence are
   separate stages with their own queues. The preparation of block N+1 therefore overlaps the
   commit of block N. This is the deferred-execution shape of Monad and the DAG BFT clients.

   Within a block, a UTXO body is an explicit access list. The node therefore needs no
   optimistic-concurrency machinery (Block-STM). The node knows all reads in advance. The
   in-block dependencies form a DAG that the node checks after parallel preparation.
9. **Measure against the incumbent.** Every performance claim has a benchmark in `hayai-bench`.
   The baseline is Zakura's code or Zakura's data layout.

## Crates

```
hayai-crypto      the cryptography backend behind one set of names (upstream or zakura)
hayai-wire        retained-bytes block and transaction model, txids, merkle roots
hayai-consensus   network parameters and one rule set per network upgrade
hayai-sinsemilla  MerkleCRH^Orchard: position-weighted tables, batch-affine lanes
hayai-trees       note-commitment frontiers with batched append (Sapling, Orchard, Ironwood)
hayai-coins       outpoint-keyed coins and nullifier stores with an in-memory cache
hayai-prepared    prepared-transaction store (context-free verification, once)
hayai-state       layered chain state and contextual validation
hayai-validate    bulk block validation pipeline producing a layer
hayai-relay       compact block relay protocol (docs/protocol-compact-relay.md)
hayai-template    live block template (docs/protocol-template-push.md)
hayai-blockstore  flat append-only block files with a height index
hayai-index       wallet index: transactions by id, transparent addresses, note commitment subtrees
hayai-sync        header chain with forks, block download scheduler, peer misbehaviour score
hayai-net         legacy Zcash P2P codec and handshake, compact-relay negotiation, both-paths relay, address book, peer manager
hayai-rpc         getblocktemplate/submitblock shim over the live template
hayai-bench       benchmarks against zakura-* crates and Zakura's data layouts
```

The dependency direction is top to bottom within this list. No crate depends on a crate below
it in the list, except through the traits named in its section.

### Crypto backends

`hayai-crypto` re-exports every protocol and curve crate that the workspace uses (`orchard`,
`sapling_crypto`, `zcash_primitives`, `zcash_protocol`, `zcash_transparent`, `zcash_proofs`,
`pasta_curves`, `halo2_proofs`, `ff`, `group`, `equihash`, `sinsemilla`, `jubjub`, and the
backend-independent `zcash_encoding`, `zcash_script`, `subtle`, `incrementalmerkletree`,
`zcash_history` and its `primitive_types`). Its `upstream` feature (default) points them at the
upstream crates. Its `zakura` feature points them at the `zakura-*` forks (`=2.2.0`). The forks
keep the upstream module paths and build on ff/group 0.14 and rand 0.10.

The 2 features are mutually exclusive. Exactly one feature must be on. No other crate names a
crypto crate in its manifest. A module writes `use hayai_crypto::orchard;`, and the rest of
the file is backend-neutral.

Every crate carries `upstream`/`zakura` features that forward to its hayai dependencies, with
`default-features = false` on all inter-crate edges. Therefore `cargo test -p <crate>
--no-default-features --features zakura` builds any crate alone on the forks.

One API difference reaches hayai code: the RNG line. Upstream APIs take rand_core 0.6
generators. Zakura APIs take rand_core 0.10 generators. `hayai_crypto::rng` names the generator
types and traits that the APIs of a backend accept (`StdRng`, `SeedableRng`, `RngCore`,
`os_rng()`, `seeded()`). Randomness that never reaches a backend API uses the workspace `rand`
directly. `hayai_crypto::BACKEND_SUFFIX` (`""` or `"-zk"`) tags the benchmark ids.

## Data flow

```
 peers ──▶ hayai-net (legacy `tx`/`block` or compact-relay frames inside `zcmpct`)
                 │ one TxSink / one IncomingBlock path, header check, forward on both paths
                 ▼
 tx gossip ──▶ hayai-prepared::prepare(bytes, &dyn CoinsView)
                 │ parse once (hayai-wire), spent coins, sighashes, scripts,
                 │ shielded batches, fee, sigops → Arc<PreparedTx> keyed by wtxid
                 ▼
          ┌── PreparedStore ──┐
          │                   │
 compact  │ lookup by short id│ feerate order
 block ───┼──▶ reconstruct ───┼──▶ hayai-template (live, pushed)
          │   (hayai-relay)   │
          ▼                   │
 hayai-validate::build_layer(raw, &store, &ChainView)        (the view may hold speculative layers)
   1. header, txids, merkle root, auth root     (parallel over txs)
   2. split known / unknown transactions
   3. one fetch of every input                  (speculative layers, window index, then hayai-coins multi_get)
   4. unknown: drafts in bulk
   5. contextual checks + tree appends + header commitment + history append   (hayai-state, hayai-trees)
   → (Layer, Verification)
          │
          ├──▶ hayai-state::Chain::push_speculative(layer) → SpecId
          │        └──▶ speculative tip event → hayai-template on_speculative_tip: Empty, then Full
          │                 └──▶ hayai-rpc TemplateFeed (getblocktemplate long polls wake; submitblock rebuilds)
          │
          └──▶ hayai-validate::verify(verification), concurrently:
                 scripts (flat array) | shielded batch (one task per group, bisect on failure)
                 ├── ok   → Chain::confirm(id): commit = index push + Arc push, oldest first
                 │          → hayai-template on_confirm; hayai-prepared remove_mined
                 └── fail → Chain::reject(id): the layer and its speculative descendants go
                            → hayai-template on_revert: TemplateRevert on the parent

 committed chain: reorg = pop; finalize = merge oldest into coins cache
          ├──▶ hayai-net forwards (already done after step 1 for received blocks)
          └──▶ hayai-blockstore appends wire bytes; hayai-coins flushes in batches
```

`validate_block` is `build_layer` and `verify` in one call. Callers that commit with
`Chain::push` use it (restart replay, tests, the zero-layer benchmarks).

## Speculative tip

A template on block B needs the layer of B, not only the header of B. The template header
commits to the ZIP 221 history tree after B. The leaf of B holds the final Sapling, Orchard and
Ironwood roots of B. The layer build is therefore the earliest point for any template on B. The
scripts and proofs of the unknown transactions of B are not inputs of the layer, so they are not
on the path to the template:

- `build_layer` returns the layer and a `Verification` (the drafts and the shielded batch).
  The node pushes the layer with `Chain::push_speculative` and moves the template with
  `LiveTemplate::on_speculative_tip`. `verify` runs at the same time on the pool.
- The validator of the next block takes `Chain::view_speculative`, so the node can build and
  verify a chain of blocks in a pipeline.
- `Chain::confirm(id)` marks a layer verified and commits every verified layer from the
  bottom of the speculative stack. A block that verifies before its parent waits for it.
- `Chain::reject(id)` removes the layer and every speculative layer above it. The template
  returns to the parent with `on_revert`. `on_revert` adds back the candidates that the
  removed tip events removed, except the candidates that the store removed since then.
- Finalization and flushes see only committed layers. `Chain::push` (a direct commit) fails
  while speculative layers exist. `Chain::pop` removes them, because they extend the popped
  block.

## Prebuilt bodies

A node knows some blocks before they exist: its own template, and the candidates that peers
publish in their lanes (`docs/protocol-compact-relay.md`, Candidates). Everything that the
contextual check of a block does with its body after the coinbase depends on the parent only:

- `hayai_state::prebuild_body` runs that work on the view of the parent.
  `hayai_validate::prebuild` does the same with the body from the prepared store, verified
  under the epoch of the next block. The work is:
  - the outputs, one input round, the nullifiers, the anchors;
  - every rule that needs only the height;
  - the totals, the value pools, the tree appends;
  - the branches of position 0 in the merkle tree and the auth data tree.

  The function records a time lock that needs the block time, and does not judge it.
- `PrebuiltBody::commit` (`hayai_validate::commit_prebuilt`) commits a block whose body is
  exactly the prebuilt one, on the same parent. Its work is:
  - the merkle root and the auth data root from the coinbase and the branches;
  - the coinbase rules (placement, height, expiry, finality, value, sigops, txid uniqueness);
  - the recorded time locks;
  - the header commitment and the history append.

  The layer takes the prebuilt maps and trees and adds the coinbase outputs: a pointer swap.
  The result is `Mismatch` for another body, another parent, or a shielded coinbase (its note
  commitments come before the note commitments of the body). The block then goes through
  `validate_block`.
- `prebuild_body` and `contextual_check_with_outputs` share every rule (`check_txs`,
  `check_coinbase`, `check_history`, ...). The bench test `tests/prebuilt.rs` checks that a
  swap commit gives the layer and the window index state of full validation.
- hayaid prebuilds the body of the newest template (`mining.prebuild_own`, on by default). It
  also prebuilds up to `network.prebuilt_candidates` candidates of the lanes of peers on the tip
  (off by default). It does this work while the driver is idle, at most once per 200 ms. A
  commit removes every prebuilt body.

## History tree

`hayai_state::history` keeps the ZIP 221 chain history tree (a Merkle mountain range over the
blocks of one network upgrade) with the upstream `zcash_history` crate. The node of Zakura also
uses this crate.

- `HistoryState` is the tree after a block: the upgrade, the node count of the MMR, and the
  peaks (each a ZIP 221 serialized node). The peaks are sufficient to append a leaf and to
  compute `hashChainHistoryRoot`. Each `Layer` and the `Base` hold one, or `None` when the
  node does not know it.
- A leaf holds these values:
  - the block hash, time, `nBits`, the work of `nBits`, the height, the final Sapling root and
    the count of transactions with Sapling spends or outputs;
  - from NU5 (tree version 2), also the final Orchard root and the count of transactions with
    Orchard actions;
  - from NU6.3 (tree version 3), also the final Ironwood root and the count of transactions
    with Ironwood actions.

  The first block of an upgrade starts a new tree.
- The contextual check applies the header rule with the state of the parent. The rule for each
  upgrade is:
  - Sapling, Blossom: the final Sapling root;
  - Heartwood, Canopy: the tree root of the parent (all zeros in the Heartwood activation
    block);
  - from NU5: `BLAKE2b("ZcashBlockCommit", root || auth_data_root || [0; 32])`.

  An activation block commits to the whole tree of the previous upgrade.
- Seed of the tree: before Heartwood the tree is empty, and every layer knows it. A base at or
  after Heartwood starts unknown. The node seeds `Base::history` with
  `HistoryState::from_peaks`, because the node cannot derive the peaks from headers.
- The rule set of an upgrade names the tree version (`RuleSet::history`). From NU6.3 the tree
  has version 3. An upgrade without a rule set returns `HistoryError::Unsupported`.
- `Layer::history_root()` is the `history_root` of a template `Tip` on that layer.

## hayai-wire

- `RawBlock { bytes: Bytes, header: BlockHeader, txs: Vec<RawTx> }` where
  `RawTx { bytes: Bytes (slice of the block), tx: Arc<Transaction>, txid, auth_digest }`.
- A boundary scanner (`tx_wire_len`) delimits every transaction with length arithmetic only.
  The parser then parses the slices in parallel with
  `zcash_primitives::transaction::Transaction::read`. The crate keeps the slice and the parsed
  form. The crate does not serialize anything again. The crate hashes the ZIP 244 authorizing
  digest directly from the scanned byte ranges.
- `merkle_root(&[txid])` and `auth_data_root(&[auth_digest])` compute the roots with parallel
  sha256d / BLAKE2b. `merkle_branch_first` and `auth_branch_first` are the branches of
  position 0, so a body keeps its share of both roots when only the coinbase changes.
- `order::canonical_order`: the canonical block order (depth, then txid) from the txids and
  the transparent inputs of a set.
- The crate parses and hashes headers. The solution keeps its length. The parser accepts the
  lengths of the known Equihash parameter sets (`PowParams::MAINNET` (200, 9), 1344 bytes;
  `PowParams::REGTEST` (48, 5), 36 bytes), and `serialize` writes the parsed bytes back. The
  hash is over the exact serialized bytes. `check_equihash(header, params)` verifies with the
  upstream `equihash` crate and the parameters that the caller takes from the network.

## hayai-consensus

- `Network { Mainnet, Testnet, Regtest }` and `NetworkParams` (`Network::params()`): genesis
  hash and time, Equihash parameters (`hayai_wire::PowParams`), proof-of-work limit (256-bit
  value and compact form), slow start interval, halving interval. The message start (magic)
  stays in hayai-net.
- Constants: `COINBASE_MATURITY` (100), `FINALITY_DEPTH` (1,000 blocks),
  `TX_EXPIRY_HEIGHT_THRESHOLD`, `LOCKTIME_THRESHOLD`, target spacing before and after Blossom
  (150 s, 75 s), `MEDIAN_TIME_SPAN` (11), `DIFFICULTY_CONTEXT_BLOCKS` (28).
- `Checkpoints` and `Network::checkpoints()`: the checkpoint list of a network, with
  `hash_at(height)`, `last_height()` and `last_at_or_below(height)`. The Mainnet and Testnet
  lists are Zakura's files (`src/checkpoints/*.txt`; source and revision in
  `docs/consensus.md`, section Checkpoints). `build.rs` converts each file to 36 bytes
  for each checkpoint (880 kB in the binary for both lists). The first use of a list decodes
  it. `Checkpoints::new` makes a list for a chain of generated blocks.
  `Network::mandatory_checkpoint_height()` is the last height before Canopy.
- `Upgrade` names every network upgrade from `Sprout` to `Nu7`.
  `Network::activation_height(upgrade)` reads Mainnet and Testnet heights from the
  `zcash_protocol` of the backend (`MAIN_NETWORK`, `TEST_NETWORK`), except the NU7 height.
  Regtest activates Overwinter to NU5 at height 1.
- `Network::ConfiguredRegtest(&RegtestConfig)` is Regtest with its own activation heights for
  NU6 to NU6.3, its own checkpoint list and its own mandatory checkpoint height (`[regtest]` of
  hayaid). Every other value is the value of Regtest, and `Network::is_regtest()` is true for
  both. `Network::upgrade_at(height)` and `Network::next_upgrade(height)` derive from it.
- `RuleSet` holds the rules of one upgrade: branch id, allowed transaction versions
  (`TxVersions`), script flags, shielded pools (`ShieldedPools`), block limits
  (`BlockLimits`), history tree version (`HistoryVersion`), coinbase rules (`CoinbaseRules`)
  and the difficulty parameters (`DifficultyParams`). The rule sets are one table. A new
  upgrade is one more entry.
- `rules_at(network, height) -> Result<&'static RuleSet, ConsensusError>` is the one
  interface that selects a rule set. When the upgrade that is active at `height` has no rule
  set, it returns `ConsensusError::UnsupportedUpgrade { upgrade, height }`. It never returns
  the rule set of an earlier upgrade for such a height.
- `difficulty::expected_bits(network, time, &ParentChain) -> Result<u32, DifficultyError>`
  is the `nBits` that a block must have (specification §7.7.3 and the Testnet
  minimum-difficulty rule). `ParentChain { height, times, bits }` is the context: the times
  of the 28 blocks before the header and the `bits` of the 17 blocks before it, newest
  first. `difficulty::block_work(bits)` is the work of a block.
- `header::check_header(network, header, &ParentChain, now)` holds every header rule. Its
  parts are `check_contextual`, `check_local_time` and `check_proof_of_work`. `check_version`
  is the version rule, for a caller that has no context (the header chain). The result
  `HeaderVerdict::ContextTooShort` names the rules that did not run because the context
  holds fewer blocks than they read (`docs/consensus.md`, Header rule paths). `NetworkParams`
  holds the values that the rules read: `disable_pow` (Regtest), `min_difficulty_start_height`
  (Testnet 299,188), `max_time_start_height` (Mainnet 2, Testnet 653,606, Regtest 2).
- NU7 has a rule set when the crypto backend has the NU7 branch id
  (`hayai_crypto::nu7_branch()`, the only `cfg` for it). The `zakura` backend has it. The
  upstream backend does not have it. The NU7 activation height is a constant of hayai-consensus
  on every backend: Testnet 4,465,026 (Zakura `zakura-chain/src/parameters/constants.rs:80`),
  no height on Mainnet, and the configured height on Regtest.
- hayaid takes the branch, the epoch and the validation configuration of every height through
  `rules_at`. A node without the rule set therefore stops with the error before it prepares or
  validates anything at that height. The schedule functions (`subsidy::halving`,
  `subsidy::scheduled_issuance`, `funding::funding_streams`) do not read the rule set and give
  the NU7 values on each backend. `nsm` holds the NSM of NU7. The NSM value balance is a
  function of the height and of the total of the chain value pools, so the state stores no
  value for it.
- `subsidy::block_subsidy(network, height)` is the block subsidy schedule of the 3 networks.
  `founders`, `funding` and `lockbox` hold the founders' reward, the funding streams and the
  lockbox disbursement. `coinbase::CoinbaseTerms::at(network, height)` collects what the
  coinbase of a height must pay. `check` applies the output rules and the value rule (ZIP 236
  from NU6). `deferred_pool_after` gives the deferred pool after the block. hayai-state calls
  them in the contextual check, and hayai-template builds its coinbase from the same terms
  (`CoinbaseSpec { network, .. }`).
- `rules_at` also selects a rule set that depends on the height inside one upgrade. From the
  Orchard soft fork (Mainnet 3,363,426, Testnet 4,048,500) until the NU6.2 activation, it gives
  the NU6.1 rule set with the Orchard pool off. hayai-prepared takes the rule set from the
  branch id, so the contextual check of hayai-state applies the pools of the rule set of the
  height.
- Users: hayai-prepared (`RuleEpoch::of(&RuleSet)`, expiry threshold), hayai-state
  (`CheckConfig { network, rules }`, history tree version, `LAYER_WINDOW`), hayai-validate
  (`ValidateConfig { network, rules, keys, header }`), hayaid (`params.rs` is a thin
  wrapper).

## hayai-sinsemilla

- `MerkleCrhOrchard` with a position-weighted table: for word position `i` of the 52-word
  MerkleCRH input and 10-bit value `j`, the table holds `[2^(51-i)] S(j)`. A hash is 52 affine
  additions and no doublings.
- The function finds the exceptional cases of incomplete addition (equal x-coordinates) and
  handles them with a complete fallback. The function therefore equals the specification on
  every input. It does not rely on a discrete-log argument.
- `combine_many(layer, pairs: &[(Fp, Fp)]) -> Vec<Fp>` runs lanes in lockstep with one batched
  inversion per addition column (Montgomery's trick). A single pair uses the scalar path.
- Tests: equality with `orchard::tree::MerkleHashOrchard::combine` on random and edge inputs.

## hayai-trees

- `OrchardFrontier` and `SaplingFrontier` wrap `incrementalmerkletree::Frontier<_, 32>`.
- `append_many(&mut self, leaves: &[Leaf]) -> Root` splits the leaves into aligned perfect
  subtrees, hashes each level of each subtree as one lane batch, and merges the carries. It
  returns the new root with about N + 32 hashes. The result is byte-identical to sequential
  appends (tested against upstream `Frontier::append` loops).
- The crate computes roots and anchors per block. There is no lazy mode, because the root of
  every block is an anchor and a history-tree input.

## hayai-coins

- `OutPoint → Coin { value, script_pubkey, height, is_coinbase }`, keyed by the 36-byte outpoint.
  One read per input.
- `CoinsCache`: an in-memory map with `fresh` (created since the last flush) and `dirty` flags.
  `spend` of a fresh coin removes it in memory, and the coin never gets to the disk. The model
  is Bitcoin Core's `CCoinsViewCache`.
- A flush has 3 phases:
  1. `begin_flush` takes the dirty index as the generation in flight and returns copies of its
     coins and tombstones. The entries stay in the map and stay readable. `begin_flush` marks
     them not fresh, so a later spend becomes a tombstone for the next flush.
  2. `CoinsBacking::write_generation` writes the generation outside every lock as one RocksDB
     `WriteBatch`, together with the pending nullifiers and a best-block record.
  3. `end_flush` marks the entries of the generation clean and removes its tombstones. An entry
     that the writer changed during the write is in the new dirty index and stays.

  `flush` runs the 3 phases in a row.
- `fetch_many(&[OutPoint])`: cache hits first, then one `multi_get` to the backing store.
- `NullifierStore`: per-pool sets with `contains_many` by `multi_get` and batched insert. A
  query gets the answer from the pending set, then from the generation in flight, then from
  the backing.
- `CoinsBacking` is the trait of the backing store. Its RocksDB implementation has the `coins`,
  `nf_*` and `meta` column families, no compression, Ribbon filters, pinned filter and index
  blocks, and a block cache sized from configuration. The default block cache is 256 MiB: it
  holds the whole benchmark store of 2 M coins. A 32 MiB cache made the 13,000-input lookup 6
  times slower.
- Recovery rule. `RocksBacking::best_block()` returns the `{height, hash}` record of the last
  generation written. The batch is atomic, so the coins and nullifiers on disk are the state
  after exactly that block, whatever phase a crash interrupted. A restart replays the blocks
  after that height from the block store (contextual rules and layer build, no proofs) and
  builds the layer window again from the newest ones.
- Hashing: `ahash` with random keys. Txids and nullifiers are attacker-grindable, so the crate
  does not use an identity hasher.
- `MemBacking`: a second `CoinsBacking`. It keeps the whole coin set and the nullifier sets in
  memory and writes only sequential files.
  - Coins: 256 shards by the first txid byte. A shard holds a dense vector of 69-byte entries
    (outpoint key, value, height, tag, 20-byte script hash) and a `hashbrown::HashTable<u32>`
    of positions. The backing builds P2PKH and P2SH scripts again from the tag and the hash.
    Other scripts stay whole in a side map. Cost: 69 bytes per coin plus 5 bytes per index
    bucket.
  - Nullifiers: 256 shards per pool. A shard holds one sorted run (32 bytes per nullifier) and
    a small hash set of new nullifiers. The set merges into the run when it holds more than an
    eighth of the run.
  - A batched lookup locks each shard once and copies the entries out. The lookup builds the
    coins again outside the lock.
  - `coins.log`: one record per write (`write_generation`, `write_batch`, `insert_many`). A
    record has a 20-byte header (magic, payload length, payload CRC32C, header CRC32C), a
    sequence number, the optional best block, the adds, the spends and the nullifiers per
    pool. `MemConfig::fsync_every_generations` (default 1) sets how often the backing syncs
    the log.
  - `coins.snapshot`: the whole set at one sequence number, in 1,280 sections that each have
    a CRC32C (256 coin shards, 4 × 256 nullifier shards). `snapshot()` writes it to
    `coins.snapshot.tmp`, syncs it, renames it into place and truncates the log. Writes wait
    for the snapshot. Lookups do not wait.
- Recovery rule of `MemBacking::open`:
  1. The function loads the snapshot.
  2. It replays the log records with a sequence number after the sequence number of the
     snapshot.
  3. It cuts a torn last record and reports it in `Recovery`. A torn record is a short
     header, a zero-filled tail, a payload that passes the end of the file, or a last record
     that fails its payload CRC.
  4. Any other damage, a sequence gap or a damaged snapshot is an error.
  5. `best_block()` then gives the block to replay from, as for `RocksBacking`.
- Mainnet sizing of `MemBacking` (2026-10-03, height 3,505,115).
  - Inputs:
    - Transparent coins: 27,322,296 (all outputs minus all non-coinbase inputs; Blockchair
      API `zcash/transactions` aggregate). zcashd leaves out OP_RETURN outputs, which gives
      about 27.23 M.
    - Orchard nullifiers: 50,472,352 (the Orchard tree size, one leaf and one nullifier per
      action; Blockchair `raw/block/3505115`).
    - Ironwood nullifiers: 669,333 (the Ironwood tree size, same source).
    - Sapling nullifiers: 3,068,534. Sprout nullifiers: 1,663,236. Both are RocksDB key
      estimates from a Zebra node (ZcashFoundation/zebra PR #8895, 2024-11-29), probably
      too high. No source publishes an exact count.
    - Zakura's docs and CHANGELOG state none of these figures.
  - Memory: 75.1 bytes per coin at mainnet size (69 + 6.1 index bytes; the 2 M-coin
    benchmark measures 79.7, the model gives 79.5) → 2.05 GB. 32 bytes per nullifier ×
    55.9 M → 1.79 GB. Total 3.84 GB (3.58 GiB) after a load. New nullifiers that have not
    merged yet add up to about 4 bytes per nullifier.
  - Snapshot: 69 bytes per P2PKH coin and 32 bytes per nullifier → 3.67 GB.
  - Time, extrapolated from 2,002,000 coins (138 MB) on a Ryzen 9 9950X with other load:
    write and sync 104–301 ms → 2.8–8.0 s; load from the page cache 49–63 ms for coins and
    31 ms for 2 M nullifiers → 1.5–1.7 s, plus the disk read of 3.67 GB when the file is not
    cached.

## hayai-prepared

- `PreparedTx`:
  - `raw: RawTx` (wire bytes, parsed form, txid, auth digest, `WtxId = (txid, auth_digest)`),
  - `epoch: RuleEpoch`,
  - `spent: Vec<Coin>` in input order (contents only; existence is contextual),
  - `fee`, `sigops`, ZIP 317 `conventional_fee` and `weight_ratio`,
  - `nullifiers` per pool, `commitments` per pool, `anchors` per pool,
  - `scripts_ok: bool`, `shielded_ok: bool`, `sighash_cache`.
- `prepare(raw: RawTx, epoch, coins: &dyn CoinsView) -> Result<PreparedTx, PrepareError>` does
  all context-free work. Script verification runs through `zcash_script`. Shielded bundles go
  through `ShieldedBatcher`: block-scoped or mempool-scoped batches over the upstream
  `orchard::bundle::BatchValidator` and `sapling_crypto::bundle::BatchValidator`. The batcher
  bisects a failing batch until it isolates the failing items.
- `PreparedStore`: a concurrent map `WtxId → Arc<PreparedTx>` plus the index
  `OutPoint → WtxId` of spent outpoints (conflict detection) and a feerate-ordered view for the
  template. Each entry holds its ZIP 317 values (conventional fee, unpaid actions, weight
  ratio) and its ZIP 401 values (cost, eviction weight), computed once at insert.
- The store evicts by ZIP 401: a cost limit, a weighted random selection with an injected
  random number generator, and a list of recently evicted txids. `remove_expired(next_height)`
  removes the transactions that the next block cannot contain (ZIP 203). The store removes
  entries whose epoch differs from the current one. Each removal emits `SetEvent::Removed`.
- `MempoolPolicy::admit(&PreparedTx, &PolicyContext) -> Result<(), PolicyReject>` applies
  the relay rules to a valid transaction: ZIP 317 unpaid actions, the minimum relay fee,
  zcashd standardness, expiry, lock time and coinbase maturity at the next block
  (`docs/mempool-policy.md`). `PolicyContext` holds the next block height, the
  median-time-past and the rule set of the next block.
- Modules: `shielded.rs` holds the batch, `orchard.rs` and `sapling.rs` hold the keys and the
  batch verification of each pool.
- `VerifyingKeys`: one Orchard verifying key per circuit version and one Sapling key pair.
  - The Sapling keys are in the binary (`SaplingKeys::embedded`, `sapling_vk/`).
  - `VerifyingKeys::prebuild(epoch, next)` builds the key of the active epoch, and of the next
    one (`hayai_consensus::Network::next_upgrade(height)`), on a thread of its own at
    construction (0.6 s on 32 threads and 1.7 s on 1 thread, release build of upstream
    `orchard`).
  - `prebuild_more(branches)` starts the same build later, for the keys that no call asked
    for before. The node calls it after each commit, so the build of the key of an upgrade
    starts at the activation before it. The key sets of one process share each built key.
  - `ready()` waits for the builds, on a thread that is not a rayon worker. Nothing else builds
    a key. The keygen runs on the rayon pool, and a lazy build on a pool worker deadlocked
    parallel validations.
  - `ScopedBatch::add` rejects a bundle whose key is not built with
    `PrepareError::Unsupported`. `ScopedBatch::finalize` verifies each bundle group (one per
    Orchard circuit version, one for Sapling) as a concurrent task.
- Ironwood (NU6.3): a v6 transaction has an Orchard slot and an Ironwood slot, and both
  hold a bundle of the Orchard protocol. `draft` reads both with one code path
  (`Pool::Orchard`, `Pool::Ironwood`). The batch puts an Ironwood bundle in the group of
  the NU6.3 circuit, with the Orchard bundles of NU6.3. `draft` takes the allowed
  transaction versions, the pools and the coinbase rules from the rule set of the epoch
  (`RuleSet::of_branch`).

## hayai-state

- `Layer` as in Terminology, plus `height`, `hash`, `parent`, `time`, `bits`, chain value pools
  and the history tree after the block (section History tree).
  - The base keeps the times and the `bits` of its newest 28 blocks.
    `ChainView::difficulty_context()` returns `(time, bits)` of the newest blocks, newest
    first: the input of the difficulty rule.
  - `Layer`, `Base`, `Anchors` and `ValuePools` hold the Ironwood frontier, anchor and value
    pool, and the transparent, Sprout and deferred pools. The Ironwood pool (NU6.3) has the
    rules of the Orchard pool with its own state: nullifier set (`Pool::Ironwood`), tree (the
    Orchard node type and hash), anchor set and value pool. Before NU6.3 the Ironwood tree is
    empty, and its root is in the anchor set of every base.
  - `ChainView::frontiers()` returns the frontiers and their roots in one read.
  - The Sprout state is the Sprout frontier of each layer and of the base, the final Sprout
    treestate of every block by root in the base (`ChainView::sprout_tree`: a JoinSplit
    continues the tree of its anchor), the nullifier set `Pool::Sprout` and the Sprout pool. A
    base that starts above the genesis block without the treestates does not know the Sprout
    state (`Base::set_sprout_unknown`).
- `Chain { base: Arc<RwLock<Base>>, layers: VecDeque<Arc<Layer>>, index }` where `Base` is
  the coins cache, the nullifier store and the finalized tip trees. `ChainView` is a cheap
  clone (an Arc of the layer list, the shared base and the shared index) that the chain gives
  to validators and the template.
- The window index (`window.rs`) holds `OutPoint → (height, Created(coin) | Spent)` and one
  `nullifier → height` map per pool over the whole window.
  - A layer contributes `Spent` for each outpoint it spends, and `Created` for each output it
    creates and does not spend. The coin index keeps the contribution of the newest layer. The
    nullifier index keeps the oldest layer that reveals the nullifier.
  - `push` inserts the contributions of the layer. `pop` removes the keys of the layer and
    derives them again from the remaining layers. `finalize_excess` merges the layer into the
    base, then removes the entries still at that height.
  - A lookup is one probe in the index, then one base round for the misses. The index carries
    the tip it describes: the committed tip. A view first walks its layers above that tip
    (speculative layers, or layers that a pop removed after the chain made the view), then
    probes the index.
  - A view that does not hold the tip of the index (a view taken before a pop and a push)
    walks all its layers newest first (`get_coins_by_walk`,
    `contains_nullifier_many_by_walk`). The tests also use that walk as the reference for the
    index, with random push, pop, finalize, speculative push, confirm and reject histories.
  - Commits push. Reorgs pop. Finalization merges the oldest layer into the base when the
    layer count exceeds the window (`LAYER_WINDOW`: 1,000 blocks, the finality depth of
    hayai-consensus). A writer blocks readers only for the time of a map scan.
- `Chain::begin_flush` and `Chain::end_flush` are the 2 locked phases of a flush
  (hayai-coins section). `Chain::flush` runs them around one `write_generation` on the
  backing.
- Speculative layers (`push_speculative`, `confirm`, `reject`, `view_speculative`) sit above
  the committed layers and never enter the index (section Speculative tip).
- `contextual_check(view, &PreparedBlock) -> Result<Checked, ContextError>` implements these
  rules:
  - inputs exist and are unspent in the view;
  - coinbase maturity;
  - no duplicate nullifier in the view;
  - anchors present in the view;
  - expiry height, lock time;
  - the pools of the height;
  - the coinbase terms (required outputs and value rule);
  - the 6 chain value pools;
  - the header commitment to the history tree of the parent.

  It appends the block to the history tree. `docs/consensus.md` lists every rule with
  its status.
- `block_outputs(raw, height)` is the set of the coins of the block, keyed by outpoint.
  `resolve_inputs(view, raw, &created)` is the coin of every transparent input of the block,
  from that map or from one view round. The validator builds both once, prepares the unknown
  transactions from them, and then gives them to `contextual_check_with_outputs`. There the
  map becomes the `created` map of the layer.

## hayai-validate

- `build_layer(raw, &store, &view, &cfg) -> Result<(Layer, Verification, Timings), BlockError>`
  and `verify(Verification) -> Result<VerifyTimings, BlockError>`: the 2 halves of a
  validation (section Speculative tip). `validate_block(raw, &store, &view, &cfg) ->
  Result<(Layer, Timings), BlockError>` runs both.
- The stage order is as in Data flow. Stage 3 does one `get_coins` for every input of the
  block that the block does not create, for known and unknown transactions. The reason is
  that the contextual check compares each prepared coin with the coin of the view. Stage 5
  does one `contains_many` for nullifiers and one for anchors, and lays out all unknown inputs
  as one flat array for the pool.
- In `validate_block`, after the drafts exist, the scripts, the shielded batch and the
  contextual check with the tree appends run as 3 concurrent rayon tasks. Each task
  stops at its first broken rule. The block fails with the first error in stage order
  (scripts, shielded, context), not with the error of the task that stops first. The verdict
  therefore does not depend on the schedule. `build_layer` runs the contextual check alone, so
  a contextual error comes before any script runs. `verify` reports scripts before shielded.
- `Timings` records the per-stage durations for the benchmarks and the HTML report. The 3
  concurrent stages overlap, so `total` bounds each of them and not their sum.
- `apply_checkpointed(&raw, expected, &view, &cfg, &checkpoints) -> Result<(Layer, Timings),
  BlockError>` is the checkpoint path: the path of a block that the download scheduler
  delivers with `checkpointed = true`. `expected` is the hash of that delivery. The function
  checks the parent, the height against the last checkpoint, the hash against `expected` and
  against the checkpoint of the height, and the merkle root.
- Then `hayai_state::checkpoint_layer` builds the layer from the parsed transactions, with no
  prepared transaction:
  - it reads the inputs in one round;
  - it collects the spent outpoints, the nullifiers and the note commitments;
  - it computes the value pools;
  - it appends the trees;
  - it applies the header commitment rule with the history append.

  The layer equals the layer of `validate_block` for a valid block. `docs/consensus.md`,
  section Checkpoints, lists what the path checks.
- `validate_block`, `build_layer` and `commit_prebuilt` refuse a block at or below
  `Network::mandatory_checkpoint_height()` (`BlockError::BelowMandatoryCheckpoint`).

## hayai-relay

See `docs/protocol-compact-relay.md`. Library API:

- `CompactBlock::from_block`;
- `reconstruct(&CompactBlock, &dyn TxLookup, &LaneStore, BranchId) -> Result<RawBlock, ReconstructError>`;
- `LanePublisher::publish(parent, seq, ids) -> Publication` (a template change as a batch and
  a candidate);
- `CandidateStore`;
- `CompactBuilder::build_candidate` (the candidate form of a block in canonical order);
- `resolve_candidate(&CandidateBlock, ..) -> CandidatePartial` and
  `CandidatePartial::into_partial` (the set in canonical order, then the gates of any
  `Partial`);
- `BlockTxnRequest`, `BlockTxn`, codec functions.

Header-first forwarding is a policy of the caller. The relay layer exposes
`HeaderCheck::check(&header)` over a caller-supplied `HeaderContext` (parent lookup: the
height, and the times and bits of the blocks before the header). A node can therefore forward a
block after the check passes. `StandardHeaderCheck { context, network }` applies every header
rule of `hayai_consensus::header::check_header` for the network. `CompactBlock::header` holds
the serialized header with its real length (1487 bytes on Mainnet, 177 bytes on Regtest).

## hayai-template

See `docs/protocol-template-push.md`. `LiveTemplate` subscribes to prepared-store events and
chain events. It keeps the candidate set ordered by ZIP 317 weight ratio, and it tracks the
dependencies. The selection walks that order. A template lists the selected set in canonical
order (`hayai_wire::canonical_order`: parents first, then txid), so the block bytes depend
on the set only.

On each event, `LiveTemplate` builds again only the affected part and publishes
`TemplateUpdate` messages. A tip event (`on_tip(tip, mined, conflicting)`) removes the mined
candidates and removes their ids from the dependencies of their children, which stay
selectable. It removes the conflicting candidates with their descendants.

`on_speculative_tip` moves the template to a speculative block. It keeps the candidates that
the event removes and the dependencies that the event releases. `on_confirm` releases them.
`on_revert` restores the parent tip with them and emits `TemplateUpdate::Reverted` (message
`TemplateRevert`). `TemplateConfig::pow` sets the header length in the byte budget and the
solution length of a submission.

## hayai-blockstore

- `blk-NNNNN.dat` append-only files of `[len u32][wire bytes]` records, 128 MiB each, plus an
  index `height → (file, offset, len)` and `hash → height` in RocksDB.
- To serve a block, the store reads the wire bytes from the file. The store does not parse the
  block.

## hayai-index

The wallet index of hayaid (`[state] wallet_index`, off by default) is one RocksDB database
with the transactions by id and by location, the transparent addresses (transactions,
unspent outputs, balance) and the completed note commitment subtrees. `docs/hayaid.md`,
Wallet index, has the column families and the consistency rule.

- Input: each committed block with the coins of its inputs (`Layer::spent_coins`, which the
  validation fills from its one coin read) and the frontiers before the block. The index
  reads no coin and no value before a write: the balances are RocksDB merge operands.
- `IndexWriter`: one thread, a bounded queue (64 blocks), the entries of the waiting blocks
  built in parallel on the rayon pool, one write batch for them. The buffers of the build
  stay for the next batch.
- Each block has an undo record (the address outputs and spends, the subtrees). A reorg and
  a start undo blocks from these records. They do not read the block files.
- Subtrees: a block whose leaves cross a multiple of 2^16 appends its leaves up to the last
  leaf of the subtree to a copy of the frontier before it, and takes the root of level 16.
  Only such a block does this work, also in the checkpoint range.
- Keys have a fixed size. A location is the height (big-endian) and the index, so the keys
  of one address sort in chain order. `addr_tx` and `addr_utxo` have a prefix extractor on
  the 21-byte address. `tx_loc` has a Bloom filter.

## hayai-sync

The crate holds the state machines for synchronization. The crate has no sockets and no
threads of its own.

### Header chain (`headers`, `locator`, `store`)

- `HeaderChain` holds the headers as a tree with the genesis block as the root. An entry has
  the hash, the parent, the height, the time, the bits, the cumulative work, the offset of
  the header in the header log and a `Status` (`HeaderValid`, `BodyKnown`, `BodyValid`,
  `Invalid`). An entry has 96 bytes. The hash index (`hashbrown::HashTable` of positions) and
  the best chain (position for each height) add about 11 bytes for each entry. The measured
  size is 107 bytes for each entry in use, and 138 bytes with the spare capacity of the vectors
  at 100,001 entries.
- `accept_headers(headers, rules, now)` adds a batch in order. Each new header passes these
  checks:
  - the version and `hayai_consensus::header::check_proof_of_work` (solution length, target
    limit, hash, Equihash; Regtest has the waiver), on the rayon pool for the batch;
  - the parent is in the chain and is not invalid;
  - the checkpoint of its height;
  - the finality rule;
  - the `HeaderRules` trait with the times and bits of the 28 ancestors on the branch of the
    header.

  The node implements `HeaderRules` with `hayai_consensus::header::check_contextual`
  and `check_local_time`. The batch stops at the first header that fails. The error has the
  position, the reason and the changes of the headers before it. A header without its parent
  is `RejectReason::Unconnected`: the caller sends `getheaders` with `locator()`.
- Best tip: the most cumulative work among the entries that are not `Invalid`. On equal
  work the entry that the chain accepted first stays (Bitcoin Core and zcashd). Zebra and
  Zakura take the larger hash.
- `mark_invalid` sets an entry and its descendants to `Invalid` and selects the best tip
  again. `mark_body_valid` sets an entry and its ancestors to `BodyValid`.
  `BestTipChange { old, new, fork_point }` reports each move of the best tip.
  `is_reorg()` is true when the best chain lost blocks.
- Finalized height: the larger of the best height minus the finality depth (1,000) and the
  height of the last checkpoint at or below the best tip. The chain refuses a header whose
  branch leaves the best chain below the finalized height (`ForkBelowFinalized`), and it
  removes the entries of such branches. A header at a checkpoint height with another hash is
  `CheckpointMismatch`. `ChainConfig::new(network)` takes the list of
  `Network::checkpoints()`. A test sets `ChainConfig::checkpoints` to a list of its own
  chain. The finalized height comes from the header chain, not from the block
  state: it decreases when a block of the best chain becomes `Invalid`.
- Side headers: at most `ChainConfig::max_side_headers` (65,536) entries are not on the
  best chain when a side header arrives. At the bound, the chain removes the side entry with
  the least work that has no child. A new side header with no more work is
  `RejectReason::SideHeaderLimit` before it is in the log.
- `mark_unavailable(hash)` takes a block and its descendants out of the choice of the
  best tip (`EntryInfo::unavailable`), in memory only. `clear_unavailable()` ends each
  mark. The node uses the pair for a chain whose blocks no peer sends.
  `best_chain_ancestor(hash)` gives the block at which a branch leaves the best chain.
- `locator()` gives the hashes of the best chain at `locator_heights(tip)`: the tip and the
  9 blocks before it, then steps that double, then the genesis block (at most 41 hashes).
  `headers_after(locator, stop, max)` answers `getheaders` from the first locator hash on the
  best chain. It reads the full headers from the header log.
- For block download: `best_chain_from(height)`, `next_blocks_to_download(n)` (the first
  blocks of the best chain without a body, in height order), `mark_body_received`.
- Header log (`store`): one append-only file. A record has the frame of the coin log (magic,
  payload length, payload CRC32C, frame CRC32C) and holds one serialized header or the hash
  of an invalid block. A Mainnet header record has 1,508 bytes: about 5.3 GB for 3.5 million
  headers. The log has no `fsync` for each record (`HeaderChain::sync`). The body states
  `BodyKnown` and `BodyValid` are not in the log: the node sets them from its block state.
- `HeaderChain::open` applies the records in order without the proof-of-work and contextual
  checks. It cuts a torn tail and reports it, and returns an error for other damage. It skips
  a header record that the checkpoint list or the finalized height refuses, with the records
  of its descendants (`skipped_records()`). A build with a new checkpoint thus starts on the
  log of the build before it.
- Measured on Regtest (no Equihash): 100,000 headers in batches of 160 in 0.18 s (1.8 µs for
  each header, with the write to the log); a start from that log in 0.05 s.

### Misbehaviour score (`score`)

The score is pure logic: the caller gives the time in seconds. hayai-net depends on hayai-sync
for this type, so that the block download and the relay use the same reasons.

| Reason (`Misbehaviour`) | Points |
|---|---|
| `InvalidHeader`, `InvalidBlock`, `InvalidProof` | 100 |
| `Malformed` | 50: the first one disconnects, the second one before the decay bans |
| `UnconnectedHeaders`, `Unsolicited` | 20 |
| `InvalidTransaction` | 10 |
| `Stall` | 0; 2 stalls disconnect, and never ban |

- `PeerScore::record(reason, now)` returns `Keep`, `Disconnect` (50 points or 2 stalls) or
  `Ban` (100 points; the caller bans for 24 h).
- Decay: 1 point each 60 s, 1 stall each 10 min.
- `ScoreBoard<K>` holds one score for each key, with a bound on the entries. A ban removes
  the entry.

### Block download (`download`)

`Scheduler<P>` decides which peer gets which `getdata` and in which order the validator gets
the bodies. It has no I/O, no clock and no block bodies. The node gives an `Event` and the
time in milliseconds to `handle`, and applies the returned `Action`s in order. `P` is the
peer key of the node.

```
 Event                                   Action
 PeerConnected / PeerDisconnected        Request { peer, hashes }     -> getdata
 BlockReceived { peer, hash, bytes_len } Store { hash }               -> keep the body
 NotFound { peer, hashes }               Deliver { block, checkpointed } -> validator
 BlockCommitted { hash }                 Discard { hash }             -> drop the body
 BlockInvalid { hash }                   Penalize { peer, reason }    -> score
 BestHeaderTipChanged { committed }      Disconnect { peer }
 Tick
```

- Window: the blocks of the best header chain after the committed tip
  (`HeaderChain::best_chain_from`), at most `window_blocks`. A block is missing, requested
  (one active request) or held (the node has the body). A block whose header chain state is
  not `HeaderValid` is held from the start: the node has its body from another source.
- Memory bound: `held bytes + requested blocks x max_block_bytes <= memory_budget_bytes`.
  A request reserves 2 MB because the size is unknown before the body arrives. The scheduler
  admits a request up to the budget minus 2 MB. Only the first block of the window can use
  the last 2 MB, so the block that the validator needs next always has space.
- Order: `Deliver` goes out in height order, at most `validation_lookahead` blocks before
  their `BlockCommitted`. `checkpointed` is true at or below the last checkpoint that the
  best chain reached (`HeaderChain::last_checkpoint_reached`): the node gives such a block
  to `hayai_validate::apply_checkpointed`.
- Peer selection: the missing blocks go out in height order, each to the peer with the
  smallest `(requests in flight + 1) x moving average of the delivery time`. A peer has at
  most `peer_in_flight_blocks` requests and at most `peer_in_flight_bytes` at the mean size
  of the recent blocks. A peer without a delivery has at most 4. The scheduler uses a peer
  that reported a height below the block only when no other peer reported the height.
- Rescue: when the peer of the lowest block that is not held has a measured rate and sends
  nothing for `rescue_timeout_ms` plus the time of one 2 MB block at its rate, all its
  requests move to other peers. The peer gets no penalty. The scheduler uses a body that
  arrives later.
- Stall: when a peer sends nothing for `request_timeout_ms` plus the same allowance and a
  request has no answer, the peer gets `Misbehaviour::Stall` and its requests move. The
  scheduler keeps a `PeerScore` for each peer: the second stall gives `Disconnect`. A body
  of a later request is no progress for an earlier request, so a peer cannot hold one block
  while it sends the others.
- `notfound` moves the request without a penalty. When each connected peer failed for a
  block, the block waits `backoff_ms`, then 2, 4, ... 32 times that. A body that answers no
  request gives `Unsolicited`, and the scheduler does not store it. A body above
  `max_block_bytes` gives `Malformed`.
- `BlockInvalid`: the supplier gets `InvalidBlock`. When the header chain has the block as
  `Invalid`, the window follows the new best chain. When it does not (the body does not
  match the header), the scheduler requests the block again, and the validator gets the
  delivered blocks from that block again.
- Reorg: `BestHeaderTipChanged { committed }` compares the window with the best chain. The
  common blocks keep their bodies and requests. The other blocks leave: `Discard` for each
  stored body, and the requests become released requests whose late bodies give no penalty.
- `PeerForkPoint { peer, height }`: the chain of the peer leaves the best chain after
  `height`, so the peer gets no request above it. A peer without a delivery has the median
  delivery time of the measured peers in the selection. `Scheduler::withheld()` names the
  lowest block that the node does not have when each connected peer that can have it
  failed, or none can have it.
- Start: `Scheduler::new(config, chain, committed)`. The scheduler keeps nothing on disk.

| Default | Value | Reason |
|---|---|---|
| `window_blocks` | 1,024 | Bitcoin Core's window. With small blocks the count is the bound; with 2 MB blocks the budget is the bound. |
| `memory_budget_bytes` | 1 GiB | Below the checkpoint a block costs 1 to 5 ms (`validate/block` warm, `state/contextual_check`), so the validator is faster than a 50 MB/s download and the buffer only puts bodies in order: one request timeout of 8 s at 50 MB/s is 400 MB above a block that does not arrive. Above the checkpoint cold validation is 26 to 135 ms for each full block (11 to 60 MB/s), so the buffer is full at each size and more memory gives no speed. 1 GiB also permits 535 requests in flight. |
| `peer_in_flight_bytes` | 8 MB | 4 full blocks: the connection stays in use during one round trip. |
| `peer_in_flight_blocks` | 64 | Small blocks: 8 peers have 512 requests in flight, and the budget permits 535. |
| `validation_lookahead` | 16 | Parse and preparation of 16 blocks run in parallel with the commit of the first; at most 32 MB. |
| `request_timeout_ms` | 8,000 | Zakura's request timeout. With `min_rate_bytes_per_sec` (256 KiB/s) a peer must send 2 MB in 15.6 s. |
| `rescue_timeout_ms` | 2,000 | Zakura's floor rescue timeout and Bitcoin Core's stall timeout. |
| `backoff_ms` | 1,000 | One `notfound` round trip is 20 to 300 ms; the wait stops a request loop. |

Simulated throughput (`tests/download.rs`, 8 peers, 20 to 300 ms round trip, 1.5 to
12.5 MB/s each, 50 MB/s in sum; run with `--nocapture`):

| Scenario | Result |
|---|---|
| 5,000 blocks of 1.5 to 20 kB | 2.6 s: 1,916 blocks/s, 20 MB/s; 10.8 MB held at most; 512 requests in flight |
| 5,000 blocks of 1.5 to 2 MB, validation 3 ms | 177.7 s: 28 blocks/s, 49.3 MB/s; 202 MB held at most |
| 1,500 full blocks, 1 more peer that stops after 20 answers | 55.0 s: 47.7 MB/s; 2 stalls, then disconnect |
| 5,000 small blocks, 1 more peer that stops after 20 answers | 4.7 s: 1,053 blocks/s |
| 600 full blocks, budget 16 MB, validation 20 ms | 35.3 s: 29.7 MB/s; 14 MB at most |
| 400 full blocks, budget 64 MB, validation 135 ms | 55.0 s: 7 blocks/s (the validator sets the rate); 62 MB at most |

## hayai-net

See `docs/protocol-compact-relay.md`, section Negotiation and legacy coexistence.

- `codec`: Bitcoin framing (magic, 12-byte command, length, SHA-256d checksum) and every
  legacy message that zcashd and Zebra exchange, plus `zcmpctver` and `zcmpct`. The decoder
  bounds every count before it allocates, and it never panics. `headers` accepts only the
  solution length of the network of the stream (`Network::pow`).
- `session`: the `PeerSession` state machine, Handshaking → Established(Legacy) → optionally
  Established(CompactRelay(v)); pings and timeouts.
- `transport`: the `Transport` trait, and `TcpTransport` on `std::net` with one reader thread
  per peer and a bounded outbound queue.
- `relay`: `Relay` owns the peer set. It applies the both-paths policy through the traits
  `TxSink` (prepared store), `BlockSink` (validator), `ChainSource` (headers and stored
  blocks) and `TxLookup` (hayai-wire). `IncomingBlock` is the single entry that every block
  takes.
- `protocol`: the node sends protocol version 170,160 (NU6.3) and the user agent
  `/hayai:<version>/`. `min_peer_version(network, upgrade)` gives the oldest peer version
  for the active upgrade, never below 170,150. `Relay::set_min_peer_version` applies it to
  new handshakes and disconnects the established peers below it.

### Peer management (`addrbook`, `connect`)

```
 DNS seeders ─┐                        ┌─▶ select (responded, never tried, failed; one per /16)
 addr/addrv2 ─┼─▶ AddrBook (4,096) ────┤
 config ──────┘      ▲   │             └─▶ getaddr answer (23 %, seen in the last 3 h)
                     │   ▼
   attempt/success/failure      PeerManager ──▶ Relay::connect      (outbound target)
   ban (24 h) ◀── ScoreBoard ◀── Relay ◀── admit (ban, inbound limit, limit per IP)
```

- `AddrBook` holds at most 4,096 addresses. An entry has the services, the last time seen,
  the last attempt, the last success, the failures since the last success and the IP
  address of the source peer. Its state is never tried, responded or failed. The book takes
  the time as a parameter and has no clock.
- Selection for outbound connections: responded addresses first, then failed addresses
  that responded before, then never tried, then failed without a success, in random order
  inside a class. At most one outbound peer for each network group
  (/16 for IPv4, /32 for IPv6). The manager does not dial an address again before 119 s. Each
  failure doubles the delay, up to 6 h. An address leaves the book after 3 failures when it
  never responded, and after 10 failures in a row otherwise.
- The book does not trust addresses from peers.
  - A timestamp at or below 100,000,000 or more than 10 min after the local time becomes 5
    days old. Each address then loses 2 h (zcashd).
  - The book removes an address that the node cannot dial (private, loopback, port 0).
  - One source peer holds at most 256 entries.
  - One connection adds at most 1 address at the start, 1 more each 10 s, and 1,000 after a
    `getaddr` of this node.
  - A full book replaces the failed entry with the most failures among the entries that never
    responded. When there is no such entry, it replaces the oldest never-tried entry of the
    source peer with the most never-tried entries. It never replaces an entry that responded
    at any time, or an entry from a seeder or the configuration.
- `addrv2` (ZIP 155) decodes to the same entries as `addr`. The book keeps IPv4 and IPv6, and
  removes other network ids. The node sends `addr` only, and it does not send `sendaddrv2`
  (ZIP 155 has no such message).
- `getaddr`: the node asks each new outbound peer. It answers an inbound peer once for each
  connection with a random sample:
  - at most 23 % of the book and 1,000 addresses;
  - addresses that responded to this node in the last 3 h and did not fail since;
  - timestamps rounded down to 30 min.

  An unsolicited `addr` of at most 10 addresses goes on to 2 other peers, for the addresses
  that are new to the book and not older than 10 min.
- The book file (`peers.dat`): magic, version, entries, bans and a SHA-256 of the content.
  A save writes a temporary file, syncs it and renames it. A load of a damaged file is an
  error (`AddrBookError::Damaged`). A missing file is an empty book.
- `PeerManager` holds the book, the scores, the limits (`PeerConfig`), the clock and the
  DNS resolver (`PeerEnv`: the caller injects both). Defaults: 8 outbound peers, 64 inbound
  peers, 1 connection for each IP address. An IPv6 address counts as its /64 for this limit,
  for the scores and for the bans. Regtest has no bound for each IP address or group and
  accepts local addresses.
- `maintain(relay)` is one step:
  - it dials addresses from the book until the outbound target is reached;
  - it asks the DNS seeders (resolved with `ToSocketAddrs`, at most once each 10 min) when the
    book gives too few addresses;
  - it saves the book each 5 min.

  `spawn(relay)` runs the step on a thread.
- The relay asks `PeerManager::admit` under the lock of the peer set before it adds a peer.
  The relay closes these connections before the `version` message: a banned IP address, an
  inbound peer above the limit, and a connection above the limit for each IP address.
- Misbehaviour: the relay records a reason from `hayai_sync::score` for the IP address of
  the peer. These faults have 100 points:
  - a frame that does not decode;
  - a message before the handshake;
  - a message that the negotiated protocol does not permit;
  - a compact-relay message that breaks the frame rules;
  - a header that fails a context-free rule (version, solution length, proof of work,
    Equihash).
- `Relay::misbehaved(source, reason)` takes the reasons that the node finds outside the relay.
  A compact-relay failure that is not a fault costs nothing:
  - a root mismatch;
  - an unknown batch or candidate;
  - a late `BlockTxn`;
  - a transaction that the local parser refuses;
  - an invalid block from a compact-relay peer (that peer forwards on the proof of work,
    before validation).

  At 50 points the relay disconnects the peer. At 100 points the book bans the IP address for
  24 h and the relay closes every connection with it.
- hayaid in full mode builds the relay with a peer manager (`[network]` keys, the book in
  `peers.dat` in the directory of `[network] cache_dir`). It also dials its configured peers
  and has its own peer limit.

### Block synchronization (`SyncSink`)

The relay does not synchronize. With a `SyncSink` (`RelayDeps::sync`) the node owns the
header sync and the block download:

- the relay has no consensus branch of its own. `ChainSource::tx_branch()` gives the
  branch of the block after the committed tip for a received transaction, and
  `ChainSource::block_branch(parent)` the branch of a block on `parent` for a compact
  block;
- the relay gives the sink `SyncEvent`s: `PeerConnected` (with the height of the `version`
  message), `PeerDisconnected`, `Headers`, `BlockInv`, `Block` (the payload, not parsed)
  and `NotFound`;
- the relay sends no `getdata` for a block. An `inv` with a new block hash, a compact
  block whose parent the header check does not know, and a compact block that the relay
  cannot complete (root mismatch, no announcer left) are `BlockInv` events;
- the node sends through `Relay::send_getheaders` and `Relay::request_blocks`, and gives a
  downloaded block to `Relay::forward_block`. The relay keeps the block, sends it to the
  compact peers and announces it to the legacy peers, and does not call the `BlockSink`.

The reader checks the payload length of a frame against the bound of its command
(`codec::max_body_len(network, command)`) before it reads the payload. The outbound queue
of a connection has a bound in frames and in bytes (`transport::MAX_QUEUED_BYTES`). A
write without progress for `transport::WRITE_TIMEOUT` closes the connection. The `getdata`
items of a peer wait in a list for each peer, and the relay answers them while the
outbound queue is below 4 MB.

Without a sink the relay asks for each announced block itself, as before (shadow mode,
tests). `docs/hayaid.md`, Full mode: synchronization, has the use of the sink.

## hayai-rpc

See `docs/protocol-template-push.md`, section getblocktemplate compatibility. `TemplateFeed`
receives every `TemplateUpdate`. `Rpc` answers `getblocktemplate`, `submitblock`,
`getblockcount` and `getbestblockhash` over JSON-RPC 1.0/2.0. `HttpServer` is a thread-per-
connection HTTP/1.1 front end. The node supplies `BlockSubmitSink` and `TipSource`.

`index` has the methods of the wallet index (`getrawtransaction`, `gettxout`,
`getaddress*`, `z_getsubtreesbyindex`) over `NodeQuery`. `IndexError::Off` is the answer
of a node without the index.

## hayai-bench

- Fixtures: deterministic synthetic blocks (transparent-heavy, Orchard-heavy, mixed). The
  fixtures come from upstream builders and carry real signatures and proofs. The cache is under
  `bench-fixtures/`.
- Baselines: `zakura-chain` (block parsing, `parallel::batch_frontier`, Orchard tree),
  `zakura-orchard` (weighted Sinsemilla, batch validator), and faithful ports of Zakura's data
  layouts where its code is not usable as a library (RocksDB UTXO schema with 2 gets per
  lookup, ZIP 317 selection with per-pick index rebuild, deep-cloned non-finalized maps).
- The benchmarks write the results as JSON under `bench-results/`. `scripts/report.py` renders
  them into `docs/report.html`.
- The scenario bodies are in `hayai_bench::scenarios` (one module per area). The criterion
  benches and the system benchmarks therefore time the same code.
- `benches/structures.rs` measures drop-in replacements for the hashers, maps, sets and small
  lists that hayai uses, on the key shapes and sizes of hayai. It reports wall time and
  allocated bytes per arm. The project adopts a candidate only on these numbers, never on
  advertised ones (`CHANGES.md` records each verdict). The `multitable` arms need the
  `multitable` feature.

### System benchmarks

`scripts/sysbench.sh` (binary `sysbench`) runs every scenario of `hayai_bench::scenarios` for
hayai and for the Zakura baseline. It writes `bench-results/system.json`: the hardware that a
block costs, and not only the time. Each scenario runs on the fixtures that the criterion bench
uses. The scenarios are:

- `parse_block`;
- `validate_block_cold`/`_warm`;
- `coins_lookup_13000`;
- `coins_commit_13000`;
- `state_push_1000_window`;
- `tree_append_2048`;
- `template_build_8000`;
- `relay_reconstruct` (compact reconstruction against the full parse of zakura-chain with ids).

`--threads T` sizes the rayon pool of the children (default: one thread per logical CPU).
`--iterations N` sets the number of recorded steps. The `mimalloc` cargo feature of
`hayai-bench` replaces the base allocator of `sysbench` and of every criterion bench
(`machine.allocator` in the JSON records which one ran).

Each (scenario, parameter, implementation) triple runs in a fresh child process (the binary
runs itself again). The child builds the scenario, runs one warm-up step and then runs
`--iterations` (default 20) recorded steps. In each step, only the scenario body is inside the
measured region. The per-iteration preparation (candidate clones, block generation) is outside
the measured region, as in the criterion benches. In each row, `wall_ms_median` is the median
step. Every other counter is the total over the recorded steps divided by their number.

| field | source |
|---|---|
| `cpu_user_ms`, `cpu_sys_ms`, `minor_faults`, `major_faults`, `ctx_voluntary`, `ctx_involuntary` | `getrusage(RUSAGE_SELF)` deltas around each step, all threads |
| `max_rss_kb` | `ru_maxrss` from the parent's `wait4` on the child: the child's high-water mark, fixtures and warm-up included |
| `alloc_bytes`, `alloc_count`, `peak_heap_bytes` | a counting `#[global_allocator]` around the base allocator, installed in `sysbench` only, with per-thread counter stripes (shared counters would serialise the threads they measure: four shared atomics per allocation turned a 2 ms parallel parse into 7.5 ms); peak is the largest live heap during a step, baseline included, exact to within 64 KiB per thread |
| `cycles`, `instructions`, `cache_refs`, `cache_misses`, `llc_loads`, `llc_misses`, `branch_misses` | one `perf_event_open` counter per event, counting mode, user space only, `inherit` so rayon threads are included, scaled by `time_enabled/time_running` when multiplexed; `null` for events the PMU does not expose (`counters_unavailable` names them, `counters_available` is true only when all seven opened) |
| `io_write_bytes`, `io_read_bytes` | `/proc/self/io` `write_bytes`/`read_bytes` deltas (bytes handed to or fetched from the storage layer) |
| `scratch_bytes` | growth of the scenario's scratch directory over the recorded steps (RocksDB scenarios; 0 for in-memory ones) |
| `blocked_ms` | `wall − user − sys` of the steps: time no thread of the process was on a CPU; negative when several threads ran in parallel, so it reads as blocked time only for single-threaded scenarios |

Caveats:

- Both implementations run on the same allocator in the same binary (glibc malloc through the
  counting wrapper by default, mimalloc with the feature). Allocator-level differences that a
  deployment could make are therefore not part of the comparison.
- The wall-time medians of the millisecond scenarios move by 10–30 % between runs on a loaded
  machine. A decision between 2 variants therefore needs back-to-back runs. 2 JSON files
  from different hours are not sufficient.
- `max_rss_kb` includes the fixtures and the scratch stores that each child loads. The
  comparison is the difference between the 2 rows of a scenario, and not the absolute value.
- The hardware counters are user-space only (`perf_event_paranoid` 2). Kernel work on behalf
  of the process (page faults, RocksDB file I/O) is therefore in `cpu_sys_ms` and not in
  `cycles`.
- The benchmarks do not measure lock contention as such. Rayon has no contention counters, and
  the parking_lot locks that hayai uses are not instrumented. `blocked_ms` and `ctx_voluntary`
  are the substitutes.
- The benchmarks do not need an installed `perf`. They read the counters through the
  `perf_event_open` syscall.

## Out of scope for this phase

- Header and block synchronization in the node (hayai-net relays and serves blocks, and it
  does not sync), the use of the peer manager by hayaid, the RPC surface beyond mining, full
  consensus rule coverage, Sprout, checkpoint sync, snapshots. `docs/consensus.md` lists
  what is implemented.
