#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The NFS export test of issue #18: export a sacdfs mount with the kernel NFS server, mount
# it over loopback NFS, read, restart sacdfs, and read on through the same open file.
#
# Runs as root in a privileged container that has nfs-utils, fuse3 and python3 (CI uses
# Fedora), on a host whose kernel has the nfsd module loaded:
#
#   sudo modprobe nfsd
#   docker run --rm --privileged -v "$PWD:/src" -w /src registry.fedoraproject.org/fedora:44 \
#     sh -c 'dnf -y -q install nfs-utils fuse3 python3 && tests/nfs-loopback.sh target/debug/sacdfs'
#
# NFSv3, because it is stateless: its client keeps nothing but file handles, so reading on
# after the restart tests exactly what this is about. The restart stops the NFS server
# around it (its clients wait, as for a server reboot), since the server's open files and
# export cache keep the old FUSE mount busy.
set -euo pipefail

SACDFS=$(realpath "${1:?usage: $0 SACDFS-BINARY}")
work=$(mktemp -d)
src=$work/src mnt=$work/mnt client=$work/client
mkdir -p "$src/Artist/Album" "$mnt" "$client"
cp tests/corpus/images/dsd-stereo-mch-2048.iso "$src/Artist/Album/dsd.iso"
cp tests/corpus/images/dst-stereo-5ch-2054.iso "$src/Artist/dst.iso"
# A sidecar file passed through (#79): its handle must survive the restart too.
head -c 300001 tests/corpus/images/dsd-stereo-mch-2048.iso > "$src/Artist/Album/folder.jpg"
export XDG_CACHE_HOME=$work/cache

pid=
start_sacdfs() {
    "$SACDFS" -f --allow-other "$src" "$mnt" &
    pid=$!
    for _ in $(seq 100); do
        mountpoint -q "$mnt" && return 0
        sleep 0.1
    done
    echo "sacdfs did not mount" >&2
    return 1
}

stop_sacdfs() {
    # Unmount (retried while the kernel still holds the mount busy); sacdfs then exits.
    for _ in $(seq 200); do
        mountpoint -q "$mnt" || break
        umount "$mnt" 2>/dev/null && break
        sleep 0.1
    done
    if mountpoint -q "$mnt"; then
        echo "the mount stays busy" >&2
        grep -r . /proc/fs/nfsd/filecache 2>/dev/null | head -5 >&2
        return 1
    fi
    wait "$pid"
}

cleanup() {
    set +e
    umount -f -l "$client" 2>/dev/null
    rpc.nfsd 0 2>/dev/null
    exportfs -ua 2>/dev/null
    [ -n "$pid" ] && kill "$pid" 2>/dev/null
    fusermount3 -u -z "$mnt" 2>/dev/null
    rm -rf "$work"
}
trap cleanup EXIT

set -x
mountpoint -q /proc/fs/nfsd || mount -t nfsd nfsd /proc/fs/nfsd
# The state files the nfs-server service would create.
mkdir -p /var/lib/nfs /run/rpcbind
touch /var/lib/nfs/etab /var/lib/nfs/rmtab
rpcbind -w 2>/dev/null || rpcbind
start_sacdfs
# FUSE has no device number of its own: an export needs an fsid.
exportfs -i -o ro,fsid=4242,no_subtree_check,no_root_squash,insecure "127.0.0.1:$mnt"
rpc.mountd
rpc.nfsd -N 4 8
mount -t nfs -o vers=3,proto=tcp,nolock,hard,timeo=50 "127.0.0.1:$mnt" "$client"

# The client opens files, reads half of each, and waits; meanwhile sacdfs restarts.
fifo=$work/go
mkfifo "$fifo"
python3 - "$client" "$mnt" "$fifo" <<'EOF' &
import os, sys, hashlib
client, mnt, fifo = sys.argv[1:4]
files = []
for root, _, names in os.walk(client):
    for n in sorted(names):
        if n.endswith((".dsf", ".wav")):
            files.append(os.path.relpath(os.path.join(root, n), client))
files = ["Artist/Album/folder.jpg"] + files[:5]
assert len(files) >= 4, files
opened = []
for rel in files:
    # O_DIRECT: every read goes to the server, none is answered from the client's cache.
    fd = os.open(os.path.join(client, rel), os.O_RDONLY | os.O_DIRECT)
    st = os.fstat(fd)
    first = os.pread(fd, st.st_size // 2, 0)
    assert len(first) == st.st_size // 2
    opened.append((rel, fd, st, first))
print(f"client: {len(opened)} files open, half read", flush=True)
with open(fifo, "w") as w:
    w.write("ready\n")
with open(fifo) as r:
    assert r.readline().strip() == "restarted"
for rel, fd, st, first in opened:
    # The same open file, after the restart.
    rest = os.pread(fd, st.st_size - len(first), len(first))
    whole = open(os.path.join(mnt, rel), "rb").read()  # straight from the new mount
    assert first + rest == whole, f"{rel}: differs after the restart"
    assert os.fstat(fd).st_ino == st.st_ino, rel
    print(f"client: {rel}: {len(whole)} bytes, same after the restart", flush=True)
print("client: ok", flush=True)
EOF
reader=$!

read -r line < "$fifo"
[ "$line" = ready ]
echo "restarting sacdfs"
# As a NAS app update: stop serving, restart sacdfs, serve again.
rpc.nfsd 0
exportfs -ua
exportfs -f
stop_sacdfs
start_sacdfs
exportfs -i -o ro,fsid=4242,no_subtree_check,no_root_squash,insecure "127.0.0.1:$mnt"
rpc.nfsd -N 4 8
echo restarted > "$fifo"
wait "$reader"

# A changed image: its open file goes stale instead of reading other data.
exec 3< "$(find "$client/Artist/Album" -name '*.dsf' | head -1)"
rpc.nfsd 0; exportfs -ua; exportfs -f
stop_sacdfs
touch -d '2001-01-01' "$src/Artist/Album/dsd.iso"
start_sacdfs
exportfs -i -o ro,fsid=4242,no_subtree_check,no_root_squash,insecure "127.0.0.1:$mnt"
rpc.nfsd -N 4 8
if out=$(dd bs=4096 count=1 skip=1 iflag=direct status=none <&3 2>&1 >/dev/null); then
    echo "read of a changed image's file through an old handle succeeded" >&2
    exit 1
fi
echo "changed image: $out"
case $out in *"Stale file handle"*) ;; *) echo "expected ESTALE" >&2; exit 1 ;; esac
exec 3<&-
echo "nfs loopback: ok"
