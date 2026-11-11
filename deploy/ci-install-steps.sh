#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# One run of deploy/install.sh (#340) the way users run it, piped from the release URL into
# sh, inside a container or on a host: --dry-run, a real install, a second run that must
# change nothing, the files against a reference, --uninstall twice, and the images folder
# untouched. Plain POSIX sh, for BusyBox. Called by deploy/ci-install-test.sh:
#
#   ci-install-steps.sh URL ISO_DIR REF EXPECT [install.sh options...]
#
# URL: the releases URL (its latest/download/install.sh is fetched); REF: sha256 lines of
# every file the mount should show; EXPECT: "install"; "refuse" where the binary can't run
# (the script must then stop before changing anything); "pointer" on TrueNAS and Unraid (it
# must only point to their app screens). Prints "RESULT <step>: pass|FAIL".
url=$1 iso=$2 ref=$3 expect=$4
shift 4
fails=0
# On a NAS, sacdfs's own folders go on the images' volume, next to the images folder.
vol=$(dirname "$iso")
res() {
    echo "RESULT $1: $2"
    case $2 in pass*) ;; *) fails=$((fails + 1)) ;; esac
}
fetch() {
    if command -v curl >/dev/null 2>&1; then curl -fsSL "$1"; else wget -q -O - "$1"; fi
}
# The one-liner: the installer from the "latest" URL, piped into sh.
oneliner() {
    fetch "$url/latest/download/install.sh" | SACDFS_RELEASES_URL=$url sh -s -- --iso-dir "$iso" "$@"
}
state() {
    ls -d /opt/sacdfs /opt/sacdfs/compose.yaml /srv/sacd /etc/default/sacdfs /etc/systemd/system/sacdfs.service \
        /usr/local/bin/sacdfs /usr/local/etc/init.d/S99sacdfs "$vol/sacdfs" "$vol/sacd" 2>/dev/null
    grep -c ' fuse' /proc/mounts
}
# same A B: equal files (cmp isn't in every image).
same() { [ "$(sha256sum <"$1")" = "$(sha256sum <"$2")" ]; }
isostate() { (cd "$iso" && ls -la . && sha256sum ./*.iso) | grep -v ' \.\.$'; }

isostate >/tmp/iso.before
state >/tmp/state0

echo "=== dry run"
oneliner --dry-run "$@"
rc=$?
state >/tmp/state1
if [ "$expect" = pointer ]; then
    # TrueNAS and Unraid: the script points to the app screens and installs nothing.
    if [ "$rc" = 0 ] && same /tmp/state0 /tmp/state1; then res dry-run "pass (nothing changed)"; else res dry-run FAIL; fi
    echo "=== install"
    oneliner "$@" >/tmp/pointer.log 2>&1
    rc=$?
    cat /tmp/pointer.log
    state >/tmp/state2
    if [ "$rc" = 0 ] && grep -q "doesn't" /tmp/pointer.log && same /tmp/state0 /tmp/state2; then
        res install "pass (pointed to the app screen, nothing changed)"
    else
        res install FAIL
    fi
elif [ "$expect" = refuse ]; then
    if [ "$rc" != 0 ] && same /tmp/state0 /tmp/state1; then res dry-run "pass (refused, nothing changed)"; else res dry-run FAIL; fi
    echo "=== install"
    oneliner "$@"
    rc=$?
    state >/tmp/state2
    if [ "$rc" != 0 ] && same /tmp/state0 /tmp/state2; then res install "pass (refused, nothing changed)"; else res install FAIL; fi
else
    if [ "$rc" = 0 ] && same /tmp/state0 /tmp/state1; then res dry-run "pass (nothing changed)"; else res dry-run FAIL; fi

    echo "=== install"
    oneliner "$@"
    rc=$?
    if [ "$rc" = 0 ]; then res install pass; else res install FAIL; fi
    # shellcheck disable=SC2016 # expanded by the sh -c below
    hashes='cd "$1" && find . -type f | LC_ALL=C sort | while read -r f; do sha256sum "$f"; done'
    if [ -f /opt/sacdfs/compose.yaml ]; then
        # The container route: the mount is inside the container, at /srv/sacd.
        docker exec sacdfs sh -c "$hashes" sh /srv/sacd >/tmp/got.sha 2>/dev/null
    else
        mnt=$(sed -n 's/^SACDFS_MOUNT=//p' /etc/default/sacdfs /opt/sacdfs/sacdfs.conf "$vol/sacdfs/sacdfs.conf" 2>/dev/null | head -n 1)
        sh -c "$hashes" sh "$mnt" >/tmp/got.sha 2>/dev/null
    fi
    if [ -s /tmp/got.sha ] && same /tmp/got.sha "$ref"; then
        res files "pass ($(wc -l </tmp/got.sha | tr -d ' ') files identical)"
    else
        res files FAIL
    fi

    echo "=== second run"
    oneliner "$@" >/tmp/again.log 2>&1
    rc=$?
    cat /tmp/again.log
    if [ "$rc" = 0 ] && grep -q "Nothing changed" /tmp/again.log && ! grep -q "running:\|wrote:" /tmp/again.log; then
        res second-run "pass (nothing changed)"
    else
        res second-run FAIL
    fi

    echo "=== uninstall"
    oneliner --uninstall "$@"
    rc=$?
    state >/tmp/state3
    if [ "$rc" = 0 ] && same /tmp/state0 /tmp/state3; then res uninstall "pass (back to the start)"; else res uninstall FAIL; fi
    oneliner --uninstall "$@" >/tmp/un2.log 2>&1
    if grep -q "Nothing of sacdfs" /tmp/un2.log; then res uninstall-again pass; else res uninstall-again FAIL; fi
fi

isostate >/tmp/iso.after
if same /tmp/iso.before /tmp/iso.after; then res iso-dir "pass (unchanged)"; else res iso-dir FAIL; fi
exit "$fails"
