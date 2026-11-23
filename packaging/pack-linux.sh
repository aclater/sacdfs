#!/bin/bash
# Packs a Linux release tarball's files as .rpm and .deb (#460), with nfpm (packaging/nfpm.yaml).
#   packaging/pack-linux.sh TARBALL OUTDIR
# TARBALL is a release's sacdfs-<version>-<x86_64|aarch64>-linux.tar.gz. The binary is packed as
# it is; the unit, sacdfs-ctl and the settings file get the package's paths: /usr/bin, and the
# settings in /etc/sysconfig/sacdfs (rpm) or /etc/default/sacdfs (deb) (package-files.sh).
# Needs nfpm in PATH.
# The files' time in the packages is SOURCE_DATE_EPOCH if set, else the tarball's binary's.
set -euo pipefail

tarball=$1
out=$2
here=$(cd "$(dirname "$0")" && pwd)
name=$(basename "$tarball" .tar.gz)
if [[ ! $name =~ ^sacdfs-([0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?)-(x86_64|aarch64)-linux$ ]]; then
    echo "pack-linux.sh: $tarball is not a sacdfs-<version>-<x86_64|aarch64>-linux.tar.gz" >&2
    exit 1
fi
export VERSION=${BASH_REMATCH[1]}
arch=${BASH_REMATCH[3]}
case $arch in x86_64) export NFPM_ARCH=amd64 ;; aarch64) export NFPM_ARCH=arm64 ;; esac

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
tar -C "$stage" -xzf "$tarball"
src=$stage/$name
for f in sacdfs sacdfs-ctl sacdfs.service sacdfs.conf.example COPYING README.md CHANGELOG.md; do
    [[ -f $src/$f ]] || { echo "pack-linux.sh: $name has no $f" >&2; exit 1; }
done
"$src/sacdfs" --version 2>/dev/null | grep -qx "sacdfs $VERSION" ||
    [[ $(uname -m) != "$arch" ]] ||
    { echo "pack-linux.sh: $name's sacdfs is not version $VERSION" >&2; exit 1; }

w=$stage/work
mkdir -p "$w/scripts"
install -p -m 0755 "$src/sacdfs" "$w/sacdfs"
install -p -m 0644 "$src/COPYING" "$src/README.md" "$src/CHANGELOG.md" "$w/"
install -m 0755 "$here/scripts/"*.sh "$w/scripts/"
# The manual pages (#581): the archive's, or, for an archive of a release before them, this tree's.
mandir=$src/man
[[ -d $mandir ]] || mandir=$here/../man
mkdir -p "$w/man"
install -p -m 0644 "$mandir/sacdfs.1" "$mandir/sacdfs-ctl.8" "$w/man/"
for p in rpm deb; do
    "$here/package-files.sh" "$p" "$src/sacdfs-ctl" "$src/sacdfs.service" "$src/sacdfs.conf.example" "$w/$p"
done

# Fixed file times: the same tarball packs to the same file contents and times.
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(stat -c %Y "$src/sacdfs")}
find "$w" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +

mkdir -p "$out"
out=$(cd "$out" && pwd)
for p in rpm deb; do
    (cd "$w" && nfpm package --config "$here/nfpm.yaml" --packager "$p" --target "$out/")
done
ls -l "$out"
