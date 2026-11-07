#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Runs the three real-disc tests (#220) on a directory of real SACD images:
#
#     scripts/real-discs.sh DIR
#
# DIR holds `.iso` images of SACDs only (searched recursively, links followed), at least one.
# Real images can't go into CI, so this is a step of the release checklist
# (docs/test-plan.md). Exit status:
#   0   all three tests ran and passed
#   77  skipped: no directory given (the automake convention for a skip: not a pass)
#   1   a test failed, or fewer than three ran
# SACDFS_PCM_RATE=88200 (or 352800) picks the WAV rate; CARGO_FLAGS adds cargo options.
set -u
cd "$(dirname "$0")/.."
dir="${1:-${SACDFS_REAL_IMAGES:-}}"
if [ -z "$dir" ]; then
    echo "SKIPPED real-discs: no image directory (usage: scripts/real-discs.sh DIR)" >&2
    exit 77
fi
if [ ! -d "$dir" ]; then
    echo "real-discs: $dir is not a directory" >&2
    exit 1
fi
out="$(mktemp "${TMPDIR:-/tmp}/real-discs.XXXXXX")"
trap 'rm -f "$out"' EXIT
# SACDFS_REAL_TESTS=1: a test that finds no input fails instead of skipping.
# shellcheck disable=SC2086
SACDFS_REAL_IMAGES="$dir" SACDFS_REAL_TESTS=1 cargo test --locked --release ${CARGO_FLAGS:-} \
    --test dsf_header --test library_index --test wav \
    real_discs -- --ignored --nocapture --test-threads=1 2>&1 | tee "$out"
status=${PIPESTATUS[0]}
# --nocapture interleaves a test's output with libtest's "ok", so count the result lines instead.
passed=$(grep -cE '^test result: ok\. 1 passed; 0 failed; 0 ignored;' "$out")
skipped=$(grep -c 'SKIPPED' "$out")
echo "real-discs: $passed of 3 tests passed, $skipped skipped, cargo exit $status"
if [ "$status" -ne 0 ] || [ "$passed" -ne 3 ] || [ "$skipped" -ne 0 ]; then
    exit 1
fi
