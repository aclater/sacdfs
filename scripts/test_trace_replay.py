#!/usr/bin/env python3
"""Tests of scripts/trace-replay.py's profiles and numbers: `python3 -I scripts/test_trace_replay.py`
(#506). No mount is needed.

- Each profile in tests/fixtures/players comes from its trace as its `source` says: the probes,
  the playing handle's first read and read size, and the rescan are what --profile derives (the
  rate and lead of the Zidoo profiles are set by hand, as their notes say, and so are the turns
  of one whose playing handle opens after the heads).
- A read keeps its place from trace to local file: header, audio and tail.
- The simulated player stalls when replies come later than it plays, and not otherwise.
- --anonymise rounds file sizes, keeps head and tail reads in place, and refuses a trace that
  would show a size where reads from both ends meet.
"""

import importlib.util
import itertools
import json
import shlex
import sys
import tempfile
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
    a = {k: v for k, v in itertools.pairwise(argv) if k in ("--play", "--before", "--secs", "--wav-audio-start")}
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
                    want = d["rescan"]
                    # Most re-reads come before a playing handle opened after the heads, so the
                    # trace cannot show turns: set by hand (zidoo-turns-warm-dsf).
                    if "play_after_heads" in p:
                        want = dict(want, turns=p["rescan"]["turns"])
                    self.assertEqual(p["rescan"], want)
                if not p["name"].startswith("zidoo"):
                    self.assertEqual(p["play"], d["play"])

    def test_browse_is_the_probe_of_a_first_start(self):
        # The probe the #456 player makes when it lists the album (#510): its round-4 first start's.
        players = ROOT / "tests" / "fixtures" / "players"
        warm = json.loads((players / "zidoo-turns-warm-dsf.json").read_text())
        first = json.loads((players / "zidoo-turns-dsf.json").read_text())
        self.assertEqual(warm["browse"], first["probes"][0])
        self.assertEqual(warm["browse"][0], ["h", 0, 131072])

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


class Anonymise(unittest.TestCase):
    SIZE = 788668824  # not a multiple of 4 KiB
    TAIL = 788668416  # the page the size's last bytes are in

    def run_on(self, reads, unit=tr.ROUND):
        lines = [f'1.0 open fh=1 r=0 pid=7 size={self.SIZE} "/music/A/#2 6ch .dsf"']
        lines += [f"{2 + i}.0 read fh=1 off={o} len={n} miss q=0.1 sw=0.0 dec=1.0" for i, (o, n) in enumerate(reads)]
        with tempfile.TemporaryDirectory() as d:
            src, out = Path(d, "in.log"), Path(d, "out.trace")
            src.write_text("\n".join(lines) + "\n")
            tr.anonymise(str(src), str(out), unit)
            text = out.read_text()
            return text, tr.parse(str(out))[0]

    def test_sizes_are_rounded_and_reads_keep_their_place(self):
        reads = [(0, 131072), (self.TAIL, 4096), (131072, 131072), (self.TAIL - 65536, 65536)]
        text, h = self.run_on(reads)
        self.assertNotIn(str(self.SIZE), text)
        self.assertNotIn("/music", text)
        self.assertEqual(h.size % tr.ROUND, 0)
        self.assertGreaterEqual(h.size, self.SIZE)
        self.assertEqual([o for _, o, _, _ in h.reads],
                         [0, h.size - 4096, 131072, h.size - 4096 - 65536])
        for (o, n), (_, o2, n2, _) in zip(reads, h.reads):
            self.assertEqual(tr.place(o, n, self.SIZE, 92)[0], tr.place(o2, n2, h.size, 92)[0])
        # Again: nothing changes.
        with tempfile.TemporaryDirectory() as d:
            Path(d, "a").write_text(text)
            tr.anonymise(str(Path(d, "a")), str(Path(d, "b")))
            self.assertEqual(Path(d, "b").read_text(), text)

    def test_exact_sizes_on_request(self):
        text, h = self.run_on([(self.TAIL, 4096)], unit=0)
        self.assertEqual((h.size, h.reads[0][1]), (self.SIZE, self.TAIL))

    def test_reads_meeting_in_the_middle_are_refused(self):
        with self.assertRaises(SystemExit):
            self.run_on([(0, self.SIZE // 2 + 4096), (self.SIZE // 2 + 4096, 1 << 20)])

    def test_new_fixtures_have_rounded_sizes(self):
        h = tr.parse(str(ROOT / "tests" / "fixtures" / "traces" / "session-456-f.trace.gz"))
        self.assertTrue(all(x.size % tr.ROUND == 0 for x in h.values()))


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
