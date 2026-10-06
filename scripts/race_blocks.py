#!/usr/bin/env python3
"""Writes the table of blocks of the race: one row for each block height at the tip.

Usage:
    scripts/race_blocks.py --hayai-traces DIR [--zakura-traces DIR --zakura-log FILE]
        [--zakura-series FILE ...] [--hayai-caller FILE] [--zakura-caller FILE]
        [--from-height N] --out-csv blocks.csv --out-md blocks.md

Inputs (docs/sync-race.md, Compared quantities):

  --hayai-traces   commit_state.jsonl and template.jsonl of hayaid. Each duration is a
                   field of a row: block_validated.since_received_us,
                   block_validated.contextual_commit_us, commit_finish.received_to_commit_us,
                   template_empty.since_received_us, template_full.since_received_us.
  --zakura-traces  legacy_peer_request.jsonl and legacy_sync.jsonl of zakurad.
  --zakura-log     The log of zakurad (the lines `downloaded and verified gossiped block`
                   and `starting sync, obtaining new tips` are sufficient).
  --zakura-series  Answers of the Prometheus query_range API for the three series of
                   zakurad: state_contextual_total_duration_seconds_sum, _count and
                   zcash_chain_verified_block_height, with the scrape interval as step.
  --hayai-caller, --zakura-caller
                   The JSON lines of scripts/race_rpc_caller.py of each node machine.

Zakura, "received to committed": the row block_request_finish (result available) is the
start, and the log line `downloaded and verified gossiped block` of the same hash is the
stop. The row has the clock of its trace emitter (µs from the start of the emitter). The
script finds the offset of that clock to the wall clock in two steps:
  1. legacy_sync: the row round_start number k and the log line `starting sync` number k
     come from one code point. The offset of legacy_sync is the median of the differences.
  2. legacy_peer_request: each block_request_finish row is before its log line (upper
     limit of the offset), and a find_blocks_finish row is between round_start and
     tips_obtained of a round. The offset is the middle of the range below the upper
     limit that puts the most find_blocks_finish rows in a round. Half of the width of
     the range is the error of each Zakura value, and the table states it.
Without the rows or the lines of a step the Zakura column stays empty.

Zakura, "contextual commit": between two samples in which the count increased by 1, the
increase of the sum is the value of one block. The height of that block is the height
gauge at the next sample without a commit minus the blocks after it. An interval with
two or more blocks gives no value.

"Template served" (`hayai_template_served_s`, `zakura_template_served_s`): the time of
the first `getblocktemplate` answer of the caller with the block as `previousblockhash`,
minus the commit time of the block on the same machine. Both times are on the wall clock
of that machine. hayaid: the commit time is `unix_us` of the row commit_finish. zakurad:
the time of the log line `downloaded and verified gossiped block`, so only a gossiped
block has a value, and the node can answer some µs before it writes the line (a value
below 0). Without the long poll of the caller the value has an error between 0 and the
call interval. The summary states which mode gave the first answer.

"Template transactions" (`hayai_template_transactions`, `zakura_template_transactions`):
the transactions of the first answer without `longpollid` (mode `poll` or
`after_longpoll`) at or after the first answer on the block. The caller sends such a call
at once after each long-poll answer.

scripts/race_sidecar.py uses the clock calibration (`calibrate`), the log parser
(`zakura_log_line`) and the reader of the caller file (`CallerAnswers`) of this file for
the live metrics of the same quantities.

A height is in the table when hayaid committed a block at it, from --from-height on
(default: the first height of a gossiped block in the log of zakurad, else each height).
The Zakura value of "received to committed" is in the row only when zakurad has the same
block hash. Exit status 1: no committed block in the traces of hayaid.
"""

import argparse
import bisect
import csv
import json
import os
import re
import statistics
import sys
from datetime import datetime, timezone

# The offset of legacy_peer_request is searched this far below its upper limit.
SEARCH_US = 5_000_000

COLUMNS = [
    "height",
    "hash",
    "hayai_source",
    "hayai_transactions",
    "hayai_bytes",
    "hayai_received_to_validated_s",
    "hayai_received_to_committed_s",
    "zakura_received_to_committed_s",
    "diff_received_to_committed_s",
    "hayai_contextual_commit_s",
    "zakura_contextual_commit_s",
    "diff_contextual_commit_s",
    "hayai_received_to_template_empty_s",
    "hayai_received_to_template_full_s",
    "hayai_template_served_s",
    "zakura_template_served_s",
    "hayai_template_transactions",
    "zakura_template_transactions",
    "note",
]

LOG_TIME = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d{1,9}))?Z")
GOSSIPED = re.compile(
    r"download_and_verify\{hash=([0-9a-f]{64}).*"
    r"downloaded and verified gossiped block height=Height\((\d+)\)"
)
ROUND = "starting sync, obtaining new tips"
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def read_rows(path):
    rows = []
    if not os.path.exists(path):
        return rows
    with open(path) as f:
        for n, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError as e:
                print(f"race_blocks: {path}:{n}: {e}", file=sys.stderr)
    return rows


def log_micros(line):
    """The time of a log line in µs since the Unix epoch, or None."""
    match = LOG_TIME.match(line)
    if not match:
        return None
    seconds = datetime.strptime(match.group(1), "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc)
    fraction = (match.group(2) or "0").ljust(9, "0")[:6]
    return int(seconds.timestamp()) * 1_000_000 + int(fraction)


def seconds(micros):
    return None if micros is None else micros / 1e6


def hayai_blocks(directory):
    """height -> the measurements of the last block that hayaid committed at it."""
    validated, blocks, origins = {}, {}, {}
    for r in read_rows(os.path.join(directory, "commit_state.jsonl")):
        event, block_hash = r.get("event"), r.get("hash")
        if event == "commit_start":
            origins[block_hash] = r.get("origin")
        elif event == "block_validated" and r.get("result") == "valid":
            validated[block_hash] = r
        elif event == "commit_finish" and r.get("result") == "committed":
            v = validated.get(block_hash, {})
            blocks[r["height"]] = {
                "hash": block_hash,
                "source": origins.get(block_hash),
                "transactions": v.get("txs"),
                "bytes": v.get("bytes"),
                "received_to_validated": seconds(v.get("since_received_us")),
                "contextual_commit": seconds(v.get("contextual_commit_us")),
                "received_to_committed": seconds(r.get("received_to_commit_us")),
                "committed_unix_us": r.get("unix_us"),
            }
    by_hash = {b["hash"]: b for b in blocks.values()}
    for r in read_rows(os.path.join(directory, "template.jsonl")):
        key = {"template_empty": "template_empty", "template_full": "template_full"}.get(r.get("event"))
        block = by_hash.get(r.get("parent"))
        if key and block is not None and r.get("since_received_us") is not None:
            block.setdefault(key, seconds(r["since_received_us"]))
    return blocks


def best_range(intervals, low, high):
    """The range inside [low, high] that the most intervals contain: (count, start, end).
    With two such ranges, the one nearest to `high`."""
    points = []
    for start, end in intervals:
        start, end = max(start, low), min(end, high)
        if start <= end:
            points.append((start, 0))
            points.append((end, 1))
    points.sort()
    best, depth = (0, low, high), 0
    for i, (at, closes) in enumerate(points):
        if closes:
            depth -= 1
            continue
        depth += 1
        end = points[i + 1][0]
        if depth >= best[0]:
            best = (depth, at, end)
    return best


def zakura_log_line(line):
    """("round", time) for a line `starting sync`, ("commit", time, hash, height) for a line
    `downloaded and verified gossiped block`, else None. Each time is in µs since the Unix
    epoch."""
    line = ANSI.sub("", line)
    at = log_micros(line)
    if at is None:
        return None
    if ROUND in line:
        return ("round", at)
    match = GOSSIPED.search(line)
    if match:
        return ("commit", at, match.group(1), int(match.group(2)))
    return None


def zakura_log(log_path, since=0):
    """(times of the sync rounds, hash -> (height, commit time)) from the log lines of
    zakurad at or after `since`. Each time is in µs since the Unix epoch."""
    rounds, committed = [], {}
    with open(log_path, errors="replace") as f:
        for line in f:
            parsed = zakura_log_line(line)
            if parsed is None or parsed[1] < since:
                continue
            if parsed[0] == "round":
                rounds.append(parsed[1])
            else:
                committed.setdefault(parsed[2], (parsed[3], parsed[1]))
    return rounds, committed


def peer_offset(windows, finds, upper):
    """best_range of the offsets of legacy_peer_request below `upper` that put a
    find_blocks_finish row (`finds`) in a round. `windows`: the rounds on the wall clock,
    sorted, one after the other."""
    starts = [start for start, _ in windows]
    intervals = []
    for f in finds:
        # Only a round that ends at or after f + upper - SEARCH_US and starts at or before
        # f + upper can contain the row.
        i = bisect.bisect_right(starts, f + upper)
        while i > 0 and windows[i - 1][1] >= f + upper - SEARCH_US:
            i -= 1
            intervals.append((windows[i][0] - f, windows[i][1] - f))
    return best_range(intervals, upper - SEARCH_US, upper)


def calibrate(pairs, windows, finds, gaps):
    """(offset, calibration): the offset in µs of the clock of legacy_peer_request to the
    wall clock, or None when a step has no data. The calibration is a dict for the summary;
    its key `reason` names the missing data, and `error_s` is the error of each value.

    pairs: (ts of round_start, time of its log line `starting sync`) of each round.
    windows: (ts of round_start, ts of the next tips_obtained) on the clock of legacy_sync.
    finds: ts of the find_blocks_finish rows.
    gaps: log time minus ts of block_request_finish, for each block with both."""
    calibration = {"error_s": None, "reason": None}
    if not pairs:
        calibration["reason"] = "no round_start row with a log line"
        return None, calibration
    offsets = [at - ts for ts, at in pairs]
    sync_offset = statistics.median(offsets)
    calibration["sync_rounds"] = len(pairs)
    calibration["sync_spread_s"] = (max(offsets) - min(offsets)) / 1e6
    if not gaps:
        calibration["reason"] = "no block has a block_request_finish row and a log line"
        return None, calibration
    rounds = sorted((start + sync_offset, end + sync_offset) for start, end in windows)
    count, low, high = peer_offset(rounds, finds, min(gaps))
    if count == 0:
        calibration["reason"] = "no find_blocks_finish row is in a round"
        return None, calibration
    calibration.update(
        error_s=(high - low) / 2e6,
        find_rows=len(finds),
        find_rows_in_a_round=count,
        blocks=len(gaps),
    )
    return (low + high) / 2, calibration


def zakura_received_to_committed(trace_dir, log_path):
    """(hash -> (height, seconds), calibration). The calibration is a dict for the
    summary; its key `error_s` is None when the clocks have no calibration."""
    peer_rows = read_rows(os.path.join(trace_dir, "legacy_peer_request.jsonl"))
    sync_rows = read_rows(os.path.join(trace_dir, "legacy_sync.jsonl"))
    if not peer_rows or not sync_rows:
        return {}, {"error_s": None, "reason": "no legacy_peer_request or legacy_sync rows"}
    # The rows and the log lines of the last start of the process only.
    process = peer_rows[-1]["process_trace_id"]
    peer_rows = [r for r in peer_rows if r["process_trace_id"] == process]
    sync_rows = [r for r in sync_rows if r["process_trace_id"] == process]
    # The first row of the process and its log line can be some µs apart.
    rounds, committed = zakura_log(log_path, int(process.split("-")[1]) // 1000 - 1_000_000)
    starts = [r["ts"] for r in sync_rows if r["event"] == "round_start"]
    requests = {}
    for r in peer_rows:
        if r["event"] == "block_request_finish" and r.get("result") == "available":
            requests.setdefault(r.get("returned_hash"), r["ts"])
    both = [h for h in requests if h in committed]
    # Each round: round_start to the next tips_obtained.
    windows, opened = [], None
    for r in sync_rows:
        if r["event"] == "round_start":
            opened = r["ts"]
        elif r["event"] == "tips_obtained" and opened is not None:
            windows.append((opened, r["ts"]))
            opened = None
    finds = [r["ts"] for r in peer_rows if r["event"] == "find_blocks_finish"]
    offset, calibration = calibrate(
        list(zip(starts, rounds)), windows, finds, [committed[h][1] - requests[h] for h in both]
    )
    if offset is None:
        return {}, calibration
    values = {h: (committed[h][0], (committed[h][1] - requests[h] - offset) / 1e6) for h in both}
    return values, calibration


def series_of(paths):
    """metric name -> sorted [(time, value)] from query_range answers."""
    series = {}
    for path in paths:
        with open(path) as f:
            answer = json.load(f)
        for result in answer.get("data", {}).get("result", []):
            name = result["metric"].get("__name__")
            points = series.setdefault(name, {})
            for at, value in result["values"]:
                points[float(at)] = float(value)
    return {name: sorted(points.items()) for name, points in series.items()}


def zakura_contextual_commit(paths):
    """(height -> seconds, counts) from the Prometheus series of zakurad."""
    series = series_of(paths)
    sums = dict(series.get("state_contextual_total_duration_seconds_sum", []))
    counts = series.get("state_contextual_total_duration_seconds_count", [])
    heights = dict(series.get("zcash_chain_verified_block_height", []))
    values, stats = {}, {"blocks": 0, "shared_intervals": 0, "no_height": 0}
    # height - count at each sample after which the count did not change: the gauge
    # then has the height of the block of that count.
    settled = {}
    for (at, count), (after, next_count) in zip(counts, counts[1:]):
        if next_count == count and after in heights:
            settled[at] = heights[after] - count
    times = sorted(settled)
    for (before, old), (at, new) in zip(counts, counts[1:]):
        if new <= old:
            continue
        if new - old > 1:
            stats["shared_intervals"] += 1
            continue
        later = next((t for t in times if t >= at), None)
        earlier = next((t for t in reversed(times) if t <= before), None)
        # A reorg changes height - count: the block then has no certain height.
        if later is None or earlier is None or settled[later] != settled[earlier]:
            stats["no_height"] += 1
            continue
        if before in sums and at in sums:
            values[int(new + settled[later])] = sums[at] - sums[before]
            stats["blocks"] += 1
    return values, stats


# Modes of a call without `longpollid` (scripts/race_rpc_caller.py).
PLAIN = ("poll", "after_longpoll")


class CallerAnswers:
    """The answers of the RPC caller of one node, line by line (scripts/race_rpc_caller.py).
    This class is also the reader of scripts/race_sidecar.py.

    first: previousblockhash -> (time, mode) of the first answer on the block.
    transactions: previousblockhash -> (time, transactions) of the first answer without
    `longpollid` at or after the first answer on the block.
    stats: `polls` (calls without `longpollid` with a template), `long_polls`, `errors`.
    plain_us: the sum of the times of the calls in `polls`, on the client side.
    keep: when set, `first` and `transactions` keep the newest `keep` blocks only."""

    def __init__(self, keep=None):
        self.keep = keep
        self.first, self.transactions = {}, {}
        self.stats = {"polls": 0, "long_polls": 0, "errors": 0}
        self.plain_us = 0

    def add(self, r):
        if not r.get("ok"):
            self.stats["errors"] += 1
            return
        plain = r.get("mode") in PLAIN
        if plain:
            self.stats["polls"] += 1
            self.plain_us += r["duration_us"]
        else:
            self.stats["long_polls"] += 1
        block, at = r["previousblockhash"], r["unix_us"]
        first = self.first.get(block)
        if first is None or at < first[0]:
            first = self.first[block] = (at, r.get("mode"))
            if self.keep is not None and len(self.first) > self.keep:
                oldest = next(iter(self.first))
                del self.first[oldest]
                self.transactions.pop(oldest, None)
        known = self.transactions.get(block)
        if plain and at >= first[0] and (known is None or at < known[0]):
            self.transactions[block] = (at, r["transactions"])

    def mean_plain_s(self):
        return self.plain_us / self.stats["polls"] / 1e6 if self.stats["polls"] else None


def caller_answers(path):
    """The CallerAnswers of a file of scripts/race_rpc_caller.py."""
    answers = CallerAnswers()
    for r in read_rows(path):
        answers.add(r)
    return answers


def template_served(answers, block_hash, committed_unix_us):
    """Seconds from the commit of a block to the first answer of the caller on it."""
    answer = answers.first.get(block_hash)
    if answer is None or committed_unix_us is None:
        return None
    key = "first_by_long_poll" if answer[1] == "longpoll" else "first_by_poll"
    answers.stats[key] = answers.stats.get(key, 0) + 1
    return (answer[0] - committed_unix_us) / 1e6


def template_transactions(answers, block_hash):
    known = answers.transactions.get(block_hash)
    return None if known is None else known[1]


def difference(a, b):
    return None if a is None or b is None else a - b


def build_rows(hayai, zakura_commit, zakura_contextual, from_height, callers=None, zakura_committed=None):
    """`callers`: node -> CallerAnswers. `zakura_committed`: hash -> (height, commit
    time) of zakura_log."""
    rows = []
    callers = callers or {}
    hayai_answers = callers.get("hayai", CallerAnswers())
    zakura_answers = callers.get("zakura", CallerAnswers())
    zakura_committed = zakura_committed or {}
    by_height = {height: h for h, (height, _) in zakura_commit.items()}
    for height in sorted(hayai):
        if from_height is not None and height < from_height:
            continue
        b = hayai[height]
        zakura = zakura_commit.get(b["hash"], (None, None))[1]
        note = ""
        if zakura is None and height in by_height:
            note = "zakurad has another block at this height"
        contextual = zakura_contextual.get(height)
        rows.append(
            {
                "height": height,
                "hash": b["hash"],
                "hayai_source": b["source"],
                "hayai_transactions": b["transactions"],
                "hayai_bytes": b["bytes"],
                "hayai_received_to_validated_s": b["received_to_validated"],
                "hayai_received_to_committed_s": b["received_to_committed"],
                "zakura_received_to_committed_s": zakura,
                "diff_received_to_committed_s": difference(b["received_to_committed"], zakura),
                "hayai_contextual_commit_s": b["contextual_commit"],
                "zakura_contextual_commit_s": contextual,
                "diff_contextual_commit_s": difference(b["contextual_commit"], contextual),
                "hayai_received_to_template_empty_s": b.get("template_empty"),
                "hayai_received_to_template_full_s": b.get("template_full"),
                "hayai_template_served_s": template_served(hayai_answers, b["hash"], b["committed_unix_us"]),
                "zakura_template_served_s": template_served(
                    zakura_answers, b["hash"], zakura_committed.get(b["hash"], (None, None))[1]
                ),
                "hayai_template_transactions": template_transactions(hayai_answers, b["hash"]),
                "zakura_template_transactions": template_transactions(zakura_answers, b["hash"]),
                "note": note,
            }
        )
    return rows


def cell(value):
    if value is None:
        return ""
    if isinstance(value, float):
        return f"{value:.6f}"
    return str(value)


def quantiles(values):
    values = sorted(values)
    if not values:
        return "0 | | | "
    p90 = values[min(len(values) - 1, int(0.9 * len(values)))]
    return f"{len(values)} | {statistics.median(values):.6f} | {p90:.6f} | {values[-1]:.6f}"


def summary(rows, calibration, contextual_stats, from_height, callers=None):
    lines = ["# Blocks of the race at the tip", ""]
    if rows:
        lines.append(f"Heights {rows[0]['height']} to {rows[-1]['height']}: {len(rows)} blocks of hayaid.")
    else:
        lines.append("No block.")
    if from_height is not None:
        lines.append(f"First height of the table: {from_height}.")
    lines += [
        "",
        "Each value is in seconds, but a `*_template_transactions` value is a number of"
        " transactions. A difference is hayaid minus zakurad.",
        "",
    ]
    lines += ["| Quantity | Blocks | Median | 90 % | Largest |", "|---|---|---|---|---|"]
    for column in COLUMNS[5:-1]:
        values = [r[column] for r in rows if r[column] is not None]
        lines.append(f"| `{column}` | {quantiles(values)} |")
    lines += ["", "## Zakura clock of \"received to committed\"", ""]
    if calibration is None:
        lines.append("No trace and no log of zakurad: the column is empty.")
    elif calibration["error_s"] is None:
        lines.append(f"No calibration ({calibration['reason']}): the column is empty.")
    else:
        lines += [
            f"- Error of each Zakura value from the clock calibration: {calibration['error_s']:.6f} s"
            " (plus or minus).",
            f"- legacy_sync: {calibration['sync_rounds']} pairs of a row and a log line, spread"
            f" {calibration['sync_spread_s']:.6f} s.",
            f"- legacy_peer_request: {calibration['find_rows_in_a_round']} of"
            f" {calibration['find_rows']} find_blocks_finish rows are in a round.",
            f"- Blocks with a row and a log line: {calibration['blocks']}.",
        ]
    lines += ["", "## Zakura \"contextual commit\" from Prometheus", ""]
    if contextual_stats is None:
        lines.append("No series of zakurad: the column is empty.")
    else:
        lines += [
            f"- Blocks with a value: {contextual_stats['blocks']}.",
            f"- Scrape intervals with 2 or more blocks (no value): {contextual_stats['shared_intervals']}.",
            f"- Blocks without a certain height (no value): {contextual_stats['no_height']}.",
        ]
    lines += ["", "## getblocktemplate caller", ""]
    if not callers:
        lines.append("No file of the caller: the columns `*_template_served_s` are empty.")
    else:
        lines += [
            "Calls without `longpollid`: the calls of each interval and the call after each long-poll"
            " answer. Mean time of such a call, on the client side, for the whole file."
            " \"First answer\": the mode of the first answer on a block of the table. An answer"
            " by a call without `longpollid` is late by 0 to the call interval.",
            "",
            "| Node | Calls without `longpollid` | Mean time, s | Long poll answers | Errors"
            " | First answer by long poll | First answer by call without `longpollid` |",
            "|---|---|---|---|---|---|---|",
        ]
        for node, name in (("hayai", "hayaid"), ("zakura", "zakurad")):
            if node not in callers:
                lines.append(f"| {name} | no file | | | | | |")
                continue
            stats = callers[node].stats
            lines.append(
                f"| {name} | {stats['polls']} | {cell(callers[node].mean_plain_s())} | {stats['long_polls']}"
                f" | {stats['errors']} | {stats.get('first_by_long_poll', 0)}"
                f" | {stats.get('first_by_poll', 0)} |"
            )
    lines += ["", "## First rows", "", "| " + " | ".join(COLUMNS) + " |", "|" + "---|" * len(COLUMNS)]
    for row in rows[:20]:
        lines.append("| " + " | ".join(cell(row[c]) for c in COLUMNS) + " |")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--hayai-traces", required=True)
    parser.add_argument("--zakura-traces")
    parser.add_argument("--zakura-log")
    parser.add_argument("--zakura-series", nargs="*", default=[])
    parser.add_argument("--hayai-caller")
    parser.add_argument("--zakura-caller")
    parser.add_argument("--from-height", type=int)
    parser.add_argument("--out-csv", required=True)
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    hayai = hayai_blocks(args.hayai_traces)
    if not hayai:
        print("race_blocks: no committed block in the traces of hayaid", file=sys.stderr)
        return 1
    zakura_commit, calibration = {}, None
    if args.zakura_traces and args.zakura_log and os.path.exists(args.zakura_log):
        zakura_commit, calibration = zakura_received_to_committed(args.zakura_traces, args.zakura_log)
    zakura_contextual, contextual_stats = {}, None
    series = [p for p in args.zakura_series if os.path.exists(p)]
    if series:
        zakura_contextual, contextual_stats = zakura_contextual_commit(series)
    from_height = args.from_height
    if from_height is None and zakura_commit:
        from_height = min(height for height, _ in zakura_commit.values())
    callers = {}
    for node, path in (("hayai", args.hayai_caller), ("zakura", args.zakura_caller)):
        if path and os.path.exists(path) and os.path.getsize(path) > 0:
            callers[node] = caller_answers(path)
    zakura_committed = {}
    if "zakura" in callers and args.zakura_log and os.path.exists(args.zakura_log):
        zakura_committed = zakura_log(args.zakura_log)[1]
    rows = build_rows(hayai, zakura_commit, zakura_contextual, from_height, callers, zakura_committed)
    with open(args.out_csv, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(COLUMNS)
        for row in rows:
            writer.writerow([cell(row[c]) for c in COLUMNS])
    with open(args.out_md, "w") as f:
        f.write(summary(rows, calibration, contextual_stats, from_height, callers))
    print(f"race_blocks: {len(rows)} rows in {args.out_csv}, summary in {args.out_md}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
