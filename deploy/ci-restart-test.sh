#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Clients keep their files across a container restart (#218). A client mounts the shares over
# SMB and NFSv4.2, opens a file and reads half of it, the container restarts, and the client
# must read the rest through the same open file and still stat and list by path. Two rounds:
#
#   1. a plain restart, what the user runs;
#   2. stop, take the mount's device number away, start. A FUSE mount's device number is the
#      lowest free one, so whether a restart changes it depends on what else mounted in
#      between, and on the runtime's version: a plain restart may keep it. This round changes
#      it on purpose, and fails if it did not.
#
#   deploy/ci-restart-test.sh RUNTIME IMAGE     # RUNTIME: "docker" or "sudo podman"
#
# Run from the repository root, by deploy/ci-share-test.sh, which has built the client image.
# DOCKER: the command that runs the client container, "docker" by default.
set -euo pipefail

read -r -a rt <<<"$1"
image=$2
read -r -a dk <<<"${DOCKER:-docker}"
client_image=${CLIENT:-sacdfs-client:ci}
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=deploy/ci-caps.sh
. "$here/ci-caps.sh"
srv=sacdfs-restart
cli=sacdfs-restart-client
# Not the same ports for both runtimes: Podman refuses a port with TIME_WAIT sockets left on it.
case "$1" in
    *podman*) nfs_port=12050 smb_port=10446 ;;
    *) nfs_port=12049 smb_port=10445 ;;
esac
held=()

cleanup() {
    local rc=$?
    set +e
    "${dk[@]}" rm -f "$cli" >/dev/null 2>&1
    if [ "$rc" != 0 ]; then "${rt[@]}" logs "$srv" 2>&1 | grep -v ' INFO  sacdfs::' | tail -n 60; fi
    "${rt[@]}" rm -f "$srv" >/dev/null 2>&1
    for d in "${held[@]}"; do sudo umount "$d"; rmdir "$d"; done
    exit "$rc"
}
trap cleanup EXIT

export RUNTIME="${rt[*]}"
"${rt[@]}" run -d --name "$srv" "${SERVER_OPTS[@]}" --security-opt label=disable \
    -p "127.0.0.1:$nfs_port:2049" -p "127.0.0.1:$smb_port:445" \
    -v "$PWD/tests/corpus/images:/images:ro" "$image" >/dev/null
"$here/ci-wait-healthy.sh" "$srv"

device() { "${rt[@]}" exec "$srv" stat -c %d /srv/sacd; }

"${dk[@]}" run -d --name "$cli" --network host --privileged --security-opt label=disable \
    -v "$PWD:/repo:ro" "$client_image" sleep infinity >/dev/null
"${dk[@]}" exec "$cli" bash -euc "
    mkdir -p /mnt/smb /mnt/nfs
    mount -t cifs //127.0.0.1/sacd /mnt/smb -o ro,guest,vers=3.1.1,port=$smb_port
    mount -t nfs4 127.0.0.1:/sacd /mnt/nfs -o ro,vers=4.2,port=$nfs_port"
dir=dst-features-2ch-6ch/Stereo
file="$dir/$("${dk[@]}" exec "$cli" ls "/mnt/nfs/$dir" | grep -m1 '^01.*dsf$')"

hold() {
    local round=$1 p
    for p in smb nfs; do
        "${dk[@]}" exec "$cli" mkdir -p "/run/hold/$round/$p"
        "${dk[@]}" exec -d "$cli" python3 /repo/deploy/ci-hold-read.py "/mnt/$p/$file" "/run/hold/$round/$p" >/dev/null
    done
    for p in smb nfs; do
        timeout 30 bash -c "until ${dk[*]} exec $cli test -e /run/hold/$round/$p/half; do sleep 0.2; done"
    done
}

check() {
    local round=$1 p r status=0
    "${dk[@]}" exec "$cli" touch "/run/hold/$round/smb/go" "/run/hold/$round/nfs/go"
    for p in smb nfs; do
        timeout 120 bash -c "until ${dk[*]} exec $cli test -e /run/hold/$round/$p/result; do sleep 0.2; done" || true
        r=$("${dk[@]}" exec "$cli" cat "/run/hold/$round/$p/result" 2>/dev/null || echo "no result")
        echo "${rt[*]}, $round, $p: $r"
        [ "$r" = ok ] || status=1
    done
    return "$status"
}

# Round 1: a plain restart.
hold restart
before=$(device)
"${rt[@]}" restart -t 20 "$srv" >/dev/null
"$here/ci-wait-healthy.sh" "$srv"
echo "${rt[*]}, restart: the mount's device number went from $before to $(device)"
check restart

# Round 2: the device number must change. tmpfs mounts take the lowest free numbers, so
# mounting until one has the old number or a higher one leaves the old number taken.
hold new-device
before=$(device)
"${rt[@]}" stop -t 20 "$srv" >/dev/null
for _ in $(seq 256); do
    d=$(mktemp -d)
    sudo mount -t tmpfs none "$d"
    held+=("$d")
    [ "$(stat -c %d "$d")" -ge "$before" ] && break
done
"${rt[@]}" start "$srv" >/dev/null
"$here/ci-wait-healthy.sh" "$srv"
after=$(device)
echo "${rt[*]}, new device: the mount's device number went from $before to $after"
[ "$after" != "$before" ] || { echo "the device number did not change: this round tested nothing"; exit 1; }
check new-device
"${dk[@]}" exec "$cli" umount -l /mnt/smb /mnt/nfs || true
echo "restart test passed: ${rt[*]}"
