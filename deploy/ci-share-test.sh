#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The container share test (CI): serve the corpus from an image of the Containerfile and read
# it back over SMB and NFS from a client container with kernel mounts, checking every track's
# hashes against the corpus manifest; keep reading open files across a server restart, under
# Docker and Podman (deploy/ci-restart-test.sh); a player container on the same host reading the
# mount directly (deploy/ci-same-host-test.sh); then user access, DSF only, flattened.
#
#   deploy/ci-share-test.sh IMAGE
#
# Run from the repository root on a Docker host that can load the cifs and nfs modules (a
# GitHub runner). Used by container.yml on every pull request and by release.yml on the image
# it is about to publish. Leaves the server container `sacdfs` for the caller's log step.
set -euo pipefail

image=$1
# The client: Fedora 44, for mount.cifs and mount.nfs4 (pinned like the image's bases).
client_image=${CLIENT_IMAGE:-registry.fedoraproject.org/fedora:44@sha256:b414c09be79dd1f32529b55b096b6bb30435d82ee16fbdbbabf8985eb5f86db0}
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=deploy/ci-caps.sh
. "$here/ci-caps.sh"

group() { echo "::group::$*"; }
endgroup() { echo "::endgroup::"; }

server() {
    docker run -d --name sacdfs --network sacdfs-ci "${SERVER_OPTS[@]}" \
        "$@" -v "$PWD/tests/corpus/images:/images:ro" "$image"
    "$here/ci-wait-healthy.sh" sacdfs
}

group "Client image (kernel SMB and NFS clients)"
sudo modprobe -a cifs nfs nfsv4
docker network create sacdfs-ci
docker run --name client-build "$client_image" \
    dnf -y -q --setopt=install_weak_deps=False install cifs-utils nfs-utils python3 attr
docker commit client-build sacdfs-client:ci
docker rm client-build
endgroup

group "Start the server on the corpus (guest access)"
server
endgroup

group "Read the corpus over SMB and NFS"
docker run --rm --network sacdfs-ci --privileged \
    -v "$PWD:/repo:ro" sacdfs-client:ci bash -euxc '
      # The share has the size of its images, all of it used, and is never 0 bytes (#62, #217).
      full() {
        read -r size avail < <(df -B1 --output=size,avail "$1" | tail -n 1)
        echo "$1: size $size, available $avail"
        [ "$size" -gt 0 ] && [ "$avail" -eq 0 ]
      }
      # A long listing prints no error, and over NFS neither does an xattr dump (#174: every
      # entry of `ls -l` on the NFS export said "Unknown error 524" while sacdfs answered
      # xattr requests with ENOSYS). SMB has no xattrs (ea support = no): getfattr fails there.
      quiet() {
        ls -lR "$1" >/dev/null 2>/tmp/err || { head -n 5 /tmp/err; return 1; }
        if [ -n "${2:-}" ]; then
          getfattr -R -d "$1" >/dev/null 2>>/tmp/err || { head -n 5 /tmp/err; return 1; }
        fi
        [ ! -s /tmp/err ] || { head -n 5 /tmp/err; return 1; }
        echo "$1: no errors"
      }
      mkdir -p /mnt/smb /mnt/nfs
      mount -t cifs //sacdfs/sacd /mnt/smb -o ro,guest,vers=3.1.1
      full /mnt/smb
      # Files carry the read-only attribute (map readonly = yes, #217): no write bit.
      [ -z "$(find /mnt/smb -type f -perm /222 -print -quit)" ]
      quiet /mnt/smb
      python3 /repo/deploy/check-share.py /repo/tests/corpus/manifest.json /mnt/smb --min 400 --wav
      umount /mnt/smb
      mount -t nfs4 sacdfs:/sacd /mnt/nfs -o ro,vers=4.2
      full /mnt/nfs
      quiet /mnt/nfs xattrs
      python3 /repo/deploy/check-share.py /repo/tests/corpus/manifest.json /mnt/nfs --min 400 --wav
      umount /mnt/nfs'
endgroup

# Clients keep files open across a server restart (a NAS reboot, an image update), under both
# runtimes the deployment guide covers. A failure here stops the script with the server's log.
group "Keep reading across a server restart (Docker)"
"$here/ci-restart-test.sh" docker "$image"
endgroup

group "Keep reading across a server restart (Podman)"
command -v podman >/dev/null || sudo apt-get install -y -q --no-install-recommends podman
podman_image=$(docker save "$image" | sudo podman load -q | sed -n 's/^Loaded image[^:]*: //p' | head -n 1)
RUNTIME="sudo podman" "$here/ci-restart-test.sh" "sudo podman" "$podman_image"
endgroup

# A stop does not wait for files that stay open (#276).
group "Stop with files held open (Docker)"
"$here/ci-held-stop-test.sh" docker "$image"
endgroup

group "Stop with files held open (Podman)"
RUNTIME="sudo podman" "$here/ci-held-stop-test.sh" "sudo podman" "$podman_image"
endgroup

group "Players on the same host read the mount directly (#186)"
"$here/ci-same-host-test.sh" "$image"
endgroup

group "User access, DSF only, flattened"
docker rm -f sacdfs
server -e SMB_USER=listener -e SMB_PASSWORD=ci-only-password -e NFS=0 \
    -e SACDFS_FORMATS=dsf -e SACDFS_FLATTEN=1
docker run --rm --network sacdfs-ci --privileged \
    -v "$PWD:/repo:ro" sacdfs-client:ci bash -euxc '
      mkdir -p /mnt/smb
      if mount -t cifs //sacdfs/sacd /mnt/smb -o ro,guest,vers=3.1.1; then
        echo "guest access was not refused"; exit 1
      fi
      mount -t cifs //sacdfs/sacd /mnt/smb -o ro,username=listener,password=ci-only-password,vers=3.1.1
      test -z "$(find /mnt/smb -name "*.wav" -print -quit)"
      python3 /repo/deploy/check-share.py /repo/tests/corpus/manifest.json /mnt/smb --min 400
      umount /mnt/smb'
endgroup
group "A bad SACDFS_MOUNT_TIMEOUT stops the container with a message"
for v in abc 0 99999; do
    if out=$(docker run --rm -e SACDFS_MOUNT_TIMEOUT="$v" "$image" 2>&1); then
        echo "SACDFS_MOUNT_TIMEOUT=$v was accepted"; exit 1
    fi
    grep -q "SACDFS_MOUNT_TIMEOUT must be a whole number of seconds" <<<"$out" || { echo "$out"; exit 1; }
done
endgroup
echo "share test passed: $image"
