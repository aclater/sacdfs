#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Replays a library import's reads over a sacdfs mount and prints sacdfs's memory (#73).

  scripts/import-replay.py MOUNT PID [--ext .dsf] [--every 50]

For each file under MOUNT with the extension, in name order: 768 KiB from the start (in
128 KiB reads, the FUSE maximum), 36 KiB at the middle and 33 KiB at the end, which is what
Roon reads when it imports a file. Every `--every` files and at the end it prints the files
done and the resident memory (VmRSS) and its high-water mark (VmHWM) of process PID, in MiB.
"""
import argparse
import os


def memory(pid: int) -> str:
    vals = {}
    with open(f"/proc/{pid}/status") as f:
        for line in f:
            key, _, val = line.partition(":")
            if key in ("VmRSS", "VmHWM"):
                vals[key] = int(val.split()[0]) // 1024
    return f"rss {vals['VmRSS']} MiB, hwm {vals['VmHWM']} MiB"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("mount")
    ap.add_argument("pid", type=int)
    ap.add_argument("--ext", default=".dsf")
    ap.add_argument("--every", type=int, default=50)
    a = ap.parse_args()
    files = sorted(
        os.path.join(d, f) for d, _, fs in os.walk(a.mount) for f in fs if f.endswith(a.ext)
    )
    print(f"0 files: {memory(a.pid)}", flush=True)
    chunk = 128 << 10
    for i, path in enumerate(files, 1):
        fd = os.open(path, os.O_RDONLY)
        try:
            size = os.fstat(fd).st_size
            for off in range(0, min(768 << 10, size), chunk):
                os.pread(fd, chunk, off)
            os.pread(fd, 36 << 10, size // 2)
            os.pread(fd, 33 << 10, max(0, size - (33 << 10)))
        finally:
            os.close(fd)
        if i % a.every == 0 or i == len(files):
            print(f"{i} files: {memory(a.pid)}", flush=True)


if __name__ == "__main__":
    main()
