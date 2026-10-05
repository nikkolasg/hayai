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
3. Each metric in an expression of the sync race (docker/race) is a name that hayaid
   registers, a name of ZAKURA_ONLY, a name of NODE_EXPORTER, `up`, or a series that
   docker/race/prometheus/rules/race.yml records.
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
RACE_DASHBOARD = REPO / "docker/race/grafana/dashboards/sync-race.json"

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
    "zcash_mempool_size_transactions",
    "zcash_mempool_size_bytes",
    "mining_template_rebuilt",
    "rpc_requests_total",
    "rpc_request_duration_seconds",
    "rpc_errors_total",
    "rpc_active_requests",
    "process_resident_memory_bytes",
    "process_cpu_seconds_total",
}
# Metrics of zakurad that hayaid does not export and that the sync race reads.
ZAKURA_ONLY = {"sync_block_best_header_tip_height", "sync_estimated_network_tip_height"}
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
KEYWORDS = {"or", "and", "unless", "by", "on", "without", "bool", "group_left", "group_right"}
IDENTIFIER = re.compile(r"[A-Za-z_:][A-Za-z0-9_:]*")


def base(name: str) -> str:
    return re.sub(r"_(bucket|sum|count)$", "", name)


def metrics_of(expr: str) -> set:
    """The metric names of a PromQL expression: each identifier that is not a function,
    a keyword, a label of a selector or of a grouping clause, or a part of a duration."""
    expr = re.sub(r'"[^"]*"', "", expr)
    expr = re.sub(r"\{[^}]*\}", "", expr)
    expr = re.sub(r"\[[^\]]*\]", "", expr)
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
    recorded = set(re.findall(r"^ *- record: *(\S+)$", rules, flags=re.M))
    known = registered | ZAKURA_ONLY | NODE_EXPORTER | recorded | {"up"}
    race = {
        RACE_RULES: rule_expressions(rules),
        RACE_DASHBOARD: dashboard_expressions(json.loads(RACE_DASHBOARD.read_text())),
    }
    for path, expressions in race.items():
        if not expressions:
            errors.append(f"{path.relative_to(REPO)} has no expression")
        names = set().union(*(metrics_of(e) for e in expressions))
        for name in sorted(names):
            if name not in known and base(name) not in known:
                errors.append(f"unknown metric {path.relative_to(REPO)}: {name}")

    for line in errors:
        print(line, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
