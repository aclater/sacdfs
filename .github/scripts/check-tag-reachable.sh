#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# A release tag must point at a commit that is on main or on a release branch (release/X.Y.Z,
# for a patch release cut from an earlier tag). Used by .github/workflows/release.yml before
# anything is built: a tag pushed on any other commit, such as one on a pull request's branch
# or a commit that was never pushed to a branch, publishes nothing.
#
#   check-tag-reachable.sh COMMIT [REMOTE]
#
# REMOTE (default origin) is fetched first: its main and every release/* branch, with their
# history, into refs/remotes/REMOTE/. Exits 0 and names the branch if COMMIT is an ancestor of
# (or equal to) one of them, 1 if it is on none, 2 on a usage or fetch error.
set -eu
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || {
    echo "usage: check-tag-reachable.sh COMMIT [REMOTE]" >&2
    exit 2
}
remote=${2:-origin}
commit=$(git rev-parse --verify --quiet "$1^{commit}") || {
    echo "check-tag-reachable.sh: not a commit: $1" >&2
    exit 2
}

# A shallow clone has too little history to tell; fetch all of it for these branches.
unshallow=
[ "$(git rev-parse --is-shallow-repository)" = true ] && unshallow=--unshallow
# shellcheck disable=SC2086 # $unshallow is empty or one word
git fetch --quiet --no-tags $unshallow "$remote" \
    "+refs/heads/main:refs/remotes/$remote/main" \
    "+refs/heads/release/*:refs/remotes/$remote/release/*" || {
    echo "check-tag-reachable.sh: could not fetch $remote's main and release/* branches" >&2
    exit 2
}

for ref in $(git for-each-ref --format='%(refname)' "refs/remotes/$remote/main" "refs/remotes/$remote/release/"); do
    if git merge-base --is-ancestor "$commit" "$ref"; then
        echo "check-tag-reachable.sh: $commit is on ${ref#refs/remotes/}"
        exit 0
    fi
done
echo "check-tag-reachable.sh: $commit is on neither $remote/main nor any $remote/release/* branch" >&2
exit 1
