#!/usr/bin/env python3
"""Render docs/report.html from bench-results/*.json.

Inputs:
- bench-results/summary.json      criterion means per (group, function id, parameter), from
                                  scripts/collect_bench.py
- bench-results/system.json       sysbench rows (wall, CPU, RSS, allocations, counters)
- bench-results/relay-bytes.json  bytes on the wire per fixture, from the relay bench

Page structure:
1. A short introduction.
2. General benchmarks: one table that compares hayai with Zakura and Zebra on operations
   that every node does. Each row states how the baseline was obtained.
3. Features: one card per feature. The closed card shows one bold sentence and at most two
   sentences of impact. The open card explains the problem, the necessary context, the
   solution, related systems (explained for a reader who does not know them), and the
   benchmarks of that feature.
4. Method and status.

Every number on the page comes from the input files. A missing value shows as a dash.
"""

import html
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RESULTS = ROOT / "bench-results"
OUT = ROOT / "docs" / "report.html"


# ---------------------------------------------------------------- data access


def load(name, default):
    path = RESULTS / name
    if not path.exists():
        return default
    with open(path) as f:
        return json.load(f)


class Bench:
    def __init__(self, summary):
        self.rows = {}
        for r in summary.get("rows", []):
            self.rows[(r["group"], r["function"], r["param"])] = r["mean_ns"]
        self.machine = summary.get("machine", {})
        self.rev = summary.get("hayai_rev", "")

    def get(self, group, func, param=""):
        return self.rows.get((group, func, param))

    def first(self, group, funcs, param=""):
        for f in funcs:
            v = self.get(group, f, param)
            if v is not None:
                return v
        return None

    def params(self, group):
        return sorted({p for (g, _, p) in self.rows if g == group}, key=param_key)


class System:
    def __init__(self, data):
        self.by = {}
        for r in data.get("scenarios", []):
            self.by[(r["name"], r["param"], r["impl"])] = r
        self.machine = data.get("machine", {})
        self.note = data.get("note", "")
        self.rows = data.get("scenarios", [])

    def get(self, name, param, impl, key):
        r = self.by.get((name, param, impl))
        if r is None:
            return None
        if key == "cpu_ms":
            return (r.get("cpu_user_ms") or 0) + (r.get("cpu_sys_ms") or 0)
        return r.get(key)


def param_key(p):
    head = p.split("-")[-1].split("x")[0]
    return (0, int(head), p) if head.isdigit() else (1, 0, p)


# ---------------------------------------------------------------- formatting


def esc(s):
    return html.escape(str(s))


def fmt_time(ns):
    if ns is None:
        return "–"
    if ns < 1_000:
        return f"{ns:.0f} ns"
    if ns < 1_000_000:
        return f"{ns / 1_000:.3g} µs"
    if ns < 1_000_000_000:
        return f"{ns / 1_000_000:.3g} ms"
    return f"{ns / 1_000_000_000:.3g} s"


def fmt_ms(ms):
    return "–" if ms is None else fmt_time(ms * 1_000_000)


def fmt_bytes(b):
    if b is None:
        return "–"
    for unit, div in (("GB", 1e9), ("MB", 1e6), ("kB", 1e3)):
        if b >= div:
            return f"{b / div:.3g} {unit}"
    return f"{b:.0f} B"


def fmt_kib(kb):
    return "–" if kb is None else f"{kb / 1024:.0f} MiB"


def fmt_ratio(r):
    if r >= 100:
        return f"{r:,.0f}×"
    if r >= 10:
        return f"{r:.0f}×"
    return f"{r:.1f}×"


# ---------------------------------------------------------------- charts

HAYAI = "var(--s1)"
HAYAI_ALT = "var(--s1b)"
ZAKURA = "var(--s2)"
OTHER = "var(--s3)"


def chart(title, unit_fmt, series, rows, note="", worse="slower"):
    """Horizontal grouped bars on one linear scale.

    series: list of (key, display name, color token), in drawing order.
    rows:   list of (row label, {key: value}). Missing values are skipped.
    """
    rows = [(label, vals) for label, vals in rows if any(vals.get(k) is not None for k, _, _ in series)]
    if not rows:
        return ""
    vmax = max(v for _, vals in rows for v in vals.values() if v is not None) or 1
    row_h, gap, label_w, plot_w, val_w = 22, 14, 200, 500, 140
    width = label_w + plot_w + val_w
    y = 8
    out = []
    for label, vals in rows:
        bars = [(k, name, color, vals[k]) for k, name, color in series if vals.get(k) is not None]
        group_h = len(bars) * row_h
        out.append(f'<text x="{label_w - 10}" y="{y + group_h / 2 + 4}" text-anchor="end" class="lbl">{esc(label)}</text>')
        best = min(v for *_, v in bars)
        for i, (_, name, color, v) in enumerate(bars):
            w = max(2, plot_w * v / vmax)
            yy = y + i * row_h
            out.append(
                f'<rect x="{label_w}" y="{yy + 3}" width="{w:.1f}" height="{row_h - 6}" rx="3" fill="{color}">'
                f"<title>{esc(name)}: {esc(unit_fmt(v))}</title></rect>"
            )
            ratio = v / best if best else 1
            tag = f'  <tspan class="ratio">{fmt_ratio(ratio)} {worse}</tspan>' if ratio >= 1.05 else ""
            out.append(f'<text x="{label_w + w + 6:.1f}" y="{yy + row_h / 2 + 4}" class="val">{esc(unit_fmt(v))}{tag}</text>')
        y += group_h + gap
    legend = "".join(
        f'<span class="key"><i style="background:{color}"></i>{esc(name)}</span>'
        for k, name, color in series
        if any(vals.get(k) is not None for _, vals in rows)
    )
    return (
        f'<figure><figcaption>{esc(title)}</figcaption><div class="legend">{legend}</div>'
        f'<div class="scroll"><svg viewBox="0 0 {width} {y + 4}" width="{width}" height="{y + 4}" role="img" aria-label="{esc(title)}">'
        + "".join(out)
        + "</svg></div>"
        + (f'<p class="note">{esc(note)}</p>' if note else "")
        + "</figure>"
    )


# ---------------------------------------------------------------- latency model for miners and the network
#
# The model adds the measured processing cost of each node to the network cost of each hop.
# Processing costs come from the benchmarks. Network costs come from assumptions the reader
# can change on the page. The page labels the result as a model.

HEADER_BYTES = 1487
SHORT_ID_BYTES = 6


def model_inputs(bench, relay_bytes):
    """Measured inputs of the latency model, per block type. Times in ms, sizes in bytes."""
    def ms(v):
        return None if v is None else v / 1e6

    fwd_2000 = ms(bench.get("relay/forward_latency", "v2_forward_on_ids", "transparent-2000x2"))
    fwd_main = ms(bench.get("relay/forward_latency", "v2_forward_on_ids", "main-1687107"))
    main = next((r for r in relay_bytes if r["fixture"] == "main-1687107"), None)
    return {
        "full": {
            "label": "Full block: 6,500 transparent transactions, 1.56 MB",
            "size": 1_560_000,
            "ntx": 6501,
            "vz": ms(bench.get("validate/block", "zakura-model-cold", "transparent-6500x1")),
            "vh": ms(bench.get("validate/block", "hayai-warm", "transparent-6500x1")),
            # Measured on 2,000 transactions; scaled linearly to 6,500.
            "fwd": None if fwd_2000 is None else fwd_2000 * 6501 / 2001,
        },
        "small": {
            "label": "Typical mainnet block: 6 transactions, 16 kB (block 1,687,107)",
            "size": main["full_bytes"] if main else 15_957,
            "ntx": main["txs"] if main else 6,
            # Validation of a 6-transaction block is under 1 ms for both nodes; the model uses 1 ms.
            "vz": 1.0,
            "vh": 1.0,
            "fwd": fwd_main,
        },
        "template_hayai": ms(bench.get("template/tip_event", "hayai-empty")) or 0.0,
        "template_full_hayai": ms(bench.get("template/tip_event", "hayai-full")),
        "template_full_zakura": ms(bench.get("template/build_from_scratch", "zakura-zip317", "8000")),
    }


def latency(case, block, inp, rtt, hops, bw_mbit):
    """Time from the block's discovery to the pool hashing on it, in ms, split in three parts."""
    full_wire = block["size"] * 8 / (bw_mbit * 1000)
    compact_wire = (HEADER_BYTES + SHORT_ID_BYTES * block["ntx"] + 50) * 8 / (bw_mbit * 1000)
    th = inp["template_hayai"]
    if case == "legacy":
        # Every node forwards after full validation: 1.5 round trips and the full body per hop.
        # Zakura's commit time is not counted, and its empty template is counted as free.
        return {"network": hops * (1.5 * rtt + full_wire), "validation": hops * block["vz"], "template": 0.0}
    if case == "you":
        # Your node runs hayai; every other node is legacy.
        return {"network": hops * (1.5 * rtt + full_wire), "validation": (hops - 1) * block["vz"] + block["vh"], "template": th}
    # Every node runs hayai: half a round trip and a compact block per hop; forwarders only check ids.
    return {"network": hops * (0.5 * rtt + compact_wire), "validation": (hops - 1) * (block["fwd"] or 1.0) + block["vh"], "template": th}


CASES = [
    ("legacy", "Every node runs Zakura", ZAKURA),
    ("you", "Your node runs hayai, the other nodes run Zakura", HAYAI_ALT),
    ("hayai", "Every node runs hayai", HAYAI),
]


def latency_rows(inp, block_key, rtt, hops, bw, spacing):
    block = inp[block_key]
    out = []
    for key, label, color in CASES:
        parts = latency(key, block, inp, rtt, hops, bw)
        total = sum(parts.values())
        out.append((key, label, color, parts, total, total / (spacing * 1000) * 100))
    return out


def bars_html(rows):
    vmax = max(r[4] for r in rows) or 1
    html_rows = []
    for key, label, color, parts, total, pct in rows:
        segs = "".join(
            f'<span class="seg {name}" style="width:{parts[name] / vmax * 100:.2f}%" title="{name}: {parts[name]:.1f} ms"></span>'
            for name in ("network", "validation", "template")
        )
        html_rows.append(
            f'<div class="lrow" data-case="{key}"><div class="llabel">{esc(label)}</div>'
            f'<div class="ltrack">{segs}</div>'
            f'<div class="lval"><b>{total:,.0f} ms</b><span class="sub"> · {pct:.2f} % of hash power</span></div></div>'
        )
    return "".join(html_rows)


def miners_section(bench, relay_bytes):
    inp = model_inputs(bench, relay_bytes)
    if inp["full"]["vz"] is None or inp["full"]["vh"] is None:
        return ""
    rtt, hops, bw, spacing = 100, 3, 100, 25
    rows = latency_rows(inp, "full", rtt, hops, bw, spacing)
    legacy, you, all_h = rows[0], rows[1], rows[2]
    tf_h, tf_z = inp["template_full_hayai"], inp["template_full_zakura"]
    data = json.dumps(inp)
    return f"""
<section id="miners">
<h2>For miners: less hash power on an outdated block</h2>
<p>When a competitor finds a block, your pool keeps hashing on the old block until your node has received the new block, validated it, and sent a new template. A block that your pool finds in that time competes with a block the network already has, and it usually loses. The time is the sum of three parts: the network path, the validation at each node, and the template. At 25 s per block, each 250 ms of that time is 1 % of your hash power.</p>
<div class="calc" id="calc">
<div class="controls">
<label>Block <select id="c-block"><option value="full">{esc(inp['full']['label'])}</option><option value="small">{esc(inp['small']['label'])}</option></select></label>
<label>Round trip between nodes <input id="c-rtt" type="number" min="1" max="1000" value="{rtt}"> ms</label>
<label>Hops from the finder to your node <input id="c-hops" type="number" min="1" max="10" value="{hops}"></label>
<label>Bandwidth <input id="c-bw" type="number" min="1" max="10000" value="{bw}"> Mbit/s</label>
<label>Block spacing <input id="c-spacing" type="number" min="1" max="600" value="{spacing}"> s</label>
</div>
<div class="lkey"><span><i class="seg network"></i>network</span><span><i class="seg validation"></i>validation</span><span><i class="seg template"></i>template</span></div>
<div id="c-rows">{bars_html(rows)}</div>
<p class="note">Model. Measured inputs: validation of the block at each node (hayai with prepared transactions; Zakura's scheduling model), the forward check of hayai, and the template time. Assumed inputs: the values above. The transactions are already in the memory pools. Zakura's commit time is not counted, its empty template is counted as free, and its validation time is the lower-bound model of its scheduling; all three choices favour Zakura. With these inputs the network time is the largest part for both nodes, so most of the gain comes from the relay and not from validation speed.</p>
</div>
<div class="facts">
<div class="fact"><div class="big">{legacy[5] - you[5]:.1f} %</div><div>of your hash power stops working on outdated blocks when only your node switches to hayai (full block, values above).</div></div>
<div class="fact"><div class="big">{legacy[5]:.1f} % → {all_h[5]:.2f} %</div><div>hash power on outdated blocks when the network switches.</div></div>
<div class="fact"><div class="big">{esc(fmt_ms(tf_h))}</div><div>to a full template with fees after a new block. Zakura rebuilds the selection on each request: {esc(fmt_ms(tf_z))} for 8,000 candidates.</div></div>
</div>
<p>The same model applies to a block that your pool finds. The faster your block reaches the other miners, the less often a competing block wins.</p>
<script type="application/json" id="c-data">{data}</script>
<script>
(function () {{
  var inp = JSON.parse(document.getElementById('c-data').textContent);
  var names = {{legacy: 'Every node runs Zakura', you: 'Your node runs hayai, the other nodes run Zakura', hayai: 'Every node runs hayai'}};
  function latency(c, b, rtt, hops, bw) {{
    var full = b.size * 8 / (bw * 1000);
    var compact = (1487 + 6 * b.ntx + 50) * 8 / (bw * 1000);
    var th = inp.template_hayai;
    if (c === 'legacy') return {{network: hops * (1.5 * rtt + full), validation: hops * b.vz, template: 0}};
    if (c === 'you') return {{network: hops * (1.5 * rtt + full), validation: (hops - 1) * b.vz + b.vh, template: th}};
    return {{network: hops * (0.5 * rtt + compact), validation: (hops - 1) * (b.fwd || 1) + b.vh, template: th}};
  }}
  function num(id, d) {{ var v = parseFloat(document.getElementById(id).value); return isFinite(v) && v > 0 ? v : d; }}
  function render() {{
    var b = inp[document.getElementById('c-block').value];
    var rtt = num('c-rtt', 100), hops = Math.round(num('c-hops', 3)), bw = num('c-bw', 100), sp = num('c-spacing', 25);
    var rows = ['legacy', 'you', 'hayai'].map(function (c) {{
      var p = latency(c, b, rtt, hops, bw); var t = p.network + p.validation + p.template;
      return {{c: c, p: p, t: t, pct: t / (sp * 1000) * 100}};
    }});
    var vmax = Math.max.apply(null, rows.map(function (r) {{ return r.t; }})) || 1;
    var out = rows.map(function (r) {{
      var segs = ['network', 'validation', 'template'].map(function (n) {{
        return '<span class="seg ' + n + '" style="width:' + (r.p[n] / vmax * 100).toFixed(2) + '%" title="' + n + ': ' + r.p[n].toFixed(1) + ' ms"></span>';
      }}).join('');
      return '<div class="lrow"><div class="llabel">' + names[r.c] + '</div><div class="ltrack">' + segs + '</div><div class="lval"><b>' +
        Math.round(r.t).toLocaleString('en-US') + ' ms</b><span class="sub"> · ' + r.pct.toFixed(2) + ' % of hash power</span></div></div>';
    }}).join('');
    document.getElementById('c-rows').innerHTML = out;
  }}
  ['c-block', 'c-rtt', 'c-hops', 'c-bw', 'c-spacing'].forEach(function (id) {{
    document.getElementById(id).addEventListener('input', render);
  }});
}})();
</script>
</section>"""


def network_section(bench, system, relay_bytes):
    inp = model_inputs(bench, relay_bytes)
    if inp["full"]["vz"] is None:
        return ""
    rtt, hops, bw = 100, 3, 100

    def spacing_for(case, key, stale_pct):
        t = sum(latency(case, inp[key], inp, rtt, hops, bw).values())
        return t / (stale_pct / 100) / 1000

    vz = inp["full"]["vz"]
    vc = bench.get("validate/block", "hayai-cold", "transparent-6500x1")
    vc = None if vc is None else vc / 1e6
    cpu_h = system.get("validate_block_cold", "transparent-6500x1", "hayai", "cpu_ms")
    cpu_z = system.get("validate_block_cold", "transparent-6500x1", "zakura", "cpu_ms")
    rows = [
        ("Blocks that lose because they arrive late (stale blocks), at 25 s spacing, full blocks", f"{sum(latency('legacy', inp['full'], inp, rtt, hops, bw).values()) / 250:.1f} %",
         f"{sum(latency('hayai', inp['full'], inp, rtt, hops, bw).values()) / 250:.2f} %"),
        ("Shortest block spacing that keeps stale blocks at 1 %, full blocks", f"{spacing_for('legacy', 'full', 1):.0f} s", f"{spacing_for('hayai', 'full', 1):.0f} s"),
        ("Shortest block spacing that keeps stale blocks at 1 %, typical blocks", f"{spacing_for('legacy', 'small', 1):.0f} s", f"{spacing_for('hayai', 'small', 1):.0f} s"),
        ("Full blocks a node can validate per second, transactions not seen before", f"{1000 / vz:.0f}", f"{1000 / vc:.0f}" if vc else "–"),
        ("CPU time per full block, transactions not seen before", fmt_ms(cpu_z), fmt_ms(cpu_h)),
    ]
    trs = "".join(f"<tr><td>{esc(a)}</td><td class='num'>{esc(z)}</td><td class='num h'>{esc(h)}</td></tr>" for a, z, h in rows)
    return f"""
<section id="network">
<h2>For the Zcash network: room to scale</h2>
<p>A stale block is a valid block that loses, because another miner found a block before this one arrived. A block that needs a long time to reach the miners makes more stale blocks. Stale blocks waste hash power, favour the largest miners, and limit how short the block spacing and how large the blocks can be. ZIP 218 brings the spacing to 25 s with NU7. The relay time of a full block then decides how much of the network's work is lost.</p>
<div class="scroll"><table><thead><tr><th>Measure</th><th>Zakura network</th><th>hayai network</th></tr></thead><tbody>{trs}</tbody></table></div>
<p class="note">The first three rows use the latency model of the miners section with a 100 ms round trip, 3 hops and 100 Mbit/s. The stale rate is the relay time divided by the block spacing. The last two rows are measurements.</p>
<p>A node that validates and relays faster also syncs faster after a restart, and the same hardware can follow a chain with more transactions.</p>
</section>"""


def headline(bench, relay_bytes):
    """The opening of the page: one sentence, then the new features with one figure each."""
    T = "transparent-6500x1"
    warm = bench.get("validate/block", "hayai-warm", T)
    cold = bench.get("validate/block", "hayai-cold", T)
    zak = bench.get("validate/block", "zakura-model-cold", T)
    t_h = bench.get("template/incremental_add", "hayai", "")
    t_z = bench.get("template/incremental_add", "zakura-zip317", "")
    c_h = bench.get("coins/lookup_block_inputs", "hayai", "13000")
    c_z = bench.get("coins/lookup_block_inputs", "zakura-node-3rounds", "13000")
    sp = bench.get("template/switch_after_block", "hayai-speculative", "orchard-165x2-cold")
    se = bench.get("template/switch_after_block", "hayai-serial", "orchard-165x2-cold")
    orch = next((r for r in relay_bytes if r["fixture"].startswith("orchard")), None)

    def times(a, b):
        r = a / b
        return f"{r:,.0f}×" if r >= 10 else f"{r:.1f}×"

    full, ref = orch["full_bytes"], orch["batch_ref_bytes"]
    items = [
        ("relay", "New protocol", "Block relay by reference",
         "A new block travels as a header and references to transactions that the peer already holds. The peer rebuilds the block and forwards it at once.",
         times(full, ref), f"fewer bytes: {fmt_bytes(ref)}, not {fmt_bytes(full)}"),
        ("lane", "New protocol", "Transaction lanes",
         "A miner chooses the transactions of its next block, then searches for the proof of work. hayai shares that choice with peers during the search. When the proof of work is found, the block goes out as one short reference.",
         "61 B", "to name a block that equals its published candidate"),
        ("verify-once", "Performance", "Each transaction is verified one time",
         "The node checks a transaction when it arrives and keeps the result. A new block needs only the checks that depend on the chain.",
         times(zak, warm), f"faster on a new full block: {fmt_time(warm)}, not {fmt_time(zak)}"),
        ("template", "Mining", "A mining template that is always ready",
         "Each new transaction updates the template in place, and the node pushes the change to the pool server.",
         times(t_z, t_h), f"faster update: {fmt_time(t_h)}, not {fmt_time(t_z)}"),
        ("speculative", "Mining", "Work on a new block before its validation ends",
         "The pool gets a template on the new block as soon as the node has built it in memory. The node takes it back if a check fails.",
         times(se, sp), f"sooner on a shielded block of unseen transactions: {fmt_time(sp)}, not {fmt_time(se)} without this feature"),
        ("state", "Performance", "Chain state in memory",
         "The coin set and the newest 1,000 blocks stay in memory. A block reads all its coins in one batch.",
         times(c_z, c_h), f"faster coin lookup: {fmt_time(c_h)}, not {fmt_time(c_z)}"),
    ]
    cells = "".join(
        f"<a class='nf' href='#{fid}'><span class='eyebrow'>{esc(kind)}</span><span class='nft'>{esc(title)}</span>"
        f"<span class='nfd'>{esc(text)}</span><span class='nfv'><b>{esc(big)}</b> {esc(detail)}</span></a>"
        for fid, kind, title, text, big, detail in items
    )
    return f"""<p class="lead">hayai is a new Zcash node for miners, written apart from Zakura and Zebra. It brings a new block relay protocol, transactions that are verified one time, and a mining template that is always ready. It validates a new full block <b>{times(zak, warm)} faster</b> than Zakura and stays compatible with every Zcash node.</p>
<h2 class="new">What is new</h2>
<div class="nfs">{cells}</div>
<p class="note">The figures compare hayai with Zakura code on the same machine. The figure of the speculative tip compares hayai with and without that feature. A miner node receives almost every transaction before the block that contains it, so the validation figure is the normal case; with transactions that the node never saw, hayai is {times(zak, cold)} faster ({fmt_time(cold)}, not {fmt_time(zak)}). The Zakura validation time is a model of its scheduling with the same cryptography. Open a feature for its method and its measurements.</p>
<nav class="toc" aria-label="Contents"><span class="eyebrow">Contents</span><ol>
<li><a href="#miners">For miners</a></li>
<li><a href="#network">For the Zcash network</a></li>
<li><a href="#general">General benchmarks</a></li>
<li><a href="#features">Features in detail</a></li>
<li><a href="#compatibility">Compatibility</a></li>
<li><a href="#safety">Hardening</a></li>
<li><a href="#method">Method</a></li>
<li><a href="#status">Status</a></li>
</ol></nav>"""


def audience_cards(bench, system, relay_bytes):
    """The two cards at the top of the page, each with direct Zakura and hayai figures."""
    inp = model_inputs(bench, relay_bytes)
    rtt, hops, bw, spacing = 100, 3, 100, 25

    def pct(case, key):
        return sum(latency(case, inp[key], inp, rtt, hops, bw).values()) / (spacing * 1000) * 100

    def spacing_1pct(case, key):
        return sum(latency(case, inp[key], inp, rtt, hops, bw).values()) / 0.01 / 1000

    vz, vc = inp["full"]["vz"], bench.get("validate/block", "hayai-cold", "transparent-6500x1")
    tf_h, tf_z = inp["template_full_hayai"], inp["template_full_zakura"]

    def rows(items):
        body = "".join(
            f"<tr><td>{esc(what)}</td><td class='num'>{esc(z)}</td><td class='num h'>{esc(h)}</td></tr>"
            for what, z, h in items
        )
        return f"<table class='cmp'><thead><tr><th></th><th>Zakura</th><th>hayai</th></tr></thead><tbody>{body}</tbody></table>"

    miners = rows([
        ("Hash power on outdated blocks, every node switches", f"{pct('legacy', 'full'):.1f} %", f"{pct('hayai', 'full'):.2f} %"),
        ("Same, only your node switches", f"{pct('legacy', 'full'):.1f} %", f"{pct('you', 'full'):.1f} %"),
        ("Full template with fees after a new block", fmt_ms(tf_z), fmt_ms(tf_h)),
        ("New fee-paying transaction reaches the pool", "up to 5 s", "a few ms"),
    ])
    network = rows([
        ("Blocks that lose because they arrive late (stale blocks), at 25 s spacing, full blocks", f"{pct('legacy', 'full'):.1f} %", f"{pct('hayai', 'full'):.2f} %"),
        ("Shortest block spacing that keeps stale blocks at 1 %", f"{spacing_1pct('legacy', 'full'):.0f} s", f"{spacing_1pct('hayai', 'full'):.0f} s"),
        ("Full blocks validated per second", f"{1000 / vz:.0f}", f"{1e9 / vc:.0f}" if vc else "–"),
        ("Bytes to send a 2 MB shielded block", fmt_bytes(next((r["full_bytes"] for r in relay_bytes if r["fixture"].startswith("orchard")), None)),
         fmt_bytes(next((r["batch_ref_bytes"] for r in relay_bytes if r["fixture"].startswith("orchard")), None))),
    ])
    return f"""<div class="aud">
<a class="audc" href="#miners"><span class="eyebrow">For miners</span><span class="ah">Less hash power on outdated blocks, and fees from the first seconds of a new block.</span>{miners}</a>
<a class="audc" href="#network"><span class="eyebrow">For the Zcash network</span><span class="ah">Fewer stale blocks at 25 s spacing, and room for larger blocks.</span>{network}</a>
</div>
<p class="note">Full block: 6,500 transactions. Network values: 100 ms round trip, 3 hops, 100 Mbit/s; the sections below explain the model and let you change these values.</p>"""


# ---------------------------------------------------------------- general benchmarks

WORDS = {
    "time": ("faster", "slower"),
    "bytes": ("smaller", "larger"),
    "amount": ("less", "more"),
}

BASELINE_KIND = {
    "code": "Zakura's published crates, same process",
    "layout": "Zakura's data layout on the same storage engine",
    "model": "Zakura's scheduling rebuilt around the same cryptography",
    "port": "Zakura's algorithm ported line by line",
    "protocol": "Fixed by the legacy protocol",
}


def general_table(bench, system, relay_bytes):
    def relay(fixture_prefix, key):
        for r in relay_bytes:
            if r["fixture"].startswith(fixture_prefix):
                return r[key]
        return None

    T6500 = "transparent-6500x1"
    rows = [
        dict(
            what="Validate a new 2 MB block whose transactions are already in the mempool",
            detail="6,500 transparent transactions. This is the normal case for a miner at the tip.",
            fmt=fmt_time,
            hayai=bench.get("validate/block", "hayai-warm", T6500),
            zakura=bench.get("validate/block", "zakura-model-cold", T6500),
            zebra=bench.first("validate/block", ["zebra-model-cold", "zebra-model"], T6500),
            kind="model",
            foot="Zakura and Zebra keep no result of transparent checks from their memory pools, so their path for known transparent transactions is their cold path. The baseline is a model with the same cryptography and no runtime overhead, so the real nodes take longer.",
        ),
        dict(
            what="Validate the same block when the node saw none of its transactions before",
            detail="Every signature, every coin lookup, every rule. Both nodes use all cores; hayai needs more peak memory.",
            fmt=fmt_time,
            hayai=bench.get("validate/block", "hayai-cold", T6500),
            zakura=bench.get("validate/block", "zakura-model-cold", T6500),
            zebra=bench.first("validate/block", ["zebra-model-cold", "zebra-model"], T6500),
            kind="model",
        ),
        dict(
            what="Send a 2 MB shielded block to one peer",
            detail="Bytes on the wire for one announcement.",
            fmt=fmt_bytes,
            words="bytes",
            hayai=relay("orchard", "batch_ref_bytes") or relay("orchard", "short_id_bytes"),
            zakura=relay("orchard", "full_bytes"),
            zebra=relay("orchard", "full_bytes"),
            kind="protocol",
            foot="Legacy relay sends the full block. hayai sends the header and references when the peer runs hayai; legacy peers get the full block.",
        ),
        dict(
            what="Parse a 2 MB block and compute its transaction ids",
            detail="6,500 transparent transactions.",
            fmt=fmt_time,
            hayai=bench.get("wire/parse_block", "hayai", T6500),
            zakura=bench.get("wire/parse_block", "zakura", T6500),
            zebra=bench.get("wire/parse_block", "zebra", T6500),
            kind="code",
        ),
        dict(
            what="Read the spent coins of a block with 13,000 inputs",
            detail="2 million coins on disk.",
            fmt=fmt_time,
            hayai=bench.get("coins/lookup_block_inputs", "hayai", "13000"),
            zakura=bench.get("coins/lookup_block_inputs", "zakura-node-3rounds", "13000"),
            zebra=bench.get("coins/lookup_block_inputs", "zebra", "13000"),
            kind="layout",
        ),
        dict(
            what="Add a block to the in-memory chain state",
            detail="1,000 recent blocks in memory.",
            fmt=fmt_time,
            hayai=bench.get("state/push_block", "hayai-layer", "1000"),
            zakura=bench.get("state/push_block", "zakura-clone", "1000"),
            zebra=bench.get("state/push_block", "zebra-clone", "1000"),
            kind="model",
            foot="The Zebra value is a model of the maps that Zebra copies for each block. The size of its address index is an estimate.",
        ),
        dict(
            what="Update the block template after one new transaction",
            detail="8,000 candidate transactions.",
            fmt=fmt_time,
            hayai=bench.get("template/incremental_add", "hayai"),
            zakura=bench.get("template/incremental_add", "zakura-zip317"),
            zebra=bench.get("template/incremental_add", "zebra-zip317"),
            kind="port",
        ),
        dict(
            what="Serve a stored 2 MB block to a peer",
            detail="Read from storage and encode for the wire.",
            fmt=fmt_time,
            hayai=bench.get("blockstore/get_block", "hayai", T6500),
            zakura=bench.get("blockstore/get_block", "zakura", T6500),
            zebra=bench.get("blockstore/get_block", "zebra", T6500),
            kind="layout",
        ),
        dict(
            what="CPU time to validate the block of unseen transactions",
            detail="All threads, one block.",
            fmt=fmt_ms,
            words="amount",
            hayai=system.get("validate_block_cold", T6500, "hayai", "cpu_ms"),
            zakura=system.get("validate_block_cold", T6500, "zakura", "cpu_ms"),
            zebra=system.get("validate_block_cold", T6500, "zebra", "cpu_ms"),
            kind="model",
        ),
        dict(
            what="Peak memory of the same run",
            detail="Process high-water mark, fixtures included.",
            fmt=fmt_kib,
            words="amount",
            hayai=system.get("validate_block_cold", T6500, "hayai", "max_rss_kb"),
            zakura=system.get("validate_block_cold", T6500, "zakura", "max_rss_kb"),
            zebra=system.get("validate_block_cold", T6500, "zebra", "max_rss_kb"),
            kind="model",
            foot="hayai holds the prepared form of every transaction of the block; this costs memory and saves the work when the block is seen again.",
        ),
    ]
    trs = []
    feet = []
    for r in rows:
        h, z = r["hayai"], r["zakura"]
        if h is not None and z is not None and h > 0:
            ratio = z / h
            good, bad = WORDS[r.get("words", "time")]
            if ratio >= 1:
                diff = f'<span class="better">{fmt_ratio(ratio)} {good}</span>'
            else:
                diff = f'<span class="worse">{fmt_ratio(1 / ratio)} {bad}</span>'
        else:
            diff = "–"
        mark = ""
        if r.get("foot"):
            feet.append(r["foot"])
            mark = f'<sup>{len(feet)}</sup>'
        trs.append(
            "<tr>"
            f'<td><span class="what">{esc(r["what"])}{mark}</span><br><span class="sub">{esc(r["detail"])}</span></td>'
            f'<td class="num h">{esc(r["fmt"](h))}</td>'
            f'<td class="num">{esc(r["fmt"](z))}</td>'
            f'<td class="num">{esc(r["fmt"](r["zebra"]))}</td>'
            f'<td class="num">{diff}</td>'
            f'<td class="sub">{esc(BASELINE_KIND[r["kind"]])}</td>'
            "</tr>"
        )
    zebra_measured = any(r["zebra"] is not None for r in rows if r["kind"] != "protocol")
    zebra_note = (
        "" if zebra_measured
        else "<li>Zebra baselines other than protocol-defined values are not measured yet. The live comparison on Testnet adds them.</li>"
    )
    foot_html = "".join(f"<li><sup>{i + 1}</sup> {esc(t)}</li>" for i, t in enumerate(feet))
    return f"""
<section id="general">
<h2>General benchmarks</h2>
<p>These operations exist in every Zcash node. Lower is better in every row. The difference column compares hayai with Zakura. Open a feature below to see the measurements of that feature.</p>
<div class="scroll"><table class="general">
<thead><tr><th>Operation</th><th>hayai</th><th>Zakura</th><th>Zebra</th><th>Difference</th><th>How the baseline is obtained</th></tr></thead>
<tbody>{''.join(trs)}</tbody></table></div>
<ul class="foot">{foot_html}{zebra_note}</ul>
{system_details(system)}
</section>"""


def miss_rate(r):
    refs = r.get("cache_refs")
    if not refs:
        return "–"
    return f"{r['cache_misses'] / refs * 100:.0f} %"


def system_details(system):
    if not system.rows:
        return ""
    by = {}
    for r in system.rows:
        by.setdefault((r["name"], r["param"]), {})[r["impl"]] = r

    def cell(r, f):
        return "–" if r is None else esc(f(r))

    trs = []
    for (name, param), impls in by.items():
        h, z = impls.get("hayai"), impls.get("zakura")
        trs.append(
            "<tr>"
            f"<td>{esc(name)}<br><span class='sub'>{esc(param)}</span></td>"
            f"<td class='num'>{cell(h, lambda r: fmt_ms(r['wall_ms_median']))}<br><span class='sub'>{cell(z, lambda r: fmt_ms(r['wall_ms_median']))}</span></td>"
            f"<td class='num'>{cell(h, lambda r: fmt_ms((r.get('cpu_user_ms') or 0) + (r.get('cpu_sys_ms') or 0)))}<br><span class='sub'>{cell(z, lambda r: fmt_ms((r.get('cpu_user_ms') or 0) + (r.get('cpu_sys_ms') or 0)))}</span></td>"
            f"<td class='num'>{cell(h, lambda r: fmt_kib(r['max_rss_kb']))}<br><span class='sub'>{cell(z, lambda r: fmt_kib(r['max_rss_kb']))}</span></td>"
            f"<td class='num'>{cell(h, lambda r: fmt_bytes(r['alloc_bytes']))}<br><span class='sub'>{cell(z, lambda r: fmt_bytes(r['alloc_bytes']))}</span></td>"
            f"<td class='num'>{cell(h, miss_rate)}<br><span class='sub'>{cell(z, miss_rate)}</span></td>"
            "</tr>"
        )
    m = system.machine
    return f"""
<details class="sys">
<summary>All system measurements: CPU time, memory, cache misses</summary>
<p>Each scenario runs in a fresh process. In each cell, the first line is hayai and the second line is the Zakura baseline. Hardware counters come from <code>perf_event_open</code> in user space.</p>
<div class="scroll"><table>
<thead><tr><th>Scenario</th><th>Wall</th><th>CPU time</th><th>Peak memory</th><th>Allocated</th><th>Cache misses</th></tr></thead>
<tbody>{''.join(trs)}</tbody></table></div>
<p class="note">Machine: {esc(m.get('cpu', ''))}, {esc(m.get('threads', ''))} threads. Peak memory includes the benchmark fixtures. Both implementations share one allocator in a benchmark process. {esc(system.note)}</p>
</details>"""


# ---------------------------------------------------------------- features


def card(fid, title, summary, problem, context, solution, related, compat, benches):
    sol = "".join(f"<li>{s}</li>" for s in solution)
    rel = (
        "<h4>Related systems</h4><dl>" + "".join(f"<dt>{esc(n)}</dt><dd>{esc(d)}</dd>" for n, d in related) + "</dl>"
        if related
        else ""
    )
    ben = f"<h4>Measurements</h4>{''.join(benches)}" if any(benches) else ""
    return f"""
<details class="feature" id="{fid}">
<summary><span class="title">{esc(title)}</span><span class="summary">{summary}</span></summary>
<div class="body">
<h4>Problem</h4><p>{problem}</p>
<h4>Context</h4><p>{context}</p>
<h4>Solution</h4><ul>{sol}</ul>
{rel}
<h4>Who must run hayai</h4><p>{esc(compat)}</p>
{ben}
</div>
</details>"""


def features(bench, relay_bytes):
    cards = []

    # 1. Relay.
    by_fix = {r["fixture"]: r for r in relay_bytes if not r["fixture"].startswith("main")}
    orch = next((r for k, r in by_fix.items() if k.startswith("orchard")), None)
    ratio = f"{orch['full_bytes'] / orch['batch_ref_bytes']:,.0f}" if orch else "–"
    fwd_v1 = bench.get("relay/forward_latency", "v1_reconstruct_first", "orchard-200x2")
    fwd_v2 = bench.get("relay/forward_latency", "v2_forward_on_ids", "orchard-200x2")
    s3 = [("full", "full block (legacy relay)", ZAKURA), ("short", "hayai, short ids", HAYAI), ("batch", "hayai, one batch reference", HAYAI_ALT)]
    bytes_chart = chart(
        "Bytes to announce one block", fmt_bytes, s3,
        [(f'{r["fixture"]} ({r["txs"]} txs)', {"full": r["full_bytes"], "short": r["short_id_bytes"], "batch": r["batch_ref_bytes"]}) for r in by_fix.values()],
        worse="more",
    )
    rec_chart = chart(
        "Time to have the full block in memory, all transactions already held", fmt_time,
        [("parse", "parse the full block (zakura-chain)", ZAKURA), ("short", "hayai, short ids", HAYAI), ("batch", "hayai, batch reference", HAYAI_ALT)],
        [(p, {"parse": bench.get("relay/full_block_parse", "zakura_chain_with_ids", p),
              "short": bench.get("relay/reconstruct", "short_ids_store10k", p),
              "batch": bench.get("relay/reconstruct", "batch_ref", p)})
         for p in bench.params("relay/reconstruct") if not p.startswith("main") and "synthetic" not in p],
        note="The legacy column excludes the round trip and the transfer of the full block, which come before the parse.",
    )
    fwd_chart = chart(
        "Time until the next hop receives the announcement, one transaction missing, 20 ms round trip", fmt_time,
        [("v1", "forward after the missing bytes arrive", ZAKURA), ("v2", "forward when the id list matches the header", HAYAI)],
        [(p, {"v1": bench.get("relay/forward_latency", "v1_reconstruct_first", p), "v2": bench.get("relay/forward_latency", "v2_forward_on_ids", p)})
         for p in bench.params("relay/forward_latency") if "synthetic" not in p],
    )
    cards.append(card(
        "relay",
        "A new block crosses each network hop as about 2 kB of references, not up to 2 MB of data.",
        f"Peers rebuild the block from transactions they already hold, and forward it before they have all its bytes. A 2 MB shielded block becomes {ratio} times smaller on the wire, and a hop with one missing transaction forwards in {esc(fmt_time(fwd_v2))} instead of {esc(fmt_time(fwd_v1))}.",
        "Zakura and Zebra forward a block only after they validate and store it. They send a short notice to one third of their peers. Each peer asks for the block and then receives all of it. One hop costs one and a half network round trips, the transfer of up to 2 MB, and one full validation.",
        "From NU7, Zcash makes a block every 25 s. When a block needs <i>d</i> seconds to reach the other miners, the probability that a competing block appears in that time is about <i>d</i>&nbsp;/&nbsp;25. One second of delay costs the miner about 4&nbsp;% of its blocks. Most transactions of a block are already in each peer's memory pool, because peers exchange transactions before a miner includes them.",
        [
            "The sender transmits the block header and a 6-byte fingerprint for each transaction. The receiver finds the matching transactions in its own memory pool.",
            "Miners can publish the transactions of their next block in advance, as batches. A block then names a batch with one 32-byte identifier.",
            "A receiver checks the proof of work in the header and checks the list of transaction identifiers against the header. Then it forwards the block at once and requests the missing transactions in parallel.",
            "Every hayai node keeps the normal Zcash protocol on the same connection. The new messages start only when both peers agree on a protocol version. The specification is the draft ZIP in <code>zip/</code>.",
        ],
        [
            ("Bitcoin compact blocks (BIP 152, 2016)", "Bitcoin nodes send a block as its header plus short fingerprints of its transactions. The receiver rebuilds the block from its own memory pool and asks only for what it lacks."),
            ("Narwhal and Autobahn (research systems, 2022 and 2024)", "In these designs, nodes send batches of transactions to each other all the time, and the agreement on block order uses only the identifiers of the batches. hayai uses the same split: batches travel ahead of the block, and the block names them."),
        ],
        "Peers that also run hayai, or that implement the draft ZIP. Legacy peers receive every block by the normal protocol, after hayai validates it.",
        [bytes_chart, rec_chart, fwd_chart],
    ))

    # 2. Verify once.
    warm = bench.get("validate/block", "hayai-warm", "transparent-6500x1")
    zmodel = bench.get("validate/block", "zakura-model-cold", "transparent-6500x1")
    cold = bench.get("validate/block", "hayai-cold", "transparent-6500x1")
    rows = [(p, {"zakura": bench.get("validate/block", "zakura-model-cold", p),
                 "cold": bench.get("validate/block", "hayai-cold", p),
                 "warm": bench.get("validate/block", "hayai-warm", p)}) for p in bench.params("validate/block")]
    warm_chart = chart(
        "Validate one block", fmt_time,
        [("zakura", "Zakura scheduling model", ZAKURA), ("cold", "hayai, transactions not seen before", HAYAI_ALT), ("warm", "hayai, transactions already prepared", HAYAI)],
        rows, note="The Zakura model exists for the transparent blocks only.",
    )
    win_chart = chart(
        "Validate one block with 100 recent blocks in memory", fmt_time,
        [("cold", "hayai, transactions not seen before", HAYAI_ALT), ("warm", "hayai, transactions already prepared", HAYAI)],
        [(p, {"cold": bench.get("validate/block_windowed", "hayai-cold", p), "warm": bench.get("validate/block_windowed", "hayai-warm", p)}) for p in bench.params("validate/block_windowed")],
    )
    cards.append(card(
        "verify-once",
        "A transaction is verified once, when it arrives, and never again.",
        f"When a block contains transactions that the node already knows, only the checks that depend on the chain run. A known 2 MB block takes {esc(fmt_time(warm))}. The Zakura scheduling model, with the same cryptography and no runtime overhead, takes {esc(fmt_time(zmodel))} for the same block, because it has no result of the transparent checks. For a block of transactions that the node has not seen, hayai is {esc(fmt_ratio(zmodel / cold) if zmodel and cold else '–')} faster and uses more peak memory.",
        "Zakura keeps only the proof results of shielded transactions from its memory pool. When the same transactions arrive in a block, it parses each transaction again, computes its identifiers and signature hashes again, reads every spent coin again, and runs every transparent script again. Zebra keeps none of these results.",
        "A transaction has two kinds of checks. Some checks depend only on the transaction and the coins it spends: signatures, proofs, fees. Other checks depend on the chain at that block: the coins must be unspent, the nullifiers must be new, the time limits must hold. A miner at the tip has seen almost every transaction of a new block before the block arrives.",
        [
            "The node keeps a prepared transaction for each transaction in its memory pool: the parsed form, the identifiers, the spent coins, and the result of every check that does not depend on the chain.",
            "Block validation finds each transaction by its full identifier: the transaction id plus the hash of its signatures and proofs. A transaction with a changed signature or proof never matches.",
            "For a known transaction, only the checks that depend on the chain run.",
        ],
        [("Bitcoin Core script cache", "Bitcoin Core stores the result of each script check that it does for its memory pool. When a block contains the same transaction, it does not run the script again. hayai applies the same idea to every check that does not depend on the chain.")],
        "Nobody else. The memory pool fills from normal transaction exchange with any peer.",
        [warm_chart, win_chart],
    ))

    # 3. Bulk validation of unseen transactions.
    bis_chart = chart(
        "One invalid proof in a batch of 64 Orchard bundles", fmt_time,
        [("zakura", "check each item alone after the batch fails (Zakura)", ZAKURA), ("hayai", "split the batch in halves (hayai)", HAYAI)],
        [("64 bundles, 1 invalid", {"zakura": bench.get("prepared/bisection", "zakura-fallback", "64x2-1bad"), "hayai": bench.get("prepared/bisection", "hayai", "64x2-1bad")}),
         ("64 bundles, all valid", {"hayai": bench.get("prepared/bisection", "hayai", "64x2-valid")})],
    )
    cards.append(card(
        "bulk",
        "Transactions that the node has not seen are checked in bulk and in parallel.",
        "A block of unknown transactions costs one read of the state, one parallel pass over all scripts, and one batch of proofs. One invalid proof does not force the node to check every proof alone.",
        "Zakura drives all transactions of a block from one task. It reads each input with a separate request, starts one task for each script, and decodes Sapling proofs one after another. When a batch of proofs fails, it checks every proof of the batch alone.",
        "Proof systems such as Groth16 and Halo 2 can check many proofs together for close to the cost of one. A failed batch tells only that at least one proof is bad, not which one.",
        [
            "The node collects every input of the block and reads all of them in one batch.",
            "It runs all script checks of the block as one flat parallel array.",
            "It checks all proofs of the block in one batch. When the batch fails, it splits the batch in halves and checks each half until it finds the bad proofs.",
            "Scripts, proofs and the chain checks run at the same time.",
        ],
        [],
        "Nobody else.",
        [bis_chart],
    ))

    # 4. State.
    look = chart(
        "Read the spent coins of one block, 2 million coins on disk", fmt_time,
        [("z3", "Zakura layout, three reads per coin as the node does", ZAKURA), ("z1", "Zakura layout, one read per coin", OTHER), ("h", "hayai, empty cache", HAYAI_ALT), ("hw", "hayai, warm cache", HAYAI)],
        [(f"{p} inputs", {"z3": bench.get("coins/lookup_block_inputs", "zakura-node-3rounds", p), "z1": bench.get("coins/lookup_block_inputs", "zakura-layout-1round", p),
                          "h": bench.get("coins/lookup_block_inputs", "hayai", p), "hw": bench.get("coins/lookup_block_inputs", "hayai-warm", p)}) for p in bench.params("coins/lookup_block_inputs")],
    )
    writes = chart(
        "Write the coins of one block", fmt_time,
        [("z", "Zakura layout", ZAKURA), ("h", "hayai", HAYAI)],
        [("13,000 spends and 13,000 new coins", {"z": bench.get("coins/commit_block", "zakura", "13000"), "h": bench.get("coins/commit_block", "hayai", "13000")}),
         ("13,000 coins created and spent before a write", {"z": bench.get("coins/fresh_spend", "zakura", "13000"), "h": bench.get("coins/fresh_spend", "hayai", "13000")})],
    )
    push = chart(
        "Add one block to the in-memory chain state", fmt_time,
        [("z", "copy the maps of recent blocks (Zakura model)", ZAKURA), ("h", "add one layer (hayai)", HAYAI)],
        [(f"{p} recent blocks", {"z": bench.get("state/push_block", "zakura-clone", p), "h": bench.get("state/push_block", "hayai-layer", p)}) for p in bench.params("state/push_block")],
    )
    walk = chart(
        "13,000 coin lookups through the recent blocks", fmt_time,
        [("walk", "check every recent block in turn", ZAKURA), ("index", "one index over the recent blocks", HAYAI)],
        [(f"{p} recent blocks", {"walk": bench.get("state/lookup_through_window", "hayai-walk", p), "index": bench.get("state/lookup_through_window", "hayai-index", p)}) for p in bench.params("state/lookup_through_window")],
    )
    cards.append(card(
        "state",
        "Each spent coin costs one memory lookup, and a new block is one entry added to a list.",
        "The node reads the coins of a whole block in one batch and keeps recent changes in memory. Coins that are created and spent within a short time never reach the disk.",
        "Zakura finds a coin with two database reads, and it reads the same coin up to three times for one block. It has no coin cache. For each new block, it copies its in-memory maps of the recent blocks once or twice.",
        "A block reads two kinds of state: the set of unspent transparent coins, and the sets of nullifiers that mark spent shielded notes. The node keeps the most recent blocks in memory so that it can switch to a competing chain.",
        [
            "The node keys each coin by its outpoint, so one read finds the coin.",
            "A cache in memory holds recent coins. A coin that is created and spent before the next write to disk is never written.",
            "Each recent block is one layer of changes on top of a shared base. A new block adds a layer. A switch to a competing chain removes layers.",
            "One index over all layers answers each lookup with one probe.",
            "Writes to disk run outside the lock that readers use. Each write records the last block that it contains, so the node can recover after a crash.",
        ],
        [
            ("Bitcoin Core coins cache", "Bitcoin Core keeps unspent coins in memory and writes them to disk in large batches. A coin that is spent before the write never reaches the disk."),
            ("Geth and Reth (Ethereum nodes)", "These nodes keep each recent block as a set of changes over a shared state. A new block or a chain switch does not copy the state."),
        ],
        "Nobody else.",
        [look, writes, push, walk],
    ))

    # 5. Wire bytes.
    parse = chart(
        "Parse a block and compute its transaction ids", fmt_time,
        [("z", "Zakura (zakura-chain)", ZAKURA), ("seq", "hayai, one thread", OTHER), ("h", "hayai", HAYAI)],
        [(p, {"z": bench.get("wire/parse_block", "zakura", p), "seq": bench.get("wire/parse_block", "hayai-sequential", p), "h": bench.get("wire/parse_block", "hayai", p)}) for p in bench.params("wire/parse_block")],
    )
    serve = chart(
        "Serve a stored block to a peer", fmt_time,
        [("z", "one database row per transaction, rebuild and encode (Zakura layout)", ZAKURA), ("h", "read the bytes from a flat file (hayai)", HAYAI)],
        [(p, {"z": bench.get("blockstore/get_block", "zakura", p), "h": bench.get("blockstore/get_block", "hayai", p)}) for p in bench.params("blockstore/get_block")],
    )
    cards.append(card(
        "wire",
        "The node parses each block once and never encodes it again.",
        "The node keeps the exact bytes it received, splits the block into transactions without a parse, and parses the transactions in parallel. Hashing, storage, relay and serving all use the same bytes.",
        "Zakura parses a block into objects and drops the bytes. It then encodes the block again to hash it, to store it, to measure it and to serve it. Its storage keeps one database row per transaction, so a request from a peer rebuilds the block.",
        "A Zcash block is at most 2 MB. A parsed block uses 3 to 4 times more memory than its bytes, according to Zakura's own measurement.",
        [
            "A scanner finds the boundary of each transaction with length arithmetic only.",
            "The node parses the transactions in parallel and keeps the bytes of each one.",
            "The node stores blocks in flat files exactly as received. To serve a block, it reads one file range.",
        ],
        [("Bitcoin Core block files", "Bitcoin Core stores blocks in append-only files, exactly as received, and serves them from these files.")],
        "Nobody else.",
        [parse, serve],
    ))

    # 6. Template.
    empty = bench.get("template/tip_event", "hayai-empty")
    full = bench.get("template/tip_event", "hayai-full")
    tmpl = chart(
        "Template selection", fmt_time,
        [("z", "build again on each request (Zakura)", ZAKURA), ("h", "update the live template (hayai)", HAYAI)],
        [(f"{p} candidates, first build", {"z": bench.get("template/build_from_scratch", "zakura-zip317", p), "h": bench.get("template/build_from_scratch", "hayai", p)}) for p in bench.params("template/build_from_scratch")]
        + [("one new transaction, 8,000 candidates", {"z": bench.get("template/incremental_add", "zakura-zip317"), "h": bench.get("template/incremental_add", "hayai")})],
    )
    cards.append(card(
        "template",
        "Your pool hashes on the new block a few milliseconds after your node accepts it, and the template carries fees from the start.",
        f"After a new block, a template with only the coinbase goes out in {esc(fmt_time(empty))} and the full template with fees in {esc(fmt_time(full))}. New fee-paying transactions reach the pool in milliseconds, not after a 5 s poll.",
        "Zakura builds the template again on every request and checks its memory pool for changes every 5 s. Its selection draws transactions at random and computes its weights again after each draw, so a full rebuild takes tens of milliseconds with a full memory pool. A fee-paying transaction can wait up to 5 s before it reaches the pool.",
        "A pool asks its node for a template: the block it will mine, without the proof of work. Until the pool has a template on the new block, all its hash power works on an outdated block. A template without the latest transactions loses their fees. The section <a href='#miners'>For miners</a> shows the full time from a competitor's block to your pool, of which the template is the last part.",
        [
            "The node keeps one template and applies each new transaction, each removed transaction and each new block as a small change.",
            "The selection is deterministic: transactions in order of fee per unit of cost (ZIP 317), parents before children. Two pool servers receive the same template.",
            "The node pushes each change to the subscribed pools. Pools that use <code>getblocktemplate</code> receive the same template through a compatible interface with long polling.",
        ],
        [("Stratum V2 template provider", "In Bitcoin mining, Stratum V2 lets the node push new templates to the pool when they change, instead of waiting for a request.")],
        "The pool connects to a hayai node. Existing pool software works through the getblocktemplate interface.",
        [tmpl],
    ))

    # 7. Cryptography.
    crypto_val = chart(
        "Validate a block of 165 Orchard bundles, transactions not seen before", fmt_time,
        [("up", "hayai with the official crates", HAYAI_ALT), ("zk", "hayai with Zakura's crates", HAYAI)],
        [("orchard-165x2", {"up": bench.get("validate/block", "hayai-cold", "orchard-165x2"), "zk": bench.get("validate/block", "hayai-zk-cold", "orchard-165x2")}),
         ("mixed-2000x1-100x2", {"up": bench.get("validate/block", "hayai-cold", "mixed-2000x1-100x2"), "zk": bench.get("validate/block", "hayai-zk-cold", "mixed-2000x1-100x2")})],
    )
    crh = chart(
        "One MerkleCRH hash (the hash of the Orchard note tree)", fmt_time,
        [("up", "official orchard crate", OTHER), ("z", "Zakura", ZAKURA), ("h", "hayai", HAYAI)],
        [("one hash", {"up": bench.get("sinsemilla/merkle_crh", "upstream"), "z": bench.get("sinsemilla/merkle_crh", "zakura"), "h": bench.get("sinsemilla/merkle_crh", "hayai")}),
         ("256 hashes in lockstep, per hash", {"z": (bench.get("sinsemilla/merkle_crh", "zakura-batch", "256") or 0) / 256 or None, "h": (bench.get("sinsemilla/merkle_crh", "hayai-lanes", "256") or 0) / 256 or None})],
    )
    tree = chart(
        "Add the note commitments of one block to the Orchard tree", fmt_time,
        [("up", "official crates, one at a time", OTHER), ("z", "Zakura", ZAKURA), ("h", "hayai", HAYAI)],
        [(f"{p} notes", {"up": bench.get("trees/orchard_append", "upstream", p), "z": bench.get("trees/orchard_append", "zakura", p), "h": bench.get("trees/orchard_append", "hayai", p)}) for p in bench.params("trees/orchard_append")],
    )
    cards.append(card(
        "crypto",
        "The cryptography comes from the official Zcash crates or from Zakura's faster versions, at build time.",
        "One build switch selects the cryptography, and every other improvement on this page works with both. hayai's own kernels follow the specification exactly.",
        "Zakura is fast partly because of its own versions of the Zcash cryptographic libraries. A second node that uses the same libraries shares their bugs. A node that uses only the official libraries is slower where they are slower.",
        "The official libraries take Zakura's improvements over time. For example, Zakura's Equihash improvements are being ported to the official crate.",
        [
            "All hayai crates use the cryptography through one interface crate. A build feature selects the official crates or Zakura's crates.",
            "hayai has its own MerkleCRH with precomputed tables, and its own fast field inversion that checks each result with one multiplication.",
            "hayai's MerkleCRH handles every special case that the specification defines. Zakura's version omits some of these checks and relies on a mathematical argument that they cannot occur.",
        ],
        [("Bernstein–Yang inversion (2019)", "A method to compute modular inverses with few operations. The Bitcoin signature library libsecp256k1 uses it. hayai uses it for the field of the Pallas curve.")],
        "Nobody else.",
        [crypto_val, crh, tree],
    ))


    # 9. Speculative tip and history tree.
    sw_rows = []
    for blk in ("orchard-165x2", "mixed-2000x1-100x2", "transparent-6500x1"):
        sw_rows.append((blk + ", transactions not seen before", {"s": bench.get("template/switch_after_block", "hayai-serial", blk + "-cold"), "p": bench.get("template/switch_after_block", "hayai-speculative", blk + "-cold")}))
        sw_rows.append((blk + ", transactions already prepared", {"s": bench.get("template/switch_after_block", "hayai-serial", blk + "-warm"), "p": bench.get("template/switch_after_block", "hayai-speculative", blk + "-warm")}))
    sw_chart = chart(
        "Time from a received block to the full template on it", fmt_time,
        [("s", "validate the block, then build the template", HAYAI_ALT), ("p", "build the template on the speculative layer", HAYAI)],
        sw_rows, note="The speculative path builds the template when the chain rules and the chain state check pass. Scripts and proofs finish in parallel.",
    )
    sp_cold = bench.get("template/switch_after_block", "hayai-speculative", "orchard-165x2-cold")
    se_cold = bench.get("template/switch_after_block", "hayai-serial", "orchard-165x2-cold")
    cards.append(card(
        "speculative",
        "Your pool mines on a new block while the node still checks its proofs, and a failed check restores the old template.",
        f"For a block with many shielded transactions that the node has not seen, the full template is ready after {esc(fmt_time(sp_cold))} instead of {esc(fmt_time(se_cold))}. The node commits the block only after all checks pass.",
        "Block validation is one serial step in front of the template. A block with 165 Orchard bundles needs about 140 ms for its proofs. During this time the pool hashes on the old block.",
        "A template on a new block must contain the block's effect on the chain history tree (ZIP 221): the header of the next block commits to it. The tree needs the new block's final note commitment roots, which the node computes in a few milliseconds. The proofs and scripts are the slow part, and they do not change the roots.",
        [
            "The node splits validation in two. The first part builds the new chain state: roots, history tree, coin changes, and all rules that depend on the chain. The second part checks scripts and proofs.",
            "When the first part succeeds, the node publishes the new state as a speculative tip and builds the template on it. The second part runs at the same time.",
            "When the second part succeeds, the node confirms the block. When it fails, the node removes the speculative state and every block built on it, restores the old template, and penalizes the sender.",
            "The node never commits or relays a block as valid before all checks pass. The window of work on an invalid block is at most the proof check time, under 150 ms with the NU7 limits.",
        ],
        [("Optimistic execution in Ethereum clients", "Some clients start to build on a new block before they finish all checks, and they roll back when a check fails. The idea is the same. The chain state of Zcash lets the node know which part is cheap.")],
        "Nobody else.",
        [sw_chart],
    ))

    # 10. Template as lane and candidate blocks.
    cand_rows = []
    for r in relay_bytes:
        if r["fixture"].startswith("main") or "candidate_ref_bytes" not in r:
            continue
        cand_rows.append((r["fixture"], {"full": r["full_bytes"], "short": r["short_id_bytes"], "batch": r["batch_ref_bytes"], "cand": r["candidate_ref_bytes"]}))
    cand_chart = chart(
        "Bytes to announce one block that equals a published candidate", fmt_bytes,
        [("full", "full block (legacy relay)", ZAKURA), ("short", "short ids", HAYAI_ALT), ("batch", "one batch reference", OTHER), ("cand", "candidate reference", HAYAI)],
        cand_rows, worse="more",
        note="The header (1,487 bytes) and the coinbase transaction are in every compact form.",
    )
    pre_chart = chart(
        "Commit a block that the node prepared before it arrived", fmt_time,
        [("w", "validate again (transactions already prepared)", HAYAI_ALT), ("s", "swap in the prepared state", HAYAI)],
        [(p, {"w": bench.get("state/commit_prebuilt", "hayai-warm", p), "s": bench.get("state/commit_prebuilt", "hayai-swap", p)}) for p in bench.params("state/commit_prebuilt")],
    )
    own_chart = chart(
        "Your own block: from the solved block to the full template on it", fmt_time,
        [("w", "validate and update the template", HAYAI_ALT), ("s", "swap in the prepared state", HAYAI)],
        [(p, {"w": bench.get("template/own_block_commit", "hayai-warm", p), "s": bench.get("template/own_block_commit", "hayai-swap", p)}) for p in bench.params("template/own_block_commit")],
    )
    cards.append(card(
        "lane",
        "A miner publishes its block candidate as it changes, so a solved block is a 61-byte reference and a diff.",
        "Every template change goes out as a small batch. A solved block names the candidate and lists only what changed. Your own block commits by a swap of state the node prepared in advance.",
        "A compact block names every transaction by a short fingerprint, so its size grows with the transaction count. A receiver that lacks one transaction needs one more round trip. The template order follows fee weight, so one new transaction in the middle breaks every batch reference after it.",
        "Pool servers know their candidate block long before they find a solution. They can tell their peers about it at no cost, as they do for their own hashers.",
        [
            "The block order is canonical: parents before children, then by transaction id. The same set of transactions always gives the same bytes. Selection by fee weight stays unchanged.",
            "Each template change goes out as one batch of the added transactions and one candidate announcement, which names the lane, the revision, the parent, and the removed positions.",
            "A solved block carries the header, the coinbase, the candidate reference, and a diff. A block that equals its candidate costs 61 bytes besides the header and the coinbase.",
            "A receiver that holds the candidate can prepare the new chain state in advance. A block that equals the candidate then commits by a swap. This is off by default for received candidates, because the preparation costs as much as the validation that it saves. For the node's own template it is on.",
            "A peer without the candidate feature sees none of this. It receives the normal compact block, or the legacy announcement.",
        ],
        [("Narwhal and Autobahn (research systems, 2022 and 2024)", "Nodes in these systems spread batches of transactions continuously, and the block only names them. The candidate reference goes one step further: it names the whole block that the miner is working on.")],
        "The peer, and the miner who publishes the lane. Peers without the feature receive the normal announcement.",
        [cand_chart, pre_chart, own_chart],
    ))

    # 11. In-memory coin set.
    mem_lookup = chart(
        "Read the spent coins of one block, 2 million coins", fmt_time,
        [("r", "RocksDB, hayai cache", HAYAI_ALT), ("m", "in memory, hayai cache", HAYAI), ("mb", "in memory, no cache", OTHER)],
        [(f"{p} inputs", {"r": bench.get("coins/lookup_block_inputs", "hayai", p), "m": bench.get("coins/lookup_block_inputs", "hayai-mem", p), "mb": bench.get("coins/lookup_block_inputs", "hayai-mem-backing", p)}) for p in ("1000", "13000")],
    )
    mem_commit = chart(
        "Write one block of 13,000 spends and 13,000 new coins", fmt_time,
        [("r", "RocksDB", HAYAI_ALT), ("m", "in memory, log synced per block", HAYAI), ("n", "in memory, no sync per block", OTHER)],
        [("13,000 spends and 13,000 new coins", {"r": bench.get("coins/commit_block", "hayai", "13000"), "m": bench.get("coins/commit_block", "hayai-mem", "13000"), "n": bench.get("coins/commit_block", "hayai-mem-nofsync", "13000")})],
        note="RocksDB does not sync its write-ahead log by default, so the no-sync column is the like-for-like comparison.",
    )
    snap_w = bench.get("coins/snapshot", "hayai-mem-write", "2002000")
    snap_l = bench.get("coins/snapshot", "hayai-mem-load", "2002000")
    cards.append(card(
        "memcoins",
        "The whole coin set lives in memory, at 80 bytes per coin, and a restart loads it in seconds.",
        f"Mainnet needs about 3.8 GB of memory for 27 million coins and 56 million nullifiers. A snapshot of 2 million coins writes in {esc(fmt_time(snap_w))} and loads in {esc(fmt_time(snap_l))}.",
        "A database on disk makes every cold coin lookup a random read, and the database rewrites uniform keys during its compaction. A node must also be able to restart after a crash without a long resync.",
        "Coins are write-once and delete-once. A miner node can rebuild its state from its block files. The Zcash chain has about 27 million unspent transparent coins and about 56 million nullifiers (Blockchair and the Orchard tree size, October 2026). The sizes of the Sapling and Sprout nullifier sets are estimates.",
        [
            "The node keeps coins and nullifiers in memory, in 256 shards. A shard stores a dense array of fixed-size entries and a small position index. P2PKH and P2SH scripts are stored as a 20-byte hash.",
            "Every block flush appends one checksummed record to a log. A snapshot writes the whole set in one sequential file, and then the log restarts.",
            "At start, the node loads the newest valid snapshot and replays the log. A torn last record is cut. Any other damage stops the start with an explicit error.",
            "A second log records the chain state: frontiers, anchors, history tree, and times. After a crash, the node replays the block files above the last record.",
            "The RocksDB backing stays available for nodes with less memory.",
        ],
        [("Bitcoin Core UTXO snapshots (assumeutxo)", "Bitcoin Core can start from a snapshot of the coin set. hayai uses its own snapshot for fast restart. A snapshot for first synchronization needs a trusted hash of the state, as in assumeutxo.")],
        "Nobody else.",
        [mem_lookup, mem_commit],
    ))

    # 12. hayaid and deployment.
    cards.append(card(
        "hayaid",
        "hayaid runs on Regtest, on Testnet next to a Zakura node, and on Mainnet, with traces and metrics from the first start.",
        "In shadow mode the node follows a local Zakura node, validates every block, and writes traces with the same event names. Docker, systemd and Terraform files install it with Prometheus and Grafana.",
        "A new implementation needs a safe way to prove that it agrees with the nodes that miners trust, and a way to compare its speed with theirs on the same blocks.",
        "Zakura writes JSONL traces and Prometheus metrics. Testnet blocks are small, so the shadow comparison shows the typical case. The Regtest pair shows full blocks.",
        [
            "Shadow mode reads each block from the local Zakura node, validates it with hayai, and compares the verdict. A disagreement stops the node and writes an error row. The node never announces blocks and never serves templates in this mode.",
            "The shadow node trusts a short list of facts from the upstream node. Each one has a counter in the metrics: coins before the start height, old nullifiers, old anchors, difficulty bits.",
            "Full mode on Regtest produces blocks and interoperates with Zakura Regtest: both use 36-byte Equihash solutions.",
            "The traces use Zakura's event names for block receipt and commit, so one script joins the traces of both nodes by block hash. The metrics use Zakura's names where the meaning is the same.",
            "A Dockerfile, a compose stack with Prometheus, Grafana and Alertmanager, a systemd unit, an install script, and an AWS Terraform module install the node. CI runs the checks on both cryptography backends.",
        ],
        [],
        "Nobody else.",
        [],
    ))

    # 8. Compatibility.
    compat_rows = [
        ("Parse once, keep the received bytes", "Nobody else"),
        ("Flat block files", "Nobody else"),
        ("Coin cache, one batched read per block", "Nobody else"),
        ("Prepared transactions", "Nobody else"),
        ("Bulk validation and batch splitting", "Nobody else"),
        ("Layered chain state and window index", "Nobody else"),
        ("Own cryptographic kernels", "Nobody else; the output is identical"),
        ("Live template", "The pool; getblocktemplate works for other pools"),
        ("Compact blocks and batches", "The peer; legacy peers receive the full block"),
        ("Forward on a verified identifier list", "The peer; legacy peers receive the block before the commit"),
    ]
    table = "<div class='scroll'><table><thead><tr><th>Feature</th><th>Who must run hayai</th></tr></thead><tbody>" + "".join(
        f"<tr><td>{esc(a)}</td><td>{esc(b)}</td></tr>" for a, b in compat_rows) + "</tbody></table></div>"
    cards.append(card(
        "compat",
        "Every hayai node speaks the normal Zcash protocol, so miners who do not switch lose nothing.",
        "No feature changes which blocks are valid. The new relay messages start only when both peers ask for them.",
        "Most miners now run one implementation. One bug in it can stop or split most of the hash power at the same time.",
        "A second implementation helps only if miners use it, and miners use it only if it is faster and if it does not isolate them from the rest of the network.",
        [
            "hayai implements the consensus rules of Zcash and changes none of them. The list of rules and their status is in <code>docs/consensus-rules.md</code>.",
            "On every connection, hayai speaks the legacy protocol. A peer that sets a service bit and sends a version message for the extension gets the compact relay; every other peer gets the legacy messages.",
            "The extension has a version range and feature bits, so it can change without a coordinated upgrade.",
        ],
        [],
        "Nobody else.",
        [table],
    ))
    order = ["relay", "lane", "speculative", "verify-once", "template", "state", "memcoins", "bulk", "wire", "crypto", "hayaid", "compat"]
    by_id = {c.split('id="', 1)[1].split('"', 1)[0]: c for c in cards}
    return "\n".join(by_id[i] for i in order if i in by_id)


def compatibility_section():
    def table(head, rows):
        th = "".join(f"<th>{esc(h)}</th>" for h in head)
        trs = "".join("<tr>" + "".join(f"<td>{c}</td>" for c in r) + "</tr>" for r in rows)
        return f"<div class='scroll'><table><thead><tr>{th}</tr></thead><tbody>{trs}</tbody></table></div>"

    tests = table(["Test", "What it shows", "Result"], [
        ("Regtest pair with a real Zakura node",
         "A <code>hayaid</code> and a <code>zakurad</code> run next to each other on a private chain. Each node syncs from the other, mines on the blocks of the other, relays transactions, and resolves forks of depth 1, 3 and 10.",
         "Same tip, value pools and tree roots at each compared height. Zakura accepts each block that hayai makes, with Orchard and Ironwood transactions. Both refuse the same 20 invalid blocks. A 30 min run of 622 blocks has no divergence. A run across the NU7 activation gives equal state. The test found 7 defects in the network and RPC code of hayai; all are fixed. One difference is open: on a private chain, the two nodes can be configured differently at the NU6.1 activation block."),
        ("Published block and transaction vectors",
         "hayai accepts real blocks of each network upgrade and computes the published ids, signature hashes and tree roots.",
         "90 block vectors: 57 pass every stage, 32 pass until the vector set has no more chain state, 1 is rejected as expected."),
        ("Rule-by-rule checklist against Zakura's source",
         "Each header, block and transaction rule has the same constants and boundaries.",
         "About 60 rules compared and found equal. The differences found are fixed."),
        ("Comparison with Zakura's and Zebra's library code",
         "Subsidy, funding streams, difficulty, block limits and tree roots are equal across each activation height, NU7 included.",
         "Equal on all tested heights and on 6,000 generated blocks for the difficulty."),
        ("Differential fuzz search",
         "hayai and Zakura's check functions give the same verdict on changed blocks.",
         "About 260 million cases, no disagreement on validity."),
        ("Legacy protocol",
         "The message encoding equals Zebra's byte vectors. A legacy peer and a hayai peer follow the tip together in the same test network.",
         "Pass."),
    ])
    features = table(["Feature", "Gain with one hayai node", "Needs hayai on both sides"], [
        ("Stored verification results", "Yes", "No"),
        ("In-memory coin set and chain state", "Yes", "No"),
        ("Template kept up to date", "Yes", "No"),
        ("Speculative tip", "Yes", "No"),
        ("Template push to the pool server", "Yes, when the pool server uses the protocol. <code>getblocktemplate</code> works for the others.", "No"),
        ("Compact block relay", "No", "Yes. A legacy peer gets the normal announcement and the full block."),
        ("Batch lanes", "No", "Yes. A miner can turn the publication off."),
        ("Candidate blocks", "No", "Yes"),
        ("Forward before validation", "No", "Yes. A legacy peer gets a block only after its validation."),
    ])
    return f"""
<section id="compatibility">
<h2>Compatibility</h2>
<p>hayai follows the same consensus rules as the other Zcash nodes and speaks the normal Zcash protocol with them. The new protocols start only when both peers announce them. No feature changes which blocks are valid.</p>
<h3>Tests of compatibility with the network</h3>
{tests}
<p class="note">Not done yet: a sync of Testnet and a replay of Mainnet blocks. NU7 works with Zakura's cryptography crates only, because the official crates do not know the NU7 branch id yet.</p>
<h3>Features and the nodes they need</h3>
{features}
<p class="note">Two choices of hayai differ from Zakura without a protocol and without an effect on validity. hayai puts the transactions of its own blocks in a fixed order, so that a short reference gives the exact block. hayai selects transactions by fee weight in a fixed order, where ZIP 317 recommends a weighted random pick.</p>
</section>"""



TESTS = 799


def safety_section():
    def block(title, items):
        lis = "".join(f"<li><b>{esc(h)}</b> {b}</li>" for h, b in items)
        return f"<h3>{esc(title)}</h3><ul class='safety'>{lis}</ul>"

    design = block("Rules the code follows", [
        ("Almost no unsafe code.", "14 of the 19 crates forbid <code>unsafe</code> at compile time. The node has one <code>unsafe</code> block, in the Sinsemilla hash crate. The benchmark crate has the others, for its measurement counters."),
        ("No silent skip.", "A rule that hayai does not implement returns an explicit error, and the block is not accepted. At a height where a network upgrade that hayai does not implement is active, the node stops with a clear error."),
        ("No panic from network data.", "Each assertion, index and conversion that peer data can reach returns an error. A search of the consensus crates found two such sites, and both are fixed."),
        ("A block is marked invalid only when its header commits to the fault.", "The node sorts each failure into one of three classes. A body that does not match its header costs the peer that sent it, and the node asks another peer. A fault of the node itself, for example a missing key, stops the node. Only a fault that the header hash commits to marks the block invalid. A peer cannot make the node reject an honest block."),
        ("Stored results cannot be reused for a different transaction.", "The key of a prepared transaction is its transaction id, the hash of its signatures and proofs, and the rule set in force. At a network upgrade the node drops the stored transactions of the old rule set."),
        ("Checks that depend on the chain are never stored.", "Unspent inputs, new nullifiers, valid anchors, expiry and lock time are checked against the exact parent state for every block."),
        ("Fast arithmetic checks its own result.", "The fast field inversion multiplies its result by the input and compares the product with 1. On a mismatch it uses the standard routine and counts the event."),
        ("The pool never mines on a block the node rejected.", "With the speculative tip, the node commits a block only after all checks pass. A failed check removes the block, every block built on it, and restores the template."),
        ("Stored state is checksummed and written in a safe order.", "Each record of the coin log, the state log and the header log has a checksum. The block files are synced to disk before the coin set. A torn last record is cut and reported. Any other damage stops the start with an error."),
        ("The normal protocol always works.", "The new relay messages need an agreement of both peers. A failure in the new path falls back to the full block. No relay feature changes which blocks are valid."),
    ])
    peers = block("Protection against a hostile peer", [
        ("Scores and bans.", "Each fault of a peer has a score. A peer that reaches the limit is disconnected and banned. A header that fails only the local clock rule costs nothing, because an honest peer can send it."),
        ("Bounded memory.", "The block download holds at most 1 GiB. The outbound queue of a connection holds at most 32 MB. The driver queue holds at most 32 messages for each peer. Each network command has its own size limit. The header chain holds at most 65,536 side headers."),
        ("Stalls and withheld blocks.", "A peer that stops sending loses its requests after 2 s and gets a penalty after 8 s. A peer that repeats known headers loses the header sync. When no peer sends the blocks of the header chain with the most work, the node follows and mines on the best chain that it validated in full."),
        ("Mutated bodies.", "A body with a repeated transaction can have the same merkle root as the real block (CVE-2012-2459). The node detects this form and treats it as a wrong body."),
        ("Checkpoints and reorg depth.", "The node embeds 14,385 Mainnet and 10,059 Testnet checkpoints. It refuses a fork below the last checkpoint it reached, and a reorg deeper than 1,000 blocks."),
        ("Mempool.", "Admission follows ZIP 317, ZIP 401, ZIP 203 and the standardness rules. A transaction prepared on an old tip is not inserted after a new block is committed."),
        ("Legacy peers learn a block only after its validation.", "A legacy peer bans a node that announces an invalid block. hayai forwards before validation only to peers that agreed to the new relay protocol."),
    ])
    tests = block("How the code is tested", [
        (f"{TESTS} tests on each cryptography backend.", "Both builds (official crates and Zakura's crates) must pass the same tests. A difference between the two libraries shows as a failure."),
        ("Published test vectors.", "Transaction ids and signature hashes (ZIP 244), the chain history tree (ZIP 221), 1,046 script vectors, the Sprout tree vectors, and Zebra's byte vectors for the network messages."),
        ("90 published block vectors.", "57 pass every stage. 32 pass the stages that need no earlier chain state, and stop where the vector set does not hold that state. 1 is rejected, as the set expects. No vector is unsupported."),
        ("Real proofs.", "Generated blocks hold real ECDSA signatures and real Orchard and Ironwood proofs. 5 real Sprout JoinSplit proofs from published transactions pass, and each fails after a change of one bit."),
        ("Comparison with other implementations.", "Merkle roots, tree roots, subsidies and funding streams equal those of Zakura's and Zebra's library code. The in-memory coin set equals the RocksDB store under random operation sequences. The checkpoint path gives the same state as full validation on the same chain."),
        ("A test network in one process.", "Scenarios run real nodes against each other: sync from genesis, a peer that stalls, a peer with an invalid block, a peer that withholds blocks, a reorg with the return of transactions to the mempool, two network upgrades without a restart, and compact relay together with a legacy peer."),
        ("Crash tests.", "A sync is stopped at random points without a clean shutdown. The final state must equal the state of a sync without a stop. The coin log is truncated at every byte of its last record."),
        ("Property tests.", "Random cases test the window index, the transaction scanner, the incremental template, the block download scheduler and each message decoder. Each decoder also runs on truncated valid messages."),
        ("Differential fuzz search.", "A fuzzer changes valid blocks and headers in 13 ways and compares the verdict of hayai with Zakura's check functions. About 260 million cases gave no case where hayai and Zakura disagree on validity. One difference is not a consensus matter: hayai refuses a block message with extra bytes after the block."),
        ("Shadow mode.", "hayaid validates each block next to a Zakura node and stops at the first disagreement."),
        ("Continuous checks.", "Format, lints as errors, the tests on both backends, the fuzz smoke test, and a license and advisory check run in CI."),
    ])
    reviews = block("Independent reviews", [
        ("Review of the core, 2026-10-03.", "A reviewer that did not write the code compared each consensus rule with Zebra and Zakura. It found 9 defects, 4 of them in consensus rules. All 9 are fixed, each with a test."),
        ("Rule-by-rule checklist.", "A comparison of every header, block and transaction rule with Zakura found 6 more gaps. All 6 are fixed."),
        ("Review of consensus and sync, 2026-10-04.", "A second review found 6 critical, 8 high, 11 medium and 14 low findings. The critical ones were a header version that only hayai accepted, three ways for a peer to stop the node, and two ways for an honest block to get an invalid mark. All critical and high findings are fixed. 12 of the 14 have a test; the lock order fix and the disk sync order are verified by reading. 10 medium findings are fixed. Of the low findings, 4 are fixed, 2 are not defects and 8 are open. The review file lists each one."),
        ("Audit of the benchmarks.", "A separate audit found that the first model of Zakura's validation made Zakura look slower than it is. The model is corrected, and this page uses the corrected numbers."),
    ])
    limits = block("Known limits", [
        ("No run against the public network yet.", "The node syncs and follows the tip in a test network and next to a real Zakura node on a private chain. A Testnet sync from genesis and a replay of Mainnet blocks are the next gates."),
        ("NU7 needs Zakura's cryptography crates.", "The official Zcash crates do not know the NU7 branch id yet. A hayai build with the official crates stops at the NU7 activation height with a clear error."),
        ("Trust below the last checkpoint.", "As in Zakura, blocks below the last checkpoint are verified by hash, not by proof. Sprout proofs of the old BCTV14 type are never verified."),
        ("Limits of the fuzzer.", "It does not cover Sapling and Sprout proofs, anchors, the chain value pools or the checkpoints. Some state rules on the Zakura side are models. The Regtest pair covers these."),
        ("Shadow mode and Sprout.", "No RPC of a Zakura node gives the Sprout tree. A shadow node stops with a clear error at the first block that holds a JoinSplit."),
        ("Growth of two log files.", "The state log and the header log have no compaction yet."),
    ])
    return f"""
<section id="safety">
<h2>Hardening</h2>
<p>A node that is fast and wrong loses blocks for its miner and can split the chain. These are the rules the code follows, the protection against hostile peers, the tests, the reviews, and the limits that remain.</p>
{design}
{peers}
{tests}
{reviews}
{limits}
</section>"""


def status_section():
    rows = [
        ("done", "Validation engine", "Consensus rules up to NU6.3 with both cryptography backends, and NU7 with Zakura's crates."),
        ("done", "The hayaid node", "Header sync, block download, checkpoints, reorg, restart, and a mempool that follows ZIP 317, ZIP 401 and ZIP 203."),
        ("done", "Relay protocols", "Compact relay, transaction lanes and candidate blocks, with the draft ZIP. The normal protocol stays in use with every other node."),
        ("done", "Mining interface", "Template push protocol and a getblocktemplate interface."),
        ("done", "Benchmarks", "Against Zakura and Zebra code, with both cryptography backends."),
        ("done", "Verification", "Published vectors, comparison with Zakura's library code, a fuzz search and two independent reviews."),
        ("done", "Pair with a real Zakura node", "Sync, mining, transactions, forks and invalid blocks in both directions on a private chain, across the upgrades up to NU7."),
        ("done", "Deployment", "Docker, systemd, Terraform for AWS, Prometheus and Grafana."),
        ("progress", "Continuous integration", "The fast workflow runs on each push. The first runs on hosted runners are in progress."),
        ("progress", "Operator interface", "The RPC methods that pools and operators use, each compared with a Zakura node."),
        ("progress", "Lane publication setting", "A miner can publish its candidate, publish nothing, or keep chosen transactions private."),
        ("progress", "Fee policy values", "The same fee constants as Zakura, so that both nodes relay the same transactions."),
        ("progress", "Private chain configuration", "Lockbox and funding stream settings on Regtest, to match Zakura at the NU6.1 activation block."),
        ("todo", "Testnet", "A sync from genesis and 24 h at the tip, next to a Zakura node."),
        ("todo", "Mainnet replay", "10,000 Mainnet blocks in shadow mode with zero disagreements."),
        ("todo", "Live latency measurement", "Block and template latency against Zakura on Testnet (docs/testnet-benchmark-plan.md)."),
        ("todo", "Index for wallets", "Transaction and address indexes, so that lightwalletd or Zaino can use hayai as a full node."),
        ("todo", "NU7 with the official crates", "It waits for the NU7 branch id in the official Zcash crates."),
        ("todo", "Log compaction", "The state log and the header log grow without a limit today."),
    ]
    label = {"done": "Done", "progress": "In progress", "todo": "To do"}
    trs = "".join(
        f"<tr><td><span class='st {k}'>{label[k]}</span></td><td class='what'>{esc(t)}</td><td>{esc(d)}</td></tr>"
        for k, t, d in rows
    )
    n = {k: sum(1 for r in rows if r[0] == k) for k in label}
    return f"""<section id="status">
<h2>Status</h2>
<p>{n['done']} items are done, {n['progress']} are in progress and {n['todo']} are to do. The consensus coverage is listed rule by rule in <code>docs/consensus-rules.md</code>. A rule that is not implemented returns an error. It never passes silently.</p>
<div class="scroll"><table class="status"><thead><tr><th>State</th><th>Item</th><th>Detail</th></tr></thead><tbody>{trs}</tbody></table></div>
</section>"""


# ---------------------------------------------------------------- page


def main():
    bench = Bench(load("summary.json", {}))
    system = System(load("system.json", {}))
    relay_bytes = load("relay-bytes.json", [])
    m = bench.machine

    page = f"""<title>hayai</title>
<meta name="description" content="A Zcash miner node core, measured against Zakura">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Sora:wght@500;600;700&family=IBM+Plex+Sans:wght@400;500&family=IBM+Plex+Mono:wght@400;500&display=swap">
<style>
/* Layout: one reading column, 880px wide; tables and charts scroll inside their own box. */
:root {{
  --bg:#f6f7f9; --fg:#14181f; --muted:#5b6472; --line:#d9dee6; --card:#ffffff; --accent:#1c5cab;
  --good:#0a7d33; --bad:#b4401f;
  --s1:#2a78d6; --s1b:#86b6ef; --s2:#eb6834; --s3:#1baf7a;
  --display:'Sora',system-ui,sans-serif; --body:'IBM Plex Sans',system-ui,sans-serif; --mono:'IBM Plex Mono',ui-monospace,monospace;
}}
@media (prefers-color-scheme: dark) {{ :root:not([data-theme="light"]) {{
  --bg:#121417; --fg:#eceff3; --muted:#a7b0bd; --line:#2a3038; --card:#1a1e24; --accent:#6da7ec;
  --good:#4cc77a; --bad:#f08a64;
  --s1:#3987e5; --s1b:#5598e7; --s2:#d95926; --s3:#199e70; color-scheme:dark }} }}
:root[data-theme="dark"] {{
  --bg:#121417; --fg:#eceff3; --muted:#a7b0bd; --line:#2a3038; --card:#1a1e24; --accent:#6da7ec;
  --good:#4cc77a; --bad:#f08a64;
  --s1:#3987e5; --s1b:#5598e7; --s2:#d95926; --s3:#199e70; color-scheme:dark }}
body {{ background:var(--bg); color:var(--fg); font-family:var(--body); font-size:16px; line-height:1.55; padding-inline:16px; padding-block:0 64px; margin:0; }}
main {{ max-width:880px; margin:0 auto; }}
header.hero {{ padding-block:56px 8px; }}
h1 {{ font-family:var(--display); font-weight:700; font-size:clamp(40px,8vw,72px); letter-spacing:-0.02em; margin:0; line-height:1; }}
.tag {{ font-family:var(--display); color:var(--muted); font-size:clamp(18px,3vw,24px); margin:12px 0 0; text-wrap:balance; max-width:32ch; }}
p, li, dd {{ max-width:70ch; }}
h2 {{ font-family:var(--display); font-weight:600; font-size:28px; letter-spacing:-0.01em; margin:48px 0 12px; text-wrap:balance; }}
h4 {{ font-family:var(--display); font-weight:600; font-size:15px; margin:20px 0 6px; text-transform:uppercase; letter-spacing:0.05em; color:var(--muted); }}
code {{ font-family:var(--mono); font-size:0.9em; background:var(--card); border:1px solid var(--line); padding:0 4px; border-radius:3px; }}
table {{ border-collapse:collapse; width:100%; font-size:14px; font-variant-numeric:tabular-nums; }}
th, td {{ text-align:left; padding:9px 10px; border-bottom:1px solid var(--line); vertical-align:top; }}
th {{ font-weight:500; color:var(--muted); text-transform:uppercase; letter-spacing:0.04em; font-size:12px; }}
td.num {{ text-align:right; white-space:nowrap; font-family:var(--mono); }}
td.h {{ font-weight:500; color:var(--accent); }}
table.general .what {{ font-weight:500; }}
.better {{ color:var(--good); font-weight:500; }}
.worse {{ color:var(--bad); font-weight:500; }}
.sub, .note, .foot {{ color:var(--muted); font-size:13px; }}
ul.foot {{ list-style:none; padding:0; margin:8px 0 0; }}
.scroll {{ overflow-x:auto; }}
details.feature {{ border:1px solid var(--line); border-radius:6px; background:var(--card); margin:12px 0; }}
details.feature > summary {{ list-style:none; cursor:pointer; padding:16px 44px 16px 18px; position:relative; display:block; }}
details.feature > summary::-webkit-details-marker {{ display:none; }}
details.feature > summary::after {{ content:"+"; position:absolute; right:18px; top:14px; font-family:var(--mono); font-size:20px; color:var(--muted); }}
details.feature[open] > summary::after {{ content:"–"; }}
details.feature > summary .title {{ display:block; font-weight:600; font-size:17px; text-wrap:balance; }}
details.feature > summary .summary {{ display:block; color:var(--muted); margin-top:6px; }}
details.feature .body {{ padding:0 18px 18px; border-top:1px solid var(--line); }}
details.sys {{ margin-top:16px; }}
details.sys > summary {{ cursor:pointer; color:var(--accent); font-weight:500; }}
dl dt {{ font-weight:500; margin-top:8px; }}
dl dd {{ margin:2px 0 0 0; }}
figure {{ margin:20px 0; }}
figcaption {{ font-family:var(--display); font-weight:600; margin-bottom:6px; }}
.legend {{ display:flex; flex-wrap:wrap; gap:14px; font-size:13px; color:var(--muted); margin-bottom:6px; }}
.legend .key i {{ display:inline-block; width:12px; height:12px; border-radius:3px; margin-right:6px; vertical-align:-1px; }}
.aud {{ display:grid; grid-template-columns:repeat(auto-fit,minmax(260px,1fr)); gap:12px; margin:28px 0 8px; }}
.audc {{ display:flex; flex-direction:column; gap:6px; padding:18px; border:1px solid var(--line); border-radius:6px; background:var(--card); color:var(--fg); text-decoration:none; }}
.audc:hover {{ border-color:var(--accent); }}
.eyebrow {{ font-size:12px; text-transform:uppercase; letter-spacing:0.06em; color:var(--accent); font-weight:500; }}
.ah {{ font-family:var(--display); font-weight:600; font-size:18px; text-wrap:balance; }}
h3 {{ font-family:var(--display); font-weight:600; font-size:18px; margin:28px 0 8px; }}
ul.safety {{ padding-left:20px; }}
ul.safety li {{ margin:6px 0; }}
.st {{ display:inline-block; white-space:nowrap; font-size:12px; font-weight:500; text-transform:uppercase; letter-spacing:0.04em; padding:2px 8px; border-radius:10px; border:1px solid currentColor; }}
.st.done {{ color:var(--good); }}
.st.progress {{ color:var(--accent); }}
.st.todo {{ color:var(--muted); }}
table.status .what {{ font-weight:500; white-space:nowrap; }}
@media (max-width:620px) {{ table.status .what {{ white-space:normal; }} }}
table.cmp {{ font-size:13px; margin-top:8px; }}
table.cmp td, table.cmp th {{ padding:5px 6px; }}
table.cmp th {{ text-align:right; }}
.calc {{ border:1px solid var(--line); border-radius:6px; background:var(--card); padding:16px 18px; margin:16px 0; }}
.controls {{ display:flex; flex-wrap:wrap; gap:10px 18px; font-size:14px; margin-bottom:12px; }}
.controls label {{ display:flex; align-items:center; gap:6px; color:var(--muted); }}
.controls input, .controls select {{ font:inherit; color:var(--fg); background:var(--bg); border:1px solid var(--line); border-radius:4px; padding:3px 6px; }}
.controls input {{ width:5.5em; }}
.controls select {{ max-width:100%; }}
.lkey {{ display:flex; gap:16px; font-size:13px; color:var(--muted); margin-bottom:8px; }}
.lkey i {{ display:inline-block; width:12px; height:12px; border-radius:3px; margin-right:6px; vertical-align:-1px; }}
.lrow {{ display:grid; grid-template-columns:minmax(0,15em) minmax(0,1fr) auto; gap:10px; align-items:center; padding:6px 0; border-top:1px solid var(--line); }}
.llabel {{ font-size:14px; }}
.ltrack {{ display:flex; height:16px; background:var(--bg); border-radius:3px; overflow:hidden; }}
.seg {{ display:block; height:100%; }}
.seg.network {{ background:var(--s2); }}
.seg.validation {{ background:var(--s1); }}
.seg.template {{ background:var(--s3); }}
.lval {{ font-family:var(--mono); font-size:13px; white-space:nowrap; text-align:right; }}
@media (max-width:620px) {{ .lrow {{ grid-template-columns:1fr; }} .lval {{ text-align:left; }} }}
.facts {{ display:grid; grid-template-columns:repeat(auto-fit,minmax(220px,1fr)); gap:12px; margin:16px 0; }}
.fact {{ border-left:3px solid var(--accent); padding:4px 0 4px 12px; font-size:14px; }}
.fact .big {{ font-family:var(--display); font-weight:700; font-size:28px; color:var(--accent); font-variant-numeric:tabular-nums; }}
.lead {{ font-size:clamp(18px,2.4vw,21px); line-height:1.45; margin:28px 0 8px; max-width:62ch; }}
h2.new {{ margin-top:36px; }}
.toc {{ border-top:1px solid var(--line); border-bottom:1px solid var(--line); padding:14px 0; margin:24px 0 8px; }}
.toc ol {{ list-style:decimal; display:flex; flex-direction:column; gap:4px; padding:0 0 0 22px; margin:8px 0 0; font-size:15px; }}
.toc li::marker {{ color:var(--muted); font-variant-numeric:tabular-nums; }}
.toc a {{ text-decoration:none; }}
.toc a:hover {{ text-decoration:underline; }}
.nfs {{ display:grid; grid-template-columns:repeat(auto-fit,minmax(250px,1fr)); gap:0 28px; margin:4px 0 8px; }}
.nf {{ display:flex; flex-direction:column; gap:6px; padding:18px 0 20px; border-top:1px solid var(--line); color:var(--fg); text-decoration:none; min-width:0; }}
.nf:hover .nft {{ color:var(--accent); }}
.nft {{ font-family:var(--display); font-weight:600; font-size:19px; line-height:1.25; text-wrap:balance; }}
.nfd {{ color:var(--muted); font-size:15px; }}
.nfv {{ margin-top:auto; padding-top:6px; font-size:14px; }}
.nfv b {{ font-family:var(--display); font-weight:700; font-size:30px; color:var(--accent); font-variant-numeric:tabular-nums; margin-right:6px; }}
.lead b {{ color:var(--accent); font-weight:600; }}
.facts.top .big {{ font-size:clamp(34px,6vw,44px); line-height:1.1; }}
.facts.top {{ margin:20px 0 4px; }}
svg {{ max-width:100%; height:auto; font-family:var(--body); }}
svg .lbl {{ fill:var(--fg); font-size:13px; }}
svg .val {{ fill:var(--fg); font-size:12px; font-family:var(--mono); }}
svg .ratio {{ fill:var(--muted); }}
summary:focus-visible, a:focus-visible {{ outline:2px solid var(--accent); outline-offset:2px; }}
a {{ color:var(--accent); }}
</style>
<main>
<header class="hero">
<h1>hayai</h1>
<p class="tag">The fast, independent Zcash node for miners.</p>
{headline(bench, relay_bytes)}
{audience_cards(bench, system, relay_bytes)}
</header>
{miners_section(bench, relay_bytes)}
{network_section(bench, system, relay_bytes)}
{general_table(bench, system, relay_bytes)}
<section id="features">
<h2>Features</h2>
<p>Open a feature to see the problem, the solution, and its measurements.</p>
{features(bench, relay_bytes)}
</section>
{compatibility_section()}
{safety_section()}
<section id="method">
<h2>Method</h2>
<ul>
<li>Machine: {esc(m.get('cpu', ''))}, {esc(m.get('threads', ''))} threads, Linux.</li>
<li>A Zakura baseline is one of four kinds, named in each row: Zakura's published crates run in the same process; Zakura's data layout on the same storage engine; Zakura's scheduling rebuilt around the same cryptography; or Zakura's algorithm ported line by line. Each port cites the Zakura source lines in <code>crates/hayai-bench/src/</code>.</li>
<li>Test blocks are synthetic and deterministic, with real ECDSA signatures and real Orchard proofs (<code>crates/hayai-bench/src/fixtures.rs</code>).</li>
<li>Times are criterion means. System values are medians of 20 runs, each scenario in a fresh process.</li>
<li>To reproduce: <code>cargo bench -p hayai-bench</code>, <code>scripts/sysbench.sh</code>, <code>python3 scripts/collect_bench.py</code>, <code>python3 scripts/report.py</code>.</li>
</ul>
</section>
{status_section()}
</main>
"""
    OUT.write_text(page)
    print(f"wrote {OUT} ({OUT.stat().st_size // 1024} kB)")


if __name__ == "__main__":
    main()
