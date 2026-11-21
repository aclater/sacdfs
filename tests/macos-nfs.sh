#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The NFS front end (#420) end to end, with the system's own NFS client: on macOS as the
# current user, through `sacdfs-nfs SOURCE MOUNTPOINT`; on Linux with `sudo mount -t nfs`.
#
#   tests/macos-nfs.sh SACDFS-NFS OUT
#
# 1. Writes the SHA-256 of every file of the mount, sorted, to OUT (to compare with a Linux
#    FUSE mount of the same commit).
# 2. Looks every name up in both Unicode normalisation forms (NFC and NFD).
# 3. Checks that the mount is read-only and gets no `._` files, and that its size is the
#    server's (FSSTAT), not a fixed 1 TiB.
# 4. Checks that another user's calls are refused (needs passwordless sudo; skipped without).
# 5. Kills the server while a file is open and half read, starts it again, and reads on:
#    the file handles must survive the restart, and the bytes must be the same.
# 6. macOS: stops `sacdfs-nfs SOURCE MOUNTPOINT` with SIGTERM and checks that it unmounted.
#
# SOURCE (default tests/corpus/images) and PORT (default the server's own) may be set.
# Bash 3.2 (macOS's) compatible.
set -euo pipefail

OUT=${2:?usage: $0 SACDFS-NFS OUT}
SERVER=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
SOURCE=${SOURCE:-tests/corpus/images}
# The port the server chooses for a user other than root (default_port in src/nfs/serve.rs).
# It is always passed with --port, so the two cannot differ.
PORT=${PORT:-$((20000 + $(id -u) % 10000))}
OS=$(uname)

# World-traversable, so that another user can reach the mount (step 4).
work=$(mktemp -d /tmp/sacdfs-nfs.XXXXXX)
chmod 755 "$work"
mnt=$work/mnt
mkdir -p "$mnt"
pid=

# The SHA-256 command, as an array: one word per element.
if [ "$OS" = Darwin ]; then
    SHA=(shasum -a 256)
else
    SHA=(sha256sum)
fi

# umount as whoever mounted: the user on macOS, root on Linux.
system_umount() {
    if [ "$OS" = Darwin ]; then umount "$@"; else sudo umount "$@"; fi
}

unmount() {
    # A file closed just now can keep the mount busy for a moment.
    for _ in 1 2 3 4 5 6 7 8 9; do
        system_umount "$mnt" 2> /dev/null && return
        sleep 1
    done
    system_umount "$mnt"
}

mounted() {
    # The corpus mount is never empty; the bare folder is.
    [ -n "$(ls -A "$mnt" 2> /dev/null)" ]
}

cleanup() {
    exec 3<&- 2> /dev/null || true
    if mounted; then unmount || true; fi
    if [ -n "$pid" ]; then kill "$pid" 2> /dev/null || true; fi
    sleep 0.2
    rm -rf "$work"
}
trap cleanup EXIT

wait_for() {
    local what=$1
    shift
    for _ in $(seq 300); do
        if "$@"; then return 0; fi
        sleep 0.1
    done
    echo "timed out waiting for $what; server log:" >&2
    cat "$work/server.log" >&2
    exit 1
}

# Our server, not another program on the port: it must still be running.
listening() {
    kill -0 "$pid" 2> /dev/null || {
        echo "the server exited; its log:" >&2
        cat "$work/server.log" >&2
        exit 1
    }
    nc -z 127.0.0.1 "$PORT" 2> /dev/null
}

# Starts the server; with "mount", the macOS helper mounts it too, else (Linux) it is
# mounted here with sudo.
start() {
    if [ "${1:-}" = mount ] && [ "$OS" = Darwin ]; then
        "$SERVER" --port "$PORT" "$SOURCE" "$mnt" >> "$work/server.log" 2>&1 3<&- &
        pid=$!
        wait_for "the mount" mounted
        return
    fi
    "$SERVER" --port "$PORT" "$SOURCE" >> "$work/server.log" 2>&1 3<&- &
    pid=$!
    wait_for "the server" listening
    if [ "${1:-}" = mount ]; then
        # The options sacdfs-nfs itself gives on Linux (mount_options in src/nfs/serve.rs):
        # keep the two lists the same.
        sudo mount -t nfs -o "vers=3,proto=tcp,port=$PORT,mountport=$PORT,mountproto=tcp,nolock,soft,timeo=10,retrans=30,rsize=1048576,ro,nosuid,nodev,noexec" \
            127.0.0.1:/ "$mnt"
    fi
}

start mount
echo "mounted on $mnt"

# 1. Every file's bytes.
(cd "$mnt" && find . -type f -print0 | xargs -0 "${SHA[@]}") | LC_ALL=C sort > "$OUT"
echo "$(wc -l < "$OUT" | tr -d ' ') files hashed"

# 2. Names in both normalisation forms.
# Linux's client has no `nfc` option: there, NFD names that differ from their NFC form are
# not found (as over Samba or the FUSE mount), so they are only counted.
python3 -I - "$mnt" "$OS" << 'EOF'
import os, sys, unicodedata
root, n, bad = sys.argv[1], 0, []
for d, dirs, files in os.walk(root):
    for name in dirs + files:
        for form in {unicodedata.normalize("NFC", name), unicodedata.normalize("NFD", name)}:
            n += 1
            if not os.path.exists(os.path.join(d, form)):
                bad.append(os.path.join(d, form))
print(f"{n} lookups in both normalisation forms, {len(bad)} missing")
sys.exit(1 if bad and sys.argv[2] == "Darwin" else 0)
EOF

# 3. Read-only, no AppleDouble files, and a real size.
if touch "$mnt/sacdfs-write-test" 2> /dev/null; then
    echo "FAIL: a file could be made on the mount" >&2
    exit 1
fi
if [ -n "$(find "$mnt" -name '._*' -print -quit)" ]; then
    echo "FAIL: ._ files on the mount" >&2
    exit 1
fi
kib=$(df -k "$mnt" | awk 'NR == 2 { print $2 }')
echo "size: $kib KiB"
if [ "$kib" -le 0 ] || [ "$kib" -eq 1073741824 ]; then
    echo "FAIL: the mount's size is $kib KiB, not the server's" >&2
    exit 1
fi

# 4. Another user's calls are refused.
sample=$(cd "$mnt" && find . -type f -name '*.dsf' | LC_ALL=C sort | sed -n 1p)
if sudo -n true 2> /dev/null; then
    if sudo -n -u nobody cat "$mnt/$sample" > /dev/null 2>&1; then
        echo "FAIL: user nobody read $sample" >&2
        exit 1
    fi
    echo "another user: refused"
else
    echo "another user: skipped (no passwordless sudo)"
fi

# 5. A restart with a file open and half read.
# The largest file, read up to its middle.
big=$(cd "$mnt" && python3 -I -c '
import os
print(max((os.path.getsize(os.path.join(d, f)), os.path.join(d, f)) for d, _, fs in os.walk(".") for f in fs)[1])')
half=$(($(wc -c < "$mnt/$big") / 2 / 4096))
want=$(grep -F "  $big" "$OUT" | sed -n 1p | cut -d' ' -f1)
if [ -z "$want" ] || [ "$half" -eq 0 ]; then
    echo "FAIL: no file to read across a restart" >&2
    exit 1
fi
exec 3< "$mnt/$big"
dd bs=4096 count="$half" <&3 > "$work/part" 2> /dev/null
kill -9 "$pid"
wait "$pid" 2> /dev/null || true
start
cat <&3 >> "$work/part"
exec 3<&-
got=$("${SHA[@]}" "$work/part" | cut -d' ' -f1)
if [ "$got" != "$want" ]; then
    echo "FAIL: $big read across a restart: $got, want $want" >&2
    exit 1
fi
ls -R "$mnt" > /dev/null
echo "restart: $big read on, the same bytes; the tree lists again"
unmount
kill "$pid"
wait "$pid" 2> /dev/null || true
pid=

# 6. The helper unmounts when stopped (macOS: the mount is the helper's).
if [ "$OS" = Darwin ]; then
    start mount
    kill -TERM "$pid"
    wait "$pid"
    pid=
    if mounted; then
        echo "FAIL: still mounted after SIGTERM" >&2
        exit 1
    fi
    echo "SIGTERM: unmounted and stopped"
fi
echo "all NFS checks passed"
