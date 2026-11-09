#!/usr/bin/env python3
"""Tests of the verdict in scripts/soak.py: `python3 -I scripts/test_soak.py` (#247).

The series are built, not recorded: a flat level with a little noise, then a leak, a step or a
thread spike added at a chosen minute. The checks they pin down:

- a single step of 3 to 6.5 MiB, up or down, must not fail a 2-hour run on its own;
- a steady climb (10 MiB/h) must fail, with a step in the run or without one, a step down
  included, and a slow climb must fail a long run;
- one thread sample at the spike line must pass, two in a row must pass, three in a row
  (a stall's spare threads end within a minute) must fail, and so must one above twice the line.
"""

import argparse
import random
import sys
import unittest
from pathlib import Path

sys.dont_write_bytecode = True  # leaves no __pycache__ in the checkout
sys.path.insert(0, str(Path(__file__).resolve().parent))

import soak  # noqa: E402

MIB = 1024  # KiB


def args(**kw):
    a = argparse.Namespace(warmup=600, cache_mb=64, noise_mb=32, max_slope=4, trend_floor_mb=3,
                           thread_spike=16, spike_hold=120)
    for k, v in kw.items():
        setattr(a, k, v)
    return a


def series(hours=2.0, interval=60, leak=0.0, steps=(), threads=None, seed=1):
    """Rows like the soak's. `leak` in MiB/h from the warm-up on; `steps` as (minute, MiB);
    `threads` maps a minute to a thread count (default 64, with a little noise)."""
    rng = random.Random(seed)
    rows = []
    for t in range(interval, int(hours * 3600) + 1, interval):
        mib = 150 + rng.uniform(-1, 1)
        if t >= 600:
            mib += leak * (t - 600) / 3600
        mib += sum(s for minute, s in steps if t >= minute * 60)
        th = 64 + rng.randint(-2, 2)
        if threads and t // 60 in threads:
            th = threads[t // 60]
        rows.append({"t": t, "rss_kb": int(mib * MIB), "anon_kb": int((mib - 6) * MIB), "hwm_kb": 0,
                     "threads": th, "fds": 18, "cache_kb": 100 * MIB})
    return rows


def run(rows, **kw):
    ok, lines = soak.verdict(rows, args(**kw))
    return ok, "\n".join(lines)


class MemoryTrend(unittest.TestCase):
    def test_flat_run_passes(self):
        ok, out = run(series())
        self.assertTrue(ok, out)

    def test_single_step_passes_wherever_it_falls(self):
        # The steps in the 0.4.0 to 0.4.1-rc.3 runs were 3 to 6.5 MiB, up and down.
        for size in (3, 4.8, 6.5, -5.2):
            for minute in (20, 45, 74, 83, 98, 110, 118):
                with self.subTest(size=size, minute=minute):
                    ok, out = run(series(steps=[(minute, size)]))
                    self.assertTrue(ok, out)

    def test_old_rule_failed_that_step(self):
        # The line through the second half, which the verdict used before, fails a 6.5 MiB step.
        rows = series(steps=[(83, 6.5)])
        late = [r for r in rows if r["t"] >= 600]
        late = late[len(late) // 2:]
        self.assertGreater(soak.slope_per_hour(late, "rss_kb") / MIB, 4.4)

    def test_steady_leak_fails(self):
        for rate in (10, 15, 30):
            with self.subTest(rate=rate):
                ok, out = run(series(leak=rate))
                self.assertFalse(ok, out)

    def test_leak_beside_a_step_fails(self):
        for size in (6.5, -6.5):
            with self.subTest(step=size):
                ok, _ = run(series(leak=10, steps=[(83, size)]))
                self.assertFalse(ok)

    def test_slow_leak_fails_a_long_run(self):
        ok, out = run(series(hours=8, leak=5))
        self.assertFalse(ok, out)
        ok, out = run(series(hours=8, leak=1.5))
        self.assertTrue(ok, out)

    def test_step_in_a_long_run_passes(self):
        ok, out = run(series(hours=8, steps=[(378, 3.2)]))
        self.assertTrue(ok, out)

    def test_two_steps_in_the_same_direction_are_not_one_step(self):
        # Two steps of 6.5 MiB in under an hour are a climb of about 13 MiB/h.
        ok, _ = run(series(steps=[(70, 6.5), (100, 6.5)]))
        self.assertFalse(ok)

    def test_growth_past_the_cache_cap_fails_whatever_the_trend(self):
        ok, out = run(series(steps=[(30, 120)]))
        self.assertFalse(ok, out)

    def test_step_trend_reports_the_step_it_set_aside(self):
        rows = [r for r in series(steps=[(83, 6.5)]) if r["t"] >= 3600]
        rate, left, hours, step = soak.step_trend(rows, "rss_kb")
        self.assertAlmostEqual(step / MIB, 6.5, delta=1.5)
        self.assertLess(abs(rate / MIB), 3)
        self.assertGreater(hours, 0.4)

    def test_too_few_rows_for_windows_fall_back_to_the_line(self):
        self.assertIsNone(soak.step_trend(series()[:8], "rss_kb"))
        rows = series(hours=0.25, interval=30)  # the CI run: 15 minutes, 5 of warm-up
        ok, out = run(rows, warmup=300)
        self.assertTrue(ok, out)


class Threads(unittest.TestCase):
    def test_one_sample_at_the_spike_line_passes_with_margin(self):
        # base 64, line 80: one sample at 80, and one above it, are a stall's spare threads.
        for peak in (80, 81, 90, 96):
            with self.subTest(peak=peak):
                ok, out = run(series(threads={100: peak}))
                self.assertTrue(ok, out)

    def test_one_sample_past_twice_the_line_fails(self):
        ok, out = run(series(threads={100: 97}))
        self.assertFalse(ok, out)

    def test_two_samples_in_a_row_above_the_line_pass_three_fail(self):
        ok, out = run(series(threads={100: 85, 101: 85}))
        self.assertTrue(ok, out)
        ok, out = run(series(threads={100: 85, 101: 85, 102: 85}))
        self.assertFalse(ok, out)

    def test_separate_spikes_do_not_add_up(self):
        ok, out = run(series(threads={60: 85, 61: 85, 80: 85, 81: 85, 100: 85, 101: 85}))
        self.assertTrue(ok, out)

    def test_threads_that_accumulate_fail_below_the_line(self):
        rows = series()
        for r in rows:
            if r["t"] >= 3900:
                r["threads"] += 8
        ok, out = run(rows)
        self.assertFalse(ok, out)

    def test_the_interval_sets_how_many_samples_make_a_hold(self):
        # 30 s samples: four in a row span 90 s and pass, five span 120 s and fail.
        rows = series(interval=30)
        for r in rows:
            if r["t"] in (6000, 6030, 6060, 6090):
                r["threads"] = 85
        self.assertTrue(run(rows)[0])
        for r in rows:
            if r["t"] == 6120:
                r["threads"] = 85
        self.assertFalse(run(rows)[0])

    def test_a_noisy_end_does_not_fail_a_healthy_run(self):
        # Three samples 5 over the first half's median, as in a healthy 2-hour cut of a 6-hour run.
        rows = series()
        for r in rows[-3:]:
            r["threads"] = 69
        ok, out = run(rows)
        self.assertTrue(ok, out)


class Swapped(unittest.TestCase):
    """The kernel swapping pages out and back in moves the resident size, not the memory (#247)."""

    @staticmethod
    def reclaim(rows, spans):
        """Moves MiB from resident to swap for the samples in each (from_minute, to_minute, MiB)."""
        for r in rows:
            r["swap_kb"] = 0
            for a, b, mib in spans:
                if a * 60 <= r["t"] < b * 60:
                    r["rss_kb"] -= mib * MIB
                    r["anon_kb"] -= mib * MIB
                    r["swap_kb"] = mib * MIB
        return rows

    def test_pages_reclaimed_to_swap_do_not_count_as_a_step(self):
        # As in the four parallel 2-hour soaks: -4.5 then -9 MiB resident late in the run, with the
        # same amount in swap.
        spans = [(100, 108, 4), (108, 120, 9)]
        rows = self.reclaim(series(), spans)
        ok, out = run(rows)
        self.assertTrue(ok, out)
        self.assertIn("swapped out", out)

    def test_a_leak_that_is_swapped_out_still_fails(self):
        rows = series(leak=10)
        for r in rows:
            if r["t"] >= 600:
                held = int(10 * (r["t"] - 600) / 3600 * MIB)
                r["rss_kb"] -= held
                r["anon_kb"] -= held
                r["swap_kb"] = held
        self.assertFalse(run(rows)[0])


if __name__ == "__main__":
    unittest.main()
