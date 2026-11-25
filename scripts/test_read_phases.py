"""Tests for read_phases.py's check (python3 -I -m unittest discover -s scripts -p test_read_phases.py)."""

import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
TOOL = os.path.join(HERE, "read_phases.py")


def check(base, head, *extra):
    with tempfile.TemporaryDirectory() as d:
        b, h = os.path.join(d, "b.json"), os.path.join(d, "h.json")
        for path, data in ((b, base), (h, head)):
            with open(path, "w") as f:
                json.dump(data, f)
        return subprocess.run(
            [sys.executable, "-I", TOOL, "check", "--base", b, "--head", h, *extra],
            check=False,
            capture_output=True,
            text=True,
        )


def phase(calls, nbytes):
    return {"files": 1, "calls": calls, "bytes": nbytes}


class Check(unittest.TestCase):
    def test_equal_passes(self):
        r = check({"headtail": phase(19815, 1416357584)}, {"headtail": phase(19815, 1416357584)})
        self.assertEqual(r.returncode, 0, r.stdout)

    def test_the_second_mount_phase_is_checked_too(self):
        r = check({"headtail2": phase(19815, 1416357584)}, {"headtail2": phase(20522, 1528744192)})
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("FAIL headtail2 bytes", r.stdout)
        r = check({"headtail2": phase(19815, 1528744192)}, {"headtail2": phase(19815, 1416357584)})
        self.assertEqual(r.returncode, 0, r.stdout)

    def test_eight_percent_more_bytes_fails(self):
        r = check({"headtail": phase(19815, 1416357584)}, {"headtail": phase(20522, 1528744192)})
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("FAIL headtail  bytes", r.stdout)

    def test_warmer_noise_passes(self):
        # three calls and 335,872 bytes after 90 s idle, from nothing: the folder warmer
        r = check({"idle90": phase(0, 0)}, {"idle90": phase(3, 335872)})
        self.assertEqual(r.returncode, 0, r.stdout)

    def test_a_few_calls_of_noise_pass(self):
        r = check({"headtail": phase(19815, 1416357584)}, {"headtail": phase(19845, 1416750000)})
        self.assertEqual(r.returncode, 0, r.stdout)

    def test_threshold_is_an_option(self):
        r = check({"headtail": phase(19815, 1416357584)}, {"headtail": phase(20522, 1528744192)}, "--max-percent", "10")
        self.assertEqual(r.returncode, 0, r.stdout)

    def test_a_phase_the_base_lacks_is_skipped(self):
        r = check({"list": phase(0, 0)}, {"list": phase(0, 0), "headtail": phase(5, 5 << 30)})
        self.assertEqual(r.returncode, 0, r.stdout)


if __name__ == "__main__":
    unittest.main()
