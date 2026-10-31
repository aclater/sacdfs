#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Container health: the sacdfs mount answers, and every server the entry point started is
# still running. Cheap: one stat of the mount root (no decoding) and a few kill -0.
set -u
mountpoint -q /srv/sacd || { echo "sacdfs is not mounted"; exit 1; }
timeout 5 stat -t /srv/sacd >/dev/null || { echo "the mount does not answer"; exit 1; }
[ -r /run/sacdfs-pids ] || { echo "starting"; exit 1; }
while read -r p; do
    kill -0 "$p" 2>/dev/null || { echo "process $p is gone"; exit 1; }
done </run/sacdfs-pids
case "${SMB:-1}" in 1 | yes | true | on)
    timeout 5 bash -c '</dev/tcp/127.0.0.1/445' 2>/dev/null || { echo "smbd is not listening"; exit 1; } ;;
esac
case "${NFS:-1}" in 1 | yes | true | on)
    timeout 5 bash -c '</dev/tcp/127.0.0.1/2049' 2>/dev/null || { echo "NFS is not listening"; exit 1; } ;;
esac
exit 0
