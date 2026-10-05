# Sync race of zakurad and hayaid on Testnet

Date: 2026-10-05. Zakura: 1.6.0, commit `13779158253cfe315f73eadffb9b4c93c25e82a5`.
hayaid: the commit of the checkout that deploys, with the zakura crypto backend.

## Purpose

One `zakurad` and one `hayaid` do a full sync of the public Testnet from an empty data
directory, at the same time, on two equal machines. A third machine records both. The
result is the time of each node to the last checkpoint (block 4,023,200) and to the tip,
and the CPU, memory, disk and network use on the way.

```
 machine A                 machine B                 machine C (small)
 zakurad      :18233 P2P   hayaid       :18233 P2P   Prometheus (loopback)
 node-exporter             node-exporter             Grafana :3000
   :9999 :9100  <──────────  :19101 :9100  <──────── scrape each 15 s
```

Files: `docker/race/` (compose files, node configurations, Prometheus rules, dashboard),
`deploy/terraform/aws-race/`, `scripts/race_deploy.sh`.

## Machine size

| Item | Value for A and B | Source |
|---|---|---|
| CPU and memory | 8 vCPUs, 32 GiB (`m7i.2xlarge`) | Zakura `docs/zcashd-compat.md`, hardware table: 4 CPUs and 16 GiB minimum, 8 CPUs and 32 GiB recommended. Zakura `docker/docker-compose.yml` reserves 4 CPUs and 16 GiB |
| Data disk | gp3, 200 GiB, 3,000 IOPS, 125 MiB/s | Estimate. Zakura gives 30 GiB minimum and 100 GiB recommended for Testnet in the same table. The disk also holds the image build (about 20 GiB, estimate) |
| Machine C | 2 vCPUs, 2 GiB (`t3.small`), 30 GiB | Estimate: 2 nodes and 2 node-exporters at one scrape each 15 s |

Not measured: the memory and the disk that hayaid needs for Testnet. Its default coins
store keeps the coin set in memory (Mainnet: about 3.8 GB, `docs/architecture.md`), and its
block files keep each block. Read "Machine memory in use" and "Disk in use" on the
dashboard in the first hours.

## Deployment with Terraform (AWS)

```sh
cd deploy/terraform/aws-race
cp terraform.tfvars.example terraform.tfvars    # region, repositories, hayai_ref, start_at, admin_cidr
terraform init
terraform apply
terraform output grafana_url
```

- A and B have the same instance type, the same gp3 volume, and the same subnet and
  availability zone. Each one builds its image on its first boot (`zakura_ref` has the
  commit above as its default; `hayai_ref` must be a full commit hash).
- `start_at` is the UTC minute of the start. Each node machine waits for it on its own
  clock (chrony). Set it 90 minutes or more after the apply (estimate of the build time).
- After `start_at`, run the value of `terraform output start_log_command` on A and on B.
  `late_seconds` must be 0 on both. If it is not, the race is not valid: destroy and apply
  again with a later `start_at`.
- Security groups: P2P (TCP 18233, and UDP 8234 for the Zakura stack of zakurad) from each
  address; the metrics ports from machine C only; SSH and Grafana from `admin_cidr`; no
  RPC port.

## Deployment with the script (any three hosts with Docker and SSH)

```sh
ZAKURA_SRC=~/src/zakura scripts/race_deploy.sh build      # Zakura checkout at the commit above
scripts/race_deploy.sh start   user@a.example user@b.example user@c.example
scripts/race_deploy.sh status  user@a.example user@b.example user@c.example
scripts/race_deploy.sh collect user@a.example user@b.example user@c.example
scripts/race_deploy.sh stop    user@a.example user@b.example user@c.example
```

- `build` makes `zakurad:race` and `hayaid:race` on this machine. `start` copies each image
  to its host, so both hosts run the same binaries as a later run.
- `start` stops when a host has data of a race. It starts both nodes at the second full
  minute after its last step, on the clock of each host: the hosts need NTP or chrony.
  `status` shows `planned_start_epoch` and `actual_start_epoch` of each node.
- `start` sets iptables rules on A and B that close ports 9999, 19101 and 9100 to each
  host but C. They need `sudo` without a password. Set `RACE_MONITOR_IP` when A and B see
  C under another address than the SSH target. Open TCP 18233 on A and B, and UDP 8234
  on A, in the firewall of the provider.
- Grafana: `http://c.example:3000`, user `admin`, password in
  `hayai-race/secrets/grafana_admin_password` on C.

## Dashboard

Dashboard "sync race". Each panel has one line for each node (label `node`).

| Panel | Reading |
|---|---|
| Time to the last checkpoint | Seconds from the first scrape of the node to block 4,023,200. "not yet" before that |
| Time to the tip | Seconds to the first time that the block height is 2 blocks or less below the reference: the highest header chain of the two nodes or the network tip that zakurad estimates |
| Block height, Blocks behind the other node | The progress now |
| Header height | The header chain. zakurad has a line only when its Zakura stack syncs headers |
| Blocks per second | Mean of 1 minute |
| Peers | For zakurad: the peers of its legacy stack |
| Machine CPU, memory, disk, disk I/O, network | node-exporter of the machine of the node. Each machine runs one node |

The annotations mark the first scrape, the last checkpoint and the tip of each node. The
resolution of each time is 15 s. The Docker build of Zakura exports no process metrics, so
the dashboard compares the machines. `status` and `collect` add `docker stats` of each
node container.

## Fairness rules

1. Same machine type, disk type and size, region and availability zone for A and B.
2. Both nodes start in the same minute. Record the planned and the actual start.
3. Empty data directories. `start` refuses a host with data.
4. The nodes are not peers of each other. Each one finds its peers with the DNS seeders
   of its defaults. Both have `peerset_initial_target_size = 25` and
   `max_connections_per_ip = 1`.
5. Run the race two times, the second time with the machines exchanged:
   run `collect`, then
   `scripts/race_deploy.sh swap user@a.example user@b.example user@c.example`. It removes
   the data on the three hosts (it asks first) and starts zakurad on B and hayaid on A. With Terraform: `terraform destroy`, then a new
   apply (new machines of the same type), and compare the two runs.
6. Record the versions and commits of both nodes: `collect` writes `*-version.txt` and
   `*-info.txt` (image id, commit label, start times).
7. Known differences that stay: zakurad runs its default P2P stack for Testnet (the legacy
   stack and the Zakura stack with its bootstrap peers); hayaid runs the legacy protocol
   only. hayaid writes trace tables; zakurad does not.

## Report when hayaid stops

A node that stops stays stopped (`restart: "no"`). Run `collect` and send the directory
`race-results/<time>/`. It has:

- `race-hayaid.log`: the last 20,000 log lines. The last `ERROR` line and the line
  `shutting down reason="fatal error: ..."` name the cause;
- `hayaid-fault-rows.jsonl`: the trace rows with `not_validated`, an invalid result or a
  fault, and `traces/`: all trace tables;
- `race-hayaid-metrics.txt` (absent when the node is stopped), `race-series.json`: the
  height over time; the last height is in the "Block height" panel;
- the same files of zakurad, and the versions.

Also send the output of `status`.

## Known risks

- hayaid never synced a public chain. The first stop is the expected result of the first
  race. Each stop is a finding.
- The block before NU7 on Testnet has the NSM seed check: the total of the value pools
  must give the balance 55,768,414,957 zatoshis (`docs/consensus-rules.md`). An error of
  hayaid in a value pool of the history shows there, and the node stops.
- The NU7 rules need the image with the zakura crypto backend
  (`--build-arg CRYPTO_BACKEND=zakura`; both deployment ways build it). An image with the
  upstream backend stops at the NU7 height.
- `peerset_initial_target_size = 25` gives hayaid 37 outbound peers. hayaid ran with 8
  until now.
- The estimates of this page (disk, build time) are not measured.

## Removal

```sh
scripts/race_deploy.sh clean user@a.example user@b.example user@c.example   # containers, volumes, firewall rules
terraform -chdir=deploy/terraform/aws-race destroy                          # the AWS resources
```

`clean` leaves the images and the directory `hayai-race` on each host.

## Dry run on one machine

The same compose files run with the Regtest configurations of `docker/race/config`: all
addresses are on 127.0.0.1, and no node has a peer.

```sh
cd docker/race
export RACE_ZAKURAD_CONFIG=./config/zakurad.regtest.toml RACE_HAYAID_CONFIG=./config/hayaid.regtest.toml
export RACE_NODE_EXPORTER_ADDR=127.0.0.1:39100
docker compose -p race-dryrun-node -f compose.node.yml --profile zakurad --profile hayaid up -d
export RACE_ZAKURAD_HOST=127.0.0.1 RACE_HAYAID_HOST=127.0.0.1 RACE_ZAKURAD_METRICS_PORT=39999
export RACE_HAYAID_METRICS_PORT=39101 RACE_ZAKURAD_EXPORTER_PORT=39100 RACE_HAYAID_EXPORTER_PORT=39100
export RACE_PROMETHEUS_ADDR=127.0.0.1:39090 RACE_GRAFANA_ADDR=127.0.0.1 RACE_GRAFANA_PORT=33000
mkdir -p secrets && (umask 022 && head -c 18 /dev/urandom | base64 >secrets/grafana_admin_password)
docker compose -p race-dryrun-monitor -f compose.monitor.yml up -d
```

Set `RACE_ZAKURA_IMAGE` and `RACE_HAYAI_IMAGE` when the images have other names than
`zakurad:race` and `hayaid:race`. The `generate` method of each RPC server (ports 38345 and 38232, with the cookie) makes
blocks. `docker compose -p <project> ... down --volumes` removes each project.
