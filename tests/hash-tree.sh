#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Test of .github/scripts/hash-tree.sh: the same list as the old find|sort|while loop on a clean
# tree, and a non-zero exit (with no success on a short list) when something cannot be read.
# Unreadable files and directories are only unreadable to a user other than root: as root the
# permission cases are skipped and say so.
set -uo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
tool="$here/.github/scripts/hash-tree.sh"
fail=0
check() { # check "name" expected-rc actual-rc
  if [[ "$2" == "$3" ]]; then echo "ok   $1"; else echo "FAIL $1: wanted rc $2, got $3"; fail=1; fi
}
work=$(mktemp -d)
trap 'chmod -R u+rwx "$work" 2>/dev/null; rm -rf "$work"' EXIT

mkdir -p "$work/t/sub" "$work/t/sp ace"
echo a > "$work/t/a"; echo b > "$work/t/sub/b"; echo c > "$work/t/sp ace/c d"
printf 'x' > "$work/t/back\\slash"

# 1. a clean tree: same lines as the loop it replaces
"$tool" "$work/t" > "$work/new" 2> "$work/err"; check "clean tree rc" 0 $?
(cd "$work/t" && find . -type f | LC_ALL=C sort | while read -r f; do sha256sum "$f"; done) > "$work/old.raw" 2>/dev/null
# the old loop mangles a backslash name (read -r keeps it, sha256sum then escapes): compare only the
# names without one
grep -v 'slash' "$work/new" > "$work/new.f"; grep -v 'slash' "$work/old.raw" > "$work/old.f"
cmp -s "$work/new.f" "$work/old.f"; check "same lines as the old loop (names without a backslash)" 0 $?
[[ $(wc -l < "$work/new") == 4 ]]; check "four files listed (space and backslash names included)" 0 $?

# 2. a missing directory and a wrong argument count
"$tool" "$work/none" > /dev/null 2>&1; rc=$?
[[ $rc != 0 ]]; check "missing directory fails (rc $rc)" 0 $?
"$tool" > /dev/null 2>&1; check "no argument fails" 2 $?

# 3. an empty tree is a valid, empty list (the callers check the line count themselves)
mkdir "$work/empty"; "$tool" "$work/empty" > "$work/e.out" 2>&1; check "empty tree rc" 0 $?

# 4. unreadable file / directory
if [[ $(id -u) == 0 ]]; then
  echo "SKIP unreadable-file cases: running as root reads everything"
else
  chmod 000 "$work/t/a"
  "$tool" "$work/t" > /dev/null 2>&1; rc=$?
  [[ $rc != 0 ]]; check "unreadable file fails (rc $rc)" 0 $?
  chmod 644 "$work/t/a"
  chmod 000 "$work/t/sub"
  "$tool" "$work/t" > /dev/null 2>&1; rc=$?
  [[ $rc != 0 ]]; check "unreadable directory fails (rc $rc)" 0 $?
  # the old loop passed this one: find's error was lost in the pipe
  (cd "$work/t" && set -e +o pipefail && find . -type f | LC_ALL=C sort | while read -r f; do sha256sum "$f"; done) > /dev/null 2>&1
  check "(the old loop on an unreadable directory: documents why it was replaced)" 0 $?
  chmod 755 "$work/t/sub"
fi
exit $fail
