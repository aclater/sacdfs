#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Packs a macOS release folder into DIR/PKG.tar.gz with DIR/PKG.tar.gz.sha256. Used by
# .github/workflows/release.yml for the unsigned archive (job "macos") and again for the signed
# one (job "macos-sign"), so both are packed the same way: fixed order, owner and times (the
# commit's), no gzip timestamp.
#
#   pack-macos.sh DIR PKG
#
# Run from the repository: the times are those of HEAD's commit.
set -euo pipefail
dir=${1:?usage: pack-macos.sh DIR PKG}
pkg=${2:?usage: pack-macos.sh DIR PKG}
[ -d "$dir/$pkg" ] || { echo "no folder $dir/$pkg" >&2; exit 2; }
stamp=$(date -r "$(git log -1 --format=%ct)" +%Y%m%d%H%M.%S)
find "$dir/$pkg" -exec touch -h -t "$stamp" {} +
cd "$dir"
find "$pkg" | LC_ALL=C sort > files.txt
tar --uid 0 --gid 0 --no-recursion -cf - -T files.txt | gzip -n -9 > "$pkg.tar.gz"
rm files.txt
shasum -a 256 "$pkg.tar.gz" > "$pkg.tar.gz.sha256"
cat "$pkg.tar.gz.sha256"
