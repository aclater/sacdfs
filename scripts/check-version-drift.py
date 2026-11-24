#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Versions the docs and deploy files name, against the ones they must match.

- Cargo.toml's version (a final release, not -rc) against what users install from: the
  install guides' commands, download links and "You should see sacdfs X" lines
  (docs/install/*.md; table rows and history lines such as "Checked on 0.6.0" are skipped),
  deploy/nas-compose.yaml, the Unraid template and the TrueNAS app (image tag, app_version).
- The scarletbook pins quoted in docs/design.md and docs/pcm-quality.md against Cargo.toml.
- CHANGELOG.md's "## [X.Y.Z] - DATE" headings against the published GitHub releases (date
  in UTC), through the GitHub API; --offline skips it.

    scripts/check-version-drift.py [--offline]

Exit 1 with one line per problem, file:line: what.
"""

import json
import os
import re
import subprocess
import sys
import urllib.request

REPO = "dsd-toolbox/sacdfs"
V = r"\d+\.\d+\.\d+(?:-rc\.\d+)?"
# How a guide tells users which release to take.
GUIDE_FORMS = [
    re.compile(rf"ghcr\.io/aclater/sacdfs:({V})"),
    re.compile(rf"releases/download/v({V})/"),
    re.compile(rf"\bsacdfs[-_]({V})[-_.~]"),
    re.compile(rf"--version\s+({V})\b"),
    re.compile(rf"You should see\*?\s*`?sacdfs ({V})"),
]
# Lines (and tables after such a line, and "What was checked" sections) that record what was
# checked on an earlier release, not what to install.
HISTORY = re.compile(r"\b(checked|validated|tested|untested|earlier|last run)\b", re.IGNORECASE)


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def cargo():
    text = read("Cargo.toml")
    version = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE).group(1)
    pins = dict(re.findall(r'^(scarletbook(?:-pcm)?)\s*=\s*"=([^"]+)"', text, re.MULTILINE))
    return version, pins


def line_of(text, pos):
    return text.count("\n", 0, pos) + 1


def check_guides(version, problems):
    folder = "docs/install"
    for name in sorted(os.listdir(folder)):
        if not name.endswith(".md"):
            continue
        path = f"{folder}/{name}"
        history_section = False
        context, blank = "", True  # the last paragraph, for a table after it
        for n, line in enumerate(read(path).splitlines(), 1):
            if line.startswith("#"):
                history_section = bool(HISTORY.search(line))
                continue
            if history_section:
                continue
            if line.lstrip().startswith("|"):
                if HISTORY.search(context):
                    continue
            elif not line.strip():
                blank = True
            else:
                context = line if blank else context + " " + line
                blank = False
                if HISTORY.search(line):
                    continue
            for form in GUIDE_FORMS:
                for found in form.findall(line):
                    if found != version:
                        problems.append(f"{path}:{n}: names sacdfs {found}, Cargo.toml is {version}")


def check_deploy(version, problems):
    checks = [
        ("deploy/nas-compose.yaml", rf"image:\s*ghcr\.io/aclater/sacdfs:({V})"),
        ("deploy/unraid/sacdfs.xml", rf"<Repository>ghcr\.io/aclater/sacdfs:({V})</Repository>"),
        ("deploy/truenas/sacdfs/ix_values.yaml", rf"repository:\s*ghcr\.io/aclater/sacdfs\s*\n\s*tag:\s*\"?({V})"),
        ("deploy/truenas/sacdfs/app.yaml", rf"^app_version:\s*({V})\s*$"),
    ]
    for path, pattern in checks:
        text = read(path)
        found = list(re.finditer(pattern, text, re.MULTILINE))
        if not found:
            problems.append(f"{path}: no sacdfs version found (pattern {pattern!r}); update this check")
        for m in found:
            if m.group(1) != version:
                problems.append(
                    f"{path}:{line_of(text, m.start(1))}: names sacdfs {m.group(1)}, Cargo.toml is {version}"
                )


def check_pins(pins, problems):
    sb, pcm = pins.get("scarletbook"), pins.get("scarletbook-pcm")
    path = "docs/design.md"
    text = read(path)
    m = re.search(rf"The pin is scarletbook ({V}), from crates\.io \(`=({V})`\)", text)
    if not m:
        problems.append(f'{path}: no "The pin is scarletbook X, from crates.io (`=X`)" line; update this check')
    else:
        for i in (1, 2):
            if m.group(i) != sb:
                problems.append(f"{path}:{line_of(text, m.start(i))}: scarletbook {m.group(i)}, Cargo.toml pins ={sb}")
    path = "docs/pcm-quality.md"
    text = read(path)
    # "`scarletbook-pcm` (0.7.0)", or "(from 0.5.3; main pins 0.7.0, ...)", in its first mention.
    m = re.search(rf"`scarletbook-pcm` \((?:[^)]*?main pins )?({V})\b", text)
    if not m:
        problems.append(f'{path}: no "`scarletbook-pcm` (X)" or "main pins X" mention; update this check')
    elif m.group(1) != pcm:
        problems.append(f"{path}:{line_of(text, m.start(1))}: scarletbook-pcm {m.group(1)}, Cargo.toml pins ={pcm}")


def releases():
    """{version: UTC publish date} of the published releases, from the GitHub API (GH_TOKEN or
    GITHUB_TOKEN if set, else anonymous)."""
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    published = {}
    for page in range(1, 20):
        req = urllib.request.Request(
            f"https://api.github.com/repos/{REPO}/releases?per_page=100&page={page}",
            headers={"Accept": "application/vnd.github+json", "User-Agent": "sacdfs-docs-check"},
        )
        if token:
            req.add_header("Authorization", f"Bearer {token}")
        with urllib.request.urlopen(req, timeout=30) as r:
            batch = json.load(r)
        for rel in batch:
            if not rel["draft"] and rel["published_at"]:
                published[rel["tag_name"].removeprefix("v")] = rel["published_at"][:10]
        if len(batch) < 100:
            return published
    return published


def check_changelog(version, problems):
    published = releases()
    path = "CHANGELOG.md"
    text = read(path)
    for m in re.finditer(r"^## \[([^\]]+)\] - (\d{4}-\d\d-\d\d)", text, re.MULTILINE):
        v, date = m.groups()
        n = line_of(text, m.start())
        if v not in published:
            if v != version:  # the release pull request's own version isn't tagged yet
                problems.append(f"{path}:{n}: {v} has no published release")
        elif published[v] != date:
            problems.append(f"{path}:{n}: {v} dated {date}, released {published[v]} (UTC)")


def main(argv):
    top = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
    ).stdout.strip()
    os.chdir(top)
    version, pins = cargo()
    problems = []
    if "-" in version:
        print(f"Cargo.toml is {version}, a pre-release: install guides and deploy files not checked", file=sys.stderr)
    else:
        check_guides(version, problems)
        check_deploy(version, problems)
    check_pins(pins, problems)
    if "--offline" not in argv:
        check_changelog(version, problems)
    for p in problems:
        print(p)
    print(f"check-version-drift: sacdfs {version}, {len(problems)} problems", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
