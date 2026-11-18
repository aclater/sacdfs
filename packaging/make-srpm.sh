#!/bin/bash
# The source RPM of a release (#460), for COPR:
#   packaging/make-srpm.sh REF OUTDIR
# From REF in this repository (the release's tag; any commit for a dry run): its source (git
# archive), its crates (cargo vendor of its Cargo.lock) and packaging/sacdfs.spec with the
# version of REF's Cargo.toml filled in. Writes OUTDIR/sacdfs-<version>-1.src.rpm (0.7.0~rc.1
# for 0.7.0-rc.1). Needs git, cargo (with network, for vendoring), rpmbuild and xz. Fixed times
# and order: the same REF gives the same tarballs with the same cargo.
set -euo pipefail

ref=$1 out=$2
here=$(cd "$(dirname "$0")" && pwd)
commit=$(git rev-parse --verify "$ref^{commit}")
version=$(git show "$commit:Cargo.toml" | sed -n 's/^version = "\(.*\)"$/\1/p' | head -n 1)
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "make-srpm.sh: version '$version'" >&2; exit 1; }
rpmversion=${version/-/\~}
mtime=$(git log -1 --format=%ct "$commit")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/SOURCES" "$work/SPECS" "$out"

src=sacdfs-$version
git archive --format=tar --prefix="$src/" "$commit" | gzip -n -9 > "$work/SOURCES/$src.tar.gz"
tar -C "$work" -xzf "$work/SOURCES/$src.tar.gz"
(cd "$work/$src" && cargo vendor --locked --versioned-dirs vendor > /dev/null)
tar -C "$work/$src" --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$mtime" \
    -cf - vendor | xz -T1 -6 > "$work/SOURCES/$src-vendor.tar.xz"
install -m 0644 "$here/package-files.sh" "$work/SOURCES/package-files.sh"
date=$(LC_ALL=C date -u -d "@$mtime" '+%a %b %d %Y')
sed -e "s/@UPSTREAM@/$version/g" -e "s/@RPMVERSION@/$rpmversion/g" -e "s/@DATE@/$date/" \
    "$here/sacdfs.spec" > "$work/SPECS/sacdfs.spec"
if grep -n '@[A-Z]*@' "$work/SPECS/sacdfs.spec"; then exit 1; fi

SOURCE_DATE_EPOCH=$mtime rpmbuild -bs --define "_topdir $work" --define "dist %{nil}" \
    --define "use_source_date_epoch_as_buildtime 1" "$work/SPECS/sacdfs.spec"
cp "$work/SRPMS/sacdfs-$rpmversion-1.src.rpm" "$out/"
ls -l "$out/sacdfs-$rpmversion-1.src.rpm"
