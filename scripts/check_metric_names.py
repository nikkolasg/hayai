#!/usr/bin/env python3
"""Checks the metric names of the alert rules, the recording rules and the dashboards.

Usage: scripts/check_metric_names.py

Three checks. Exit status 1 lists every unknown name.

1. Each name of ZAKURA_NAMES (the metrics that hayaid exports under the name of Zakura) is
   a string literal in the sources of hayaid.
2. The rules and the dashboard of docker/observability use only names that hayaid
   registers: a `hayai_` name of the sources, or a name of ZAKURA_NAMES. A metric name is
   a token that starts with one of PREFIXES. The suffixes _bucket, _sum and _count of a
   histogram map to the histogram name.
3. Each metric in an expression of the race (docker/race) is a known name:
   - the recording rules and the dashboard comparison.json: a name that hayaid
     registers, a name of zakurad, a name of NODE_EXPORTER, a name of the sidecar, `up`,
     or a series that the rules record (race.yml, and the rule that compose.monitor.yml
     writes);
   - the dashboard hayai-node.json: a name that hayaid registers, or a name of
     NODE_EXPORTER or of the sidecar;
   - the dashboard zakura-node.json: a name of zakurad, or a name of NODE_EXPORTER or of
     the sidecar.
   The names of the sidecar are the keys of METRICS in scripts/race_sidecar.py, with
   _sum and _count for a summary.
   The names of zakurad are the families of scripts/zakura_metric_names.txt: the
   `/metrics` text of a running zakurad. zakurad exports summaries, so a `_bucket`
   series of a name of zakurad is not a known name.
"""

import json
import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
SOURCES = [
    REPO / "crates/hayaid/src/metrics.rs",
    REPO / "crates/hayai-rpc/src/rpc.rs",
]
USERS = [
    *sorted((REPO / "docker/observability/prometheus/rules").glob("*.yml")),
    *sorted((REPO / "docker/observability/grafana/dashboards").glob("*.json")),
]
RACE_RULES = REPO / "docker/race/prometheus/rules/race.yml"
RACE_DASHBOARDS = REPO / "docker/race/grafana/dashboards"
ZAKURA_RUNNING = REPO / "scripts/zakura_metric_names.txt"
SIDECAR = REPO / "scripts/race_sidecar.py"

# The metrics that hayaid exports with the name, the labels and the unit of Zakura
# (docs/zakura-compat.md, Metrics).
ZAKURA_NAMES = {
    "zcash_chain_verified_block_height",
    "zcash_chain_verified_block_total",
    "state_memory_best_committed_block_height",
    "state_finalized_block_height",
    "sync_block_verify_duration_seconds",
    "sync_downloads_in_flight",
    "zcash_net_peers",
    "zcash_net_in_bytes_total",
    "zcash_net_out_bytes_total",
    "sync_downloaded_block_count",
    "zcash_mempool_size_transactions",
    "zcash_mempool_size_bytes",
    "rpc_requests_total",
    "rpc_request_duration_seconds",
    "rpc_errors_total",
    "rpc_active_requests",
    "process_resident_memory_bytes",
    "process_cpu_seconds_total",
}
# Metrics of node-exporter that the sync race reads.
NODE_EXPORTER = {
    "node_cpu_seconds_total",
    "node_memory_MemTotal_bytes",
    "node_memory_MemAvailable_bytes",
    "node_filesystem_size_bytes",
    "node_filesystem_avail_bytes",
    "node_disk_read_bytes_total",
    "node_disk_written_bytes_total",
    "node_network_receive_bytes_total",
    "node_network_transmit_bytes_total",
}

PREFIXES = ("hayai_", "zcash_", "state_", "sync_", "mining_", "process_", "rpc_")
TOKEN = re.compile(r"\b(?:%s)[a-z0-9_]+" % "|".join(PREFIXES))
# PromQL words that are not metric names.
KEYWORDS = {
    "or",
    "and",
    "unless",
    "by",
    "on",
    "without",
    "bool",
    "group_left",
    "group_right",
    "offset",
}
IDENTIFIER = re.compile(r"[A-Za-z_:][A-Za-z0-9_:]*")


def base(name: str) -> str:
    return re.sub(r"_(bucket|sum|count)$", "", name)


def metrics_of(expr: str) -> set:
    """The metric names of a PromQL expression: each identifier that is not a function,
    a keyword, a label of a selector or of a grouping clause, or a part of a duration."""
    expr = re.sub(r'"[^"]*"', "", expr)
    expr = re.sub(r"\{[^}]*\}", "", expr)
    expr = re.sub(r"\[[^\]]*\]", "", expr)
    expr = re.sub(r"\boffset\s+[0-9]+[a-z]+", "", expr)
    expr = re.sub(r"\b(by|on|without|group_left|group_right)\s*\([^)]*\)", "", expr)
    names = set()
    for match in IDENTIFIER.finditer(expr):
        name = match.group()
        before = expr[: match.start()]
        is_call = expr[match.end() :].lstrip().startswith("(")
        in_number = bool(re.search(r"[0-9.]$", before))
        if not is_call and not in_number and name not in KEYWORDS:
            names.add(name)
    return names


def rule_expressions(text: str) -> list:
    """The `expr` values of a rule file: one line, or a folded block."""
    text = "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))
    blocks = re.findall(r"^( *)expr: *(>-\n(?:\1 +.*\n?)+|.*)", text, flags=re.M)
    return [block.removeprefix(">-") for _, block in blocks]


def dashboard_expressions(node) -> list:
    if isinstance(node, dict):
        own = [node["expr"]] if isinstance(node.get("expr"), str) else []
        return own + [e for value in node.values() for e in dashboard_expressions(value)]
    if isinstance(node, list):
        return [e for value in node for e in dashboard_expressions(value)]
    return []


def main() -> int:
    literals = set()
    for source in SOURCES:
        literals |= set(re.findall(r'"([a-z_:][a-z0-9_:]*)"', source.read_text()))
    errors = [
        f"{name} is in ZAKURA_NAMES and is not a name in the sources of hayaid"
        for name in sorted(ZAKURA_NAMES - literals)
    ]
    registered = {name for name in literals if name.startswith("hayai_")} | ZAKURA_NAMES

    for path in USERS:
        for name in sorted(set(TOKEN.findall(path.read_text()))):
            if name not in registered and base(name) not in registered:
                errors.append(f"unknown metric {path.relative_to(REPO)}: {name}")

    rules = RACE_RULES.read_text()
    # The Prometheus container writes one more rule at its start (compose.monitor.yml).
    monitor = (REPO / "docker/race/compose.monitor.yml").read_text()
    recorded = set(re.findall(r"^ *- record: *(\S+)$", rules + monitor, flags=re.M))
    zakura = {
        line.strip()
        for line in ZAKURA_RUNNING.read_text().splitlines()
        if line.strip() and not line.startswith("#")
    }

    def of_hayai(name):
        return name in registered or base(name) in registered

    def of_zakura(name):
        return name in zakura or re.sub(r"_(sum|count)$", "", name) in zakura

    sidecar = set()
    for name, kind in re.findall(r'^    "(race_[a-z_]+)": \(\n\s+"(\w+)"', SIDECAR.read_text(), flags=re.M):
        sidecar |= {name + "_sum", name + "_count"} if kind == "summary" else {name}
    if not sidecar:
        errors.append(f"no metric name in METRICS of {SIDECAR.relative_to(REPO)}")

    def of_machine(name):
        return name in NODE_EXPORTER or name in sidecar

    def of_race(name):
        return name in recorded or name == "up" or of_hayai(name) or of_zakura(name) or of_machine(name)

    def dashboard(name):
        return dashboard_expressions(json.loads((RACE_DASHBOARDS / name).read_text()))

    race = [
        (RACE_RULES, rule_expressions(rules), of_race),
        (RACE_DASHBOARDS / "comparison.json", dashboard("comparison.json"), of_race),
        (
            RACE_DASHBOARDS / "hayai-node.json",
            dashboard("hayai-node.json"),
            lambda name: of_hayai(name) or of_machine(name),
        ),
        (
            RACE_DASHBOARDS / "zakura-node.json",
            dashboard("zakura-node.json"),
            lambda name: of_zakura(name) or of_machine(name),
        ),
    ]
    for path, expressions, known in race:
        if not expressions:
            errors.append(f"{path.relative_to(REPO)} has no expression")
        names = set().union(*(metrics_of(e) for e in expressions))
        for name in sorted(names):
            if not known(name):
                errors.append(f"unknown metric {path.relative_to(REPO)}: {name}")
    others = sorted(p.name for p in RACE_DASHBOARDS.glob("*.json") if p not in [r[0] for r in race])
    for name in others:
        errors.append(f"docker/race/grafana/dashboards/{name} is not a dashboard that this script checks")

    for line in errors:
        print(line, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
