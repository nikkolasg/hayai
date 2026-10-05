#!/usr/bin/env python3
"""Joins the JSONL traces of two nodes by block hash and compares receive-to-commit times.

Usage:
    scripts/join_traces.py --hayai DIR --zakura DIR --out report.csv [--names A,B]

Each DIR holds the tables of one node: commit_state.jsonl (commit_start, commit_finish,
and for hayai block_validated) and, when present, block_sync.jsonl. Both nodes write the
envelope of zakura-jsonl-trace: ts (µs since the emitter started), node,
process_trace_id, event. Intervals are computed inside one process (same
process_trace_id), so the two nodes need no common clock.

Per node and block hash:
  - receive: hayai `block_received` (by hash) or Zakura `block_body_received` (by height,
    mapped to the hash of that node's commit_start at that height). Without a receive row,
    the interval starts at commit_start and the receive source column says so.
  - commit: the first commit_finish with result "committed".
  - receive_to_commit_ms = commit - receive.
Block class (empty, transparent, shielded) comes from hayai's block_validated rows.

The report has one row per hash that either node committed. The summary on stdout gives,
per class, the count, median and p95 of each node's receive_to_commit_ms and of the
difference (first node minus second) over the blocks that both nodes committed.
--names relabels the two nodes (default: hayai,zakura), for example for two hayaid nodes.
"""

import argparse
import csv
import json
import os
import statistics
import sys


def read_rows(path):
    if not os.path.exists(path):
        return []
    rows = []
    with open(path) as f:
        for n, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError as e:
                print(f"join_traces: {path}:{n}: {e}", file=sys.stderr)
    return rows


class Node:
    """The per-hash timeline of one node."""

    def __init__(self, directory):
        self.start = {}  # hash -> (pid, ts, height)
        self.finish = {}  # hash -> (pid, ts, elapsed_ms)
        self.rejected = {}  # hash -> reason
        self.receive = {}  # hash -> (pid, ts, source)
        self.classes = {}  # hash -> class
        by_height = {}  # (pid, height) -> hash
        commits = read_rows(os.path.join(directory, "commit_state.jsonl"))
        for r in commits:
            event, h = r.get("event"), r.get("hash")
            pid, ts = r.get("process_trace_id"), r.get("ts")
            if event == "commit_start" and h and h not in self.start:
                self.start[h] = (pid, ts, r.get("height"))
                by_height.setdefault((pid, r.get("height")), h)
            elif event == "commit_finish" and h:
                if r.get("result") == "committed" and h not in self.finish:
                    self.finish[h] = (pid, ts, r.get("elapsed_ms"))
                elif r.get("result") != "committed":
                    self.rejected.setdefault(h, r.get("reason", r.get("result")))
            elif event == "block_validated" and h and r.get("class"):
                self.classes[h] = r["class"]
        for r in read_rows(os.path.join(directory, "block_sync.jsonl")):
            event, pid, ts = r.get("event"), r.get("process_trace_id"), r.get("ts")
            if event == "block_received" and r.get("hash"):
                self.receive.setdefault(r["hash"], (pid, ts, r.get("source", "block_received")))
            elif event == "block_body_received" and r.get("height") is not None:
                h = by_height.get((pid, r["height"]))
                if h:
                    self.receive.setdefault(h, (pid, ts, "block_body_received"))

    def latency_ms(self, h):
        """(receive_to_commit_ms, receive source) or (None, None)."""
        finish = self.finish.get(h)
        if finish is None:
            return None, None
        pid, finish_ts, _ = finish
        receive = self.receive.get(h)
        if receive is not None and receive[0] == pid and receive[1] <= finish_ts:
            return (finish_ts - receive[1]) / 1000.0, receive[2]
        start = self.start.get(h)
        if start is not None and start[0] == pid:
            return (finish_ts - start[1]) / 1000.0, "commit_start"
        return None, None

    def height(self, h):
        start = self.start.get(h)
        return start[2] if start else None


def p95(values):
    ordered = sorted(values)
    rank = max(1, -(-95 * len(ordered) // 100))  # nearest rank, ceil(0.95 n)
    return ordered[rank - 1]


def summarize(label, values):
    if not values:
        return f"{label:>22}: no blocks"
    return (
        f"{label:>22}: n={len(values):5d}  median={statistics.median(values):9.3f} ms"
        f"  p95={p95(values):9.3f} ms"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--hayai", required=True, help="trace directory of the first node")
    parser.add_argument("--zakura", required=True, help="trace directory of the second node")
    parser.add_argument("--out", required=True)
    parser.add_argument("--names", default="hayai,zakura")
    args = parser.parse_args()
    names = args.names.split(",")
    if len(names) != 2:
        parser.error("--names takes two labels")
    a, b = Node(args.hayai), Node(args.zakura)

    hashes = set(a.finish) | set(b.finish)
    rows = []
    for h in hashes:
        cls = a.classes.get(h) or b.classes.get(h) or "unknown"
        la, sa = a.latency_ms(h)
        lb, sb = b.latency_ms(h)
        rows.append(
            {
                "hash": h,
                "height": a.height(h) if a.height(h) is not None else b.height(h),
                "class": cls,
                f"{names[0]}_receive_source": sa or "",
                f"{names[0]}_receive_to_commit_ms": "" if la is None else f"{la:.3f}",
                f"{names[0]}_commit_ms": (a.finish.get(h) or (None, None, ""))[2],
                f"{names[0]}_rejected": a.rejected.get(h, ""),
                f"{names[1]}_receive_source": sb or "",
                f"{names[1]}_receive_to_commit_ms": "" if lb is None else f"{lb:.3f}",
                f"{names[1]}_commit_ms": (b.finish.get(h) or (None, None, ""))[2],
                f"{names[1]}_rejected": b.rejected.get(h, ""),
                "diff_ms": "" if la is None or lb is None else f"{la - lb:.3f}",
            }
        )
    rows.sort(key=lambda r: (r["height"] is None, r["height"] or 0, r["hash"]))
    columns = list(rows[0].keys()) if rows else ["hash", "height", "class"]
    with open(args.out, "w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=columns)
        writer.writeheader()
        writer.writerows(rows)

    print(f"{len(rows)} blocks, {sum(1 for r in rows if r['diff_ms'])} committed by both")
    for cls in ("empty", "transparent", "shielded", "unknown"):
        subset = [r for r in rows if r["class"] == cls]
        if not subset:
            continue
        print(f"[{cls}]")
        for name in names:
            values = [
                float(r[f"{name}_receive_to_commit_ms"])
                for r in subset
                if r[f"{name}_receive_to_commit_ms"]
            ]
            print(summarize(f"{name} receive->commit", values))
        diffs = [float(r["diff_ms"]) for r in subset if r["diff_ms"]]
        print(summarize(f"{names[0]} - {names[1]}", diffs))
    disagreements = [r for r in rows if r[f"{names[0]}_rejected"] or r[f"{names[1]}_rejected"]]
    if disagreements:
        print(f"{len(disagreements)} blocks rejected by one node; see the *_rejected columns")


if __name__ == "__main__":
    main()
