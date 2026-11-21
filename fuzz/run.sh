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
    # 64-bit off_t, rlim_t and time_t, as the release's i686 musl build has: sacdfs doesn't build
    # against 32-bit glibc's 32-bit ones. cargo fuzz adds RUSTFLAGS to its own.
    # shellcheck disable=SC2089 # the quotes are part of the flag
    fuzz_rustflags="${RUSTFLAGS:-} --cfg=libc_unstable_gnu_time_bits=\"64\""
fi
fuzz() { RUSTFLAGS=${fuzz_rustflags:-${RUSTFLAGS:-}} cargo fuzz "$@"; }

cargo run --locked --quiet --example fuzz_seeds -- fuzz/corpus || exit 2
fuzz build "${fuzz_args[@]}" || exit 2

if [ $# -gt 0 ]; then
    targets=("$@")
else
    mapfile -t targets < <(fuzz list)
fi

mkdir -p fuzz/logs
# Inputs are parts of images and small files; 256 KiB holds a whole TOC region.
run_one() {
    local log="fuzz/logs/$1.log"
    # shellcheck disable=SC2086 # the options, one word each
    if fuzz run $FUZZ_ARGS "$1" -- \
        -max_total_time="$seconds" -max_len=262144 -timeout=30 -rss_limit_mb=4096 \
        -print_final_stats=1 >"$log" 2>&1; then
        echo "ok    $1: $(grep -E '^stat::number_of_executed_units' "$log" | awk '{print $2}') runs"
    else
        echo "CRASH $1 (fuzz/logs/$1.log):"
        grep -E -m3 'panicked at|ERROR: |SUMMARY' "$log" | sed 's/^/    /'
        return 1
    fi
}
export -f run_one fuzz
# shellcheck disable=SC2090
export fuzz_rustflags
export seconds
FUZZ_ARGS="${fuzz_args[*]}"
export FUZZ_ARGS

printf '%s\n' "${targets[@]}" | xargs -P "$jobs" -I{} bash -c 'run_one {}'
status=$?
[ "$status" -eq 0 ] && echo "no crashes" || echo "crashes found"
exit "$status"
