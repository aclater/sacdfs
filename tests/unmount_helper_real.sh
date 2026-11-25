#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Lab test of the unmount helper with the REAL sacdfs binary on a real FUSE mount (the checks of
# STATUS section 11 that need no container engine): the helper is started and armed, hardened as
# documented, and does its job on a stop, after a crash (#57), with an outside unmount, and when
# it is killed first. As root with /dev/fuse and CAP_SYS_ADMIN (a container: --device /dev/fuse
# --cap-add SYS_ADMIN).
#
#   bash tests/unmount_helper_real.sh target/debug/sacdfs tests/corpus/images
#
# Ends with `unmount_helper_real: N passed, 0 failed`; any other last line is a failure.
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

# The first child of a process, from /proc (no pgrep in a minimal container).
first_child() {
    local s ppid
    for s in /proc/[0-9]*/stat; do
        ppid=$(sed 's/^.*) [A-Za-z] //; s/ .*//' "$s" 2>/dev/null) || continue
        [[ $ppid == "$1" ]] && { basename "$(dirname "$s")"; return; }
    done
}
start() { # sets $pid and $helper
    : > "$log"
    "$bin" -f -vv "$images" "$mnt" 2> "$log" &
    pid=$!
    # Ready = the daemon's own "mounted" line: the mount shows in the table before the helper is armed
    # and the capabilities are dropped, and the checks below look at both.
    wait_for "grep -q ' mounted ' '$log'" 600 || { echo "FAIL: sacdfs did not mount; log:"; cat "$log"; exit 1; }
    helper=$(first_child "$pid")
    [[ -n $helper ]] || { echo "FAIL: sacdfs ($pid) has no child process: the helper did not start; log:"; cat "$log"; exit 1; }
}
cleanup() { umount -l "$mnt" 2>/dev/null; kill -9 "$pid" 2>/dev/null; rm -rf "$work"; }
trap cleanup EXIT

echo "== kernel $(uname -r)"

# 1. started, armed, hardened
start
check "log: unmount helper started (once)" test "$(grep -c 'unmount helper started' "$log")" = 1
check "log: armed, with the proof level" grep -q 'unmount helper armed, identity proof level [123]' "$log"
check "log: no 'threads exist' refusal" bash -c "! grep -q 'no unmount helper' '$log'"
st=/proc/$helper/status
echo "   helper $helper: $(grep -E '^(CapEff|CapPrm|CapBnd|CapInh|CapAmb|Seccomp|NoNewPrivs):' "$st" | tr '\n\t' '  ')"
check "helper CapEff = CapPrm = CAP_SYS_ADMIN only" bash -c "[[ \$(awk '/^CapEff/{print \$2}' $st) == 0000000000200000 && \$(awk '/^CapPrm/{print \$2}' $st) == 0000000000200000 ]]"
check "helper CapInh = CapAmb = 0" bash -c "[[ \$(awk '/^CapInh/{print \$2}' $st) == 0000000000000000 && \$(awk '/^CapAmb/{print \$2}' $st) == 0000000000000000 ]]"
check "helper Seccomp 2, NoNewPrivs 1" bash -c "grep -q '^Seccomp:.2' $st && grep -q '^NoNewPrivs:.1' $st"
check "helper has five descriptors (0-2 /dev/null, socket, mount table)" test "$(find /proc/$helper/fd -type l | wc -l)" = 5
check "the mount serves files" bash -c "find '$mnt' -name '*.dsf' | head -n1 | grep -q ."

# 2. SIGTERM: unmounted by the helper, both gone
kill -TERM "$pid"
check "SIGTERM: mount gone" wait_for unmounted 100
check "SIGTERM: sacdfs exits" wait_for "dead $pid" 100
check "SIGTERM: helper gone" wait_for "dead $helper" 50
check "SIGTERM: log says the helper detached it" grep -q 'detached by the unmount helper' "$log"

# 3. crash (#57): SIGKILL of the daemon only
start
kill -KILL "$pid"
check "SIGKILL: mount gone within 2 s" wait_for unmounted 20
check "SIGKILL: no ENOTCONN left behind" bash -c "ls '$mnt' >/dev/null 2>&1 || ! ls '$mnt' 2>&1 | grep -q 'not connected'"
check "SIGKILL: helper gone" wait_for "dead $helper" 20
start
check "restart on the same path mounts at once" mounted

# 4. outside unmount while armed (no descriptor is held on the mount)
umount "$mnt"
check "outside umount succeeds while armed" wait_for unmounted 20
check "sacdfs exits after an outside unmount" wait_for "dead $pid" 100
check "helper gone after an outside unmount" wait_for "dead $helper" 50

# 5. helper killed first, then SIGTERM. A daemon that still has CAP_SYS_ADMIN unmounts from here;
#    one that dropped it (part 2) cannot, says so, and the mount stays until it is unmounted from
#    outside (the supervisor's job): tests/drop_caps_real.sh covers that side in full.
start
dropped=0
grep -q 'dropped CAP_SYS_ADMIN' "$log" && dropped=1
kill -KILL "$helper"
sleep 0.3
kill -TERM "$pid"
if [[ $dropped == 1 ]]; then
    check "helper killed first, capability dropped: the daemon says it cannot unmount from here" wait_for "grep -q 'cannot unmount .* from here' '$log'" 50
    umount -l "$mnt"
    check "helper killed first, capability dropped: an outside unmount works" wait_for unmounted 50
    kill -KILL "$pid" 2>/dev/null
else
    check "helper killed first: mount still gets unmounted" wait_for unmounted 100
    check "helper killed first: log says unmounting from here" grep -q 'unmounting from here' "$log"
fi

# (A successor on the same path being left alone is tests/unmount_helper_fuse.rs, case 7.)

echo "unmount_helper_real: $pass passed, $fail failed"
[[ $fail == 0 ]]
