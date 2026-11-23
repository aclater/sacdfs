#!/usr/bin/env bash
# Tests for scripts/check-private-names.sh: it must fail when it finds a private name and
# when it cannot search (exit 2), never pass. Run from anywhere: bash scripts/test-check-private-names.sh
set -uo pipefail

check=$(cd "$(dirname "$0")" && pwd)/check-private-names.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fails=0

# new_repo NAME: a throwaway repo with the script's known-commits list and one clean commit
new_repo() {
    local d="$work/$1"
    mkdir -p "$d/scripts" && cd "$d" || exit 1
    git init -q . && git config user.name "Test User" && git config user.email "test@example.org"
    : > scripts/private-names-known-commits
    echo "nothing private" > README
    git add -A && git commit -q -m init
}

# expect NAME WANTED_EXIT [ENV=VALUE ...]: run the check in the current repo
expect() {
    local name=$1 want=$2 got
    shift 2
    env "$@" bash "$check" >"$work/out" 2>&1
    got=$?
    if [ "$got" -eq "$want" ]; then
        echo "ok   $name"
    else
        echo "FAIL $name: exit $got, wanted $want"
        sed 's/^/     /' "$work/out"
        fails=$((fails + 1))
    fi
}

new_repo clean
expect "clean tree passes" 0 PRIVATE_NAMES=

new_repo home-arpa
echo "see host.home.arpa" > notes && git add -A && git commit -q -m add
expect "home.arpa name in a file fails" 1 PRIVATE_NAMES=

new_repo regex-hit
echo "the word zebrafish here" > notes && git add -A && git commit -q -m add
expect "PRIVATE_NAMES match in a file fails" 1 PRIVATE_NAMES=zebrafish

# Case 1 (Auditor): the file list spans several batches; only one holds a match
new_repo batches
mkdir many
for i in $(seq 1 3000); do echo x > "many/a-long-file-name-to-fill-the-argument-list-$i"; done
echo "zebrafish" > many/zzz-last
git add -A && git commit -q -m many
expect "a match among thousands of files fails" 1 PRIVATE_NAMES=zebrafish

# Case 2 (Auditor): a tracked symlink to a directory must not hide a match elsewhere
new_repo symlink
mkdir real && echo x > real/f && ln -s real link-to-dir
echo "zebrafish" > notes
git add -A && git commit -q -m link
expect "a directory symlink does not hide a match" 1 PRIVATE_NAMES=zebrafish
expect "a directory symlink alone passes" 0 PRIVATE_NAMES=nomatchanywhere

# Case 3 (Auditor): an invalid regex is a failed check (2), not a pass
new_repo badregex
expect "an invalid PRIVATE_NAMES regex fails the check" 2 'PRIVATE_NAMES=(unbalanced'

# An empty alternative would match every line: refused up front
new_repo emptyalt
expect "a trailing | is refused" 2 'PRIVATE_NAMES=abc|'
expect "a leading | is refused" 2 'PRIVATE_NAMES=|abc'
expect "a blank value is refused" 2 'PRIVATE_NAMES=  '

# Commit identities
new_repo identity
echo y > f && git add -A && git -c user.name="Zebrafish Person" commit -q -m id
expect "a PRIVATE_NAMES match in an identity fails" 1 PRIVATE_NAMES=zebrafish
expect "the same identity passes with no match" 0 PRIVATE_NAMES=nomatchanywhere
expect "an invalid regex fails the identity search too" 2 'PRIVATE_NAMES=(unbalanced'

new_repo ip-identity
echo y > f && git add -A && git -c user.email="me@192.168.1.5" commit -q -m id
expect "a private address in an identity fails" 1 PRIVATE_NAMES=

# Known commits are skipped
new_repo known
echo y > f && git add -A && git -c user.name="Zebrafish Person" commit -q -m id
git log -1 --format=%H > scripts/private-names-known-commits
git add -A && git commit -q -m known
expect "a listed known commit is skipped" 0 PRIVATE_NAMES=zebrafish

[ "$fails" -eq 0 ] && echo "all checks passed"
exit "$((fails > 0))"
