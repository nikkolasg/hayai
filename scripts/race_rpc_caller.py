#!/usr/bin/env python3
"""Calls `getblocktemplate` of one node of the race at a fixed interval and records each
answer (docs/sync-race.md, RPC caller). The same program runs beside zakurad and beside
hayaid. It uses the Python 3 standard library only (3.11 or later).

Usage:
    scripts/race_rpc_caller.py --config NODE.toml --cookie FILE --out FILE.jsonl
        [--interval SECONDS] [--longpoll 0|1] [--timeout SECONDS] [--url URL]

  --config    The configuration of the node. The caller reads `[rpc] listen_addr` from it.
  --url       The RPC address, in place of --config.
  --cookie    The cookie file of the node (`__cookie__:<secret>`). The caller reads it for
              each call: the node writes a new one at each start.
  --out       The caller adds one JSON line for each call to this file.
  --interval  Seconds between two calls without `longpollid` (default 5).
  --longpoll  1: also holds one call with the `longpollid` of the last answer, on a second
              connection, and records its return. 0 (default): no such call. The node
              counts a long poll in `rpc_request_duration_seconds` with its wait, so the
              mean of that metric is then not the time of a call without `longpollid`.
  --timeout   Limit of a call without `longpollid`, in seconds (default 30).

The only method is `getblocktemplate`. The caller changes nothing in the node.

One line for each call:
  unix_us      Wall clock of this machine at the return of the call, µs since the epoch.
  mode         `poll` (no `longpollid`) or `longpoll`.
  duration_us  Time of the call on the client side: request sent to answer read.
  ok           true: the answer has a template. Then `height`, `previousblockhash` and
               `transactions` (the number of transactions of the template).
               false: `error` (`rpc`, `http`, `connection`, `timeout`, `cookie`,
               `answer`), `message`, and `http_status` and `rpc_code` when they exist.
An error is one line in the file and one line on the standard error output. The caller
then continues.

Long poll, error sources: a long poll also returns when the template changes on the same
block (the reader uses the first line with a new `previousblockhash` only); no long poll
is open between a return and the next request; the clock is the wall clock of the node
machine, and a step of that clock changes the value.
"""

import argparse
import base64
import http.client
import json
import signal
import socket
import sys
import threading
import time
import tomllib
import urllib.error
import urllib.request

# A long poll that has no answer after this time is sent again.
LONGPOLL_TIMEOUT_S = 600


def rpc_url(config_path):
    with open(config_path, "rb") as f:
        config = tomllib.load(f)
    return "http://" + config["rpc"]["listen_addr"] + "/"


def request(url, cookie_path, params, timeout):
    """One getblocktemplate call: the fields of its line, without the time and the mode."""
    try:
        with open(cookie_path) as f:
            cookie = f.read().strip()
    except OSError as e:
        return {"ok": False, "error": "cookie", "message": str(e)}, 0
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "getblocktemplate", "params": params})
    http_request = urllib.request.Request(
        url,
        data=body.encode(),
        headers={
            "Content-Type": "application/json",
            "Authorization": "Basic " + base64.b64encode(cookie.encode()).decode(),
        },
    )
    status = None
    start = time.monotonic_ns()
    try:
        try:
            with urllib.request.urlopen(http_request, timeout=timeout) as response:
                status, text = response.status, response.read()
        except urllib.error.HTTPError as e:
            # A JSON-RPC error can have another HTTP status than 200.
            status, text = e.code, e.read()
        fields = answer_fields(status, text)
    except (TimeoutError, socket.timeout):
        fields = {"ok": False, "error": "timeout", "message": f"no answer after {timeout} s"}
    except urllib.error.URLError as e:
        kind = "timeout" if isinstance(e.reason, TimeoutError) else "connection"
        fields = {"ok": False, "error": kind, "message": str(e.reason)}
    except (OSError, http.client.HTTPException) as e:
        fields = {"ok": False, "error": "connection", "message": f"{type(e).__name__}: {e}"}
    return fields, (time.monotonic_ns() - start) // 1000


def answer_fields(status, text):
    try:
        answer = json.loads(text)
    except ValueError:
        answer = None
    if not isinstance(answer, dict):
        kind = "answer" if status == 200 else "http"
        return {"ok": False, "error": kind, "http_status": status, "message": text[:200].decode(errors="replace")}
    error, result = answer.get("error"), answer.get("result")
    if error:
        fields = {"ok": False, "error": "rpc", "http_status": status}
        if isinstance(error, dict):
            fields.update(rpc_code=error.get("code"), message=str(error.get("message"))[:200])
        else:
            fields["message"] = str(error)[:200]
        return fields
    if not isinstance(result, dict) or "previousblockhash" not in result:
        return {"ok": False, "error": "answer", "http_status": status, "message": "no template in the result"}
    return {
        "ok": True,
        "height": result.get("height"),
        "previousblockhash": result["previousblockhash"],
        "transactions": len(result.get("transactions", [])),
        "longpollid": result.get("longpollid"),
    }


class Caller:
    def __init__(self, url, cookie, out, timeout):
        self.url, self.cookie, self.timeout = url, cookie, timeout
        self.out = open(out, "a", buffering=1)
        self.lock = threading.Lock()
        self.longpollid = None

    def call(self, mode, params, timeout):
        """Makes one call and writes its line. Returns the fields of the line."""
        fields, duration = request(self.url, self.cookie, params, timeout)
        line = {"unix_us": time.time_ns() // 1000, "mode": mode, "duration_us": duration, **fields}
        longpollid = line.pop("longpollid", None)
        with self.lock:
            if longpollid is not None:
                self.longpollid = longpollid
            self.out.write(json.dumps(line) + "\n")
        if not line["ok"]:
            print(f"race_rpc_caller: {mode}: {json.dumps(line)}", file=sys.stderr, flush=True)
        return line

    def poll(self, interval):
        next_call = time.monotonic()
        while True:
            self.call("poll", [], self.timeout)
            # A call that is longer than the interval does not make a burst of calls.
            next_call = max(next_call + interval, time.monotonic())
            time.sleep(next_call - time.monotonic())

    def longpoll(self, interval):
        while True:
            with self.lock:
                longpollid = self.longpollid
            if longpollid is None:
                time.sleep(interval)
                continue
            # hayaid waits only with the capability `longpoll` (BIP 22). zakurad waits
            # with the `longpollid` alone and accepts the capability.
            params = [{"capabilities": ["longpoll"], "longpollid": longpollid}]
            line = self.call("longpoll", params, LONGPOLL_TIMEOUT_S)
            with self.lock:
                same = self.longpollid == longpollid
            # An error, or a node that does not hold the call: no loop without a wait.
            if not line["ok"] or (same and line["duration_us"] < 1_000_000):
                time.sleep(interval)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--config")
    source.add_argument("--url")
    parser.add_argument("--cookie", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--interval", type=float, default=5)
    parser.add_argument("--longpoll", type=int, choices=(0, 1), default=0)
    parser.add_argument("--timeout", type=float, default=30)
    args = parser.parse_args()
    if args.interval <= 0:
        parser.error("--interval must be above 0")

    # The stop of a container sends SIGTERM.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    url = args.url or rpc_url(args.config)
    caller = Caller(url, args.cookie, args.out, args.timeout)
    print(
        f"race_rpc_caller: getblocktemplate on {url} each {args.interval} s,"
        f" long poll {'on' if args.longpoll else 'off'}, lines in {args.out}",
        file=sys.stderr,
        flush=True,
    )
    if args.longpoll:
        threading.Thread(target=caller.longpoll, args=(args.interval,), daemon=True).start()
    try:
        caller.poll(args.interval)
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
