#!/usr/bin/env bash
# Runs every fuzz target (or those named) for SECONDS each, JOBS at a time, from the seeds
# made from the test corpus plus whatever fuzz/corpus already holds. Fails if a target
# crashes; the input that did it is in fuzz/artifacts/<target>/. See fuzz/README.md.
#
#   fuzz/run.sh SECONDS JOBS [TARGET...]
set -u

seconds=${1:?usage: fuzz/run.sh SECONDS JOBS [TARGET...]}
jobs=${2:?usage: fuzz/run.sh SECONDS JOBS [TARGET...]}
shift 2
cd "$(dirname "$0")/.." || exit 2

cargo run --locked --quiet --example fuzz_seeds -- fuzz/corpus || exit 2
cargo fuzz build || exit 2

if [ $# -gt 0 ]; then
    targets=("$@")
else
    mapfile -t targets < <(cargo fuzz list)
fi

mkdir -p fuzz/logs
# Inputs are parts of images and small files; 256 KiB holds a whole TOC region.
run_one() {
    local log="fuzz/logs/$1.log"
    if cargo fuzz run "$1" -- \
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

printf '%s\n' "${targets[@]}" | xargs -P "$jobs" -I{} bash -c 'run_one {}'
status=$?
[ "$status" -eq 0 ] && echo "no crashes" || echo "crashes found"
exit "$status"
