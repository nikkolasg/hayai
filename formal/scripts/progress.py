#!/usr/bin/env python3
"""The progress of the formal verification, rule by rule.

The script reads every rule row of `docs/consensus.md` (the tables with the columns Rule,
Code and Test) and gives each one a status:

  proven       `formal/proven.tsv` maps the rule to Lean theorems, and each theorem exists
               and depends on no `sorryAx` (checked with `#print axioms`);
  in core      the Code column links into `hayai-consensus-core`: the code is translated to
               Lean, but no theorem covers the rule yet;
  outside      the code is outside the core (cryptography, parsing, state, network): the
               current stages do not cover it.

It writes `formal/PROGRESS.md`: a summary by section and a table with a row for each rule,
linked to its line in `docs/consensus.md`.

    formal/scripts/progress.py            # check the theorems with Lean, write PROGRESS.md
    formal/scripts/progress.py --check    # the same, and fail when PROGRESS.md was stale
    formal/scripts/progress.py --no-lean  # skip the Lean check (a quick look; never in CI)
    formal/scripts/progress.py --pr       # print the Markdown of the summary for a PR body

`formal/proven.tsv` has one line per rule: the heading of the section of `docs/consensus.md`
(the text after `## `, up to the first `:`, without the link), a tab, the exact text of the
Rule cell, a tab, and the Lean theorems that prove it, separated by spaces. A line whose rule
is not in `docs/consensus.md` is an error: the text of a rule changed, and the mapping must
follow it.
"""
import argparse
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DOC = os.path.join(ROOT, "docs", "consensus.md")
MAP = os.path.join(ROOT, "formal", "proven.tsv")
OUT = os.path.join(ROOT, "formal", "PROGRESS.md")
FORMAL = os.path.join(ROOT, "formal")
BLOB = "https://github.com/zodl-inc/hayai/blob/main/docs/consensus.md"
CORE = "hayai-consensus-core/src/"

PROVEN, IN_CORE, OUTSIDE = "proven", "in core", "outside"
MARK = {PROVEN: "✅ proven", IN_CORE: "🟡 in core, not yet proven", OUTSIDE: "⬜ outside the core"}


def section_name(heading):
    """`[ZIP 207](url): Funding Streams` -> `ZIP 207`; `Checkpoints` -> `Checkpoints`."""
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", heading)
    return text.split(":", 1)[0].strip()


def cells(line):
    """The cells of a table row. A cell holds no unescaped `|`."""
    return [c.strip() for c in line.strip().strip("|").split("|")]


def read_rules():
    rules = []
    section = None
    in_rule_table = False
    with open(DOC, encoding="utf-8") as f:
        for number, line in enumerate(f, 1):
            if line.startswith("## "):
                section = section_name(line[3:].strip())
                in_rule_table = False
                continue
            if not line.startswith("|"):
                in_rule_table = False
                continue
            row = cells(line)
            if row[:3] == ["Rule", "Code", "Test"]:
                in_rule_table = True
                continue
            if not in_rule_table or set(row[0]) <= set("-: "):
                continue
            code = row[1] if len(row) > 1 else ""
            rules.append({"line": number, "section": section, "rule": row[0], "code": code})
    return rules


def read_map():
    mapping = {}
    with open(MAP, encoding="utf-8") as f:
        for number, line in enumerate(f, 1):
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) != 3 or not parts[2].split():
                sys.exit(f"{MAP}:{number}: expected section<TAB>rule<TAB>theorems")
            mapping[(parts[0], parts[1])] = parts[2].split()
    return mapping


def check_theorems(theorems):
    """Each theorem exists and depends on no sorryAx: the result of `#print axioms`."""
    names = sorted(set(theorems))
    with tempfile.NamedTemporaryFile("w", suffix=".lean", dir=FORMAL, delete=False) as f:
        f.write("import Hayai\n")
        for name in names:
            f.write(f"#print axioms {name}\n")
        path = f.name
    try:
        out = subprocess.run(["lake", "env", "lean", path], cwd=FORMAL, capture_output=True,
                             text=True)
    finally:
        os.unlink(path)
    text = out.stdout + out.stderr
    bad = {}
    for name in names:
        found = re.search(r"'" + re.escape(name) +
                          r"' (depends on axioms: \[[^\]]*\]|does not depend on any axioms)", text)
        if not found:
            bad[name] = "not found (use the full name)"
        elif "sorryAx" in found.group(1):
            bad[name] = "uses sorry"
    if out.returncode != 0 and not bad:
        sys.exit(f"lake env lean failed:\n{text}")
    return bad


def status_of(rule, mapping):
    if (rule["section"], rule["rule"]) in mapping:
        return PROVEN
    if CORE in rule["code"]:
        return IN_CORE
    return OUTSIDE


def short(text, limit=110):
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    return text if len(text) <= limit else text[: limit - 1].rstrip() + "…"


def summary(rules):
    order = []
    counts = {}
    for r in rules:
        if r["section"] not in counts:
            order.append(r["section"])
            counts[r["section"]] = {PROVEN: 0, IN_CORE: 0, OUTSIDE: 0}
        counts[r["section"]][r["status"]] += 1
    total = {s: sum(c[s] for c in counts.values()) for s in (PROVEN, IN_CORE, OUTSIDE)}
    lines = [
        f"**{total[PROVEN]} of {len(rules)} rules proven**; {total[IN_CORE]} more have their "
        f"code in the translated core; {total[OUTSIDE]} are outside the core for now.",
        "",
        "| Section | Rules | ✅ Proven | 🟡 In core | ⬜ Outside |",
        "|---|---:|---:|---:|---:|",
    ]
    for s in order:
        c = counts[s]
        n = sum(c.values())
        lines.append(f"| {s} | {n} | {c[PROVEN] or ''} | {c[IN_CORE] or ''} | {c[OUTSIDE] or ''} |")
    lines.append(f"| **Total** | **{len(rules)}** | **{total[PROVEN]}** | **{total[IN_CORE]}** "
                 f"| **{total[OUTSIDE]}** |")
    return lines


def rows(rules, mapping, wanted=None):
    lines = ["| Section | Rule (line of `docs/consensus.md`) | Status | Theorems |",
             "|---|---|---|---|"]
    for r in rules:
        if wanted and r["status"] not in wanted:
            continue
        theorems = " ".join(f"`{t}`" for t in mapping.get((r["section"], r["rule"]), []))
        link = f"[L{r['line']}]({BLOB}#L{r['line']})"
        lines.append(f"| {r['section']} | {short(r['rule'])} ({link}) | {MARK[r['status']]} "
                     f"| {theorems} |")
    return lines


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--no-lean", action="store_true")
    parser.add_argument("--pr", action="store_true")
    args = parser.parse_args()

    rules = read_rules()
    mapping = read_map()
    known = {(r["section"], r["rule"]) for r in rules}
    missing = [k for k in mapping if k not in known]
    if missing:
        for section, rule in missing:
            print(f"proven.tsv: no rule {rule!r} in section {section!r} of docs/consensus.md",
                  file=sys.stderr)
        sys.exit(1)
    if not args.no_lean:
        bad = check_theorems([t for ts in mapping.values() for t in ts])
        if bad:
            for name, why in sorted(bad.items()):
                print(f"proven.tsv: theorem {name}: {why}", file=sys.stderr)
            sys.exit(1)
    for r in rules:
        r["status"] = status_of(r, mapping)

    if args.pr:
        print("\n".join(summary(rules)))
        print()
        print("**Proven rules**")
        print()
        print("\n".join(rows(rules, mapping, {PROVEN})))
        print()
        print("<details><summary><b>Rules in the core, not yet proven</b> (the next ones)</summary>")
        print()
        print("\n".join(rows(rules, mapping, {IN_CORE})))
        print()
        print("</details>")
        print()
        print("The rules outside the core are in [`formal/PROGRESS.md`]"
              "(https://github.com/zodl-inc/hayai/blob/formal-core/formal/PROGRESS.md), with every row.")
        return

    text = "\n".join([
        "# Progress of the formal verification",
        "",
        "Generated by `formal/scripts/progress.py` from `docs/consensus.md` and",
        "`formal/proven.tsv`; do not edit. A rule is **proven** when Lean theorems show that",
        "the translation of the Rust code of `hayai-consensus-core` decides the rule as",
        "`formal/Hayai/Spec/` states it, and the theorems use no `sorry`. The Spec files are",
        "written from the protocol specification and the ZIPs: they are the part a reader",
        "checks against the documents.",
        "",
        "## Summary",
        "",
        *summary(rules),
        "",
        "## Rules",
        "",
        *rows(rules, mapping),
        "",
    ])
    if args.check:
        try:
            with open(OUT, encoding="utf-8") as f:
                current = f.read()
        except FileNotFoundError:
            current = ""
        if current != text:
            sys.exit("formal/PROGRESS.md is stale: run formal/scripts/progress.py and commit it")
        print("progress: formal/PROGRESS.md is current")
        return
    with open(OUT, "w", encoding="utf-8") as f:
        f.write(text)
    proven = sum(1 for r in rules if r["status"] == PROVEN)
    print(f"progress: {proven} of {len(rules)} rules proven; wrote formal/PROGRESS.md")


if __name__ == "__main__":
    main()
