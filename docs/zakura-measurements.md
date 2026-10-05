# Measurements of Zakura and hayai for the Mainnet comparison

Date: 2026-10-05. Zakura: commit `1377915` (`zakurad 1.6.0`), default build features
(`prometheus`, `opentelemetry`, `release_max_level_info`, `progress-bar`). hayai: branch
`followups`, working tree of 2026-10-05.

Paths of Zakura are relative to the Zakura source root. Paths of hayai are relative to this
repository. The line numbers of hayai are those of the working tree before the edits of
2026-10-05 evening.

Evidence labels:

- "run": seen on a running `zakurad` on Regtest, 127.0.0.1 (section "Ground truth runs").
- "source only": read in the source, not seen on a running node.
- "delegated": the hayai code was read by a second reader, not by the author of the verdict.
  No row has the verdict SAME for this reason.

## Summary

- Zakura exports no duration from "block received" to "block committed" in `/metrics`.
- Zakura exports no time from "new tip" to "template ready". Zakura builds a template only
  when a `getblocktemplate` request arrives (source only).
- Zakura exports no `process_*` metric (run).
- Each Zakura duration is a Prometheus summary, not a histogram (run). Quantiles: 0, 0.5,
  0.9, 0.95, 0.99, 0.999, 1. The quantiles use a window of 60 s. `_sum` and `_count` are
  cumulative.
- On Mainnet the default stack is `legacy` (`crates/zakura-network/src/config.rs:136`). With
  this stack `zakurad` writes 2 trace tables: `legacy_sync` and `legacy_peer_request` (run).
- The rows `commit_start` and `commit_finish` come only from the native block sync driver
  (`crates/zakurad/src/commands/start/zakura/block_sync_driver.rs:1384`, `:1409`). A node
  with the stack `legacy` does not start this driver. A Mainnet node with the default
  configuration writes no commit row.
- With the stack `dual`, a block from a legacy peer goes through the gossip path and gets no
  commit row (run: 5 blocks from hayaid, 0 commit rows; 12 blocks from a Zakura peer, 12
  commit rows).
- The per-block comparison by height needs, on the Zakura side, the trace table
  `legacy_peer_request` and the log file. Prometheus alone gives only the time of the
  contextual commit.
- The interval "block seen to template ready" is comparable only with an external probe.

## Comparison matrix

Verdicts: SAME, CLOSE, ZAKURA-ONLY, HAYAI-ONLY, NEITHER.

### Tip phase, per block

| # | Quantity | Zakura source and definition | hayai source and definition | Verdict | Smallest change on hayai |
|---|---|---|---|---|---|
| T1 | Block received to block committed, legacy stack | No metric. Start: trace row `legacy_peer_request` / `block_request_finish` with `returned_height`, `returned_hash`, `elapsed_ms` (`crates/zakura-network/src/peer_set/set.rs:1155-1166`; the peer service returns the decoded block; `elapsed_ms` starts at the send of `getdata`, `set.rs:1148`). Stop: log line `downloaded and verified gossiped block height=Height(N)` (`crates/zakurad/src/components/inbound/downloads.rs:821`), written after `verifier.oneshot(Request::Commit(block))` returns (`downloads.rs:813-817`). The commit response comes after the contextual check, the push to the non-finalized chain and the update of the tip channels (`crates/zakura-state/src/service/write.rs:3084-3099`). Run: both exist. The 2 clocks need a calibration (section "Trace clocks of Zakura"). | Trace rows `block_received` (`crates/hayaid/src/sync.rs:609`, source `download`: on the driver thread, after the parse and after the wait in the driver queue) and `commit_finish` (`crates/hayaid/src/node.rs:1141`). Metric: none. | CLOSE. Start: hayai stamps after the queue wait, Zakura stamps when the peer service returns; this favours hayai. Stop: hayai includes the append to the block file and the cleanup of the prepared store; Zakura stops after an in-memory commit plus one task wake; this favours Zakura. | Write the arrival time of `sync.rs:336` as the time of `block_received` (stamp on the reader thread after the parse). Add `hayai_block_receive_to_commit_seconds` (histogram) with the same start and with the stop at `TipWatch::set`. The stop stays CLOSE (less than 1 ms). |
| T2 | Block received to block committed, native stack | `block_sync.jsonl` / `block_body_received` (`height`) to `commit_state.jsonl` / `commit_finish` (`elapsed_ms` = `commit_start` to verifier response, `block_sync_driver.rs:1385-1409`). Run: written only for a block that the native block sync delivers. | Same rows as T1. `scripts/join_traces.py` joins these rows by hash. | CLOSE. Same differences as T1. Not usable on Mainnet with the default stack. | None. Do not use this row for the race. |
| T3 | Block received to block validated (before commit) | None. The semantic verifier sends the block to the state in the same call (`crates/zakura-consensus/src/block.rs:735`). No event marks the end of the proof checks. | Trace row `block_validated` with stage fields (`node.rs:1056`). | HAYAI-ONLY | Cannot be made comparable without a Zakura change: Zakura has no event between receive and commit response. |
| T4 | Contextual commit time | Summary `state_contextual_total_duration_seconds`. Start `write.rs:1459`, stop `write.rs:1485`: initial contextual checks, then `commit_block` or `commit_new_chain` on the non-finalized state (transparent spends, anchors, note commitment trees, chain push). One sample for each block. Run: count 292 for 292 blocks. Parts: `state_contextual_*_duration_seconds` (16 names). | `hayai_validate_stage_duration_seconds{stage="context"|"trees"|"history"}` (`crates/hayai-state/src/check.rs:945`, `:986`, `:997`). `Chain::push` has no timer. | CLOSE. Zakura includes the chain push and the clone of the chain; hayai excludes the push. The bias is toward hayai. | Add `hayai_contextual_commit_duration_seconds`: `context` + `trees` + `history` + `Chain::push` or `Chain::confirm`, one sample for each block. |
| T5 | Wait in the queue of the block writer | Summary `state_block_writer_queue_duration_seconds`. Start `crates/zakura-state/src/service.rs:1299` (`queued_at`), stop `write.rs:2944`. Run. | None. hayai has one driver thread and no writer queue. | ZAKURA-ONLY | None. The quantity has no meaning on hayai. |
| T6 | Shielded proof and signature time | Summary `zakura_consensus_batch_duration_seconds{verifier,result}`: one sample for each batch flush, start and stop around the batch validation on the rayon pool (`crates/zakura-consensus/src/primitives/halo2.rs:519-529`; the same form in `sapling.rs`, `redjubjub.rs`, `redpallas.rs`, `ed25519.rs`). Mempool batches and block batches are in the same series. Run: `verifier="halo2"`, count 16. | `hayai_validate_stage_duration_seconds{stage="shielded"}`: one sample for each block (`crates/hayai-validate/src/lib.rs:424-431`). Mempool verification has no timer. | CLOSE, only as a sum over a long window. The unit differs (batch against block), and Zakura includes the mempool work. No per-block comparison. | Add `hayai_shielded_verify_seconds_total{context="block"|"mempool"}` (counter of seconds). Compare `rate()` of the sums. |
| T7 | Script check time | None. | `hayai_validate_stage_duration_seconds{stage="scripts"}`. | HAYAI-ONLY | Cannot be made comparable without a Zakura change: no timer around the script checks. |
| T8 | Reuse of mempool verification | Counters `zakura_consensus_cache_hit`, `_miss`, `_insert`, `_evict`, gauge `_size`, label `verifier` (`crates/zakura-consensus/src/primitives/cache.rs:446`, `:451`, `:285`, `:289`, `:294`). The cache holds successful verifications of shielded bundles (key: transaction ID, sighash, pool). A hit skips only the proofs and the signatures of the bundle; the transaction verifier runs the other checks again (`cache.rs:1-22`). Run: hit 24, miss 16, insert 16 for 41 transactions. | `hayai_prepared_store_hits_total`, `_misses_total`: transactions of a block found in the prepared store; a hit skips scripts and shielded checks; the coinbase counts as a miss (`node.rs:1054-1055`). | CLOSE. Unit: bundle against transaction. Zakura counts mempool lookups too. Zakura reuses no script result. | Add `hayai_block_shielded_bundles_total{reused="true"|"false"}`: bundles of block transactions with and without a prepared result. |
| T9 | New tip to template ready | None. The node builds a template on request (`crates/zakura-rpc/src/methods.rs:2911-3316`, source only). A long poll returns at once with a coinbase-only template when the tip changes (`methods.rs:3186-3231`, source only). | `hayai_template_latency_seconds{template="empty"|"full"}` and rows `template_empty`, `template_full` with `since_tip_us` (`node.rs:1557-1571`). | HAYAI-ONLY | Cannot be made comparable inside the node without a Zakura change: Zakura has no template before a request. Use the external probe (section "External probe"). |
| T10 | Service time of `getblocktemplate` | Summary `rpc_request_duration_seconds{method="getblocktemplate"}`. Start `crates/zakura-rpc/src/server/rpc_metrics.rs:47`, stop `:53`: the whole call after the parse of the request, with the long-poll wait. A call without `longpollid` contains the full build: state read, mempool read, ZIP 317 selection, coinbase, roots. A call with a `longpollid` waits, with or without the capability `longpoll` (Regtest run). Run: 83 calls, sum 0.127 s. | Histogram `rpc_request_duration_seconds{method="getblocktemplate"}`. Start `crates/hayai-rpc/src/rpc.rs:401`, stop `:411`: `dispatch` only, with the long-poll wait. A call without `longpollid` returns the template that the driver built before. A call waits only with a `longpollid` and the capability `longpoll` (`rpc.rs:469-483`). | CLOSE. Same boundaries. The type differs (summary against histogram): compare `_sum / _count` only. Calls with `longpollid` make the value useless on both nodes. | None in the node. The probe must send calls without `longpollid` for this row. The RPC caller of the race (`scripts/race_rpc_caller.py`) sends such a call each 5 s on each node machine. |
| T11 | RPC latency, other methods | Same summary, label `method`. Run: 11 methods. | Same histogram, label `method`. | CLOSE. Same remark on the type. Zakura parses the parameters inside the interval; hayai parses them before. | None. |
| T12 | Mempool size | Gauges `zcash_mempool_size_transactions` and `zcash_mempool_size_bytes` (`crates/zakurad/src/components/mempool/storage/verified_set.rs:407`, `:433`), set at each change of the verified set. Run. | Same names (`node.rs:1135-1140`, `:2661-2662`), set at each commit and each 1 s. Bytes = wire bytes of the prepared store. | CLOSE (delegated). The update moment differs by at most 1 s. | None. |
| T13 | Mempool admission time, transaction from RPC | `rpc_request_duration_seconds{method="sendrawtransaction"}`. The call waits for the result of the mempool verification (`methods.rs:1837-1856`). Run: 41 calls, sum 3.12 s. | Same metric. `dispatch` contains `admit` and the announcement to peers (`node.rs:2537-2541`). | CLOSE. hayai includes the announcement to the relay. | None. |
| T14 | Mempool admission time, transaction from a peer | None. Counters only: `mempool_queued_transactions_total`, `mempool_downloaded_transactions_total{version}`, `mempool_verified_transactions_total{version}` (`crates/zakurad/src/components/mempool/downloads.rs:622`, `:503`, `:541`). Run. | None. | NEITHER | Cannot be made comparable without a Zakura change. hayai can add its own timer for its own dashboard. |
| T15 | Mempool rejects | Counter `mempool_rejected_transactions_total{reason}` (`crates/zakurad/src/components/mempool.rs:860`, source only: no reject in the runs); gauge `mempool_rejected_transaction_ids` (`mempool/storage.rs:1002`, run). | `hayai_mempool_rejected_total{reason}`, peer path only (`crates/hayaid/src/mempool.rs:447`). | CLOSE. The label values differ, and hayai does not count the RPC path. | Count the RPC path. Keep the hayai name. |
| T16 | Tip height | Gauge `zcash_chain_verified_block_height` (`write.rs:3176` for a non-finalized commit, after the commit response and after the write of the block that leaves the reorg window; `crates/zakura-state/src/service/finalized_state.rs:734` for a checkpoint commit). Run. | Same name, set in `finish_commit` after the commit. | CLOSE (delegated). Same event: the tip after a commit. | None. |
| T17 | Block count | Counter `zcash_chain_verified_block_total` (`write.rs:3168`, `finalized_state.rs:736`). Run: 293 for the genesis block plus 292 blocks. | Same name, +1 for each committed block. | CLOSE (delegated). | None. |
| T18 | CPU for each block | None. `/metrics` has no `process_*` family (run). | `process_cpu_seconds_total` (1 s tick from `/proc/self/stat`). | HAYAI-ONLY | Do not use the hayai metric in the comparison. Use the same cgroup or node_exporter recipe for both nodes (section "CPU for each block"). |
| T19 | Block from `submitblock`: submit to commit | Summaries `mining_solved_header_check_duration_seconds`, `mining_state_admission_duration_seconds`, `mining_contextual_commit_duration_seconds` (`block.rs:450`, `methods.rs:3372`, `block.rs:803`), and `rpc_request_duration_seconds{method="submitblock"}`. Run: counts 251 with `generate`. | `rpc_request_duration_seconds{method="submitblock"}`: parse to commit or 30 s. | CLOSE for the RPC metric: both calls return after the commit. | None. |
| T20 | Reuse of the template at `submitblock` | Counters `mining_prepared_cache_hits`, `_misses` (`crates/zakura-consensus/src/block/prepared.rs:104`, `:85`). Run: 4 hits, 247 misses. | `hayai_prebuilt_commits_total{origin}`. | CLOSE. Zakura counts lookups; hayai counts commits from a prebuilt body. | Add a counter of `submitblock` blocks that did not use the prebuilt body. |

### Sync phase

| # | Quantity | Zakura source and definition | hayai source and definition | Verdict | Smallest change on hayai |
|---|---|---|---|---|---|
| S1 | Committed height over time | `zcash_chain_verified_block_height` (T16). | Same name. | CLOSE (delegated) | None. |
| S2 | Blocks for each second | `rate(zcash_chain_verified_block_total[1m])`. | Same. | CLOSE (delegated) | None. |
| S3 | Header height | Legacy stack: none; the legacy syncer has no header chain. Native stack: `sync_header_chain_frontier_header_best_height` (`crates/zakura-state/src/service/finalized_state/header_chain.rs:705`, run with `dual`). | `hayai_sync_header_height`. | HAYAI-ONLY with the legacy stack | Cannot be made comparable without a Zakura change (or the stack `dual`, which changes the sync method). |
| S4 | Downloaded blocks | Counter `sync_downloaded_block_count` (`crates/zakurad/src/components/sync/downloads.rs:687`, source only: the legacy syncer downloaded no block on Regtest); `gossip_downloaded_block_count` (`inbound/downloads.rs:703`, run). | None. Trace row `block_received` only. | ZAKURA-ONLY | Add `sync_downloaded_block_count` at `Action::Store` (`sync.rs:609`). |
| S5 | Height on disk | Gauge `state_finalized_block_height` (`crates/zakura-state/src/service/finalized_state/zakura_db/metrics.rs:48`): the block that the node writes to RocksDB. During checkpoint sync each block goes to disk. At the tip it is the tip minus 1,000. Run. | Same name: height of the in-memory base, not of the disk. | CLOSE. The hayai value is not on disk; the bias is toward hayai. | Set `state_finalized_block_height` at the flush (`node.rs:811-838`) to the flushed height. Move the in-memory base to `hayai_base_height`. |
| S6 | Checkpoint progress | Gauges `checkpoint_verified_height`, `checkpoint_processing_next_height`, `checkpoint_queued_max_height` (`crates/zakura-consensus/src/checkpoint.rs:400`, `:573`, `:645`); `state_checkpoint_finalized_block_height` (`finalized_state.rs:728`). Run: present, value 0 on Regtest. | None. `apply_class="checkpoint"` in the trace. | ZAKURA-ONLY | Not needed: both nodes use the same checkpoint list. Derive "time to the last checkpoint" from S1 with a recording rule. |
| S7 | Time to the last checkpoint | First time with `zcash_chain_verified_block_height >= H_last_checkpoint`. | Same expression. | CLOSE (delegated), same as S1 | None. |
| S8 | Time to the tip | `sync_estimated_network_tip_height`, `sync_estimated_distance_to_tip` (`crates/zakurad/src/components/sync/progress.rs:144-145`): estimate from the time of the tip block and the clock. Run. Log line `activating mempool`. | No estimate. | ZAKURA-ONLY | Add `sync_estimated_network_tip_height` with the estimator of Zakura (tip height + (now - tip block time) / target spacing). Or apply the Zakura gauge to both nodes in a recording rule. |
| S9 | Bytes downloaded | Counters `zcash_net_in_bytes_total`, `zcash_net_out_bytes_total` (`crates/zakura-network/src/protocol/external/codec.rs:434`, `:173`): all legacy messages. Run. Native stack: `sync_block_payload_received_bytes`. | None. | ZAKURA-ONLY | Add `zcash_net_in_bytes_total` and `zcash_net_out_bytes_total` at the message codec. |
| S10 | Peers | Gauge `zcash_net_peers` = ready + unready legacy peer services (`crates/zakura-network/src/peer_set/set.rs:1638-1644`). Native connections are in `zakura_p2p_conn_active`. Run. | `zcash_net_peers` = entries of the relay, each 1 s. | CLOSE (delegated). hayai can count a peer before its handshake ends. | Count only established peers. |
| S11 | Download queue | Gauge `sync_downloads_in_flight` (`crates/zakurad/src/components/sync.rs:2929`): tasks of the legacy syncer in download or in verification. Parts: `sync_downloads_waiting_network`, `_downloading`, `_response_received`, `_waiting_verifier`, `_verifying`. Run. | `sync_downloads_in_flight` = requests without an answer. | CLOSE. Zakura includes the blocks in verification; hayai does not. | Set the gauge to requested + held blocks. Add `sync_downloads_downloading` and `sync_downloads_verifying`. |
| S12 | Verify time for each block in sync | Summary `sync_block_verify_duration_seconds{result}`. Start `sync/downloads.rs:856`, stop `:884`: verifier call to commit response, with the wait for the parent and for the checkpoint range. Source only. | Histogram with the same name = validation time only, without queue and without commit. | CLOSE, with a large bias toward hayai. | Observe the interval "body in hand to commit finished" under this name, or rename the hayai metric to `hayai_validate_duration_seconds`. |
| S13 | Download time for each block | Summary `sync_block_download_duration_seconds{result}` (`sync/downloads.rs:623-689`). Source only. | None. | ZAKURA-ONLY | Add the same summary or histogram: request sent to body parsed. |
| S14 | Disk used | Gauge `zakura_state_rocksdb_total_disk_size_bytes` (`crates/zakura-state/src/service/finalized_state/disk_db.rs:755`). Run. | None. | ZAKURA-ONLY | Use `node_filesystem_avail_bytes` of a dedicated volume on both machines. |
| S15 | CPU, memory, disk I/O | None in `/metrics`. | `process_*` for CPU and memory. | HAYAI-ONLY | Use node_exporter and the cgroup of the service on both machines. |

## Per-block comparison with Prometheus only

Prometheus alone does not give "block received to block committed" on Zakura.

Quantities that Prometheus gives for each block on Zakura:

| Quantity | Recipe | Error |
|---|---|---|
| Commit moment | The scrape at which `state_contextual_total_duration_seconds_count` (or `zcash_chain_verified_block_total`) increases. | 0 s to 5 s late. |
| Height | `zcash_chain_verified_block_height` at the same scrape. | The gauge is set later than the summary in the same loop pass (`write.rs:1485`, then `:3176`). A scrape between the 2 points shows the height of the block before. Use the height of the next scrape. |
| Contextual commit time (T4) | `increase(state_contextual_total_duration_seconds_sum[10s]) / increase(state_contextual_total_duration_seconds_count[10s])`. With one block in the window this is the exact value of that block. | The extrapolation factor of `increase` is the same in both terms. With a block spacing of 75 s on average, 2 blocks arrive in the same 5 s interval for about 6 % of the blocks; the value is then the mean of the 2 blocks. |
| Same value from the quantile | `state_contextual_total_duration_seconds{quantile="1"}`. The window is 3 buckets of 20 s (metrics-exporter-prometheus 0.16.2, `distribution.rs:14-18`). With one block in the window, quantile 0 = quantile 1 = that block. | The value stays for 40 s to 60 s, then becomes 0 (run: count 1, quantile 0). Do not chart the quantile as a time series of blocks. |

The recipe with 75 s spacing was not run to its end. The facts behind it are from runs:
cumulative `_sum` and `_count`, quantile 0 for an empty window.

Recipe for "block received to block committed" on a Zakura node with the legacy stack:

1. Set `[network.zakura] trace_dir` and write the log to a file (`[tracing] log_file`).
2. For each block, read `ts` of `block_request_finish` with `result = "available"` in
   `legacy_peer_request.jsonl`, key `returned_hash`.
3. Read the time of the log line `downloaded and verified gossiped block` with the same
   hash in the span field `download_and_verify{hash=...}`.
4. Convert `ts` to wall-clock time with the calibration of the next section.
5. Interval = log time - converted `ts`.

Error on the Regtest run (42 blocks): 0.8 ms for the calibration, plus the write time of
the log line. The log timestamp has a resolution of 1 µs.

## Trace clocks of Zakura

- `ts` is in µs from the creation of the emitter (`crates/zakura-jsonl-trace/src/lib.rs:144`,
  `:173`). Each table group has its own emitter and its own zero.
- `process_trace_id` is `<pid>-<unix ns>`. The time is the wall clock at the first row of
  the process (`lib.rs:262-272`). Run: it is equal to the log time of the first
  `starting sync, obtaining new tips` line within 11 µs.
- `legacy_sync`: each `round_start` row has a log line `starting sync, obtaining new tips`
  at the same code point (`crates/zakurad/src/components/sync.rs:1469`, `:1478`). Run: 101
  pairs, spread 23 µs. Offset = log time - `ts`.
- `legacy_peer_request`: no log line at the same point. Bound the offset with 2 rules: each
  `find_blocks_finish` lies between `round_start` and `tips_obtained` of one round, and each
  `block_request_finish` is before its log line. Run: the window of the offset is 0.8 ms.
- The label `node` comes from `HOSTNAME` or `/etc/hostname`. The variable `ZEBRA_NODE_ID`
  that the source names stops the node at its start: the configuration loader reads it as an
  unknown key `node_id` (run).

## CPU for each block

Zakura has no process metric, so the method must be external and the same for both nodes.

1. Run each node as the only workload of its machine, as a systemd service.
2. Export the CPU time of the cgroup of the service: a textfile collector of node_exporter
   that reads `usage_usec` of `/sys/fs/cgroup/system.slice/<unit>.service/cpu.stat` each
   1 s. Without it, use `sum(rate(node_cpu_seconds_total{mode!~"idle|iowait|steal"}[...]))`
   of the machine.
3. For each block, take the commit scrape `t` (section above). CPU of the block =
   increase of the counter over `[t - 10 s, t + 10 s]` minus the base rate × 20 s. The base
   rate is the median rate of the windows without a block in the last 30 min.

Error sources: the 5 s scrape places the block inside the window with an error of 5 s; the
mempool work of the same window is in the value; for a small block the value is below the
noise of the base rate. The value is reliable only as a mean over many blocks or for large
blocks.

## Zakura configuration for the race

| Setting | Value | Reason |
|---|---|---|
| `network.p2p_stack` | `"legacy"` (the Mainnet default) | hayaid is a legacy peer. With `dual`, the commit rows cover only the blocks that a Zakura peer delivers first (run). `dual` also changes the sync method (native header and body sync). |
| `[metrics] endpoint_addr` | set | The default is off (`crates/zakurad/src/components/metrics.rs:79-83`). |
| `[network.zakura] trace_dir` | set | Gives `legacy_sync.jsonl` and `legacy_peer_request.jsonl`. |
| `[tracing] log_file` | set, filter `info` | The commit moment of a gossiped block is only in the log. |
| `HOSTNAME` | set | Label `node` of the trace rows. |

Performance effect of the trace: the emitter reserves a slot in a bounded queue (16,384
rows) and builds the row only with a slot; a full queue drops the row; one writer task
flushes each 1 s (`crates/zakura-jsonl-trace/src/lib.rs:22-46`, `:163-185`). At the tip the
legacy stack writes about 5 rows for each block and 4 rows for each sync round (run). During
the initial sync it writes `block_phase`, `block_downloaded` and `block_finish` rows for
each block (source only). The overhead was not measured. To keep the race fair, enable the
trace on hayaid too.

Observation on Regtest with `dual` (run): a follower received the blocks of a Zakura peer
in groups each 30 s. A hayaid peer of a `dual` node was disconnected several times with
`the peer of the header sync does not answer`. The cause is not known.

## External probe

Goal: "block seen to template ready" for both nodes with one method and one clock.

Design:

1. The probe is one process on a third machine with the same network distance to both
   nodes. It holds, for each node, 2 legacy P2P connections (A and B) and one RPC
   connection.
2. Each node has the probe as its only source of new blocks, or the probe is one peer
   among others and only the blocks that it delivers first are counted.
3. The probe receives a new block from the network. It sends `inv` on connection A of both
   nodes at the same time. Each node answers `getdata`. The probe writes the `block`
   message and records `t0` when the last byte is written, for each node.
4. On connection B the probe records `t_inv`: the arrival of the `inv` or `headers` of this
   block from the node. Zakura announces a block after its commit (log line
   `sending committed block broadcast`, `crates/zakurad/src/components/sync/gossip.rs:244`).
   hayaid announces a block to a legacy peer after its validation.
5. On the RPC connection the probe holds a `getblocktemplate` long poll. It records
   `t_tmpl`: the arrival of the response whose `previousblockhash` is the new block.
6. The probe then sends one `getblocktemplate` without `longpollid` and records its
   duration: the service time of a full template.

Results for each block and each node: `t_inv - t0` (seen to validated and announced),
`t_tmpl - t0` (seen to first template on the new tip), and the duration of step 6.

Error sources:

- Network delay between probe and node, 2 times. Measure it with `ping` messages and
  subtract it.
- The first template on a new tip is a coinbase-only template on both nodes (Zakura:
  `methods.rs:3186-3231`, source only; hayaid: `Empty` before `Full`). Step 6 gives the
  time of the full template.
- A node can omit the announcement to connection B. Whether Zakura announces to each ready
  peer was not verified.
- When another peer delivers the block before the probe, the node sends no `getdata` and
  the block has no `t0`.
- Zakura reads the mempool again only each 5 s during a long poll
  (`MEMPOOL_LONG_POLL_INTERVAL`, source only). This affects template updates without a tip
  change, not `t_tmpl`.
- TCP buffering: `t0` is the end of the write on the probe, not the end of the read on the
  node.

## Additions on hayai

Metrics:

| Name | Type | Definition |
|---|---|---|
| `hayai_block_receive_to_commit_seconds` | histogram | Body parsed on the reader thread to `TipWatch::set` (T1). |
| `hayai_contextual_commit_duration_seconds` | histogram | `context` + `trees` + `history` + chain push (T4). |
| `hayai_shielded_verify_seconds_total{context}` | counter | Seconds of shielded verification, for blocks and for the mempool (T6). |
| `hayai_block_shielded_bundles_total{reused}` | counter | Bundles of block transactions with and without a prepared result (T8). |
| `sync_downloaded_block_count` | counter | Bodies stored by the block sync (S4). |
| `sync_block_download_duration_seconds` | histogram | Request sent to body parsed (S13). |
| `zcash_net_in_bytes_total`, `zcash_net_out_bytes_total` | counter | Bytes of all P2P messages (S9). |
| `sync_estimated_network_tip_height` | gauge | Estimator of Zakura (S8). |
| `sync_downloads_downloading`, `sync_downloads_verifying` | gauge | Parts of the download queue (S11). |
| `hayai_base_height` | gauge | Height of the in-memory base (S5). |

Changes of existing metrics:

| Metric | Change |
|---|---|
| `state_finalized_block_height` | Set at the flush to the flushed height (S5). |
| `sync_block_verify_duration_seconds` | Body in hand to commit finished, or rename (S12). |
| `sync_downloads_in_flight` | Requested + held blocks (S11). |
| `zcash_net_peers` | Established peers only (S10). |
| `hayai_mempool_rejected_total` | Count the RPC path (T15). |

Trace rows:

| Row | Change |
|---|---|
| `block_received` | Field `received_unix_us`: the arrival time on the reader thread after the parse. |
| `commit_finish` | Field `received_to_commit_us`. |
| `mempool_admit` (new) | `txid`, `source` (`rpc`, `peer`), `result`, `elapsed_us`. For the hayai dashboard only. |

Tools:

- `scripts/join_traces.py`: read `legacy_peer_request.jsonl` and the Zakura log, with the
  calibration of this document.
- The probe of the section "External probe".
- A textfile collector for the cgroup CPU time, the same on both machines.

## Dashboards

hayai node:

- Heights: tip, header, base, flushed; blocks for each second; download queue; held bytes.
- Per block: `hayai_commit_duration_seconds`, the 10 validation stages, prepared store hits
  and misses, template latency (`empty`, `full`), prebuilt commits.
- Mempool: size, bytes, rejects. RPC: rate, errors, latency by method.
- Relay counters, shadow counters, trace rows dropped, `process_*`.

Zakura node:

- Heights: `zcash_chain_verified_block_height`, `state_finalized_block_height`,
  `checkpoint_verified_height`, `sync_estimated_network_tip_height`.
- Rates: `zcash_chain_verified_block_total`, `sync_downloaded_block_count`,
  `gossip_downloaded_block_count`, `gossip_verified_block_count`.
- Per block: `state_contextual_total_duration_seconds` and its parts,
  `state_block_writer_queue_duration_seconds`, `zakura_consensus_batch_duration_seconds`,
  `zakura_consensus_cache_hit` and `_miss`, `zakura_state_rocksdb_batch_commit_duration_seconds`.
- Mining: `mining_*` summaries and counters, `rpc_request_duration_seconds` by method.
- Mempool: `zcash_mempool_size_*`, `mempool_*` counters.
- Network: `zcash_net_peers`, `zcash_net_in_bytes_total`, `zcash_net_out_bytes_total`,
  `zcash_net_in_messages`, `zcash_net_out_messages`.
- Database: `zakura_state_rocksdb_*`.
- Use `_sum / _count` for each duration. A `histogram_quantile` expression gives no data.

Comparison (rows of the matrix with the verdict CLOSE):

| Panel | Rows | Label of the difference |
|---|---|---|
| Height and blocks for each second | S1, S2 | none |
| Time to the last checkpoint, time to the tip | S7, S8 | S8 uses the estimate of Zakura for both |
| Height on disk | S5 | after the hayai change only |
| Peers | S10 | Zakura counts legacy peers only |
| Download queue | S11 | after the hayai change only |
| Bytes received | S9 | after the hayai change; else node_exporter |
| Block received to committed, by height | T1 | from traces and log, not from Prometheus |
| Contextual commit time | T4 | hayai excludes the chain push until the change |
| Shielded verification seconds | T6 | sums only |
| Mempool reuse ratio | T8 | bundle against transaction |
| Mempool size | T12 | none |
| `getblocktemplate` and `sendrawtransaction` mean service time | T10, T13 | means only; no long poll |
| Probe: seen to announced, seen to template | section "External probe" | same method |
| CPU, memory, disk, I/O | T18, S14, S15 | node_exporter and cgroup |

## Recording rules

```
# Mean of a Zakura summary or a hayai histogram over 1 min, same expression for both.
- record: node:rpc_request_duration_seconds:mean1m
  expr: rate(rpc_request_duration_seconds_sum[1m]) / rate(rpc_request_duration_seconds_count[1m])
- record: node:blocks_per_second:rate1m
  expr: rate(zcash_chain_verified_block_total[1m])
# Zakura: contextual commit time of the last block.
- record: zakura:contextual_commit_seconds:last
  expr: increase(state_contextual_total_duration_seconds_sum[10s]) / (increase(state_contextual_total_duration_seconds_count[10s]) > 0)
# Distance to the tip with the estimate of Zakura for both nodes.
- record: node:distance_to_tip:blocks
  expr: scalar(max(sync_estimated_network_tip_height)) - zcash_chain_verified_block_height
# Unix time at which a node passes the last checkpoint height H.
- record: node:last_checkpoint_reached:timestamp
  expr: min_over_time((timestamp(zcash_chain_verified_block_height >= H))[30d:1m])
# Reuse ratio. Zakura: bundles. hayai: transactions (bundles after the change).
- record: zakura:verification_reuse:ratio5m
  expr: sum(rate(zakura_consensus_cache_hit[5m])) / (sum(rate(zakura_consensus_cache_hit[5m])) + sum(rate(zakura_consensus_cache_miss[5m])))
- record: hayai:verification_reuse:ratio5m
  expr: rate(hayai_prepared_store_hits_total[5m]) / (rate(hayai_prepared_store_hits_total[5m]) + rate(hayai_prepared_store_misses_total[5m]))
# CPU of the node service from the cgroup textfile collector.
- record: node:service_cpu:rate1m
  expr: rate(node_service_cpu_usage_seconds_total[1m])
```

## Zakura metrics on the running node

Exported names: each dot of the source name becomes an underscore (run). The exporter
writes no `# HELP` line; the source has no `describe_*` call.

Legacy stack, pair with hayaid, 292 blocks and 41 transactions (172 families: 49 counters,
81 gauges, 42 summaries):

| Group | Families (type) |
|---|---|
| Chain and state | `zcash_chain_verified_block_height` (gauge), `zcash_chain_verified_block_total` (counter), `state_full_verifier_committed_block_height` (gauge), `state_full_verifier_committed_block_count` (counter), `state_memory_*` (15: gauges and counters), `state_finalized_*` (15), `state_checkpoint_*` (5), `state_requests{service,type}` (counter), `state_vct_fast_path_miss`, `state_vct_legacy_block_count` (counters) |
| Checkpoint verifier | `checkpoint_verified_height`, `checkpoint_verified_count`, `checkpoint_processing_next_height`, `checkpoint_queued_continuous_height`, `checkpoint_queued_max_height`, `checkpoint_queued_slots` (gauges), `checkpoint_verified_block_count`, `checkpoint_handoff_checked` (counters) |
| State commit durations | `state_contextual_*_duration_seconds` and `state_contextual_mined_*_duration_seconds` (26 summaries), `state_block_writer_queue_duration_seconds`, `state_block_writer_queue_mined_duration_seconds`, `state_semantic_commit_{dispatch,prequeue_checks,queue_and_commit,ready_wait}_duration_seconds` (summaries) |
| Proofs and signatures | `zakura_consensus_batch_duration_seconds{verifier,result}` (summary), `zakura_consensus_cache_{hit,miss,insert}` (counters), `zakura_consensus_cache_size` (gauge), `proofs_halo2_verified` (counter) |
| Sync and gossip | `sync_downloads_in_flight`, `sync_downloads_{waiting_network,downloading,response_received,waiting_verifier,verifying}`, `sync_prospective_tips_len`, `sync_reserve_depth`, `sync_obtain_queued_hash_count`, `sync_estimated_network_tip_height`, `sync_estimated_distance_to_tip`, `sync_zakura_*` (4), `gossip_queued_block_count` (gauges), `sync_stage_duration_seconds{stage}` (summary), `gossip_downloaded_block_count`, `gossip_verified_block_count` (counters) |
| Network | `zcash_net_peers` (gauge), `zcash_net_in_bytes_total`, `zcash_net_out_bytes_total`, `zcash_net_in_messages{command}`, `zcash_net_out_messages{command}`, `zcash_net_peers_connected{...}`, `zakura_net_{in,out}_{requests,responses}{command}`, `zakura_net_out_requests_canceled`, `peer_canceled`, `pool_route_inv_advertiser_count` (counters), `zcash_net_peers_version_connected`, `zakura_net_connection_state{command}`, `pool_num_ready`, `pool_num_unready`, `candidate_set_*` (6), `crawler_in_flight_handshakes` (gauges), `zcash_net_peer_handshake_duration_seconds{result}` (summary) |
| Mempool | `zcash_mempool_size_transactions`, `zcash_mempool_size_bytes`, `zcash_mempool_cost_bytes`, `zcash_mempool_actions_paid`, `zcash_mempool_actions_unpaid{bk}`, `zcash_mempool_size_weighted{bk}`, `mempool_currently_queued_transactions`, `mempool_rejected_transaction_ids`, `mempool_rejected_transaction_ids_bytes` (gauges), `mempool_{queued,downloaded,pushed,verified,gossiped,gossip_pending}_transactions_total` (counters) |
| Mining | `mining_{preparation,solved_header_check,prepared_relay_preflight,contextual_commit,state_admission}_duration_seconds` (summaries), `mining_prepared_cache_{hits,misses,evictions}`, `mining_template_preparation_{cancelled,coalesced}` (counters) |
| RPC | `rpc_request_duration_seconds{method}` (summary), `rpc_requests_total{method,status}`, `rpc_errors_total{method,error_code}` (counters), `rpc_active_requests` (gauge) |
| Database | `zakura_state_rocksdb_batch_commit_duration_seconds` (summary, one sample for each finalized block), `zakura_state_rocksdb_{total_disk_size,live_data_size,total_memory_size,block_cache_usage,compaction_pending}_bytes`, `zakura_state_rocksdb_compaction_running`, `zakura_state_rocksdb_cf_disk_size_bytes{cf}`, `zakura_state_rocksdb_cf_memory_size_bytes{cf}`, `zakura_state_rocksdb_num_files_at_level{level}` (gauges) |
| Process and build | `zakura_build_info{version}` (counter), `end_of_support_enforced` (gauge). No `process_*` family. |

Sample values at the end of the legacy run: `zcash_chain_verified_block_height 292`,
`gossip_verified_block_count 41`, `state_contextual_total_duration_seconds_count 292` with
sum 0.2036 s, `mining_preparation_duration_seconds_count 274` with sum 0.334 s,
`zcash_net_in_bytes_total 238331`, `zcash_mempool_size_transactions 1`,
`sync_estimated_network_tip_height 6590382` (the estimate uses the clock, so it has no
meaning on Regtest).

Stack `dual` adds 85 families (run), all of the native stack: `sync_block_*`,
`sync_header_*`, `sync_header_chain_*`, `sync_report_{checkpoint,sapling,ironwood}_height`,
`zakura_p2p_*`, `state_header_*`, `sync_block_first_received_count{source}`,
`gossip_zakura_native_sync_block_hash_count`.

Names of the source that appeared in no run: 230 of 487. The ones that matter for the race:

| Name | Reason |
|---|---|
| `sync_block_verify_duration_seconds`, `sync_block_download_duration_seconds`, `sync_downloaded_block_count`, `sync_verified_block_count` | The legacy syncer downloaded no block on Regtest; the blocks came by gossip. Expected in the Mainnet initial sync (source only). |
| `mining_template_rebuilt`, `mining_template_preparation_{rejected,timed_out}`, `mining_prepared_cache_mismatches` | No such event. |
| `mempool_rejected_transactions_total`, `mempool_failed_verify_tasks_total` | No reject in the runs. |
| `signatures_*`, `proofs_sapling_*` | No such transaction in the runs. |
| `zakura_state_write_block_tx_count`, `zakura_state_write_checkpoint_compute_duration_seconds` | Feature `commit-metrics`, not in the default build. |
| `sync_block_payload_committed_bytes` | Feature `sync-metrics`, not in the default build. |
| `end_of_support_remaining_blocks`, `end_of_support_last_supported_height` | Not set on Regtest. |
| `zcashd_compat_supervisor_*` | Component not enabled. |

Files of the runs, below `target/zakura-measure/`: `pair-g/scrape/zakura-last.prom`
(legacy), `dual2/scrape/zakura-last.prom` and `dual2/scrape/hayai-last.prom` (the 2 `dual`
nodes), `pair-g/g/zakura-trace/`, `dual2/z1-trace/`, `dual2/z2-trace/`.

## Zakura trace tables

Common fields of each row: `ts`, `node`, `process_trace_id`, `event`.

| Table | Written with | Events (run unless marked) |
|---|---|---|
| `legacy_sync` | `trace_dir`, legacy syncer (`crates/zakurad/src/components/sync/legacy_trace.rs:14`) | `round_start` (`state_tip`), `tips_obtained` (`reserve`, `prospective_tips`), `round_finish` (`reason`, `state_tip`). Source only: `tips_extended`, `block_downloaded` (`hash`, `height`, `download_elapsed_ms`, `peer`), `block_phase` (`hash`, `phase`, `previous_phase`, `height`, `phase_elapsed_ms`, `elapsed_ms`), `block_finish` (`hash`, `height`, `result`, `error`, `phase`, `elapsed_ms`, `phase_elapsed_ms`), `pipeline_reset`, `round_error_snapshot`, `round_stalled` |
| `legacy_peer_request` | `trace_dir`, legacy peer set (`crates/zakura-network/src/peer_set/legacy_peer_trace.rs:30`) | `find_blocks_finish` (`request_id`, `peer_id`, `peer`, `peer_start_height`, `local_tip_height`, `elapsed_ms`, `locator_tip`, `result`, `hash_count`, `inferred_start_height`, `inferred_end_height`), `block_request_finish` (`request_id`, `peer_id`, `peer`, `peer_start_height`, `local_tip_height`, `elapsed_ms`, `requested_hash`, `route`, `result`, `returned_hash`, `returned_height`) |
| `commit_state` | `trace_dir`, native stack (`crates/zakura-network/src/zakura/trace.rs:130`; emit in `crates/zakurad/src/commands/start/zakura/trace/block_driver.rs`) | `action_received`, `state_read_start`, `state_read_success`, `block_submit_queued`, `commit_start` (`height`, `hash`, `apply_token`, `apply_class`), `commit_finish` (+ `result`, `elapsed_ms`), `reactor_event_sent`. Source only: `state_read_error`, `state_read_timeout`, `commit_stalled` |
| `block_sync` | `trace_dir`, native stack (`trace.rs:126`) | 25 events in the run, among them `block_body_received` (`peer`, `height`, `serialized_bytes`, `request_elapsed_ms`), `block_body_submitted`, `block_apply_finished`, `block_frontiers_changed`, `block_sync_state`. 33 event names in the source (`trace.rs:306-376`) |
| `header_sync` | `trace_dir`, native stack (`trace.rs:43`) | `header_status_sent`, `header_status_received`, `header_request_sent`, `header_response_received`, `header_outcome`, `header_target_admitted`, `header_request_terminal`, `header_snapshot_observed`, `header_peer_connected`. 14 event names in the source (`trace.rs:105-118`) |
| `handshake`, `conn`, `stream`, `discovery`, `legacy_request` | `trace_dir`, native stack | `control.started`, `control.succeeded`; `accepted`, `duplicate`, `closed.neutral`; `accepted`; `discovery_dial_result`; `outbound.request`, `outbound.response`, `inbound.request`, `inbound.response` |
| `ratelimit`, `queue_send` | `trace_dir`, native stack | No row in the runs (source only) |

A row of the syncer for each block (`block_downloaded`, `block_phase`, `block_finish`)
exists only for a block that the legacy syncer downloads. A block at the tip comes by
gossip and gets only the `block_request_finish` row.

## Dashboards and alerts of Zakura

Source: `docker/observability` (source only). Scrape interval 15 s. No node_exporter.

| Dashboard | Metrics on the charts |
|---|---|
| Overview | `state_finalized_block_height`, `zcash_net_peers`, `rate(zcash_chain_verified_block_total)`, `sync_estimated_distance_to_tip`, `zcash_net_in_bytes_total`, `zcash_net_out_bytes_total`, `zcash_mempool_size_*`, `proofs_halo2_verified`, `zakura_state_rocksdb_total_disk_size_bytes`, RPC error ratio and p99, value pools, `end_of_support_remaining_blocks`, `process_*` |
| Block verification | `zcash_chain_verified_block_height`, `state_full_verifier_committed_block_height`, `state_checkpoint_finalized_block_height`, `state_memory_queued_*`, `state_memory_sent_block_height`, rates of `sync_downloaded_block_count`, `sync_verified_block_count`, `gossip_downloaded_block_count`, `gossip_verified_block_count` |
| Checkpoint verification | `checkpoint_*` heights and queues, `state_checkpoint_queued_*`, `sync_prospective_tips_len`, `sync_downloads_in_flight` |
| Syncer | `sync_downloaded_block_count`, `sync_verified_block_count`, `sync_downloads_in_flight`, `sync_obtain_*`, `sync_extend_*`, `sync_stage_duration_seconds`, `sync_block_download_duration_seconds{quantile="0.95"}`, `sync_block_verify_duration_seconds{quantile="0.95"}` |
| Transaction verification | `signatures_*_validated`, `proofs_*_verified`, `state_finalized_cumulative_*`, `zakura_consensus_batch_duration_seconds` |
| Mempool | `zcash_mempool_size_*`, `mempool_currently_queued_transactions`, `mempool_rejected_transaction_ids*`, `mempool_*_transactions_total` |
| Network, peers, messages | `zcash_net_*`, `pool_num_*`, `candidate_set_*`, `zakura_net_*` |
| RocksDB | `zakura_state_rocksdb_*` |
| RPC | `rpc_active_requests`, `rpc_requests_total`, `rpc_errors_total`, `rpc_request_duration_seconds_bucket` |
| Value pools | `state_finalized_value_pool_*`, `state_finalized_chain_supply_total` |

Alerts: node down, `changes(zcash_chain_verified_block_height[15m]) == 0`,
`zcash_net_peers < 3` and `== 0`, RPC latency and error ratio, handshake failures, value
pool below 0, end of support.

Facts against the running node:

- The RPC dashboard and 2 alerts use `rpc_request_duration_seconds_bucket` and
  `zcash_net_peer_handshake_duration_seconds_bucket`. The node exports summaries, so no
  `_bucket` series exists (run).
- The overview uses `process_*` and `zakurad_build_info`. The node exports no `process_*`
  family and the name `zakura_build_info` (run).
- No dashboard and no alert uses a `mining_*` metric, a per-block duration or a block
  propagation metric.

## Ground truth runs

All nodes on Regtest, 127.0.0.1, below `target/zakura-measure/`.

| Run | Nodes | Content |
|---|---|---|
| `pair-g` | `zakurad` (legacy) + `hayaid`, scenario g of the pair harness, 3 min | 292 blocks (251 from `generate` on zakurad, 41 from hayaid by gossip), 41 transactions, `/metrics` each 5 s |
| `dual2` | 2 `zakurad` (`dual`), then 1 `hayaid` as legacy peer of the second one | 12 blocks by native sync, 5 blocks by legacy gossip |
| `tip75` | `zakurad` (legacy) + `hayaid` | Stopped after 3 blocks; the 75 s spacing was not run |

## Not verified

- Each row marked "source only".
- The behaviour of the legacy syncer metrics and trace rows in a Mainnet initial sync.
- The Prometheus recipe with a real spacing of 75 s.
- The overhead of the trace and of the log file.
- OpenTelemetry: the span `download_and_verify{hash,source}` (info level,
  `inbound/downloads.rs:523`) covers download, verification and commit of one gossiped
  block and is in the release build (source only). No collector was run.
- Whether Zakura announces a new block to each ready peer (probe, step 4).
- The hayai definitions: read by a second reader; the file `crates/hayaid/src/node.rs`
  changed during this work.
