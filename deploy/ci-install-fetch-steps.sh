#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# A failed or cut-short download of deploy/install.sh must fail where the user sees it, and
# change nothing (#352). Plain POSIX sh, for BusyBox. Called by deploy/ci-install-test.sh:
#
#   ci-install-fetch-steps.sh URL ISO_DIR
#
# URL: the releases URL served by that test, with cut/install.sh (all but the last line) and
# half/install.sh (the first half) next to latest/download/install.sh. Prints
# "RESULT <step>: pass|FAIL".
url=$1 iso=$2
fails=0
res() {
    echo "RESULT $1: $2"
    case $2 in pass*) ;; *) fails=$((fails + 1)) ;; esac
}
# The documented forms: curl where it is, else wget (BusyBox's).
pipe() {
    if command -v curl >/dev/null 2>&1; then curl -fsSL "$1"; else wget -q -O - "$1"; fi
}
download() {
    if command -v curl >/dev/null 2>&1; then curl -fsSL -o "$2" "$1"; else wget -O "$2" "$1"; fi
}
state() {
    ls -d /opt/sacdfs /srv/sacd /etc/default/sacdfs /etc/systemd/system/sacdfs.service \
        /usr/local/bin/sacdfs "$(dirname "$iso")/sacdfs" "$(dirname "$iso")/sacd" 2>/dev/null
    grep -cE ' fuse(\.[^ ]+)? ' /proc/mounts
}
same() { [ "$(sha256sum <"$1")" = "$(sha256sum <"$2")" ]; }
state >/tmp/fetch.state0

# A pipe that ends early: between two commands (no syntax error, so only the script's own
# check can tell), and in the middle of one.
for cut in cut half; do
    echo "=== one-liner, download $cut short"
    pipe "$url/$cut/install.sh" | sh -s -- --iso-dir "$iso" 2>/tmp/fetch.err
    rc=$?
    cat /tmp/fetch.err
    state >/tmp/fetch.state1
    if [ "$rc" != 0 ] && [ -s /tmp/fetch.err ] && same /tmp/fetch.state0 /tmp/fetch.state1; then
        res "one-liner-$cut" "pass (status $rc, nothing changed)"
    else
        res "one-liner-$cut" "FAIL (status $rc)"
    fi
done
# The message names the cause (the last run above cut in the middle; this one between commands).
if pipe "$url/cut/install.sh" | sh -s -- --iso-dir "$iso" 2>&1 | grep -q "download was cut short"; then
    res cut-message pass
else
    res cut-message FAIL
fi

# Download, then run: a download that fails (TLS against a plain HTTP server, as BusyBox's
# own TLS fails against GitHub; and a missing file) stops before sh, with a status.
tls=$(echo "$url" | sed 's|^http:|https:|')
for src in "$tls/latest/download/install.sh" "$url/latest/download/no-such-install.sh"; do
    echo "=== download, then run: $src"
    rm -f /tmp/fetch-install.sh
    download "$src" /tmp/fetch-install.sh && sh /tmp/fetch-install.sh --iso-dir "$iso"
    rc=$?
    state >/tmp/fetch.state1
    case $src in https:*) step=download-tls ;; *) step=download-missing ;; esac
    if [ "$rc" != 0 ] && same /tmp/fetch.state0 /tmp/fetch.state1; then
        res "$step" "pass (status $rc, sh not run, nothing changed)"
    else
        res "$step" "FAIL (status $rc)"
    fi
done
rm -f /tmp/fetch-install.sh
exit "$fails"
