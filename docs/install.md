# hayai installation

Date: 2026-10-04. Scope: install, check, upgrade and back up a hayaid node with Docker
Compose, with systemd on a Linux host, or on AWS with Terraform. `docs/deploy.md` lists
the files. `docs/hayaid.md` describes the binary, its modes and its configuration.

## Limits

| Limit | Effect on a deployment | Issue |
|---|---|---|
| Networks: Testnet and Mainnet (shadow mode), Regtest (full mode) | The testnet and mainnet profiles run zakurad beside hayaid | — |
| Mainnet: some consensus rules are not enforced (Mainnet) | The operator decides whether the gaps are acceptable | — |
| No synchronization of old blocks | A Regtest node must run before the first block | — |

## Mainnet

The mainnet profile runs hayaid in shadow mode on Mainnet. No code refuses Mainnet. These
consensus rules are not enforced (`docs/hayaid.md`, Mainnet, and `docs/consensus-rules.md`):

- Difficulty adjustment: the node trusts the `bits` of the first 17 headers after its
  start, because the rule reads 28 blocks of context. It checks every later header.
- Funding streams: the amounts and scripts of the funding outputs of the coinbase are not
  checked.
- ZIP 213: the shielded outputs of the coinbase are not decrypted.
- Sprout JoinSplits and ZIP 234 issuance: the validator returns
  `Unsupported`. A shadow node stops when upstream accepts such a block.

The operator decides whether the gaps are acceptable.

## Prerequisites

| Target | Requirements |
|---|---|
| Docker Compose | Linux host, Docker Engine 24 or later with the Compose plugin (v2.20 or later) and BuildKit. For Testnet: a checkout of the Zakura repository. |
| Bare metal | Linux with systemd, `sudo`, `curl`, `b2sum` (GNU coreutils). To build from source: the Rust toolchain 1.91 or later (1.97 to build `hayai-bench` as well), `clang`, `libclang-dev`, a C++ compiler. For Testnet: a zakurad on the same host (`docs/hayaid.md`, Testnet shadow). |
| AWS | Terraform 1.5 or later, AWS credentials, the AWS CLI with the Session Manager plugin, Git URLs of hayai and Zakura that the instance can clone without credentials. |

## Hardware sizing

| Resource | Size | Reason |
|---|---|---|
| Memory | 16 GiB | The memory backend holds the coin set and the nullifier sets. At Mainnet size, they use 3.84 GB after a load (`docs/architecture.md`, Mainnet sizing of `MemBacking`). The Testnet set is smaller. zakurad runs on the same host in the testnet profile. |
| CPU | 4 vCPU or more | Validation uses one rayon thread per logical CPU. Halo2 batch verification scales with the thread count (`CHANGES.md`, CPU and memory review). |
| Disk | 300 GB SSD (gp3: 3000 IOPS, 125 MiB/s) | Block files, the coins snapshot (3.67 GB at Mainnet size) and log, the Zakura state, Prometheus (30 days). |
| Network | One public TCP port | Testnet: 18233 (zakurad). Mainnet: 8233 (zakurad). Regtest: 18344 (hayaid). |

## Docker Compose

Run every command from the `docker/` directory of the hayai checkout. `docker/.env` sets
the profile (`COMPOSE_PROFILES=testnet`) and binds the admin ports to 127.0.0.1.

### Testnet

1. Build the zakurad image in the Zakura checkout:

   ```
   docker build -t zakurad:local -f docker/Dockerfile --target runtime .
   ```

2. Build the hayaid image:

   ```
   docker compose build
   ```

3. Create the Grafana admin password:

   ```
   install -d -m 0700 secrets
   (umask 022 && openssl rand -base64 24 > secrets/grafana_admin_password)
   ```

4. Start zakurad and wait until its state is `healthy` (`/ready`: at most 2 blocks
   behind the Testnet tip). A new volume takes hours.

   ```
   docker compose up -d zakurad
   docker compose ps zakurad
   ```

5. Start hayaid and the observability services:

   ```
   docker compose -f compose.yml -f compose.observability.yml up -d
   ```

   Compose starts `hayaid-testnet` only after zakurad is healthy. hayaid then reads its
   start state from zakurad at the Zakura tip.

### Mainnet

The steps are the steps of Testnet with the profile `mainnet`. The services are
`zakurad-mainnet` and `hayaid-mainnet`.

```
export COMPOSE_PROFILES=mainnet
docker compose build
docker compose up -d zakurad-mainnet
docker compose ps zakurad-mainnet
docker compose -f compose.yml -f compose.observability.yml up -d
```

Create the Grafana password first (Testnet, step 3). Build the `zakurad:local` image first
(Testnet, step 1). A new `zakurad-mainnet` volume needs days to synchronize.

### Regtest

```
docker compose build
install -d -m 0700 secrets
(umask 022 && openssl rand -base64 24 > secrets/grafana_admin_password)
COMPOSE_PROFILES=regtest docker compose -f compose.yml -f compose.observability.yml up -d
curl -s -u "$(docker compose exec -T hayaid-regtest cat /var/lib/hayai/.cookie)" \
  -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"generate","params":[10]}' http://127.0.0.1:18345/
```

The RPC server has cookie authentication. The node writes the credentials to
`/var/lib/hayai/.cookie` at each start (`docs/hayaid.md`, Protection of the port). On
bare metal, read the file as root: `sudo cat /var/lib/hayai/.cookie`. The server has no
TLS: use the port through 127.0.0.1 or through an SSH tunnel.

To make Regtest the default, set `COMPOSE_PROFILES=regtest` in `docker/.env`.

### Options

- Zakura crypto backend: set `HAYAI_CRYPTO_BACKEND=zakura` and
  `HAYAI_IMAGE=hayaid:zakura` in `docker/.env`, then run `docker compose build`.
- Configuration: edit `docker/config/hayaid.testnet.toml`, `docker/config/hayaid.mainnet.toml`
  or `docker/config/hayaid.regtest.toml`. The containers mount them read-only.
- Remote access to the admin ports: keep `HAYAI_ADMIN_BIND=127.0.0.1` and use an SSH
  port forward, for example `ssh -L 3000:127.0.0.1:3000 node.example.com`.

## Bare metal with systemd

1. Build and install as a normal user. The script asks for `sudo` after the build.

   ```
   scripts/install.sh --network testnet
   ```

   To install a binary that is already built:

   ```
   sudo scripts/install.sh --network testnet --binary /path/to/hayaid
   ```

2. Edit `/etc/hayai/hayaid.toml`:
   - Testnet and Mainnet: set `[network] peers` and `[shadow] rpc_addr` to the P2P and RPC
     addresses of the local zakurad.
   - Set `[metrics] endpoint_addr` to an address that Prometheus reaches.
3. Wait until zakurad is at the tip of the network.
4. Start the service:

   ```
   sudo systemctl start hayaid
   ```

The script is idempotent. It keeps an existing `/etc/hayai/hayaid.toml`. To remove the
service and the binary, run `sudo scripts/install.sh --uninstall`. To also remove the
configuration, `/var/lib/hayai` and the user, add `--purge`.

Prometheus outside the compose stack: add a scrape job for the metrics address and copy
`docker/observability/prometheus/rules/hayaid.yml`. Set the label `network` on the
target, because the rules use it.

## AWS with Terraform

1. Write the variables:

   ```
   cd deploy/terraform/aws
   cp terraform.tfvars.example terraform.tfvars   # then edit
   ```

2. Create the resources:

   ```
   terraform init
   terraform apply
   ```

3. Follow the first boot (package install, clone, image builds, compose start):

   ```
   $(terraform output -raw ssm_shell_command)
   sudo journalctl -u hayai-bootstrap -u hayai-stack -f
   ```

   On Testnet and Mainnet, `hayai-stack` waits until zakurad is healthy, which takes hours.

4. Open Grafana through Session Manager:

   ```
   $(terraform output -raw grafana_port_forward_command)
   ```

   Then open `http://localhost:3000`. The user is `admin`. The password is in the file that
   `terraform output -raw grafana_password_command` names.

The instance has no SSH key and no open port 22 by default. With `admin_cidr`, the
security group opens the admin ports to that CIDR, and compose binds them on all
addresses. Without `admin_cidr`, the admin ports are reachable through Session Manager
port forwards only. `terraform destroy` removes the instance and the data volume.

## First-start checks

| Check | Docker Compose (Testnet) | Bare metal |
|---|---|---|
| Log | `docker compose logs -f hayaid-testnet` (`hayaid-mainnet` on Mainnet) | `journalctl -u hayaid -f` |
| The node started | Log line `hayaid started` with the P2P, RPC and metrics addresses | same |
| Metrics endpoint | `curl -s http://127.0.0.1:19101/metrics \| head` | same address as `[metrics] endpoint_addr` |
| Tip height | `curl -s http://127.0.0.1:19101/metrics \| grep '^state_memory_best_committed_block_height'` | same |
| Tip height (Regtest RPC) | `curl -s -u "$(docker compose exec -T hayaid-regtest cat /var/lib/hayai/.cookie)" -H 'content-type: application/json' --data '{"jsonrpc":"2.0","id":1,"method":"getblockcount","params":[]}' http://127.0.0.1:18345/` | same, with `-u "$(sudo cat /var/lib/hayai/.cookie)"` |
| Peers | `curl -s http://127.0.0.1:19101/metrics \| grep '^hayai_peers'`: 1 in shadow mode (the Zakura node) | same |
| Shadow agreement | `hayai_shadow_agreements_total` increases with each block; `hayai_shadow_disagreements_total` stays 0 | same |
| Prometheus targets | `http://127.0.0.1:9090/targets`: jobs `hayaid`, `node` and (Testnet) `zakurad` are up | — |
| Grafana | `http://127.0.0.1:3000`, user `admin`, password from `docker/secrets/grafana_admin_password`; dashboard `hayai / hayaid` | — |
| Alerts | `http://127.0.0.1:9093`: no alert after 30 minutes | — |

## Restart and upgrade

hayaid resumes from `cache_dir` (`docs/hayaid.md`, Restart). A restart and an upgrade need no
reset. The node replays the blocks above its last flush from the block files, so a start
takes longer after a crash than after a clean stop. The node refuses a `cache_dir` of another
network or mode.

Docker Compose (Testnet; for Mainnet, use `hayaid-mainnet`; for Regtest, use `hayaid-regtest`):

1. Update the checkout and build the image:

   ```
   git pull
   docker compose build
   ```

2. Stop hayaid. A clean stop flushes the coins and writes a snapshot.

   ```
   docker compose stop hayaid-testnet
   ```

3. Start the stack again:

   ```
   docker compose -f compose.yml -f compose.observability.yml up -d
   ```

Bare metal:

```
git pull
sudo systemctl stop hayaid
scripts/install.sh --network testnet          # builds, installs, keeps the configuration
sudo systemctl start hayaid
```

To start a node from a new start state, stop it and remove the content of `cache_dir`
(`/var/lib/hayai`) except the traces. In Docker Compose:

```
docker run --rm -v hayai_hayaid-testnet-data:/var/lib/hayai --entrypoint sh hayaid:local \
  -c 'rm -rf /var/lib/hayai/coins /var/lib/hayai/blocks /var/lib/hayai/state.log /var/lib/hayai/spent.log'
```

AWS: open a shell with `ssm_shell_command`, then run the Docker Compose procedure in
`/opt/hayai/src/docker` as root, with `git -C /opt/hayai/src checkout <ref>` in place of
`git pull`. The `hayai-stack` unit sets `COMPOSE_PROFILES` and `HAYAI_ADMIN_BIND`; export
the same values in the shell (`systemctl cat hayai-stack`).

## Backup and restore

| Data | Location (Docker volume / bare metal) | Restore |
|---|---|---|
| Coins snapshot and log | `hayai_hayaid-<network>-data` / `/var/lib/hayai`: `coins/coins.snapshot`, `coins/coins.log` | Copy back with the block files and the state logs; hayaid resumes |
| Block files and their index | same volume: `blocks/blk-NNNNN.dat`, `blocks/` RocksDB index | Copy back with the coins files |
| State logs | same volume: `state.log`, `spent.log` (shadow mode) | Copy back with the coins files |
| Traces | same volume: `traces/*.jsonl` | Copy back; the writer appends |
| Configuration | `docker/config/` / `/etc/hayai/hayaid.toml` | Copy back |
| Zakura state | `hayai_zakurad-testnet-data` | Copy back; zakurad resumes and skips hours of synchronization |
| Grafana, Prometheus | `hayai_grafana-data`, `hayai_prometheus-data` | Copy back |

Back up a volume while its service is stopped. A clean stop of hayaid flushes the coins and
writes a coins snapshot, so the copy is consistent. Copy the coins files, the block files
and the state logs together. Example for the Zakura state:

```
docker compose stop hayaid-testnet zakurad
docker run --rm -v hayai_zakurad-testnet-data:/data:ro -v "$PWD":/backup debian:bookworm-slim \
  tar -C /data -czf /backup/zakurad-testnet.tar.gz .
docker compose up -d zakurad
```

Restore a volume with the same container and `tar -C /data -xzf`, before the service
starts. Bare metal: stop the service and copy `/var/lib/hayai` with `tar`.

## Troubleshooting

| Symptom | Cause | Action |
|---|---|---|
| `hayaid: /var/lib/hayai/coins is not empty and cache_dir has no state.log ...` | The directory holds files of another program, or an incomplete copy | Remove the cause, or use an empty `cache_dir` |
| `hayaid: state log: ... belongs to a regtest/full node` | The configuration names another network or mode than the first start | Correct the configuration, or use an empty `cache_dir` |
| `hayaid: shadow seed: upstream connection: Connection refused` | zakurad does not answer on its RPC address | Check zakurad (`docker compose logs zakurad`). The failed start left `cache_dir` as it was; start again |
| `docker compose up` waits and `hayaid-testnet` does not start | zakurad is not healthy: it is not at the Testnet tip yet | Wait. Follow `docker compose logs -f zakurad` |
| systemd: `Start request repeated too quickly` | Five failed starts in 10 minutes | Read `journalctl -u hayaid`, fix the cause, then `sudo systemctl reset-failed hayaid` |
| ``unknown field `sapling_params_dir` `` | The configuration is from a version that read the Sapling parameter files | Remove the line. The Sapling verifying keys are in the binary |
| `secret "grafana_admin_password" ... no such file` | The password file is missing | Create it (Docker Compose, step 3) |
| The Grafana password does not work | Grafana sets the admin password once, when it creates its database | `docker compose exec grafana grafana cli admin reset-admin-password "$(cat secrets/grafana_admin_password)"` |
| `bind: address already in use` | Another process uses a published port | Stop that process, or change the host port in `docker/compose.yml` |
| Alert `HayaidShadowDisagreement` and the node stops | hayai rejected a block that upstream accepted, or the tree roots differ | Keep the volume. Read the `upstream_verdict` rows in `traces/commit_state.jsonl` and the log, then report the block |
| Alert `HayaidAbsent` | No hayaid target: the container is stopped | `docker compose ps`, then the log of the node |
