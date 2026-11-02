#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Leak checks off PR CI (#151); see docs/development.md. Run inside the container of
# scripts/leak-check.Containerfile (cargo, valgrind, fusermount3), from the repository root:
#
#   podman run --rm --device /dev/fuse --cap-add SYS_ADMIN -v "$PWD":/src:z -w /src \
#       sacdfs-leak scripts/leak-check.sh memcheck
#
#   memcheck            every test binary under valgrind memcheck, one test at a time,
#                       following the sacdfs processes the FUSE tests start. Fails on any
#                       definite or indirect leak, or on a memory error.
#   massif IMAGES MIN   a soak segment of MIN minutes (scripts/soak.py on the .iso files in
#                       IMAGES) with the mounted sacdfs under valgrind massif; prints the heap
#                       over time, the allocation sites still live at the end, and the growth
#                       between the first snapshot after the warm-up and the last.
#
# Output goes to $OUT (default target/leak-check).
set -euo pipefail

OUT=${OUT:-target/leak-check}
mkdir -p "$OUT"
export SACDFS_FUSE_TESTS=1

memcheck() {
    # The test binaries (the test profile: optimised, with debug info for the stacks).
    rm -f "$OUT"/*.log "$OUT"/*.out
    cargo test --locked --no-run --message-format=json 2>/dev/null \
        | python3 -I -c 'import json,sys
for l in sys.stdin:
    m = json.loads(l)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]:
        print(m["executable"])' >"$OUT/tests.txt"
    # No --error-exitcode: it would change the exit status of the sacdfs processes the tests
    # start and check. The verdict comes from the logs instead.
    local vg=(valgrind --leak-check=full --show-leak-kinds=definite,indirect
        --errors-for-leak-kinds=definite,indirect --trace-children=yes
        "--trace-children-skip=*fusermount3,*/kill,*/sh,*/bash,*/python3"
        --suppressions=scripts/leak-check.supp --num-callers=30)
    local fail=0
    while read -r bin; do
        name=$(basename "$bin")
        echo "== $name"
        # Each test on its own: valgrind serialises threads, and a FUSE test waiting on a
        # mount must not wait behind another test's decoding.
        # A test may fail under valgrind for its timing (it runs some 30 times slower): noted,
        # but the verdict is valgrind's.
        if ! "${vg[@]}" "--log-file=$OUT/$name.%p.log" "$bin" --test-threads=1 >"$OUT/$name.out" 2>&1; then
            echo "   (some tests failed under valgrind, see $OUT/$name.out)"
        fi
        if grep -l "definitely lost: [1-9]\|indirectly lost: [1-9]\|ERROR SUMMARY: [1-9]" "$OUT/$name".*.log; then
            echo "   FAIL: leaks or memory errors in the logs above"
            fail=1
        fi
    done <"$OUT/tests.txt"
    [ $fail = 0 ] && echo "memcheck: no leaks or memory errors"
    return $fail
}

massif() {
    local images=$1 minutes=$2
    cargo build --locked --release
    local hours
    hours=$(python3 -I -c "print($minutes / 60)")
    # The watchdog's timeout is longer under valgrind, which runs the decoder ~30x slower.
    python3 -I scripts/soak.py --sacdfs target/release/sacdfs --images "$images" --hours "$hours" \
        --interval 30 --warmup 120 --readers 2 --read-pause 0.5 --request-timeout 60 \
        --stall-every 240 --remount-every 300 --out "$OUT/massif-soak.tsv" \
        --wrap "valgrind --tool=massif --threshold=0.5 --detailed-freq=4 --max-snapshots=200 --time-unit=ms --massif-out-file=$OUT/massif.out" \
        || true  # the soak's verdict does not apply under valgrind
    ms_print "$OUT/massif.out" >"$OUT/massif.txt"
    python3 -I scripts/massif-summary.py "$OUT/massif.out"
}

case ${1:-} in
    memcheck) memcheck ;;
    massif) massif "${2:?IMAGES}" "${3:?MINUTES}" ;;
    *) sed -n '3,20p' "$0"; exit 2 ;;
esac
