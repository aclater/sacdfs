#!/usr/bin/env python3
"""Tests of the pin checks of scripts/check-version-drift.py: `python3 -I scripts/test_check_version_drift.py`."""

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True  # leaves no __pycache__ in the checkout
sys.path.insert(0, str(Path(__file__).resolve().parent))

import importlib.util

spec = importlib.util.spec_from_file_location("drift", Path(__file__).resolve().parent / "check-version-drift.py")
drift = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drift)

DESIGN = "The pin is scarletbook 1.0.2, from crates.io (`=1.0.2`)\n"


class Tree:
    """A temporary checkout with just the files check_pins and cargo read."""

    def __init__(self, cargo, quality):
        self.dir = tempfile.TemporaryDirectory()
        root = Path(self.dir.name)
        (root / "docs").mkdir()
        (root / "Cargo.toml").write_text(cargo)
        (root / "docs/design.md").write_text(DESIGN)
        (root / "docs/pcm-quality.md").write_text(quality)

    def problems(self):
        old = os.getcwd()
        os.chdir(self.dir.name)
        try:
            _, pins = drift.cargo()
            out = []
            drift.check_pins(pins, out)
            return pins, out
        finally:
            os.chdir(old)
            self.dir.cleanup()


CARGO_OLD = 'version = "0.8.0"\nscarletbook = "=1.0.2"\nscarletbook-pcm = "=0.7.0"\n'
CARGO_NEW = 'version = "0.9.0"\nscarletbook = "=1.0.2"\ndsd2pcm = { version = "=0.2.0", default-features = false }\n'
HISTORY = "From #84 to 0.8.0 the converter was `scarletbook-pcm` (0.5.3 to 0.7.0); since 0.9.0 it is dsd2pcm.\n"


class Pins(unittest.TestCase):
    def test_a_scarletbook_pcm_pin_is_still_compared(self):
        _, ok = Tree(CARGO_OLD, "`scarletbook-pcm` (0.7.0)\n").problems()
        self.assertEqual(ok, [])
        _, bad = Tree(CARGO_OLD, "`scarletbook-pcm` (0.6.1)\n").problems()
        self.assertEqual(len(bad), 1)
        self.assertIn("scarletbook-pcm 0.6.1, Cargo.toml pins =0.7.0", bad[0])

    def test_without_the_scarletbook_pcm_pin_the_dsd2pcm_version_must_be_quoted(self):
        pins, out = Tree(CARGO_NEW, HISTORY).problems()
        self.assertEqual(pins, {"scarletbook": "1.0.2", "dsd2pcm": "0.2.0"})
        self.assertEqual(len(out), 1)  # the history sentence alone is not enough
        self.assertIn('no "`dsd2pcm` (X)" mention', out[0])
        _, ok = Tree(CARGO_NEW, HISTORY + "The converter is `dsd2pcm` (0.2.0).\n").problems()
        self.assertEqual(ok, [])

    def test_a_quoted_dsd2pcm_version_is_compared_with_the_pin(self):
        _, bad = Tree(CARGO_NEW, HISTORY + "The converter is `dsd2pcm` (0.1.2).\n").problems()
        self.assertEqual(len(bad), 1)
        self.assertIn("dsd2pcm 0.1.2, Cargo.toml pins =0.2.0", bad[0])

    def test_with_neither_pin_the_check_fails(self):
        _, out = Tree('version = "0.9.0"\nscarletbook = "=1.0.2"\n', HISTORY).problems()
        self.assertEqual(len(out), 1)
        self.assertIn("neither a scarletbook-pcm nor a dsd2pcm pin", out[0])

    def test_the_design_pin_is_still_compared(self):
        t = Tree(CARGO_NEW.replace("=1.0.2", "=1.0.3"), HISTORY + "The converter is `dsd2pcm` (0.2.0).\n")
        _, out = t.problems()
        self.assertEqual(len(out), 2)  # both numbers of the line


D = "a" * 64
OTHER = "b" * 64
FILES = dict(drift.DIGEST_FILES)
COMPOSE = "services:\n  sacdfs:\n    image: ghcr.io/dsd-toolbox/sacdfs:0.9.0"
UNRAID = "<Repository>ghcr.io/dsd-toolbox/sacdfs:0.9.0"
TRUENAS = 'images:\n  image:\n    repository: ghcr.io/dsd-toolbox/sacdfs\n    tag: "0.9.0'


class DigestPins(unittest.TestCase):
    def check(self, path, text):
        out = []
        drift.check_digest_text(path, text, FILES[path], "0.9.0", "sha256:" + D, out)
        return out

    def texts(self, tail):
        return {
            "deploy/nas-compose.yaml": COMPOSE + tail + "\n",
            "deploy/unraid/sacdfs.xml": UNRAID + tail + "</Repository>\n",
            "deploy/truenas/sacdfs/ix_values.yaml": TRUENAS + tail + '"\n',
        }

    def test_the_published_digest_passes_in_all_three_files(self):
        for path, text in self.texts("@sha256:" + D).items():
            self.assertEqual(self.check(path, text), [], path)

    def test_a_missing_pin_is_reported_in_all_three_files(self):
        for path, text in self.texts("").items():
            out = self.check(path, text)
            self.assertEqual(len(out), 1, path)
            self.assertIn("pinned by no digest", out[0])

    def test_another_digest_is_reported(self):
        for path, text in self.texts("@sha256:" + OTHER).items():
            out = self.check(path, text)
            self.assertEqual(len(out), 1, path)
            self.assertIn(OTHER, out[0])

    def test_no_image_reference_is_a_failure_of_the_check(self):
        out = self.check("deploy/nas-compose.yaml", "services: {}\n")
        self.assertEqual(len(out), 1)
        self.assertIn("update this check", out[0])

    def test_another_version_is_left_to_the_version_check(self):
        out = []
        drift.check_digest_text(
            "deploy/nas-compose.yaml",
            COMPOSE.replace("0.9.0", "0.8.0") + "\n",
            FILES["deploy/nas-compose.yaml"],
            "0.9.0",
            "sha256:" + D,
            out,
        )
        self.assertEqual(out, [])

    def test_an_unpublished_version_is_not_checked(self):
        out = []
        drift.check_digests("0.9.1", {"0.9.0": "2026-11-25"}, out)
        self.assertEqual(out, [])


if __name__ == "__main__":
    unittest.main()
