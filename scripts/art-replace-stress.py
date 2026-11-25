#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Folder art replaced under readers, on a real mount (#628 ESTALE, #631 EIO).

  scripts/art-replace-stress.py --sacdfs target/release/sacdfs --image tests/corpus/images/dsd-stereo-mch-2048.iso

Mounts a library of one album (an image and a `cover-0.jpg`), then, for each writer mode, runs
reader threads that open, `fstat` and read `cover.jpg` in a loop while a writer replaces the
source art every `--interval` seconds. Modes: `rename` (a temporary file renamed over the art),
`in-place` (truncate and write at once), `in-place-gap` (truncate, 100 microseconds, write).

Every read error is counted by call and errno; none is retried. A reader also holds one
descriptor open across a replacement and reads it again afterwards (`held`), and a stat of the
name made after the replacement must succeed at once. The bytes a reader gets must be a whole
body some writer wrote (or, for the in-place modes, may be a prefix: an in-place rewrite shows
partial content on any file system, so only the length-zero and the errno are judged there).
Exit status 0 if no read of any mode failed, else 1.
"""
import argparse
import collections
import errno
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path


def mounted(mp):
    with open("/proc/self/mountinfo") as f:
        return any(line.split()[4] == str(mp) for line in f)


def wait(cond, secs, what):
    end = time.time() + secs
    while time.time() < end:
        if cond():
            return
        time.sleep(0.05)
    sys.exit(f"timed out waiting for {what}")


def reader(cover, stop, errors, counts, idx):
    n = 0
    while not stop.is_set():
        n += 1
        call = "open"
        try:
            fd = os.open(cover, os.O_RDONLY)
            try:
                call = "fstat"
                os.fstat(fd)
                call = "read"
                while os.read(fd, 65536):
                    pass
            finally:
                os.close(fd)
        except OSError as e:
            errors[(call, errno.errorcode.get(e.errno, str(e.errno)))] += 1
    counts[idx] = n


def holder(cover, album, stop, errors, counts):
    """Keeps a descriptor open across replacements and reads it after each."""
    n = 0
    while not stop.is_set():
        try:
            fd = os.open(cover, os.O_RDONLY)
        except OSError:
            continue
        try:
            time.sleep(0.03)
            n += 1
            try:
                os.pread(fd, 16, 0)
            except OSError as e:
                errors[("held-read", errno.errorcode.get(e.errno, str(e.errno)))] += 1
            try:
                os.stat(cover)
            except OSError as e:
                errors[("stat-after", errno.errorcode.get(e.errno, str(e.errno)))] += 1
        finally:
            try:
                os.close(fd)
            except OSError as e:
                errors[("held-close", errno.errorcode.get(e.errno, str(e.errno)))] += 1
    counts["held"] = n


def writer(album, mode, interval, stop):
    art = album / "cover-0.jpg"
    n = 0
    while not stop.is_set():
        n += 1
        body = (f"art {n} " * 200).encode()
        if mode == "rename":
            tmp = album / ".tmp"
            tmp.write_bytes(body)
            os.rename(tmp, art)
        else:
            with open(art, "wb") as f:
                if mode == "in-place-gap":
                    f.flush()
                    time.sleep(0.0001)
                f.write(body)
        stop.wait(interval)


def run_mode(args, mode, work):
    lib = work / f"lib-{mode}"
    album = lib / "Artist/Album"
    album.mkdir(parents=True)
    os.symlink(args.image.resolve(), album / "disc.iso")
    (album / "cover-0.jpg").write_bytes(b"art 0 " * 200)
    mp = work / f"mnt-{mode}"
    mp.mkdir()
    log = work / f"sacdfs-{mode}.log"
    env = dict(os.environ, XDG_CACHE_HOME=str(work / "cache"))
    with open(work / f"sacdfs-{mode}.stderr", "wb") as err:
        proc = subprocess.Popen([str(args.sacdfs), "-f", "--log-file", str(log), str(lib), str(mp)],
                                env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=err)
    try:
        wait(lambda: mounted(mp) or proc.poll() is not None, 60, "the mount")
        if proc.poll() is not None:
            sys.exit(f"sacdfs exited: {(work / f'sacdfs-{mode}.stderr').read_text()}")
        cover = str(mp / "Artist/Album/disc/Stereo/cover.jpg")
        wait(lambda: os.path.exists(cover), 60, "cover.jpg")
        errors, counts, stop = collections.Counter(), {}, threading.Event()
        threads = [threading.Thread(target=reader, args=(cover, stop, errors, counts, i))
                   for i in range(args.readers)]
        threads.append(threading.Thread(target=holder, args=(cover, album, stop, errors, counts)))
        threads.append(threading.Thread(target=writer, args=(album, mode, args.interval, stop)))
        for t in threads:
            t.start()
        time.sleep(args.seconds)
        stop.set()
        for t in threads:
            t.join()
        reads = sum(v for k, v in counts.items() if k != "held")
        print(f"{mode:13} rounds={reads} held={counts.get('held', 0)} errors={dict(errors) or 0}")
        return sum(errors.values())
    finally:
        subprocess.run(["fusermount3", "-u", "-z", str(mp)], check=False)
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--sacdfs", type=Path, required=True)
    ap.add_argument("--image", type=Path, required=True)
    ap.add_argument("--seconds", type=float, default=25)
    ap.add_argument("--interval", type=float, default=0.01, help="seconds between replacements")
    ap.add_argument("--readers", type=int, default=4)
    ap.add_argument("--logs", type=Path, help="keep sacdfs's log and stderr of each mode in this directory")
    ap.add_argument("--modes", default="rename,in-place,in-place-gap")
    args = ap.parse_args()
    work = Path(tempfile.mkdtemp(prefix="art-replace-"))
    bad = 0
    try:
        for mode in args.modes.split(","):
            bad += run_mode(args, mode, work)
    finally:
        if args.logs:
            args.logs.mkdir(parents=True, exist_ok=True)
            for f in work.glob("sacdfs-*"):
                shutil.copy(f, args.logs / f.name)
        shutil.rmtree(work, ignore_errors=True)
    print("FAIL" if bad else "PASS", bad, "errors")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
