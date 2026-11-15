#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The installer as a release publishes it: deploy/install.sh with DEFAULT_VERSION set to the
# release's version and the SHA-256 of its archives built in (RELEASE_SHA256), so that it
# checks the downloads against values from the attested install.sh itself rather than a
# .sha256 from the same server. Used by .github/workflows/release.yml after the binaries
# are built, and by deploy/ci-install-test.sh for the installer tests.
#
#   stamp-installer.sh VERSION DIR SRC OUT
#
# DIR holds sacdfs-VERSION-*-linux.tar.gz and their .sha256 files; each archive is checked
# against its .sha256 first. SRC is deploy/install.sh; OUT is written.
set -eu
[ "$#" = 4 ] || {
    echo "usage: stamp-installer.sh VERSION DIR SRC OUT" >&2
    exit 2
}
version=$1 dir=$2 src=$3 out=$4
case $version in '' | *[!0-9A-Za-z.+-]*)
    echo "stamp-installer.sh: bad version: $version" >&2
    exit 1
    ;;
esac

# The archives' checksum lines, each checked against its archive.
lines=
n=0
for f in "$dir"/sacdfs-"$version"-*-linux.tar.gz; do
    [ -f "$f" ] || continue
    name=${f##*/}
    [ -f "$f.sha256" ] || {
        echo "stamp-installer.sh: $name has no .sha256" >&2
        exit 1
    }
    line=$(cat "$f.sha256")
    printf '%s\n' "$line" | grep -Eqx "[0-9a-f]{64}  sacdfs-$(printf '%s' "$version" | sed 's/[.+]/\\&/g')-[A-Za-z0-9_-]+-linux\.tar\.gz" || {
        echo "stamp-installer.sh: $name.sha256 isn't one '<sha256>  $name' line: $line" >&2
        exit 1
    }
    [ "${line#*  }" = "$name" ] || {
        echo "stamp-installer.sh: $name.sha256 names another file" >&2
        exit 1
    }
    [ "$(sha256sum <"$f" | cut -d' ' -f1)" = "${line%%  *}" ] || {
        echo "stamp-installer.sh: $name doesn't match its .sha256" >&2
        exit 1
    }
    lines="$lines$line
"
    n=$((n + 1))
done
[ "$n" -gt 0 ] || {
    echo "stamp-installer.sh: no sacdfs-$version-*-linux.tar.gz in $dir" >&2
    exit 1
}

# DEFAULT_VERSION, and the lines between "RELEASE_SHA256='" and the "'" that closes it.
grep -qx "RELEASE_SHA256='" "$src" || {
    echo "stamp-installer.sh: $src has no RELEASE_SHA256 block" >&2
    exit 1
}
printf '%s' "$lines" | awk -v v="$version" -v sums=/dev/stdin '
    /^DEFAULT_VERSION=/ { sub(/^DEFAULT_VERSION=[^ ]*/, "DEFAULT_VERSION=" v); dv++ }
    $0 == "RELEASE_SHA256=\047" && !done {
        print
        while ((getline l < sums) > 0) print l
        skip = 1; done = 1; next
    }
    skip { if ($0 == "\047") { skip = 0; print } ; next }
    { print }
    END { if (dv != 1 || !done || skip) exit 1 }' "$src" >"$out.tmp" || {
    echo "stamp-installer.sh: $src doesn't have the layout this script fills in" >&2
    rm -f "$out.tmp"
    exit 1
}

# The result: parses, has the version and every checksum, and nothing else changed.
sh -n "$out.tmp"
grep -q "^DEFAULT_VERSION=$version " "$out.tmp"
got=$(sed -n "/^RELEASE_SHA256='\$/,/^'\$/p" "$out.tmp" | grep -c '^[0-9a-f]\{64\}  ')
[ "$got" = "$n" ] || {
    echo "stamp-installer.sh: $got checksums in the result, expected $n" >&2
    exit 1
}
mv "$out.tmp" "$out"
chmod 0755 "$out"
echo "stamped $out: sacdfs $version, $n archive checksums"
printf '%s' "$lines"
