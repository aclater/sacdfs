#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The installer (deploy/install.sh, #340), run as users run it: piped from the release URL
# into sh. A local HTTP server serves the published layout (<url>/latest/download/install.sh,
# <url>/download/v<version>/<archive>); the archive is the published release binary repacked
# with this tree's deploy files, as release.yml packs it. Then deploy/ci-install-steps.sh runs
# in each target: dry run, install, a second run that changes nothing, the files against a
# reference, uninstall, the images folder untouched. In the BusyBox and Fedora containers,
# deploy/ci-install-fetch-steps.sh also checks that failed and cut-short downloads fail loudly.
#
#   deploy/ci-install-test.sh containers    # BusyBox, Fedora, UBI 7 containers
#   deploy/ci-install-test.sh nas-sim       # simulated NAS layouts (github.com/aclater/nas-sim)
#   deploy/ci-install-test.sh host-systemd  # this host, as root through sudo (systemd unit)
#   deploy/ci-install-test.sh host-compose  # this host, as root through sudo (Compose)
#   deploy/ci-install-test.sh unsupported   # unsupported systems and --report (#382)
#
# Run from the repository root. RUNTIME: docker (default) or podman. LABEL_DISABLE=1 on an
# SELinux host. Needs curl, python3 and the corpus in tests/corpus/images.
set -euo pipefail

mode=${1:-containers}
rt=${RUNTIME:-docker}
here=$(cd "$(dirname "$0")" && pwd)
version=$(sed -n 's/^DEFAULT_VERSION=\([^ ]*\).*/\1/p' "$here/install.sh")
arch=$(uname -m)
pkg=sacdfs-$version-$arch-linux
port=${PORT:-18765}
url=http://127.0.0.1:$port/aclater/sacdfs/releases

# Pinned by digest. Red Hat-built bases only: Hummingbird (Fedora-derived, hardened) for the
# BusyBox images, Fedora, and UBI 7 for glibc 2.17.
HUMMINGBIRD_BUILDER=registry.access.redhat.com/hi/core-runtime@sha256:f82d57a594aea1f6763b25224286bcd85b1fbf5bc22f4f1a5b04b8b91864ef92
FEDORA=registry.fedoraproject.org/fedora@sha256:b414c09be79dd1f32529b55b096b6bb30435d82ee16fbdbbabf8985eb5f86db0
UBI7=registry.access.redhat.com/ubi7/ubi@sha256:046e525722f14702c360dc6092324af7c21656e76b0c254b067871f1d4d3df68

work=$(mktemp -d)
server=
cleanup() {
    [ -n "$server" ] && kill "$server" 2>/dev/null
    rm -rf "$work" 2>/dev/null || sudo rm -rf "$work"
}
trap cleanup EXIT

# Systems the installer can't support (#382), and --report: deploy/ci-install-unsupported-steps.sh
# on this host (the runner's dash, not root) and as root in the BusyBox and Fedora
# containers (no /dev/fuse there, so a FUSE that can't be loaded is covered too). The releases
# URL points at a server that serves nothing and logs every request: there must be none.
if [ "$mode" = unsupported ]; then
    mkdir -p "$work/empty"
    python3 -I -m http.server --bind 127.0.0.1 --directory "$work/empty" "$port" >>"$work/http.log" 2>&1 &
    server=$!
    for _ in $(seq 50); do curl -s -o /dev/null "$url/" && break; sleep 0.2; done
    : >"$work/http.log"
    chmod -R a+rX "$work"
    failed=()
    steps=$here/ci-install-unsupported-steps.sh
    # The runner's dash, as the user: the stops come before the root check.
    echo "::group::host, dash"
    dash "$steps" "$here/install.sh" "$url" "$work/http.log" || failed+=(host-dash)
    echo "::endgroup::"
    sed "s|@BUILDER@|$HUMMINGBIRD_BUILDER|" "$here/ci-busybox.Containerfile" >"$work/Containerfile.busybox"
    "$rt" build -q --target busybox -t sacdfs-ci-busybox -f "$work/Containerfile.busybox" "$work" >/dev/null
    uopts=(--network host)
    [ -n "${LABEL_DISABLE:-}" ] && uopts+=(--security-opt label=disable)
    for image in sacdfs-ci-busybox "$FEDORA"; do
        echo "::group::$image"
        "$rt" run --rm "${uopts[@]}" -v "$here/install.sh:/install.sh:ro" -v "$steps:/steps.sh:ro" \
            -v "$work/http.log:/http.log:ro" "$image" sh /steps.sh /install.sh "$url" /http.log ||
            failed+=("$image")
        echo "::endgroup::"
    done
    if grep '"GET ' "$work/http.log"; then
        echo "the server had requests (above): the installer downloaded something"
        failed+=(requests)
    fi
    # The server logs requests at all: one to it now must show up.
    curl -s -o /dev/null "$url/canary"
    sleep 1
    grep -q '"GET /aclater/sacdfs/releases/canary' "$work/http.log" || { echo "the server's log doesn't work"; failed+=(log); }
    if [ "${#failed[@]}" -gt 0 ]; then
        echo "failed: ${failed[*]}"
        exit 1
    fi
    echo "all passed"
    exit 0
fi

# The release, served in GitHub's layout.
rel=$work/srv/aclater/sacdfs/releases
mkdir -p "$rel/download/v$version" "$rel/latest/download" "$work/pub" "$work/pkg/$pkg"
gh_url=https://github.com/aclater/sacdfs/releases/download/v$version/$pkg.tar.gz
curl -fsSL -o "$work/pub/$pkg.tar.gz" "$gh_url"
curl -fsSL -o "$work/pub/$pkg.tar.gz.sha256" "$gh_url.sha256"
(cd "$work/pub" && sha256sum -c "$pkg.tar.gz.sha256")
tar -C "$work/pub" -xzf "$work/pub/$pkg.tar.gz"
install -m 0755 "$work/pub/$pkg/sacdfs" "$here/sacdfs-ctl" "$here/install.sh" "$work/pkg/$pkg/"
install -m 0644 "$here/systemd/sacdfs.service" "$here/sacdfs.conf.example" "$here/nas-compose.yaml" "$work/pkg/$pkg/"
tar -C "$work/pkg" -czf "$rel/download/v$version/$pkg.tar.gz" "$pkg"
(cd "$rel/download/v$version" && sha256sum "$pkg.tar.gz" >"$pkg.tar.gz.sha256")
install -m 0644 "$here/install.sh" "$rel/download/v$version/install.sh"
install -m 0644 "$here/install.sh" "$rel/latest/download/install.sh"
# Downloads cut short, for deploy/ci-install-fetch-steps.sh (#352): all but the last line, and
# the first half.
mkdir -p "$rel/cut" "$rel/half"
sed '$d' "$here/install.sh" >"$rel/cut/install.sh"
head -c "$(($(wc -c <"$here/install.sh") / 2))" "$here/install.sh" >"$rel/half/install.sh"
python3 -I -m http.server --bind 127.0.0.1 --directory "$work/srv" "$port" >"$work/http.log" 2>&1 &
server=$!
for _ in $(seq 50); do curl -fsS -o /dev/null "$url/latest/download/install.sh" && break; sleep 0.2; done

# Three corpus images (stereo and multichannel, DSD and DST) and the reference: every file of
# their mount, read with the release binary here.
mkdir -p "$work/music" "$work/ref-mnt"
for i in dsd-stereo-mch-2048 dst-features-2ch-6ch dst-stereo-5ch-2054; do
    cp "tests/corpus/images/$i.iso" "$work/music/"
done
"$work/pub/$pkg/sacdfs" "$work/music" "$work/ref-mnt"
(cd "$work/ref-mnt" && find . -type f | LC_ALL=C sort | while read -r f; do sha256sum "$f"; done) >"$work/ref.sha"
fusermount3 -u "$work/ref-mnt" 2>/dev/null || fusermount -u "$work/ref-mnt"
echo "reference: $(wc -l <"$work/ref.sha") files"
chmod -R a+rX "$work"

opts=(--network host --device /dev/fuse --cap-add SYS_ADMIN --security-opt apparmor=unconfined)
[ -n "${LABEL_DISABLE:-}" ] && opts+=(--security-opt label=disable)

failed=()
target() { # NAME IMAGE EXPECT [ISO_DIR [run options...]]
    local name=$1 image=$2 expect=$3 iso=${4:-/music}
    shift 3
    [ "$#" -gt 0 ] && shift
    echo "::group::$name ($expect)"
    if "$rt" run --rm "${opts[@]}" "$@" -v "$work/music:$iso:ro" -v "$work/ref.sha:/ref.sha:ro" \
        -v "$here/ci-install-steps.sh:/steps.sh:ro" "$image" sh /steps.sh "$url" "$iso" /ref.sha "$expect"; then
        echo "$name: pass"
    else
        failed+=("$name")
    fi
    echo "::endgroup::"
}
fetchtarget() { # NAME IMAGE: failed and cut-short downloads (#352)
    echo "::group::$1 (failed downloads)"
    if "$rt" run --rm "${opts[@]}" -v "$work/music:/music:ro" \
        -v "$here/ci-install-fetch-steps.sh:/fetch.sh:ro" "$2" sh /fetch.sh "$url" /music; then
        echo "$1 failed downloads: pass"
    else
        failed+=("$1-downloads")
    fi
    echo "::endgroup::"
}

case $mode in
    containers)
        # BusyBox userland, as on QNAP or ASUSTOR: glibc and the unmodified busybox package,
        # nothing else (no bash, no curl: BusyBox wget fetches). sh is BusyBox ash.
        sed "s|@BUILDER@|$HUMMINGBIRD_BUILDER|" "$here/ci-busybox.Containerfile" >"$work/Containerfile.busybox"
        "$rt" build -q --target busybox -t sacdfs-ci-busybox -f "$work/Containerfile.busybox" "$work" >/dev/null
        "$rt" build -q --target busybox-nolibc -t sacdfs-ci-busybox-nolibc -f "$work/Containerfile.busybox" "$work" >/dev/null
        target busybox sacdfs-ci-busybox install
        fetchtarget busybox sacdfs-ci-busybox
        # The same BusyBox, statically linked, and no C library: the binary can't load there.
        target busybox-nolibc sacdfs-ci-busybox-nolibc refuse
        target fedora-44 "$FEDORA" install
        fetchtarget fedora-44 "$FEDORA"
        # UBI 7: glibc 2.17, the oldest the binary supports; bash's sh.
        target ubi7 "$UBI7" install
        ;;
    nas-sim)
        # Simulated NAS layouts: identity files, userland and storage paths only, no vendor
        # software; results are "simulated" and never replace runs on the real systems.
        sim=${NAS_SIM_PREFIX:-ghcr.io/aclater/nas-sim}
        target sim-synology "$sim/synology" install /volume1/music
        target sim-qnap "$sim/qnap" install /share/CACHEDEV1_DATA/Public/music --tmpfs /tmp/config
        target sim-asustor "$sim/asustor" install /volume1/music
        target sim-terramaster "$sim/terramaster" install /Volume1/music
        target sim-ugreen "$sim/ugreen" install /volume1/music
        target sim-readynas "$sim/readynas" install /data/music
        target sim-openmediavault "$sim/openmediavault" install /srv/dev-disk-by-uuid-0000/music
        target sim-truenas "$sim/truenas" pointer /mnt/tank/music
        target sim-unraid "$sim/unraid" pointer /mnt/user/music
        ;;
    host-systemd | host-compose)
        method=${mode#host-}
        # The runner's own sh (dash on Ubuntu), as root through sudo, with the real unit or
        # Docker Compose. The compose route's images line takes the folder as it is on the host.
        sudo env PATH="$PATH" sh "$here/ci-install-steps.sh" "$url" "$work/music" "$work/ref.sha" install --method "$method" ||
            failed+=("$mode")
        ;;
    *)
        echo "usage: $0 containers|nas-sim|host-systemd|host-compose|unsupported" >&2
        exit 2
        ;;
esac

if [ "${#failed[@]}" -gt 0 ]; then
    echo "failed: ${failed[*]}"
    exit 1
fi
echo "all passed"
