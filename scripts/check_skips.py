#!/usr/bin/env python3
"""Fail when a CI job's skipped tests aren't exactly the ones expected (#207).

    cargo test --locked 2>&1 | tee test-output.txt
    python3 -I scripts/check_skips.py --job NAME test-output.txt

libtest has no way to skip at run time: a guard that returns early reports as `ok`, and no count
shows it. So guards use `common::skip!` (tests/common/mod.rs), which prints `SKIPPED test: reason`
past libtest's capture, and `#[ignore]` tests print their reason themselves. Every skip a job may
have is listed in tests/expected-skips.json, with its job, its reason as the test prints it, and why
it's legitimate there. This fails on a new skip, on a skip that has stopped (remove it from the
list), on a changed reason, and on output with no tests. It reads cargo's output and not a report
file, so that nothing has to reach a test run under sudo or in a container. The counts and the list
go to the job summary when GITHUB_STEP_SUMMARY names a file.

The method follows pysacd's tools/check_skips.py (aclater/pysacd#131).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

EXPECTED = Path(__file__).resolve().parents[1] / "tests" / "expected-skips.json"

ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
RUNNING = re.compile(r"^\s*Running (?:unittests )?(\S+)")
DOC_TESTS = re.compile(r"^\s*Doc-tests ")
IGNORED = re.compile(r"^test (.+?) \.\.\. ignored(?:, (.*))?$")
RESULT = re.compile(r"^test result: \S+ (\d+) passed; (\d+) failed; (\d+) ignored;")
# Not anchored: the line is written past libtest's capture, so it can land right after the
# "test name ... " that libtest has written so far on another thread.
SKIPPED = re.compile(r"\bSKIPPED (\S+): (.*)$")


@dataclass
class Output:
    """What one cargo test run shows. A guard's test also counts in `passed`, as libtest sees it."""

    passed: int = 0
    failed: int = 0
    ignored_count: int = 0
    ignored: dict[str, str] = field(default_factory=dict)
    guarded: dict[str, str] = field(default_factory=dict)

    @property
    def skips(self) -> dict[str, str]:
        return {**self.ignored, **self.guarded}

    @property
    def ran(self) -> int:
        return self.passed + self.failed - len(self.guarded)


def parse(text: str) -> Output:
    """Read cargo test's output. Tests are named `binary::path`, as `common::skip!` names them."""
    out = Output()
    binary = "unknown"
    for raw in text.splitlines():
        line = ANSI.sub("", raw).rstrip("\r")
        if m := RUNNING.match(line):
            binary = Path(m[1]).stem
        elif DOC_TESTS.match(line):
            binary = "doc"
        elif m := IGNORED.match(line):
            out.ignored[f"{binary}::{m[1]}"] = m[2] or ""
        elif m := RESULT.match(line):
            out.passed += int(m[1])
            out.failed += int(m[2])
            out.ignored_count += int(m[3])
        elif m := SKIPPED.search(line):
            out.guarded[m[1]] = m[2]
    return out


def read_expected(path: Path = EXPECTED) -> dict:
    """The list of expected skips, refusing an entry that can't apply to a job or says no why."""
    expected = json.loads(path.read_text(encoding="utf-8"))
    for entry in expected["skips"]:
        unknown = set(entry["jobs"]) - set(expected["jobs"])
        if unknown:
            raise ValueError(f"{path.name}: unknown jobs {sorted(unknown)} in {entry['tests']}")
        if not entry["why"].strip():
            raise ValueError(f"{path.name}: no why for {entry['tests']}")
    return expected


def expected_for(job: str, expected: dict) -> dict[str, str]:
    """{test: reason} of the entries that apply to a job."""
    return {t: e["reason"] for e in expected["skips"] if job in e["jobs"] for t in e["tests"]}


def problems(out: Output, want: dict[str, str]) -> list[str]:
    bad = []
    if not out.passed + out.failed + out.ignored_count:
        bad.append("the output holds no test results")
    if out.ignored_count != len(out.ignored):
        bad.append(f"libtest counts {out.ignored_count} ignored tests, but {len(out.ignored)} could be read")
    skips = out.skips
    bad += [f"unexpected skip: {t}: {skips[t]}" for t in sorted(skips.keys() - want.keys())]
    bad += [f"no longer skips (remove it from {EXPECTED.name}): {t}" for t in sorted(want.keys() - skips.keys())]
    bad += [
        f"skip reason changed: {t}: expected {want[t]!r}, got {skips[t]!r}"
        for t in sorted(skips.keys() & want.keys())
        if skips[t] != want[t]
    ]
    return bad


def counts(job: str, out: Output, bad: list[str]) -> str:
    return (
        f"{job}: {out.ran} tests ran, {len(out.guarded)} skipped by a guard, "
        f"{out.ignored_count} ignored, {len(bad)} problem(s)"
    )


def summary(job: str, out: Output, bad: list[str]) -> str:
    """The job summary: libtest's `ok` includes the guards' tests, so the counts are told apart."""
    lines = [f"## Test skips: {job}", "", counts(job, out, bad), ""]
    if bad:
        lines += [f"- **{line}**" for line in bad] + [""]
    if out.skips:
        lines += ["| Test | Skipped by | Reason |", "|---|---|---|"]
        lines += [f"| `{t}` | guard | {r} |" for t, r in sorted(out.guarded.items())]
        lines += [f"| `{t}` | `#[ignore]` | {r} |" for t, r in sorted(out.ignored.items())]
        lines.append("")
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--job", required=True)
    ap.add_argument("output", help="cargo test's stdout and stderr")
    args = ap.parse_args(argv)
    expected = read_expected()
    if args.job not in expected["jobs"]:
        print(f"unknown job {args.job!r}: not in {EXPECTED.name}", file=sys.stderr)
        return 2
    out = parse(Path(args.output).read_text(encoding="utf-8", errors="replace"))
    bad = problems(out, expected_for(args.job, expected))
    for line in bad:
        print(f"::error::{line}")
    print(counts(args.job, out, bad))
    if path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(path, "a", encoding="utf-8") as f:
            f.write(summary(args.job, out, bad))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
