#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The container share test (CI): serve the corpus from an image of the Containerfile and read
# it back over SMB and NFS from a client container with kernel mounts, checking every track's
# hashes against the corpus manifest; keep reading open files across a server restart; then
# user access, DSF only, flattened.
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

group() { echo "::group::$*"; }
endgroup() { echo "::endgroup::"; }

server() {
    docker run -d --name sacdfs --network sacdfs-ci \
        --device /dev/fuse --cap-add SYS_ADMIN --cap-add DAC_READ_SEARCH \
        --security-opt apparmor=unconfined \
        "$@" -v "$PWD/tests/corpus/images:/images:ro" "$image"
    "$here/ci-wait-healthy.sh" sacdfs
}

group "Client image (kernel SMB and NFS clients)"
sudo modprobe -a cifs nfs nfsv4
docker network create sacdfs-ci
docker run --name client-build "$client_image" \
    dnf -y -q --setopt=install_weak_deps=False install cifs-utils nfs-utils python3
docker commit client-build sacdfs-client:ci
docker rm client-build
endgroup

group "Start the server on the corpus (guest access)"
server
endgroup

group "Read the corpus over SMB and NFS"
docker run --rm --network sacdfs-ci --privileged \
    -v "$PWD:/repo:ro" sacdfs-client:ci bash -euxc '
      mkdir -p /mnt/smb /mnt/nfs
      mount -t cifs //sacdfs/sacd /mnt/smb -o ro,guest,vers=3.1.1
      python3 /repo/deploy/check-share.py /repo/tests/corpus/manifest.json /mnt/smb --min 400 --wav
      umount /mnt/smb
      mount -t nfs4 sacdfs:/sacd /mnt/nfs -o ro,vers=4.2
      python3 /repo/deploy/check-share.py /repo/tests/corpus/manifest.json /mnt/nfs --min 400 --wav
      umount /mnt/nfs'
endgroup

# Clients keep files open across a server restart (a NAS reboot, an image update).
# SMB reopens by path; NFS relies on sacdfs's persistent file identity (#18).
group "Keep reading across a server restart"
docker run -d --name client --network sacdfs-ci --privileged \
    -v "$PWD:/repo:ro" sacdfs-client:ci sleep infinity
docker exec client bash -euxc '
    mkdir -p /mnt/smb /mnt/nfs /run/hold/smb /run/hold/nfs
    mount -t cifs //sacdfs/sacd /mnt/smb -o ro,guest,vers=3.1.1
    mount -t nfs4 sacdfs:/sacd /mnt/nfs -o ro,vers=4.2'
f="dst-features-2ch-6ch/Stereo/$(docker exec client ls /mnt/nfs/dst-features-2ch-6ch/Stereo | grep -m1 '^01.*dsf$')"
for p in smb nfs; do
    docker exec -d client python3 /repo/deploy/ci-hold-read.py "/mnt/$p/$f" "/run/hold/$p"
done
for p in smb nfs; do
    timeout 30 bash -c "until docker exec client test -e /run/hold/$p/half; do sleep 0.5; done"
done
docker restart -t 20 sacdfs
"$here/ci-wait-healthy.sh" sacdfs
docker exec client touch /run/hold/smb/go /run/hold/nfs/go
status=0
for p in smb nfs; do
    timeout 120 bash -c "until docker exec client test -e /run/hold/$p/result; do sleep 0.5; done" || true
    r=$(docker exec client cat "/run/hold/$p/result" 2>/dev/null || echo "no result")
    echo "$p across a restart: $r"
    [ "$r" = ok ] || status=1
done
docker exec client umount -l /mnt/smb /mnt/nfs || true
docker rm -f client
endgroup
[ "$status" = 0 ] || exit "$status"

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
echo "share test passed: $image"
