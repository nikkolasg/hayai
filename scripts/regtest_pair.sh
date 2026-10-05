#!/usr/bin/env bash
# Runs a Regtest pair on loopback, mines blocks on one side, checks that the other side
# follows, and joins the two nodes' traces.
#
# Usage:
#   scripts/regtest_pair.sh [--zakurad PATH] [--blocks N] [--port-base P]
#
# Without --zakurad: two hayaid nodes; node a mines N blocks through `generate` and node b
# follows over the compact-relay extension. With --zakurad: scenario a of the pair of one
# hayaid and one zakurad (docs/regtest-pair.md), which has its own harness.
#
# Everything goes to target/regtest-pair/<time>/ in the repository: configurations, logs,
# traces, the /proc samples (scripts/sample_procs.py) and report.csv
# (scripts/join_traces.py). The script stops only the processes it started.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ZAKURAD=""
BLOCKS=10
PORT_BASE=28000
while [[ $# -gt 0 ]]; do
  case "$1" in
    --zakurad) ZAKURAD="$2"; shift 2 ;;
    --blocks) BLOCKS="$2"; shift 2 ;;
    --port-base) PORT_BASE="$2"; shift 2 ;;
    -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done

WORK="${REPO}/target/regtest-pair/$(date +%Y%m%d-%H%M%S)"
mkdir -p "${WORK}"
MINER="tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"
PIDS=()

log() { echo "[regtest_pair] $*"; }

cleanup() {
  for pid in "${PIDS[@]}"; do
    kill -INT "${pid}" 2>/dev/null || true
  done
  for pid in "${PIDS[@]}"; do
    wait "${pid}" 2>/dev/null || true
  done
}
trap cleanup EXIT

declare -A RPC_ADDR # name of a node -> address of its RPC server

# hayaid writes the credentials of its RPC server to the file .cookie in its data directory.
rpc() { # rpc NAME METHOD PARAMS_JSON -> prints the result as JSON
  local body cookie
  body=$(printf '{"jsonrpc":"2.0","id":1,"method":"%s","params":%s}' "$2" "$3")
  # Without the file the node did not start its RPC server. curl asks the terminal for a
  # password when the credentials are empty.
  cookie=$(<"${WORK}/$1-data/.cookie") || return 1
  curl -s --max-time 120 -u "${cookie}" \
    -H 'content-type: application/json' --data-binary "${body}" "http://${RPC_ADDR[$1]}/" |
    python3 -c 'import json,sys; r=json.load(sys.stdin); e=r.get("error"); sys.exit(f"rpc error: {e}") if e else print(json.dumps(r["result"]))'
}

hayaid_config() { # NAME P2P RPC METRICS PEERS PRODUCE COMPACT
  cat >"${WORK}/$1.toml" <<EOF
[network]
network = "regtest"
listen_addr = "127.0.0.1:$2"
peers = [$5]
compact_relay = $7

[state]
data_dir = "${WORK}/$1-data"

[rpc]
listen_addr = "127.0.0.1:$3"

[metrics]
listen_addr = "127.0.0.1:$4"

[trace]
dir = "${WORK}/$1-trace"
node = "$1"

[mining]
miner_address = "${MINER}"
regtest_produce = $6
EOF
}

start_hayaid() { # NAME
  "${REPO}/target/release/hayaid" start -c "${WORK}/$1.toml" >"${WORK}/$1.log" 2>&1 &
  PIDS+=("$!")
  log "hayaid $1 started (pid $!)"
}

wait_for() { # SECONDS COMMAND...
  local deadline=$((SECONDS + $1)); shift
  until "$@" >/dev/null 2>&1; do
    if ((SECONDS >= deadline)); then return 1; fi
    sleep 0.2
  done
}

log "building hayaid"
(cd "${REPO}" && cargo build --release -p hayaid)

if [[ -n "${ZAKURAD}" ]]; then
  cd "${REPO}"
  exec cargo test --release -p hayaid --test zakura_pair -- \
    --zakurad "${ZAKURAD}" --scenario a --port-base "${PORT_BASE}"
fi

A_P2P=$((PORT_BASE)); A_RPC=$((PORT_BASE + 1)); A_MET=$((PORT_BASE + 2))
B_P2P=$((PORT_BASE + 10)); B_RPC=$((PORT_BASE + 11)); B_MET=$((PORT_BASE + 12))
hayaid_config a "${A_P2P}" "${A_RPC}" "${A_MET}" "" true true
hayaid_config b "${B_P2P}" "${B_RPC}" "${B_MET}" "\"127.0.0.1:${A_P2P}\"" false true
RPC_ADDR=([a]="127.0.0.1:${A_RPC}" [b]="127.0.0.1:${B_RPC}")
start_hayaid a
wait_for 30 rpc a getblockcount '[]'
start_hayaid b
wait_for 30 rpc b getblockcount '[]'
# shellcheck disable=SC2329 # called through wait_for
peered() { curl -s "http://127.0.0.1:${A_MET}/metrics" | grep -q '^hayai_peers 1$'; }
wait_for 30 peered || { log "the two nodes did not connect"; exit 1; }
TRACE_1="${WORK}/a-trace"; TRACE_2="${WORK}/b-trace"; NAMES="a,b"
METRICS=(--metrics "http://127.0.0.1:${A_MET}/metrics" --metrics "http://127.0.0.1:${B_MET}/metrics")

python3 "${REPO}/scripts/sample_procs.py" "${PIDS[@]}" --out "${WORK}/procs.csv" --interval 1 \
  "${METRICS[@]}" &
SAMPLER=$!

log "mining ${BLOCKS} blocks on ${RPC_ADDR[a]}"
rpc a generate "[${BLOCKS}]" >"${WORK}/generated.json"
TARGET=$(rpc a getbestblockhash '[]')
# shellcheck disable=SC2329 # called through wait_for
followed() { [[ "$(rpc b getbestblockhash '[]')" == "${TARGET}" ]]; }
if wait_for 120 followed; then
  log "the follower reached ${TARGET}"
  STATUS=0
else
  log "the follower is at $(rpc b getbestblockhash '[]'), not ${TARGET}"
  log "see the node logs in ${WORK}"
  STATUS=1
fi

kill -INT "${SAMPLER}" 2>/dev/null || true
wait "${SAMPLER}" 2>/dev/null || true
cleanup
PIDS=()
python3 "${REPO}/scripts/join_traces.py" --hayai "${TRACE_1}" --zakura "${TRACE_2}" \
  --names "${NAMES}" --out "${WORK}/report.csv"
log "results in ${WORK}"
exit "${STATUS}"
