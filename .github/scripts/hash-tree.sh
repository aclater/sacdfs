#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Usage: hash-tree.sh DIR
#
# Prints "sha256sum ./path" for every regular file under DIR, in byte order of the path, one per
# line: the comparison list of the release checks (what a mount serves, what another build serves).
#
# It FAILS, with a non-zero exit, if anything under DIR cannot be listed or read: a directory find
# cannot enter, a file sha256sum cannot open, a missing DIR. A list that silently lacks a file
# would compare equal to another list that lacks the same file, so a short list is an error, never
# a result. (The loops this replaces were `find | sort | while read; do sha256sum; done`: without
# pipefail an unreadable directory made find skip what was inside it and the step still passed.)
# NUL-separated, so a name with a newline, a space or a backslash is hashed as it is.
set -euo pipefail

(($# == 1)) || { echo "usage: hash-tree.sh DIR" >&2; exit 2; }
cd -- "$1"
find . -type f -print0 | LC_ALL=C sort -z | xargs -0 -r -n1 sha256sum
