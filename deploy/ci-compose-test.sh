#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# deploy/nas-compose.yaml works (#283): the file is valid Compose, comes up healthy with the
# image under test, and stops cleanly and fast, also when stopped in the first seconds after the
# start (before the fix in the entry point, the stop then took 10 s one time in six and Podman
# killed the container).
#
#   deploy/ci-compose-test.sh IMAGE
#
# Run from the repository root, with the corpus in tests/corpus/images. The published image named
# in the file is replaced by IMAGE and the images folder by the corpus, in a second file.
# COMPOSE: the compose command, "docker compose" by default; RUNTIME: the container command, "docker".
set -euo pipefail

image=$1
read -r -a compose <<<"${COMPOSE:-docker compose}"
read -r -a rt <<<"${RUNTIME:-docker}"
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
override=$work/override.yaml
cat >"$override" <<YAML
services:
  sacdfs:
    image: $image
    volumes:
      - $PWD/tests/corpus/images:/images:ro
      - sacdfs-cache:/cache
YAML
# SELinux host (to run this by hand on Fedora): LABEL_DISABLE=1 adds label=disable.
if [ -n "${LABEL_DISABLE:-}" ]; then
    printf '    security_opt:\n      - apparmor=unconfined\n      - label=disable\n' >>"$override"
fi
dc() { "${compose[@]}" -p sacdfs-nas -f "$here/nas-compose.yaml" -f "$override" "$@"; }
cleanup() {
    local rc=$?
    set +e
    if [ "$rc" != 0 ]; then dc logs 2>&1 | grep -v ' INFO  sacdfs::' | tail -n 60; fi
    dc down -v >/dev/null 2>&1
    rm -rf "$work"
    exit "$rc"
}
trap cleanup EXIT

dc config -q
echo "nas-compose.yaml: valid"
dc up -d >/dev/null
RUNTIME="${rt[*]}" "$here/ci-wait-healthy.sh" sacdfs
# The published ports answer.
timeout 5 bash -c '</dev/tcp/127.0.0.1/10445' && timeout 5 bash -c '</dev/tcp/127.0.0.1/12049'
echo "ports 10445 and 12049: open"

# Stop 2 s after each start: the case that used to wait 10 s. Each stop must exit normally
# (status 0) in under 8 s.
dc stop >/dev/null
for i in 1 2 3 4 5 6 7 8; do
    dc up -d --force-recreate >/dev/null
    sleep 2
    start=$(date +%s.%N)
    dc stop >/dev/null
    took=$(echo "$(date +%s.%N) - $start" | bc)
    code=$("${rt[@]}" inspect -f '{{.State.ExitCode}}' sacdfs)
    echo "stop $i, 2 s after the start: $took s, exit status $code"
    [ "$code" = 0 ]
    [ "$(echo "$took < 8" | bc)" = 1 ]
done
