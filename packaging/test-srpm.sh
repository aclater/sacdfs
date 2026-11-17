#!/bin/bash
# Rebuilds the SRPM (#460) as root in a container of the target distribution, privileged with
# /dev/fuse, without network for the build, as COPR does; then installs the built package
# and checks that it serves the corpus as the release tarball's binary does:
#   packaging/test-srpm.sh SRPM TARBALL_DIR IMAGES
set -euo pipefail
srpm=$(realpath "$1") tardir=$(cd "$2" && pwd) images=$(cd "$3" && pwd)
. /etc/os-release
echo "$PRETTY_NAME, $(uname -m)"
dnf -y -q --setopt=install_weak_deps=False install rpm-build 'dnf-command(builddep)' findutils util-linux >/dev/null
dnf -y -q builddep "$srpm" >/dev/null
rustc --version
start=$(date +%s)
unshare --net rpmbuild --rebuild --define "_topdir /root/rpmbuild" "$srpm" > /tmp/rpmbuild.log 2>&1 ||
    { tail -n 60 /tmp/rpmbuild.log; exit 1; }
echo "rpmbuild: $(( $(date +%s) - start )) s, offline"
rpm=$(find /root/rpmbuild/RPMS -name 'sacdfs-*.rpm')
rpm -qpl "$rpm"
dnf -y -q install "$rpm" >/dev/null
rpm -V sacdfs
sacdfs --version
grep -qx 'EnvironmentFile=/etc/sysconfig/sacdfs' /usr/lib/systemd/system/sacdfs.service
grep -q '^ExecStart=/usr/bin/sacdfs ' /usr/lib/systemd/system/sacdfs.service
[[ $(systemctl is-enabled sacdfs.service 2>/dev/null) == disabled ]]
systemd-analyze verify /usr/lib/systemd/system/sacdfs.service
hashes() { (cd "$1" && find . -type f | LC_ALL=C sort | while read -r f; do sha256sum "$f"; done); }
mkdir -p /mnt/ref /mnt/pkg
"$tardir/sacdfs" "$images" /mnt/ref 2>/tmp/ref.log
hashes /mnt/ref > /tmp/ref.sha256
umount /mnt/ref
/usr/bin/sacdfs "$images" /mnt/pkg 2>/tmp/pkg.log
hashes /mnt/pkg > /tmp/pkg.sha256
umount /mnt/pkg
[[ -s /tmp/ref.sha256 ]]
cmp /tmp/ref.sha256 /tmp/pkg.sha256
echo "same $(wc -l < /tmp/pkg.sha256) files as the release binary"
dnf -y -q remove sacdfs >/dev/null
echo "PASS: $PRETTY_NAME, $(uname -m), ${rpm##*/}"
