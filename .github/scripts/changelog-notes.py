# SPDX-License-Identifier: GPL-3.0-or-later
"""Prints the release notes of one version: its section of CHANGELOG.md.

    changelog-notes.py CHANGELOG.md VERSION [--dry-run]

The section starts at the heading `## [VERSION]` or `## [VERSION] - YYYY-MM-DD` and ends at
the next `## ` heading or at the link definitions (`[x]: url`) at the end of the file. Exits
with status 1 if the section is missing or empty, so that a release can't be published
without notes: the commit that is tagged must have renamed `## [Unreleased]` for the version.

With --dry-run (release.yml run by hand, nothing published), a missing section falls back to
`## [Unreleased]`, with a warning, so the rest of the release can be tried before the
version's section is written.
"""

import re
import sys


def find_section(lines: list[str], name: str) -> str | None:
    heading = re.compile(r"^## \[" + re.escape(name) + r"\](?: - \d{4}-\d{2}-\d{2})?[ \t]*$")
    for i, line in enumerate(lines):
        if heading.match(line):
            body = []
            for rest in lines[i + 1 :]:
                if rest.startswith("## ") or re.match(r"^\[[^\]]+\]:\s", rest):
                    break
                body.append(rest)
            return "\n".join(body).strip()
    return None


def main(argv: list[str]) -> int:
    args = [a for a in argv if a != "--dry-run"]
    dry_run = len(args) != len(argv)
    if len(args) != 2:
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    path, version = args
    try:
        with open(path, encoding="utf-8") as f:
            lines = f.read().splitlines()
    except OSError as e:
        print(f"::error::{path}: {e}", file=sys.stderr)
        return 1
    notes = find_section(lines, version)
    if notes is None and dry_run:
        print(f"::warning::{path} has no [{version}] section; dry run, using [Unreleased]",
              file=sys.stderr)
        notes = find_section(lines, "Unreleased")
    if notes is None:
        print(f"::error::{path} has no `## [{version}]` section", file=sys.stderr)
        return 1
    if not notes:
        print(f"::error::the release notes for {version} in {path} are empty", file=sys.stderr)
        return 1
    print(notes)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
