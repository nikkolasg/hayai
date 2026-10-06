# hayaid for an operator of zakurad

Date: 2026-10-05. Reference: Zakura 1.6.0, commit `13779158253c`.

## Command line

| Form of `zakurad` | `hayaid` |
|---|---|
| `zakurad -c <file> start`, `zakurad --config <file> start` | The same form. The form `hayaid start -c <file>` stays |
| `zakurad -c <file>` (no command) | The same: `start` is the default command |
| `zakurad start` (no `-c`) | Reads `hayaid.toml` in the directory of `$XDG_CONFIG_HOME`, or in `$HOME/.config` (macOS: `$HOME/Library/Preferences`). This is the rule of `zakurad` (`dirs::preference_dir`), with the file name of hayaid. Without that file: an error that names the path. `zakurad` starts with its defaults in that case |
| `zakurad -v`, `--verbose` | The log level `debug` |
| `zakurad --filters <f>`, `zakurad start <f>` | One log level (`error`, `warn`, `info`, `debug`, `trace`) in place of `[tracing] filter`. A filter of a module is an error |
| `zakurad start --zcashd-compat`, `--unsafe-low-specs` | Error: `start --zcashd-compat: hayaid does not have the zcashd-compat mode of zakurad` |
| `zakurad generate` | Prints the default Mainnet configuration of hayaid (shadow mode) |
| `zakurad generate -o <file>`, `--output-file <file>` | Writes that configuration to the file |
| `zakurad tip-height -n <network> [-c <dir>]`, `--network`, `--cache-dir` | Prints the height of the best tip that the data directory holds, as `zakurad` does: the tip that a restart of the node resumes at. The command starts no node and writes no file. It reads the best block of the coins store, the record of `state.log` for that block, and the blocks of the block files above it that extend it. A node can run on the directory: the command then prints a tip that the directory held during the read, or an error. The directory is `--cache-dir`, or `[state] cache_dir` of the configuration file, or the default of that key. The networks are `mainnet`, `testnet` and `regtest`. An error is one line on stderr that starts with `hayaid: Failed to read chain tip height from state:`, with the exit status 1. A directory without `state.log` gives `State directory doesn't have a chain tip block: <dir> has no state.log`. `zakurad` writes its error line to stdout and exits with the status 0 |
| `zakurad audit-historical-treestates` | Error, exit status 1: `audit-historical-treestates: hayaid does not have this command of zakurad` |
| `zakurad verify-historical-treestates` | The same error with its name |
| `zakurad prune-state` | The same error with its name |
| `zakurad rollback-state` | The same error with its name |
| `zakurad help`, `-h`, `--help` | The usage text |
| `zakurad -V`, `--version` | `hayaid <version>` |
| none | `hayaid config --network regtest\|testnet\|mainnet`: the default configuration of a network |

`zakurad` reads variables with the prefix `ZAKURA_` in place of configuration keys.
hayaid reads no such variable: each setting is in the file. The Docker image of Zakura
sets `ZAKURA_STATE__CACHE_DIR` and `ZAKURA_RPC__COOKIE_DIR`.

## Configuration file

hayaid reads a configuration of `zakurad` as follows.

- A key in the column "same key" has the name, the value format and the meaning of Zakura.
- A key of Zakura that hayaid does not use gives one warning line in the log:
  `<section>.<key>: Zakura setting that hayaid does not use`. The node starts.
- A key of Zakura that changes the consensus rules, the network or a data location, and
  that hayaid cannot follow, is an error:
  `<section>.<key> = <value>: Zakura setting that hayaid does not use and cannot ignore (<reason>)`.
  The value that is the fixed behaviour of hayaid gives no message.
- One error has each such line and each warning line of the file.
- Each other unknown key is an error.

`crates/hayaid/src/config.rs` has the two lists (`ZAKURA_UNUSED`, `ZAKURA_REFUSED`).
Tests read `docker/default-zakura-config.toml` of Zakura, the output of
`zakurad generate` and `docker/config/zakurad.testnet.toml`, and compare the exact lines.

Differences that give no message:

- hayaid needs `[mining] miner_address` (or `miner_script`). `zakurad` starts without it.
  The error is `[mining] has no miner_address: hayaid needs miner_address or miner_script
  for the coinbase of its templates`.
- `[state] cache_dir` of hayaid must be empty or a directory of hayaid. The directory of a
  `zakurad` is an error at the start (`... is not empty and cache_dir has no state.log`).
- Without `[network] listen_addr`, hayaid does not listen. `zakurad` listens on the
  default port.
- The defaults of the directories differ: `zakurad` uses the cache directory of the user,
  hayaid uses `hayaid-data` in the working directory.

### Keys of Zakura

| Section | Key | Default of Zakura | Meaning | hayaid |
|---|---|---|---|---|
| `[consensus]` | `checkpoint_sync` | `true` | Use the checkpoints during the sync | `true`: no message, hayaid always uses its checkpoints. `false`: error |
| | `vct_fast_sync` | none | Fast path for the note commitment trees below the last checkpoint | warning |
| | `debug_skip_parameter_preload` | `false` | No use in Zakura | warning |
| `[health]` | `listen_addr`, `min_connected_peers`, `ready_max_blocks_behind`, `enforce_on_test_networks`, `ready_max_tip_age` | none, `1`, `2`, `false`, `5m` | Health endpoints `/healthy` and `/ready` | warning: hayaid has no health endpoint |
| `[mempool]` | `tx_cost_limit` | `80000000` | ZIP 401 cost limit | same key |
| | `max_transaction_bytes` | `250000` | Largest transaction of the mempool | warning |
| | `eviction_memory_time` | `1h` | Time for which an evicted transaction is refused | warning: hayaid has the same value as a constant |
| | `max_datacarrier_bytes` | `83` | Largest standard `OP_RETURN` script | warning |
| | `debug_enable_at_height` | none | Start of the mempool at a height | warning |
| `[metrics]` | `endpoint_addr` | none | Prometheus endpoint | same key |
| `[mining]` | `miner_address` | none | Address of the miner output | same key. hayaid takes a transparent address only, and needs the key |
| | `extra_coinbase_data` | none | Text in the coinbase input after the marker of the node | same key. hayaid writes `hayai: <text>`, 86 bytes of text at most |
| | `miner_memo` | none | Memo of a shielded coinbase output | warning: hayaid has no shielded coinbase |
| | `internal_miner` | `false` | Miner inside the node | warning |
| | `optimistic_block_inventory` | `true` | Announce a mined block before its commit | warning |
| `[network]` | `network` | `Mainnet` | `Mainnet`, `Testnet`, `Regtest` | same key and values. The table form (a configured Testnet or Regtest) is an error |
| | `listen_addr` | `[::]:8233` | P2P listen address, the port is optional | same key. Default: no listener |
| | `cache_dir` | `true` | Directory of the peer cache: `true`, `false` or a path | same key: the directory of the address book `peers.dat` |
| | `initial_mainnet_peers` | 4 DNS seeders | Seeders and peers of Mainnet | same key |
| | `initial_testnet_peers` | 2 DNS seeders | Seeders and peers of Testnet | same key; hayaid reads it on Regtest too |
| | `peerset_initial_target_size` | `100` | Size of the peer set: 3/2 of it outbound, 3 times it inbound | same key and rule. Default of hayaid: 8 outbound, 64 inbound |
| | `max_connections_per_ip` | `1` | Connections with one IP address | same key |
| | `testnet_parameters` | none | Parameters of a configured Testnet or Regtest | error. hayaid has `[regtest]` for a Regtest network |
| | `external_addr` | none | Address that the node announces | warning |
| | `identity_dir`, `zakura_node_secret_key` | `~/.zakura`, none | Identity of the node in the Zakura P2P stack | warning |
| | `p2p_stack`, `legacy_p2p`, `v2_p2p` | `default` | Choice of the P2P stack | warning: hayaid runs the legacy protocol and its compact-relay extension |
| | `crawl_new_peer_interval` | `1m 1s` | Time between two searches for peers | warning |
| | `expose_peer_addresses` | `false` | Peer addresses in logs and metrics | warning: the log of hayaid has peer addresses |
| `[network.zakura]` | `trace_dir` | none | Directory of the JSONL trace tables | same key: the trace tables of hayaid |
| | `bootstrap_peers`, `listen_addr`, `nat_traversal`, `max_connections`, `max_connections_per_ip`, `max_pending_handshakes`, `stream_open_rate_per_second`, `message_rate_per_second`, `header_sync`, `block_sync`, `dev_network` | see `zakurad generate` | The Zakura P2P stack (QUIC) | warning: hayaid does not have this stack |
| `[rpc]` | `listen_addr` | none | JSON-RPC server | same key |
| | `enable_cookie_auth` | `true` | Cookie authentication | same key |
| | `cookie_dir` | cache directory of the user | Directory of the cookie file | same key. Default: `[state] cache_dir` |
| | `cookie_file_name` | `.cookie` | Name of the cookie file | `.cookie`: no message. Another name: error |
| | `admin_listen_addr`, `indexer_listen_addr`, `indexer_tls`, `tls` | none | More listeners, TLS | warning: hayaid has one listener without TLS |
| | `parallel_cpu_threads`, `max_response_body_size`, `debug_force_finished_sync` | `0`, `52428800`, `false` | Tuning of the server | warning |
| `[state]` | `cache_dir` | cache directory of the user | Data directory | same key. hayaid puts its files in the directory, without a `state/` level |
| | `ephemeral` | `false` | State in a temporary directory | `false`: no message. `true`: error |
| | `storage_mode` | `archive` | Archive or pruned state | warning |
| | `delete_old_database`, `should_backup_non_finalized_state`, `historical_frontier_artifact`, `repair_zakura_header_store_on_startup` | `true`, `true`, none, `false` | Maintenance of the Zakura database | warning |
| | `debug_stop_at_height`, `debug_validity_check_interval`, `debug_skip_non_finalized_state_backup_task` | none, none, `false` | Debug settings | warning |
| `[sync]` | `download_concurrency_limit`, `checkpoint_verify_concurrency_limit`, `full_verify_concurrency_limit`, `zakura_block_apply_concurrency_limit`, `parallel_cpu_threads` | `100`, `1000`, `20`, `32`, `0` | Limits of the sync pipeline of Zakura | warning. hayaid bounds its download by memory (`memory_budget_bytes`) |
| | `debug_skip_regtest_genesis_self_seed`, `debug_blocksync_throughput_target_height` | `false`, none | Debug settings | warning |
| `[tracing]` | `filter` | none (`info`) | Log filter | same key, one level only |
| | `use_color` | `true` | Colours on a terminal | same key |
| | `force_use_color` | `false` | Colours in each case | same key |
| | `log_file` | none | Log file | same key. Without it hayaid logs to stderr, `zakurad` to stdout |
| | `buffer_limit`, `endpoint_addr`, `flamegraph`, `progress_bar`, `use_journald`, `opentelemetry_endpoint`, `opentelemetry_service_name`, `opentelemetry_sample_percent` | `128000`, none, none, none, `false`, none, none, none | Log buffer, filter endpoint, flamegraph, progress bars, journald, OpenTelemetry | warning |
| `[zcashd_compat]` | `enabled`, `manage_zcashd`, `zcashd_source`, `zcashd_path`, `zcashd_datadir`, `zcashd_extra_args`, `block_gossip_peer_ips`, `startup_delay`, `restart_backoff`, `restart_backoff_max`, `restart_reset_after`, `shutdown_grace_period` | `false` and the values of `zakurad generate` | The zcashd-compat mode | warning |

### Keys of hayaid only

`[network]`: `mode`, `peers`, `compact_relay`, `max_peers`, `prebuilt_candidates`,
`outbound_peers`, `max_inbound`, `ban_secs`. `[sync]`: `memory_budget_bytes`,
`request_timeout_ms`, `header_timeout_ms`, `header_poll_ms`, `header_poll_max_ms`. `[state]`: `backend`, `flush_interval_blocks`,
`snapshot_interval_blocks`, `wallet_index` (Zakura writes its indexes in each
`storage_mode`). `[trace]`: `node`. `[mining]`: `miner_script`,
`regtest_produce`, `prebuild_own`, `lane_publication`. The sections `[shadow]` and
`[regtest]`. `docs/hayaid.md` (Configuration) has each key.

### Renamed keys

| Before | Now |
|---|---|
| `[network] network = "regtest"`, `"testnet"`, `"mainnet"` | `"Regtest"`, `"Testnet"`, `"Mainnet"` |
| `[network] seeders` | `initial_mainnet_peers`, `initial_testnet_peers` |
| `[network] max_per_ip` | `max_connections_per_ip` |
| `[state] data_dir` | `[state] cache_dir` |
| `[metrics] listen_addr` | `[metrics] endpoint_addr` |
| `[trace] dir` | `[network.zakura] trace_dir` |
| `[log] level` | `[tracing] filter` |

The old names are unknown keys.

## Metrics

The metrics that the dashboards and the alert rules of Zakura (`docker/observability`)
use for the basic health of a node. hayaid exports a name only with the meaning of Zakura.
The other metrics of hayaid start with `hayai_`.

| Metric of Zakura | Labels | hayaid | Meaning in hayaid, or the reason |
|---|---|---|---|
| `zcash_chain_verified_block_height` | none | yes | Height of the newest committed block |
| `zcash_chain_verified_block_total` | none | yes | Blocks committed since the start. `zakurad` also counts the genesis block of an empty state |
| `state_memory_best_committed_block_height` | none | yes | Height of the tip of the best chain |
| `state_finalized_block_height` | none | yes | Height of the newest block whose coins are in the coins store on disk (the last flush). `hayai_base_height` is the base in memory: the newest block below the 1,000 layers of the reorganization depth |
| `sync_downloads_in_flight` | none | yes | Block requests without an answer plus downloaded blocks that wait for the validator. `hayai_sync_requests_in_flight` has the requests only |
| `sync_block_verify_duration_seconds` | `result` | yes | Reception of a block to its commit (`success`) or to its rejection (`failure`), with the wait for the parent blocks, as in `zakurad`. hayaid exports a histogram (`_bucket`, `_sum`, `_count`); `zakurad` exports a summary (`quantile`, `_sum`, `_count`). The validation time alone is `hayai_validate_stage_duration_seconds{stage="total"}` |
| `zcash_net_peers` | none | yes | Connected peers after the handshake |
| `zcash_net_in_bytes_total`, `zcash_net_out_bytes_total` | none | yes | Bytes of the P2P messages, received and sent |
| `sync_downloaded_block_count` | none | yes | Blocks that the block download received and stored |
| `zcash_mempool_size_transactions`, `zcash_mempool_size_bytes` | none | yes | Transactions of the mempool and their bytes |
| `rpc_requests_total` | `method`, `status` (`success`, `error`) | yes | RPC requests. A method that hayaid does not have has the label value `unknown`, so the number of series has a bound |
| `rpc_request_duration_seconds` | `method` | yes | Time of a request. Histogram in hayaid, summary in `zakurad` |
| `rpc_errors_total` | `method`, `error_code` | yes | RPC errors by JSON-RPC error code |
| `rpc_active_requests` | none | yes | Requests in progress |
| `mining_template_rebuilt` | none | no | `zakurad` counts a template that a new tip replaced during its build. hayaid has no such event: `hayai_template_updates_total` counts each full or changed template |
| `process_resident_memory_bytes`, `process_cpu_seconds_total` | none | yes | Memory and CPU time of the process. The Docker build of Zakura 1.6.0 does not export them |
| `sync_estimated_distance_to_tip`, `sync_estimated_network_tip_height` | none | no | hayaid makes no estimate of the network tip from the time. `hayai_sync_header_height` is the height of its best header chain |
| `sync_verified_block_count` | none | no | hayaid has no separate count of the verified blocks of the sync. `zcash_chain_verified_block_total` counts each committed block |
| `sync_block_download_duration_seconds`, `sync_stage_duration_seconds`, `sync_*_hash_count`, `sync_prospective_tips_len`, `sync_cancelled_*` | several | no | Stages of the sync pipeline of Zakura. The sync of hayaid has other stages (`hayai_sync_*`) |
| `sync_block_best_header_tip_height` | none | no | Metric of the Zakura P2P stack. hayaid has `hayai_sync_header_height` |
| `zcash_net_in_bytes_total`, `zcash_net_out_bytes_total` | none | no | hayai-net does not count the bytes of the connections |
| `zcash_net_in_messages`, `zcash_net_out_messages` | `command` | no | hayai-net does not count the messages by command |
| `zcash_net_peers_connected`, `zcash_net_peers_initial`, `zcash_net_peers_obsolete`, `zcash_net_peers_version_*` | versions, addresses | no | hayaid has no count of peers by version |
| `zcash_net_peer_handshake_duration_seconds`, `zcash_net_peer_handshake_failures_total` | several | no | hayaid does not measure the handshake |
| `mempool_queued_transactions_total`, `mempool_verified_transactions_total`, `mempool_rejected_transactions_total`, `mempool_rejected_transaction_ids` | `version`, `reason` | no | The mempool of hayaid has no download queue, and its reject reasons are other classes (`hayai_mempool_rejected_total{reason}`) |
| `checkpoint_*`, `state_checkpoint_*`, `state_full_verifier_committed_block_height`, `state_memory_queued_*`, `state_memory_sent_block_height` | none | no | Queues of the verifiers of Zakura. hayaid has one commit path |
| `state_finalized_cumulative_*`, `state_finalized_value_pool_*`, `state_finalized_chain_supply_total` | none | no | hayaid exports no totals of the finalized state. The RPC method `getblockchaininfo` has the value pools |
| `zakura_state_rocksdb_*` | several | no | Database of Zakura |
| `zakurad_build_info`, `end_of_support_remaining_blocks`, `zakura_errors_total` | several | no | Names of the Zakura program. hayaid has `hayai_build_info` |
| `proofs_*_verified`, `signatures_*_validated`, `zakura_consensus_batch_duration_seconds` | none | no | Batch verifiers of Zakura. hayaid has `hayai_validate_stage_duration_seconds{stage}` |

`scripts/check_metric_names.py` has the names of the rows with "yes" in a list
(`ZAKURA_NAMES`) and fails when a name is not in the sources of hayaid.
