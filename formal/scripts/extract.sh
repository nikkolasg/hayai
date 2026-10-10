#!/usr/bin/env bash
# Regenerates the Lean translation of `hayai-consensus-core` (`formal/Hayai/Core/`):
#
#   charon cargo   Rust -> LLBC (the crate as rustc sees it, after the Charon passes)
#   aeneas         LLBC -> Lean (`formal/Hayai/Core/{Types,Funs,FunsExternal_Template}.lean`)
#
# The translation is committed. A reader of the proofs needs neither tool: `lake build` in
# `formal/` checks the proofs against the committed translation. The CI job `formal`
# regenerates the translation and fails when it differs from the committed one.
#
# The translation starts from the `pub` items. The `Display`, `Debug` and `Error` methods
# (the `thiserror` derives) are not translated: no rule reads them, and their bodies are
# outside the subset of Aeneas.
#
# Tools: `charon` and `aeneas` on PATH, at the versions of `formal/TOOLCHAIN` (the Charon
# commit is the `charon-pin` of the Aeneas commit). Charon brings its own rustc nightly
# through rustup.
#
#   formal/scripts/extract.sh            # regenerate
#   formal/scripts/extract.sh --check    # regenerate into a scratch directory and diff

set -euo pipefail
cd "$(dirname "$0")/../.."

CRATE=hayai-consensus-core
DEST=formal/Hayai/Core
ROOT=formal
LLBC=formal/target/${CRATE}.llbc
CHARON=${CHARON:-charon}
AENEAS=${AENEAS:-aeneas}

check=0
if [[ "${1:-}" == "--check" ]]; then
    check=1
fi

mkdir -p formal/target
"$CHARON" cargo --dest-file "$LLBC" --preset=aeneas --start-from-pub \
    --exclude=core::fmt::Display::fmt --exclude=core::fmt::Debug::fmt \
    --exclude='{core::error::Error<_>}::source' --opaque=core::fmt::Formatter -- \
    -p "$CRATE" --no-default-features --features upstream

out=$ROOT
if [[ $check -eq 1 ]]; then
    out=$(mktemp -d)
fi
mkdir -p "$out/Hayai/Core"
# `Hayai/Core/FunsExternal.lean` is written by hand and stays; the three files below are
# generated. `-split-files` puts the external functions in a template, so that the hand
# written file can define them.
"$AENEAS" "$LLBC" -backend lean -dest "$out" -subdir Hayai/Core -namespace HayaiCore \
    -split-files -abort-on-error -no-progress-bar

if [[ $check -eq 1 ]]; then
    status=0
    for file in Types.lean Funs.lean FunsExternal_Template.lean; do
        if ! diff "$out/Hayai/Core/$file" "$DEST/$file"; then
            echo "extract: $DEST/$file differs from the code" >&2
            status=1
        fi
    done
    rm -r "$out"
    if [[ $status -ne 0 ]]; then
        echo "extract: run formal/scripts/extract.sh and commit the result" >&2
        exit 1
    fi
    echo "extract: the committed translation is current"
fi
