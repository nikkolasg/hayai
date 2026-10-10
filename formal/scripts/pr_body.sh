#!/usr/bin/env bash
# Replaces the progress section of a pull request description (between the markers
# `<!-- progress:start -->` and `<!-- progress:end -->`) with the output of
# `formal/scripts/progress.py --pr`.
#
#   formal/scripts/pr_body.sh 10
set -euo pipefail
cd "$(dirname "$0")/../.."
pr=$1
body=$(mktemp)
table=$(mktemp)
gh pr view "$pr" --json body -q .body > "$body"
python3 formal/scripts/progress.py --pr > "$table"
python3 - "$body" "$table" <<'PY'
import sys
body = open(sys.argv[1]).read()
table = open(sys.argv[2]).read()
start, end = "<!-- progress:start -->", "<!-- progress:end -->"
i, j = body.index(start) + len(start), body.index(end)
open(sys.argv[1], "w").write(body[:i] + "\n" + table + "\n" + body[j:])
PY
gh pr edit "$pr" --body-file "$body" > /dev/null
rm "$body" "$table"
echo "pr_body: updated the progress of #$pr"
