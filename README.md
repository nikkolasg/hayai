# ⚡ hayai

**A fast, independent Zcash node for miners.**

[![CI](https://github.com/nikkolasg/hayai/actions/workflows/ci.yml/badge.svg)](https://github.com/nikkolasg/hayai/actions/workflows/ci.yml)
![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)
![Rust 1.91+](https://img.shields.io/badge/rust-1.91%2B-orange)
![Status: experimental](https://img.shields.io/badge/status-experimental-yellow)

hayai is a Zcash full node, written from scratch for mining pools and solo miners. It does not
share code with Zakura or Zebra above the cryptography, so a defect in one of them does not stop
the miners that run hayai. It follows the same consensus rules as every other Zcash node, and it
speaks the normal Zcash peer-to-peer protocol with all of them.

Its focus is speed at the moments that cost a miner money: the arrival of a new block, the next
mining template, and the first sync. "Hayai" (速い) means "fast" in Japanese.

> ⚠️ hayai is experimental. It has synced the public Testnet and runs next to Zakura in tests,
> but it has not yet run for a long time on Mainnet. Do not use it for a pool with real hash power
> yet.

## 📑 Contents

- [Highlights](#-highlights)
- [Why a second node implementation](#-why-a-second-node-implementation)
- [Features](#-features)
- [Quick start](#-quick-start)
- [Build from source](#-build-from-source)
- [Usage](#-usage)
- [Monitoring](#-monitoring)
- [Consensus and correctness](#-consensus-and-correctness)
- [Benchmarks](#-benchmarks)
- [Project layout](#-project-layout)
- [Documentation](#-documentation)
- [Status and roadmap](#-status-and-roadmap)
- [Contributing](#-contributing)
- [Security](#-security)
- [License](#-license)
- [Acknowledgements](#-acknowledgements)

## ✨ Highlights

| | hayai against Zakura, on the same machine |
|---|---|
| 🔁 Testnet sync from genesis to the same height | **3.4×** faster (24.2 min, not 81.9 min), preliminary |
| ✅ Validation of a new full block whose transactions are known | **12×** faster |
| ⛏️ Update of the mining template after a new transaction | **29×** faster |
| 📦 Bytes to send a 2 MB block to another hayai node | **1,134×** fewer (1.64 kB, not 1.86 MB) |

The [report](docs/report.html) gives each measurement, its method and its limits.

## 🤔 Why a second node implementation

Most Zcash miners move to one node implementation. When every miner runs the same code, one
defect can stop block production for the whole network, or split the chain. A second
implementation that miners can switch to removes that single point of failure.

A second implementation is only useful if miners want to run it. hayai gives them a reason: it
is faster where a miner earns or loses money, and it is easy to switch to from Zakura.

## 🚀 Features

**Performance**
- Each transaction is verified one time, when it enters the mempool. A new block then needs only
  the checks that depend on the chain.
- The chain state stays in memory, and a block reads all the coins it spends in one batch.
- The mining template is always ready. It changes in place with each transaction and each block.
- A speculative tip gives the pool a template on a new block before validation ends, and takes it
  back if a check fails.

**Protocols** (optional, between hayai nodes only)
- Compact block relay: a block travels as references to transactions that the peer already holds.
- Transaction lanes: a miner shares its next block while it searches for the proof of work.
- Template push: the node sends each template change to the pool server.
- A [draft ZIP](zip/zip-draft-compact-block-relay.md) specifies the relay protocol.

**Node**
- Full sync from the network, with the checkpoint lists of Zakura.
- The command line, the configuration keys and the main metrics of Zakura, so an operator can
  switch with few changes.
- RPC with cookie authentication, including `getblocktemplate` and the methods that pools use.
- An optional wallet index with the RPC methods that lightwalletd needs.
- Two cryptography backends: the official Zcash crates by default, or Zakura's crates.

## 🏁 Quick start

Start a private Regtest node with Prometheus, Grafana and Alertmanager in Docker Compose:

```sh
cd docker
docker compose build
install -d -m 0700 secrets
(umask 022 && openssl rand -base64 24 > secrets/grafana_admin_password)
COMPOSE_PROFILES=regtest docker compose -f compose.yml -f compose.observability.yml up -d
curl -s http://127.0.0.1:19101/metrics | grep '^state_memory_best_committed_block_height'
```

Grafana is at `http://127.0.0.1:3000`. The user is `admin`, and the password is in
`docker/secrets/grafana_admin_password`. The [install guide](docs/install.md) gives the
procedures for Testnet, Mainnet, systemd and AWS.

## 🛠️ Build from source

hayai needs Rust 1.91 or later for the node, and Rust 1.97 for the benchmark crate.

```sh
cargo build --release -p hayaid
```

The default build uses the official Zcash crates. To build with Zakura's crates instead, which
the NU7 rules on Testnet need today:

```sh
cargo build --release -p hayaid --no-default-features --features zakura
```

## 📖 Usage

```sh
# Write a default configuration, then start the node
target/release/hayaid generate -o hayaid.toml
target/release/hayaid -c hayaid.toml start

# The height of the chain that a stopped node holds on disk
target/release/hayaid tip-height -n mainnet -c /var/lib/hayai

# Two local nodes on Regtest, 10 blocks, with joined traces
scripts/regtest_pair.sh --blocks 10
```

hayai accepts the command forms of `zakurad` and most of its configuration keys. The
[Zakura compatibility guide](docs/zakura-compat.md) lists each key and command. The
[node guide](docs/hayaid.md) explains the modes, the configuration, the RPC methods and the
limits.

## 📊 Monitoring

hayaid serves Prometheus metrics, with the main metrics also under Zakura's names, so an
existing Zakura dashboard works. The `docker/` directory has a complete monitoring stack:
Prometheus, Grafana dashboards and Alertmanager rules. The
[deployment guide](docs/deploy.md) describes each file.

## 🧪 Consensus and correctness

A node that is fast and wrong loses blocks for its miner and can split the chain. hayai uses
several layers of checks:

- **Rule by rule:** [docs/consensus.md](docs/consensus.md) maps each consensus rule of the ZIPs
  and of the protocol specification to the line of code that enforces it and to its test.
- **Published test vectors:** transaction ids, signature hashes, tree roots, scripts and 90 real
  blocks.
- **Comparison with the reference:** a pair of hayai and a real Zakura node on a private chain
  gives the same verdict on every valid block and on invalid blocks.
- **Real network:** hayai synced the whole public Testnet from genesis, across NU7.
- **Reviews:** two independent reviews, and a differential fuzz search against Zakura's checks.

## 🏎️ Benchmarks

```sh
cargo bench -p hayai-bench
python3 scripts/collect_bench.py
python3 scripts/report.py
```

The benchmarks compare hayai with Zakura's and Zebra's code on the same machine. The
[report](docs/report.html) presents the results. The [sync race runbook](docs/sync-race.md)
describes a comparison of a Zakura node and a hayai node on 2 equal machines.

## 🗂️ Project layout

| Crate | Purpose |
|---|---|
| `hayaid` | The node binary |
| `hayai-consensus` | Network parameters, rule sets of each upgrade, checkpoints |
| `hayai-validate` | Block validation |
| `hayai-prepared` | Transactions verified one time, mempool policy |
| `hayai-state` | Chain state in memory, contextual checks |
| `hayai-coins` | Coin set and its storage |
| `hayai-trees` | Note commitment trees |
| `hayai-sinsemilla` | Fast Sinsemilla hash for the Orchard tree |
| `hayai-sync` | Header chain and block download |
| `hayai-net` | Peer-to-peer network |
| `hayai-relay` | Compact block relay protocol |
| `hayai-template` | Mining template |
| `hayai-rpc` | JSON-RPC server and metrics |
| `hayai-index` | Optional wallet index |
| `hayai-blockstore` | Block files |
| `hayai-wire` | Parse and encoding of blocks and transactions |
| `hayai-crypto` | The one entry point to the cryptography crates |
| `hayai-trace` | Trace files |
| `hayai-bench` | Benchmarks and comparisons with Zakura and Zebra |
| `hayai-fuzz` | Differential fuzzer against Zakura's checks |

The [architecture document](docs/architecture.md) explains how they fit together.

## 📚 Documentation

| Document | Content |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Crates, data flow and design principles |
| [docs/hayaid.md](docs/hayaid.md) | The node: modes, configuration, RPC, traces, metrics, limits |
| [docs/install.md](docs/install.md) | Installation with Docker Compose, systemd or Terraform on AWS |
| [docs/zakura-compat.md](docs/zakura-compat.md) | hayai for an operator of Zakura |
| [docs/consensus.md](docs/consensus.md) | Each consensus rule, its code and its test |
| [docs/protocol-compact-relay.md](docs/protocol-compact-relay.md) | The block and transaction relay protocol |
| [docs/protocol-template-push.md](docs/protocol-template-push.md) | Template delivery to pools |
| [docs/mempool-policy.md](docs/mempool-policy.md) | Mempool and fee policy |
| [docs/deploy.md](docs/deploy.md) | Deployment, monitoring and CI files |
| [docs/sync-race.md](docs/sync-race.md) | Comparison of a Zakura node and a hayai node |
| [CHANGELOG.md](CHANGELOG.md) | Changes of each version |

## 🗺️ Status and roadmap

**Done:** the consensus rules up to NU7, full sync from the network, the relay and template
protocols, the deployment files, and a full sync of the public Testnet.

**Next:**
- A comparison with Zakura on Mainnet, on 2 equal machines.
- A private network with full blocks, to measure the relay gain between hayai nodes.
- TLS for the RPC port, and compaction of the state and header logs.
- NU7 with the official Zcash crates, when they support its branch id.

## 🤝 Contributing

Issues and pull requests are welcome. Before a pull request, run the checks of CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --exclude hayai-fuzz --no-default-features --features upstream --all-targets -- -D warnings
cargo test --workspace --exclude hayai-fuzz --no-default-features --features upstream --release -- --skip slow::
```

Code that touches consensus needs a test at the boundary of each rule. The
[CLAUDE.md](CLAUDE.md) file states the code rules of the project, and
[CHANGES.md](CHANGES.md) records the design decisions and their reasons.

## 🔒 Security

Please report a security problem privately, through a
[GitHub security advisory](https://github.com/nikkolasg/hayai/security/advisories/new), and
not in a public issue. A problem that can split the chain or stop a node has the highest
priority.

## 📄 License

hayai is available under the terms of either the MIT license or the Apache License 2.0, at
your option.

`third-party/README.md` gives each source of code, data or designs, its licence and the files
that hold its code or data. The licence files of each source are in `third-party/`.

## 🙏 Acknowledgements

hayai builds on the work of the Zcash community: the protocol specification and the ZIPs, the
Zcash crates of the Electric Coin Company, Zebra of the Zcash Foundation, and Zakura. The
Zakura and Zebra code is the reference that hayai compares itself with, rule by rule.
