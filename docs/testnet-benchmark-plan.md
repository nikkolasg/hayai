# Testnet and same-hardware benchmark plan

Date: 2026-10-03. Scope: how to measure hayai against Zakura on real blocks, on the same
hardware, and on the network, with a small budget.

## Questions

There are 4 questions. Each question maps to one metric of `docs/architecture.md`.

| Question | Metric |
|---|---|
| How fast does a node validate and commit a block it receives? | M1 |
| How fast does a block from one miner reach the other miners? | M2 |
| How fast does the template follow a tip change or a mempool change? | M3 |
| How much CPU, memory, and disk does the node use for the same work? | M4 and resources |

## What Zakura already measures

Zakura runs its own benchmarks. Most of them are private. Some are reproducible.

| Zakura asset | Location in the Zakura repository | Public | Use for hayai |
|---|---|---|---|
| Continuous sync: 3 Mainnet hosts that sync from genesis after each merge | `deploy/continuous-sync/` | No (Slack digest, private object store) | Reproduce the method, not the data |
| Feedrun: sync from a Mainnet snapshot against 2 frozen peers, metrics to CSV | `deploy/runner/feed_run.sh`, `feed_analyze.py`, `scripts/make/perf.mk` | Scripts yes, peers no | Run the same script against a local `zakurad` |
| Perf-bench workflow: latency for each block from `commit_start` to `commit_finish` | `docs/cpu-profiling.md`, `.github/workflows/zakura-perf-bench.yml` | Method yes | Make hayai write the same 2 events |
| JSONL traces: `block_sync.jsonl`, `commit_state.jsonl` | `crates/zakura-jsonl-trace`, config `[network.zakura] trace_dir` | Yes, on each node that an operator runs | Join rows by block hash |
| Prometheus metrics | config `[metrics] endpoint_addr` | Yes, on each node that an operator runs | Sample both nodes with one script |
| Fleet status page for the testnet nodes | `deploy/runner/zakura-cluster-status.py` | Yes | Read the tip agreement of the Zakura nodes |
| Mempool first-seen matrix across their testnet fleet | `deploy/mempool-spam/scripts/round_robin_selfsend.py` | Scripts yes | Reuse the first-seen method for blocks |

Zakura has no script that measures block propagation on the real network. The propagation
numbers of Zakura come from simulations. A measurement from hayai is new data for everyone.

## Phases

### Phase 0: component benchmarks

Status: done. The `hayai-bench` crate compares each component with Zakura code or with a port
of the Zakura data layout. The `sysbench` binary adds CPU time, memory, page faults, cache
misses, and disk bytes. These numbers isolate each design decision. They do not show the end to
end effect.

### Phase 1: same hardware, same blocks, Testnet

Goal: measure M1, M3, and resources for both nodes on identical input, with zero risk to the
chain. Machines: this workstation. Cost: none.

Set up:

1. Run `zakurad` on Testnet with `[metrics] endpoint_addr`, `[network.zakura] trace_dir`, RPC
   enabled, and `storage_mode = "pruned"`. Start from a Testnet snapshot (about 8.3 GB).
2. Run `hayaid` in shadow mode. Shadow mode has these properties:
   - `hayai-net` connects only to the local `zakurad`. It receives each block and each
     transaction from it. The relay forwards each block to each other peer, so a shadow node
     has no other peer.
   - `hayai-validate` validates each block against its own state.
   - The chain tip comes from the local `zakurad` through RPC (`getbestblockhash`,
     `getblock`). Thus a rule that hayai does not implement cannot split the view of hayai.
   - The compact-relay extension is off, and hayai does not announce blocks. The node only
     listens and validates.
   - `docs/hayaid.md` gives the limits of the state that hayai seeds from `zakurad`.
3. Run one sampler script that reads `/proc/<pid>/{status,io,stat}` for both processes every
   1 s and both `/metrics` endpoints every 5 s.

Measure these events for each block hash:

| Event | Zakura source | hayai source |
|---|---|---|
| First byte of the body received | `block_sync.jsonl` `block_body_received` | hayai trace `block_received` |
| Validation done | `sync.block.verify.duration_seconds` | hayai trace `block_validated` |
| Commit start and finish | `commit_state.jsonl` `commit_start`, `commit_finish` | hayai trace with the same 2 event names |
| Template after tip | `mining.*` histograms, `getblocktemplate` poll | `TemplateEmpty` and `TemplateFull` timestamps |
| RSS, CPU time, IO bytes | `/proc` | `/proc` |

Compare the medians and the p95 for each block class: empty, transparent, shielded. The
expected result is that hayai is close to Zakura on typical Testnet blocks, because those
blocks are small. The gap appears on full blocks. Phase 2 makes full blocks.

### Phase 2: local multi-node testbed, Regtest

Goal: measure M2 as a function of the share of nodes that run hayai, with full blocks and real
latency. Machines: this workstation. Cost: none.

Set up:

1. Create N network namespaces with `ip netns`. Connect them through a bridge. Add latency and
   bandwidth with `tc netem` (for example 40 ms to 150 ms one way, 100 Mbit/s to 1 Gbit/s).
2. Start `zakurad` Regtest nodes from the configuration in
   `docker/zakura-regtest-e2e/node1.toml`. Start `hayaid` Regtest nodes. Enable `trace_dir`
   or the hayai trace on each node.
3. Produce blocks on one node. Regtest Equihash is small (n = 48, k = 5). Thus the Zakura
   internal miner or a hayai solver finds a block in less than 1 s.
4. Fill the mempool of each node through `sendrawtransaction`. Use the fixture generator of
   `hayai-bench` (transparent and Orchard transactions with real signatures and proofs).
5. Set the share of hayai nodes to each of these values in turn: 0 %, 25 %, 50 %, 75 %,
   100 %. Keep the topology fixed.
6. For each block, record the time of the first announcement at the producer. Also record the
   time of validation at each node. Report the time until 50 %, 90 %, and 100 % of the nodes
   have validated the block.

Sizing: a Regtest `zakurad` uses less than 1 GB of RSS at this chain size. 10 Zakura nodes and
20 hayai nodes fit in the 60 GB of this workstation. 50 nodes give no more information than
20 nodes. The result depends on the share and the hop count. It does not depend on the
absolute count.

### Phase 3: real network, few machines

Goal: confirm Phase 2 on real paths. Machines: 2 or 3 small virtual servers (2 vCPU, 4 GB RAM,
40 GB SSD) in different regions. Cost: about 20 USD to 40 USD per month.

Set up:

1. Run one `hayaid` on each server. Peer them with each other and with the public Testnet.
2. Run one `zakurad` on one server of the same size, for the resource comparison.
3. Reuse the first-seen method of Zakura's mempool matrix for blocks: every node records the
   first time it sees each block hash. Compute the spread.

3 servers in different regions are enough to measure a path of 2 hops. More servers add cost
and do not add a new effect.

## Benefit of hayai nodes

The local improvements apply to one node. A single hayai node validates faster, serves blocks
earlier, and gives its pool a template sooner. No other node has to change.

The relay improvement applies to each hop. The gain on a path depends on the number of hops where
both ends run hayai. A hayai node also helps its legacy neighbours: it serves the full body
before it commits the block. This removes the commit time from the legacy hop.

Model for the expected time of one block between 2 miners, with `h` hops:

```
T = sum over hops of (RTT_hop * r + transfer_hop + processing_hop)
    r = 0.5 if both ends run hayai (push), 1.5 otherwise (inv, getdata, body)
    transfer_hop = 0 if both ends run hayai and the body reconstructs, else body size / bandwidth
    processing_hop = header check if the forwarder runs hayai, else full verify + commit
```

With a share `f` of hayai nodes at random positions, the probability that a hop has hayai at both
ends is about `f²`. The probability that the forwarder runs hayai is `f`. The curve is convex.
The first 25 % of adoption gives about 40 % of the maximum gain through the forwarder term.
The second half of the gain needs both ends. Phase 2 measures this curve directly.

## Instrumentation of hayai before Phase 1

| Item | Crate | Status |
|---|---|---|
| `hayaid` binary with Regtest, Testnet, shadow mode, and a configuration file | new `hayaid` crate | done (`docs/hayaid.md`) |
| JSONL trace with `block_received`, `block_header_checked`, `block_reconstructed`, `block_validated`, `commit_start`, `commit_finish`, `template_empty`, `template_full`, each with `height`, `hash`, `ts` | new `hayai-trace` crate | done, plus `unix_us` and `upstream_verdict`; `block_reconstructed` and `block_forwarded` need an observer interface in hayai-net |
| `/metrics` endpoint with the same names as Zakura where the meaning is the same | `hayai-rpc` | done |
| Sampler and join scripts (`scripts/sample_procs.py`, `scripts/join_traces.py`) | `scripts/` | done, plus `scripts/regtest_pair.sh` |
| Regtest Equihash solver (n = 48, k = 5) for the testbed producer | `hayai-wire` or `hayaid` | not needed for Zakura: its Regtest does not check the proof of work; the producer sends a 36-byte null solution. A pair with zcashd needs a solver |
| Consensus rules that Testnet blocks exercise and hayai does not implement yet: difficulty, funding streams | `hayai-prepared`, `hayai-state` | to do, shadow mode tolerates gaps |

## Controls

- Do not mine on Testnet with hayai while `docs/consensus.md` has a `not implemented` row
  that a Testnet block can hit.
- Keep the compact-relay extension off in Phase 1. Turn it on in Phase 2 and Phase 3.
- Record the commit hash of both binaries in each result file. Record the CPU model, the
  thread count, the RAM, and the disk model.
- Run each Phase 1 comparison for at least 24 h. The sample then includes full blocks.
