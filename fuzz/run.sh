#!/usr/bin/env bash
# Runs every fuzz target (or those named) for SECONDS each, JOBS at a time, from the seeds
# made from the test corpus plus whatever fuzz/corpus already holds. Fails if a target
# crashes; the input that did it is in fuzz/artifacts/<target>/. See fuzz/README.md.
#
#   fuzz/run.sh SECONDS JOBS [TARGET...]
#
# FUZZ_TRIPLE=i686-unknown-linux-gnu builds and runs the targets for that Rust target instead of
# the host's (#498), without AddressSanitizer, which Rust doesn't offer there; overflow checks
# stay on (fuzz/Cargo.toml).
set -u

seconds=${1:?usage: fuzz/run.sh SECONDS JOBS [TARGET...]}
jobs=${2:?usage: fuzz/run.sh SECONDS JOBS [TARGET...]}
shift 2
cd "$(dirname "$0")/.." || exit 2

fuzz_args=()
if [ -n "${FUZZ_TRIPLE:-}" ]; then
    fuzz_args=(--target "$FUZZ_TRIPLE" --sanitizer none)
    # libFuzzer's C++ (cc-rs): the host's gcc and g++, which add -m32 for i686, unless set.
    t=${FUZZ_TRIPLE//-/_}
    export "CC_$t=$(printenv "CC_$t" || echo gcc)" "CXX_$t=$(printenv "CXX_$t" || echo g++)"
fi

# fuzz/Cargo.lock is checked in and is what the build uses: stop, rather than let the build
# rewrite it, when it no longer matches the manifests.
if ! cargo metadata --locked --format-version 1 --manifest-path fuzz/Cargo.toml >/dev/null; then
    echo "fuzz/Cargo.lock doesn't match fuzz/Cargo.toml and ../Cargo.toml: update it (cargo update --workspace --manifest-path fuzz/Cargo.toml) and commit it" >&2
    exit 2
fi
cargo run --locked --quiet --example fuzz_seeds -- fuzz/corpus || exit 2
cargo fuzz build "${fuzz_args[@]}" || exit 2

if [ $# -gt 0 ]; then
    targets=("$@")
else
    mapfile -t targets < <(cargo fuzz list)
fi

mkdir -p fuzz/logs
# Inputs are parts of images and small files; 256 KiB holds a whole TOC region.
# shellcheck disable=SC2329 # called by xargs through bash -c
run_one() {
    local log="fuzz/logs/$1.log"
    # shellcheck disable=SC2086 # the options, one word each
    if cargo fuzz run $FUZZ_ARGS "$1" -- \
        -max_total_time="$seconds" -max_len=262144 -timeout=30 -rss_limit_mb=4096 \
        -print_final_stats=1 >"$log" 2>&1; then
        echo "ok    $1: $(grep -E '^stat::number_of_executed_units' "$log" | awk '{print $2}') runs"
    else
        echo "CRASH $1 (fuzz/logs/$1.log):"
        grep -E -m3 'panicked at|ERROR: |SUMMARY' "$log" | sed 's/^/    /'
        return 1
    fi
}
export -f run_one
export seconds
FUZZ_ARGS="${fuzz_args[*]}"
export FUZZ_ARGS

printf '%s\n' "${targets[@]}" | xargs -P "$jobs" -I{} bash -c 'run_one {}'
status=$?
[ "$status" -eq 0 ] && echo "no crashes" || echo "crashes found"
exit "$status"
