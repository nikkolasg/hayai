#!/usr/bin/env python3
"""Points the Code and Test columns of docs/consensus.md at the current tree.

Each link of the table names a file and a line of one commit. The script reads the text of
that line in that commit (`git show <commit>:<path>`), finds the same text in the current
tree (the same file, the file that the crate split moved it to, or any file of `crates/`
when the text is unique there), and rewrites the link to `<base>/<new path>#L<new line>`.

A link whose line the script does not find is left as it is and listed: the rule moved
with other text, or its code is gone. Each one needs a reader.

    scripts/consensus_links.py [--base https://github.com/zodl-inc/hayai/blob/main] [--write]

Without `--write` the script reports and changes nothing.
"""

import argparse
import os
import re
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOC = os.path.join(REPO, "docs", "consensus.md")
LINK = re.compile(
    r"\[`([^`]+)`\]\((https://github\.com/[\w-]+/hayai/blob/([0-9a-f]{7,40}|main)/([^#)]+)#L(\d+))\)"
)
# The relay policy (M3) takes `io` in each handler: the text of a line is the same without it.
IO_PARAM = re.compile(r"\bio: &dyn Io, |\(io\)|\bio, ")
DEF = "(?:pub(?:\\([a-z]+\\))? )?(?:const |static |fn |struct |enum |trait |type |mod )(?:mut )?{name}\\b"

# The files that the crate split moved, oldest path first. A path maps to each candidate
# in order; the first candidate with the line wins.
MOVED = {
    "crates/hayaid/src/persist.rs": ["crates/hayai-state/src/persist.rs"],
    "crates/hayaid/src/headers.rs": [
        "crates/hayai-node/src/headers.rs",
        "crates/hayai-sync/src/index.rs",
        "crates/hayai-relay/src/header_check.rs",
    ],
    "crates/hayaid/src/upstream.rs": ["crates/hayai-shadow/src/upstream.rs"],
    "crates/hayaid/src/shadow.rs": ["crates/hayai-shadow/src/lib.rs"],
    "crates/hayaid/src/backing.rs": ["crates/hayai-shadow/src/backing.rs"],
    "crates/hayaid/src/shadow_tests.rs": [
        "crates/hayai-node/src/shadow_tests.rs",
        "crates/hayai-shadow/src/tests.rs",
        "crates/hayai-shadow/src/test_support.rs",
    ],
    "crates/hayai-net/src/relay.rs": [
        "crates/hayai-net/src/policy.rs",
        "crates/hayai-net/src/relay.rs",
    ],
    "crates/hayai-prepared/src/store.rs": ["crates/hayai-mempool/src/store.rs"],
    "crates/hayai-prepared/src/policy.rs": ["crates/hayai-mempool/src/policy.rs"],
    "crates/hayai-prepared/src/test_support.rs": ["crates/hayai-mempool/src/test_support.rs"],
    "crates/hayai-bench/src/fixtures.rs": ["crates/hayai-fixtures/src/lib.rs"],
    "crates/hayai-bench/src/regtest.rs": ["crates/hayai-fixtures/src/regtest.rs"],
    "crates/hayai-rpc/src/metrics.rs": ["crates/hayai-metrics/src/lib.rs"],
    "crates/hayai-rpc/src/cookie.rs": ["crates/hayai-http/src/cookie.rs"],
    "crates/hayai-rpc/src/http.rs": ["crates/hayai-rpc/src/http.rs", "crates/hayai-http/src/lib.rs"],
    "crates/hayai-template/src/messages.rs": ["crates/hayai-template-messages/src/lib.rs"],
    "crates/hayai-consensus/src/coinbase.rs": [
        "crates/hayai-consensus/src/coinbase.rs",
        "crates/hayai-consensus-core/src/coinbase_value.rs",
    ],
}

# Links whose line and symbol both changed on the branch: the rule moved into another
# function. Each entry names the new file, the pattern of its line, and the new label
# (`None` keeps the label of the table).
CORE = "crates/hayai-consensus-core/src/"
CONSENSUS = "crates/hayai-consensus/src/"
OVERRIDES = {
    (CONSENSUS + "network.rs", 1009): (
        CONSENSUS + "rules.rs",
        r"fn every_upgrade_with_a_branch_id_has_one_rule_set_with_its_branch",
        "every_upgrade_with_a_branch_id_has_one_rule_set_with_its_branch",
    ),
    (CONSENSUS + "network.rs", 646): (CORE + "spec.rs", r"SpecError::Order", "SpecError::Order"),
    ("crates/hayaid/src/mempool.rs", 240): (
        "crates/hayai-mempool/src/admission.rs",
        r"^\s*(let|match|if)\b.*rules_at\(",
        "rules_at",
    ),
    (CONSENSUS + "difficulty.rs", 154): (
        CORE + "difficulty.rs",
        r"match spec\.min_difficulty_start_height",
        "min_difficulty_start_height",
    ),
    (CONSENSUS + "difficulty.rs", 159): (CORE + "difficulty.rs", r"\.min_difficulty_gap_spacings", None),
    (CONSENSUS + "funding.rs", 338): (
        CORE + "funding.rs",
        r"if set\.start <= height && height < set_end",
        "set_end",
    ),
    (CONSENSUS + "coinbase.rs", 164): (
        CORE + "coinbase_value.rs",
        r"founders::founders_reward\(spec, height\)",
        None,
    ),
    (CONSENSUS + "coinbase.rs", 170): (
        CORE + "coinbase_value.rs",
        r"funding::funding_streams\(spec, height, total\)",
        None,
    ),
    (CONSENSUS + "difficulty.rs", 142): (
        CORE + "rules.rs",
        r"target_spacing: PRE_BLOSSOM_TARGET_SPACING",
        "DifficultyParams::target_spacing",
    ),
    (CONSENSUS + "subsidy.rs", 198): (CORE + "subsidy.rs", r"^pub fn block_subsidy\(", "block_subsidy"),
    (CONSENSUS + "founders.rs", 54): (CORE + "founders.rs", r"subsidy::halving\(spec, height\)", None),
    (CONSENSUS + "funding.rs", 327): (
        CORE + "funding.rs",
        r"let canopy_active = match spec\.activation_height\(Upgrade::Canopy\)",
        "canopy_active",
    ),
    (CONSENSUS + "network.rs", 639): (CONSENSUS + "network.rs", r"\(Upgrade::Nu7, 4_465_026\)", "TESTNET_HEIGHTS"),
    (CONSENSUS + "network.rs", 651): (CONSENSUS + "network.rs", r"\(Upgrade::Nu7, 4_465_026\)", "TESTNET_HEIGHTS"),
    (CONSENSUS + "subsidy.rs", 101): (CORE + "subsidy.rs", r"let Some\(interval_seconds\)", None),
    (CONSENSUS + "coinbase.rs", 137): (CORE + "coinbase_value.rs", r"nsm::reissuance_active\(spec, height\)", None),
    (CONSENSUS + "coinbase.rs", 141): (CORE + "coinbase_value.rs", r"nsm::reissuance_bonus\(", None),
    (CONSENSUS + "rules.rs", 300): (CORE + "rules.rs", r"^const NU7: RuleSet", "NU7"),
    (CONSENSUS + "lockbox.rs", 56): (CONSENSUS + "lockbox.rs", r"^const MAINNET_ADDRESS", "MAINNET_ADDRESS"),
    (CONSENSUS + "lockbox.rs", 57): (CONSENSUS + "lockbox.rs", r"^const TESTNET_ADDRESS", "TESTNET_ADDRESS"),
    (CONSENSUS + "coinbase.rs", 267): (CORE + "coinbase_value.rs", r"matched\[index\] = true", "CoinbaseTerms::check"),
    (CONSENSUS + "rules.rs", 361): (CORE + "rules.rs", r"if spec\.orchard_disabled\(height\)", "CoreSpec::orchard_disabled"),
    (CONSENSUS + "header.rs", 152): (
        CORE + "header.rs",
        r"spec\.max_time_start_height && header\.time > limit",
        None,
    ),
    (CONSENSUS + "difficulty.rs", 194): (
        CORE + "difficulty.rs",
        r"let target = match scaled\.checked_mul_u64\(timespan\)",
        "Uint256::checked_mul_u64",
    ),
    (CONSENSUS + "founders.rs", 61): (CORE + "founders.rs", r"let change_interval = ", None),
    (CONSENSUS + "founders.rs", 65): (
        CORE + "founders.rs",
        r"let adjusted_height = match spec\.activation_height\(Upgrade::Blossom\)",
        None,
    ),
    (CONSENSUS + "funding.rs", 269): (
        CONSENSUS + "network.rs",
        r"=> ChainSpecError::StreamAddresses",
        "ChainSpecError::StreamAddresses",
    ),
    (CONSENSUS + "coinbase.rs", 355): (CONSENSUS + "address.rs", r"^const OP_HASH160", "OP_HASH160"),
    (CONSENSUS + "funding.rs", 214): (CORE + "funding.rs", r"^pub fn address_period\(", "address_period"),
    (CONSENSUS + "coinbase.rs", 271): (CORE + "coinbase_value.rs", r"if paid != payable \{", "CoinbaseTerms::check"),
    (CONSENSUS + "coinbase.rs", 177): (
        CORE + "coinbase_value.rs",
        r"terms\.subsidy\.deferred = add_money\(",
        "Subsidy::deferred",
    ),
    (CONSENSUS + "network.rs", 652): (CONSENSUS + "network.rs", r"^const MAINNET_HEIGHTS", "MAINNET_HEIGHTS"),
    (CONSENSUS + "founders.rs", 76): (
        CORE + "founders.rs",
        r"adjusted_height\.checked_div\(change_interval\)",
        "founders_reward",
    ),
    (CONSENSUS + "header.rs", 175): (CORE + "header.rs", r"match expected_bits\(spec, rules, header\.time, chain\)", None),
    (CONSENSUS + "header.rs", 139): (CORE + "header.rs", r"let median = match median_time_past\(chain\.times\)", None),
    (CONSENSUS + "difficulty.rs", 164): (
        CORE + "difficulty.rs",
        r"if height <= params\.averaging_window \{",
        "DifficultyParams::averaging_window",
    ),
    (CONSENSUS + "coinbase.rs", 369): (CONSENSUS + "address.rs", r"fn address_script\(", "address_script"),
    (CONSENSUS + "coinbase.rs", 184): (
        CORE + "coinbase_value.rs",
        r"let disbursements = lockbox::disbursements\(spec, height\);",
        None,
    ),
}


def candidates(path):
    out = []
    if path in MOVED:
        out.extend(MOVED[path])
    if path.startswith("crates/hayaid/src/"):
        out.append(path.replace("crates/hayaid/src/", "crates/hayai-node/src/", 1))
    if path.startswith("crates/hayai-consensus/src/"):
        out.append(path.replace("crates/hayai-consensus/src/", "crates/hayai-consensus-core/src/", 1))
    out.append(path)
    seen = []
    for c in out:
        if c not in seen and os.path.isfile(os.path.join(REPO, c)):
            seen.append(c)
    return seen


_old_files = {}


def old_line(commit, path, n):
    key = (commit, path)
    if key not in _old_files:
        try:
            text = subprocess.run(
                ["git", "show", f"{commit}:{path}"], cwd=REPO, check=True, capture_output=True, text=True
            ).stdout
            _old_files[key] = text.split("\n")
        except subprocess.CalledProcessError:
            _old_files[key] = None
    lines = _old_files[key]
    if lines is None or n < 1 or n > len(lines):
        return None
    return lines[n - 1].strip()


_new_files = {}


def new_lines(path):
    if path not in _new_files:
        with open(os.path.join(REPO, path), encoding="utf-8") as f:
            _new_files[path] = [l.strip() for l in f.read().split("\n")]
    return _new_files[path]


def all_sources():
    for root, _, files in os.walk(os.path.join(REPO, "crates")):
        if "/target" in root:
            continue
        for f in files:
            if f.endswith(".rs"):
                yield os.path.relpath(os.path.join(root, f), REPO)


def find_in(path, text, near):
    """The 1-based lines of `path` whose text is `text`, the one nearest to `near` first.
    A line of the relay policy matches without its `io` argument."""
    hits = [i + 1 for i, l in enumerate(new_lines(path)) if l == text or IO_PARAM.sub("", l) == text]
    hits.sort(key=lambda i: abs(i - near))
    return hits


def find_pattern(path, pattern):
    """The first 1-based line of `path` that matches `pattern`."""
    rx = re.compile(pattern)
    for i, l in enumerate(new_lines(path)):
        if rx.search(l):
            return i + 1
    return None


def find_def(path, name, near):
    """The lines of `path` that define `name` (an item, an enum variant or a field)."""
    item = re.compile(DEF.format(name=re.escape(name)))
    member = re.compile(r"^(?:pub )?" + re.escape(name) + r"\b\s*[:,{(]")
    hits = [i + 1 for i, l in enumerate(new_lines(path)) if item.search(l) or member.match(l)]
    hits.sort(key=lambda i: abs(i - near))
    return hits


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="https://github.com/zodl-inc/hayai/blob/main")
    ap.add_argument("--write", action="store_true")
    ap.add_argument("--verbose", action="store_true", help="list each ambiguous and symbol-resolved link")
    args = ap.parse_args()
    with open(DOC, encoding="utf-8") as f:
        doc = f.read()
    stats = {"same": 0, "moved": 0, "global": 0, "symbol": 0, "override": 0, "unresolved": 0, "ambiguous": 0}
    unresolved = []
    sources = None

    def rewrite(m):
        nonlocal sources
        label, commit, path, n = m.group(1), m.group(3), m.group(4), int(m.group(5))
        if commit == "main":
            return m.group(0)
        link = lambda p, i: f"[`{label}`]({args.base}/{p}#L{i})"
        if (path, n) in OVERRIDES:
            new_path, pattern, new_label = OVERRIDES[(path, n)]
            i = find_pattern(new_path, pattern)
            if i is None:
                stats["unresolved"] += 1
                unresolved.append((path, n, f"override pattern not found in {new_path}: {pattern}"))
                return m.group(0)
            stats["override"] += 1
            label = new_label or label
            return link(new_path, i)
        text = old_line(commit, path, n)
        if text is None:
            stats["unresolved"] += 1
            unresolved.append((path, n, "(no such line in the commit)"))
            return m.group(0)
        for cand in candidates(path):
            hits = find_in(cand, text, n)
            if hits:
                if len(hits) > 1:
                    stats["ambiguous"] += 1
                    if args.verbose:
                        print(f"  ambiguous `{label}` {path}#L{n} -> {cand}#L{hits[0]} of {hits}: {text}")
                stats["same" if cand == path else "moved"] += 1
                return link(cand, hits[0])
        # A unique line somewhere in the crates, when the text says something.
        if len(text) >= 16:
            if sources is None:
                sources = list(all_sources())
            found = [(p, i) for p in sources for i in find_in(p, text, n)]
            if len(found) == 1:
                stats["global"] += 1
                p, i = found[0]
                return link(p, i)
        # The symbol of the label, by its definition: in the candidates, then anywhere when
        # the definition is unique.
        name = label.split("::")[-1].rstrip("()")
        if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name):
            for cand in candidates(path):
                hits = find_def(cand, name, n)
                if hits:
                    stats["symbol"] += 1
                    if args.verbose:
                        print(f"  symbol `{label}` {path}#L{n} -> {cand}#L{hits[0]}: {new_lines(cand)[hits[0] - 1][:90]}")
                    return link(cand, hits[0])
            if sources is None:
                sources = list(all_sources())
            found = [(p, i) for p in sources for i in find_def(p, name, n)]
            if len(found) == 1:
                stats["symbol"] += 1
                if args.verbose:
                    print(f"  symbol `{label}` {path}#L{n} -> {found[0][0]}#L{found[0][1]}")
                return link(*found[0])
        stats["unresolved"] += 1
        unresolved.append((path, n, f"`{label}`: {text}"))
        return m.group(0)

    new_doc = LINK.sub(rewrite, doc)

    # A link to a whole file (a vector), without a line: the same file in the tree.
    def rewrite_file(m):
        path = m.group(2)
        if m.group(1) == "main" or not os.path.isfile(os.path.join(REPO, path)):
            return m.group(0)
        return f"({args.base}/{path})"

    new_doc = re.sub(
        r"\(https://github\.com/[\w-]+/hayai/blob/([0-9a-f]{7,40}|main)/([^#)]+)\)", rewrite_file, new_doc
    )
    print(
        f"links: same file {stats['same']}, moved file {stats['moved']}, found elsewhere "
        f"{stats['global']}, by the symbol of the label {stats['symbol']}, by an override "
        f"{stats['override']}, ambiguous (nearest "
        f"line taken) {stats['ambiguous']}, "
        f"unresolved {stats['unresolved']}"
    )
    for path, n, text in unresolved:
        print(f"  unresolved {path}#L{n}: {text}")
    if args.write:
        with open(DOC, "w", encoding="utf-8") as f:
            f.write(new_doc)
        print("written")
    return 1 if unresolved else 0


if __name__ == "__main__":
    sys.exit(main())
