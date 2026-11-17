#!/bin/sh
# sacdfs package, after removal or upgrade (rpm %postun, deb postrm).
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || :
fi
exit 0
