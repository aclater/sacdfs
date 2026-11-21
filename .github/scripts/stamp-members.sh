#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# install.sh verifies release files beside it against built-in checksums: this script builds
# those checksums in, and checks a finished archive against the install.sh inside it. The
# files are the ones install.sh can take from next to itself: sacdfs, sacdfs-ctl,
# sacdfs.service and nas-compose.yaml. Used by .github/workflows/release.yml when it packs
# each archive and the installer published on its own (through stamp-installer.sh), and by
# deploy/ci-install-test.sh for the installer tests.
#
#   stamp-members.sh fill INSTALL_SH DIR...   # INSTALL_SH's MEMBER_SHA256 block becomes one
#                                             # "<sha256>  <folder>/<file>" line per file in
#                                             # each DIR, an archive's folder
#                                             # (sacdfs-VERSION-ARCH-linux)
#   stamp-members.sh check ARCHIVE            # unpacks ARCHIVE and checks its files against
#                                             # the block of the install.sh inside it
set -eu
members="sacdfs sacdfs-ctl sacdfs.service nas-compose.yaml"
die() {
    echo "stamp-members.sh: $*" >&2
    exit 1
}
sha() { sha256sum <"$1" | cut -d' ' -f1; }
# block FILE: the lines of FILE's MEMBER_SHA256 block.
block() { sed -n "/^MEMBER_SHA256='\$/,/^'\$/p" "$1" | sed '1d;$d'; }

case ${1:-} in
    fill)
        [ "$#" -ge 3 ] || die "usage: stamp-members.sh fill INSTALL_SH DIR..."
        out=$2
        shift 2
        grep -qx "MEMBER_SHA256='" "$out" || die "$out has no MEMBER_SHA256 block"
        lines=
        n=0
        for d in "$@"; do
            name=${d%/}
            name=${name##*/}
            case $name in sacdfs-*-linux) ;; *) die "$d isn't an archive's folder (sacdfs-VERSION-ARCH-linux)" ;; esac
            [ -f "$d/sacdfs" ] && [ -f "$d/sacdfs-ctl" ] || die "$d has no sacdfs or sacdfs-ctl"
            for m in $members; do
                [ -f "$d/$m" ] || continue
                [ ! -L "$d/$m" ] || die "$d/$m is a symbolic link"
                h=$(sha "$d/$m")
                printf '%s\n' "$h" | grep -Eqx '[0-9a-f]{64}' || die "no checksum of $d/$m"
                lines="$lines$h  $name/$m
"
                n=$((n + 1))
            done
        done
        printf '%s' "$lines" | awk -v sums=/dev/stdin '
            $0 == "MEMBER_SHA256=\047" && !done {
                print
                while ((getline l < sums) > 0) print l
                skip = 1; done = 1; next
            }
            skip { if ($0 == "\047") { skip = 0; print } ; next }
            { print }
            END { if (!done || skip) exit 1 }' "$out" >"$out.members" || {
            rm -f "$out.members"
            die "$out doesn't have the layout this script fills in"
        }
        sh -n "$out.members"
        got=$(block "$out.members" | grep -c '^[0-9a-f]\{64\}  ' || true)
        [ "$got" = "$n" ] || die "$got checksums of release files in the result, expected $n"
        # Written over in place, so that the file keeps its mode.
        cat "$out.members" >"$out"
        rm -f "$out.members"
        echo "$out: $n checksums of release files built in"
        printf '%s' "$lines"
        ;;
    check)
        [ "$#" = 2 ] || die "usage: stamp-members.sh check ARCHIVE"
        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT
        tar -C "$tmp" -xzf "$2"
        set -- "$tmp"/sacdfs-*-linux
        [ "$#" = 1 ] && [ -d "$1" ] || die "the archive doesn't hold one sacdfs-VERSION-ARCH-linux folder"
        d=$1
        name=${d##*/}
        [ -f "$d/install.sh" ] || die "$name has no install.sh"
        block "$d/install.sh" >"$tmp/block"
        n=0
        for m in $members; do
            if [ -f "$d/$m" ]; then
                want=$(awk -v f="$name/$m" '$2 == f { print $1 }' "$tmp/block")
                [ -n "$want" ] || die "$name/$m has no checksum in its install.sh"
                [ "$(sha "$d/$m")" = "$want" ] || die "$name/$m doesn't match the checksum in its install.sh"
                n=$((n + 1))
            fi
        done
        # Every line names a file of this archive, and nothing else.
        lines=$(grep -c . "$tmp/block" || true)
        [ "$lines" = "$n" ] || die "$name/install.sh has $lines checksum lines for $n files"
        [ "$n" -ge 2 ] || die "$name has too few release files"
        echo "$name: its $n release files match the checksums in its install.sh"
        ;;
    *)
        die "usage: stamp-members.sh fill INSTALL_SH DIR... | check ARCHIVE"
        ;;
esac
