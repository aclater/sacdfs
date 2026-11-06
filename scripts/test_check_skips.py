#!/usr/bin/env python3
"""Tests of scripts/check_skips.py: `python3 -I scripts/test_check_skips.py`."""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.dont_write_bytecode = True  # leaves no __pycache__ in the checkout
sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_skips  # noqa: E402

# As cargo prints it with CARGO_TERM_COLOR=always: the codes must not hide anything.
OUTPUT = """\
   \x1b[1m\x1b[92mCompiling\x1b[0m sacdfs v0.0.0
     \x1b[1m\x1b[92mRunning\x1b[0m unittests src/lib.rs (target/debug/deps/sacdfs-abc)

running 2 tests
test tree::tests::one ... \x1b[32mok\x1b[0m
test tree::tests::two ... \x1b[32mok\x1b[0m

test result: \x1b[32mok\x1b[0m. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     \x1b[1m\x1b[92mRunning\x1b[0m tests/export.rs (target/debug/deps/export-abc)

running 2 tests
test sidecar ... SKIPPED export::sidecar: needs a capability
ok
SKIPPED export::dirs: needs a capability
test dirs ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running tests/wav.rs (target/debug/deps/wav-abc)

running 2 tests
test real_discs ... ignored, needs real disc images: set SACDFS_REAL_IMAGES
test header ... ok

test result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s

   Doc-tests sacdfs

running 1 test
test src/lib.rs - a (line 3) ... ignored

test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s
"""

WANT = {
    "export::dirs": "needs a capability",
    "export::sidecar": "needs a capability",
    "wav::real_discs": "needs real disc images: set SACDFS_REAL_IMAGES",
    "doc::src/lib.rs - a (line 3)": "",
}


class Parse(unittest.TestCase):
    def test_reads_guards_ignored_and_counts(self):
        out = check_skips.parse(OUTPUT)
        self.assertEqual(out.skips, WANT)
        self.assertEqual((out.passed, out.failed, out.ignored_count), (5, 0, 2))
        self.assertEqual(out.ran, 3)  # libtest's 5 passed hold the 2 guards' tests

    def test_same_test_name_in_two_binaries_stays_apart(self):
        out = check_skips.parse(
            "Running tests/wav.rs (x)\ntest real_discs ... ignored, a\n"
            "Running tests/library_index.rs (y)\ntest real_discs ... ignored, b\n"
        )
        self.assertEqual(out.ignored, {"wav::real_discs": "a", "library_index::real_discs": "b"})


class Problems(unittest.TestCase):
    def check(self, want):
        return check_skips.problems(check_skips.parse(OUTPUT), want)

    def test_exactly_the_expected_skips_pass(self):
        self.assertEqual(self.check(WANT), [])

    def test_new_skip_fails(self):
        want = {k: v for k, v in WANT.items() if k != "export::dirs"}
        self.assertEqual(self.check(want), ["unexpected skip: export::dirs: needs a capability"])

    def test_stopped_skip_fails(self):
        bad = self.check({**WANT, "mount::gone": "x"})
        self.assertEqual(len(bad), 1)
        self.assertIn("no longer skips", bad[0])
        self.assertIn("mount::gone", bad[0])

    def test_changed_reason_fails(self):
        bad = self.check({**WANT, "export::dirs": "another reason"})
        self.assertEqual(len(bad), 1)
        self.assertIn("skip reason changed: export::dirs", bad[0])

    def test_no_tests_fails(self):
        self.assertIn("the output holds no test results", check_skips.problems(check_skips.parse(""), {}))

    def test_an_ignored_test_that_cannot_be_read_fails(self):
        # The count comes from libtest: a line lost in the output must not pass as "no skip".
        text = "Running tests/wav.rs (x)\ntest result: ok. 1 passed; 0 failed; 1 ignored; 0 measured;\n"
        bad = check_skips.problems(check_skips.parse(text), {})
        self.assertEqual(bad, ["libtest counts 1 ignored tests, but 0 could be read"])


class Main(unittest.TestCase):
    def run_main(self, text, job="tests", env=None):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "out.txt"
            path.write_text(text, encoding="utf-8")
            stdout, stderr = io.StringIO(), io.StringIO()
            with (
                contextlib.redirect_stdout(stdout),
                contextlib.redirect_stderr(stderr),
                mock.patch.dict(os.environ, env or {"GITHUB_STEP_SUMMARY": ""}),
            ):
                code = check_skips.main(["--job", job, str(path)])
            return code, stdout.getvalue(), stderr.getvalue()

    def test_unknown_job(self):
        code, _, err = self.run_main("", job="nonesuch")
        self.assertEqual(code, 2)
        self.assertIn("unknown job", err)

    def test_new_skip_prints_an_annotation_and_fails(self):
        text = "Running tests/x.rs (x)\nSKIPPED x::new: why\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n"
        code, out, _ = self.run_main(text, job="nfs")
        self.assertEqual(code, 1)
        self.assertIn("::error::unexpected skip: x::new: why", out)

    def test_summary_lists_counts_and_skips(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "summary.md"
            text = "Running tests/x.rs (x)\nSKIPPED x::new: why\ntest result: ok. 3 passed; 0 failed; 0 ignored;\n"
            self.run_main(text, job="nfs", env={"GITHUB_STEP_SUMMARY": str(path)})
            written = path.read_text(encoding="utf-8")
        self.assertIn("nfs: 2 tests ran, 1 skipped by a guard, 0 ignored, 1 problem(s)", written)
        self.assertIn("| `x::new` | guard | why |", written)


class ExpectedList(unittest.TestCase):
    def test_the_list_in_the_repository_is_well_formed(self):
        expected = check_skips.read_expected()
        for job in expected["jobs"]:
            listed = [t for e in expected["skips"] if job in e["jobs"] for t in e["tests"]]
            self.assertEqual(len(listed), len(set(listed)), f"a test is listed twice for {job}")

    def test_an_entry_for_an_unknown_job_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "e.json"
            path.write_text(
                '{"jobs": ["tests"], "skips": [{"why": "w", "reason": "r", "jobs": ["test"], "tests": ["t"]}]}',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "unknown jobs"):
                check_skips.read_expected(path)


if __name__ == "__main__":
    unittest.main()
