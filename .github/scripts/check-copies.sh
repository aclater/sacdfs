#!/bin/sh
# Values kept in several files on purpose must stay equal, and the shipped container
# definitions must keep their hardening. Run from the repository root; status 1 on a mismatch.
#   a. one RUST_STABLE across the workflows;
#   b. entry_unsafe and path_unsafe identical in deploy/install.sh and deploy/sacdfs-ctl;
#   c. the shipped container definitions drop all capabilities, set no-new-privileges, and are
#      never privileged (the TrueNAS app's library drops all and sets no-new-privileges itself).
set -u
fail=0
bad() { echo "check-copies: $*" >&2; fail=1; }

# a
n=$(grep -h '^  RUST_STABLE:' .github/workflows/*.yml | sort -u | wc -l)
[ "$n" -eq 1 ] || bad "RUST_STABLE has $n values in .github/workflows: $(grep -H '^  RUST_STABLE:' .github/workflows/*.yml | tr '\n' ' ')"

# b: a function runs from its "name() {" line to the first "}" in column 1.
body() { awk -v f="$1() {" '$0 == f { p = 1 } p { print } p && /^}/ { exit }' "$2"; }
for f in entry_unsafe path_unsafe; do
    a=$(body "$f" deploy/install.sh) b=$(body "$f" deploy/sacdfs-ctl)
    [ -n "$a" ] || bad "$f not found in deploy/install.sh"
    [ "$a" = "$b" ] || bad "$f differs between deploy/install.sh and deploy/sacdfs-ctl"
done

# c
for f in deploy/compose.yaml deploy/nas-compose.yaml; do
    if ! grep -Eq '^ +cap_drop:' "$f" || ! grep -Eq '^ +- ALL$' "$f"; then bad "$f: no cap_drop ALL"; fi
    grep -q 'no-new-privileges:true' "$f" || bad "$f: no no-new-privileges"
    ! grep -Eq '^ +privileged:' "$f" || bad "$f: privileged"
done
f=deploy/unraid/sacdfs.xml
grep -q -- '--cap-drop=ALL' "$f" || bad "$f: no --cap-drop=ALL"
grep -q 'no-new-privileges:true' "$f" || bad "$f: no no-new-privileges"
grep -q '<Privileged>false</Privileged>' "$f" || bad "$f: Privileged is not false"
! grep -q -- '--privileged' "$f" || bad "$f: --privileged"
! grep -rqi 'privileged' deploy/truenas/sacdfs/templates || bad "deploy/truenas/sacdfs/templates: privileged"
! grep -Eqi -- '--privileged|privileged *[:=] *true' Containerfile || bad "Containerfile: privileged"

exit "$fail"
