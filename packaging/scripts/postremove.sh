#!/bin/sh
# sacdfs package, after removal or upgrade (rpm %postun: $1 = 0 erase, 1 upgrade; deb postrm:
# remove, purge, upgrade, ...). The service's seek index cache goes on erase (rpm) or purge
# (deb), with the settings.
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || :
fi
case $1 in
    0 | purge) rm -rf /var/cache/sacdfs ;;
esac
exit 0
