#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Lab test of the shipped systemd unit with the REAL binary, as real root on a real host (a VM: root in a
# user namespace, as the container scripts have, is not what a service manager gives). The unit's own
# SystemCallFilter and bounding set are underneath, so this is also where the unit's filter meets the
# helper's, the capability drop's and the deny-list's calls.
#
#   bash tests/systemd_real.sh BINARY UNIT IMAGES_DIR
#
# Installs BINARY as /usr/local/bin/sacdfs and UNIT as sacdfs.service, runs it on IMAGES_DIR, mounted at
# /srv/sacd, and ends with `systemd_real: N passed, 0 failed`. It leaves nothing running.
set -uo pipefail
bin=$(readlink -f "$1") unit=$(readlink -f "$2") images=$(readlink -f "$3")
[[ $(id -u) == 0 && -d /run/systemd/system ]] || { echo "SKIP: needs root and systemd"; exit 0; }
mnt=/srv/sacd
pass=0 fail=0
ok() { echo "ok   $*"; pass=$((pass + 1)); }
bad() { echo "FAIL $*"; fail=$((fail + 1)); }
check() { local name=$1; shift; if "$@"; then ok "$name"; else bad "$name"; fi; }
mounted() { awk -v m="$mnt" '{ for (i = 7; i <= NF && $i != "-"; i++); if ($5 == m && $(i + 1) ~ /^fuse/) f = 1 } END { exit !f }' /proc/self/mountinfo; }
unmounted() { ! mounted; }
wait_for() { local i; for ((i = 0; i < ${2:-100}; i++)); do eval "$1" && return 0; sleep 0.2; done; return 1; }
hexfield() { awk -v k="$2:" '$1 == k { print $2 }' "$1"; }
first_child() { local s p; for s in /proc/[0-9]*/stat; do p=$(sed 's/^.*) [A-Za-z] //; s/ .*//' "$s" 2>/dev/null) || continue; [[ $p == "$1" ]] && { basename "$(dirname "$s")"; return; }; done; }
jlog() { journalctl -u sacdfs --no-pager -o cat --since "$since" 2>/dev/null; }
cleanup() { systemctl stop sacdfs 2>/dev/null; umount -l "$mnt" 2>/dev/null; }
trap cleanup EXIT

echo "== $(uname -r), systemd $(systemctl --version | head -n1)"
install -m 0755 "$bin" /usr/local/bin/sacdfs
install -m 0644 "$unit" /etc/systemd/system/sacdfs.service
printf 'SACDFS_IMAGES=%s\nSACDFS_MOUNT=%s\nSACDFS_OPTIONS="-vv"\n' "$images" "$mnt" > /etc/default/sacdfs
systemctl daemon-reload
echo "   $(systemd-analyze security --offline=true /etc/systemd/system/sacdfs.service 2>&1 | tail -n 1 | sed 's/\x1b\[[0-9;]*m//g')"

start() { since=$(date '+%Y-%m-%d %H:%M:%S'); systemctl start sacdfs; }
start
check "unit starts (Type=notify ready)" systemctl is-active --quiet sacdfs
check "mounted" wait_for mounted 50
pid=$(systemctl show -p MainPID --value sacdfs)
helper=$(first_child "$pid")
echo "   --- log (info lines of interest)"
jlog | grep -E "unmount helper|dropped CAP|keeping CAP|deny-list|could not|cannot|mounted /" | sed 's/^/   /' | head -n 12
check "log: unmount helper armed" bash -c "journalctl -u sacdfs --no-pager -o cat --since '$since' | grep -q 'unmount helper armed, identity proof level'"
echo "   proof level: $(jlog | grep -o 'identity proof level [0-9]' | head -n1)"
check "a helper process exists" test -n "$helper"
check "log: dropped CAP_SYS_ADMIN" bash -c "journalctl -u sacdfs --no-pager -o cat --since '$since' | grep -q 'dropped CAP_SYS_ADMIN'"
check "log: seccomp deny-list on, umount2 included" bash -c "journalctl -u sacdfs --no-pager -o cat --since '$since' | grep -q 'seccomp deny-list on: .* umount2 included'"
echo "   daemon: CapEff $(hexfield /proc/$pid/status CapEff) CapBnd $(hexfield /proc/$pid/status CapBnd) $(grep -E 'NoNewPrivs|Seccomp:' /proc/$pid/status | tr -d '\t' | tr '\n' ' ')"
[[ -n $helper ]] && echo "   helper: CapEff $(hexfield /proc/$helper/status CapEff) CapBnd $(hexfield /proc/$helper/status CapBnd)"
check "daemon (every thread): no CAP_SYS_ADMIN / SYS_PTRACE / SETPCAP effective" bash -c "
  for t in /proc/$pid/task/*; do [[ -r \$t/status ]] || continue; v=\$(awk '/^CapEff:/{print \$2}' \$t/status); (( 16#\$v & ((1<<21)|(1<<19)|(1<<8)) )) && exit 1; done; exit 0"
check "daemon (every thread): NoNewPrivs 1 and Seccomp 2" bash -c "
  for t in /proc/$pid/task/*; do [[ -r \$t/status ]] || continue; grep -q '^NoNewPrivs:.1' \$t/status && grep -q '^Seccomp:.2' \$t/status || exit 1; done; exit 0"
check "the mount serves files (a DSF reads, non-empty)" bash -c "f=\$(find $mnt -name '*.dsf' | head -n1); [[ -n \$f ]] && [[ \$(head -c 1048576 \"\$f\" | wc -c) -gt 0 ]]"
# stop through the helper
t0=$(date +%s%N)
systemctl stop sacdfs
check "systemctl stop: unmounted" unmounted
echo "   stop took $(( ($(date +%s%N) - t0) / 1000000 )) ms"
check "stop: log says it is unmounting" bash -c "journalctl -u sacdfs --no-pager -o cat --since '$since' | grep -q 'signal 15: unmounting'"
# crash: SIGKILL of the daemon only (Restart=on-failure brings it back after 5 s)
start; wait_for mounted 50; pid=$(systemctl show -p MainPID --value sacdfs); helper=$(first_child "$pid")
kill -KILL "$pid"
check "SIGKILL of the daemon: the helper detaches the mount (or the unit restarted it)" wait_for "unmounted || [[ \$(systemctl show -p MainPID --value sacdfs) != $pid ]]" 40
check "...and the unit comes back mounted (Restart=on-failure)" wait_for mounted 100
systemctl stop sacdfs; wait_for unmounted 50
# helper killed first, then stop: the unit's ExecStopPost umount -l cleans up
start; wait_for mounted 50; pid=$(systemctl show -p MainPID --value sacdfs); helper=$(first_child "$pid")
[[ -n $helper ]] && kill -KILL "$helper"
sleep 1
check "helper killed: its death is logged" bash -c "journalctl -u sacdfs --no-pager -o cat --since '$since' | grep -q 'ended without being asked to'"
t0=$(date +%s%N)
systemctl stop sacdfs
check "helper killed, systemctl stop: the mount is gone (ExecStopPost)" unmounted
echo "   stop took $(( ($(date +%s%N) - t0) / 1000000 )) ms"
check "no dead mount left (ls the mountpoint)" bash -c "ls $mnt >/dev/null 2>&1"
# the escape hatch
printf 'SACDFS_IMAGES=%s\nSACDFS_MOUNT=%s\nSACDFS_OPTIONS="-vv"\nSACDFS_KEEP_SYS_ADMIN=1\n' "$images" "$mnt" > /etc/default/sacdfs
start; wait_for mounted 50; pid=$(systemctl show -p MainPID --value sacdfs)
check "SACDFS_KEEP_SYS_ADMIN=1: kept" bash -c "(( 16#$(hexfield /proc/$pid/status CapEff) & (1<<21) ))"
systemctl stop sacdfs
check "SACDFS_KEEP_SYS_ADMIN=1: stop unmounts" unmounted

echo "systemd_real: $pass passed, $fail failed"
[[ $fail == 0 ]]
