#!/usr/bin/env python3
"""Tests of scripts/trace-replay.py's profiles and numbers: `python3 -I scripts/test_trace_replay.py`
(#506). No mount is needed.

- Each profile in tests/fixtures/players comes from its trace as its `source` says: the probes,
  the playing handle's first read and read size, and the rescan are what --profile derives (the
  rate and lead of the Zidoo profiles are set by hand, as their notes say).
- A read keeps its place from trace to local file: header, audio and tail.
- The simulated player stalls when replies come later than it plays, and not otherwise.
"""

import importlib.util
import json
import shlex
import sys
import unittest
from pathlib import Path

sys.dont_write_bytecode = True  # leaves no __pycache__ in the checkout
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("trace_replay", ROOT / "scripts" / "trace-replay.py")
tr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tr)


def derived(source):
    """The profile --profile prints for a profile's `source` command."""
    argv = shlex.split(source.split(",")[0])[1:]
    a = {k: v for k, v in zip(argv, argv[1:]) if k in ("--play", "--before", "--secs", "--wav-audio-start")}
    handles = tr.parse(str(ROOT / argv[0]))
    play = int(a["--play"])
    wav = int(a.get("--wav-audio-start", 0)) or tr.wav_audio_start(handles, tr.trace_rate) or 0
    return tr.profile(handles, play, float(a["--before"]), float(a["--secs"]), wav)


class Profiles(unittest.TestCase):
    def test_profiles_come_from_their_traces(self):
        files = sorted((ROOT / "tests" / "fixtures" / "players").glob("*.json"))
        self.assertGreaterEqual(len(files), 8)
        for f in files:
            with self.subTest(profile=f.name):
                p = json.loads(f.read_text())
                d = derived(p["source"])
                self.assertEqual(p["file"], d["file"])
                self.assertEqual(p["probes"], d["probes"])
                self.assertEqual(p["play"]["from"], d["play"]["from"])
                self.assertEqual(p["play"]["read"], d["play"]["read"])
                if "rescan from" not in p["source"]:
                    self.assertEqual(p["rescan"], d["rescan"])
                if not p["name"].startswith("zidoo"):
                    self.assertEqual(p["play"], d["play"])

    def test_zidoo_rereads_the_album(self):
        p = json.loads((ROOT / "tests" / "fixtures" / "players" / "zidoo-wav.json").read_text())
        self.assertGreaterEqual(p["rescan"]["files"], 15)
        self.assertEqual(p["rescan"]["exts"], ["dsf", "wav"])
        self.assertEqual(p["rescan"]["head"]["dsf"], 1572864 - 92)

    def test_a_read_keeps_its_place(self):
        size, audio = 1000000, 92
        for off, n, kind in ((0, 4096, "h"), (92, 4096, "a"), (500000, 131072, "a"), (999000, 4096, "e")):
            w = tr.place(off, n, size, audio)
            self.assertEqual(w[0], kind)
            self.assertEqual(tr.located(w, size, audio, audio), (off, n))
        # Another file: audio at the same byte of the audio, the tail at the same distance from its end.
        self.assertEqual(tr.located(["a", 1000, 10], 5000000, 200, 92), (1200, 10))
        self.assertEqual(tr.located(["e", -452, 4096], 5000000, 200, 92), (5000000 - 452, 4096))


class Numbers(unittest.TestCase):
    RATE = 1000  # bytes per second of audio

    def reads(self, latency):
        """A player reading 100 ms of audio at a time, each read sent when the last replied."""
        out, t = [], 0.0
        for i in range(60):
            out.append((t, t + latency, i * 100, 100))
            t += latency
        return out

    def test_fast_replies_do_not_stall(self):
        n = tr.numbers(self.reads(0.01), 0, 0, self.RATE, 0.5)
        self.assertEqual(n["stalled_s"], 0.0)
        self.assertAlmostEqual(n["worst_ms"], 10.0, places=3)

    def test_replies_slower_than_playback_stall(self):
        n = tr.numbers(self.reads(0.2), 0, 0, self.RATE, 0.5)  # 0.5x real time
        self.assertGreater(n["stalled_s"], 4.0)
        self.assertGreater(n["stalls"], 0)


if __name__ == "__main__":
    unittest.main()
