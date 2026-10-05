# hayaid

Date: 2026-10-04. Scope: the `hayaid` binary, its two modes, its configuration, its traces
and metrics, and the limits of each mode.

## Modes

| Mode | Network | State | Purpose |
|---|---|---|---|
| `full` | Regtest, Testnet, Mainnet | Own state from the genesis block of the network, synchronized from the peers | Synchronize, produce, validate, relay and serve templates. Phase 2 of `docs/testnet-benchmark-plan.md`. |
| `shadow` | Testnet, Mainnet (Regtest for tests) | Seeded from a local Zakura node at a start height | Validate every block of the Zakura node and record traces and metrics. Phase 1. |

One process holds these parts:

- hayai-net `Relay` over `TcpTransport`.
- The prepared store (`PreparedStore`) as the transaction sink.
- The driver: hayai-validate on hayai-state `Chain`, hayai-blockstore, hayai-template
  `LiveTemplate`.
- hayai-rpc: the JSON-RPC server (full mode) and the `/metrics` server.
- hayai-trace: the JSONL trace writer.

CPU work runs on the global rayon pool. Every other part is a `std` thread with channels,
as in hayai-net. The driver owns the chain state. Every block reaches the driver as one
event, from the relay, from the producer through the relay, from the block download, or
from the shadow follower.

## Full mode: synchronization

A full node needs no upstream node. It reads the chain from its peers.

```
 peers ──headers──▶ HeaderChain (headers.log) ──best chain──▶ Scheduler ──getdata──▶ peers
                         ▲                                        │ Deliver, in height order
 compact relay, own ─────┘ header and body                        ▼
 blocks (BlockSink)                                   driver: build_layer on the speculative tip,
                                                      verify in parallel, confirm, commit
```

- Peers. The peer manager of hayai-net keeps the outbound peers (`outbound_peers`, the
  DNS seeders of the network, the address book in `data_dir/peers.dat`). The node also
  dials the `peers` of the configuration every 10 s while they are not connected.
- Header sync. One peer at a time gives the headers. The node sends `getheaders` with the
  locator of its best header chain, and again after each message of 160 headers. When the
  peer has no more headers, the node asks each other peer one time. A peer that does not
  answer in `header_timeout_ms` is disconnected. An `inv` with an unknown block is followed
  by `getheaders` to its peer, and by at most 5 more each 300 ms while the header is
  missing (a hayaid peer announces a block before it serves the header). Each header passes the proof of work, the contextual header
  rules and the checkpoint list (`hayai_sync::headers`).
- Fork choice. The best chain is the header chain with the most cumulative work that has
  no invalid block. On equal work the first-seen chain stays (Bitcoin Core, zcashd).
- Block download. `hayai_sync::download::Scheduler` is the only sender of `getdata` for a
  block. The relay sends none: an announced block and a compact block that the relay cannot
  complete go to the header sync. The node parses each block on the thread of its peer
  with the rule set of its height, and checks the merkle root.
- Validation. The scheduler delivers the blocks in height order, at most 16 before a
  commit. The driver builds the layer of each delivered block on the speculative tip
  (`build_layer`, `Chain::push_speculative`), verifies the scripts and the proofs of all
  of them in parallel (`verify`), and commits them in order (`Chain::confirm`). A block at
  or below the last checkpoint that the header chain reached takes the checkpoint path
  (`apply_checkpointed`). A block at or below the mandatory checkpoint waits until the
  header chain reaches a checkpoint above it.
- At the tip. A block of the compact relay, or a block of this node, is a body from
  another source: its header goes into the header chain and the scheduler delivers it. A
  downloaded block that is the best header tip goes on to the compact-relay peers before
  its validation. A legacy peer gets the `inv` of a block after its commit, the headers of
  validated blocks only, and `notfound` for a block that the node did not validate yet: zcashd, Zebra and Zakura give the penalty for an invalid block
  to the peer that sent it, and Zakura bans that peer.
- Zebra and Zakura peers. The node answers `getblocks` with the hashes of the validated
  blocks after the locator, or with the hash of its tip when it has none: a Zakura peer
  waits 6 s for an answer and sends no block announcement on that connection in this
  time. One `getdata` message has at most 16 blocks, and fewer for large blocks: these
  peers answer at most 16 blocks and 1 MB for one message. The node sends `mempool` to
  each legacy peer after the handshake and then each 60 s: these peers announce a
  transaction one time, to a part of their peers.
  When the one delivered block is the best header tip, the template moves to it at the
  layer build and goes back when the verification fails.
- A block that fails its validation has one of three faults
  (`crates/hayaid/src/node/fault.rs`):

  | Fault | Cause | Result |
  |---|---|---|
  | Wrong body | The body is not the body that the header hash commits to: a parse error, a merkle root mismatch, a transaction twice with the merkle root of the list without the repeat (CVE-2012-2459), or a header commitment mismatch (from NU5 the commitment binds the authorizing data) | The peer gets 100 points (a ban). The header stays valid, and the scheduler asks another peer. No record in `headers.log` |
  | Invalid | The block breaks a consensus rule, and the header hash commits to the fault | The header chain records the block and its descendants as invalid (a record in `headers.log`), the peer gets 100 points, and the node follows the next best chain |
  | Local | The node cannot validate the block: the Orchard key of the rule set is not built, the node does not know the Sprout state, an upgrade has no rule set, or the driver selected a validation path that the block does not have | The node stops with an error that names the block. No penalty and no record in `headers.log`: the block can be valid |

  The check of the body (no merkle mutation, the authorizing data of the header
  commitment) runs before each commit path: the full path, the prebuilt path and the
  checkpoint path. A downloaded block that is the best header tip goes on to the peers
  after this check.
- Upgrades. Each choice that depends on the height comes from `rules_at(network, height)`
  at the time of its use. The relay parses a transaction under the rule set of the block
  after the committed tip, and a block under the rule set of its height. At a commit that
  makes the next block the first block of an upgrade, the prepared store changes its rule
  epoch and drops each transaction of the old epoch: such a transaction commits to the
  old consensus branch id in its signature hash, so no block of the new epoch can contain
  it and no second preparation can make it valid. The mempool takes transactions of the
  new rule set from that commit on.
- Verifying keys. A background thread builds the Orchard key of the rule set of the next
  block with full validation and the key of the upgrade after it
  (`VerifyingKeys::prebuild_more`). The node asks for the keys at its start and after
  each commit, so the key of an upgrade is in work from the activation before it. The
  driver thread waits for a key (`VerifyingKeys::ready`) before the first batch that
  needs it. The validation on the rayon pool never builds a key. In the checkpoint range
  no block reads a key: the node starts the build 1,000 blocks before the last
  checkpoint.
- Withheld bodies. The node asks a peer for a block only when the chain of the peer can
  have it: a peer whose last `headers` message ends on another branch gets no request
  above the fork point. When each peer that can have the lowest missing block of the best
  header chain failed to send it (a stall, or `notfound`), or no connected peer can have
  it, the node takes the block and its descendants out of the fork choice. The best header
  chain is then the chain with the most work among the other chains, and the node
  downloads its blocks. zcashd has the same result: it activates the chain with the most
  work among the chains whose blocks it has. Zakura keeps the header chain with the most
  work and raises an alarm. The headers stay valid. They come back into the fork choice
  when a peer sends a header of the chain again, when the relay completes a block of it,
  and after 30 s (the time doubles at each exclusion in a row, up to 16 min). The mark is
  in memory only. The template and the blocks of the node are always on the committed
  tip. `hayai_sync_bodies_withheld` is 1 while a chain is out of the fork choice.
- Templates during the synchronization. The node builds no template while its committed
  tip is more than 100 blocks below the best header tip (the distance at which Zakura
  refuses `getblocktemplate`): a block on such a tip is not a block of the chain of the
  network. The driver validates one batch of delivered blocks (at most 16), then takes
  the messages that wait and runs the tick, then validates the next batch. The template
  time is the clock of the node, at least the median-time-past plus 1 s and at most the
  median-time-past plus 90 min from the start height of that rule.
- Mempool and commit. The driver counts each change of the tip and cleans the prepared
  store under the lock of the count. An admission that ran its checks on another tip
  does not insert: it runs again on the new tip, at most 3 times.
- Reorg. When the first delivered block does not extend the committed tip, the driver
  waits until the delivered blocks of the branch end at the best header tip, or have more
  work than the committed tip, or number 16. Then it disconnects the committed blocks down
  to the fork point (at most 1,000, the finality depth) and validates the branch. When a
  block of the branch is not valid, the next best chain can be the first one: its blocks
  come back from the block store. A transaction of the mempool stays when its inputs are
  the same coins and the policy and the tip state accept it on the new tip: its scripts
  and its proofs do not run again. The transactions of the disconnected blocks pass the
  whole admission, the newest 4 MB of them.
- The block store keeps every committed block by hash. The index by height names the
  block of the newest commit at each height.

## How to run

### Regtest pair

```
cargo build --release -p hayaid
scripts/regtest_pair.sh --blocks 10                      # two hayaid nodes
scripts/regtest_pair.sh --zakurad /path/to/zakurad        # scenario a of docs/regtest-pair.md
```

The script writes configurations, logs, traces, `/proc` samples and `report.csv` to
`target/regtest-pair/<time>/`. It stops only the processes that it started.

Manual start of one node:

```
target/release/hayaid config --network regtest > a.toml   # then edit ports and paths
target/release/hayaid start -c a.toml
curl -s -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"generate","params":[10]}' http://127.0.0.1:18345/
```

A second node sets `peers = ["127.0.0.1:18344"]`. A node that starts later synchronizes the
blocks that it does not have.

### Testnet shadow

The same steps run a Mainnet shadow node: use `network = "Mainnet"` in `zakurad`, the
RPC and P2P ports of Mainnet (8232, 8233) and `hayaid config --network mainnet`.
Read the section Mainnet first.

1. Run `zakurad` on Testnet with these settings:
   - `[rpc] listen_addr = "127.0.0.1:18232"` and `enable_cookie_auth = false`;
   - `[metrics] endpoint_addr = "127.0.0.1:9999"`;
   - `[network.zakura] trace_dir = "/path/zakura-traces"`.
2. Wait until the Zakura node is at the Testnet tip.
3. Write the hayaid configuration:

   ```
   target/release/hayaid config --network testnet > shadow.toml
   ```

   Set `[network] peers` to the P2P address of the Zakura node, and set `[trace] dir`.
4. Start the node:

   ```
   target/release/hayaid start -c shadow.toml
   ```

5. Sample both processes and join the traces:

   ```
   scripts/sample_procs.py <zakurad pid> <hayaid pid> --out procs.csv \
     --metrics http://127.0.0.1:9999/metrics --metrics http://127.0.0.1:19101/metrics
   scripts/join_traces.py --hayai traces --zakura /path/zakura-traces --out report.csv
   ```

The node stops on SIGINT or SIGTERM. It flushes the coins, writes a snapshot (memory
backend), syncs the block files and closes the trace files.

### Restart

A start with a `data_dir` that holds `state.log` resumes the node. A start with an empty
`data_dir` is a first start. A `data_dir` with files of `coins` or `blocks` and no
`state.log` is not a hayaid directory, and the node refuses it.

The node writes the files below:

| File | Content |
|---|---|
| `coins/` | The coins and the nullifiers. The best block record says which block the files hold. |
| `blocks/` | The block files and their index. |
| `state.log` | One checksummed record per coins flush: frontiers, value pools, history tree, block times and `bits`, and the anchors and the Sprout treestates that are new. The first record is the start state (the genesis block or the shadow seed). The node writes record version 4. A record of version 3 has no Sprout state: the node resumes from it, and above the genesis block it then does not know the Sprout state and refuses a block with a JoinSplit. A record of version 1 or 2 does not hold the transparent and the deferred value pool. The node resumes from such a record only at the genesis block. Above the genesis block the node stops with an error: remove `data_dir` and start the node again (a full node validates from the genesis block, a shadow node reads a new seed). |
| `spent.log` | Shadow mode: the outpoints that hayai spent, as the coins store forgot them. |
| `headers.log` | Full mode: every header that the header chain accepted, and the blocks that it found invalid. The node makes the log durable before each coins flush. |
| `peers.dat` | Full mode: the address book of the peer manager. |

A restart takes these steps:

1. Open the coins store and read its best block.
2. Select the record of `state.log` for that block. Drop the later records. The node writes a
   record before the flush that it belongs to, so a record for the best block always exists.
   Without a best block, the start record is the state.
3. Restore the base of the chain from the record (the node made the block files durable
   before the coins flush of that record), then validate and push every block of
   the block files above the base (about the last 1,000 blocks, the finality depth, plus
   the blocks since the last flush). A block above the last checkpoint of the network is
   validated in full. A full node applies a block at or below it with the checkpoint path.
4. Full mode: open the header chain from `headers.log`, without a second run of the header
   rules. The node adds the headers of the committed blocks that the log does not hold,
   from the block files. The committed blocks are valid bodies in the header chain.
5. Open the P2P port. A full node continues the header sync from its best header and the
   block download from its committed tip. The bodies that were in memory are requested
   again.

A clean stop and a crash differ only in the number of replayed blocks. A shadow node that
restarts does not read a new seed. Its follower fetches the blocks that it lacks, up to
`MAX_CATCH_UP` (1,000).

A replay ends at the first stored block that does not extend the replayed chain: after a
reorg to a shorter branch, the index by height names a block of the old branch above the
tip of the new branch.

A failed first start removes what it created in `data_dir`, up to the write of the start
record. A seed that fails (the Zakura node is not reachable or not synchronized) leaves
`data_dir` as it was. A node refuses a `data_dir` of another network or mode.

## Configuration

`hayaid config --network regtest|testnet|mainnet` prints every key with its default.
Unknown keys are errors.

| Section | Key | Default | Meaning |
|---|---|---|---|
| `[network]` | `network` | (required) | `regtest`, `testnet` or `mainnet` |
| | `mode` | `full` | `full` or `shadow` |
| | `listen_addr` | none | P2P listen address; none: dial out only |
| | `peers` | `[]` | Peers to dial, and to dial again every 10 s while disconnected |
| | `compact_relay` | `true` | Offer the compact-relay extension |
| | `max_peers` | `16` | The node closes the newest inbound connections above this count |
| | `prebuilt_candidates` | `0` | Candidates of peers' lanes on the tip whose body the node prebuilds while idle, at most; 0: off |
| | `outbound_peers` | `8` | Full mode: outbound peers that the peer manager keeps (`outbound_target`) |
| | `max_inbound` | `64` | Full mode: inbound peers accepted, at most |
| | `max_per_ip` | `1`; Regtest: no bound | Full mode: connections with one IP address, at most |
| | `seeders` | Mainnet: `dnsseed.str4d.xyz:8233`, `dnsseed.z.cash:8233`, `mainnet.seeder.shieldedinfra.net:8233`, `mainnet.seeder.zfnd.org:8233`; Testnet: `dnsseed.testnet.z.cash:18233`, `testnet.seeder.zfnd.org:18233`; Regtest: none | Full mode: DNS seeders as `host:port` |
| | `ban_secs` | `86400` | Full mode: duration of a ban |
| `[sync]` | `memory_budget_bytes` | `1073741824` | Full mode: bound of the downloaded blocks in memory plus 2 MB for each request without an answer |
| | `request_timeout_ms` | `8000` | Full mode: a peer with a request that sends no block for this time stalls; two stalls disconnect it |
| | `header_timeout_ms` | `120000` | Full mode: the peer of the header sync is disconnected after this time without an answer |
| `[state]` | `data_dir` | `hayaid-data` | Coins store, block files and state logs; an empty directory starts a new node, a hayaid directory resumes it (Restart) |
| | `backend` | `memory` | `memory` (`MemBacking`: log and snapshots) or `rocksdb` (`RocksBacking`) |
| | `flush_interval_blocks` | `100` | Blocks between two flushes of the finalized coins |
| | `snapshot_interval_blocks` | `10000` | Memory backend: finalized blocks between two snapshots |
| `[rpc]` | `listen_addr` | none | JSON-RPC server; full mode only |
| `[metrics]` | `listen_addr` | none | `GET /metrics` |
| `[trace]` | `dir` | none | JSONL trace directory; none: tracing is off |
| | `node` | `hayaid` | The `node` field of every row |
| `[mining]` | `miner_address` or `miner_script` | (one is required) | Script of the coinbase miner output |
| | `regtest_produce` | `false` | Serve `generate n` (Regtest full mode) |
| | `prebuild_own` | `true` | Full mode: prebuild the body of the newest template, so an own block commits as a pointer swap |
| `[regtest]` | `activation_heights` | none | Regtest only: `{ nu6 = h, nu6_1 = h, nu6_2 = h, nu6_3 = h }`, the activation heights of the upgrades after NU5. Each height is above 1 and at or above the height of the upgrade before it. Each node of one Regtest network needs the same values, and a node keeps them for the life of its `data_dir` |
| | `checkpoints` | `[]` | Regtest only: `[[height, "hash"], ...]`, the hash as `getbestblockhash` prints it. The genesis block is always a checkpoint |
| | `mandatory_checkpoint_height` | `0` | Regtest only: a block at or below this height has the checkpoint path only. The last checkpoint must be at or above it |
| `[shadow]` | `rpc_addr` | (required in shadow mode) | JSON-RPC of the Zakura node |
| | `start_height` | upstream tip | hayai validates from `start_height + 1` |
| | `poll_interval_ms` | `200` | Time between two `getbestblockhash` calls |
| `[log]` | `level` | `info` | `error`, `warn`, `info`, `debug` or `trace` |

The node writes its log to stderr. It writes ANSI colour codes only when stderr is a terminal.

The JSON-RPC server serves `getblocktemplate`, `submitblock`, `getblockcount`,
`getbestblockhash` and, with `regtest_produce`, `generate`. It also serves these query
methods:

| Method | Parameters | Result |
|---|---|---|
| `getblockhash` | height | The hash of the block of the committed chain |
| `getblock` | height or hash, verbosity `0` | The block as hex. No other verbosity |
| `getblockchaininfo` | none | `chain`, `blocks`, `bestblockhash`, and `valuePools` with `id` and `chainValueZat` for `transparent`, `sprout`, `sapling`, `orchard`, `ironwood` and `lockbox` |
| `z_gettreestate` | height or hash of the tip | `hash`, `height`, `time`, and `commitments.finalRoot` of `sapling`, `orchard` and `ironwood`. The node has the tree state of the tip only |
| `getrawmempool` | none | The transaction ids of the mempool |
| `sendrawtransaction` | transaction as hex | The transaction id. The mempool admission applies; a refusal is error -26 with the reason |

`getblocktemplate`: `mintime` is the median-time-past plus 1 s, `maxtime` is the
median-time-past plus 90 min, and `curtime` is the clock of the node inside these limits.

## Regtest

hayaid follows Zakura's Regtest (`zakura-chain/src/parameters/network/testnet.rs`,
`Parameters::new_regtest`):

- the zcashd Regtest genesis block `029f11d8…e327` and the Regtest network magic;
- Overwinter, Sapling, Blossom, Heartwood and Canopy at height 1 (Zakura's default);
- NU5 at height 1: a Zakura node of the pair must set
  `[network.testnet_parameters.activation_heights] NU5 = 1`;
- NU6, NU6.1, NU6.2 and NU6.3 at the heights of `[regtest] activation_heights`, and no
  activation without that key;
- the checkpoints and the mandatory checkpoint height of `[regtest]`: the genesis block
  and height 0 without that section;
- no funding streams, no lockbox, no slow start; the subsidy is 6.25 ZEC, halved every
  288 blocks after Blossom;
- the proof-of-work limit `0x0f0f…0f` (compact `0x200f0f0f`);
- the median-time-past + 90 min rule from height 2.

### Proof of work

Regtest uses Equihash (48, 5): a solution is 36 bytes and a header is 177 bytes. hayai-wire
parses these headers, and hayai-net, hayai-relay and hayai-template use the parameters of
the network (`NetParams::pow`).

Zakura's Regtest sets `disable_pow`. Under this waiver Zakura checks two things only:

- the solution has the Regtest shape (36 bytes, Equihash (48, 5));
- the bits encode a target that is not easier than the limit.

Zakura runs neither the hash-to-target filter nor Equihash on Regtest, and its internal
miner sends a null solution. hayaid applies the same waiver in its header check: it
rejects a solution of another length, and it checks the bits. Its producer is trivial: it
takes the current template, sets a counter as the nonce and a 36-byte all-zero solution,
and gives the block to the relay. hayaid needs no Equihash solver, and a Zakura node
accepts its blocks.

zcashd Regtest is different: it verifies Equihash (48, 5) and the hash filter. A pair with
zcashd needs a (48, 5) solver. hayai-wire verifies (48, 5) solutions
(`check_equihash(header, PowParams::REGTEST)`, tested on the zcashd Regtest genesis block).

`docs/regtest-pair.md` has the pair of one hayaid and one zakurad: the configuration that
both nodes share, the scenarios and their results.

## Mainnet

Mainnet is a network kind like Testnet. The operator chooses it. No code refuses it.

| Parameter | Value |
|---|---|
| Genesis block | `00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08`, time 1,477,641,360 |
| P2P magic | `0x24e92764` |
| Proof of work | Equihash (200, 9), limit `bits` `0x1f07ffff` |
| Activation heights | Overwinter 347,500; Sapling 419,200; Blossom 653,600; Heartwood 903,000; Canopy 1,046,400; NU5 1,687,104 (`zcash_protocol` `MAIN_NETWORK`) |
| Coinbase terms (both modes) | `hayai_consensus::coinbase::CoinbaseTerms`: slow start, halvings with Blossom, founders' reward, funding streams, the 12 % lockbox share from height 2,726,400 to 4,406,399, the lockbox disbursement at height 3,146,400, the exact value from NU6 (ZIP 236) |

Both modes compute the subsidy and the coinbase terms of each block. The template takes its
coinbase outputs from the same terms. Full mode on Mainnet starts at the genesis block and
synchronizes from its peers (Full mode: synchronization).

The rules below are not enforced yet (`docs/consensus-rules.md` has the full list). The
operator decides whether the gaps are acceptable:

- The NU7 rules on the default (`upstream`) backend: the node stops with an error when
  its next block is the first block of NU7 (Testnet 4,465,026). A build with
  `--no-default-features --features zakura` has the NU7 rule set and continues
  (`docs/consensus-rules.md`, NU7).
- Sprout JoinSplits in shadow mode: the seed has no Sprout treestate, and a shadow node
  stops at the first block with a JoinSplit (Shadow mode, Sprout state).
- `hashBlockCommitments` in shadow mode before the history tree is known, and the start
  state of a shadow node (Trust limits).

## Shadow mode

The node reads its start state from the Zakura node, then follows it:

- The follower polls `getbestblockhash`. For a new tip, it fetches every block back to a
  block that hayai holds (`getblock <hash> 0`) and the tree state of each new block
  (`z_gettreestate <hash>`). It gives the driver the fork point and the new blocks.
- The P2P connection to the Zakura node fills the prepared store and gives the receive time
  of each block. A block from P2P that extends the tip is validated at once.
- The driver validates each upstream block that it does not hold yet, then compares its
  Sapling, Orchard and Ironwood roots with the roots of upstream (`z_gettreestate`, fields
  `sapling`, `orchard` and `ironwood`). From NU6.3 an answer without the Ironwood tree is
  an error of the follower. For each block it writes an
  `upstream_verdict` row.
- An upstream reorg disconnects hayai's layers back to the fork point (at most 1,000 blocks,
  the finality depth).

### Trust limits

hayai has no state before the start height. These items come from upstream and are not
checked by hayai:

| Item | Source | Counted in |
|---|---|---|
| Hash of the start block; time and `bits` of the start block and of the 27 blocks before it (the context of the header rules). The seed fails when upstream does not give all of them | `getblock <hash> 1` | — |
| Sapling, Orchard and Ironwood frontiers at the start height | `z_gettreestate` | — |
| The six chain value pools at the start height | `getblock <hash> 1` `valuePools` (ids `transparent`, `sprout`, `sapling`, `orchard`, `ironwood`, `lockbox`). The seed fails without `transparent`, `sprout`, `sapling` or `orchard`, without `ironwood` from NU6.3, and without a `lockbox` value above zero from NU6 | — |
| Coins created at or below the start height (value, script, height, coinbase flag) | `getrawtransaction <txid> 1` | `hayai_shadow_trusted_coins_total`, trace `trusted_coins` |
| Unspentness of those coins before the start height | none | as above |
| Uniqueness of a nullifier against the history before the start height | none | `hayai_shadow_trusted_nullifiers_total`, trace `trusted_nullifiers` |
| Anchors older than the start height | none | `hayai_shadow_trusted_anchors_total`, trace `trusted_anchors` |
| `hashBlockCommitments` (no ZIP 221 history before the start) | none | `hayai_block_commitments_unchecked_total`, trace `block_commitments_checked` |

Notes:

- hayai computes the block subsidy, the founders' reward, the funding streams, the lockbox
  terms and the expected `bits` of every block after the start. It makes no
  `getblocksubsidy` request.
- `hayai_shadow_trusted_bits_total` counts the headers that passed a header rule only because
  the context of the node was too short for the rule. The seed and the state records hold the
  whole context (28 blocks), so the counter reads 0. A value above 0 shows a node whose
  context is not complete.
- The deferred pool of the start state is the base of the lockbox rules after the start: the
  NU6.1 activation block must find its disbursement in the pool.
- The node compares its tree roots with upstream after every block. hayai therefore holds
  every root from the start height on, and an anchor that hayai does not hold is older than
  the start height.
- `gettxout` is in Zakura's restricted method set. It answers for the upstream tip, which
  runs ahead of hayai: a coin that the block under validation spends already reads as
  spent. It also gives the value as a float and no height. The backing therefore uses
  `getrawtransaction`. A coin that hayai spent after the start never goes upstream again.
- An upstream call fails three times: the node stops (a missing coin would fail a valid
  block).
- The shadow template uses a zero history root when the history tree is unknown, and is
  never served. Its coinbase pays the terms of its height. It exists for the `template_empty` and `template_full` times.

### Sprout state

A shadow node has no Sprout treestate. The RPC of the upstream node cannot give it:

- `z_gettreestate` of zakurad and of zebrad returns the Sapling, Orchard and Ironwood
  trees. The `sprout` field of the response type is never set
  (`zakura-rpc/src/methods.rs:2649-2659`, revision `1377915`: "We can't currently return
  Sprout data because we don't store it for old heights";
  `zakura-rpc/src/methods/trees.rs:85-89`).
- `getblock` gives tree sizes for Sapling, Orchard and Ironwood only, and
  `z_getsubtreesbyindex` accepts `sapling`, `orchard` and `ironwood` only
  (`zakura-rpc/src/methods.rs:2670`).
- The state of Zakura holds the Sprout tip frontier and the frontier of each anchor
  (`sprout_note_commitment_tree`, `sprout_anchors`), and no RPC method reads them.

A JoinSplit needs the frontier of its anchor, so the seed cannot replace the state. At the
first block with a JoinSplit the node writes an `upstream_verdict` row with
`"hayai": "not_validated"`, `"agree": null` and the reason, and stops with an error that
names the block and this limit. The row is not a disagreement: hayai has no verdict. A
full node holds the Sprout state from the genesis block.

### Disagreement

A block that upstream accepted and hayai rejects, or a difference of tree roots, is a
disagreement. The node writes an `upstream_verdict` row with `"level": "error"` and
`"agree": false`, logs an error, and stops: it cannot build on a block that it does not
hold.

### Restrictions

- Set `peers` to the Zakura node only. The relay forwards every block to the other peers;
  the Zakura node is the source of each block and receives no forward.
- Transactions of blocks that an upstream reorg disconnects do not go back to the prepared
  store.

## Traces

The rows have the envelope of Zakura's `zakura-jsonl-trace`: `ts` (µs since the tracer
opened), `node`, `process_trace_id`, `event`. hayai adds `unix_us` (µs since the Unix
epoch) to every row. The writer has a bounded queue of 16,384 rows, drops and counts rows
when the queue is full (`hayai_trace_dropped_rows_total`), and flushes and fsyncs every
1 s.

| File | Event | Fields |
|---|---|---|
| `block_sync.jsonl` | `block_header_checked` | `hash`, `height`, `result` (`ok`, `rejected`), `reason`, `elapsed_us` |
| | `block_received` | `peer`, `height`, `hash`, `bytes`, `source` (`legacy`, `compact`, `local`, `download`, `upstream_rpc`) |
| | `sync_progress` | full mode, at most one row each second, when a value changed: `headers_height`, `blocks_height`, `peers`, `in_flight`, `held_bytes` |
| `commit_state.jsonl` | `commit_start` | `source` (`hayaid`), `apply_class` (expected: `full`, `prebuilt_own`, `prebuilt_candidate`, `checkpoint`), `origin` (`local`, `legacy`, `compact`, `download`, `stored`, `upstream_rpc`), `height`, `hash` |
| | `block_validated` | `height`, `hash`, `result` (`valid`, `invalid`, `wrong_body`, `not_validated` for a local fault), `class` (`empty`, `transparent`, `shielded`), `txs`, `known`, `unknown`, `<stage>_us` for each stage of `hayai_validate::Timings`, `block_commitments_checked`, `trusted_coins`, `trusted_nullifiers`, `trusted_anchors`, `reason` |
| | `commit_finish` | `source`, `apply_class` (taken: `full`, `prebuilt_own`, `prebuilt_candidate`, or `prebuilt` for a block a prebuilt body rejected), `height`, `hash`, `result` (`committed`, `rejected`, `stopped` for a local fault), `reason`, `elapsed_ms` |
| | `block_disconnected` | a reorg: `height`, `hash` |
| | `upstream_verdict` | shadow mode: `height`, `hash`, `hayai` (`valid`, `invalid`, `not_validated`), `upstream`, `agree` (`null` when hayai has no verdict), `level`, `reason` |
| `template.jsonl` | `template_empty`, `template_full` | `height`, `parent`, `template_id`, `txs`, `fees`, `since_tip_us` |

`commit_start` and `commit_finish` have Zakura's names and fields. The relay emits
`block_received` when the complete body reaches the driver's queue: after the header
check and the forward. `source` is the protocol of the sending peer.

The relay of hayai-net has no observer interface, so hayaid emits no
`block_reconstructed` and no `block_forwarded` rows. The counters of
`Relay::metrics()` are in `/metrics`.

## Metrics

| Name | Type | Meaning |
|---|---|---|
| `zcash_chain_verified_block_height` | gauge | Height of the newest validated block on the best chain (Zakura name) |
| `state_memory_best_committed_block_height` | gauge | Height of the committed tip (Zakura name) |
| `sync_block_verify_duration_seconds{result}` | histogram | Validation time, `success` or `failure` (Zakura name) |
| `zcash_mempool_size_transactions`, `zcash_mempool_size_bytes` | gauge | Prepared store (Zakura names) |
| `mining_template_rebuilt` | counter | Full and changed templates (Zakura name) |
| `hayai_validate_stage_duration_seconds{stage}` | histogram | Each stage of `hayai_validate::Timings` |
| `hayai_commit_duration_seconds` | histogram | `commit_start` to `commit_finish` |
| `hayai_prepared_store_hits_total`, `hayai_prepared_store_misses_total` | counter | Block transactions from the store, or prepared during validation |
| `hayai_blocks_rejected_total` | counter | Blocks that failed validation: invalid blocks and wrong bodies |
| `hayai_peers` | gauge | Connected peers |
| `hayai_sync_header_height` | gauge | Full mode: height of the best header chain |
| `hayai_sync_peers` | gauge | Full mode: peers that the block download can ask |
| `hayai_sync_requests_in_flight` | gauge | Full mode: block requests without an answer |
| `hayai_sync_held_bytes` | gauge | Full mode: bytes of the downloaded blocks that wait for the validator |
| `hayai_sync_bodies_withheld` | gauge | Full mode: 1 while a header chain is out of the fork choice because no peer sends its blocks |
| `hayai_sync_withheld_chains_total` | counter | Full mode: times that the node took a header chain out of the fork choice for that reason |
| `hayai_blocks_disconnected_total` | counter | Committed blocks that a reorg disconnected |
| `hayai_mempool_rejected_total{reason}` | counter | Transactions that the mempool refused: `policy` (policy and store rules, and a tip that changed during 3 admissions) or `invalid` (every other reason) |
| `hayai_relay_forwarded_on_ids_total`, `hayai_relay_forwarded_without_auth_root_total`, `hayai_relay_forwarded_after_body_total`, `hayai_relay_root_mismatches_total` | counter | `Relay::metrics()` |
| `hayai_relay_candidate_blocks_sent_total`, `hayai_relay_candidate_blocks_resolved_total`, `hayai_relay_candidate_fallbacks_total` | counter | Candidate blocks sent, rebuilt from a stored candidate, and fetched in full instead |
| `hayai_prebuilt_commits_total{origin}` | counter | Blocks committed from a prebuilt body, `own` or `candidate` |
| `hayai_prebuild_duration_seconds` | histogram | One body prebuild |
| `hayai_trace_dropped_rows_total{table}` | counter | Trace rows dropped |
| `hayai_block_commitments_unchecked_total` | counter | Blocks without the `hashBlockCommitments` check |
| `hayai_shadow_trusted_coins_total`, `_nullifiers_total`, `_anchors_total`, `_bits_total` | counter | Trust limits of shadow mode |
| `hayai_shadow_agreements_total`, `hayai_shadow_disagreements_total` | counter | Upstream verdicts |
| `process_resident_memory_bytes` | gauge | Resident memory of the process (`/proc/self/status`), updated every second |
| `process_cpu_seconds_total` | counter | User and system CPU time of the process (`/proc/self/stat`) |
| `hayai_template_latency_seconds{template}` | histogram | Tip change to the published `empty` or `full` template |
| `hayai_coins_cache_entries`, `hayai_coins_cache_bytes` | gauge | Entries and memory of the coins cache of the finalized state |
| `hayai_coins_store_coins` | gauge | Coins in the memory backing (memory backend) |
| `hayai_build_info{version,chain,mode,crypto_backend,coins_backend}` | gauge | Always 1; the labels name the build and the configuration |

## Limits

- Restart: the replay validates the blocks above the base in full (Restart). A node that
  stopped for more than `MAX_CATCH_UP` blocks does not catch up in shadow mode. A restarted
  shadow node serves headers from its restart point on: the header index holds the last 28
  blocks of the base (hash, time and `bits`) and the replayed blocks. A full node serves
  the headers of its header chain, up to the first block whose body it does not have.
- Withheld bodies: a header chain that leaves the committed chain more than 1,000 blocks
  below its own tip removes the headers of the committed chain from the header chain
  before the node takes it out of the fork choice. The node then downloads and validates
  the blocks above the fork point again. Such a chain needs more work than 1,000 blocks
  of the network.
- A request to a peer whose chain is not known (no `headers` message from it) uses the
  height that the peer reported. A zcashd peer does not answer a `getdata` for a block
  that it does not have, so such a request can give that peer a stall.
- State files: `state.log` and `headers.log` have no compaction. A start reads both files
  from their first record. The header log keeps the record of a side header that left the
  header chain. A start skips a header record that the checkpoint list or the finalized
  height of the build refuses, with the records of its descendants.
- A local fault stops the node. A restart resumes the synchronization and stops at the
  same block while the cause stays (Full mode: synchronization, the fault table).
- Shadow mode and Sprout: Shadow mode, Sprout state.
- No rate limit for `mempool` requests, and no eviction of inbound peers.
