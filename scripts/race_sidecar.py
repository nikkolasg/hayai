#!/usr/bin/env python3
"""Writes the race metrics of one node machine to a Prometheus textfile each second
(docs/sync-race.md, Sidecar). The node-exporter of the machine reads the file with its
textfile collector. The same program runs beside zakurad and beside hayaid. It uses the
Python 3 standard library only (3.11 or later) and scripts/race_blocks.py.

Usage:
    scripts/race_sidecar.py --node zakurad|hayaid --out FILE.prom --data DIR --traces DIR
        [--log FILE] [--caller FILE] [--cgroup DIR] [--interval SECONDS]

  --node      zakurad or hayaid: the value of the label `node`, and the files to read.
  --out       The textfile. The sidecar writes FILE.prom.tmp, then renames it.
  --data      The data directory of the node (read only): race_node_data_bytes.
  --traces    The trace directory of the node. zakurad: legacy_sync.jsonl and
              legacy_peer_request.jsonl. hayaid: commit_state.jsonl.
  --log       zakurad only: its log file.
  --caller    The file of scripts/race_rpc_caller.py of this machine.
  --cgroup    The cgroup v2 directory of the node container (the slice of its
              `cgroup_parent`, docker/race/compose.node.yml).
  --interval  Seconds between two writes (default 1).

Each metric has the label `node`. METRICS has the definition of each metric on each node
and its verdict; the textfile has it as the HELP text. A metric without a value is not
in the file: no block yet, no answer of the caller on the last block, no cgroup.

Bounds of the memory: KEEP_BLOCKS blocks for the commits, the requests and the answers
of the caller; CALIBRATION_ROWS rounds and blocks for the clock calibration of zakurad
(5 times that number of find_blocks_finish rows); READ_LIMIT bytes for each read of a
file.
"""

import argparse
import collections
import json
import os
import signal
import sys
import time

import race_blocks

DATA_SCAN_S = 60
READ_LIMIT = 16 << 20
KEEP_BLOCKS = 10_000
CALIBRATION_ROWS = 1_000

ZAKURA_START = "zakurad: trace row legacy_peer_request block_request_finish (the peer service returns the decoded block), on the wall clock through the clock calibration of scripts/race_blocks.py"
HAYAI_START = "hayaid: arrival of the block message, before the parse (trace field commit_finish.received_to_commit_us)"
ZAKURA_COMMIT = "zakurad: time of the log line 'downloaded and verified gossiped block', after the commit response"
HAYAI_COMMIT = "hayaid: unix_us of the trace row commit_finish"
CGROUP = "of the cgroup v2 of the node container (the slice race-<node>.slice), the same file on both machines"

# name -> (type, help). One text for both nodes: a node-exporter that reads the files of
# both nodes (the dry run) needs one HELP text for each name.
METRICS = {
    "race_last_block_height": (
        "gauge",
        "Height of the last block that the node committed. The other race_last_block metrics are of this block. "
        "zakurad: the last log line 'downloaded and verified gossiped block' (a block from gossip only, not a block of the legacy syncer). "
        "hayaid: the last trace row commit_finish with the result committed.",
    ),
    "race_last_block_received_to_committed_seconds": (
        "gauge",
        f"Block received to block committed, for the last block. Start: {ZAKURA_START}; {HAYAI_START}. "
        "Stop: zakurad, the log line 'downloaded and verified gossiped block' (after the commit to the state in memory); "
        "hayaid, the block is the tip (after the write to the block file and the update of the prepared store). "
        "Verdict CLOSE: hayaid has the parse and the block file write inside the interval, zakurad does not; "
        "the zakurad value has the error race_last_block_clock_error_seconds.",
    ),
    "race_last_block_clock_error_seconds": (
        "gauge",
        "zakurad only: plus or minus error of race_last_block_received_to_committed_seconds from the clock calibration "
        f"(half of the range of the offset of the clock of legacy_peer_request), from the last {CALIBRATION_ROWS} rounds and blocks.",
    ),
    "race_last_block_template_served_seconds": (
        "gauge",
        "Template served after the last block: time of the first getblocktemplate answer of the RPC caller of this machine "
        "with the block as previousblockhash (the long poll, or a call without longpollid when it comes first), "
        f"minus the commit time of the block on the same machine ({ZAKURA_COMMIT}; {HAYAI_COMMIT}). "
        "Verdict CLOSE: same client and same stop on both machines; the start of zakurad is later, and its value can be below 0.",
    ),
    "race_last_block_template_transactions": (
        "gauge",
        "Transactions of the first getblocktemplate answer without longpollid at or after the first answer on the last block "
        "(the RPC caller sends such a call at once after each long-poll answer). "
        "Verdict CLOSE: zakurad builds the template in the call, hayaid returns the template that it built at the tip change; "
        "the mempools differ, because each node has other peers.",
    ),
    "race_rpc_getblocktemplate_seconds": (
        "summary",
        "Time on the client side of each getblocktemplate call without longpollid that returned a template "
        "(the call of each interval and the call after each long-poll answer): request sent to answer read, "
        "from the file of the RPC caller. Mean: rate(_sum) / rate(_count). "
        "Verdict CLOSE: zakurad builds the template in the call, hayaid returns the template that it built at the tip change.",
    ),
    "race_node_cpu_seconds_total": (
        "counter",
        f"CPU time of the node container: usage_usec of cpu.stat {CGROUP}. Verdict SAME.",
    ),
    "race_node_memory_bytes": (
        "gauge",
        f"Memory of the node container: memory.current {CGROUP}. It includes the page cache of the files that the node reads and writes. Verdict SAME.",
    ),
    "race_node_memory_peak_bytes": (
        "gauge",
        f"Largest memory of the node container: memory.peak {CGROUP} (Linux 5.19 or later). It includes the page cache. Verdict SAME.",
    ),
    "race_node_io_read_bytes_total": (
        "counter",
        f"Bytes that the node container read from block devices: rbytes of io.stat {CGROUP}, devices without a lower device (no device-mapper or md device). Verdict SAME.",
    ),
    "race_node_io_write_bytes_total": (
        "counter",
        f"Bytes that the node container wrote to block devices: wbytes of io.stat {CGROUP}, devices without a lower device. Verdict SAME.",
    ),
    "race_node_data_bytes": (
        "gauge",
        f"Disk blocks of the data directory of the node, each file one time (du), each {DATA_SCAN_S} s. "
        "Verdict CLOSE: both directories have the trace tables; the zakurad directory also has its log file, and hayaid writes its log to Docker.",
    ),
}


class Tail:
    """The new complete lines of a file that grows. A new file (other inode) or a shorter
    file is read again from its start."""

    def __init__(self, path):
        self.path, self.inode, self.offset, self.rest = path, None, 0, b""

    def lines(self):
        try:
            st = os.stat(self.path)
        except FileNotFoundError:
            return []
        if st.st_ino != self.inode or st.st_size < self.offset:
            if self.inode is not None:
                print(f"race_sidecar: {self.path} is a new file: read from its start", file=sys.stderr, flush=True)
            self.inode, self.offset, self.rest = st.st_ino, 0, b""
        if st.st_size == self.offset:
            return []
        with open(self.path, "rb") as f:
            f.seek(self.offset)
            data = f.read(READ_LIMIT)
        self.offset += len(data)
        lines = (self.rest + data).split(b"\n")
        self.rest = lines.pop()
        return [line.decode(errors="replace") for line in lines]


def json_rows(tail):
    rows = []
    for line in tail.lines():
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError as e:
            print(f"race_sidecar: {tail.path}: {e}", file=sys.stderr, flush=True)
    return rows


def bounded(mapping, key, value):
    mapping[key] = value
    mapping.move_to_end(key)
    while len(mapping) > KEEP_BLOCKS:
        mapping.popitem(last=False)


class Node:
    """The commits of a node and the answers of its RPC caller. A subclass reads the
    commits and sets `received_to_committed` (hash, seconds, error) of a block."""

    def __init__(self, caller):
        self.commits = collections.OrderedDict()  # hash -> (height, commit time in µs)
        self.last = None
        self.received_to_committed = None
        self.caller = Tail(caller) if caller else None
        self.answers = race_blocks.CallerAnswers(keep=KEEP_BLOCKS)

    def commit(self, block_hash, height, at):
        bounded(self.commits, block_hash, (height, at))
        self.last = block_hash

    def read(self):
        if self.caller:
            for r in json_rows(self.caller):
                self.answers.add(r)

    def values(self):
        out = {}
        if self.caller:
            out["race_rpc_getblocktemplate_seconds_sum"] = self.answers.plain_us / 1e6
            out["race_rpc_getblocktemplate_seconds_count"] = self.answers.stats["polls"]
        if self.last is None:
            return out
        height, at = self.commits[self.last]
        out["race_last_block_height"] = height
        known = self.received_to_committed
        if known is not None and known[0] == self.last:
            out["race_last_block_received_to_committed_seconds"] = known[1]
            if known[2] is not None:
                out["race_last_block_clock_error_seconds"] = known[2]
        first = self.answers.first.get(self.last)
        if first is not None:
            out["race_last_block_template_served_seconds"] = (first[0] - at) / 1e6
        transactions = race_blocks.template_transactions(self.answers, self.last)
        if transactions is not None:
            out["race_last_block_template_transactions"] = transactions
        return out


class Hayai(Node):
    def __init__(self, traces, caller):
        super().__init__(caller)
        self.commit_state = Tail(os.path.join(traces, "commit_state.jsonl"))

    def read(self):
        for r in json_rows(self.commit_state):
            if r.get("event") == "commit_finish" and r.get("result") == "committed":
                self.commit(r["hash"], r["height"], r["unix_us"])
                micros = r.get("received_to_commit_us")
                if micros is not None:
                    self.received_to_committed = (r["hash"], micros / 1e6, None)
        super().read()


class Zakura(Node):
    """The live form of race_blocks.zakura_received_to_committed: the same calibration
    (race_blocks.calibrate) on the recent rows and lines of the last start of the node."""

    def __init__(self, traces, log, caller):
        super().__init__(caller)
        self.sync = Tail(os.path.join(traces, "legacy_sync.jsonl"))
        self.peer = Tail(os.path.join(traces, "legacy_peer_request.jsonl"))
        self.log = Tail(log)
        # Log times of the lines `starting sync` without their round_start row yet.
        self.round_lines = collections.deque(maxlen=CALIBRATION_ROWS)
        self.reset(None, 0)

    def reset(self, process, start_us):
        self.process, self.start_us = process, start_us
        self.round_starts = collections.deque(maxlen=CALIBRATION_ROWS)  # ts without a log line yet
        self.pairs = collections.deque(maxlen=CALIBRATION_ROWS)
        self.windows = collections.deque(maxlen=CALIBRATION_ROWS)
        self.opened = None
        self.finds = collections.deque(maxlen=5 * CALIBRATION_ROWS)
        self.requests = collections.OrderedDict()  # hash -> ts of block_request_finish
        self.gaps = collections.deque(maxlen=CALIBRATION_ROWS)
        self.offset, self.error = None, None

    def of_this_process(self, r):
        """False for a row of an earlier start of the node. A row of a later start resets
        the state: the trace clocks start again."""
        process = r.get("process_trace_id")
        if process == self.process:
            return True
        start_us = int(process.split("-")[1]) // 1000
        if start_us < self.start_us:
            return False
        print(f"race_sidecar: zakurad process {process}: new clock calibration", file=sys.stderr, flush=True)
        self.reset(process, start_us)
        return True

    def pair_rounds(self):
        # The k-th round_start row and the k-th line `starting sync` of one start of the
        # node come from one code point. A line before that start is of an earlier start.
        while self.round_lines and self.round_lines[0] < self.start_us - 1_000_000:
            self.round_lines.popleft()
        while self.round_starts and self.round_lines:
            self.pairs.append((self.round_starts.popleft(), self.round_lines.popleft()))

    def match(self, block_hash):
        """A block with a request row and a commit line: one more bound of the offset."""
        request = self.requests.get(block_hash)
        commit = self.commits.get(block_hash)
        if request is None or commit is None or commit[1] < self.start_us - 1_000_000:
            return
        self.gaps.append(commit[1] - request)
        self.pair_rounds()
        self.offset, calibration = race_blocks.calibrate(self.pairs, self.windows, self.finds, self.gaps)
        self.error = calibration["error_s"]
        if self.offset is None:
            print(f"race_sidecar: no clock calibration of zakurad: {calibration['reason']}", file=sys.stderr, flush=True)
            return
        if block_hash == self.last:
            seconds = (commit[1] - request - self.offset) / 1e6
            self.received_to_committed = (block_hash, seconds, self.error)

    def read(self):
        for r in json_rows(self.sync):
            if not self.of_this_process(r):
                continue
            if r["event"] == "round_start":
                self.round_starts.append(r["ts"])
                self.opened = r["ts"]
            elif r["event"] == "tips_obtained" and self.opened is not None:
                self.windows.append((self.opened, r["ts"]))
                self.opened = None
        for r in json_rows(self.peer):
            if not self.of_this_process(r):
                continue
            if r["event"] == "find_blocks_finish":
                self.finds.append(r["ts"])
            elif r["event"] == "block_request_finish" and r.get("result") == "available":
                block_hash = r.get("returned_hash")
                if block_hash not in self.requests:
                    bounded(self.requests, block_hash, r["ts"])
                    self.match(block_hash)
        for line in self.log.lines():
            parsed = race_blocks.zakura_log_line(line)
            if parsed is None:
                continue
            if parsed[0] == "round":
                self.round_lines.append(parsed[1])
            elif parsed[2] not in self.commits:
                _, at, block_hash, height = parsed
                self.commit(block_hash, height, at)
                self.match(block_hash)
        self.pair_rounds()
        super().read()


def read_file(path):
    """The content of a file of a cgroup, or None: a file that the kernel does not have
    (memory.peak before Linux 5.19, io.stat without the io controller), or a cgroup that
    the node left."""
    try:
        with open(path) as f:
            return f.read()
    except FileNotFoundError:
        return None


def stacked(device):
    """True for a block device on top of another one (device-mapper, md): its bytes are
    also in io.stat of the lower device."""
    try:
        return bool(os.listdir(f"/sys/dev/block/{device}/slaves"))
    except FileNotFoundError:
        return False


def cgroup_values(directory):
    """The resource metrics of the cgroup, or None without the cgroup directory."""
    if not os.path.isdir(directory):
        return None
    out = {}
    text = read_file(os.path.join(directory, "cpu.stat"))
    for line in (text or "").splitlines():
        key, value = line.split()
        if key == "usage_usec":
            out["race_node_cpu_seconds_total"] = int(value) / 1e6
    for name, metric in (("memory.current", "race_node_memory_bytes"), ("memory.peak", "race_node_memory_peak_bytes")):
        text = read_file(os.path.join(directory, name))
        if text is not None:
            out[metric] = int(text)
    text = read_file(os.path.join(directory, "io.stat"))
    if text is not None:
        read = written = 0
        for line in text.splitlines():
            device, *fields = line.split()
            if stacked(device):
                continue
            values = dict(field.split("=") for field in fields)
            read += int(values.get("rbytes", 0))
            written += int(values.get("wbytes", 0))
        out["race_node_io_read_bytes_total"] = read
        out["race_node_io_write_bytes_total"] = written
    return out


def disk_bytes(directory):
    """Bytes of the disk blocks below a directory, each inode one time (as du)."""
    seen, total = set(), 0

    def walk_error(error):
        # The node removes files and directories during the walk (RocksDB compaction).
        if not isinstance(error, FileNotFoundError):
            raise error

    for root, dirs, files in os.walk(directory, onerror=walk_error):
        for name in [root] + [os.path.join(root, n) for n in dirs + files]:
            try:
                st = os.lstat(name)
            except FileNotFoundError:
                continue
            if (st.st_dev, st.st_ino) not in seen:
                seen.add((st.st_dev, st.st_ino))
                total += st.st_blocks * 512
    return total


def textfile(node, values):
    lines = []
    for name, (kind, text) in METRICS.items():
        family = [(n, v) for n, v in values.items() if n in (name, name + "_sum", name + "_count")]
        if not family:
            continue
        lines.append("# HELP " + name + " " + text.replace("\\", "\\\\").replace("\n", " "))
        lines.append(f"# TYPE {name} {kind}")
        lines += [f'{n}{{node="{node}"}} {v}' for n, v in family]
    return "\n".join(lines) + "\n"


def write(path, text):
    with open(path + ".tmp", "w") as f:
        f.write(text)
    os.replace(path + ".tmp", path)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--node", required=True, choices=("zakurad", "hayaid"))
    parser.add_argument("--out", required=True)
    parser.add_argument("--data", required=True)
    parser.add_argument("--traces", required=True)
    parser.add_argument("--log")
    parser.add_argument("--caller")
    parser.add_argument("--cgroup")
    parser.add_argument("--interval", type=float, default=1)
    args = parser.parse_args()
    if args.node == "zakurad" and not args.log:
        parser.error("--log is necessary for zakurad")
    if not os.path.isdir(args.data):
        parser.error(f"--data {args.data} is not a directory")

    # The stop of a container sends SIGTERM.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    node = Zakura(args.traces, args.log, args.caller) if args.node == "zakurad" else Hayai(args.traces, args.caller)
    print(f"race_sidecar: {args.node}, textfile {args.out} each {args.interval} s", file=sys.stderr, flush=True)
    next_scan, data_bytes, had_cgroup = 0, None, None
    try:
        while True:
            started = time.monotonic()
            node.read()
            values = node.values()
            if args.cgroup:
                resources = cgroup_values(args.cgroup)
                if (resources is not None) != had_cgroup:
                    state = "found" if resources is not None else "not found: no race_node_cpu, memory and io metric"
                    print(f"race_sidecar: cgroup {args.cgroup} {state}", file=sys.stderr, flush=True)
                    had_cgroup = resources is not None
                values.update(resources or {})
            if started >= next_scan:
                data_bytes, next_scan = disk_bytes(args.data), started + DATA_SCAN_S
            values["race_node_data_bytes"] = data_bytes
            write(args.out, textfile(args.node, values))
            time.sleep(max(0.0, args.interval - (time.monotonic() - started)))
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
