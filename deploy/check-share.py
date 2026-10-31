#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Checks a copy or a mount of the corpus served by sacdfs against the corpus manifest.

    check-share.py MANIFEST ROOT [--min N] [--cases ID,ID,...] [--wav]

ROOT is sacdfs's view of tests/corpus/images: a mounted SMB or NFS share, or a directory
copied from one. For every valid case (or those given with --cases) that has a directory
under ROOT, each track the manifest expects is read in full, and its DSF file's audio is
hashed per channel as the corpus defines it (DSD bytes, earliest sample in the MSB, padding
removed). With --wav, each track also needs a WAV file next to it; any WAV file present must
have a RIFF/RF64 WAVE header.
Exits non-zero on any mismatch, or if fewer than N tracks (default 1) were checked.
Standard library only, so it runs in any client container.
"""
import argparse
import hashlib
import json
import re
import struct
import sys
from pathlib import Path

REV = bytes(int(f"{b:08b}"[::-1], 2) for b in range(256))
AREAS = {"2ch": "Stereo", "multichannel": "Multichannel"}


def dsf_channels(raw: bytes) -> list[bytes]:
    if raw[:4] != b"DSD " or raw[28:32] != b"fmt " or raw[80:84] != b"data":
        raise ValueError("not a DSF file")
    total = struct.unpack_from("<Q", raw, 12)[0]
    if total != len(raw):
        raise ValueError(f"size field {total} != file size {len(raw)}")
    ch = struct.unpack_from("<I", raw, 52)[0]
    samples = struct.unpack_from("<Q", raw, 64)[0]
    block = struct.unpack_from("<I", raw, 72)[0]
    size = struct.unpack_from("<Q", raw, 84)[0] - 12
    data = raw[92:92 + size]
    per = [bytearray() for _ in range(ch)]
    for off in range(0, len(data), block * ch):
        for c in range(ch):
            per[c] += data[off + c * block:off + (c + 1) * block]
    n = samples // 8
    return [bytes(p[:n]).translate(REV) for p in per]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("manifest", type=Path)
    ap.add_argument("root", type=Path)
    ap.add_argument("--min", type=int, default=1)
    ap.add_argument("--cases", default="")
    ap.add_argument("--wav", action="store_true", help="require a WAV file next to each DSF file")
    a = ap.parse_args()
    only = set(filter(None, a.cases.split(",")))
    cases = json.loads(a.manifest.read_text())["cases"]
    checked = failed = 0
    for case in cases:
        cid = case["id"]
        if case["expected"]["outcome"] != "ok" or (only and cid not in only):
            continue
        disc = a.root / cid
        if not disc.is_dir():
            if only:
                print(f"FAIL {cid}: not shown")
                failed += 1
            continue
        for area in case["expected"]["audio"]:
            adir = disc / AREAS[area["area"]]
            names = sorted(p.name for p in adir.glob("*")) if adir.is_dir() else []
            for t in area["tracks"]:
                if t["outcome"] != "ok":
                    continue
                num = t["number"]
                dsf = [adir / n for n in names if n.endswith(".dsf") and int(re.match(r"\d+", n)[0]) == num]
                wav = [adir / n for n in names if n.endswith(".wav") and int(re.match(r"\d+", n)[0]) == num]
                where = f"{cid}/{AREAS[area['area']]}/{num}"
                if len(dsf) != 1:
                    print(f"FAIL {where}: {len(dsf)} DSF files")
                    failed += 1
                    continue
                try:
                    got = [hashlib.sha256(c).hexdigest() for c in dsf_channels(dsf[0].read_bytes())]
                except (OSError, ValueError) as e:
                    print(f"FAIL {where}: {e}")
                    failed += 1
                    continue
                if got != t["sha256"]:
                    print(f"FAIL {where}: per-channel SHA-256 differs")
                    failed += 1
                    continue
                if a.wav and len(wav) != 1:
                    print(f"FAIL {where}: {len(wav)} WAV files")
                    failed += 1
                    continue
                if wav:
                    head = wav[0].open("rb").read(12)
                    if head[:4] not in (b"RIFF", b"RF64") or head[8:12] != b"WAVE":
                        print(f"FAIL {where}: WAV header {head!r}")
                        failed += 1
                        continue
                checked += 1
    print(f"{checked} tracks match the manifest, {failed} failures")
    return 1 if failed or checked < a.min else 0


if __name__ == "__main__":
    sys.exit(main())
