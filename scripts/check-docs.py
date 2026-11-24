#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Offline checks of the Markdown files: no network, so GitHub's 429 and 503 can't fail it.

- Relative links (and links to github.com/dsd-toolbox/sacdfs/blob|tree/main/...) must name a file
  or folder in the checkout; a #fragment on a Markdown file must be a heading anchor
  (GitHub's slugs, with -1, -2 for repeats) or an <a id/name> in that file.
- No link or mention of the old repository name aclater/sacd (now dsd-toolbox/dsd-toolbox) in
  any tracked file; aclater/sacd-spec is fine (it stays under aclater).

    scripts/check-docs.py [FILE.md ...]     (default: every tracked .md but tests/corpus/)

Exit 1 with one line per problem, file:line: what.
"""

import os
import re
import subprocess
import sys
import urllib.parse

SKIP_PREFIXES = ("tests/corpus/",)  # a verbatim copy of sacd-spec's corpus, links to that repo
SELF = re.compile(r"^https://github\.com/dsd-toolbox/sacdfs/(?:blob|tree)/main/([^?#]*)(#.*)?$")
OLD_REPO = re.compile(r"aclater/sacd(?![\w-])")
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})")
INLINE_CODE = re.compile(r"(`+)(.+?)\1")
LINK = re.compile(r"!?\[(?:[^\[\]]|\[[^\]]*\])*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
REFDEF = re.compile(r"^ {0,3}\[[^\]]+\]:\s*<?(\S+?)>?(?:\s|$)")
HREF = re.compile(r"""(?:href|src)\s*=\s*["']([^"']+)["']""", re.IGNORECASE)
HEADING = re.compile(r"^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$")
SETEXT = re.compile(r"^ {0,3}(=+|-+)\s*$")
HTML_ANCHOR = re.compile(r"""<a\s[^>]*?(?:id|name)\s*=\s*["']([^"']+)["']""", re.IGNORECASE)


def slug(text):
    """GitHub's heading anchor: the rendered text, lower case, punctuation but - and _ dropped."""
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)  # links and images: their text
    text = re.sub(r"<[^>]+>", "", text)  # inline HTML
    text = text.replace("`", "")
    text = re.sub(r"(\*+|(?<!\w)_+|_+(?!\w))", "", text)  # emphasis markers, not snake_case
    text = text.strip().lower()
    text = re.sub(r"[^\w\- ]", "", text)
    return text.replace(" ", "-")


def lines_outside_code(path, keep_code=False):
    """(line number, text) of the lines outside fenced code blocks; inline code spans are
    removed (links in them aren't links) unless keep_code (headings keep their text)."""
    out = []
    fence = None
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            m = FENCE.match(line)
            if fence:
                if m and m.group(1)[0] == fence[0] and len(m.group(1)) >= len(fence):
                    fence = None
                continue
            if m:
                fence = m.group(1)
                continue
            line = line.rstrip("\n")
            out.append((n, line if keep_code else INLINE_CODE.sub("", line)))
    return out


_anchors = {}


def anchors(path):
    if path not in _anchors:
        seen = {}
        found = set()
        prev = None
        for _, line in lines_outside_code(path, keep_code=True):
            text = None
            m = HEADING.match(line)
            if m:
                text = m.group(2)
            elif prev and prev.strip() and SETEXT.match(line) and not HEADING.match(prev):
                text = prev.strip()
            if text is not None:
                s = slug(text)
                k = seen.get(s, 0)
                seen[s] = k + 1
                found.add(s if k == 0 else f"{s}-{k}")
            for a in HTML_ANCHOR.findall(line):
                found.add(a)
            prev = line
        _anchors[path] = found
    return _anchors[path]


def check_target(src, n, target, problems):
    m = SELF.match(target)
    if m:
        rel, frag = m.group(1), (m.group(2) or "")[1:]
        dest = rel.rstrip("/") or "."
    else:
        if re.match(r"^[a-z][a-z0-9+.-]*:", target, re.IGNORECASE) or target.startswith("//"):
            return  # external: the online check's job
        path, _, frag = target.partition("#")
        path = urllib.parse.unquote(path.split("?")[0])
        if not path:
            dest = src
        elif path.startswith("/"):
            dest = path.lstrip("/")
        else:
            dest = os.path.normpath(os.path.join(os.path.dirname(src), path))
    if not os.path.exists(dest):
        problems.append(f"{src}:{n}: {target}: no such file or folder ({dest})")
        return
    frag = urllib.parse.unquote(frag)
    if frag and dest.endswith(".md") and os.path.isfile(dest) and frag not in anchors(dest):
        problems.append(f"{src}:{n}: {target}: no anchor #{frag} in {dest}")


def check_file(src, problems):
    for n, line in lines_outside_code(src):
        for t in LINK.findall(line) + HREF.findall(line):
            check_target(src, n, t, problems)
        m = REFDEF.match(line)
        if m:
            check_target(src, n, m.group(1), problems)


def main(argv):
    top = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
    ).stdout.strip()
    os.chdir(top)
    files = (
        argv or subprocess.run(["git", "ls-files", "*.md"], capture_output=True, text=True, check=True).stdout.split()
    )
    files = [f for f in files if not f.startswith(SKIP_PREFIXES)]
    problems = []
    for f in files:
        check_file(f, problems)
    # The old name, in every tracked file (code, workflows and comments too), but this one.
    grep = subprocess.run(
        ["git", "grep", "-n", "-I", "-P", OLD_REPO.pattern, "--", ".", ":!tests/corpus/", ":!scripts/check-docs.py"],
        capture_output=True,
        text=True,
        check=False,  # exit 1: no match
    )
    for hit in grep.stdout.splitlines():
        if argv and hit.split(":", 1)[0] not in argv:
            continue
        where, _, text = hit.partition(":")
        n, _, text = text.partition(":")
        problems.append(f"{where}:{n}: names aclater/sacd, now dsd-toolbox/dsd-toolbox: {text.strip()[:120]}")
    for p in problems:
        print(p)
    print(f"check-docs: {len(files)} files, {len(problems)} problems", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
