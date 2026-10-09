# hayai deployment files

Date: 2026-10-04. `docs/install.md` gives the procedures.

## Docker

| File | Content |
|---|---|
| `docker/Dockerfile` | Multi-stage image of hayaid: Rust 1.97.1 (the lowest toolchain that builds the whole workspace; the node crates need 1.91), a cargo-chef dependency layer, a release build that removes the DWARF sections, debian-slim with the user `hayai` (10001). Build argument `CRYPTO_BACKEND` (`upstream` or `zakura`). Volume `/var/lib/hayai`. Health check on `/metrics`. |
| `docker/Dockerfile.dockerignore` | Build context: the Cargo workspace. |
| `docker/compose.yml` | Profile `testnet`: zakurad and `hayaid-testnet` (shadow mode, in the network namespace of zakurad; it starts when zakurad reports `/ready`). Profile `mainnet`: zakurad-mainnet and `hayaid-mainnet`, the same on Mainnet. Profile `regtest`: `hayaid-regtest` (full mode). One volume per node and network. |
| `docker/compose.observability.yml` | Prometheus, Alertmanager, Grafana (admin password from `docker/secrets/grafana_admin_password`), node-exporter. |
| `docker/.env` | Compose variables: `COMPOSE_PROFILES`, `HAYAI_ADMIN_BIND`, `HAYAI_IMAGE`, `HAYAI_CRYPTO_BACKEND`, `ZAKURA_IMAGE`. |
| `docker/config/hayaid.testnet.toml` | hayaid in shadow mode: peer and RPC of zakurad on 127.0.0.1, data in `/var/lib/hayai`, traces on. |
| `docker/config/hayaid.mainnet.toml` | hayaid in shadow mode on Mainnet: peer 8233 and RPC 8232 of zakurad-mainnet on 127.0.0.1. |
| `docker/config/hayaid.regtest.toml` | hayaid in full mode: P2P 18344, RPC 18345 with cookie authentication (cookie file `/var/lib/hayai/.cookie`), `generate` on. |
| `docker/config/zakurad.testnet.toml` | zakurad on Testnet: RPC on loopback without cookie authentication, metrics 9999, health 8080, traces. |
| `docker/config/zakurad.mainnet.toml` | zakurad on Mainnet: the same settings, P2P 8233, RPC 8232. |

## Observability

| File | Content |
|---|---|
| `docker/observability/prometheus/prometheus.yml` | Scrape jobs `hayaid` (DNS names `hayaid-testnet`, `hayaid-mainnet`, `hayaid-regtest`; label `network`), `zakurad` (`zakurad`, `zakurad-mainnet`), `node`, `prometheus`. |
| `docker/observability/prometheus/rules/hayaid.yml` | Alerts: node down or absent, tip stalled (Testnet and Mainnet, 30 min = 24 spacings of 75 s), behind zakurad, p90 validation time above 1 s, p90 latency of a full template above 1 s, process memory above 80 % of the host memory, no peers, shadow disagreement, rejected blocks, dropped trace rows, disk below 10 % and 5 %, memory below 10 %. |
| `docker/observability/prometheus/tests/hayaid_test.yml` | `promtool test rules` cases for the stall, no-peers, disagreement, down, absent, memory and template latency alerts. |
| `docker/observability/alertmanager/alertmanager.yml` | One placeholder receiver without a notifier; inhibition of follow-up alerts. |
| `docker/observability/grafana/provisioning/` | Prometheus data source and the dashboard provider. |
| `docker/observability/grafana/dashboards/hayaid.json` | Dashboard: tip height, blocks per minute, validation time and stages, commit time, size and hit rate of the prepared store, template rebuilds, relay counters, shadow verdicts and trust limits, dropped trace rows, host CPU, memory and disk, process memory and CPU, coins cache, template latency, build info. |

## Race of zakurad and hayaid

`docs/sync-race.md` is the runbook: first sync and comparison at the tip, Mainnet by
default.

| Path | Content |
|---|---|
| `docker/race/compose.node.yml` | One node machine: profile `zakurad` or profile `hayaid`, and node-exporter. Host network, no restart, the same CPU and memory limits for both node services (`RACE_CPUS`, `RACE_MEMORY`, default none). `RACE_NETWORK` (default `mainnet`) selects the node configurations. |
| `docker/race/compose.monitor.yml` | The monitoring machine: Prometheus (targets from `RACE_ZAKURAD_HOST` and `RACE_HAYAID_HOST`, last checkpoint height from `RACE_NETWORK`) and Grafana. |
| `docker/race/config/zakurad.<network>.toml`, `hayaid.<network>.toml` | The 2 nodes on Mainnet and on Testnet: full sync, DNS seeders of the defaults, the same peer limits, RPC on loopback with the cookie, metrics on 9999 and 19101, trace tables on both nodes, legacy P2P stack and a log file on zakurad. |
| `docker/race/config/zakurad.regtest.toml`, `hayaid.regtest.toml` | The dry run on one machine: Regtest, 127.0.0.1, hayaid is the only peer of zakurad. |
| `docker/race/prometheus/prometheus.yml` | One scrape job each 5 s with the targets of the file service discovery; each target has the label `node`. |
| `docker/race/prometheus/rules/race.yml` | Recording rules `race:*`: heights, blocks for each second, peers, P2P bytes, the first time at the last checkpoint and at the tip, the mean service time of RPC, one value for each block (contextual commit time and its block height), verification reuse, machine CPU, memory, disk and network. |
| `docker/race/prometheus/tests/race_test.yml`, `race_blocks_test.yml`, `race_sidecar_test.yml` | `promtool test rules` cases for each rule. |
| `docker/race/grafana/dashboards/comparison.json` | Dashboard "zakurad and hayaid: comparison": only the quantities with a close meaning on both nodes, each with its difference. |
| `docker/race/grafana/dashboards/hayai-node.json`, `zakura-node.json` | One dashboard for each node with each metric that the node exports. |
| `scripts/race_deploy.sh` | `build`, `start`, `status`, `stop`, `collect`, `swap`, `clean` for 3 hosts with Docker and SSH. |
| `scripts/race_rpc_caller.py` | The RPC caller of one node: `getblocktemplate` with the long poll of a pool, at the cadence of `RACE_CALLER_INTERVAL`. The same image and settings on both machines. |
| `scripts/race_sidecar.py` | The sidecar of one node: the compared quantities of the node from its cgroup (`cpu.stat`, `memory.current`, `memory.peak`, `io.stat`), its data directory and its RPC, as a textfile of node-exporter. |
| `scripts/gen_race_dashboards.py` | Writes the 3 dashboards of `docker/race/grafana/dashboards/`. CI checks that the files equal its output. |
| `scripts/test_race_blocks.py`, `scripts/test_race_sidecar.py` | Unit tests of `race_blocks.py` and `race_sidecar.py` (`python3 -m unittest`). |
| `scripts/race_blocks.py` | The table of blocks at the tip (`blocks.csv`, `blocks.md`) from the traces of both nodes, the log of zakurad and its Prometheus series. `scripts/test_race_blocks.py` tests it with the files of `scripts/fixtures/race_blocks`. |
| `scripts/zakura_metric_names.txt` | The metric families of a running zakurad. `scripts/check_metric_names.py` reads it. |
| `deploy/terraform/aws-race/` | 3 instances (A and B equal, C small), security groups, first-boot scripts that build the image and start the node at `start_at`. Variable `network` (default `mainnet`). |

## Bare metal

| File | Content |
|---|---|
| `deploy/systemd/hayaid.service` | Unit for user `hayai`: `StateDirectory=hayai`, `ProtectSystem=strict` and other sandbox settings, `Restart=on-failure` with a start limit, `LimitNOFILE=65536`, 2 min for a clean stop. |
| `scripts/install.sh` | Build or take a binary, create the user and directories, write `/etc/hayai/hayaid.toml` from `hayaid config`, install and enable the unit. `--uninstall`, `--purge`. Idempotent. |
| `scripts/fetch-params.sh` | Download of `sapling-spend.params` and `sapling-output.params` with the size and BLAKE2b-512 checks of zcash_proofs. hayaid does not read the files. `scripts/extract-sapling-vk.sh` reads them. |
| `scripts/extract-sapling-vk.sh` | Writes the 2 Sapling verifying keys that hayai embeds (`crates/hayai-prepared/src/sapling_vk/`) from the parameter files, after the same checks. |
| `scripts/extract-sprout-vk.sh` | Writes the Sprout verifying key that hayai embeds (`crates/hayai-prepared/src/sprout_vk/`) from `sprout-groth16.params` (`scripts/fetch-params.sh DIR --sprout`), after the same checks. |

## AWS

| File | Content |
|---|---|
| `deploy/terraform/aws/versions.tf` | The versions of Terraform and of the AWS provider, default tags. |
| `deploy/terraform/aws/variables.tf` | Region, network (`testnet`, `mainnet` or `regtest`), instance type (default `m7i.xlarge`, 16 GiB), gp3 data volume (size, IOPS, throughput), `admin_cidr`, optional key pair, subnet, Git URLs and refs of hayai and Zakura, crypto backend. |
| `deploy/terraform/aws/main.tf` | Ubuntu 24.04 instance with IMDSv2, encrypted gp3 data volume, security group (P2P open, admin ports from `admin_cidr` only), IAM role for SSM Session Manager. |
| `deploy/terraform/aws/cloud-init.yaml.tftpl` | Install of Docker; `hayai-bootstrap` (data volume on `/var/lib/docker`, clones, zakurad image on Testnet and Mainnet, Grafana password); `hayai-stack` (compose up with both files). |
| `deploy/terraform/aws/outputs.tf` | Instance id, public IP, P2P endpoint, the commands for an SSM shell and for a port forward to Grafana, Grafana URL, password and log commands. |
| `deploy/terraform/aws/terraform.tfvars.example` | Example variables. |

## CI and checks

| File | Content |
|---|---|
| `.github/workflows/ci.yml` | Jobs of each push: fmt; clippy `-D warnings` and release tests on the upstream backend; cargo-deny; compose check; Prometheus, Alertmanager and dashboard checks (also the race rules, with the tests of `scripts/race_blocks.py` and `scripts/race_sidecar.py`); Terraform fmt and validate of both root modules; shellcheck. |
| `.github/workflows/full.yml` | Jobs on demand: clippy and release tests on both backends; msrv check of the node crates on the workspace `rust-version`; benches compile; Docker build. |
| `deny.toml` | Licenses (MIT, Apache-2.0, BSD, ISC, Unicode, Zlib, CC0, one MPL-2.0 exception), RustSec advisories (none ignored), sources. The file excludes the `multitable` git dependency from the graph: the dependency has no license. It is a feature of hayai-bench that is off by default. |
| `scripts/check_metric_names.py` | Fails when a rule or a dashboard uses a metric name that hayaid does not register, or when a rule or a dashboard of `docker/race` reads a name that is not in its lists (hayaid, the families of a running zakurad, node-exporter). |
