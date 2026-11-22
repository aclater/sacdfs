#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Tests of the verdict of scripts/stall-gate.py: `python3 -I scripts/test_stall_gate.py`. No mount
is needed: the verdict is worked out from saved numbers.

- Head as good as base passes, and so does a difference inside either threshold: a criterion
  fails only above both its ratio and its absolute margin, measured from base's worst round.
- Each criterion fails by itself and is named in the failure: worst read, stall, audio by 5 s,
  re-reads (only when every run has them), and a track opened later (only for a profile whose
  player opens it after re-reading heads).
"""

import argparse
import importlib.util
import sys
import unittest
from pathlib import Path

sys.dont_write_bytecode = True  # leaves no __pycache__ in the checkout
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("stall_gate", ROOT / "scripts" / "stall-gate.py")
sg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sg)

PLAIN = str(ROOT / "tests" / "fixtures" / "players" / "vlc-dsf.json")
AFTER_HEADS = str(ROOT / "tests" / "fixtures" / "players" / "zidoo-turns-warm-dsf.json")


def thresholds(scenarios):
    """The gate's default thresholds, as `main` parses them."""
    return argparse.Namespace(
        scenarios=scenarios, rounds=3, quota=40, cpus="0,1", secs=15.0,
        worst_ratio=1.25, worst_ms=250.0, stall_ratio=1.25, stall_s=1.0, rescan_ratio=1.2, rescan_ms=20.0,
        audio_ratio=1.25, audio_s=0.5, open_ratio=1.25, open_s=0.5)


def runs(n=3, **numbers):
    """`n` runs with the numbers of a good run, but for `numbers`."""
    good = {"worst_ms": 100.0, "stalled_s": 0.0, "audio_by_5s": 6.0, "rescan_ms": None, "play_open_s": 0.0}
    return [dict(good, **numbers) for _ in range(n)]


def judge(scenario=PLAIN, base=None, head=None):
    res = {scenario: {"base": base or runs(), "head": head or runs()}}
    return sg.verdict(thresholds([scenario]), res, 12.0)


class Verdict(unittest.TestCase):
    def test_equal_builds_pass(self):
        text, failed = judge()
        self.assertEqual(failed, [])
        self.assertIn("| vlc-dsf | 100 → 100 (100-100 / 100-100) ", text)
        self.assertIn("| - | ok |", text)
        self.assertNotIn("WORSE", text)

    def test_worst_read_needs_both_the_ratio_and_the_margin(self):
        # 100 → 340 ms: above 1.25x, not above 100 + 250 ms.
        self.assertEqual(judge(head=runs(worst_ms=340.0))[1], [])
        # 2000 → 2400 ms: above 2000 + 250 ms, not above 1.25x.
        self.assertEqual(judge(base=runs(worst_ms=2000.0), head=runs(worst_ms=2400.0))[1], [])
        text, failed = judge(head=runs(worst_ms=360.0))
        self.assertEqual(failed, ["vlc-dsf (worst read)"])
        self.assertIn("| **WORSE**: worst read |", text)

    def test_head_is_compared_with_base_s_worst_round(self):
        base = runs(worst_ms=100.0)
        base[1]["worst_ms"] = 300.0
        # Head's median, 400 ms, is above base's median by both thresholds, but not above its worst.
        self.assertEqual(judge(base=base, head=runs(worst_ms=400.0))[1], [])

    def test_one_bad_round_of_head_does_not_fail_its_median(self):
        head = runs()
        head[0].update(worst_ms=5000.0, stalled_s=9.0)
        self.assertEqual(judge(head=head)[1], [])

    def test_stall(self):
        self.assertEqual(judge(head=runs(stalled_s=1.0))[1], [])
        self.assertEqual(judge(head=runs(stalled_s=1.1))[1], ["vlc-dsf (stall)"])

    def test_audio_by_5s(self):
        # 6.0 → 5.6 s: less than 0.5 s lower. 6.0 → 4.9 s: lower by more than 0.5 s, but not below 6.0 / 1.25.
        self.assertEqual(judge(head=runs(audio_by_5s=5.6))[1], [])
        self.assertEqual(judge(head=runs(audio_by_5s=4.9))[1], [])
        self.assertEqual(judge(head=runs(audio_by_5s=4.7))[1], ["vlc-dsf (audio by 5 s)"])

    def test_rereads_count_only_when_every_run_has_them(self):
        text, failed = judge(base=runs(rescan_ms=50.0), head=runs(rescan_ms=90.0))
        self.assertEqual(failed, ["vlc-dsf (re-reads)"])
        self.assertIn("| 50 → 90 (50-50 / 90-90) |", text)
        self.assertEqual(judge(base=runs(rescan_ms=50.0), head=runs(rescan_ms=65.0))[1], [])
        head = runs(rescan_ms=90.0)
        head[2]["rescan_ms"] = None
        text, failed = judge(base=runs(rescan_ms=50.0), head=head)
        self.assertEqual(failed, [])
        self.assertIn("| - | ok |", text)

    def test_a_track_opened_later_counts_for_a_player_that_opens_after_heads(self):
        late = {"base": runs(play_open_s=1.0), "head": runs(play_open_s=1.8)}
        text, failed = judge(AFTER_HEADS, **late)
        self.assertEqual(failed, ["zidoo-turns-warm-dsf (track opened later)"])
        self.assertIn("; opened 1.00 → 1.80 s", text)
        self.assertEqual(judge(PLAIN, **late)[1], [])
        self.assertEqual(judge(AFTER_HEADS, base=runs(play_open_s=1.0), head=runs(play_open_s=1.4))[1], [])

    def test_several_failures_are_all_named(self):
        text, failed = judge(head=runs(worst_ms=900.0, stalled_s=3.0, audio_by_5s=2.0))
        self.assertEqual(failed, ["vlc-dsf (worst read, stall, audio by 5 s)"])
        self.assertIn("| **WORSE**: worst read, stall, audio by 5 s |", text)

    def test_the_report_names_the_run_and_the_thresholds(self):
        text, _ = judge()
        self.assertTrue(text.startswith("## Player stall gate (3 rounds, sacdfs at 40% of one CPU on CPUs 0,1)\n"))
        self.assertIn("measured in 12 s.", text)
        self.assertIn("more than 1.25x and 250 ms (worst read)", text)

    def test_scenario_names(self):
        self.assertEqual(sg.name("a/b/vlc-dsf.json"), "vlc-dsf")
        self.assertEqual(sg.name("a/vlc-dsf.json@idle"), "vlc-dsf-idle")
        self.assertEqual(sg.name("a/vlc-dsf.json@probe:20"), "vlc-dsf-probe20")
        self.assertEqual(sg.name("t/x.trace.gz:7"), "x:7")


if __name__ == "__main__":
    unittest.main()
