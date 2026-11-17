#!/bin/bash
# Tests the .rpm or .deb (#460) as root in a container of the target distribution, privileged
# with /dev/fuse:
#   packaging/test-packages.sh PKGDIR TARBALL_DIR IMAGES
# PKGDIR holds this build's package and the previous release's (same format and architecture);
# TARBALL_DIR is this build's unpacked release tarball; IMAGES the test corpus's images.
# Checks: install, package verify, systemd-analyze verify of the unit, --version, the same
# binary and the same mounted files as the tarball's, sacdfs-ctl with the package's settings
# file, removal leaves nothing; then the previous package, enabled and with edited settings,
# upgraded to this one: settings and enablement kept; removal leaves only the edited settings.
set -euo pipefail

pkgdir=$(cd "$1" && pwd)
tardir=$(cd "$2" && pwd)
images=$(cd "$3" && pwd)
fail() { echo "FAIL: $*" >&2; exit 1; }
step() { echo "=== $*"; }
. /etc/os-release
echo "$PRETTY_NAME, $(uname -m)"

if command -v dnf >/dev/null; then
    fmt=rpm conf=/etc/sysconfig/sacdfs
    pkg_install() { dnf -y -q install "$@"; }
    pkg_remove() { dnf -y -q remove sacdfs; }
    pkg_purge() { :; }
    pkg_verify() { rpm -V sacdfs; }
    step "tools and fuse3 (before the snapshot)"
    dnf -y -q --setopt=install_weak_deps=False install fuse3 systemd findutils diffutils procps-ng >/dev/null
else
    fmt=deb conf=/etc/default/sacdfs
    export DEBIAN_FRONTEND=noninteractive
    pkg_install() { apt-get install -y -q "$@" >/dev/null; }
    pkg_remove() { apt-get remove -y -q sacdfs >/dev/null; }
    pkg_purge() { apt-get purge -y -q sacdfs >/dev/null; }
    pkg_verify() { dpkg --verify sacdfs; }
    # Minimal images (Ubuntu's) leave /usr/share/doc out; a real system installs it.
    sed -i '\#^path-exclude=*/usr/share/doc/#d' /etc/dpkg/dpkg.cfg.d/* 2>/dev/null || :
    step "tools and fuse3 (before the snapshot)"
    apt-get update -q >/dev/null
    apt-get install -y -q --no-install-recommends fuse3 systemd findutils diffutils procps >/dev/null
fi

ver=$("$tardir/sacdfs" --version | awk '{print $2}')
mapfile -t cur < <(find "$pkgdir" -maxdepth 1 -name "sacdfs[-_]${ver/-/[~-]}[-_]*.$fmt")
[[ ${#cur[@]} == 1 ]] || fail "not one $fmt of $ver in $pkgdir: ${cur[*]}"
mapfile -t prev < <(find "$pkgdir" -maxdepth 1 -name "sacdfs[-_]*.$fmt" ! -path "${cur[0]}")
[[ ${#prev[@]} == 1 ]] || fail "not one previous $fmt in $pkgdir: ${prev[*]}"
echo "this build: ${cur[0]##*/}; previous: ${prev[0]##*/}"

# Every file outside the package databases, caches and pseudo file systems.
snapshot() {
    find / -xdev \( -path /proc -o -path /sys -o -path /dev -o -path /run -o -path /tmp \
        -o -path /var/cache -o -path /var/log -o -path /var/lib/rpm -o -path /var/lib/dnf \
        -o -path /usr/lib/sysimage -o -path /var/lib/dpkg -o -path /var/lib/apt \
        -o -path /etc/ld.so.cache -o -path "$pkgdir" -o -path "$tardir" -o -path "$images" \
        -o -path /work \) -prune -o -print | LC_ALL=C sort
}
hashes() { (cd "$1" && find . -type f | LC_ALL=C sort | while read -r f; do sha256sum "$f"; done); }

step "reference: the tarball's binary serves the corpus"
mkdir -p /mnt/ref /mnt/pkg
"$tardir/sacdfs" "$images" /mnt/ref 2>/tmp/ref.log
hashes /mnt/ref > /tmp/ref.sha256
umount /mnt/ref
[[ -s /tmp/ref.sha256 ]] || fail "the tarball's binary served no files"
wc -l < /tmp/ref.sha256

snapshot > /tmp/before.txt

step "install ${cur[0]##*/}"
pkg_install "${cur[0]}"
out=$(pkg_verify 2>&1) || fail "package verify: $out"
[[ -z $out ]] || fail "package verify: $out"
/usr/bin/sacdfs --version | grep -qx "sacdfs $ver" || fail "--version: $(/usr/bin/sacdfs --version)"
cmp /usr/bin/sacdfs "$tardir/sacdfs" || fail "/usr/bin/sacdfs is not the tarball's binary"
for f in /usr/bin/sacdfs-ctl /usr/lib/systemd/system/sacdfs.service "$conf"; do
    [[ -f $f ]] || fail "$f missing"
done
# The documents as listed by the package (Debian's container images don't install docs).
[[ $fmt == rpm ]] && copying=/usr/share/licenses/sacdfs/COPYING || copying=/usr/share/doc/sacdfs/COPYING
if [[ $fmt == rpm ]]; then files=$(rpm -qlp "${cur[0]}"); else files=$(dpkg-deb -c "${cur[0]}" | awk '{print substr($6, 2)}'); fi
for f in "$copying" /usr/share/doc/sacdfs/README.md /usr/share/doc/sacdfs/CHANGELOG.md; do
    grep -qx "$f" <<<"$files" || fail "$f not in the package"
done
grep -qx "EnvironmentFile=$conf" /usr/lib/systemd/system/sacdfs.service || fail "unit's EnvironmentFile"
grep -qx "CacheDirectory=sacdfs" /usr/lib/systemd/system/sacdfs.service || fail "unit's CacheDirectory"
grep -q '^ExecStart=/usr/bin/sacdfs ' /usr/lib/systemd/system/sacdfs.service || fail "unit's ExecStart"
! grep -rn /usr/local/bin/sacdfs /usr/bin/sacdfs-ctl /usr/lib/systemd/system/sacdfs.service "$conf" ||
    fail "/usr/local/bin/sacdfs still named"
[[ $(systemctl is-enabled sacdfs.service 2>/dev/null) == disabled ]] || fail "enabled on install"
step "systemd-analyze verify"
systemd-analyze verify /usr/lib/systemd/system/sacdfs.service 2>&1 | tee /tmp/verify.log
grep -v -e 'PermissionsStartOnly= is deprecated' -e '^$' /tmp/verify.log | grep . &&
    fail "systemd-analyze verify reported more than PermissionsStartOnly's deprecation"

step "the package's binary serves the same files"
/usr/bin/sacdfs "$images" /mnt/pkg 2>/tmp/pkg.log
hashes /mnt/pkg > /tmp/pkg.sha256
umount /mnt/pkg
cmp /tmp/ref.sha256 /tmp/pkg.sha256 || fail "hashes differ from the tarball's binary"

step "sacdfs-ctl with the package's settings file"
cp -p "$conf" /tmp/conf.orig
sed -i "s#^SACDFS_IMAGES=.*#SACDFS_IMAGES=$images#; s#^SACDFS_MOUNT=.*#SACDFS_MOUNT=/mnt/pkg#" "$conf"
sacdfs-ctl start
sacdfs-ctl status
hashes /mnt/pkg > /tmp/ctl.sha256
sacdfs-ctl stop
cmp /tmp/ref.sha256 /tmp/ctl.sha256 || fail "sacdfs-ctl mount: hashes differ"
cp -p /tmp/conf.orig "$conf"
rm -f /var/run/sacdfs.pid /var/log/sacdfs.log

step "remove"
pkg_remove
pkg_purge
snapshot > /tmp/after.txt
diff /tmp/before.txt /tmp/after.txt || fail "removal left files behind (or removed some)"

step "previous ${prev[0]##*/}, enabled, settings edited; upgrade"
pkg_install "${prev[0]}"
systemctl enable sacdfs.service 2>&1
echo "SACDFS_OPTIONS=\"--allow-other --stop-grace 12\"" >> "$conf"
cp -p "$conf" /tmp/conf.edited
pkg_install "${cur[0]}" 2>&1 | tee /tmp/upgrade.log
/usr/bin/sacdfs --version | grep -qx "sacdfs $ver" || fail "not upgraded"
cmp "$conf" /tmp/conf.edited || fail "edited settings changed on upgrade"
[[ $(systemctl is-enabled sacdfs.service) == enabled ]] || fail "no longer enabled after upgrade"
out=$(pkg_verify 2>&1 || :)
echo "$out"
grep -v " c $conf\$" <<<"$out" | grep . && fail "verify after upgrade: more than the edited settings"
pkg_remove
snapshot > /tmp/after.txt
if [[ $fmt == rpm ]]; then left="$conf.rpmsave"; else left=$conf; fi
diff /tmp/before.txt /tmp/after.txt | tee /tmp/left.diff || :
[[ "$(diff /tmp/before.txt /tmp/after.txt | grep '^[<>]')" == "> $left" ]] ||
    fail "removal after upgrade: expected only $left left"
cmp "$left" /tmp/conf.edited || fail "$left is not the edited settings"
[[ -z $(find /etc/systemd -name 'sacdfs*') ]] || fail "enablement left behind"
pkg_purge
rm -f "$left"
diff /tmp/before.txt <(snapshot) || fail "purge left files behind"
echo "PASS: $PRETTY_NAME, $(uname -m), ${cur[0]##*/}"
