#!/usr/bin/env bash
# Deploys the race of one zakurad and one hayaid (docs/sync-race.md) on three hosts
# that have Docker with the compose plugin and SSH access. The files are those of
# docker/race.
#
# Usage: scripts/race_deploy.sh COMMAND [A B C]
#
#   A  SSH target of the machine of zakurad, for example user@a.example
#   B  SSH target of the machine of hayaid
#   C  SSH target of the monitoring machine (Prometheus and Grafana)
#   The target `local` runs the commands of that host on this machine, without SSH
#   (the dry run of docs/sync-race.md).
#
# Commands:
#   build    Build the two images on this machine. It needs no host.
#   start    Copy the files and the images, start the monitoring on C, then start both
#            nodes at the same minute, each one with its RPC caller
#            (scripts/race_rpc_caller.py) and its sidecar (scripts/race_sidecar.py). It
#            stops when a node host has data of a race, or when the Docker of A or B
#            does not use cgroup v2 with the systemd cgroup driver (the sidecar reads
#            the cgroup of the node container).
#   status   Containers, resources, height, peers and last log line of each node, the
#            last line of each RPC caller, the values of each sidecar, and
#            net.ipv4.tcp_slow_start_after_idle of A and B (it must be 0 on both).
#   stop     Stop both nodes, both RPC callers and both sidecars. The data and the
#            monitoring stay.
#   collect  Fetch the versions, the node logs, the metrics, the trace files of both
#            nodes, the files of both RPC callers, the last textfile of both sidecars
#            and the series of the race into race-results/<UTC time>/, and write
#            the table of blocks at the tip (blocks.csv, blocks.md) with
#            scripts/race_blocks.py. It needs python3 on this machine.
#   swap     `clean`, then `start` with the roles of A and B exchanged.
#   clean    Remove the containers, the volumes (node data, Prometheus data) and the
#            firewall rules of the race on the three hosts. It asks first.
#
# Shell variables:
#   RACE_NETWORK      mainnet (default) or testnet. It selects the node configurations
#                     and the crypto backend of the hayaid image (mainnet: the default
#                     build; testnet: zakura). Use the same value for each command.
#   ZAKURA_SRC        Zakura checkout for `build`. Its HEAD must be ZAKURA_COMMIT.
#   RACE_DIR          Directory on each host, below the home directory (hayai-race).
#   RACE_ZAKURAD_HOST Address of A as C reaches it (default: the host part of A).
#   RACE_HAYAID_HOST  Address of B as C reaches it (default: the host part of B).
#   RACE_MONITOR_IP   IPv4 address of C as A and B see it (default: the host part of C,
#                     resolved on A and B).
#   RACE_FIREWALL     1 (default): iptables rules on A and B close the metrics ports to
#                     each host but C. They need sudo without a password. 0: no rule.
#   RACE_CPUS         CPU limit of both node containers (default 0: no limit).
#   RACE_MEMORY       Memory limit of both node containers (default 0: no limit).
#   RACE_LOG_LINES    Lines of each node log that `collect` fetches (default 20000).
#   RACE_CALLER       1 (default): `start` starts an RPC caller beside each node, which
#                     sends one `getblocktemplate` call each RACE_CALLER_INTERVAL
#                     seconds. 0: no RPC caller.
#   RACE_CALLER_INTERVAL  Seconds between two calls of an RPC caller (default 5).
#   RACE_CALLER_LONGPOLL  1 (default): each RPC caller also holds one long poll and
#                     sends one call without `longpollid` after each answer. 0: no long poll.
#   RACE_GRAFANA_ADDR Listen address of Grafana on C (default 127.0.0.1: open it through
#                     an SSH tunnel). Set 0.0.0.0 only behind a firewall that limits port
#                     3000 to the operator.
#   RACE_ZAKURAD_METRICS_PORT, RACE_HAYAID_METRICS_PORT, RACE_PROMETHEUS_ADDR
#                     Other ports than 9999, 19101 and 127.0.0.1:9090, as in
#                     docker/race/compose.monitor.yml (the dry run).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The Zakura commit of the race. docs/sync-race.md names it too.
ZAKURA_COMMIT="13779158253cfe315f73eadffb9b4c93c25e82a5"
ZAKURA_IMAGE="zakurad:race"
HAYAI_IMAGE="hayaid:race"

RACE_NETWORK="${RACE_NETWORK:-mainnet}"
# The crypto backend of the hayaid image: the NU7 rules of Testnet need the zakura
# backend, and Mainnet uses the default build (the upstream Zcash crates).
case "${RACE_NETWORK}" in
  mainnet) HAYAI_BACKEND=upstream P2P_PORT=8233 ;;
  testnet) HAYAI_BACKEND=zakura P2P_PORT=18233 ;;
  *)
    echo "[race_deploy] error: RACE_NETWORK must be mainnet or testnet" >&2
    exit 1
    ;;
esac
RACE_DIR="${RACE_DIR:-hayai-race}"
RACE_FIREWALL="${RACE_FIREWALL:-1}"
RACE_CPUS="${RACE_CPUS:-0}"
RACE_MEMORY="${RACE_MEMORY:-0}"
RACE_LOG_LINES="${RACE_LOG_LINES:-20000}"
RACE_CALLER="${RACE_CALLER:-1}"
RACE_CALLER_INTERVAL="${RACE_CALLER_INTERVAL:-5}"
RACE_CALLER_LONGPOLL="${RACE_CALLER_LONGPOLL:-1}"
# The file of an RPC caller in its container (docker/race/compose.node.yml).
CALLER_FILE="/var/lib/race-caller/getblocktemplate.jsonl"
ZAKURAD_METRICS_PORT="${RACE_ZAKURAD_METRICS_PORT:-9999}"
HAYAID_METRICS_PORT="${RACE_HAYAID_METRICS_PORT:-19101}"
EXPORTER_PORT=9100
PROMETHEUS_ADDR="${RACE_PROMETHEUS_ADDR:-127.0.0.1:9090}"
GRAFANA_ADDR="${RACE_GRAFANA_ADDR:-127.0.0.1}"
# The data directory of zakurad in its container (docker/race/config).
ZAKURAD_DATA="/home/zebra/.cache/zakura"
# The scrape interval of docker/race/prometheus/prometheus.yml, in seconds.
SCRAPE_SECONDS=5

log() { echo "[race_deploy] $*"; }
die() {
  echo "[race_deploy] error: $*" >&2
  exit 1
}

usage() {
  sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed -e '$d' -e 's/^# \{0,1\}//'
  exit 2
}

# Runs a command line on a host.
remote() { # HOST COMMAND
  local host=$1
  shift
  if [[ "${host}" == local ]]; then
    bash -c "$*"
    return
  fi
  ssh -o BatchMode=yes -o ConnectTimeout=20 "${host}" "$@"
}

# The host part of an SSH target.
host_of() { # TARGET
  local target="${1#*@}"
  echo "${target%%:*}"
}

# docker compose in the race directory of a host.
node_compose() { # HOST ARGS
  local host=$1
  shift
  remote "${host}" "cd '${RACE_DIR}' && docker compose -f compose.node.yml $*"
}

monitor_compose() { # HOST ARGS
  local host=$1
  shift
  remote "${host}" "cd '${RACE_DIR}' && docker compose -f compose.monitor.yml $*"
}

# GET of a path of the Prometheus API on C, through the Prometheus container.
prometheus_get() { # PATH_AND_QUERY
  remote "${C}" "docker exec race-prometheus wget -qO- 'http://${PROMETHEUS_ADDR}$1'"
}

build() {
  [[ -n "${ZAKURA_SRC:-}" ]] || die "set ZAKURA_SRC to a Zakura checkout at ${ZAKURA_COMMIT}"
  local head revision
  head=$(git -C "${ZAKURA_SRC}" rev-parse HEAD)
  [[ "${head}" == "${ZAKURA_COMMIT}" ]] ||
    die "${ZAKURA_SRC} is at ${head}; the race uses ${ZAKURA_COMMIT}"
  [[ -z "$(git -C "${ZAKURA_SRC}" status --porcelain)" ]] ||
    die "${ZAKURA_SRC} has changes that are not in a commit"
  revision=$(git -C "${REPO}" rev-parse HEAD)
  if [[ -n "$(git -C "${REPO}" status --porcelain)" ]]; then
    revision="${revision}-dirty"
    log "warning: this checkout has changes that are not in a commit"
  fi
  log "building ${ZAKURA_IMAGE} from Zakura ${ZAKURA_COMMIT}"
  docker build -t "${ZAKURA_IMAGE}" -f "${ZAKURA_SRC}/docker/Dockerfile" --target runtime \
    --build-arg "SHORT_SHA=${ZAKURA_COMMIT:0:8}" \
    --label "org.opencontainers.image.revision=${ZAKURA_COMMIT}" "${ZAKURA_SRC}"
  log "building ${HAYAI_IMAGE} from hayai ${revision} (${HAYAI_BACKEND} crypto backend, ${RACE_NETWORK})"
  docker build -t "${HAYAI_IMAGE}" -f "${REPO}/docker/Dockerfile" \
    --build-arg "CRYPTO_BACKEND=${HAYAI_BACKEND}" \
    --label "org.opencontainers.image.revision=${revision}" \
    --label "org.hayai.race.crypto-backend=${HAYAI_BACKEND}" "${REPO}"
}

check_hosts() {
  [[ "$(host_of "${A}")" != "$(host_of "${B}")" ]] ||
    die "the two nodes must not share a machine"
  local host
  for host in "${A}" "${B}" "${C}"; do
    remote "${host}" "docker compose version >/dev/null" ||
      die "${host}: no SSH access, or no Docker with the compose plugin"
  done
  # The sidecar reads /sys/fs/cgroup/race.slice/race-<node>.slice (compose.node.yml).
  local cgroup
  for host in "${A}" "${B}"; do
    cgroup=$(remote "${host}" "docker info --format '{{.CgroupDriver}} {{.CgroupVersion}}'")
    [[ "${cgroup}" == "systemd 2" ]] ||
      die "${host}: Docker has the cgroup driver and version '${cgroup}'; the sidecar needs 'systemd 2'"
  done
}

# Copies docker/race and the program of the RPC caller to a host. The secrets and the
# .env file of a host stay.
copy_files() { # HOST
  remote "$1" "mkdir -p '${RACE_DIR}'"
  tar -C "${REPO}/docker/race" --exclude=./secrets --exclude=./.env -cf - . |
    remote "$1" "tar -C '${RACE_DIR}' -xf -"
  tar -C "${REPO}/scripts" -cf - race_rpc_caller.py race_sidecar.py race_blocks.py |
    remote "$1" "tar -C '${RACE_DIR}' -xf -"
}

# Copies a local image to a host that does not have it.
push_image() { # HOST IMAGE
  local id
  id=$(docker image inspect -f '{{.Id}}' "$2" 2>/dev/null) ||
    die "no local image $2: run the build command first"
  if [[ "$(remote "$1" "docker image inspect -f '{{.Id}}' '$2' 2>/dev/null || true")" == "${id}" ]]; then
    log "$1 has $2"
    return
  fi
  log "copying $2 to $1"
  docker save "$2" | gzip | remote "$1" "gunzip | docker load"
}

# The IPv4 address of C as a node host sees it.
monitor_ip() { # HOST
  if [[ -n "${RACE_MONITOR_IP:-}" ]]; then
    echo "${RACE_MONITOR_IP}"
    return
  fi
  local ip
  ip=$(remote "$1" "getent ahostsv4 '$(host_of "${C}")' | awk 'NR == 1 { print \$1 }'")
  [[ -n "${ip}" ]] || die "$1 cannot resolve $(host_of "${C}"): set RACE_MONITOR_IP"
  echo "${ip}"
}

# Closes the metrics ports of a node host to each host but C. The node and node-exporter
# use the network of the host, so the INPUT chain decides.
firewall() { # HOST PORTS
  [[ "${RACE_FIREWALL}" == 1 ]] || {
    log "warning: no firewall rule on $1: each host that reaches it can read ports $2"
    return
  }
  local ip rule
  ip=$(monitor_ip "$1")
  rule="INPUT ! -i lo -p tcp -m multiport --dports $2 ! -s ${ip} -m comment --comment hayai-race -j DROP"
  remote "$1" "sudo -n iptables -C ${rule} 2>/dev/null || sudo -n iptables -I ${rule}" ||
    die "$1: cannot set the iptables rule (sudo without a password is necessary, or RACE_FIREWALL=0)"
  rule="INPUT ! -i lo -p tcp -m multiport --dports $2 -m comment --comment hayai-race -j DROP"
  remote "$1" "sudo -n ip6tables -C ${rule} 2>/dev/null || sudo -n ip6tables -I ${rule}" ||
    die "$1: cannot set the ip6tables rule"
  log "$1: ports $2 are open to ${ip} only"
}

unfirewall() { # HOST
  local tool
  for tool in iptables ip6tables; do
    remote "$1" "sudo -n ${tool} -S INPUT 2>/dev/null | grep -- 'hayai-race' | sed 's/^-A /-D /' | xargs -r -L1 sudo -n ${tool}" ||
      log "warning: $1: the ${tool} rules of the race are not removed"
  done
}

# Schedules the start of the node of a host, of its sidecar and of its RPC caller at an
# epoch second, in the background of the host, and records the start in race-info.txt.
schedule() { # HOST PROFILE IMAGE EPOCH
  local services="$2 $2-sidecar"
  [[ "${RACE_CALLER}" != 1 ]] || services="${services} $2-caller"
  remote "$1" "cd '${RACE_DIR}' && {
    echo 'profile=$2'
    echo 'planned_start_epoch=$4'
    echo \"image_id=\$(docker image inspect -f '{{.Id}}' '$3')\"
    echo \"image_revision=\$(docker image inspect -f '{{index .Config.Labels \"org.opencontainers.image.revision\"}}' '$3')\"
  } >race-info.txt
  nohup sh -c '
    wait=\$(( $4 - \$(date +%s) ))
    [ \"\$wait\" -gt 0 ] && sleep \"\$wait\"
    echo \"actual_start_epoch=\$(date +%s)\" >>race-info.txt
    docker compose -f compose.node.yml --profile $2 up -d ${services}
  ' >race-start.log 2>&1 </dev/null &"
}

start() {
  check_hosts
  local zakurad_host hayaid_host host volume backend
  # An image of the other network has the wrong crypto backend.
  backend=$(docker image inspect -f '{{index .Config.Labels "org.hayai.race.crypto-backend"}}' "${HAYAI_IMAGE}" 2>/dev/null) ||
    die "no local image ${HAYAI_IMAGE}: run the build command first"
  [[ "${backend}" == "${HAYAI_BACKEND}" ]] ||
    die "${HAYAI_IMAGE} has the crypto backend '${backend}'; ${RACE_NETWORK} needs ${HAYAI_BACKEND}: run the build command with RACE_NETWORK=${RACE_NETWORK}"
  zakurad_host="${RACE_ZAKURAD_HOST:-$(host_of "${A}")}"
  hayaid_host="${RACE_HAYAID_HOST:-$(host_of "${B}")}"
  # The race starts from empty data directories.
  for host in "${A}" "${B}"; do
    for volume in race-node_zakurad-data race-node_hayaid-data; do
      if remote "${host}" "docker volume inspect '${volume}' >/dev/null 2>&1"; then
        die "${host} has the volume ${volume} of a race: run the collect command, then the clean command"
      fi
    done
  done
  if remote "${C}" "docker volume inspect race-monitor_prometheus-data >/dev/null 2>&1"; then
    die "${C} has the Prometheus data of a race: run the collect command, then the clean command"
  fi
  # The iptables rules come after the monitoring is up. A sudo failure then would leave
  # the Prometheus data of a race on C, so the check runs before any start.
  if [[ "${RACE_FIREWALL}" == 1 ]]; then
    for host in "${A}" "${B}"; do
      remote "${host}" "sudo -n true" ||
        die "${host}: sudo without a password is necessary for the iptables rules (or RACE_FIREWALL=0)"
    done
  fi

  for host in "${A}" "${B}" "${C}"; do
    copy_files "${host}"
  done
  push_image "${A}" "${ZAKURA_IMAGE}"
  push_image "${B}" "${HAYAI_IMAGE}"
  for host in "${A}" "${B}"; do
    remote "${host}" "cd '${RACE_DIR}' && printf '%s\n' 'RACE_NETWORK=${RACE_NETWORK}' \
      'RACE_ZAKURA_IMAGE=${ZAKURA_IMAGE}' 'RACE_HAYAI_IMAGE=${HAYAI_IMAGE}' \
      'RACE_CPUS=${RACE_CPUS}' 'RACE_MEMORY=${RACE_MEMORY}' \
      'RACE_CALLER_SCRIPT=./race_rpc_caller.py' 'RACE_CALLER_INTERVAL=${RACE_CALLER_INTERVAL}' \
      'RACE_CALLER_LONGPOLL=${RACE_CALLER_LONGPOLL}' 'RACE_SCRIPTS_DIR=.' >.env"
  done
  # The image of the sidecar and of the RPC caller (one image) is on each host before
  # the start time.
  node_compose "${A}" "--profile zakurad pull -q zakurad-sidecar"
  node_compose "${B}" "--profile hayaid pull -q hayaid-sidecar"
  [[ "${RACE_CALLER}" == 1 ]] ||
    log "RACE_CALLER=0: no RPC caller; the getblocktemplate panels and the template columns stay empty"

  log "starting Prometheus and Grafana on ${C}"
  # The directory keeps the password private. The file stays readable: Compose mounts it
  # with its mode of the host, and Grafana runs as uid 472.
  remote "${C}" "cd '${RACE_DIR}' && printf '%s\n' 'RACE_NETWORK=${RACE_NETWORK}' \
    'RACE_ZAKURAD_HOST=${zakurad_host}' 'RACE_HAYAID_HOST=${hayaid_host}' \
    'RACE_GRAFANA_ADDR=${GRAFANA_ADDR}' >.env &&
    mkdir -p secrets && chmod 700 secrets &&
    { [ -f secrets/grafana_admin_password ] ||
      (umask 022 && head -c 18 /dev/urandom | base64 >secrets/grafana_admin_password); }"
  monitor_compose "${C}" "up -d"

  firewall "${A}" "${ZAKURAD_METRICS_PORT},${EXPORTER_PORT}"
  firewall "${B}" "${HAYAID_METRICS_PORT},${EXPORTER_PORT}"
  node_compose "${A}" "up -d node-exporter"
  node_compose "${B}" "up -d node-exporter"

  # The start of both nodes: the second full minute from now, on the clock of each
  # host. The hosts need a synchronized clock (NTP or chrony).
  local epoch
  epoch=$((($(date +%s) / 60 + 2) * 60))
  schedule "${A}" zakurad "${ZAKURA_IMAGE}" "${epoch}"
  schedule "${B}" hayaid "${HAYAI_IMAGE}" "${epoch}"
  log "both nodes start on ${RACE_NETWORK} at $(date -u -d "@${epoch}" +%Y-%m-%dT%H:%M:%SZ) (epoch ${epoch})"
  log "the firewall of the provider must open TCP ${P2P_PORT} on ${A} and ${B}"
  if [[ "${GRAFANA_ADDR}" == 127.0.0.1 ]]; then
    log "Grafana: ssh -L 3000:127.0.0.1:3000 ${C}, then http://localhost:3000 (user admin, password in ${RACE_DIR}/secrets/grafana_admin_password on ${C})"
  else
    log "Grafana: http://$(host_of "${C}"):3000 (user admin, password in ${RACE_DIR}/secrets/grafana_admin_password on ${C})"
  fi
  log "run the status command after that time"
}

node_status() { # HOST PROFILE CONTAINER METRICS_PORT
  log "$2 on $1"
  remote "$1" "cd '${RACE_DIR}' && cat race-info.txt 2>/dev/null
    docker compose -f compose.node.yml --profile $2 ps --all
    docker stats --no-stream --format 'cpu={{.CPUPerc}} memory={{.MemUsage}}' $3 2>/dev/null
    docker exec race-node-exporter wget -qO- http://127.0.0.1:$4/metrics 2>/dev/null |
      grep -E '^(zcash_chain_verified_block_height|state_finalized_block_height|zcash_net_peers) '
    # zakurad writes its log to a file of its data volume (docker/race/config).
    { docker exec $3 tail -n 1 /home/zebra/.cache/zakura/zakurad.log 2>/dev/null ||
      docker logs --tail 1 $3 2>&1; }
    docker exec $3-caller tail -n 1 '${CALLER_FILE}' 2>/dev/null ||
      echo 'no line of the RPC caller'
    docker exec $3-sidecar grep -v '^#' /textfile/race_$2.prom 2>/dev/null ||
      echo 'no textfile of the sidecar'
    echo \"net.ipv4.tcp_slow_start_after_idle=\$(sysctl -n net.ipv4.tcp_slow_start_after_idle)\"" ||
    log "warning: no status of $1"
}

status() {
  node_status "${A}" zakurad race-zakurad "${ZAKURAD_METRICS_PORT}"
  node_status "${B}" hayaid race-hayaid "${HAYAID_METRICS_PORT}"
  log "monitoring on ${C}"
  monitor_compose "${C}" "ps --all" || log "warning: no status of ${C}"
}

stop() {
  node_compose "${A}" "--profile zakurad stop zakurad-caller zakurad-sidecar zakurad"
  node_compose "${B}" "--profile hayaid stop hayaid-caller hayaid-sidecar hayaid"
  log "both nodes, both RPC callers and both sidecars are stopped; the data and the monitoring stay"
}

# A file of a container, also of a stopped one, on the standard output of the host.
container_file() { # CONTAINER PATH
  echo "docker cp '$1:$2' - | tar -xO"
}

collect_node() { # HOST CONTAINER IMAGE METRICS_PORT VERSION_COMMAND OUT
  local out=$6
  remote "$1" "cat '${RACE_DIR}/race-info.txt' '${RACE_DIR}/race-start.log' /var/log/race-start.log 2>/dev/null" >"${out}/$2-info.txt" || true
  remote "$1" "docker run --rm --network none '$3' $5 2>&1" >"${out}/$2-version.txt" || true
  remote "$1" "docker inspect '$2'" >"${out}/$2-inspect.json" || true
  remote "$1" "docker stats --no-stream '$2'" >"${out}/$2-stats.txt" || true
  remote "$1" "docker exec race-node-exporter wget -qO- http://127.0.0.1:$4/metrics" >"${out}/$2-metrics.txt" ||
    log "$2 does not answer on its metrics port (the node is stopped)"
}

# The trace tables of a node into OUT/<container>-traces.
collect_traces() { # HOST CONTAINER DIRECTORY OUT
  local to="$4/$2-traces"
  mkdir -p "${to}"
  if remote "$1" "docker cp '$2:$3' -" >"$4/$2-traces.tar"; then
    tar -C "${to}" --strip-components=1 -xf "$4/$2-traces.tar"
    rm "$4/$2-traces.tar"
  else
    rm -f "$4/$2-traces.tar"
    log "warning: no trace files of $2"
  fi
}

# The value of an instant query of Prometheus, or nothing.
prometheus_value() { # QUERY [TIME]
  local query
  query=$(python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "$1")
  prometheus_get "/api/v1/query?query=${query}${2:+&time=$2}" |
    sed -n 's/.*"value":\[[^,]*,"\([0-9]*\).*/\1/p' || true
}

collect() {
  local out now first step tip from start end part
  local -a series=() from_height=()
  command -v python3 >/dev/null || die "collect needs python3 on this machine"
  out="${REPO}/race-results/$(date -u +%Y%m%dT%H%M%SZ)"
  mkdir -p "${out}"
  collect_node "${A}" race-zakurad "${ZAKURA_IMAGE}" "${ZAKURAD_METRICS_PORT}" "zakurad --version" "${out}"
  collect_node "${B}" race-hayaid "${HAYAI_IMAGE}" "${HAYAID_METRICS_PORT}" "--version" "${out}"

  # hayaid writes its log to the output of the container. zakurad writes it to a file
  # of its data volume: the last lines, and each line that the table of blocks reads.
  remote "${B}" "docker logs --tail '${RACE_LOG_LINES}' race-hayaid 2>&1" >"${out}/race-hayaid.log" || true
  remote "${A}" "$(container_file race-zakurad "${ZAKURAD_DATA}/zakurad.log") | tail -n '${RACE_LOG_LINES}'" \
    >"${out}/race-zakurad.log" || log "warning: no log file of zakurad"
  remote "${A}" "$(container_file race-zakurad "${ZAKURAD_DATA}/zakurad.log") |
    grep -E 'downloaded and verified gossiped block|starting sync, obtaining new tips'" \
    >"${out}/race-zakurad-block-lines.log" || log "warning: no block lines in the log of zakurad"

  # The trace tables of both nodes, and the rows of hayaid of a block that it did not
  # accept.
  collect_traces "${B}" race-hayaid /var/lib/hayai/traces "${out}"
  collect_traces "${A}" race-zakurad "${ZAKURAD_DATA}/traces" "${out}"
  grep -h -E 'not_validated|"result":"invalid"|"fault"' "${out}"/race-hayaid-traces/*.jsonl \
    >"${out}/hayaid-fault-rows.jsonl" 2>/dev/null || true

  # Each series of the race rules, from the first start to now, at most 5000 points.
  now=$(date +%s)
  first=$(prometheus_value "min(race:start_timestamp_seconds)")
  [[ -n "${first}" ]] || first=$((now - 86400))
  step=$(((now - first) / 5000 + SCRAPE_SECONDS))
  prometheus_get "/api/v1/query_range?query=%7B__name__%3D~%22race%3A.%2B%22%7D&start=${first}&end=${now}&step=${step}" \
    >"${out}/race-series.json" || log "warning: no series from Prometheus on ${C}"

  # The file of each RPC caller: one line for each getblocktemplate call.
  remote "${A}" "$(container_file race-zakurad-caller "${CALLER_FILE}")" \
    >"${out}/race-zakurad-getblocktemplate.jsonl" || log "no file of the RPC caller of zakurad"
  remote "${B}" "$(container_file race-hayaid-caller "${CALLER_FILE}")" \
    >"${out}/race-hayaid-getblocktemplate.jsonl" || log "no file of the RPC caller of hayaid"
  # The last textfile of each sidecar: the race_* values at the time of collect.
  remote "${A}" "$(container_file race-zakurad-sidecar /textfile/race_zakurad.prom)" \
    >"${out}/race-zakurad-sidecar.prom" || log "no textfile of the sidecar of zakurad"
  remote "${B}" "$(container_file race-hayaid-sidecar /textfile/race_hayaid.prom)" \
    >"${out}/race-hayaid-sidecar.prom" || log "no textfile of the sidecar of hayaid"

  # The tip phase starts when the second node reaches the tip. The table of blocks
  # starts at the block after the lowest height of that moment. Without that moment
  # (a Regtest dry run has no tip of a network) the table starts at the first block
  # that zakurad got by gossip, and the series start 50000 s before now.
  tip=$(prometheus_value "max(race:tip_timestamp_seconds) and on() (count(race:tip_timestamp_seconds) == 2)")
  if [[ -n "${tip}" ]]; then
    from=$(prometheus_value "min(race:block_height)" "${tip}")
    [[ -z "${from}" ]] || from_height=(--from-height "$((from + 1))")
    start=${tip}
  else
    log "the two nodes are not at the tip of a network: the table of blocks starts at the first gossiped block of zakurad"
    start=$((now - 10000 * SCRAPE_SECONDS))
    ((start > first)) || start=${first}
  fi
  # The three series of zakurad that give its contextual commit time for each block,
  # with the scrape interval as step, at most 10000 points for each request.
  part=0
  while ((start < now)); do
    end=$((start + 10000 * SCRAPE_SECONDS))
    ((end < now)) || end=${now}
    prometheus_get "/api/v1/query_range?query=%7B__name__%3D~%22state_contextual_total_duration_seconds_sum%7Cstate_contextual_total_duration_seconds_count%7Czcash_chain_verified_block_height%22%2Cjob%3D%22zakurad%22%7D&start=${start}&end=${end}&step=${SCRAPE_SECONDS}" \
      >"${out}/zakurad-series-${part}.json" && series+=("${out}/zakurad-series-${part}.json")
    start=${end}
    part=$((part + 1))
  done

  python3 "${REPO}/scripts/race_blocks.py" \
    --hayai-traces "${out}/race-hayaid-traces" \
    --zakura-traces "${out}/race-zakurad-traces" \
    --zakura-log "${out}/race-zakurad-block-lines.log" \
    --zakura-series "${series[@]}" "${from_height[@]}" \
    --hayai-caller "${out}/race-hayaid-getblocktemplate.jsonl" \
    --zakura-caller "${out}/race-zakurad-getblocktemplate.jsonl" \
    --out-csv "${out}/blocks.csv" --out-md "${out}/blocks.md" ||
    log "warning: no table of blocks"
  log "results in ${out}"
}

clean() {
  local answer host
  echo "This removes the node data of ${A} and ${B} and the Prometheus data of ${C}."
  read -r -p "Type yes to continue: " answer
  [[ "${answer}" == yes ]] || die "stopped: nothing is removed"
  node_compose "${A}" "--profile zakurad --profile hayaid down --volumes" || true
  node_compose "${B}" "--profile zakurad --profile hayaid down --volumes" || true
  remote "${C}" "cd '${RACE_DIR}' && RACE_ZAKURAD_HOST=x RACE_HAYAID_HOST=x docker compose -f compose.monitor.yml down --volumes" || true
  for host in "${A}" "${B}"; do
    unfirewall "${host}"
  done
  log "the three hosts have no container and no volume of the race"
}

swap() {
  clean
  local first="${A}"
  A="${B}"
  B="${first}"
  log "roles exchanged: zakurad on ${A}, hayaid on ${B}"
  start
}

COMMAND="${1:-}"
if [[ "${COMMAND}" == build ]]; then
  build
  exit 0
fi
[[ $# -eq 4 ]] || usage
A=$2
B=$3
C=$4
case "${COMMAND}" in
  start) start ;;
  status) status ;;
  stop) stop ;;
  collect) collect ;;
  swap) swap ;;
  clean) clean ;;
  *) usage ;;
esac
