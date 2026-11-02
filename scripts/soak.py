#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Soak test: hours of reads, seeks and churn on one sacdfs mount, then a verdict on its
memory (#151). Run on a test host, not in PR CI; see docs/development.md.

  scripts/soak.py --sacdfs target/release/sacdfs --images DIR [--hours 2] [--out soak.tsv]

DIR holds `.iso` images (real discs, or tests/corpus/images). The script builds a library
of links to them in a work directory and mounts it with `--tags`. While it runs:

- readers (`--readers`) read the mounted files: long sequential stretches from random
  offsets, bursts of small reads at random seeks, and the start/middle/end reads of a
  library scan;
- the churner, every few seconds: adds or removes an image and a sidecar (the re-read,
  #101/#123), adds or removes an `Artwork/` folder, and edits a tags file (the reload, #121).
  Each added album has a new name, unless `--churn-slots K` reuses K names;
- the staller, every `--stall-every` seconds: points an image's link at a FIFO, reads it
  until the watchdog fails the request (#141), then releases the stuck open and restores
  the link (the abandoned request then finishes);
- the remounter, every `--remount-every` seconds: mounts the library a second time with the
  same caches, reads, and unmounts (the persistent seek index and tags state across mounts).

Each `--interval` seconds it writes one row to the TSV: the main sacdfs's resident memory,
its anonymous memory (the malloc heap plus thread stacks), threads, open files, the size of
the cache directory, and the work done. At the end:

PASS if, after `--warmup`, the resident and anonymous memory stay within the cache cap plus
`--noise-mb` of their level at the end of the warm-up, their trend over the second half is
at most `--max-slope` MiB per hour (or adds under `--trend-floor-mb` over that half), threads and open files do not climb, and the cache
directory plateaus. Exit status 0 on PASS, 1 on FAIL.

`--wrap 'heaptrack -o OUT'` (or valgrind massif) runs the main sacdfs under a profiler.
"""
import argparse
import os
import random
import shlex
import signal
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

STOP = threading.Event()
LOCK = threading.Lock()
COUNTS = {"bytes": 0, "reads": 0, "read_errors": 0, "churn": 0, "stalls": 0, "stall_fails": 0,
          "remounts": 0, "remount_fails": 0}


def count(key, n=1):
    with LOCK:
        COUNTS[key] += n


def log(msg):
    print(f"{time.strftime('%H:%M:%S')} {msg}", flush=True)


def is_mounted(mp: Path) -> bool:
    mp = str(mp)
    with open("/proc/self/mountinfo") as f:
        for line in f:
            parts = line.split(" ")
            if parts[4] == mp and " - fuse" in line:
                return True
    return False


def wait(cond, secs, what):
    end = time.monotonic() + secs
    while not cond():
        if time.monotonic() > end:
            raise RuntimeError(f"timed out waiting for {what}")
        time.sleep(0.1)


def find_sacdfs(pid: int) -> int:
    """The sacdfs process: `pid` itself, or its descendant when run under a wrapper."""
    todo = [pid]
    while todo:
        p = todo.pop()
        try:
            if Path(f"/proc/{p}/comm").read_text().strip() == "sacdfs":
                return p
            for t in Path(f"/proc/{p}/task").iterdir():
                todo += [int(c) for c in (t / "children").read_text().split()]
        except OSError:
            pass
    return pid


class Mount:
    def __init__(self, args, lib, mp, logname, wrap=""):
        self.mp = mp
        cmd = shlex.split(wrap) + [args.sacdfs, "-f", "--tags", str(args.work / "tags"),
                                   "--cache-mb", str(args.cache_mb),
                                   "--request-timeout", str(args.request_timeout),
                                   "--log-file", str(args.work / logname), str(lib), str(mp)]
        env = dict(os.environ, XDG_CACHE_HOME=str(args.work / "cache"), RUST_LOG="warn")
        self.proc = subprocess.Popen(cmd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                     stderr=open(args.work / f"{logname}.stderr", "ab"))
        wait(lambda: is_mounted(mp) or self.proc.poll() is not None, 120, f"the mount on {mp}")
        if self.proc.poll() is not None:
            raise RuntimeError(f"sacdfs exited with {self.proc.returncode}")
        self.pid = find_sacdfs(self.proc.pid)

    def stop(self, secs=60):
        try:
            os.kill(self.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            self.proc.wait(secs)
        except subprocess.TimeoutExpired:
            subprocess.run(["fusermount3", "-u", "-z", str(self.mp)], check=False)
            self.proc.kill()
            self.proc.wait()
            return False
        return not is_mounted(self.mp)


def files_under(root: Path):
    out = []
    for d, _, names in os.walk(root):
        out += [Path(d) / n for n in names if n.endswith((".dsf", ".wav", ".jpg"))]
    return out


def reader(seed, args, state):
    rnd = random.Random(seed)
    while not STOP.is_set():
        files = state["files"]
        if not files:
            time.sleep(1)
            continue
        p = rnd.choice(files)
        try:
            with open(p, "rb") as f:
                size = os.fstat(f.fileno()).st_size
                kind = rnd.random()
                if kind < 0.3:  # a long stretch from a random offset
                    f.seek(rnd.randrange(max(size, 1)))
                    left = rnd.randrange(1 << 20, 16 << 20)
                    while left > 0 and not STOP.is_set():
                        b = f.read(min(left, 1 << 20))
                        if not b:
                            break
                        left -= len(b)
                        count("bytes", len(b))
                elif kind < 0.7:  # seeks
                    for _ in range(rnd.randrange(5, 30)):
                        f.seek(rnd.randrange(max(size, 1)))
                        count("bytes", len(f.read(rnd.choice((4096, 65536, 131072)))))
                else:  # a library scan's reads
                    for off, n in ((0, 768 << 10), (size // 2, 36 << 10), (max(size - (33 << 10), 0), 33 << 10)):
                        f.seek(off)
                        count("bytes", len(f.read(n)))
            count("reads")
        except OSError:
            count("read_errors")  # churned away, or stalled: expected
        time.sleep(args.read_pause)


def refresher(args, state):
    while not STOP.is_set():
        try:
            state["files"] = files_under(args.work / "mnt")
        except OSError:
            pass
        STOP.wait(30)


def tags_key(args, image: Path):
    out = subprocess.run([args.sacdfs, "tags", "key", "--tags", str(args.work / "tags"), str(image)],
                         capture_output=True, text=True, check=True)
    return out.stdout.split("\t")[0]


def write_tags(args, key, n):
    title = "Soak edit " + "x" * (n % 17)
    (args.work / "tags" / f"{key}.toml").write_text(f'[track.1]\ntitle = "{title} {n}"\n')


def churner(args, images, keys):
    rnd = random.Random(1)
    lib = args.work / "lib"
    churn = lib / "churn"
    n = 0
    while not STOP.wait(args.churn_every):
        n += 1
        act = n % 5
        try:
            if act == 0:  # an image appears in a new album folder
                if args.churn_slots:  # names come back: album-0 ... album-(K-1), same image each
                    slot = (n // 5) % args.churn_slots
                    d, img = churn / f"album-{slot}", images[slot % len(images)]
                    if d.exists():
                        continue
                else:  # every album is new, as on a library that grows and gets renamed
                    d, img = churn / f"album-{n}", rnd.choice(images)
                d.mkdir(parents=True)
                (d / "disc.iso").symlink_to(img)
            elif act == 1:  # sidecars come and go
                (lib / "a0" / f"cover-{n}.jpg").write_bytes(b"\xff\xd8\xff\xe0 soak" * rnd.randrange(1, 64))
                for old in sorted((lib / "a0").glob("cover-*.jpg"))[:-2]:
                    old.unlink()
            elif act == 2:  # a tags file is edited
                write_tags(args, rnd.choice(keys), n)
            elif act == 3:  # an art folder comes and goes
                art = lib / "a1" / "Artwork"
                if art.exists():
                    for f in art.iterdir():
                        f.unlink()
                    art.rmdir()
                else:
                    art.mkdir()
                    (art / "back.png").write_bytes(b"\x89PNG soak" * rnd.randrange(1, 64))
            else:  # the oldest added albums go
                keep = 1 if args.churn_slots else 3
                for d in sorted(churn.glob("album-*"), key=lambda p: p.stat().st_mtime)[:-keep]:
                    (d / "disc.iso").unlink()
                    d.rmdir()
            count("churn")
        except OSError as e:
            log(f"churn: {e}")


def staller(args, image):
    link = args.work / "lib" / "stall" / "disc.iso"
    fifo = args.work / "fifo"
    os.mkfifo(fifo)
    while not STOP.wait(args.stall_every):
        try:
            names = [p for p in files_under(args.work / "mnt" / "stall") if p.suffix == ".dsf"]
            if not names:
                continue
            tmp = link.with_name("disc.tmp")
            tmp.symlink_to(fifo)
            os.replace(tmp, link)
            # sacdfs's open handle of the image may still be good: closed when idle or when the
            # link changes; read until a request stalls and the watchdog fails it.
            t0 = time.monotonic()
            failed = False
            while time.monotonic() - t0 < args.request_timeout * 6 and not STOP.is_set():
                try:
                    with open(names[0], "rb") as f:
                        f.seek(random.randrange(max(os.fstat(f.fileno()).st_size, 1)))
                        f.read(65536)
                except OSError:
                    failed = True
                    break
                time.sleep(0.5)
            count("stalls")
            if not failed:
                count("stall_fails")
            # Release the stuck opens: a writer comes and goes, so they see an empty FIFO.
            for _ in range(20):
                try:
                    os.close(os.open(fifo, os.O_WRONLY | os.O_NONBLOCK))
                except OSError:
                    pass
                time.sleep(0.05)
            tmp.symlink_to(image)
            os.replace(tmp, link)
        except OSError as e:
            log(f"stall: {e}")


def remounter(args):
    mp = args.work / "mnt2"
    while not STOP.wait(args.remount_every):
        try:
            m = Mount(args, args.work / "lib", mp, "remount.log")
            for p in files_under(mp)[:20]:
                with open(p, "rb") as f:
                    f.read(256 << 10)
            ok = m.stop()
            count("remounts")
            if not ok:
                count("remount_fails")
        except (OSError, RuntimeError) as e:
            count("remount_fails")
            log(f"remount: {e}")


def sample(pid, cache: Path):
    st = {}
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        k, _, v = line.partition(":")
        st[k] = v.split()[0] if v.split() else ""
    anon = 0
    for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
        if line.startswith("Anonymous:"):
            anon = int(line.split()[1])
    fds = len(os.listdir(f"/proc/{pid}/fd"))
    du = 0
    for d, _, names in os.walk(cache):
        for n in names:
            try:
                du += os.lstat(os.path.join(d, n)).st_size
            except OSError:
                pass  # a temporary file renamed meanwhile
    du //= 1024
    return {"rss_kb": int(st["VmRSS"]), "hwm_kb": int(st["VmHWM"]), "anon_kb": anon,
            "threads": int(st["Threads"]), "fds": fds, "cache_kb": du}


def slope_per_hour(rows, key):
    """Least-squares slope of `key` over time, in units per hour."""
    xs = [r["t"] / 3600 for r in rows]
    ys = [r[key] for r in rows]
    mx, my = statistics.fmean(xs), statistics.fmean(ys)
    den = sum((x - mx) ** 2 for x in xs)
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / den if den else 0.0


def verdict(rows, args):
    after = [r for r in rows if r["t"] >= args.warmup]
    if len(after) < 6:
        return False, ["too few samples after the warm-up"]
    late = after[len(after) // 2:]
    ok, lines = True, []
    for key in ("rss_kb", "anon_kb"):
        base = statistics.median(r[key] for r in after[:3])
        peak = max(r[key] for r in after)
        allowed = (args.cache_mb + args.noise_mb) * 1024
        slope = slope_per_hour(late, key) / 1024
        # A short second half makes the slope noisy: a trend whose growth over that half stays
        # under --trend-floor-mb passes too (a 2-hour run: about 5 MiB/h).
        late_h = (late[-1]["t"] - late[0]["t"]) / 3600
        limit = max(args.max_slope, args.trend_floor_mb / late_h if late_h else args.max_slope)
        good = peak - base <= allowed and slope <= limit
        ok &= good
        lines.append(f"{key}: warm {base / 1024:.1f} MiB, peak {peak / 1024:.1f} MiB (+{(peak - base) / 1024:.1f}, "
                     f"allowed {allowed / 1024:.0f}), second-half trend {slope:+.2f} MiB/h "
                     f"(allowed {limit:.1f}) {'ok' if good else 'FAIL'}")
    for key, extra in (("threads", 4), ("fds", 8)):
        early = max(r[key] for r in after[: len(after) // 2])
        top = max(r[key] for r in late)
        good = top <= early + extra
        ok &= good
        lines.append(f"{key}: first-half max {early}, second-half max {top} {'ok' if good else 'FAIL'}")
    slope = slope_per_hour(late, "cache_kb") / 1024
    good = slope <= args.max_slope
    ok &= good
    lines.append(f"cache dir: {late[-1]['cache_kb'] / 1024:.1f} MiB, second-half trend {slope:+.2f} MiB/h "
                 f"{'ok' if good else 'FAIL'}")
    with LOCK:
        c = dict(COUNTS)
    lines.append("work: " + ", ".join(f"{k} {v}" for k, v in c.items()))
    if c["stall_fails"] or c["remount_fails"]:
        lines.append("note: some stalls were not failed by the watchdog, or some remounts failed")
    return ok, lines


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--sacdfs", required=True, help="the sacdfs binary")
    ap.add_argument("--images", required=True, type=Path, help="a directory of .iso images")
    ap.add_argument("--work", type=Path, help="work directory (default: a new one under /var/tmp)")
    ap.add_argument("--out", type=Path, default=Path("soak.tsv"))
    ap.add_argument("--hours", type=float, default=2.0)
    ap.add_argument("--interval", type=float, default=60, help="seconds between samples")
    ap.add_argument("--warmup", type=float, default=600, help="seconds before the verdict looks")
    ap.add_argument("--readers", type=int, default=4)
    ap.add_argument("--read-pause", type=float, default=0.05)
    ap.add_argument("--cache-mb", type=int, default=64)
    ap.add_argument("--request-timeout", type=int, default=3)
    ap.add_argument("--churn-every", type=float, default=5)
    ap.add_argument("--churn-slots", type=int, default=0,
                    help="reuse this many album names (0: every added album has a new name)")
    ap.add_argument("--stall-every", type=float, default=180)
    ap.add_argument("--remount-every", type=float, default=300)
    ap.add_argument("--noise-mb", type=float, default=32)
    ap.add_argument("--max-slope", type=float, default=4, help="MiB per hour")
    ap.add_argument("--trend-floor-mb", type=float, default=4,
                    help="second-half growth (by the trend) that passes whatever the slope")
    ap.add_argument("--wrap", default="", help="command line to run the main sacdfs under")
    args = ap.parse_args()
    args.sacdfs = str(Path(args.sacdfs).resolve())
    if args.work is None:
        args.work = Path(subprocess.run(["mktemp", "-d", "/var/tmp/sacdfs-soak.XXXXXX"],
                                        capture_output=True, text=True, check=True).stdout.strip())
    args.work = args.work.resolve()
    images = sorted(p.resolve() for p in args.images.glob("*.iso"))
    if len(images) < 2:
        sys.exit("--images needs at least two .iso images")
    lib = args.work / "lib"
    for i, img in enumerate(images):
        (lib / f"a{i}").mkdir(parents=True, exist_ok=True)
        (lib / f"a{i}" / "disc.iso").symlink_to(img)
    (lib / "churn").mkdir()
    (lib / "stall").mkdir()
    (lib / "stall" / "disc.iso").symlink_to(images[0])
    for d in ("tags", "cache", "mnt", "mnt2"):
        (args.work / d).mkdir(exist_ok=True)
    keys = [tags_key(args, img) for img in images]
    for n, k in enumerate(keys):
        write_tags(args, k, n)

    # SIGTERM or SIGINT end the run early, with a verdict on what was sampled (a background
    # job of a non-interactive shell ignores SIGINT, so SIGTERM is the one to send).
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_: STOP.set())
    log(f"work directory {args.work}; {len(images)} images; {args.hours} h")
    main_mount = Mount(args, lib, args.work / "mnt", "main.log", args.wrap)
    state = {"files": files_under(args.work / "mnt")}
    threads = [threading.Thread(target=reader, args=(i, args, state), daemon=True) for i in range(args.readers)]
    threads += [threading.Thread(target=f, args=a, daemon=True) for f, a in (
        (refresher, (args, state)), (churner, (args, images, keys)), (staller, (args, images[0])),
        (remounter, (args,)))]
    for t in threads:
        t.start()

    keys_out = ["t", "rss_kb", "hwm_kb", "anon_kb", "threads", "fds", "cache_kb"] + list(COUNTS)
    rows = []
    t0 = time.monotonic()
    try:
        run(args, main_mount, keys_out, rows, t0)
    finally:
        STOP.set()
        for t in threads:
            t.join(timeout=120)
        alive = main_mount.proc.poll() is None
        main_mount.stop(300)
    ok, lines = verdict(rows, args)
    if not alive:
        ok = False
        lines.append("the main sacdfs exited before the end")
    for line in lines:
        log(line)
    log("PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)


def run(args, main_mount, keys_out, rows, t0):
    with open(args.out, "w") as out:
        out.write("\t".join(keys_out) + "\n")
        while time.monotonic() - t0 < args.hours * 3600 and not STOP.is_set():
            STOP.wait(args.interval)
            if main_mount.proc.poll() is not None:
                log("the main sacdfs exited")
                break
            r = {"t": round(time.monotonic() - t0)} | sample(main_mount.pid, args.work / "cache")
            with LOCK:
                r |= COUNTS
            rows.append(r)
            out.write("\t".join(str(r[k]) for k in keys_out) + "\n")
            out.flush()
            log(f"rss {r['rss_kb'] // 1024} MiB anon {r['anon_kb'] // 1024} MiB threads {r['threads']} "
                f"fds {r['fds']} cache {r['cache_kb'] // 1024} MiB reads {r['reads']}")


if __name__ == "__main__":
    main()
