#!/usr/bin/env bash
# Fail if commit identities or tracked files carry private details (adapted from pysacd).
# - Commit identities (author and committer): names in the home.arpa special-use domain,
#   private (RFC 1918) IPv4 addresses, and the regex in $PRIVATE_NAMES (a repository
#   variable, kept outside the repo).
# - Tracked files: home.arpa names and $PRIVATE_NAMES. Generic paths such as /mnt/smb and
#   private example addresses are normal in a filesystem's docs and tests, so they aren't
#   flagged here; real addresses belong in $PRIVATE_NAMES.
# Commits listed in scripts/private-names-known-commits are already public in main's
# history (and in an immutable tag), so they are skipped. Never add new ones there.
# Commits are checked from $BASE_SHA (exclusive) to HEAD, or all of history.
set -euo pipefail

files='\.home\.arpa\b'
ids="$files"'|\b192\.168\.[0-9]{1,3}\.[0-9]{1,3}\b|\b10\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\b'
ids="$ids"'|\b172\.(1[6-9]|2[0-9]|3[01])\.[0-9]{1,3}\.[0-9]{1,3}\b'
if [ -n "${PRIVATE_NAMES:-}" ]; then
    files="$files|$PRIVATE_NAMES"
    ids="$ids|$PRIVATE_NAMES"
fi
status=0

# This script spells the patterns out, and the known-commits list names a domain
if git ls-files -z | grep -zvE '^scripts/(check-private-names\.sh|private-names-known-commits)$' \
        | xargs -0 grep -nIiE "$files" -- 2>/dev/null; then
    echo "::error::private details found in tracked files (matches above)"
    status=1
fi

range=HEAD
if [ -n "${BASE_SHA:-}" ] && [ "${BASE_SHA}" != "0000000000000000000000000000000000000000" ] \
        && git cat-file -e "${BASE_SHA}^{commit}" 2>/dev/null; then
    range="${BASE_SHA}..HEAD"
fi
known=$(grep -oE '^[0-9a-f]{7,40}' scripts/private-names-known-commits 2>/dev/null | tr '\n' '|' | sed 's/|$//')
if git rev-parse --verify -q HEAD >/dev/null; then
    hits=$(git log --format='%H %h %an <%ae> %cn <%ce>' "$range" | grep -iE "$ids" || true)
    [ -n "$known" ] && hits=$(printf '%s\n' "$hits" | grep -vE "^($known)" || true)
    if [ -n "$hits" ]; then
        printf '%s\n' "$hits" | cut -d' ' -f2-
        echo "::error::private details found in commit identities (matches above)"
        status=1
    fi
fi

[ "$status" -eq 0 ] && echo "no private details found"
exit "$status"
