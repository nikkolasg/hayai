#!/usr/bin/env bash
# System-resource benchmarks of every scenario, hayai against the Zakura baseline, in a fresh
# child process per (scenario, impl): wall and CPU time, max RSS, page faults, context
# switches, heap traffic, hardware counters and disk traffic. Writes
# bench-results/system.json and prints the summary table. Extra arguments go to the binary
# (for example `--iterations 50`, or `--scenario parse_block` in place of `--all`).
set -euo pipefail
cd "$(dirname "$0")/.."

args=("$@")
case " ${args[*]} " in
    *" --scenario "*) ;;
    *) args=(--all "${args[@]}") ;;
esac

exec cargo run --release -p hayai-bench --bin sysbench -- "${args[@]}"
