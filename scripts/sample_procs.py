#!/usr/bin/env python3
"""Samples the resources of running processes from /proc into one CSV.

Usage:
    scripts/sample_procs.py PID [PID ...] --out procs.csv [--interval 1]
        [--metrics URL [--metrics URL ...]] [--metrics-out metrics.csv] [--duration S]

Every --interval seconds, one row per PID: wall clock, PID, process name, RSS (VmRSS),
peak RSS (VmHWM), user and system CPU time (/proc/PID/stat), threads, and the bytes the
process read and wrote through the storage layer and through system calls
(/proc/PID/io). A field that /proc does not expose to this user is empty.

With --metrics, every 5 s, the script scrapes each Prometheus endpoint and appends every
sample to --metrics-out (default: the --out name with a "-metrics" suffix): wall clock,
URL, metric name with labels, value.

The script stops when every PID has exited, after --duration seconds, or on Ctrl-C.
"""

import argparse
import csv
import os
import signal
import sys
import time
import urllib.request

CLOCK_TICKS = os.sysconf("SC_CLK_TCK")
PAGE_FIELDS = ("VmRSS", "VmHWM", "Threads")
IO_FIELDS = ("read_bytes", "write_bytes", "rchar", "wchar")
METRICS_PERIOD_S = 5.0

COLUMNS = [
    "unix_s",
    "pid",
    "name",
    "rss_kb",
    "hwm_kb",
    "utime_s",
    "stime_s",
    "threads",
    "read_bytes",
    "write_bytes",
    "rchar",
    "wchar",
]


def read_status(pid):
    """VmRSS, VmHWM (kB) and Threads from /proc/PID/status, plus the process name."""
    out = {}
    with open(f"/proc/{pid}/status") as f:
        for line in f:
            key, _, value = line.partition(":")
            if key == "Name":
                out["name"] = value.strip()
            elif key in PAGE_FIELDS:
                out[key] = int(value.split()[0])
    return out


def read_stat(pid):
    """utime and stime in seconds. The name field can hold spaces, so split after ')'."""
    with open(f"/proc/{pid}/stat") as f:
        data = f.read()
    fields = data[data.rindex(")") + 2 :].split()
    # fields[0] is field 3 (state); utime and stime are fields 14 and 15.
    return int(fields[11]) / CLOCK_TICKS, int(fields[12]) / CLOCK_TICKS


def read_io(pid):
    """Byte counters of /proc/PID/io; empty when this user may not read them."""
    try:
        with open(f"/proc/{pid}/io") as f:
            pairs = (line.split(":") for line in f)
            return {k.strip(): int(v) for k, v in pairs if k.strip() in IO_FIELDS}
    except PermissionError:
        return {}


def sample(pid):
    status = read_status(pid)
    utime, stime = read_stat(pid)
    io = read_io(pid)
    return {
        "unix_s": f"{time.time():.3f}",
        "pid": pid,
        "name": status.get("name", ""),
        "rss_kb": status.get("VmRSS", ""),
        "hwm_kb": status.get("VmHWM", ""),
        "utime_s": f"{utime:.2f}",
        "stime_s": f"{stime:.2f}",
        "threads": status.get("Threads", ""),
        **{k: io.get(k, "") for k in IO_FIELDS},
    }


def scrape(url):
    """(name_with_labels, value) for every sample line of a Prometheus text page."""
    with urllib.request.urlopen(url, timeout=5) as response:
        text = response.read().decode()
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        name, _, value = line.rpartition(" ")
        yield name, value


def metrics_path(out):
    root, ext = os.path.splitext(out)
    return f"{root}-metrics{ext or '.csv'}"


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("pids", nargs="+", type=int)
    parser.add_argument("--out", required=True)
    parser.add_argument("--interval", type=float, default=1.0)
    parser.add_argument("--metrics", action="append", default=[], metavar="URL")
    parser.add_argument("--metrics-out")
    parser.add_argument("--duration", type=float)
    args = parser.parse_args()

    stop = False

    def on_signal(_sig, _frame):
        nonlocal stop
        stop = True

    signal.signal(signal.SIGINT, on_signal)
    signal.signal(signal.SIGTERM, on_signal)

    started = time.monotonic()
    alive = list(args.pids)
    metrics_file = None
    metrics_writer = None
    if args.metrics:
        metrics_file = open(args.metrics_out or metrics_path(args.out), "w", newline="")
        metrics_writer = csv.writer(metrics_file)
        metrics_writer.writerow(["unix_s", "url", "metric", "value"])
    next_scrape = started
    with open(args.out, "w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=COLUMNS)
        writer.writeheader()
        next_sample = started
        while alive and not stop:
            if args.duration is not None and time.monotonic() - started >= args.duration:
                break
            for pid in list(alive):
                try:
                    writer.writerow(sample(pid))
                except (FileNotFoundError, ProcessLookupError):
                    print(f"sample_procs: process {pid} exited", file=sys.stderr)
                    alive.remove(pid)
            f.flush()
            if metrics_writer is not None and time.monotonic() >= next_scrape:
                now = f"{time.time():.3f}"
                for url in args.metrics:
                    try:
                        for name, value in scrape(url):
                            metrics_writer.writerow([now, url, name, value])
                    except OSError as e:
                        print(f"sample_procs: scrape {url}: {e}", file=sys.stderr)
                metrics_file.flush()
                next_scrape += METRICS_PERIOD_S
            next_sample += args.interval
            time.sleep(max(0.0, next_sample - time.monotonic()))
    if metrics_file is not None:
        metrics_file.close()


if __name__ == "__main__":
    main()
