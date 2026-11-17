#!/bin/sh
# sacdfs package, after install or upgrade (rpm %post: $1 = 1 install, 2 upgrade; deb
# postinst: configure [old version]). The service is never enabled or started here.
case $1 in
    configure | [0-9]*) ;;
    *) exit 0 ;;
esac
upgrade=
case $1 in configure) [ -n "${2:-}" ] && upgrade=1 ;; *) [ "$1" -ge 2 ] && upgrade=1 ;; esac

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || :
    # A running sacdfs restarts on the new program; a stopped one stays stopped.
    [ -n "$upgrade" ] && { systemctl try-restart sacdfs.service >/dev/null 2>&1 || :; }
fi

# An install.sh setup (systemd route) beside the package.
warn() { echo "sacdfs: warning: $*" >&2; }
[ -e /usr/local/bin/sacdfs ] &&
    warn "/usr/local/bin/sacdfs (from install.sh) is still there; the package's program is /usr/bin/sacdfs"
[ -e /etc/systemd/system/sacdfs.service ] &&
    warn "/etc/systemd/system/sacdfs.service (from install.sh) overrides the package's unit; remove it and run systemctl daemon-reload to use the package's"
if [ -e /etc/default/sacdfs ] && [ -e /etc/sysconfig/sacdfs ] &&
    grep -q '^EnvironmentFile=/etc/sysconfig/sacdfs' /usr/lib/systemd/system/sacdfs.service 2>/dev/null; then
    warn "the package's unit and sacdfs-ctl read /etc/sysconfig/sacdfs, not /etc/default/sacdfs (from install.sh)"
fi
exit 0
