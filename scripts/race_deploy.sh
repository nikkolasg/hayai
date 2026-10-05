#!/usr/bin/env bash
# Deploys the sync race of one zakurad and one hayaid (docs/sync-race.md) on three hosts
# that have Docker with the compose plugin and SSH access. The files are those of
# docker/race.
#
# Usage: scripts/race_deploy.sh COMMAND [A B C]
#
#   A  SSH target of the machine of zakurad, for example user@a.example
#   B  SSH target of the machine of hayaid
#   C  SSH target of the monitoring machine (Prometheus and Grafana)
#
# Commands:
#   build    Build the two images on this machine. It needs no host.
#   start    Copy the files and the images, start the monitoring on C, then start both
#            nodes at the same minute. It stops when a node host has data of a race.
#   status   Containers, resources, height, peers and last log line of each node.
#   stop     Stop both nodes. The data and the monitoring stay.
#   collect  Fetch the versions, the node logs, the metrics, the trace files of hayaid
#            and the series of the race into race-results/<UTC time>/.
#   swap     `clean`, then `start` with the roles of A and B exchanged.
#   clean    Remove the containers, the volumes (node data, Prometheus data) and the
#            firewall rules of the race on the three hosts. It asks first.
#
# Shell variables:
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
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The Zakura commit of the race. docs/sync-race.md names it too.
ZAKURA_COMMIT="13779158253cfe315f73eadffb9b4c93c25e82a5"
ZAKURA_IMAGE="zakurad:race"
HAYAI_IMAGE="hayaid:race"

RACE_DIR="${RACE_DIR:-hayai-race}"
RACE_FIREWALL="${RACE_FIREWALL:-1}"
RACE_CPUS="${RACE_CPUS:-0}"
RACE_MEMORY="${RACE_MEMORY:-0}"
RACE_LOG_LINES="${RACE_LOG_LINES:-20000}"
ZAKURAD_METRICS_PORT=9999
HAYAID_METRICS_PORT=19101
EXPORTER_PORT=9100
PROMETHEUS_ADDR="127.0.0.1:9090"

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
  log "building ${HAYAI_IMAGE} from hayai ${revision} (zakura crypto backend)"
  docker build -t "${HAYAI_IMAGE}" -f "${REPO}/docker/Dockerfile" \
    --build-arg CRYPTO_BACKEND=zakura \
    --label "org.opencontainers.image.revision=${revision}" "${REPO}"
}

check_hosts() {
  [[ "$(host_of "${A}")" != "$(host_of "${B}")" ]] ||
    die "the two nodes must not share a machine"
  local host
  for host in "${A}" "${B}" "${C}"; do
    remote "${host}" "docker compose version >/dev/null" ||
      die "${host}: no SSH access, or no Docker with the compose plugin"
  done
}

# Copies docker/race to a host. The secrets and the .env file of a host stay.
copy_files() { # HOST
  remote "$1" "mkdir -p '${RACE_DIR}'"
  tar -C "${REPO}/docker/race" --exclude=./secrets --exclude=./.env -cf - . |
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
  rule="INPUT -p tcp -m multiport --dports $2 ! -s ${ip} -m comment --comment hayai-race -j DROP"
  remote "$1" "sudo -n iptables -C ${rule} 2>/dev/null || sudo -n iptables -I ${rule}" ||
    die "$1: cannot set the iptables rule (sudo without a password is necessary, or RACE_FIREWALL=0)"
  rule="INPUT -p tcp -m multiport --dports $2 -m comment --comment hayai-race -j DROP"
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

# Schedules the start of the node of a host at an epoch second, in the background of the
# host, and records the start in race-info.txt.
schedule() { # HOST PROFILE IMAGE EPOCH
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
    docker compose -f compose.node.yml --profile $2 up -d $2
  ' >race-start.log 2>&1 </dev/null &"
}

start() {
  check_hosts
  local zakurad_host hayaid_host host volume
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

  for host in "${A}" "${B}" "${C}"; do
    copy_files "${host}"
  done
  push_image "${A}" "${ZAKURA_IMAGE}"
  push_image "${B}" "${HAYAI_IMAGE}"
  for host in "${A}" "${B}"; do
    remote "${host}" "cd '${RACE_DIR}' && printf '%s\n' \
      'RACE_ZAKURA_IMAGE=${ZAKURA_IMAGE}' 'RACE_HAYAI_IMAGE=${HAYAI_IMAGE}' \
      'RACE_CPUS=${RACE_CPUS}' 'RACE_MEMORY=${RACE_MEMORY}' >.env"
  done

  log "starting Prometheus and Grafana on ${C}"
  remote "${C}" "cd '${RACE_DIR}' && printf '%s\n' \
    'RACE_ZAKURAD_HOST=${zakurad_host}' 'RACE_HAYAID_HOST=${hayaid_host}' >.env &&
    mkdir -p secrets &&
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
  log "both nodes start at $(date -u -d "@${epoch}" +%Y-%m-%dT%H:%M:%SZ) (epoch ${epoch})"
  log "Grafana: http://$(host_of "${C}"):3000 (user admin, password in ${RACE_DIR}/secrets/grafana_admin_password on ${C})"
  log "run the status command after that time"
}

node_status() { # HOST PROFILE CONTAINER METRICS_PORT
  log "$2 on $1"
  remote "$1" "cd '${RACE_DIR}' && cat race-info.txt 2>/dev/null
    docker compose -f compose.node.yml --profile $2 ps --all
    docker stats --no-stream --format 'cpu={{.CPUPerc}} memory={{.MemUsage}}' $3 2>/dev/null
    docker exec race-node-exporter wget -qO- http://127.0.0.1:$4/metrics 2>/dev/null |
      grep -E '^(zcash_chain_verified_block_height|state_finalized_block_height|zcash_net_peers) '
    docker logs --tail 1 $3 2>&1" || log "warning: no status of $1"
}

status() {
  node_status "${A}" zakurad race-zakurad "${ZAKURAD_METRICS_PORT}"
  node_status "${B}" hayaid race-hayaid "${HAYAID_METRICS_PORT}"
  log "monitoring on ${C}"
  monitor_compose "${C}" "ps --all" || log "warning: no status of ${C}"
}

stop() {
  node_compose "${A}" "--profile zakurad stop zakurad"
  node_compose "${B}" "--profile hayaid stop hayaid"
  log "both nodes are stopped; the data and the monitoring stay"
}

collect_node() { # HOST CONTAINER IMAGE METRICS_PORT VERSION_COMMAND OUT
  local out=$6
  remote "$1" "cat '${RACE_DIR}/race-info.txt' '${RACE_DIR}/race-start.log' 2>/dev/null" >"${out}/$2-info.txt" || true
  remote "$1" "docker run --rm --network none '$3' $5 2>&1" >"${out}/$2-version.txt" || true
  remote "$1" "docker inspect '$2'" >"${out}/$2-inspect.json" || true
  remote "$1" "docker stats --no-stream '$2'" >"${out}/$2-stats.txt" || true
  remote "$1" "docker logs --tail '${RACE_LOG_LINES}' '$2' 2>&1" >"${out}/$2.log" || true
  remote "$1" "docker exec race-node-exporter wget -qO- http://127.0.0.1:$4/metrics" >"${out}/$2-metrics.txt" ||
    log "$2 does not answer on its metrics port (the node is stopped)"
}

collect() {
  local out now first step
  out="${REPO}/race-results/$(date -u +%Y%m%dT%H%M%SZ)"
  mkdir -p "${out}"
  collect_node "${A}" race-zakurad "${ZAKURA_IMAGE}" "${ZAKURAD_METRICS_PORT}" "zakurad --version" "${out}"
  collect_node "${B}" race-hayaid "${HAYAI_IMAGE}" "${HAYAID_METRICS_PORT}" "--version" "${out}"

  # The trace tables of hayaid, and their rows of a block that the node did not accept.
  if remote "${B}" "docker cp race-hayaid:/var/lib/hayai/traces -" >"${out}/hayaid-traces.tar"; then
    tar -C "${out}" -xf "${out}/hayaid-traces.tar"
    grep -h -E 'not_validated|"result":"invalid"|"fault"' "${out}"/traces/*.jsonl \
      >"${out}/hayaid-fault-rows.jsonl" || true
  else
    log "warning: no trace files of hayaid"
  fi

  # Each series of the race rules, from the first start to now, at most 5000 points.
  now=$(date +%s)
  first=$(prometheus_get "/api/v1/query?query=min(race:start_timestamp_seconds)" |
    sed -n 's/.*"value":\[[^,]*,"\([0-9]*\).*/\1/p') || true
  [[ -n "${first}" ]] || first=$((now - 86400))
  step=$(((now - first) / 5000 + 15))
  prometheus_get "/api/v1/query_range?query=%7B__name__%3D~%22race%3A.%2B%22%7D&start=${first}&end=${now}&step=${step}" \
    >"${out}/race-series.json" || log "warning: no series from Prometheus on ${C}"
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
