#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# A stop does not wait for files that stay open (#276). The container runs with
# SACDFS_STOP_GRACE=3. A client mounts the shares over SMB and NFSv4.2 and holds a file open
# on each, and a process inside the container holds one open on the mount itself (what a
# server on the same host would). `stop -t 30` must then end the container on its own, with
# status 0 and not by the runtime's SIGKILL, after the grace period and well inside the 30 s;
# sacdfs's log says it exited with files still open.
#
#   deploy/ci-held-stop-test.sh RUNTIME IMAGE     # RUNTIME: "docker" or "sudo podman"
#
# Run from the repository root, by deploy/ci-share-test.sh, which has built the client image.
# DOCKER: the command that runs the client container, "docker" by default.
set -euo pipefail

read -r -a rt <<<"$1"
image=$2
read -r -a dk <<<"${DOCKER:-docker}"
client_image=${CLIENT:-sacdfs-client:ci}
here=$(cd "$(dirname "$0")" && pwd)
srv=sacdfs-heldstop
cli=sacdfs-heldstop-client
grace=3
# Not the ports of the other tests, which may still be in TIME_WAIT.
case "$1" in
    *podman*) nfs_port=12052 smb_port=10448 ;;
    *) nfs_port=12051 smb_port=10447 ;;
esac

cleanup() {
    local rc=$?
    set +e
    "${dk[@]}" exec "$cli" umount -l /mnt/smb /mnt/nfs >/dev/null 2>&1
    "${dk[@]}" rm -f "$cli" >/dev/null 2>&1
    if [ "$rc" != 0 ]; then "${rt[@]}" logs "$srv" 2>&1 | grep -v ' INFO  sacdfs::' | tail -n 60; fi
    "${rt[@]}" rm -f "$srv" >/dev/null 2>&1
    exit "$rc"
}
trap cleanup EXIT

export RUNTIME="${rt[*]}"
"${rt[@]}" run -d --name "$srv" -e SACDFS_STOP_GRACE=$grace \
    --device /dev/fuse --cap-add SYS_ADMIN --cap-add DAC_READ_SEARCH \
    --security-opt apparmor=unconfined --security-opt label=disable \
    -p "127.0.0.1:$nfs_port:2049" -p "127.0.0.1:$smb_port:445" \
    -v "$PWD/tests/corpus/images:/images:ro" "$image" >/dev/null
"$here/ci-wait-healthy.sh" "$srv"

"${dk[@]}" run -d --name "$cli" --network host --privileged --security-opt label=disable \
    -v "$PWD:/repo:ro" "$client_image" sleep infinity >/dev/null
"${dk[@]}" exec "$cli" bash -euc "
    mkdir -p /mnt/smb /mnt/nfs
    mount -t cifs //127.0.0.1/sacd /mnt/smb -o ro,guest,vers=3.1.1,port=$smb_port
    mount -t nfs4 127.0.0.1:/sacd /mnt/nfs -o ro,vers=4.2,port=$nfs_port"
dir=dst-features-2ch-6ch/Stereo
file="$dir/$("${dk[@]}" exec "$cli" ls "/mnt/nfs/$dir" | grep -m1 '^01.*dsf$')"

# One file open on each share (it reads half and then waits for a "go" that never comes).
for p in smb nfs; do
    "${dk[@]}" exec "$cli" mkdir -p "/run/hold/$p"
    "${dk[@]}" exec -d "$cli" python3 /repo/deploy/ci-hold-read.py "/mnt/$p/$file" "/run/hold/$p" >/dev/null
done
for p in smb nfs; do
    timeout 30 bash -c "until ${dk[*]} exec $cli test -e /run/hold/$p/half; do sleep 0.2; done"
done
# And one open on the mount, inside the container.
"${rt[@]}" exec -d "$srv" bash -c 'exec 3<"$0"; exec sleep 600' "/srv/sacd/$file"
sleep 1
"${rt[@]}" exec "$srv" bash -c 'for l in /proc/[0-9]*/fd/*; do case $(readlink "$l") in */srv/sacd/*) exit 0 ;; esac; done; exit 1' ||
    { echo "no process holds a file open on the mount: this test would test nothing"; exit 1; }

start=$(date +%s%N)
"${rt[@]}" stop -t 30 "$srv" >/dev/null
took=$((($(date +%s%N) - start) / 1000000))  # ms
code=$("${rt[@]}" inspect -f '{{.State.ExitCode}}' "$srv")
echo "${rt[*]}: stop with files held open over SMB, NFS and in the container took ${took} ms, exit status $code"
logs=$("${rt[@]}" logs "$srv" 2>&1)
[ "$code" = 0 ] || { echo "the container did not exit with status 0 (a SIGKILL gives 137)"; exit 1; }
[ "$took" -lt 25000 ] || { echo "the stop took too long"; exit 1; }
[ "$took" -ge $(((grace - 1) * 1000)) ] || { echo "the stop did not wait for the held file: nothing was tested"; exit 1; }
grep -q 'files are still open: exiting' <<<"$logs" || { echo "sacdfs did not log its exit after the grace period"; exit 1; }
echo "held-open stop test passed: ${rt[*]}"
