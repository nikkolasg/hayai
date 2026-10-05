#!/usr/bin/env python3
"""Collect criterion results into bench-results/summary.json.

Criterion writes one estimates.json per (group, function id, parameter) under
target/criterion/. Bench groups are named `<component>/<operation>`, function ids are
"hayai", "hayai-<variant>", "zakura", "zakura-<variant>" or "upstream", and the parameter is
the fixture or size. The summary keeps the mean and the 95 % confidence bounds in
nanoseconds, plus the machine description, so the report can be regenerated without the
target directory.
"""

import json
import os
import platform
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Criterion resolves a relative CARGO_TARGET_DIR from the bench crate's directory, so a run with
# CARGO_TARGET_DIR=target/review writes under crates/hayai-bench/target/review.
CRITERION_DIRS = [
    ROOT / "target" / "criterion",
    ROOT / "target" / "review" / "criterion",
    ROOT / "crates" / "hayai-bench" / "target" / "review" / "criterion",
]
OUT = ROOT / "bench-results" / "summary.json"


def cpu_model():
    try:
        for line in open("/proc/cpuinfo"):
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return platform.processor() or "unknown"


def git_rev(path):
    try:
        return subprocess.check_output(["git", "-C", str(path), "rev-parse", "--short", "HEAD"],
                                       text=True, stderr=subprocess.DEVNULL).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return "unknown"


def load_estimate(path):
    with open(path) as f:
        est = json.load(f)
    mean = est["mean"]
    return {
        "mean_ns": mean["point_estimate"],
        "lo_ns": mean["confidence_interval"]["lower_bound"],
        "hi_ns": mean["confidence_interval"]["upper_bound"],
    }


def load_benchmark_id(path):
    with open(path) as f:
        return json.load(f)


def collect():
    found = [d for d in CRITERION_DIRS if d.exists()]
    if not found:
        sys.exit(f"no criterion output under {CRITERION_DIRS}; run `cargo bench -p hayai-bench` first")
    newest = {}
    for bench_dir in (d for root in found for d in root.rglob("new")):
        est = bench_dir / "estimates.json"
        bid = bench_dir / "benchmark.json"
        if not est.exists() or not bid.exists():
            continue
        meta = load_benchmark_id(bid)
        group = meta.get("group_id", "")
        func = meta.get("function_id") or ""
        value = meta.get("value_str") or ""
        key = (group, func, value)
        # The same id can exist in several trees. The newest measurement wins.
        mtime = est.stat().st_mtime
        if key in newest and newest[key][0] >= mtime:
            continue
        row = {"group": group, "function": func, "param": value}
        row.update(load_estimate(est))
        throughput = meta.get("throughput")
        if throughput:
            row["throughput"] = throughput
        newest[key] = (mtime, row)
    rows = [row for _, row in newest.values()]
    rows.sort(key=lambda r: (r["group"], r["param"], r["function"]))
    return rows


def main():
    rows = collect()
    summary = {
        "machine": {
            "cpu": cpu_model(),
            "threads": os.cpu_count(),
            "os": platform.platform(),
        },
        "hayai_rev": git_rev(ROOT),
        "rows": rows,
    }
    OUT.parent.mkdir(exist_ok=True)
    with open(OUT, "w") as f:
        json.dump(summary, f, indent=1)
    print(f"{len(rows)} measurements -> {OUT}")


if __name__ == "__main__":
    main()
