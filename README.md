# hayai

The performance core of a Zcash miner node. By default, the cryptography comes from the
upstream Zcash crates. With the `zakura` feature, it comes from the `zakura-*` forks.
Everything above `hayai-crypto` builds on either backend.

- `docs/architecture.md` — crates, data flow, principles.
- `docs/protocol-compact-relay.md` — block and transaction relay (short ids, batch lanes).
- `docs/protocol-template-push.md` — template delivery to pools.
- `docs/consensus.md` — consensus rules implemented and their status.
- `docs/report.html` — benchmarks against Zakura. `scripts/report.py` renders it from `bench-results/`.
- `docs/hayaid.md` — the node binary: modes, configuration, traces, metrics, limits.
- `docs/install.md` — installation with Docker Compose, systemd or Terraform on AWS; checks, upgrade, backup.
- `docs/zakura-compat.md` — hayaid for an operator of zakurad: command line, each configuration key, metrics.
- `docs/sync-race.md` — runbook of the sync race of zakurad and hayaid on Testnet.
- `docs/deploy.md` — the deployment, observability and CI files.

## Quick start

Start a Regtest node with Prometheus, Grafana and Alertmanager in Docker Compose.
`docs/install.md` gives the Testnet and Mainnet procedures, systemd and AWS.

```
cd docker
docker compose build
install -d -m 0700 secrets
(umask 022 && openssl rand -base64 24 > secrets/grafana_admin_password)
COMPOSE_PROFILES=regtest docker compose -f compose.yml -f compose.observability.yml up -d
curl -s http://127.0.0.1:19101/metrics | grep '^state_memory_best_committed_block_height'
```

Grafana is at `http://127.0.0.1:3000`. The user is `admin`, and the password is in
`docker/secrets/grafana_admin_password`.

## Build and test

```
# Upstream backend (default)
cargo test --workspace
cargo bench -p hayai-bench

# Zakura backend (zakura-* forks; mutually exclusive with the default). `baselines` is the
# default feature of hayai-bench that has the comparisons with Zakura and Zebra.
cargo test --workspace --no-default-features --features zakura,baselines
cargo bench -p hayai-bench --no-default-features --features zakura,baselines

# One crate on the zakura backend
cargo test -p hayai-trees --no-default-features --features zakura

# The set of the CI of each push: no comparison, no fuzzer, no test in a `slow` module
cargo test --workspace --exclude hayai-fuzz --no-default-features --features upstream --release -- --skip slow::

python3 scripts/report.py
```

Benchmark ids of hayai code are `hayai...` on the upstream backend and `hayai-zk...` on the
zakura backend. Both backends can therefore appear in one report.

## Node

`hayaid` runs a full node on Regtest or Mainnet (`mode = "full"`) or follows a local Zakura
node on Testnet or Mainnet (`mode = "shadow"`). The workspace needs Rust 1.91 for the node
crates and 1.97 for `hayai-bench`. `hayaid` writes JSONL traces in Zakura's format and serves
Prometheus metrics. `docs/hayaid.md` gives the details and the limits.

```
cargo build --release -p hayaid
target/release/hayaid config --network regtest > regtest.toml
target/release/hayaid start -c regtest.toml

# Two nodes, ten blocks, joined traces (target/regtest-pair/<time>/)
scripts/regtest_pair.sh --blocks 10
# Testnet shadow of a local zakurad (RPC on 127.0.0.1:18232, cookie authentication off)
target/release/hayaid config --network testnet > shadow.toml
target/release/hayaid start -c shadow.toml
```
