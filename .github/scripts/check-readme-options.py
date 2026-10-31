# SPDX-License-Identifier: GPL-3.0-or-later
"""Checks that the README's options table matches `sacdfs --help`.

    sacdfs --help | check-readme-options.py README.md

The table is the one whose header is `| Option | Meaning |`. Its rows must name exactly the
options of `--help`, in the same order: the short and long flags and the value name, written
as code spans (`-f`, `--foreground`; `--threads N`). An option whose help shows
`[default: X]` must have "default" and X in its row's meaning. Exits with status 1, listing
every difference, if they don't match.
"""

import re
import sys

# `  -f, --foreground`, `      --threads <N>`, `  -v, --verbose...`
HELP_OPTION = re.compile(r"^\s+(?:(-[A-Za-z]), )?(--[a-z0-9][a-z0-9-]*)(?: <([A-Z_]+)>)?(?:\.\.\.)?\s*$")
HELP_DEFAULT = re.compile(r"\[default: ([^\]]+)\]")


def help_options(text: str) -> list[tuple[tuple[str, str, str], str | None]]:
    """(short, long, value) and the default of every option in the Options section."""
    options: list[tuple[tuple[str, str, str], str | None]] = []
    in_options = False
    for line in text.splitlines():
        if re.match(r"^\S", line):
            in_options = line.startswith("Options:")
            continue
        if not in_options:
            continue
        m = HELP_OPTION.match(line)
        if m:
            options.append(((m.group(1) or "", m.group(2), m.group(3) or ""), None))
            continue
        d = HELP_DEFAULT.search(line)
        if d and options:
            options[-1] = (options[-1][0], d.group(1))
    return options


def readme_options(text: str) -> list[tuple[tuple[str, str, str], str]]:
    """(short, long, value) and the meaning of every row of the options table."""
    lines = text.splitlines()
    try:
        start = next(i for i, l in enumerate(lines) if re.match(r"^\|\s*Option\s*\|\s*Meaning\s*\|", l))
    except StopIteration:
        sys.exit("README: no table with the header | Option | Meaning |")
    rows = []
    for line in lines[start + 2 :]:
        if not line.startswith("|"):
            break
        cells = [c.strip() for c in line.strip().strip("|").split("|", 1)]
        if len(cells) != 2:
            sys.exit(f"README: options table row without two cells: {line}")
        short = long = value = ""
        for span in re.findall(r"`([^`]+)`", cells[0]):
            m = re.fullmatch(r"(--?[A-Za-z0-9][A-Za-z0-9-]*)(?: ([A-Z_]+))?", span)
            if not m:
                sys.exit(f"README: not an option in the first column: `{span}`")
            if m.group(1).startswith("--"):
                long, value = m.group(1), m.group(2) or ""
            else:
                short = m.group(1)
        rows.append(((short, long, value), cells[1]))
    return rows


def show(opt: tuple[str, str, str]) -> str:
    short, long, value = opt
    return ", ".join(x for x in (short, long + (f" {value}" if value else "")) if x)


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    with open(argv[0], encoding="utf-8") as f:
        readme = readme_options(f.read())
    helps = help_options(sys.stdin.read())
    if not helps:
        print("no options found in the --help text on standard input", file=sys.stderr)
        return 2
    errors = []
    want = [o for o, _ in helps]
    have = [o for o, _ in readme]
    if want != have:
        errors.append("the options differ (--help, then README):")
        errors.append("  --help: " + "; ".join(show(o) for o in want))
        errors.append("  README: " + "; ".join(show(o) for o in have))
        for o in want:
            if o not in have:
                errors.append(f"  missing in the README: {show(o)}")
        for o in have:
            if o not in want:
                errors.append(f"  not in --help: {show(o)}")
    meanings = dict(readme)
    for opt, default in helps:
        meaning = meanings.get(opt)
        if default is None or meaning is None:
            continue
        token = re.compile(r"(?<![\w.,-])" + re.escape(default) + r"(?![\w.,-])")
        if "default" not in meaning or not token.search(meaning):
            errors.append(f"{show(opt)}: --help says [default: {default}], the README's meaning doesn't")
    if errors:
        print("README.md's options table doesn't match `sacdfs --help`:", file=sys.stderr)
        for e in errors:
            print(e, file=sys.stderr)
        return 1
    print(f"README options table matches --help: {len(want)} options")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
