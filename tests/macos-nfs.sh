#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The macOS NFS front end (#420, spike): serve the test corpus with sacdfs-nfs on 127.0.0.1,
# mount it with macOS's own NFS client as the current user (no root, no FUSE), and write the
# SHA-256 of every file, sorted, to OUT. Then check that names given decomposed (NFD), as
# macOS gives them, are found with the `nfc` mount option.
#
#   tests/macos-nfs.sh target/release/sacdfs-nfs OUT
set -euo pipefail

SERVER=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
OUT=${2:?usage: $0 SACDFS-NFS OUT}
PORT=${PORT:-11111}
work=$(mktemp -d)
mnt=$work/mnt
mkdir -p "$mnt"

"$SERVER" tests/corpus/images "$PORT" 2> "$work/server.log" &
pid=$!
cleanup() {
    umount "$mnt" 2> /dev/null || true
    kill "$pid" 2> /dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT
for _ in $(seq 100); do
    nc -z 127.0.0.1 "$PORT" 2> /dev/null && break
    sleep 0.1
done

mount_nfs -o "vers=3,tcp,port=$PORT,mountport=$PORT,nolocks,rdonly,nfc,soft,intr,rsize=1048576" \
    localhost:/ "$mnt"
(cd "$mnt" && find . -type f -print0 | xargs -0 shasum -a 256) | LC_ALL=C sort > "$OUT"
echo "$(wc -l < "$OUT") files hashed"

python3 -I - "$mnt" << 'EOF'
import os, sys, unicodedata
root, n, bad = sys.argv[1], 0, []
for d, dirs, files in os.walk(root):
    for name in dirs + files:
        for form in {unicodedata.normalize("NFC", name), unicodedata.normalize("NFD", name)}:
            n += 1
            if not os.path.exists(os.path.join(d, form)):
                bad.append(os.path.join(d, form))
print(f"{n} lookups in both normalisation forms, {len(bad)} missing")
sys.exit(1 if bad else 0)
EOF
