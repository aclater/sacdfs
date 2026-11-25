#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Lab test of part 2 with the REAL sacdfs binary on a real FUSE mount: after the mount the daemon
# holds no CAP_SYS_ADMIN (nor CAP_SYS_PTRACE or any other capability but the two it keeps) in ANY
# thread, the helper still holds its own, a stop works through the helper, and where the helper is
# gone the daemon says so and leaves the mount. As root with /dev/fuse, CAP_SYS_ADMIN and
# CAP_SETPCAP (a container: --device /dev/fuse --cap-add SYS_ADMIN, the engine's default set has
# SETPCAP).
#
#   bash tests/drop_caps_real.sh target/debug/sacdfs tests/corpus/images
#
# Ends with `drop_caps_real: N passed, 0 failed`; any other last line is a failure.
set -uo pipefail
bin=$(readlink -f "$1")
images=$(readlink -f "$2")
[[ -c /dev/fuse && $(id -u) == 0 ]] || { echo "SKIP: needs /dev/fuse and root"; exit 0; }
work=$(mktemp -d)
mnt=$work/mnt
log=$work/log
mkdir -p "$mnt"
pass=0 fail=0
ok() { echo "ok   $*"; pass=$((pass + 1)); }
bad() { echo "FAIL $*"; fail=$((fail + 1)); }
check() { local name=$1; shift; if "$@"; then ok "$name"; else bad "$name"; fi; }
mounted() { awk -v m="$mnt" '{ for (i = 7; i <= NF && $i != "-"; i++); if ($5 == m && $(i + 1) ~ /^fuse/) f = 1 } END { exit !f }' /proc/self/mountinfo; }
wait_for() { local i; for ((i = 0; i < ${2:-100}; i++)); do eval "$1" && return 0; sleep 0.1; done; return 1; }
unmounted() { ! mounted; }
alive() { [[ -d /proc/$1 ]] && ! grep -q '^State:.Z' "/proc/$1/status" 2>/dev/null; }
dead() { ! alive "$1"; }
first_child() {
    local s ppid
    for s in /proc/[0-9]*/stat; do
        ppid=$(sed 's/^.*) [A-Za-z] //; s/ .*//' "$s" 2>/dev/null) || continue
        [[ $ppid == "$1" ]] && { basename "$(dirname "$s")"; return; }
    done
}
start() { # [ENV=VALUE...]: sets $pid and $helper
    : > "$log"
    env "$@" "$bin" -f -vv "$images" "$mnt" 2> "$log" &
    pid=$!
    # Ready = the daemon's own "mounted" line: the mount shows in the table before the helper is armed
    # and the capabilities are dropped, and the checks below look at both.
    wait_for "grep -q ' mounted ' '$log'" 600 || { echo "FAIL: sacdfs did not mount; log:"; cat "$log"; exit 1; }
    helper=$(first_child "$pid")
    [[ -n $helper ]] || { echo "FAIL: no helper child; log:"; grep -v DEBUG "$log"; exit 1; }
}
cleanup() { umount -l "$mnt" 2>/dev/null; kill -9 "$pid" 2>/dev/null; rm -rf "$work"; }
trap cleanup EXIT

# hexfield FILE KEY: the hex value of "KEY:" in a /proc status file
hexfield() { awk -v k="$2:" '$1 == k { print $2 }' "$1"; }
# every_task KEY WANT: every task of the daemon has KEY == WANT
every_task() {
    local t v n=0
    for t in /proc/$pid/task/*; do
        v=$(hexfield "$t/status" "$1")
        [[ $v == "$2" ]] || { echo "   task $(basename "$t"): $1 = $v, wanted $2" >&2; return 1; }
        n=$((n + 1))
    done
    echo "   $n tasks have $1 = $2"
}

echo "== kernel $(uname -r)"
start
KEEP_MASK=0000000000000000   # the container grants neither DAC_OVERRIDE nor DAC_READ_SEARCH here unless asked
check "log: dropped CAP_SYS_ADMIN in N threads" grep -q 'dropped CAP_SYS_ADMIN and every capability but' "$log"
check "log: no 'bounding set NOT cut' (CAP_SETPCAP was there)" bash -c "! grep -q 'NOT cut' '$log'"
want=$(hexfield "/proc/$pid/status" CapEff)
echo "   daemon CapEff $want CapPrm $(hexfield "/proc/$pid/status" CapPrm) CapBnd $(hexfield "/proc/$pid/status" CapBnd) NoNewPrivs-line: $(grep NoNewPrivs "/proc/$pid/status" | tr -d '\t')"
check "daemon (every thread): no CAP_SYS_ADMIN, CAP_SYS_PTRACE, CAP_SETPCAP in the effective set" bash -c "
  for t in /proc/$pid/task/*; do [[ -r \$t/status ]] || continue; v=\$(awk '/^CapEff:/{print \$2}' \$t/status); (( 16#\$v & ((1<<21)|(1<<19)|(1<<8)) )) && { echo task \$t has \$v; exit 1; }; done; exit 0"
check "daemon (every thread): CapEff = CapPrm = CapBnd" bash -c "
  for t in /proc/$pid/task/*; do [[ -r \$t/status ]] || continue; set -- \$(awk '/^Cap(Eff|Prm|Bnd):/{print \$2}' \$t/status); [[ \$1 == \$2 && \$2 == \$3 ]] || { echo task \$t: \$*; exit 1; }; done"
check "daemon: more than one thread was cut" test "$(find /proc/$pid/task -mindepth 1 -maxdepth 1 | wc -l)" -gt 1
check "daemon (every thread): CapInh and CapAmb are 0" bash -c "
  for t in /proc/$pid/task/*; do [[ -r \$t/status ]] || continue; set -- \$(awk '/^Cap(Inh|Amb):/{print \$2}' \$t/status); [[ \$1 == 0000000000000000 && \$2 == 0000000000000000 ]] || { echo task \$t: \$*; exit 1; }; done"
check "daemon (every thread): NoNewPrivs 1" bash -c "
  for t in /proc/$pid/task/*; do [[ -r \$t/status ]] || continue; grep -q '^NoNewPrivs:.1' \$t/status || { echo task \$t; exit 1; }; done; exit 0"
check "log: seccomp deny-list on, umount2 included (the capability is gone)" grep -q 'seccomp deny-list on: .* umount2 included' "$log"
check "daemon (every thread): Seccomp 2 (the deny-list)" bash -c "
  for t in /proc/$pid/task/*; do [[ -r \$t/status ]] || continue; grep -q '^Seccomp:.2' \$t/status || { echo task \$t; exit 1; }; done; exit 0"
check "helper still holds CAP_SYS_ADMIN only (0x200000)" test "$(hexfield /proc/$helper/status CapEff)" = 0000000000200000
check "the mount serves files" bash -c "find '$mnt' -name '*.dsf' | head -n1 | grep -q ."

# stop through the helper
kill -TERM "$pid"
check "SIGTERM: mount gone (the helper unmounted: the daemon cannot)" wait_for unmounted 100
check "SIGTERM: sacdfs exits" wait_for "dead $pid" 100
# (The line "detached by the unmount helper" is debug-level and written by the signal thread, which can
# lose the race with the process exit once the mount is gone: the signal line, written before, is checked.)
check "SIGTERM: log says it is unmounting (signal 15)" grep -q 'signal 15: unmounting' "$log"

# the helper killed first: death logged, and the daemon says it cannot unmount
start
kill -KILL "$helper"
check "helper killed: its death is logged" wait_for "grep -q 'unmount helper ended without being asked' '$log'" 50
kill -TERM "$pid"
check "helper killed, then SIGTERM: the daemon says it cannot unmount from here" wait_for "grep -q 'cannot unmount .* from here' '$log'" 50
check "helper killed, then SIGTERM: the mount stays" mounted
umount -l "$mnt"
check "an outside unmount works" wait_for unmounted 50
kill -KILL "$pid" 2>/dev/null

# the escape hatch
start SACDFS_KEEP_SYS_ADMIN=1
check "SACDFS_KEEP_SYS_ADMIN=1: log says it keeps the capability" grep -q 'keeping CAP_SYS_ADMIN: SACDFS_KEEP_SYS_ADMIN is set' "$log"
check "SACDFS_KEEP_SYS_ADMIN=1: no_new_privs is not set either (the drop path only)" grep -q '^NoNewPrivs:.0' /proc/$pid/status
check "SACDFS_KEEP_SYS_ADMIN=1: the deny-list is on but umount2 is not on it" bash -c "grep -q 'seccomp deny-list on' '$log' && ! grep -q 'umount2 included' '$log'"
check "SACDFS_KEEP_SYS_ADMIN=1: the daemon still has it" bash -c "(( 16#$(hexfield /proc/$pid/status CapEff) & (1<<21) ))"
kill -TERM "$pid"
check "SACDFS_KEEP_SYS_ADMIN=1: a stop still works" wait_for unmounted 100

echo "drop_caps_real: $pass passed, $fail failed"
[[ $fail == 0 ]]
