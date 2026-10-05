# hayai deployment files

Date: 2026-10-04. `docs/install.md` gives the procedures.

## Docker

| File | Content |
|---|---|
| `docker/Dockerfile` | Multi-stage image of hayaid: Rust 1.97.1 (the lowest toolchain that builds the whole workspace; the node crates need 1.91), a cargo-chef dependency layer, a release build with the DWARF sections removed, debian-slim with the user `hayai` (10001). Build argument `CRYPTO_BACKEND` (`upstream` or `zakura`). Volume `/var/lib/hayai`. Health check on `/metrics`. |
| `docker/Dockerfile.dockerignore` | Build context: the Cargo workspace. |
| `docker/compose.yml` | Profile `testnet`: zakurad and `hayaid-testnet` (shadow mode, in the network namespace of zakurad, started when zakurad reports `/ready`). Profile `mainnet`: zakurad-mainnet and `hayaid-mainnet`, the same on Mainnet. Profile `regtest`: `hayaid-regtest` (full mode). One volume per node and network. |
| `docker/compose.observability.yml` | Prometheus, Alertmanager, Grafana (admin password from `docker/secrets/grafana_admin_password`), node-exporter. |
| `docker/.env` | Compose variables: `COMPOSE_PROFILES`, `HAYAI_ADMIN_BIND`, `HAYAI_IMAGE`, `HAYAI_CRYPTO_BACKEND`, `ZAKURA_IMAGE`. |
| `docker/config/hayaid.testnet.toml` | hayaid in shadow mode: peer and RPC of zakurad on 127.0.0.1, data in `/var/lib/hayai`, traces on. |
| `docker/config/hayaid.mainnet.toml` | hayaid in shadow mode on Mainnet: peer 8233 and RPC 8232 of zakurad-mainnet on 127.0.0.1. |
| `docker/config/hayaid.regtest.toml` | hayaid in full mode: P2P 18344, RPC 18345, `generate` on. |
| `docker/config/zakurad.testnet.toml` | zakurad on Testnet: RPC on loopback without cookie authentication, metrics 9999, health 8080, traces. |
| `docker/config/zakurad.mainnet.toml` | zakurad on Mainnet: the same settings, P2P 8233, RPC 8232. |

## Observability

| File | Content |
|---|---|
| `docker/observability/prometheus/prometheus.yml` | Scrape jobs `hayaid` (DNS names `hayaid-testnet`, `hayaid-mainnet`, `hayaid-regtest`; label `network`), `zakurad` (`zakurad`, `zakurad-mainnet`), `node`, `prometheus`. |
| `docker/observability/prometheus/rules/hayaid.yml` | Alerts: node down or absent, tip stalled (Testnet and Mainnet, 30 minutes = 24 spacings of 75 s), behind zakurad, p90 validation time above 1 s, p90 full template latency above 1 s, process memory above 80 % of the host memory, no peers, shadow disagreement, rejected blocks, dropped trace rows, disk below 10 % and 5 %, memory below 10 %. |
| `docker/observability/prometheus/tests/hayaid_test.yml` | `promtool test rules` cases for the stall, no-peers, disagreement, down, absent, memory and template latency alerts. |
| `docker/observability/alertmanager/alertmanager.yml` | One placeholder receiver without a notifier; inhibition of follow-up alerts. |
| `docker/observability/grafana/provisioning/` | Prometheus data source and the dashboard provider. |
| `docker/observability/grafana/dashboards/hayaid.json` | Dashboard: tip height, blocks per minute, validation time and stages, commit time, prepared store size and hit rate, template rebuilds, relay counters, shadow verdicts and trust limits, dropped trace rows, host CPU, memory and disk, process memory and CPU, coins cache, template latency, build info. |

## Bare metal

| File | Content |
|---|---|
| `deploy/systemd/hayaid.service` | Unit for user `hayai`: `StateDirectory=hayai`, `ProtectSystem=strict` and further sandboxing, `Restart=on-failure` with a start limit, `LimitNOFILE=65536`, 2 minutes for a clean stop. |
| `scripts/install.sh` | Build or take a binary, create the user and directories, write `/etc/hayai/hayaid.toml` from `hayaid config`, install and enable the unit. `--uninstall`, `--purge`. Idempotent. |
| `scripts/fetch-params.sh` | Download of `sapling-spend.params` and `sapling-output.params` with the size and BLAKE2b-512 checks of zcash_proofs. hayaid does not read the files. `scripts/extract-sapling-vk.sh` reads them. |
| `scripts/extract-sapling-vk.sh` | Writes the two Sapling verifying keys that hayai embeds (`crates/hayai-prepared/src/sapling_vk/`) from the parameter files, after the same checks. |
| `scripts/extract-sprout-vk.sh` | Writes the Sprout verifying key that hayai embeds (`crates/hayai-prepared/src/sprout_vk/`) from `sprout-groth16.params` (`scripts/fetch-params.sh DIR --sprout`), after the same checks. |

## AWS

| File | Content |
|---|---|
| `deploy/terraform/aws/versions.tf` | Terraform and AWS provider versions, default tags. |
| `deploy/terraform/aws/variables.tf` | Region, network (`testnet`, `mainnet` or `regtest`), instance type (default `m7i.xlarge`, 16 GiB), gp3 data volume (size, IOPS, throughput), `admin_cidr`, optional key pair, subnet, Git URLs and refs of hayai and Zakura, crypto backend. |
| `deploy/terraform/aws/main.tf` | Ubuntu 24.04 instance with IMDSv2, encrypted gp3 data volume, security group (P2P open, admin ports from `admin_cidr` only), IAM role for SSM Session Manager. |
| `deploy/terraform/aws/cloud-init.yaml.tftpl` | Docker install; `hayai-bootstrap` (data volume on `/var/lib/docker`, clones, zakurad image on Testnet and Mainnet, Grafana password); `hayai-stack` (compose up with both files). |
| `deploy/terraform/aws/outputs.tf` | Instance id, public IP, P2P endpoint, SSM shell and Grafana port forward commands, Grafana URL, password and log commands. |
| `deploy/terraform/aws/terraform.tfvars.example` | Example variables. |

## CI and checks

| File | Content |
|---|---|
| `.github/workflows/ci.yml` | Jobs: fmt; msrv check of the node crates on the workspace `rust-version`; clippy `-D warnings` and release tests on both backends; benches compile; cargo-deny; Docker build and compose check; Prometheus, Alertmanager and dashboard checks; Terraform fmt and validate; shellcheck. |
| `deny.toml` | Licenses (MIT, Apache-2.0, BSD, ISC, Unicode, Zlib, CC0, one MPL-2.0 exception), RustSec advisories (none ignored), sources. The `multitable` git dependency, which has no license, is excluded from the graph; it is a feature of hayai-bench that is off by default. |
| `scripts/check_metric_names.py` | Fails when a rule or the dashboard uses a metric name that `crates/hayaid/src/metrics.rs` does not register. |
