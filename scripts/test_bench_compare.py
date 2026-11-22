#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Tests of the report of scripts/bench_compare.py: `python3 -I scripts/test_bench_compare.py`.
Nothing is measured: the report is worked out from given times."""

import sys
import unittest
from pathlib import Path

sys.dont_write_bytecode = True  # leaves no __pycache__ in the checkout
sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench_compare


class Report(unittest.TestCase):
    def test_builds_alternate_and_swap_who_goes_first(self):
        self.assertEqual([bench_compare.order(r) for r in range(3)],
                         [("base", "head"), ("head", "base"), ("base", "head")])

    def test_flagged_only_above_the_threshold_of_the_medians(self):
        base = [1.0, 1.0, 1.0]
        self.assertFalse(bench_compare.is_flagged(base, [1.10, 1.10, 1.10], 1.10))
        self.assertTrue(bench_compare.is_flagged(base, [1.11, 1.11, 1.11], 1.10))
        # One slow round does not move the median.
        self.assertFalse(bench_compare.is_flagged(base, [1.0, 9.0, 1.0], 1.10))

    def test_report(self):
        results = {
            "plain-dsd": {"base": [2.0, 2.2, 2.1], "head": [2.0, 2.1, 2.2]},
            "dst-stereo": {"base": [4.0, 4.0, 4.2], "head": [5.0, 5.2, 4.9]},
        }
        text, flagged = bench_compare.report(results, 1.10, 61.4)
        self.assertEqual(flagged, ["dst-stereo"])
        self.assertIn("| plain-dsd | 2.100 s (2.000 to 2.200) | 2.100 s (2.000 to 2.200) | 1.000 | ok |", text)
        self.assertIn("| dst-stereo | 4.000 s (4.000 to 4.200) | 5.000 s (4.900 to 5.200) | 1.250 | **SLOWER** |", text)
        self.assertIn("Medians of 3, builds alternated, measured in 61 s.", text)
        self.assertIn("more than 10% slower than base", text)


if __name__ == "__main__":
    unittest.main()
