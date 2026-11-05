#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""sacdfs's half of the informational CI benchmark (#162): one measurement of one workload
with one build, for scripts/bench_compare.py. Prints the seconds spent reading.

Each of the REPEATS passes mounts the image afresh, so that nothing is cached (sacdfs's frame
cache and the kernel's page cache both die with the mount), reads the area's DSF files through
the mount, and unmounts. Only the reads are timed.
"""

import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
IMAGES = ROOT / "tests" / "corpus" / "images"
# Workload: image, area directories, passes.
WORKLOADS = {
    "dst-stereo": ("dst-features-2ch-6ch", ["Stereo"], 100),
    "dst-multichannel": ("dst-features-2ch-6ch", ["Multichannel"], 40),
    "plain-dsd": ("dsd-stereo-mch-2048", ["Stereo", "Multichannel"], 150),
}


def unmount(mountpoint: str) -> None:
    deadline = time.monotonic() + 20
    while subprocess.run(["fusermount3", "-u", mountpoint], stderr=subprocess.DEVNULL, check=False).returncode:
        if time.monotonic() > deadline:
            sys.exit(f"{mountpoint} stays busy")
        time.sleep(0.01)


def one_pass(binary: str, image: Path, areas: list[str], mountpoint: str) -> float:
    subprocess.run([binary, "--no-index-cache", "--format", "dsf", str(image), mountpoint], check=True)
    try:
        start = time.perf_counter()
        for area in areas:
            directory = os.path.join(mountpoint, image.stem, area)
            for name in sorted(os.listdir(directory)):
                with open(os.path.join(directory, name), "rb", buffering=0) as f:
                    while f.read(1 << 20):
                        pass
        return time.perf_counter() - start
    finally:
        unmount(mountpoint)


def main() -> None:
    binary, workload = sys.argv[1:]
    stem, areas, passes = WORKLOADS[workload]
    with tempfile.TemporaryDirectory() as mountpoint:
        print(sum(one_pass(binary, IMAGES / f"{stem}.iso", areas, mountpoint) for _ in range(passes)))


if __name__ == "__main__":
    main()
