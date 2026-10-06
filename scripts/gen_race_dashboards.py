#!/usr/bin/env python3
"""Generates the three dashboards of docker/race/grafana/dashboards."""
import json
import pathlib
import sys

OUT = pathlib.Path(sys.argv[1])
DS = {"type": "prometheus", "uid": "${datasource}"}


class Board:
    def __init__(self, uid, title, description, variables, annotations=()):
        self.d = {
            "uid": uid,
            "title": title,
            "description": description,
            "tags": ["hayai", "race"],
            "timezone": "utc",
            "schemaVersion": 41,
            "version": 1,
            "editable": False,
            "graphTooltip": 1,
            "refresh": "10s",
            "time": {"from": "now-6h", "to": "now"},
            "templating": {
                "list": [
                    {
                        "name": "datasource",
                        "label": "Data source",
                        "type": "datasource",
                        "query": "prometheus",
                        "current": {},
                    }
                ]
                + [
                    {
                        "name": name,
                        "label": label,
                        "type": "query",
                        "datasource": DS,
                        "query": {"query": query, "refId": "v"},
                        "refresh": 2,
                        "multi": False,
                        **(
                            {"includeAll": True, "allValue": ".*", "current": {"text": "All", "value": "$__all"}}
                            if name in ("instance", "zakura", "hayai")
                            else {"includeAll": False, "current": {}}
                        ),
                    }
                    for name, label, query in variables
                ]
            },
            "annotations": {"list": list(annotations)},
            "panels": [],
        }
        self.y = 0
        self.x = 0
        self.rowh = 0
        self.id = 0

    def _add(self, panel, w, h):
        if self.x + w > 24:
            self.y += self.rowh
            self.x, self.rowh = 0, 0
        self.id += 1
        panel["id"] = self.id
        panel["gridPos"] = {"h": h, "w": w, "x": self.x, "y": self.y}
        self.x += w
        self.rowh = max(self.rowh, h)
        self.d["panels"].append(panel)

    def row(self, title):
        self.y += self.rowh
        self.x, self.rowh = 0, 0
        self._add({"type": "row", "title": title, "collapsed": False, "panels": []}, 24, 1)

    def targets(self, exprs, instant=False):
        out = []
        for n, (expr, legend) in enumerate(exprs):
            t = {"refId": chr(65 + n), "datasource": DS, "expr": expr, "legendFormat": legend}
            if instant:
                t["instant"] = True
            out.append(t)
        return out

    def ts(self, title, description, exprs, unit="short", w=12, h=8, stack=False, points=False, steps=False):
        interval = {"interval": "5s"} if points or steps else {}
        custom = {"lineWidth": 1, "fillOpacity": 10 if stack else 0, "spanNulls": False}
        if steps:
            custom["lineInterpolation"] = "stepAfter"
        if stack:
            custom["stacking"] = {"mode": "normal", "group": "A"}
        if points:
            custom.update({"drawStyle": "points", "pointSize": 5, "showPoints": "always"})
        self._add(
            {
                "type": "timeseries",
                "title": title,
                "description": description,
                "datasource": DS,
                **interval,
                "targets": self.targets(exprs),
                "fieldConfig": {"defaults": {"unit": unit, "custom": custom}, "overrides": []},
                "options": {
                    "legend": {"displayMode": "table", "placement": "bottom", "calcs": ["lastNotNull", "max"]},
                    "tooltip": {"mode": "multi", "sort": "desc"},
                },
            },
            w,
            h,
        )

    def stat(self, title, description, exprs, unit="short", w=6, h=4, no_value=None):
        defaults = {"unit": unit}
        if no_value:
            defaults["noValue"] = no_value
        self._add(
            {
                "type": "stat",
                "title": title,
                "description": description,
                "datasource": DS,
                "targets": self.targets(exprs, instant=True),
                "fieldConfig": {"defaults": defaults, "overrides": []},
                "options": {
                    "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False},
                    "colorMode": "none",
                    "graphMode": "none",
                    "textMode": "value_and_name",
                    "orientation": "horizontal",
                },
            },
            w,
            h,
        )

    def text(self, title, content, w=24, h=12):
        self._add(
            {"type": "text", "title": title, "options": {"mode": "markdown", "content": content}},
            w,
            h,
        )

    def trend(self, title, description, height_expr, values, unit="s", w=12, h=9):
        """`values` ((legend, expression)) against the block height of `height_expr`. The
        transformations join the samples on the scrape time, then keep the last value
        of each height."""
        targets = [{"refId": "X", "datasource": DS, "expr": height_expr, "legendFormat": "block height"}]
        fields = {"block height": {"aggregations": [], "operation": "groupby"}}
        rename = {}
        for n, (legend, expr) in enumerate(values):
            targets.append({"refId": f"Y{n}", "datasource": DS, "expr": expr, "legendFormat": legend})
            fields[legend] = {"aggregations": ["last"], "operation": "aggregate"}
            rename[f"{legend} (last)"] = legend
        self._add(
            {
                "type": "trend",
                "title": title,
                "description": description,
                "datasource": DS,
                "interval": "5s",
                "targets": targets,
                "transformations": [
                    {"id": "joinByField", "options": {"byField": "Time", "mode": "outer"}},
                    {"id": "groupBy", "options": {"fields": fields}},
                    {"id": "organize", "options": {"renameByName": rename}},
                    {"id": "sortBy", "options": {"sort": [{"field": "block height", "desc": False}]}},
                ],
                "fieldConfig": {
                    "defaults": {
                        "unit": unit,
                        "custom": {"drawStyle": "points", "pointSize": 5, "showPoints": "always"},
                    },
                    "overrides": [
                        {
                            "matcher": {"id": "byName", "options": "block height"},
                            "properties": [{"id": "unit", "value": "none"}, {"id": "decimals", "value": 0}],
                        }
                    ],
                },
                "options": {
                    "xField": "block height",
                    "legend": {"displayMode": "list", "placement": "bottom"},
                    "tooltip": {"mode": "multi"},
                },
            },
            w,
            h,
        )

    def table(self, title, description, exprs, units=None, w=24, h=9, limit=None):
        """A table of the range samples of `exprs`, joined on the scrape time, newest first."""
        transformations = [
            {"id": "joinByField", "options": {"byField": "Time", "mode": "outer"}},
            {"id": "sortBy", "options": {"sort": [{"field": "Time", "desc": True}]}},
        ]
        if limit:
            transformations.append({"id": "limit", "options": {"limitField": limit}})
        overrides = [
            {"matcher": {"id": "byName", "options": name}, "properties": [{"id": "unit", "value": unit}]}
            for name, unit in (units or {}).items()
        ]
        self._add(
            {
                "type": "table",
                "title": title,
                "description": description,
                "datasource": DS,
                "interval": "5s",
                "targets": self.targets(exprs),
                "transformations": transformations,
                "fieldConfig": {"defaults": {"custom": {"align": "auto"}}, "overrides": overrides},
                "options": {"showHeader": True, "cellHeight": "sm"},
            },
            w,
            h,
        )

    def write(self, name):
        (OUT / name).write_text(json.dumps(self.d, indent=2) + "\n")


def mean(metric, sel, by="", window="$__rate_interval"):
    group = f" by ({by})" if by else ""
    return f"sum{group} (rate({metric}_sum{{{sel}}}[{window}])) / sum{group} (rate({metric}_count{{{sel}}}[{window}]))"


def quantile(q, metric, sel, by="le"):
    return f"histogram_quantile({q}, sum by ({by}) (rate({metric}_bucket{{{sel}}}[$__rate_interval])))"


def machine_panels(b, sel_by_name):
    """sel_by_name: [(legend, selector of the node-exporter)]."""
    disk = 'device=~"nvme[0-9]+n[0-9]+|sd[a-z]+|vd[a-z]+|xvd[a-z]+"'
    net = 'device!~"lo|docker.*|veth.*|br-.*"'
    fs = 'fstype=~"ext4|xfs|btrfs"'
    one = "Source: node-exporter of the machine. The machine runs one node."
    b.ts(
        "Machine CPU",
        f"Share of the CPU time of the machine that is not idle. {one}",
        [(f'1 - avg(rate(node_cpu_seconds_total{{{s}, mode="idle"}}[1m]))', n) for n, s in sel_by_name],
        "percentunit",
    )
    b.ts(
        "Machine memory in use",
        f"MemTotal minus MemAvailable. {one}",
        [(f"sum(node_memory_MemTotal_bytes{{{s}}} - node_memory_MemAvailable_bytes{{{s}}})", n) for n, s in sel_by_name],
        "bytes",
    )
    b.ts(
        "Disk in use",
        f"Used bytes of the file systems ext4, xfs and btrfs, one time for each device. {one}",
        [
            (
                f"sum(max by (device) (node_filesystem_size_bytes{{{s}, {fs}}} - node_filesystem_avail_bytes{{{s}, {fs}}}))",
                n,
            )
            for n, s in sel_by_name
        ],
        "bytes",
    )
    b.ts(
        "Disk I/O",
        f"Bytes read and written for each second, whole disks only. {one}",
        [(f"sum(rate(node_disk_read_bytes_total{{{s}, {disk}}}[1m]))", f"{n} read") for n, s in sel_by_name]
        + [(f"sum(rate(node_disk_written_bytes_total{{{s}, {disk}}}[1m]))", f"{n} written") for n, s in sel_by_name],
        "Bps",
    )
    b.ts(
        "Machine network",
        f"Bytes received and sent for each second, without the loopback and the Docker interfaces. {one}",
        [(f"sum(rate(node_network_receive_bytes_total{{{s}, {net}}}[1m]))", f"{n} received") for n, s in sel_by_name]
        + [(f"sum(rate(node_network_transmit_bytes_total{{{s}, {net}}}[1m]))", f"{n} sent") for n, s in sel_by_name],
        "Bps",
        w=24,
    )


def sidecar_panels(b, sel, zakura):
    """The race_* metrics of the sidecar of the machine (scripts/race_sidecar.py, help
    text of each metric). `sel`: the selector of the node-exporter."""
    src = "Source: the sidecar of the machine (scripts/race_sidecar.py), through node-exporter."
    cg = "the cgroup v2 of the node container (slice race-<node>.slice)"
    b.ts("Container CPU", f"rate of race_node_cpu_seconds_total over 1 minute: CPU cores of {cg}. {src}", [(f"rate(race_node_cpu_seconds_total{{{sel}}}[1m])", "cores")], "none", w=8)
    b.ts("Container memory", f"race_node_memory_bytes (memory.current) and race_node_memory_peak_bytes (memory.peak) of {cg}, with the page cache. {src}", [
        (f"race_node_memory_bytes{{{sel}}}", "current"),
        (f"race_node_memory_peak_bytes{{{sel}}}", "peak"),
    ], "bytes", w=8)
    b.ts("Container disk I/O", f"rate of race_node_io_read_bytes_total and race_node_io_write_bytes_total over 1 minute: io.stat of {cg}. {src}", [
        (f"rate(race_node_io_read_bytes_total{{{sel}}}[1m])", "read"),
        (f"rate(race_node_io_write_bytes_total{{{sel}}}[1m])", "written"),
    ], "Bps", w=8)
    b.ts("Data directory", f"race_node_data_bytes: disk blocks of the data directory of the node (du), each 60 s. {src}", [(f"race_node_data_bytes{{{sel}}}", "data directory")], "bytes", w=8)
    b.ts("Template served after a block", f"race_last_block_template_served_seconds: first getblocktemplate answer of the RPC caller on the last block minus the commit time of the block on this machine. {src}", [(f"race_last_block_template_served_seconds{{{sel}}}", "served")], "s", w=8, steps=True)
    b.ts("Template transactions after a block", f"race_last_block_template_transactions: transactions of the first answer without longpollid at or after the first answer on the last block. {src}", [(f"race_last_block_template_transactions{{{sel}}}", "transactions")], "none", w=8, steps=True)
    b.ts("getblocktemplate time on the client side", f"race_rpc_getblocktemplate_seconds: mean time of a call without longpollid on the client side (rate of _sum / rate of _count over 5 minutes). The node metric rpc_request_duration_seconds has the wait of each long poll. {src}", [
        (f"rate(race_rpc_getblocktemplate_seconds_sum{{{sel}}}[5m]) / rate(race_rpc_getblocktemplate_seconds_count{{{sel}}}[5m])", "mean"),
    ], "s", w=8)
    if zakura:
        b.ts("Block received to committed", f"race_last_block_received_to_committed_seconds: trace row block_request_finish (on the wall clock through the clock calibration) to the log line 'downloaded and verified gossiped block', for the last gossiped block, and race_last_block_clock_error_seconds: the error of the calibration. {src}", [
            (f"race_last_block_received_to_committed_seconds{{{sel}}}", "received to committed"),
            (f"race_last_block_clock_error_seconds{{{sel}}}", "error of the calibration"),
        ], "s", w=8, steps=True)


# ---------------------------------------------------------------- hayai node

def hayai_node():
    S = 'job=~"$job", instance=~"$instance"'
    b = Board(
        "race-hayai-node",
        "hayai node",
        "Each metric that hayaid exports (docs/hayaid.md, Metrics).",
        [
            ("job", "Job", "label_values(hayai_build_info, job)"),
            ("instance", "Instance", 'label_values(hayai_build_info{job=~"$job"}, instance)'),
            ("machine", "node-exporter of the machine", "label_values(node_cpu_seconds_total, instance)"),
        ],
    )
    b.row("Chain")
    b.stat("Block height", "zcash_chain_verified_block_height: the committed tip.", [(f"zcash_chain_verified_block_height{{{S}}}", "tip")], "none", w=4)
    b.stat("Header height", "hayai_sync_header_height: the best header chain (full mode).", [(f"hayai_sync_header_height{{{S}}}", "headers")], "none", w=4)
    b.stat("Height on disk", "state_finalized_block_height: the newest block whose coins are in the coins store on disk. hayai_base_height: the base in memory.", [(f"state_finalized_block_height{{{S}}}", "flushed"), (f"hayai_base_height{{{S}}}", "base")], "none", w=4)
    b.stat("Peers", "zcash_net_peers: peers after the handshake. hayai_peers: each connection of the relay. hayai_sync_peers: peers of the block download.", [(f"zcash_net_peers{{{S}}}", "established"), (f"hayai_peers{{{S}}}", "connections"), (f"hayai_sync_peers{{{S}}}", "download")], "none", w=6)
    b.stat("Build", "hayai_build_info: version, chain, mode, crypto backend and coins backend of the node.", [(f"hayai_build_info{{{S}}}", "{{version}} {{chain}} {{mode}} {{crypto_backend}} {{coins_backend}}")], "none", w=6)
    b.ts("Heights", "Committed tip (zcash_chain_verified_block_height, state_memory_best_committed_block_height), header chain, base in memory, height on disk.", [
        (f"zcash_chain_verified_block_height{{{S}}}", "tip"),
        (f"state_memory_best_committed_block_height{{{S}}}", "tip of the state in memory"),
        (f"hayai_sync_header_height{{{S}}}", "headers"),
        (f"hayai_base_height{{{S}}}", "base in memory"),
        (f"state_finalized_block_height{{{S}}}", "on disk"),
    ], "none")
    b.ts("Blocks for each second", "rate of zcash_chain_verified_block_total over 1 minute, and the blocks that the download stored (sync_downloaded_block_count).", [
        (f"rate(zcash_chain_verified_block_total{{{S}}}[1m])", "committed"),
        (f"rate(sync_downloaded_block_count{{{S}}}[1m])", "downloaded"),
    ], "none")
    b.ts("Blocks not committed", "For each minute: rejected blocks, blocks that a reorg disconnected, blocks without the hashBlockCommitments check.", [
        (f"increase(hayai_blocks_rejected_total{{{S}}}[1m])", "rejected"),
        (f"increase(hayai_blocks_disconnected_total{{{S}}}[1m])", "disconnected"),
        (f"increase(hayai_block_commitments_unchecked_total{{{S}}}[1m])", "commitments not checked"),
    ], "none")
    b.ts("Block download", "hayai_sync_requests_in_flight: requests without an answer. sync_downloads_in_flight: requests plus downloaded blocks that wait for the validator. hayai_sync_bodies_withheld: 1 while a header chain is out of the fork choice.", [
        (f"hayai_sync_requests_in_flight{{{S}}}", "requests in flight"),
        (f"sync_downloads_in_flight{{{S}}}", "in flight and held"),
        (f"hayai_sync_bodies_withheld{{{S}}}", "bodies withheld"),
        (f"hayai_sync_withheld_chains_total{{{S}}}", "withheld chains (total)"),
    ], "none")
    b.ts("Held bytes", "hayai_sync_held_bytes: bytes of the downloaded blocks that wait for the validator.", [(f"hayai_sync_held_bytes{{{S}}}", "held")], "bytes")
    b.ts("P2P bytes", "rate of zcash_net_in_bytes_total and zcash_net_out_bytes_total over 1 minute.", [
        (f"rate(zcash_net_in_bytes_total{{{S}}}[1m])", "received"),
        (f"rate(zcash_net_out_bytes_total{{{S}}}[1m])", "sent"),
    ], "Bps")

    b.row("Last block (one value for each block; docs/hayaid.md, Block clock)")
    H = f"hayai_last_block_height{{{S}}}"
    b.trend("Received to validated and to committed against block height", "hayai_last_block_received_to_validated_seconds and hayai_last_block_received_to_committed_seconds against hayai_last_block_height. One point for each block that a scrape read.", f"{H} > 0", [
        ("received to validated", f"hayai_last_block_received_to_validated_seconds{{{S}}}"),
        ("received to committed", f"hayai_last_block_received_to_committed_seconds{{{S}}}"),
    ])
    b.trend("Received to template ready against block height", "hayai_last_template_received_to_ready_seconds against hayai_last_template_tip_height, for the full template. At the tip the template can be ready before the block is validated (speculative tip). The panel 'Template of the last block' has both templates against time.", f'hayai_last_template_tip_height{{{S}, template="full"}} > 0', [
        ("full template", f'hayai_last_template_received_to_ready_seconds{{{S}, template="full"}}'),
    ])
    b.trend("Contextual commit and commit against block height", "hayai_last_block_contextual_commit_seconds (stages context, trees, history and the push of the layer) and hayai_last_block_commit_seconds (valid verdict to commit_finish).", f"{H} > 0", [
        ("contextual commit", f"hayai_last_block_contextual_commit_seconds{{{S}}}"),
        ("commit", f"hayai_last_block_commit_seconds{{{S}}}"),
    ])
    b.trend("Size against block height", "hayai_last_block_size_bytes against hayai_last_block_height.", f"{H} > 0", [("bytes", f"hayai_last_block_size_bytes{{{S}}}")], unit="bytes")
    b.ts("Durations of the last block", "received to validated, start to validated (validation), validated to committed (commit), received to committed, contextual commit.", [
        (f"hayai_last_block_received_to_validated_seconds{{{S}}}", "received to validated"),
        (f"hayai_last_block_validation_seconds{{{S}}}", "validation"),
        (f"hayai_last_block_commit_seconds{{{S}}}", "commit"),
        (f"hayai_last_block_received_to_committed_seconds{{{S}}}", "received to committed"),
        (f"hayai_last_block_contextual_commit_seconds{{{S}}}", "contextual commit"),
    ], "s")
    b.ts("Template of the last block", "hayai_last_template_received_to_ready_seconds: reception of the tip block to the first empty and the first full template on it.", [
        (f"hayai_last_template_received_to_ready_seconds{{{S}}}", "{{template}}"),
    ], "s")
    b.ts("Validation stages of the last block", "hayai_last_block_validate_stage_seconds, without the stage total. prepare_unknown: transactions that the prepared store did not have. scripts, shielded: signatures and proofs. lookup, context, trees, history: checks against the state.", [
        (f'hayai_last_block_validate_stage_seconds{{{S}, stage!="total"}}', "{{stage}}"),
    ], "s", stack=True)
    b.ts("Size of the last block", "hayai_last_block_size_bytes.", [(f"hayai_last_block_size_bytes{{{S}}}", "bytes")], "bytes", w=6)
    b.ts("Transactions of the last block", "hayai_last_block_transactions (with the coinbase), and hayai_last_block_prepared_transactions: found in the prepared store (known) or prepared at block time (unknown).", [
        (f"hayai_last_block_transactions{{{S}}}", "transactions"),
        (f"hayai_last_block_prepared_transactions{{{S}}}", "{{state}}"),
    ], "none", w=6)
    b.ts("Source of the last block", "hayai_last_block_source: 1 for the source of the last block. download, compact, legacy, local (a block of this node), stored, upstream_rpc.", [
        (f"hayai_last_block_source{{{S}}} == 1", "{{source}}"),
    ], "none", w=6, points=True)
    b.stat("Last block", "hayai_last_block_height and hayai_last_block_hash_suffix (the last 12 hexadecimal digits of the hash, as an integer).", [(H, "height"), (f"hayai_last_block_hash_suffix{{{S}}}", "hash suffix")], "none", w=6, h=8)
    b.table("Last blocks", "One row for each scrape, newest first: the gauges of the last block. The same block has several rows.", [
        (H, "height"),
        (f"hayai_last_block_received_to_validated_seconds{{{S}}}", "received to validated"),
        (f"hayai_last_block_received_to_committed_seconds{{{S}}}", "received to committed"),
        (f"hayai_last_block_contextual_commit_seconds{{{S}}}", "contextual commit"),
        (f'hayai_last_template_received_to_ready_seconds{{{S}, template="empty"}}', "empty template"),
        (f'hayai_last_template_received_to_ready_seconds{{{S}, template="full"}}', "full template"),
        (f"hayai_last_block_transactions{{{S}}}", "transactions"),
        (f"hayai_last_block_size_bytes{{{S}}}", "bytes"),
    ], limit=200)

    b.row("Block durations (histograms)")
    for title, metric, text in [
        ("Received to committed", "hayai_block_receive_to_commit_seconds", "Reception of a block to the moment at which it is the tip."),
        ("Received to validated", "hayai_block_received_to_validated_seconds", "Reception of a block to its valid verdict."),
        ("Contextual commit", "hayai_contextual_commit_duration_seconds", "Stages context, trees, history and the push of the layer."),
        ("commit_start to commit_finish", "hayai_commit_duration_seconds", "hayai_commit_duration_seconds."),
        ("Body prebuild", "hayai_prebuild_duration_seconds", "One prebuild of a body: the template of the node or a candidate of a peer."),
    ]:
        b.ts(title, f"{metric}: mean and quantiles. {text}", [
            (mean(metric, S), "mean"),
            (quantile(0.5, metric, S), "p50"),
            (quantile(0.9, metric, S), "p90"),
            (quantile(0.99, metric, S), "p99"),
        ], "s", w=8)
    b.ts("Block verify time by result", "sync_block_verify_duration_seconds: reception of a block to its commit (success) or to its rejection (failure). Mean.", [
        (mean("sync_block_verify_duration_seconds", S, "result"), "{{result}}"),
    ], "s", w=8)
    b.ts("Validation stages", "hayai_validate_stage_duration_seconds: mean of each stage.", [
        (mean("hayai_validate_stage_duration_seconds", S, "stage"), "{{stage}}"),
    ], "s")
    b.ts("Template after a tip change", "hayai_template_latency_seconds: tip change to the published template. hayai_block_received_to_template_seconds: reception of the block to the first template on it. Means.", [
        (mean("hayai_template_latency_seconds", S, "template"), "since the tip change: {{template}}"),
        (mean("hayai_block_received_to_template_seconds", S, "template"), "since the reception: {{template}}"),
    ], "s")

    b.row("Mempool and template")
    b.ts("Mempool size", "zcash_mempool_size_transactions: transactions in the prepared store.", [(f"zcash_mempool_size_transactions{{{S}}}", "transactions")], "none", w=8)
    b.ts("Mempool bytes", "zcash_mempool_size_bytes: wire bytes of the prepared store.", [(f"zcash_mempool_size_bytes{{{S}}}", "bytes")], "bytes", w=8)
    b.ts("Mempool rejections", "hayai_mempool_rejected_total for each minute, by reason.", [(f"increase(hayai_mempool_rejected_total{{{S}}}[1m])", "{{reason}}")], "none", w=8)
    b.ts("Prepared store use", "Block transactions from the prepared store (hits) and prepared at block time (misses), for each minute.", [
        (f"increase(hayai_prepared_store_hits_total{{{S}}}[1m])", "hits"),
        (f"increase(hayai_prepared_store_misses_total{{{S}}}[1m])", "misses"),
    ], "none", w=8)
    b.ts("Template updates", "hayai_template_updates_total for each minute: full and changed templates.", [(f"increase(hayai_template_updates_total{{{S}}}[1m])", "updates")], "none", w=8)
    b.ts("Prebuilt commits", "hayai_prebuilt_commits_total for each minute, by origin of the body.", [(f"increase(hayai_prebuilt_commits_total{{{S}}}[1m])", "{{origin}}")], "none", w=8)

    b.row("Relay and trace")
    b.ts("Compact relay", "Counters of Relay::metrics() for each minute.", [
        (f"increase(hayai_relay_forwarded_on_ids_total{{{S}}}[1m])", "forwarded on ids"),
        (f"increase(hayai_relay_forwarded_without_auth_root_total{{{S}}}[1m])", "forwarded without auth root"),
        (f"increase(hayai_relay_forwarded_after_body_total{{{S}}}[1m])", "forwarded after body"),
        (f"increase(hayai_relay_root_mismatches_total{{{S}}}[1m])", "root mismatches"),
    ], "none", w=8)
    b.ts("Candidate blocks", "Candidate blocks sent, resolved from a stored candidate, and fetched in full, for each minute.", [
        (f"increase(hayai_relay_candidate_blocks_sent_total{{{S}}}[1m])", "sent"),
        (f"increase(hayai_relay_candidate_blocks_resolved_total{{{S}}}[1m])", "resolved"),
        (f"increase(hayai_relay_candidate_fallbacks_total{{{S}}}[1m])", "fallbacks"),
    ], "none", w=8)
    b.ts("Trace rows dropped", "hayai_trace_dropped_rows_total by table. A value above 0 means that the trace tables have gaps.", [(f"hayai_trace_dropped_rows_total{{{S}}}", "{{table}}")], "none", w=8)

    b.row("RPC")
    b.ts("Requests", "rpc_requests_total for each second, by method and status.", [(f"sum by (method, status) (rate(rpc_requests_total{{{S}}}[1m]))", "{{method}} {{status}}")], "reqps", w=8)
    b.ts("Mean service time", "rpc_request_duration_seconds: mean by method.", [(mean("rpc_request_duration_seconds", S, "method"), "{{method}}")], "s", w=8)
    b.ts("Errors and active requests", "rpc_errors_total for each minute by method and code, and rpc_active_requests.", [
        (f"increase(rpc_errors_total{{{S}}}[1m])", "{{method}} {{error_code}}"),
        (f"rpc_active_requests{{{S}}}", "active"),
    ], "none", w=8)

    b.row("Shadow mode")
    b.ts("Upstream verdicts", "hayai_shadow_agreements_total and hayai_shadow_disagreements_total. No data in full mode.", [
        (f"hayai_shadow_agreements_total{{{S}}}", "agreements"),
        (f"hayai_shadow_disagreements_total{{{S}}}", "disagreements"),
    ], "none")
    b.ts("Trust limits", "Coins, nullifiers, anchors and header bits that shadow mode took from upstream.", [
        (f"hayai_shadow_trusted_coins_total{{{S}}}", "coins"),
        (f"hayai_shadow_trusted_nullifiers_total{{{S}}}", "nullifiers"),
        (f"hayai_shadow_trusted_anchors_total{{{S}}}", "anchors"),
        (f"hayai_shadow_trusted_bits_total{{{S}}}", "bits"),
    ], "none")

    b.row("Process and coins store")
    b.ts("Process CPU", "rate of process_cpu_seconds_total over 1 minute: CPU cores in use.", [(f"rate(process_cpu_seconds_total{{{S}}}[1m])", "cores")], "none", w=6)
    b.ts("Process memory", "process_resident_memory_bytes.", [(f"process_resident_memory_bytes{{{S}}}", "resident")], "bytes", w=6)
    b.ts("Coins cache", "hayai_coins_cache_entries and hayai_coins_store_coins (memory backend).", [
        (f"hayai_coins_cache_entries{{{S}}}", "cache entries"),
        (f"hayai_coins_store_coins{{{S}}}", "coins in the memory backing"),
    ], "none", w=6)
    b.ts("Coins cache memory", "hayai_coins_cache_bytes.", [(f"hayai_coins_cache_bytes{{{S}}}", "cache")], "bytes", w=6)

    b.row("Sidecar of the machine (race_* metrics of the node-exporter of the variable)")
    sidecar_panels(b, 'job="node", instance=~"$machine"', zakura=False)

    b.row("Machine (the node-exporter of the variable; the machine runs one node)")
    machine_panels(b, [("machine", 'instance=~"$machine"')])
    b.write("hayai-node.json")
    return b


# ---------------------------------------------------------------- zakura node

def zakura_node():
    S = 'job=~"$job", instance=~"$instance"'
    b = Board(
        "race-zakura-node",
        "Zakura node",
        "Metrics that zakurad 1.6.0 exports with the legacy P2P stack (docs/zakura-measurements.md). Each duration is a summary: the panels use the sum and the count.",
        [
            ("job", "Job", "label_values(zakura_build_info, job)"),
            ("instance", "Instance", 'label_values(zakura_build_info{job=~"$job"}, instance)'),
            ("machine", "node-exporter of the machine", "label_values(node_cpu_seconds_total, instance)"),
        ],
    )
    b.row("Chain")
    b.stat("Block height", "zcash_chain_verified_block_height.", [(f"zcash_chain_verified_block_height{{{S}}}", "tip")], "none", w=4)
    b.stat("Estimated network tip", "sync_estimated_network_tip_height: estimate from the time of the tip block and the clock. sync_estimated_distance_to_tip: blocks to that height.", [(f"sync_estimated_network_tip_height{{{S}}}", "estimate"), (f"sync_estimated_distance_to_tip{{{S}}}", "distance")], "none", w=6)
    b.stat("Height on disk", "state_finalized_block_height: the newest block in RocksDB. At the tip it is the tip minus the reorg window.", [(f"state_finalized_block_height{{{S}}}", "finalized")], "none", w=4)
    b.stat("Peers", "zcash_net_peers: ready and not ready peers of the legacy stack.", [(f"zcash_net_peers{{{S}}}", "peers"), (f"pool_num_ready{{{S}}}", "ready"), (f"pool_num_unready{{{S}}}", "not ready")], "none", w=6)
    b.stat("Build", "zakura_build_info.", [(f"zakura_build_info{{{S}}}", "{{version}}")], "none", w=4)
    b.ts("Heights", "Committed tip, tip of the non-finalized state, full verifier, checkpoint verifier, height on disk.", [
        (f"zcash_chain_verified_block_height{{{S}}}", "tip"),
        (f"state_memory_best_committed_block_height{{{S}}}", "best chain in memory"),
        (f"state_full_verifier_committed_block_height{{{S}}}", "full verifier"),
        (f"checkpoint_verified_height{{{S}}}", "checkpoint verifier"),
        (f"state_checkpoint_finalized_block_height{{{S}}}", "checkpoint finalized"),
        (f"state_finalized_block_height{{{S}}}", "on disk"),
    ], "none")
    b.ts("Blocks for each second", "Rates over 1 minute: committed blocks, blocks from gossip (downloaded, verified), blocks of the checkpoint verifier.", [
        (f"rate(zcash_chain_verified_block_total{{{S}}}[1m])", "committed"),
        (f"rate(gossip_downloaded_block_count{{{S}}}[1m])", "gossip downloaded"),
        (f"rate(gossip_verified_block_count{{{S}}}[1m])", "gossip verified"),
        (f"rate(checkpoint_verified_block_count{{{S}}}[1m])", "checkpoint verified"),
    ], "none")
    b.ts("Checkpoint verifier", "checkpoint_processing_next_height, checkpoint_queued_continuous_height, checkpoint_queued_max_height, checkpoint_queued_slots.", [
        (f"checkpoint_processing_next_height{{{S}}}", "next height"),
        (f"checkpoint_queued_continuous_height{{{S}}}", "queued continuous height"),
        (f"checkpoint_queued_max_height{{{S}}}", "queued largest height"),
        (f"checkpoint_queued_slots{{{S}}}", "queued slots"),
    ], "none")
    b.ts("Download queue", "sync_downloads_in_flight (tasks of the legacy syncer in download or in verification) and its parts.", [
        (f"sync_downloads_in_flight{{{S}}}", "in flight"),
        (f"sync_downloads_waiting_network{{{S}}}", "waiting network"),
        (f"sync_downloads_downloading{{{S}}}", "downloading"),
        (f"sync_downloads_response_received{{{S}}}", "response received"),
        (f"sync_downloads_waiting_verifier{{{S}}}", "waiting verifier"),
        (f"sync_downloads_verifying{{{S}}}", "verifying"),
        (f"sync_prospective_tips_len{{{S}}}", "prospective tips"),
        (f"gossip_queued_block_count{{{S}}}", "gossip queued"),
    ], "none")
    b.ts("Sync stages", "sync_stage_duration_seconds: mean by stage.", [(mean("sync_stage_duration_seconds", S, "stage"), "{{stage}}")], "s")
    b.ts("State in memory", "Chains and blocks of the non-finalized state, and queued blocks.", [
        (f"state_memory_chain_count{{{S}}}", "chains"),
        (f"state_memory_best_chain_length{{{S}}}", "best chain length"),
        (f"state_memory_queued_block_count{{{S}}}", "queued blocks"),
        (f"state_checkpoint_queued_block_count{{{S}}}", "queued checkpoint blocks"),
    ], "none")

    b.row("Block commit (one sample for each block)")
    b.ts("Contextual commit", "state_contextual_total_duration_seconds: initial contextual checks and the commit to the non-finalized state. Mean over the rate interval.", [(mean("state_contextual_total_duration_seconds", S), "mean")], "s")
    b.ts("Parts of the contextual commit", "Means of the parts of state_contextual_total_duration_seconds.", [
        (mean(f"state_contextual_{part}_duration_seconds", S), part.replace("_", " "))
        for part in ["initial_checks", "parent_chain", "transparent_spend", "shielded_anchors", "sprout_anchor_fetch", "parallel_update", "parallel_task_chain_clone", "parallel_task_chain_push", "parallel_task_block_commitment", "block_construction", "chain_new"]
    ], "s")
    b.ts("Wait in the queue of the block writer", "state_block_writer_queue_duration_seconds: mean.", [(mean("state_block_writer_queue_duration_seconds", S), "mean")], "s", w=8)
    b.ts("Semantic commit", "state_semantic_commit_*_duration_seconds: means.", [
        (mean(f"state_semantic_commit_{part}_duration_seconds", S), part.replace("_", " "))
        for part in ["dispatch", "prequeue_checks", "queue_and_commit", "ready_wait"]
    ], "s", w=8)
    b.ts("Write to RocksDB", "zakura_state_rocksdb_batch_commit_duration_seconds: one sample for each finalized block. Mean.", [(mean("zakura_state_rocksdb_batch_commit_duration_seconds", S), "mean")], "s", w=8)

    b.row("Proofs and signatures")
    b.ts("Batch verification", "zakura_consensus_batch_duration_seconds: one sample for each batch, blocks and mempool together. Mean by verifier.", [(mean("zakura_consensus_batch_duration_seconds", S, "verifier, result"), "{{verifier}} {{result}}")], "s", w=8)
    b.ts("Verification cache", "zakura_consensus_cache_hit, _miss, _insert for each minute, and the cache size.", [
        (f"sum(increase(zakura_consensus_cache_hit{{{S}}}[1m]))", "hits"),
        (f"sum(increase(zakura_consensus_cache_miss{{{S}}}[1m]))", "misses"),
        (f"sum(increase(zakura_consensus_cache_insert{{{S}}}[1m]))", "inserts"),
        (f"sum(zakura_consensus_cache_size{{{S}}})", "size"),
    ], "none", w=8)
    b.ts("Halo 2 proofs", "proofs_halo2_verified for each minute.", [(f"increase(proofs_halo2_verified{{{S}}}[1m])", "proofs")], "none", w=8)

    b.row("Mempool")
    b.ts("Mempool size", "zcash_mempool_size_transactions and the queue of the downloader.", [
        (f"zcash_mempool_size_transactions{{{S}}}", "transactions"),
        (f"mempool_currently_queued_transactions{{{S}}}", "queued"),
        (f"mempool_rejected_transaction_ids{{{S}}}", "rejected ids"),
    ], "none", w=8)
    b.ts("Mempool bytes", "zcash_mempool_size_bytes and zcash_mempool_cost_bytes.", [
        (f"zcash_mempool_size_bytes{{{S}}}", "size"),
        (f"zcash_mempool_cost_bytes{{{S}}}", "cost"),
    ], "bytes", w=8)
    b.ts("Mempool transactions", "Transactions for each minute: queued, downloaded, pushed, verified, gossiped.", [
        (f"sum(increase(mempool_queued_transactions_total{{{S}}}[1m]))", "queued"),
        (f"sum(increase(mempool_downloaded_transactions_total{{{S}}}[1m]))", "downloaded"),
        (f"sum(increase(mempool_pushed_transactions_total{{{S}}}[1m]))", "pushed"),
        (f"sum(increase(mempool_verified_transactions_total{{{S}}}[1m]))", "verified"),
        (f"sum(increase(mempool_gossiped_transactions_total{{{S}}}[1m]))", "gossiped"),
    ], "none", w=8)

    b.row("Mining and RPC")
    b.ts("Template and block submission", "mining_*_duration_seconds: means. preparation: the build of a template for a request. zakurad builds a template only for a request.", [
        (mean(f"mining_{part}_duration_seconds", S), part.replace("_", " "))
        for part in ["preparation", "solved_header_check", "prepared_relay_preflight", "contextual_commit", "state_admission"]
    ], "s", w=8)
    b.ts("Prepared block cache", "mining_prepared_cache_hits, _misses, _evictions and cancelled or coalesced template preparations, for each minute.", [
        (f"increase(mining_prepared_cache_hits{{{S}}}[1m])", "hits"),
        (f"increase(mining_prepared_cache_misses{{{S}}}[1m])", "misses"),
        (f"increase(mining_prepared_cache_evictions{{{S}}}[1m])", "evictions"),
        (f"increase(mining_template_preparation_cancelled{{{S}}}[1m])", "cancelled"),
        (f"increase(mining_template_preparation_coalesced{{{S}}}[1m])", "coalesced"),
    ], "none", w=8)
    b.ts("RPC mean service time", "rpc_request_duration_seconds: mean by method.", [(mean("rpc_request_duration_seconds", S, "method"), "{{method}}")], "s", w=8)
    b.ts("RPC requests", "rpc_requests_total for each second, by method and status.", [(f"sum by (method, status) (rate(rpc_requests_total{{{S}}}[1m]))", "{{method}} {{status}}")], "reqps")
    b.ts("RPC errors and active requests", "rpc_errors_total for each minute, and rpc_active_requests.", [
        (f"increase(rpc_errors_total{{{S}}}[1m])", "{{method}} {{error_code}}"),
        (f"rpc_active_requests{{{S}}}", "active"),
    ], "none")

    b.row("Network")
    b.ts("P2P bytes", "rate of zcash_net_in_bytes_total and zcash_net_out_bytes_total over 1 minute: all legacy messages.", [
        (f"rate(zcash_net_in_bytes_total{{{S}}}[1m])", "received"),
        (f"rate(zcash_net_out_bytes_total{{{S}}}[1m])", "sent"),
    ], "Bps", w=8)
    b.ts("P2P messages", "zcash_net_in_messages and zcash_net_out_messages for each second, by command.", [
        (f"sum by (command) (rate(zcash_net_in_messages{{{S}}}[1m]))", "in {{command}}"),
        (f"sum by (command) (rate(zcash_net_out_messages{{{S}}}[1m]))", "out {{command}}"),
    ], "none", w=8)
    b.ts("Address book and handshakes", "candidate_set_* gauges, handshakes in flight, mean handshake time.", [
        (f"candidate_set_responded{{{S}}}", "responded"),
        (f"candidate_set_gossiped{{{S}}}", "gossiped"),
        (f"candidate_set_failed{{{S}}}", "failed"),
        (f"candidate_set_pending{{{S}}}", "pending"),
        (f"crawler_in_flight_handshakes{{{S}}}", "handshakes in flight"),
    ], "none", w=8)

    b.row("Database")
    b.ts("RocksDB size on disk", "zakura_state_rocksdb_total_disk_size_bytes and zakura_state_rocksdb_live_data_size_bytes.", [
        (f"zakura_state_rocksdb_total_disk_size_bytes{{{S}}}", "total"),
        (f"zakura_state_rocksdb_live_data_size_bytes{{{S}}}", "live data"),
    ], "bytes", w=8)
    b.ts("RocksDB memory", "zakura_state_rocksdb_total_memory_size_bytes and zakura_state_rocksdb_block_cache_usage_bytes.", [
        (f"zakura_state_rocksdb_total_memory_size_bytes{{{S}}}", "total"),
        (f"zakura_state_rocksdb_block_cache_usage_bytes{{{S}}}", "block cache"),
    ], "bytes", w=8)
    b.ts("RocksDB compaction", "zakura_state_rocksdb_compaction_pending_bytes and zakura_state_rocksdb_compaction_running.", [
        (f"zakura_state_rocksdb_compaction_pending_bytes{{{S}}}", "pending bytes"),
        (f"zakura_state_rocksdb_compaction_running{{{S}}}", "running"),
    ], "none", w=8)

    b.row("Sidecar of the machine (race_* metrics of the node-exporter of the variable). zakurad exports no process metric.")
    sidecar_panels(b, 'job="node", instance=~"$machine"', zakura=True)

    b.row("Machine (the node-exporter of the variable)")
    machine_panels(b, [("machine", 'instance=~"$machine"')])
    b.write("zakura-node.json")
    return b


# ---------------------------------------------------------------- comparison

DEFINITIONS = """
Verdict SAME: one method and one definition on both machines. Verdict CLOSE: both nodes measure the same work with the stated difference.
A quantity that only one node measures is not on a comparison panel. Source of each definition: `docs/zakura-measurements.md` and the help text of each `race_*` metric of the sidecar (`scripts/race_sidecar.py`).

| Quantity | zakurad | hayaid | Verdict and difference |
|---|---|---|---|
| Block height | `zcash_chain_verified_block_height`, set after the commit response | Same name, set at the commit | CLOSE. Same event: the tip after a commit |
| Blocks for each second | `rate(zcash_chain_verified_block_total[1m])` | Same | CLOSE |
| Time to the last checkpoint, time to the tip | First evaluation with the height at the checkpoint, or 2 blocks or less below the reference height | Same rule | CLOSE. Resolution 5 s. The reference is one value for both nodes: the highest of the header chain of hayaid and of the estimate of zakurad |
| Peers | `zcash_net_peers`: ready and not ready peers of the legacy stack | `zcash_net_peers`: peers after the handshake | CLOSE |
| Height on disk | `state_finalized_block_height`: the newest block in RocksDB | Same name: the newest block whose coins are in the coins store on disk. hayaid also has each block in its block files | CLOSE. hayaid flushes each 100 blocks by default; zakurad writes each finalized block |
| P2P bytes | `zcash_net_in_bytes_total`, `zcash_net_out_bytes_total`: header and body of each message at the codec | Same names: bytes that the reader took from the socket, and bytes of each frame written in full | CLOSE |
| Download queue | `sync_downloads_in_flight`: tasks in download or in verification | Same name: requests without an answer plus downloaded blocks that wait for the validator | CLOSE |
| Contextual commit time, for each block | `state_contextual_total_duration_seconds`: initial contextual checks, then the commit to the non-finalized state (transparent spends, anchors, note commitment trees, chain push) | `hayai_contextual_commit_duration_seconds`: stages context, trees, history, then the push of the layer on the chain | CLOSE. The split of the checks between this interval and the earlier validation is not the same on both nodes. zakurad includes a clone of the chain |
| Block received to committed, for each block | `race_last_block_received_to_committed_seconds` of the sidecar: trace row `block_request_finish` (on the wall clock through the clock calibration) to the log line `downloaded and verified gossiped block`. Gossiped blocks only | `race_last_block_received_to_committed_seconds` of the sidecar: trace field `commit_finish.received_to_commit_us` | CLOSE. Start: zakurad when the peer service returns the decoded block; hayaid at the arrival of the message, before the parse. Stop: zakurad after the commit in memory; hayaid after the write to the block file and the update of the prepared store. The zakurad value has the error `race_last_block_clock_error_seconds` |
| Template served after a block, for each block | `race_last_block_template_served_seconds`: first `getblocktemplate` answer of the RPC caller on the block (the long poll) minus the time of the log line `downloaded and verified gossiped block` | Same, minus `unix_us` of the trace row `commit_finish` | CLOSE. Same client and same stop. zakurad writes its log line after the commit response, so its start is later and a value can be below 0 |
| Template transactions after a block, for each block | `race_last_block_template_transactions`: the first answer without `longpollid` at or after the first answer on the block | Same | CLOSE. zakurad builds the template in the call; hayaid returns the template that it built at the tip change. The mempools differ |
| Reuse of the mempool verification | `zakura_consensus_cache_hit` / (hit + miss): shielded bundles, block and mempool lookups | `hayai_prepared_store_hits_total` / (hits + misses): transactions of blocks | CLOSE. Unit: bundle against transaction. zakurad reuses no script result |
| Mempool size | `zcash_mempool_size_transactions`, `zcash_mempool_size_bytes` | Same names: the prepared store | CLOSE. hayaid updates the gauges at each commit and each second |
| `getblocktemplate` time on the client side | `race_rpc_getblocktemplate_seconds` of the sidecar: calls without `longpollid` of the RPC caller, request sent to answer read | Same | CLOSE. zakurad builds the template in the call; hayaid returns the template that it built at the tip change. The node metric `rpc_request_duration_seconds` has the wait of each long poll on both nodes, so no panel reads it for `getblocktemplate` |
| RPC mean service time, other methods | `rpc_request_duration_seconds` (summary): sum / count | Same name (histogram): sum / count | CLOSE. zakurad parses the parameters inside the interval |
| CPU, memory and peak, disk I/O of the node | `race_node_*` of the sidecar: `cpu.stat`, `memory.current`, `memory.peak`, `io.stat` of the cgroup of the node container | Same | SAME. Memory includes the page cache of the container |
| Data directory | `race_node_data_bytes` of the sidecar: du of the data volume each 60 s | Same | CLOSE. The zakurad directory has its log file; hayaid writes its log to Docker |
| CPU, memory, disk, disk I/O, network of the machine | node-exporter of the machine of zakurad | node-exporter of the machine of hayaid | SAME. The values are of the machine, which runs one node and the race containers |

Not compared, because zakurad has no such measurement:

- Block received to block validated (hayai dashboard).
- Block received to template ready, and tip change to template ready (hayai dashboard). zakurad builds a template only when a `getblocktemplate` request arrives. An external probe is necessary for this comparison (docs/sync-race.md).
- Script check time, validation stages (hayai dashboard).
- Header height: zakurad with the legacy stack has no header chain.

Not compared, because hayaid has no such measurement: wait in the queue of the block writer, checkpoint verifier heights, RocksDB metrics (Zakura dashboard).

Not compared, because the definitions differ: `sync_block_verify_duration_seconds` appears on zakurad only for a block of the legacy syncer and was not seen on a running node; shielded proof time (batch on zakurad, block on hayaid).
"""


def comparison():
    Z = 'node=~"$zakura"'
    Hn = 'node=~"$hayai"'
    ZJ = f'job="zakurad", {Z}'
    HJ = f'job="hayaid", {Hn}'
    # The race_* metrics of the sidecars, through the node-exporter of each machine.
    ZN = f'job="node", {Z}'
    HN = f'job="node", {Hn}'
    both = lambda metric: [(f"{metric}{{{ZJ}}}", "zakurad"), (f"{metric}{{{HJ}}}", "hayaid")]
    rule = lambda series: [(f"{series}{{{ZJ}}}", "zakurad"), (f"{series}{{{HJ}}}", "hayaid")]
    by_node = lambda series: [
        (f"{series}{{{Z}}} and on (node) zakura_build_info", "zakurad"),
        (f"{series}{{{Hn}}} and on (node) hayai_build_info", "hayaid"),
    ]
    ann = lambda name, color, expr, text: {
        "name": name,
        "datasource": DS,
        "enable": True,
        "iconColor": color,
        "expr": expr,
        "useValueForTime": True,
        "titleFormat": text,
        "step": "60s",
    }
    b = Board(
        "race-comparison",
        "zakurad and hayaid: comparison",
        "Quantities that have a close meaning on both nodes, each with its difference (docs/sync-race.md, Compared quantities).",
        [
            ("zakura", "zakurad (label node)", "label_values(zakura_build_info, node)"),
            ("hayai", "hayaid (label node)", "label_values(hayai_build_info, node)"),
        ],
        [
            ann("start", "blue", "race:start_timestamp_seconds * 1000", "{{node}}: first scrape"),
            ann("last checkpoint", "orange", "race:checkpoint_timestamp_seconds * 1000", "{{node}}: last checkpoint"),
            ann("tip", "green", "race:tip_timestamp_seconds * 1000", "{{node}}: tip"),
        ],
    )
    b.row("Sync")
    b.stat("Time to the last checkpoint", "CLOSE. Seconds from the first scrape of the node to the last checkpoint of the network (race:last_checkpoint_height). Resolution 5 s.", rule("race:seconds_to_checkpoint"), "dtdurations", no_value="not yet")
    b.stat("Time to the tip", "CLOSE. Seconds from the first scrape to the first time that the block height is 2 blocks or less below the reference: the highest of the header chain of hayaid and of the network tip that zakurad estimates. One reference for both nodes.", rule("race:seconds_to_tip"), "dtdurations", no_value="not yet")
    b.stat("Block height", "CLOSE. zcash_chain_verified_block_height on both nodes: the tip after a commit.", rule("race:block_height"), "none")
    b.stat("Blocks behind the other node", "From the two block heights.", [
        (f"scalar(max(race:block_height)) - race:block_height{{{Z}, job=\"zakurad\"}}", "zakurad"),
        (f"scalar(max(race:block_height)) - race:block_height{{{Hn}, job=\"hayaid\"}}", "hayaid"),
    ], "none")
    b.ts("Block height", "CLOSE. zcash_chain_verified_block_height. zakurad sets it after the commit response; hayaid sets it at the commit.", rule("race:block_height"), "none")
    b.ts("Blocks for each second", "CLOSE. rate(zcash_chain_verified_block_total[1m]) on both nodes.", rule("race:blocks_per_second"), "none")
    b.ts("Height on disk", "CLOSE. state_finalized_block_height. zakurad: the newest block in RocksDB. hayaid: the newest block whose coins are in the coins store on disk (a flush each 100 blocks by default); its block files have each block.", both("state_finalized_block_height"), "none")
    b.ts("Peers", "CLOSE. zcash_net_peers. zakurad: ready and not ready peers of the legacy stack. hayaid: peers after the handshake.", rule("race:peers"), "none")
    b.ts("P2P bytes received", "CLOSE. rate(zcash_net_in_bytes_total[1m]). zakurad counts the header and the body of each message at the codec; hayaid counts the bytes that the reader took from the socket.", rule("race:net_in_bytes_per_second"), "Bps")
    b.ts("P2P bytes sent", "CLOSE. rate(zcash_net_out_bytes_total[1m]). zakurad counts at the codec; hayaid counts each frame written in full.", rule("race:net_out_bytes_per_second"), "Bps")
    b.ts("Download queue", "CLOSE. sync_downloads_in_flight. zakurad: tasks of the legacy syncer in download or in verification. hayaid: requests without an answer plus downloaded blocks that wait for the validator.", both("sync_downloads_in_flight"), "none")
    b.ts("Mempool size", "CLOSE. zcash_mempool_size_transactions. hayaid: the prepared store, updated at each commit and each second.", both("zcash_mempool_size_transactions"), "none")

    b.row("Tip, for each block")
    last = lambda series, sel: f"last_over_time({series}{{{sel}}}[$__interval])"
    step = " The panel reads the last sample of each step: with a step above the time between two blocks (a long time range) it shows a part of the blocks."
    one_x = " A panel has one X field, and each node has its own height series: one panel for each node. The panel against time below has both nodes."
    b.trend("Contextual commit time against block height: zakurad", "CLOSE. One point for each block: race:contextual_commit_seconds:last against race:contextual_commit_block_height. zakurad: state_contextual_total_duration_seconds (initial contextual checks and the commit to the non-finalized state, with a clone of the chain). Two blocks in one scrape interval of 5 s give one point with their mean." + one_x + step, last("race:contextual_commit_block_height", ZJ), [("zakurad", last("race:contextual_commit_seconds:last", ZJ))])
    b.trend("Contextual commit time against block height: hayaid", "CLOSE. One point for each block: race:contextual_commit_seconds:last against race:contextual_commit_block_height. hayaid: hayai_contextual_commit_duration_seconds (stages context, trees, history and the push of the layer). Two blocks in one scrape interval of 5 s give one point with their mean." + one_x + step, last("race:contextual_commit_block_height", HJ), [("hayaid", last("race:contextual_commit_seconds:last", HJ))])
    b.ts("Contextual commit time against time", "CLOSE. The same series as the panel on the left, against the time of the scrape after the commit." + step, [(last("race:contextual_commit_seconds:last", ZJ), "zakurad"), (last("race:contextual_commit_seconds:last", HJ), "hayaid")], "s", points=True, h=9)
    r2c = "race_last_block_received_to_committed_seconds of the sidecar of each machine. Start: zakurad when the peer service returns the decoded block (trace row block_request_finish on the wall clock through the clock calibration, error in race_last_block_clock_error_seconds); hayaid at the arrival of the block message, before the parse. Stop: zakurad after the commit in memory (log line 'downloaded and verified gossiped block'); hayaid after the write to the block file and the update of the prepared store. zakurad has a value for a gossiped block only."
    b.trend("Block received to committed against block height: zakurad", f"CLOSE. One point for each block. {r2c}" + one_x + step, last("race_last_block_height", ZN), [("zakurad", last("race_last_block_received_to_committed_seconds", ZN))])
    b.trend("Block received to committed against block height: hayaid", f"CLOSE. One point for each block. {r2c}" + one_x + step, last("race_last_block_height", HN), [("hayaid", last("race_last_block_received_to_committed_seconds", HN))])
    b.ts("Block received to committed against time", f"CLOSE. {r2c} Each step of the line is one block.", [(f"race_last_block_received_to_committed_seconds{{{ZN}}}", "zakurad"), (f"race_last_block_received_to_committed_seconds{{{HN}}}", "hayaid")], "s", steps=True, h=9)
    b.ts("Template served after a block", "CLOSE. race_last_block_template_served_seconds: the first getblocktemplate answer of the RPC caller of the machine with the last block as previousblockhash (the long poll), minus the commit time of the block on the same machine. zakurad: the time of the log line 'downloaded and verified gossiped block', which comes after the commit response, so a value can be below 0. hayaid: unix_us of the trace row commit_finish. Same client and same stop on both machines. Each step of the line is one block.", [(f"race_last_block_template_served_seconds{{{ZN}}}", "zakurad"), (f"race_last_block_template_served_seconds{{{HN}}}", "hayaid")], "s", steps=True, h=9)
    b.ts("Reuse of the mempool verification", "CLOSE. race:verification_reuse:ratio5m. zakurad: hits of the verification cache of shielded bundles / lookups, for blocks and mempool. hayaid: transactions of blocks found in the prepared store / transactions of blocks. Unit: bundle against transaction.", by_node("race:verification_reuse:ratio5m"), "percentunit", h=9)
    b.table("Last blocks: contextual commit time", "CLOSE. One row for each scrape in which a node has a value, newest first. The rows are joined on the scrape time, not on the block height: the two height columns show when the nodes are at different blocks. Grafana cannot join two nodes on a value, and the height is not a label. The table that is joined on the height is blocks.csv of `collect`." + step, [
        (last("race:contextual_commit_block_height", ZJ), "zakurad height"),
        (last("race:contextual_commit_seconds:last", ZJ), "zakurad contextual commit"),
        (last("race:contextual_commit_block_height", HJ), "hayaid height"),
        (last("race:contextual_commit_seconds:last", HJ), "hayaid contextual commit"),
    ], units={"zakurad contextual commit": "s", "hayaid contextual commit": "s"}, limit=100)
    b.ts("Template transactions after a block", "CLOSE. race_last_block_template_transactions: transactions of the first getblocktemplate answer without longpollid at or after the first answer on the last block. The RPC caller sends such a call at once after each long-poll answer. zakurad builds the template in the call; hayaid returns the template that it built at the tip change. The mempools differ: each node has other peers. Each step of the line is one block.", [(f"race_last_block_template_transactions{{{ZN}}}", "zakurad"), (f"race_last_block_template_transactions{{{HN}}}", "hayaid")], "none", steps=True, w=8)
    b.ts("getblocktemplate time on the client side", "CLOSE. race:rpc_getblocktemplate_seconds:mean5m: mean time of a getblocktemplate call without longpollid over 5 minutes, on the client side (request sent to answer read), from the file of the RPC caller: the call of each interval and the call after each long-poll answer. Without longpollid, zakurad builds the template in the call and hayaid returns the template that it built at the tip change. The node metric rpc_request_duration_seconds is not on this panel: both nodes count the wait of each long poll in it.", [
        (f"race:rpc_getblocktemplate_seconds:mean5m{{{ZN}}}", "zakurad"),
        (f"race:rpc_getblocktemplate_seconds:mean5m{{{HN}}}", "hayaid"),
    ], "s", w=8)
    b.ts("RPC mean service time: sendrawtransaction", "CLOSE. Both calls wait for the result of the mempool admission. hayaid includes the announcement to the relay.", [
        (f'race:rpc_request_seconds:mean5m{{{Z}, method="sendrawtransaction"}} and on (node) zakura_build_info', "zakurad"),
        (f'race:rpc_request_seconds:mean5m{{{Hn}, method="sendrawtransaction"}} and on (node) hayai_build_info', "hayaid"),
    ], "s", w=8)
    b.ts("RPC mean service time: other methods", "CLOSE. zakurad parses the parameters inside the interval; hayaid parses them before.", [
        (f'race:rpc_request_seconds:mean5m{{{Z}, method!~"getblocktemplate|sendrawtransaction"}} and on (node) zakura_build_info', "zakurad {{method}}"),
        (f'race:rpc_request_seconds:mean5m{{{Hn}, method!~"getblocktemplate|sendrawtransaction"}} and on (node) hayai_build_info', "hayaid {{method}}"),
    ], "s", w=8)

    b.row("Resources of the node container (the sidecar of each machine, cgroup v2 of the container)")
    cg = "Same method on both machines: the sidecar reads the cgroup v2 files of the node container (slice race-<node>.slice)."
    b.ts("Container CPU", f"SAME. race:node_cpu_cores: rate of race_node_cpu_seconds_total (usage_usec of cpu.stat) over 1 minute, in CPU cores. {cg}", [(f"race:node_cpu_cores{{{ZN}}}", "zakurad"), (f"race:node_cpu_cores{{{HN}}}", "hayaid")], "none")
    b.ts("Container memory and peak", f"SAME. race_node_memory_bytes (memory.current) and race_node_memory_peak_bytes (memory.peak). Both include the page cache of the files of the container. {cg}", [
        (f"race_node_memory_bytes{{{ZN}}}", "zakurad"),
        (f"race_node_memory_peak_bytes{{{ZN}}}", "zakurad peak"),
        (f"race_node_memory_bytes{{{HN}}}", "hayaid"),
        (f"race_node_memory_peak_bytes{{{HN}}}", "hayaid peak"),
    ], "bytes")
    b.ts("Container disk I/O", f"SAME. race:node_io_read_bytes_per_second and race:node_io_write_bytes_per_second: rbytes and wbytes of io.stat, devices without a lower device. {cg}", [
        (f"race:node_io_read_bytes_per_second{{{ZN}}}", "zakurad read"),
        (f"race:node_io_write_bytes_per_second{{{ZN}}}", "zakurad written"),
        (f"race:node_io_read_bytes_per_second{{{HN}}}", "hayaid read"),
        (f"race:node_io_write_bytes_per_second{{{HN}}}", "hayaid written"),
    ], "Bps")
    b.ts("Data directory", "CLOSE. race_node_data_bytes: disk blocks of the data volume of the node (du), each 60 s, from the sidecar. The zakurad directory has its log file; hayaid writes its log to Docker. Both have the trace tables.", [(f"race_node_data_bytes{{{ZN}}}", "zakurad"), (f"race_node_data_bytes{{{HN}}}", "hayaid")], "bytes")

    b.row("Resources of the machine (node-exporter of each machine; each machine runs one node)")
    one = "SAME. Same method on both machines: node-exporter. The values are of the machine: the node and the containers of the race."
    b.ts("Machine CPU", f"race:machine_cpu_utilisation: share of the CPU time that is not idle. {one}", [(f"race:machine_cpu_utilisation{{{Z}}} and on (node) zakura_build_info", "zakurad"), (f"race:machine_cpu_utilisation{{{Hn}}} and on (node) hayai_build_info", "hayaid")], "percentunit")
    b.ts("Machine memory in use", f"race:machine_memory_used_bytes: MemTotal minus MemAvailable. {one}", [(f"race:machine_memory_used_bytes{{{Z}}} and on (node) zakura_build_info", "zakurad"), (f"race:machine_memory_used_bytes{{{Hn}}} and on (node) hayai_build_info", "hayaid")], "bytes")
    b.ts("Disk in use", f"race:machine_disk_used_bytes: used bytes of the file system of the node data, with the image of the node. {one}", [(f"race:machine_disk_used_bytes{{{Z}}} and on (node) zakura_build_info", "zakurad"), (f"race:machine_disk_used_bytes{{{Hn}}} and on (node) hayai_build_info", "hayaid")], "bytes")
    b.ts("Disk I/O", f"race:machine_disk_read_bytes_per_second and race:machine_disk_written_bytes_per_second. {one}", [
        (f"race:machine_disk_read_bytes_per_second{{{Z}}} and on (node) zakura_build_info", "zakurad read"),
        (f"race:machine_disk_written_bytes_per_second{{{Z}}} and on (node) zakura_build_info", "zakurad written"),
        (f"race:machine_disk_read_bytes_per_second{{{Hn}}} and on (node) hayai_build_info", "hayaid read"),
        (f"race:machine_disk_written_bytes_per_second{{{Hn}}} and on (node) hayai_build_info", "hayaid written"),
    ], "Bps")
    b.ts("Machine network", f"race:machine_network_receive_bytes_per_second and race:machine_network_transmit_bytes_per_second. {one}", [
        (f"race:machine_network_receive_bytes_per_second{{{Z}}} and on (node) zakura_build_info", "zakurad received"),
        (f"race:machine_network_transmit_bytes_per_second{{{Z}}} and on (node) zakura_build_info", "zakurad sent"),
        (f"race:machine_network_receive_bytes_per_second{{{Hn}}} and on (node) hayai_build_info", "hayaid received"),
        (f"race:machine_network_transmit_bytes_per_second{{{Hn}}} and on (node) hayai_build_info", "hayaid sent"),
    ], "Bps", w=24)

    b.row("Compared quantities")
    b.text("Definition of each compared quantity", DEFINITIONS, h=22)
    b.write("comparison.json")
    return b


hayai_node()
zakura_node()
comparison()
