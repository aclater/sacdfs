#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Players on the same host read the mount directly (#186, docs/deployment.md, "Players on the
# same host"). The server binds an empty host directory over /srv with rshared propagation; a
# reader container, as an unprivileged user, binds the same directory read-only with rslave and
# checks every DSF track of the corpus under /music/sacd. Then the server restarts, and then
# sacdfs in it is killed (the restart policy starts the container again): each time the reader
# must see the tree again by itself, without being restarted.
#
#   deploy/ci-same-host-test.sh IMAGE
#
# Run from the repository root by deploy/ci-share-test.sh, which has built the client image,
# on a host whose root mount has shared propagation (systemd's default). RUNTIME: the container
# command, "docker" by default, or "sudo podman" (rootful: rootless can't share the mount).
set -euo pipefail

image=$1
read -r -a rt <<<"${RUNTIME:-docker}"
export RUNTIME="${rt[*]}"
client_image=${CLIENT:-sacdfs-client:ci}
here=$(cd "$(dirname "$0")" && pwd)
srv=sacdfs-same-host
reader=sacdfs-same-host-reader
dir=$(mktemp -d)
chmod 755 "$dir"

cleanup() {
    local rc=$?
    set +e
    "${rt[@]}" rm -f "$reader" >/dev/null 2>&1
    if [ "$rc" != 0 ]; then "${rt[@]}" logs "$srv" 2>&1 | grep -v ' INFO  sacdfs::' | tail -n 60; fi
    "${rt[@]}" rm -f "$srv" >/dev/null 2>&1
    # A killed sacdfs leaves a dead mount on the host until the next start clears it.
    if mountpoint -q "$dir/sacd" 2>/dev/null || grep -q " $dir/sacd " /proc/self/mountinfo; then
        sudo umount -l "$dir/sacd"
    fi
    sudo rm -rf "$dir"
    exit "$rc"
}
trap cleanup EXIT

"${rt[@]}" run -d --security-opt label=disable --name "$srv" --restart unless-stopped \
    --device /dev/fuse --cap-add SYS_ADMIN \
    --security-opt apparmor=unconfined \
    -e SMB=0 -e NFS=0 -e SACDFS_FORMATS=dsf \
    -v "$PWD/tests/corpus/images:/images:ro" -v "$dir:/srv:rshared" "$image" >/dev/null
"$here/ci-wait-healthy.sh" "$srv"
# The entry point made the mount point in the empty directory, and the mount is on the host.
grep -q " $dir/sacd .* - fuse sacdfs " /proc/self/mountinfo || { echo "the mount is not on the host"; exit 1; }

"${rt[@]}" run -d --security-opt label=disable --name "$reader" --user 1000:1000 \
    -v "$dir:/music:ro,rslave" -v "$PWD:/repo:ro" "$client_image" sleep infinity >/dev/null

check() {
    # The tree comes back by itself within 30 s; then every track is read and checked.
    timeout 30 bash -c "until ${rt[*]} exec $reader test -d /music/sacd/dst-features-2ch-6ch; do sleep 0.2; done" ||
        { echo "$1: the reader does not see the tree"; "${rt[@]}" exec "$reader" ls -la /music/sacd || true; exit 1; }
    "${rt[@]}" exec "$reader" python3 /repo/deploy/check-share.py /repo/tests/corpus/manifest.json /music/sacd --min 400
    echo "same host, $1: ok"
}

check start
"${rt[@]}" restart -t 20 "$srv" >/dev/null
"$here/ci-wait-healthy.sh" "$srv"
check restart
restarts=$("${rt[@]}" inspect -f '{{.RestartCount}}' "$srv")
"${rt[@]}" exec "$srv" pkill -KILL -x sacdfs
# The entry point stops and the restart policy starts the container again.
timeout 60 bash -c "until [ \"\$(${rt[*]} inspect -f '{{.RestartCount}} {{.State.Status}}' $srv)\" = \"$((restarts + 1)) running\" ]; do sleep 0.2; done" ||
    { echo "the container was not started again"; exit 1; }
"$here/ci-wait-healthy.sh" "$srv"
check "sacdfs killed"
echo "same-host test passed: $image"
