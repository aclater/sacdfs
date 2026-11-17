#!/bin/sh
# sacdfs package, before removal (rpm %preun: $1 = 0 erase, 1 upgrade; deb prerm: remove,
# upgrade, ...). On removal only: stop the service and disable it.
case $1 in
    0 | remove) ;;
    *) exit 0 ;;
esac
if command -v systemctl >/dev/null 2>&1; then
    systemctl --no-reload disable sacdfs.service >/dev/null 2>&1 || :
    [ -d /run/systemd/system ] && { systemctl stop sacdfs.service >/dev/null 2>&1 || :; }
fi
exit 0
