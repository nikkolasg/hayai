# First Mainnet sync and comparison at the tip: zakurad and hayaid

Date: 2026-10-05. Zakura: 1.6.0, commit `13779158253cfe315f73eadffb9b4c93c25e82a5`.
hayaid: the commit of the checkout that deploys. Mainnet: the default build (the upstream
Zcash crates). Testnet: the zakura crypto backend (NU7 rules).

## Purpose

One `zakurad` and one `hayaid` run on two equal machines. A third machine records both.
The nodes only observe the network: no miner uses them, and they hold no funds. An RPC
caller on each node machine calls `getblocktemplate` of its node.

```
 machine A                 machine B                 machine C (small)
 zakurad       :8233 P2P   hayaid        :8233 P2P   Prometheus (loopback)
 RPC caller (loopback)     RPC caller (loopback)     Grafana :3000
 node-exporter             node-exporter
   :9999 :9100  <──────────  :19101 :9100  <──────── scrape each 5 s
```

Files: `docker/race/` (compose files, node configurations, Prometheus rules, dashboards),
`deploy/terraform/aws-race/`, `scripts/race_deploy.sh`, `scripts/race_rpc_caller.py`,
`scripts/race_blocks.py`.
`docs/zakura-measurements.md` has the definition of each metric of both nodes.

The network is one variable: `network` in Terraform, `RACE_NETWORK` for the script and
the compose files. The default is `mainnet`. The other value is `testnet`.

## Phases

1. First sync. Both nodes start in the same minute with empty data directories and
   synchronize from the genesis block to the tip. Results: the time to the last
   checkpoint (Mainnet: block 3,499,045; Testnet: block 4,023,200), the time to the tip,
   and the CPU, memory, disk and network use of each machine.
2. Tip. Both nodes stay at the tip for 24 hours or more (about 1,150 blocks at a block
   spacing of 75 s). The comparison is for each block, by block height: the time inside
   each node from the reception of the block to its commit, and the contextual commit
   time. The arrival time of a block is not compared: each node has other peers.

## Compared quantities

Verdict CLOSE: both nodes measure the same work with the stated difference. No quantity
has the verdict SAME. The comparison dashboard and `blocks.csv` have no other pair.

| Quantity | zakurad | hayaid | Verdict and difference | Place |
|---|---|---|---|---|
| Block height | `zcash_chain_verified_block_height`, set after the commit response | Same name, set at the commit | CLOSE. Same event: the tip after a commit | Dashboard |
| Blocks for each second | `rate(zcash_chain_verified_block_total[1m])` | Same | CLOSE | Dashboard |
| Time to the last checkpoint, time to the tip | First rule evaluation with the height at the checkpoint, or 2 blocks or less below the reference height | Same rule | CLOSE. Resolution 5 s. One reference for both nodes: the highest of the header chain of hayaid and of the network tip that zakurad estimates from the time of its tip block | Dashboard |
| Peers | `zcash_net_peers`: ready and not ready peers of the legacy stack | Same name: peers after the handshake | CLOSE | Dashboard |
| Height on disk | `state_finalized_block_height`: the newest block in RocksDB | Same name: the newest block whose coins are in the coins store on disk. The block files have each block | CLOSE. hayaid flushes each 100 blocks by default; zakurad writes each finalized block | Dashboard |
| P2P bytes | `zcash_net_in_bytes_total`, `zcash_net_out_bytes_total`: header and body of each message at the codec | Same names: bytes that a reader took from its socket, and bytes of each frame written in full | CLOSE | Dashboard |
| Download queue | `sync_downloads_in_flight`: tasks in download or in verification | Same name: requests without an answer plus downloaded blocks that wait for the validator | CLOSE | Dashboard |
| Contextual commit time, for each block | `state_contextual_total_duration_seconds`: initial contextual checks, then the commit to the non-finalized state (transparent spends, anchors, note commitment trees, chain push, with a clone of the chain) | `hayai_contextual_commit_duration_seconds`: stages `context`, `trees`, `history`, then the push of the layer on the chain | CLOSE. The split of the checks between this interval and the earlier validation is not the same on both nodes | Dashboard, `blocks.csv` |
| Block received to committed, for each block | No metric. Start: trace row `block_request_finish` (the peer service returns the decoded block). Stop: log line `downloaded and verified gossiped block` (after the commit to the state in memory) | Trace field `commit_finish.received_to_commit_us`. Start: arrival of the `block` message, before the parse. Stop: the block is the tip, after the write to the block file and the update of the prepared store | CLOSE. hayaid has the parse and the block file write inside the interval; zakurad does not. The Zakura value has the error of a clock calibration, which `blocks.md` states | `blocks.csv`. The dashboard has hayaid only |
| Reuse of the mempool verification | `zakura_consensus_cache_hit` / (hit + miss): shielded bundles, block and mempool lookups | `hayai_prepared_store_hits_total` / (hits + misses): transactions of blocks | CLOSE. Unit: bundle against transaction. zakurad reuses no script result | Dashboard |
| Mempool size | `zcash_mempool_size_transactions` | Same name: the prepared store | CLOSE | Dashboard |
| RPC mean service time | `rpc_request_duration_seconds` (summary): sum / count | Same name (histogram): sum / count | CLOSE. Data only when a client calls a method: the RPC caller calls `getblocktemplate` each 5 s. `getblocktemplate` without `longpollid`: zakurad builds the template in the call; hayaid returns the template that it built at the tip change. With the long poll of the RPC caller the mean contains the wait of the long poll on both nodes: use the mean of `blocks.md` then | Dashboard, `blocks.md` |
| Template served after a block, for each block | No metric. Start: log line `downloaded and verified gossiped block`. Stop: the first `getblocktemplate` answer of the RPC caller with the block as `previousblockhash` | No metric. Start: `unix_us` of the trace row `commit_finish`. Stop: the same answer of its RPC caller | CLOSE. Same client and same stop on both machines. zakurad writes the log line after the commit, so its start is later, and a value can be below 0. Only a gossiped block has a value of zakurad. Without the long poll each value is late by 0 s to 5 s | `blocks.csv` |
| CPU, memory, disk, disk I/O, network | node-exporter of machine A | node-exporter of machine B | Same method. The values are of the machine, which runs one node | Dashboard |

Not compared, because zakurad has no such measurement (hayai dashboard only):

- Block received to block validated. zakurad has no event between the reception and the
  commit response.
- Block received to template ready, and tip change to template ready. zakurad builds a
  template only when a `getblocktemplate` request arrives.
- Script check time and the other validation stages.
- Header height: zakurad with the legacy stack has no header chain.
- CPU and memory of the process: zakurad exports no `process_*` metric.

Not compared, because hayaid has no such measurement (Zakura dashboard only): wait in the
queue of the block writer, heights of the checkpoint verifier, RocksDB metrics.

Not compared, because the definitions differ: shielded proof time (one sample for each
batch on zakurad, with the mempool; one sample for each block on hayaid).
`sync_block_verify_duration_seconds` has one definition on both nodes, but it was not
seen on a running zakurad (it comes from the legacy syncer in the first sync only), so no
panel has it.

"Template served after a block" starts at the commit of each node. For "block received
to template ready" with one start for both nodes, the next step is an external probe on
a third machine that gives each new block to both nodes and measures the return of a
`getblocktemplate` long poll (`docs/zakura-measurements.md`, External probe). The
package does not have it.

## Machine size

| Item | Value for A and B | Source |
|---|---|---|
| CPU and memory | 8 vCPUs, 32 GiB (`m7i.2xlarge`) | Zakura `docs/zcashd-compat.md`, lines 437 and 438: 4 CPUs and 16 GiB minimum, 8 CPUs and 32 GiB recommended |
| Data disk, Mainnet | gp3, 400 GiB, 3,000 IOPS, 125 MiB/s | Zakura `docs/zcashd-compat.md`, line 439: 275 GiB minimum for the data directory. Estimate: plus the image build (about 20 GiB) and a margin |
| Data disk, Testnet | gp3, 200 GiB | Same file, line 440: 30 GiB minimum. Estimate |
| Machine C | 2 vCPUs, 2 GiB (`t3.small`), 30 GiB | Estimate: 4 targets at one scrape each 5 s, 90 days |

The Zakura table is the hardware table of its zcashd-compat mode. Zakura has no other
hardware table.

hayaid on Mainnet, estimate, not measured:

- Memory. The default coins store keeps the coin set in memory. The model of
  `docs/architecture.md` gives 3.84 GB after a load (2.05 GB of coins, 1.79 GB of
  nullifiers), from a benchmark with 2 million coins. The node also holds the header
  chain from the genesis block, the layers of the last 1,000 blocks and the prepared
  store. The 32 GiB of the machine are a margin, not a measured need.
- Disk. hayaid stores each raw block one time in its block files, the header log, the
  state log and one snapshot of the coin set (3.67 GB in the same model). It has no
  transaction index and no address index. The estimate is a need below the 275 GiB of
  zakurad. Both machines have the same volume.
- Read "Machine memory in use" and "Disk in use" on the comparison dashboard in the
  first hours.

## Deployment with Terraform (AWS)

```sh
cd deploy/terraform/aws-race
cp terraform.tfvars.example terraform.tfvars    # region, network, repositories, hayai_ref, start_at, admin_cidr
terraform init
terraform apply
terraform output grafana_url
terraform output grafana_password_command
```

- A and B have the same instance type, the same gp3 volume, and the same subnet and
  availability zone. Each one builds its image on its first boot and pulls the image of
  the RPC caller (`zakura_ref` has the
  commit above as its default; `hayai_ref` must be a full commit hash). The network
  selects the crypto backend of the hayaid image.
- `rpc_caller = false` starts no RPC caller. `rpc_caller_interval` (default 5) and
  `rpc_caller_long_poll` (default `false`) are its two settings.
- `start_at` is the UTC minute of the start. Each node machine waits for it on its own
  clock (chrony). Set it 90 minutes or more after the apply (estimate of the build time).
- After `start_at`, run the value of `terraform output start_log_command` on A and on B.
  `late_seconds` must be 0 on both. If it is not, the run is not valid: destroy and apply
  again with a later `start_at`.
- Security groups: P2P (TCP 8233 on Mainnet, 18233 on Testnet) from each address; the
  metrics ports from machine C only; SSH and Grafana from `admin_cidr`; no RPC port.
  zakurad with the legacy stack opens no UDP port.
- The results: give the SSH user of each machine access to Docker
  (`sudo usermod -aG docker ubuntu`), then run on the machine of the operator:

```sh
RACE_DIR=/opt/race/hayai/docker/race scripts/race_deploy.sh collect \
  ubuntu@<zakurad address> ubuntu@<hayaid address> ubuntu@<monitor address>
```

  `terraform output public_ips` has the three addresses.

## Deployment with the script (any three hosts with Docker and SSH)

```sh
ZAKURA_SRC=~/src/zakura scripts/race_deploy.sh build      # Zakura checkout at the commit above
scripts/race_deploy.sh start   user@a.example user@b.example user@c.example
scripts/race_deploy.sh status  user@a.example user@b.example user@c.example
scripts/race_deploy.sh collect user@a.example user@b.example user@c.example
scripts/race_deploy.sh stop    user@a.example user@b.example user@c.example
```

- Testnet: set `RACE_NETWORK=testnet` for each command.
- `build` makes `zakurad:race` and `hayaid:race` on this machine. `start` copies each image
  to its host. `start` stops when the hayaid image has the crypto backend of the other
  network.
- `start` starts an RPC caller with each node, and `stop` stops both. Each node host
  pulls `python:3.13.7-alpine3.22` from the public registry. `RACE_CALLER=0` for `start`:
  no RPC caller. `RACE_CALLER_INTERVAL` and `RACE_CALLER_LONGPOLL` are its two settings.
- `start` stops when a host has data of a run. It starts both nodes at the second full
  minute after its last step, on the clock of each host: the hosts need NTP or chrony.
  `status` shows `planned_start_epoch` and `actual_start_epoch` of each node.
- `start` sets iptables rules on A and B that close ports 9999, 19101 and 9100 to each
  host but C. They need `sudo` without a password. Set `RACE_MONITOR_IP` when A and B see
  C under another address than the SSH target. Open the P2P port (TCP 8233 on Mainnet)
  on A and B in the firewall of the provider.
- Grafana: `http://c.example:3000`, user `admin`, password in
  `hayai-race/secrets/grafana_admin_password` on C.

## Node configuration

`docker/race/config/zakurad.<network>.toml` and `hayaid.<network>.toml`:

- Full sync from the DNS seeders of the defaults. The nodes are not peers of each other.
- The same peer limits: `peerset_initial_target_size = 25` (37 outbound and 75 inbound
  peers), `max_connections_per_ip = 1`.
- RPC on the loopback address with the cookie file. Metrics on 9999 (zakurad) and 19101
  (hayaid).
- zakurad: `p2p_stack = "legacy"` (the Mainnet default). hayaid is a legacy node, and the
  rows of the per-block comparison come from this stack.
- Both nodes write trace tables (`[network.zakura] trace_dir`). zakurad writes its log
  to a file of its data volume (`[tracing] log_file`), because the log has the commit
  time of each block at the tip. The cost of the trace and of the log file is not
  measured. Both nodes have a trace writer with a queue of 16,384 rows that drops a row
  when the queue is full.
- `miner_address` is the Mainnet test address of the Zakura source. No miner uses it.

## RPC caller

`scripts/race_rpc_caller.py` runs on each node machine in the service `zakurad-caller` or
`hayaid-caller` of `compose.node.yml`. Both services have the same program, image,
schedule and limits.

- Each 5 s (`RACE_CALLER_INTERVAL`) the caller sends one `getblocktemplate` call without
  `longpollid` to the RPC server of its node on the loopback address. It reads the cookie
  file of the node for each call, from the data volume of the node (read-only mount).
- For each call it writes one JSON line to `getblocktemplate.jsonl` in its volume: the
  wall clock of the machine at the return (`unix_us`), the time of the call on the client
  side (`duration_us`), and the `height`, the `previousblockhash` and the number of
  transactions of the template.
- An error is one line with `"ok": false` (`connection`, `http`, `rpc`, `timeout`,
  `cookie`, `answer`), and the caller continues: a node that is not ready, a node that
  refuses the call during the sync, a new cookie after a restart.
- The only method is `getblocktemplate`. The caller changes nothing in the node.
- Load on the node: one call each 5 s. Limits of the container: 0.5 CPU, CPU weight 64
  (the node has 1,024), 128 MiB. On the Regtest run the container used 23 MiB and less
  than 1 % of one CPU. The file grows by about 200 bytes for each call (about 3.5 MB for
  each day) and has no rotation.
- Result on the dashboards: the panels of `rpc_request_duration_seconds` for
  `getblocktemplate` have data for both nodes. Result in `blocks.csv`: the columns
  `hayai_template_served_s` and `zakura_template_served_s`. Result in `blocks.md`: the
  mean time of a call on the client side for each node.
- Long poll (`RACE_CALLER_LONGPOLL=1`, Terraform `rpc_caller_long_poll = true`; default
  off): the caller also holds one call with the `longpollid` of the last answer and the
  capability `longpoll`, on a second connection, and records its return (`"mode":
  "longpoll"`). The columns `*_template_served_s` then do not have the error of the call
  interval (Regtest run: 0.6 ms to 5.5 ms on both nodes). Both nodes count the wait of a long poll in
  `rpc_request_duration_seconds`, so the dashboard mean is then not the time of a call
  without `longpollid`. Other error sources: a long poll also returns when the template
  changes on the same block (the table reads the first answer on a block only); no long
  poll is open between a return and the next request; a step of the machine clock
  changes a value.
- Differences between the nodes that the caller hides: hayaid holds a call only with the
  capability `longpoll`, and zakurad holds it with the `longpollid` alone; hayaid
  answers a wrong cookie with the status 401, and zakurad closes the connection; hayaid
  removes its cookie file at its stop, and zakurad keeps it.
- To turn the caller off: `RACE_CALLER=0` for `start`, or `rpc_caller = false` in
  Terraform. On a running machine: `docker compose -f compose.node.yml --profile <node>
  stop <node>-caller` in the race directory.

## Dashboards

Grafana has three dashboards. The home dashboard is the comparison.

| Dashboard | Content | Variables |
|---|---|---|
| "zakurad and hayaid: comparison" (`comparison.json`) | The quantities of the table above only. Each panel description starts with the verdict and states the difference. Rows: Sync, Tip for each block, Resources, Compared quantities (the table) | `zakura`, `hayai`: the value of the label `node` of each node |
| "hayai node" (`hayai-node.json`) | Each metric of hayaid: chain, block download, the gauges of the last block, block duration histograms, mempool and template, relay, RPC, shadow mode, process, machine | `job`, `instance`, `machine` (node-exporter) |
| "Zakura node" (`zakura-node.json`) | The metrics that a running zakurad exports with the legacy stack: chain, checkpoint verifier, download queue, block commit, proofs, mempool, mining, RPC, network, RocksDB, machine. Each duration is a summary: the panels use sum / count | `job`, `instance`, `machine` |

Comparison dashboard, row Sync:

| Panel | Reading |
|---|---|
| Time to the last checkpoint | Seconds from the first scrape of the node to the last checkpoint. "not yet" before that |
| Time to the tip | Seconds to the first time that the block height is 2 blocks or less below the reference height |
| Block height, Blocks behind the other node, Blocks for each second | The progress |
| Height on disk, Peers, P2P bytes, Download queue, Mempool size | One line for each node |

The annotations mark the first scrape, the last checkpoint and the tip of each node.

Comparison dashboard, row Tip:

| Panel | Reading |
|---|---|
| Contextual commit time against block height (one panel for each node) | One point for each block. X: block height. A panel has one X field, and each node has its own height series |
| Contextual commit time against time | Both nodes, one point for each block |
| Block received to committed against block height: hayaid only | zakurad has no such metric. Its value is in `blocks.csv` |
| Last blocks: contextual commit time | Table, newest first, joined on the scrape time. The two height columns show when the nodes are at different blocks. Grafana cannot join two nodes on the height, because the height is a value and not a label. `blocks.csv` has the table that is joined on the height |
| Reuse of the mempool verification, RPC mean service time | One line for each node |

A panel of one value for each block reads the last sample of each step. With a time
range above some hours the step is above the block spacing, and the panel shows a part
of the blocks. Use a range of 2 hours or less for each block, or `blocks.csv`.

## Fairness rules

1. Same machine type, disk type and size, region and availability zone for A and B.
2. Both nodes start in the same minute. Record the planned and the actual start.
3. Empty data directories. `start` refuses a host with data.
4. The nodes are not peers of each other. Each one finds its peers with the DNS seeders
   of its defaults, with the same peer limits.
5. Both nodes write trace tables, so both have that cost.
6. Do the run two times, the second time with the machines exchanged: run `collect`,
   then `scripts/race_deploy.sh swap user@a.example user@b.example user@c.example`. It
   removes the data on the three hosts (it asks first) and starts zakurad on B and hayaid
   on A. With Terraform: `terraform destroy`, then a new apply, and compare the two runs.
7. Record the versions and commits of both nodes: `collect` writes `*-version.txt` and
   `*-info.txt` (image id, commit label, start times).
8. Differences that stay: zakurad writes its log to a file and hayaid to the output of
   its container. The sync methods differ (zakurad: legacy syncer with checkpoints;
   hayaid: header chain first, then blocks).

## Run time

- Phase 1 ends when "Time to the tip" has a value for both nodes. The time of a Mainnet
  sync on these machines is not measured for hayaid.
- Phase 2: 24 hours or more after that moment. Then run `collect`.
- Prometheus keeps 90 days. `collect` can run more than one time.

## Results of collect

`scripts/race_deploy.sh collect A B C` writes `race-results/<UTC time>/`:

| File | Content |
|---|---|
| `blocks.csv` | One row for each block height from the tip phase on. Columns: `height`, `hash`, source, transactions and bytes of the block of hayaid, `hayai_received_to_validated_s`, `hayai_received_to_committed_s`, `zakura_received_to_committed_s`, `diff_received_to_committed_s`, `hayai_contextual_commit_s`, `zakura_contextual_commit_s`, `diff_contextual_commit_s`, `hayai_received_to_template_empty_s`, `hayai_received_to_template_full_s`, `hayai_template_served_s`, `zakura_template_served_s`, `note`. A difference is hayaid minus zakurad. An empty cell: the node has no measurement for that block |
| `blocks.md` | Count, median, 90 % value and largest value of each column; the error of the Zakura clock calibration; the number of blocks without a Zakura value and the reason; for each RPC caller the number of calls, the mean time of a call without `longpollid` on the client side, the number of errors and the mode of the first answer on a block; the first 20 rows |
| `race-zakurad-getblocktemplate.jsonl`, `race-hayaid-getblocktemplate.jsonl` | The file of each RPC caller: one line for each call |
| `race-hayaid-traces/`, `race-zakurad-traces/` | The trace tables of both nodes |
| `race-zakurad-block-lines.log` | The log lines of zakurad that the table reads |
| `zakurad-series-*.json` | The three Prometheus series of zakurad for its contextual commit time, with a step of 5 s |
| `race-hayaid.log`, `race-zakurad.log` | The last 20,000 log lines of each node |
| `race-series.json` | Each `race:*` series from the first start, at most 5,000 points |
| `*-version.txt`, `*-info.txt`, `*-inspect.json`, `*-stats.txt`, `*-metrics.txt` | Versions, start times, container state, `docker stats`, the `/metrics` text |
| `hayaid-fault-rows.jsonl` | The trace rows of hayaid of a block that it did not accept |

The table of blocks (`scripts/race_blocks.py`):

- The rows start at the block after the lowest height of the two nodes at the moment at
  which the second node reached the tip.
- hayaid: each value is a field of a trace row. The node writes the field and the gauge
  of the same quantity from one clock reading.
- zakurad, "received to committed": the trace row and the log line have different
  clocks. The script finds the offset of the trace clock in two steps
  (`docs/zakura-measurements.md`, Trace clocks of Zakura). `blocks.md` states the error
  of each value (half of the range of the offset). On the Regtest runs the error was
  0.1 ms. Without the rows of a step the column is empty.
- zakurad, "contextual commit": between two samples in which the count of the summary
  increased by 1, the increase of the sum is the value of one block. An interval of 5 s
  with 2 or more blocks gives no value (about 6 % of the blocks at a spacing of 75 s).
- The value of zakurad for "received to committed" is in a row only when zakurad has the
  same block hash at that height.
- "Template served": the time of the first answer of the RPC caller with the block as
  `previousblockhash`, minus the commit time of the block on the same machine (hayaid:
  `unix_us` of the row `commit_finish`; zakurad: the time of the log line
  `downloaded and verified gossiped block`). Both times are on the wall clock of that
  machine. A block without an answer or without a commit time has an empty cell.

## Report when hayaid stops

A node that stops stays stopped (`restart: "no"`). Run `collect` and send the directory
`race-results/<time>/`. For the cause, read:

- `race-hayaid.log`: the last `ERROR` line and the line
  `shutting down reason="fatal error: ..."`;
- `hayaid-fault-rows.jsonl`: the trace rows with `not_validated`, an invalid result or a
  fault;
- `race-series.json`: the height over time. The last height is also in the panel
  "Block height".

Also send the output of `status`.

## Known risks

- hayaid never synchronized a public chain. A stop of hayaid is the expected result of
  the first run. Each stop is a finding.
- The last Mainnet checkpoint is block 3,499,045. Above it hayaid validates each block
  in full. This is its first full validation of real NU6.3 blocks.
- A typical Mainnet block is small. The times of one block are some milliseconds or
  less on both nodes, so the differences are small in absolute value, and the error of
  the Zakura clock calibration can be of the same size. Read the values of the large
  blocks and the 90 % values.
- The "received to committed" interval of hayaid during the first sync contains the wait
  for the parent blocks. Compare this quantity in the tip phase only.
- zakurad sets its height gauge after its summary. The rule
  `race:contextual_commit_block_height` reads the height one scrape after the commit. A
  next block in the same 5 s makes that height 1 too high.
- `peerset_initial_target_size = 25` gives hayaid 37 outbound peers. hayaid ran with 8
  until now.
- The log file of zakurad has no rotation. Read "Disk in use".
- The estimates of this page (disk, memory of hayaid, build time) are not measured.

## Removal

```sh
scripts/race_deploy.sh clean user@a.example user@b.example user@c.example   # containers, volumes, firewall rules
terraform -chdir=deploy/terraform/aws-race destroy                          # the AWS resources
```

`clean` leaves the images and the directory `hayai-race` on each host.

## Dry run on one machine

The same compose files run with the Regtest configurations of `docker/race/config`. All
addresses are on 127.0.0.1. hayaid connects to zakurad, and no node has another peer.

```sh
cd docker/race
export RACE_ZAKURAD_CONFIG=./config/zakurad.regtest.toml RACE_HAYAID_CONFIG=./config/hayaid.regtest.toml
export RACE_NODE_EXPORTER_ADDR=127.0.0.1:39100
export RACE_ZAKURAD_HOST=127.0.0.1 RACE_HAYAID_HOST=127.0.0.1 RACE_ZAKURAD_METRICS_PORT=39999
export RACE_HAYAID_METRICS_PORT=39101 RACE_ZAKURAD_EXPORTER_PORT=39100 RACE_HAYAID_EXPORTER_PORT=39100
export RACE_PROMETHEUS_ADDR=127.0.0.1:39090 RACE_GRAFANA_ADDR=127.0.0.1 RACE_GRAFANA_PORT=33000
export RACE_LAST_CHECKPOINT_HEIGHT=5
mkdir -p secrets && (umask 022 && head -c 18 /dev/urandom | base64 >secrets/grafana_admin_password)
docker compose -p race-dryrun-monitor -f compose.monitor.yml up -d
docker compose -p race-dryrun-node -f compose.node.yml --profile zakurad up -d node-exporter zakurad zakurad-caller
docker compose -p race-dryrun-node -f compose.node.yml --profile hayaid up -d hayaid hayaid-caller
cd ../.. && scripts/race_deploy.sh collect local local local
```

- Set `RACE_ZAKURA_IMAGE` and `RACE_HAYAI_IMAGE` when the images have other names than
  `zakurad:race` and `hayaid:race`.
- The `generate` method of each RPC server (ports 38232 and 38345, with the cookie) makes
  blocks. A block of hayaid goes to zakurad by gossip: only such a block has a
  "received to committed" value of zakurad.
- Each RPC caller reads the RPC address from the configuration of its node. The method
  `generate` is for the operator of the dry run only: the caller does not use it.
- The SSH target `local` runs the commands of `collect` on this machine.
- A Regtest chain has no tip of a network: "Time to the tip" stays empty, and the table
  of blocks starts at the first block that zakurad got by gossip.
- `docker compose -p <project> ... down --volumes` removes each project.
