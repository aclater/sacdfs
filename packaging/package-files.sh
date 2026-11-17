#!/bin/bash
# The systemd unit, sacdfs-ctl and the settings file with a package's paths (#460):
#   packaging/package-files.sh rpm|deb SACDFS_CTL SACDFS_SERVICE SACDFS_CONF_EXAMPLE OUTDIR
# writes OUTDIR/sacdfs-ctl, OUTDIR/sacdfs.service and OUTDIR/sacdfs.conf. The program is
# /usr/bin/sacdfs (install.sh's copies name /usr/local/bin/sacdfs); the settings are in
# /etc/sysconfig/sacdfs (rpm) or /etc/default/sacdfs (deb), read by both the unit and
# sacdfs-ctl. Each replacement must find its text, so a change to the source files stops here.
# Used by pack-linux.sh and by sacdfs.spec (bash and coreutils only).
set -euo pipefail

fmt=$1 out=$5
case $fmt in
    rpm) conf=/etc/sysconfig/sacdfs ;;
    deb) conf=/etc/default/sacdfs ;;
    *) echo "package-files.sh: rpm or deb, not $fmt" >&2; exit 2 ;;
esac
mkdir -p "$out"
install -m 0755 "$2" "$out/sacdfs-ctl"
install -m 0644 "$3" "$out/sacdfs.service"
install -m 0644 "$4" "$out/sacdfs.conf"

# sub FILE FROM TO: replace every FROM (a fixed string) in FILE, which must hold it.
sub() {
    local s
    s=$(cat "$1")
    [[ $s == *"$2"* ]] || { echo "package-files.sh: $(basename "$1") has no '$2'" >&2; exit 1; }
    printf '%s\n' "${s//"$2"/"$3"}" > "$1"
}
nl=$'\n'
sub "$out/sacdfs.service" "for the release binary at /usr/local/bin/sacdfs." "as packaged (/usr/bin/sacdfs)."
sub "$out/sacdfs.service" "# Install:  cp sacdfs.service /etc/systemd/system/ && systemctl daemon-reload$nl" ""
sub "$out/sacdfs.service" "ExecStart=/usr/local/bin/sacdfs " "ExecStart=/usr/bin/sacdfs "
sub "$out/sacdfs-ctl" 'BIN=${SACDFS_BIN:-/usr/local/bin/sacdfs}' 'BIN=${SACDFS_BIN:-/usr/bin/sacdfs}'
sub "$out/sacdfs.conf" "#SACDFS_BIN=/usr/local/bin/sacdfs" "#SACDFS_BIN=/usr/bin/sacdfs"
sub "$out/sacdfs.conf" "Copy to /etc/default/sacdfs and change the paths." "Change the paths, then: systemctl enable --now sacdfs."
if [[ $fmt == rpm ]]; then
    sub "$out/sacdfs.service" "# Settings: /etc/default/sacdfs" "# Settings: $conf"
    sub "$out/sacdfs.service" "EnvironmentFile=/etc/default/sacdfs" "EnvironmentFile=$conf"
    sub "$out/sacdfs-ctl" "# Reads /etc/default/sacdfs," "# Reads $conf,"
    sub "$out/sacdfs-ctl" 'CONF=${SACDFS_CONF:-/etc/default/sacdfs}' "CONF=\${SACDFS_CONF:-$conf}"
fi

# Nothing left that names install.sh's paths, and the unit and sacdfs-ctl agree.
fail() { echo "package-files.sh: $fmt: $*" >&2; exit 1; }
! grep -n '/usr/local/bin/sacdfs' "$out"/sacdfs* || fail "/usr/local/bin/sacdfs left"
[[ $fmt == deb ]] || ! grep -n '/etc/default/sacdfs' "$out"/sacdfs* || fail "/etc/default/sacdfs left"
grep -qx "EnvironmentFile=$conf" "$out/sacdfs.service" || fail "EnvironmentFile"
grep -qF "CONF=\${SACDFS_CONF:-$conf}" "$out/sacdfs-ctl" || fail "sacdfs-ctl's settings file"
grep -qx 'ExecStart=/usr/bin/sacdfs -f $SACDFS_OPTIONS ${SACDFS_IMAGES} ${SACDFS_MOUNT}' "$out/sacdfs.service" ||
    fail "ExecStart"
