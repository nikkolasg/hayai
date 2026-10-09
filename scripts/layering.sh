#!/usr/bin/env bash
# The layering of the crates (docs/architecture.md, section Crates). The script fails when:
#
# 1. a crate depends on a crate after it in the dependency order of the architecture;
# 2. a pure crate (rules, encodings, trees: no I/O, no clock, no thread, no lock) names
#    `std::fs`, `std::net`, `std::thread`, a clock or a lock outside its tests;
# 3. an in-memory crate (preparation, state, validation, mempool, template, relay) names
#    `std::fs` or `std::net` outside its tests, or pulls RocksDB or tokio;
# 4. a crate other than the network crates names `std::net`.
#
# The scan of a file stops at its `#[cfg(test)] mod tests` module. Files named `tests.rs`,
# `*_tests.rs`, `test_support.rs` and `test_util.rs` are not scanned. A line of a comment
# does not count.
#
# CI runs the script on each push (.github/workflows/ci.yml, job layering). Run it from any
# directory: it needs cargo and the lock file.

set -euo pipefail
cd "$(dirname "$0")/.."

# The crates in dependency order: no crate depends on a crate after it.
ORDER=(
    hayai-crypto
    hayai-wire
    hayai-template-messages
    hayai-consensus-core
    hayai-consensus
    hayai-sinsemilla
    hayai-trees
    hayai-coins
    hayai-prepared
    hayai-state
    hayai-validate
    hayai-relay
    hayai-template
    hayai-mempool
    hayai-blockstore
    hayai-index
    hayai-sync
    hayai-net
    hayai-http
    hayai-metrics
    hayai-rpc
    hayai-shadow
    hayai-fixtures
    hayai-node
    hayai-bench
)

# No I/O, no clock, no thread, no lock. A rayon map is a pure parallel map and is allowed.
PURE=(hayai-consensus-core hayai-consensus hayai-wire hayai-template-messages hayai-sinsemilla hayai-trees)
# In memory: threads and locks, no file and no socket, no database.
MEMORY=(hayai-prepared hayai-state hayai-validate hayai-mempool hayai-template hayai-relay)
# The crates that open a socket.
NETWORK=(hayai-net hayai-http hayai-metrics hayai-rpc hayai-shadow hayai-node hayaid)

# Files of an in-memory crate that write a file by design.
MEMORY_FILE_EXCEPTIONS=(
    # The base of the chain on disk: `state.log` (docs/architecture.md, hayai-state).
    crates/hayai-state/src/persist.rs
)

failures=0

fail() {
    echo "layering: $1" >&2
    failures=$((failures + 1))
}

# The lines of the non-test sources of a crate: `path:line: text`.
sources() {
    local crate=$1
    find "crates/$crate/src" -name '*.rs' \
        ! -name 'tests.rs' ! -name '*_tests.rs' \
        ! -name 'test_support.rs' ! -name 'test_util.rs' \
        -print0 | sort -z | xargs -0 awk '
            prev ~ /^#\[cfg\(test\)\]/ && $0 ~ /^(pub(\(crate\))? )?mod tests/ { nextfile }
            { prev = $0 }
            $0 !~ /^[[:space:]]*\/\// { print FILENAME ":" FNR ": " $0 }
        '
}

# Fails on each line of the non-test sources of `crate` that has one of the `tokens`.
forbid() {
    local crate=$1
    shift
    local pattern
    pattern=$(IFS='|'; echo "$*")
    local hits
    hits=$(sources "$crate" | grep -E "$pattern" || true)
    if [[ -n "$hits" ]]; then
        fail "$crate names a token outside its layer ($pattern):"
        echo "$hits" >&2
    fi
}

# The crates that `crate` depends on, in the build of CI (the `upstream` backend).
deps() {
    cargo tree -p "$1" -e normal --prefix none --locked \
        --no-default-features --features upstream 2>/dev/null | awk '{ print $1 }' | sort -u
}

contains() {
    local needle=$1
    shift
    local item
    for item in "$@"; do
        [[ "$item" == "$needle" ]] && return 0
    done
    return 1
}

# 1. The dependency order.
for i in "${!ORDER[@]}"; do
    crate=${ORDER[$i]}
    # `${later[@]+...}`: an empty array under `set -u` in bash 3.2 (macOS).
    later=("${ORDER[@]:$((i + 1))}")
    while read -r dep; do
        if contains "$dep" ${later[@]+"${later[@]}"}; then
            fail "$crate depends on $dep, which comes after it in the crate order"
        fi
    done < <(deps "$crate")
done

# 2. The pure crates.
for crate in "${PURE[@]}"; do
    forbid "$crate" 'std::fs' 'std::net' 'std::thread' 'Instant::now' 'SystemTime' \
        'Mutex<' 'RwLock<' 'std::io::std(in|out|err)'
done

# 3. The in-memory crates.
for crate in "${MEMORY[@]}"; do
    hits=$(sources "$crate" | grep -E 'std::fs|std::net' || true)
    for exception in "${MEMORY_FILE_EXCEPTIONS[@]}"; do
        hits=$(echo "$hits" | grep -v "^$exception:" || true)
    done
    if [[ -n "$hits" ]]; then
        fail "$crate does file or network I/O outside its tests:"
        echo "$hits" >&2
    fi
    while read -r dep; do
        case "$dep" in
            librocksdb-sys | rocksdb | tokio)
                fail "$crate pulls $dep" ;;
        esac
    done < <(deps "$crate")
done

# 4. The sockets.
for crate in "${ORDER[@]}"; do
    if ! contains "$crate" "${NETWORK[@]}"; then
        forbid "$crate" 'std::net'
    fi
done

if [[ $failures -gt 0 ]]; then
    echo "layering: $failures failure(s)" >&2
    exit 1
fi
echo "layering: ok"
