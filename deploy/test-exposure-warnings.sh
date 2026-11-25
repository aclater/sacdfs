#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Test of the exposure warnings of deploy/entrypoint.sh (warn_exposure): which settings say
# "every client may connect". The function is cut out of the entry point and run on its own,
# with the same `on` and `warn_setting`, so no container, mount or capability is needed.
#
#   bash deploy/test-exposure-warnings.sh
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
src=$here/entrypoint.sh
fail=0

def=$(sed -n '/^warn_exposure() {/,/^}/p' "$src")
[ -n "$def" ] || { echo "FAIL: warn_exposure not found in $src"; exit 1; }
helpers=$(sed -n '/^warn_setting() /p;/^on() /p;/^log() /p' "$src")

# run "ENV..." -> the warnings written
run() {
    env -i PATH="$PATH" "$@" bash -c "
        set -u
        SHARE=sacd
        $helpers
        $def
        warn_exposure" 2>&1
}
expect() { # expect "name" "pattern-or-NONE" env...
    local name=$1 want=$2 out
    shift 2
    out=$(run "$@")
    if [ "$want" = NONE ]; then
        if [ -z "$out" ]; then echo "ok   $name"; else echo "FAIL $name: wanted no output, got: $out"; fail=1; fi
    elif grep -q -- "$want" <<<"$out"; then echo "ok   $name"
    else echo "FAIL $name: wanted /$want/, got: $out"; fail=1; fi
}

expect "default: SMB guest warning" "neither SMB_USER nor SMB_GUEST=1"
expect "default: NFS warning" "NFS_CLIENTS is not set"
expect "SMB_USER set: no SMB warning" NONE SMB_USER=bob NFS_CLIENTS=192.0.2.0/24
expect "SMB_GUEST=1: no SMB warning" NONE SMB_GUEST=1 NFS_CLIENTS=198.51.100.0/24
expect "SMB_GUEST=yes counts too" NONE SMB_GUEST=yes NFS_CLIENTS=198.51.100.7
expect "SMB_GUEST=0 still warns" "neither SMB_USER nor SMB_GUEST=1" SMB_GUEST=0 NFS_CLIENTS=198.51.100.7
expect "SMB_USER with SMB_GUEST: guest ignored" "SMB_GUEST=1 is ignored" SMB_USER=bob SMB_GUEST=1 NFS_CLIENTS=198.51.100.7
expect "NFS_CLIENTS=*: warns" "NFS_CLIENTS contains \*" SMB_GUEST=1 'NFS_CLIENTS=*'
expect "NFS_CLIENTS list with * in it: warns" "NFS_CLIENTS contains \*" SMB_GUEST=1 'NFS_CLIENTS=192.0.2.0/24,*'
expect "NFS_CLIENTS=0.0.0.0/0: warns" "NFS_CLIENTS contains a network that is every address" SMB_GUEST=1 NFS_CLIENTS=0.0.0.0/0
expect "NFS_CLIENTS=::/0: warns" "NFS_CLIENTS contains a network that is every address" SMB_GUEST=1 NFS_CLIENTS=::/0
expect "NFS_CLIENTS list with 0.0.0.0/0 in it: warns" "NFS_CLIENTS contains a network that is every address" SMB_GUEST=1 NFS_CLIENTS=192.0.2.0/24,0.0.0.0/0
expect "a /16 is not every address" NONE SMB_GUEST=1 NFS_CLIENTS=198.51.0.0/16
expect "SMB=0: no SMB warning" NONE SMB=0 NFS_CLIENTS=198.51.100.7
expect "NFS=0: no NFS warning" NONE NFS=0 SMB_GUEST=1
expect "SMB=0 NFS=0: nothing" NONE SMB=0 NFS=0
# the second run of the entry point (fewer capabilities) must not repeat the warnings
expect "second run is silent" NONE SACDFS_CAPS_SET=1
exit $fail
