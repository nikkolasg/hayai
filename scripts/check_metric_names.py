#!/usr/bin/env python3
"""Checks that the alert rules and the Grafana dashboard use only metric names that hayaid
registers (string literals in crates/hayaid/src/metrics.rs).

Usage: scripts/check_metric_names.py

A metric name is a token that starts with one of the prefixes of hayaid's metrics. The
suffixes _bucket, _sum and _count of a histogram map to the histogram name. Exit status 1
lists every unknown name.
"""

import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
SOURCE = REPO / "crates/hayaid/src/metrics.rs"
USERS = [
    *sorted((REPO / "docker/observability/prometheus/rules").glob("*.yml")),
    *sorted((REPO / "docker/observability/grafana/dashboards").glob("*.json")),
]
PREFIXES = ("hayai_", "zcash_", "state_memory_", "sync_block_", "mining_", "process_")
TOKEN = re.compile(r"\b(?:%s)[a-z0-9_]+" % "|".join(PREFIXES))


def main() -> int:
    registered = set(re.findall(r'"([a-z_:][a-z0-9_:]*)"', SOURCE.read_text()))
    unknown = []
    for path in USERS:
        for name in sorted(set(TOKEN.findall(path.read_text()))):
            base = re.sub(r"_(bucket|sum|count)$", "", name)
            if name not in registered and base not in registered:
                unknown.append(f"{path.relative_to(REPO)}: {name}")
    for line in unknown:
        print(f"unknown metric {line}", file=sys.stderr)
    return 1 if unknown else 0


if __name__ == "__main__":
    sys.exit(main())
