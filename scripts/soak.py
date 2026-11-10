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

Read errors are sorted into classes by path and errno (see `classify`): the ones the churner and
the staller cause are counted apart (columns `err_*` of the TSV, and `<out>.errors.tsv` by class,
errno and area), and any other read error fails the run, with a few paths if `--sample-paths`.

PASS if, after `--warmup`, the resident and anonymous memory stay within the cache cap plus
`--noise-mb` of their level at the end of the warm-up, their trend over the second half is
at most `--max-slope` MiB per hour (or adds under `--trend-floor-mb` over that half) once the
largest single step is set aside (see `step_trend`), threads (by their median and end level; a
short spike may pass `--thread-spike`, never twice that) and open files do not climb, and the
cache directory plateaus. Exit status 0 on PASS, 1 on FAIL.

`--wrap 'heaptrack -o OUT'` (or valgrind massif) runs the main sacdfs under a profiler.
"""
import argparse
import collections
import errno
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
COUNTS = {"bytes": 0, "reads": 0, "read_errors": 0, "err_churn": 0, "err_stall": 0, "err_tags": 0, "err_sidecar": 0,
          "err_unexpected": 0, "churn": 0, "stalls": 0, "stall_fails": 0,
          "remounts": 0, "remount_fails": 0}


def count(key, n=1):
    with LOCK:
        COUNTS[key] += n


# Read errors by (class, errno name), and a few paths of the unexpected ones.
ERRORS = collections.Counter()
FAILED_PATHS = collections.defaultdict(set)  # class -> the distinct paths that failed
SAMPLES = []
# When the reader's file list was taken, and when the churner last edited a tags file or sidecar
# or toggled the art folder, and the stall window (monotonic seconds; 0: none yet).
# What the churner removed, with the time: cover files and churn albums by name.
REMOVED = {}
TIMES = {"listed": 0.0, "tags_edit": 0.0, "art_edit": 0.0, "stall_from": 0.0, "stall_until": 0.0}


def classify(path, err, mnt):
    """Expected or not, for a failed read of `path` (under the mount `mnt`) with `err`.

    Expected, and only these:
    - churn: a path under the churn folders, in an album the churner has removed: ENOENT or
      ENOTDIR (at any time: the album is gone), or EIO within 30 s of the removal (a request
      that was in flight when the image's link went away);
    - stall: a path under the stall folder, from the staller's link change until the stuck
      requests are released and a few watchdog timeouts have passed, with EIO, ETIMEDOUT,
      ENOENT or EINTR (the watchdog fails the request, #141);
    - tags: ENOENT on a .dsf when the churner edited a tags file after the reader's list was
      taken, or less than 60 s ago (a tags edit can rename a track's file, so the listed name
      is gone);
    - sidecar: ENOENT on a cover-*.jpg the churner has removed (by name, at any time: sacdfs
      lists the file until it has re-read the folder, which takes seconds on real discs, and
      the file is gone), or on a file under Artwork/ less than 60 s after the churner toggled
      that folder; ESTALE on such a cover-*.jpg too (art whose source changed since it was listed
      is refused, not served as other bytes: design.md, "Changes while mounted"); and ENOENT or
      ESTALE on an area's cover.jpg (Stereo/ or Multichannel/) in the art-churn album a0, once
      the churner has removed a cover file (#279);
    Everything else is unexpected, whatever its errno, and fails the soak.
    """
    try:
        rel = Path(path).relative_to(mnt).parts
    except ValueError:
        rel = ()
    now = time.monotonic()
    e = err.errno
    with LOCK:
        removed = dict(REMOVED)
    if rel[:2] and rel[0] == "churn":
        gone = removed.get("album:" + rel[1] if len(rel) > 1 else "")
        if gone is not None and (e in (errno.ENOENT, errno.ENOTDIR) or (e == errno.EIO and now - gone <= 30)):
            return "churn"
    if (rel[:1] == ("stall",) and TIMES["stall_from"] <= now <= TIMES["stall_until"]
            and e in (errno.EIO, errno.ETIMEDOUT, errno.ENOENT, errno.EINTR)):
        return "stall"
    if (e == errno.ENOENT and rel and rel[-1].endswith(".dsf")
            and (TIMES["tags_edit"] > TIMES["listed"] or now - TIMES["tags_edit"] <= 60)):
        return "tags"
    if e == errno.ENOENT and rel and (("file:" + rel[-1]) in removed
                                      or ("Artwork" in rel[:-1] and now - TIMES["art_edit"] <= 60)):
        return "sidecar"
    if rel[:1] == ("a0",) and any(k.startswith("file:") for k in removed):  # the art-churn album only
        if e == errno.ESTALE and ("file:" + rel[-1]) in removed and rel[-1].startswith("cover-"):
            return "sidecar"
        if (e in (errno.ENOENT, errno.ESTALE) and len(rel) >= 3 and rel[-1] == "cover.jpg"
                and rel[-2] in ("Stereo", "Multichannel")):
            return "sidecar"
    return "unexpected"


def record_error(path, err, args):
    cls = classify(path, err, args.work / "mnt")
    name = errno.errorcode.get(err.errno, str(err.errno))
    top = Path(path).relative_to(args.work / "mnt").parts[0] if Path(path).is_relative_to(args.work / "mnt") else "?"
    with LOCK:
        COUNTS["read_errors"] += 1
        COUNTS["err_" + cls] += 1
        FAILED_PATHS[cls].add(str(path))
        ERRORS[(cls, name, "churn" if top == "churn" else "stall" if top == "stall" else "other")] += 1
        if cls == "unexpected" and len(SAMPLES) < 10:
            SAMPLES.append((name, str(path) if args.sample_paths else f"<{top}>"))


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
            # Say why: sacdfs's own words are in its stderr and log, in the work directory
            # (which a CI runner throws away).
            why = ""
            for name in (f"{logname}.stderr", logname):
                try:
                    text = (args.work / name).read_text(errors="replace").strip()
                except OSError:
                    continue
                if text:
                    why += f"\n--- {name} ---\n{text[-4000:]}"
            raise RuntimeError(f"sacdfs exited with {self.proc.returncode} (mount on {mp}){why}")
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
        except OSError as e:
            record_error(p, e, args)
        time.sleep(args.read_pause)


def refresher(args, state):
    while not STOP.is_set():
        try:
            TIMES["listed"] = time.monotonic()
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
                    with LOCK:  # noted first: a reader may fail the moment the file is gone
                        REMOVED["file:" + old.name] = time.monotonic()
                    old.unlink()
            elif act == 2:  # a tags file is edited
                TIMES["tags_edit"] = time.monotonic()
                write_tags(args, rnd.choice(keys), n)
            elif act == 3:  # an art folder comes and goes
                TIMES["art_edit"] = time.monotonic()
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
                    with LOCK:
                        REMOVED["album:" + d.name] = time.monotonic()
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
            TIMES["stall_from"] = time.monotonic()
            TIMES["stall_until"] = float("inf")
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
            # abandoned requests may still fail for a few timeouts
            TIMES["stall_until"] = time.monotonic() + 3 * args.request_timeout
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
            "swap_kb": int(st.get("VmSwap") or 0),
            "threads": int(st["Threads"]), "fds": fds, "cache_kb": du}


def sample_maps(pid, t):
    """One line per mapping of `pid` with at least 64 KiB resident, from /proc/PID/smaps (#247):
    `t start perms size_kb rss_kb anon_kb name`; anonymous ones are named `[anon]`. Lets a step
    in the resident memory be traced to the heap (`[heap]`), a thread stack, an arena or a large
    allocation's own mapping, and to the pages that changed."""
    out = []
    cur = None
    for line in Path(f"/proc/{pid}/smaps").read_text().splitlines():
        head = line.split()
        if "-" in head[0] and not line.startswith(("Rss:", "Size:")) and ":" not in head[0]:
            if cur and cur[4] >= 64:
                out.append(cur)
            name = head[5] if len(head) > 5 else "[anon]"
            cur = [t, head[0], head[1], 0, 0, 0, name]
        elif cur and head[0] == "Size:":
            cur[3] = int(head[1])
        elif cur and head[0] == "Rss:":
            cur[4] = int(head[1])
        elif cur and head[0] == "Anonymous:":
            cur[5] = int(head[1])
    if cur and cur[4] >= 64:
        out.append(cur)
    return out


def malloc_info(pid, path):
    """Asks the process for glibc's malloc_info XML through gdb (it stops the process for a
    moment; for diagnosis only, `--malloc-info-every`)."""
    cmd = ["gdb", "-p", str(pid), "-batch", "-ex", f'set $f = (void*)fopen("{path}", "w")',
           "-ex", "call (int)malloc_info(0, $f)", "-ex", "call (int)fclose($f)"]
    subprocess.run(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                   check=False, timeout=120)


def slope_per_hour(rows, key):
    """Least-squares slope of `key` over time, in units per hour."""
    xs = [r["t"] / 3600 for r in rows]
    ys = [r[key] for r in rows]
    mx, my = statistics.fmean(xs), statistics.fmean(ys)
    den = sum((x - mx) ** 2 for x in xs)
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / den if den else 0.0


def step_trend(rows, key, windows=6):
    """Growth rate of `key` (units per hour) with the largest single step set aside.

    The rows are cut into `windows` runs of equal length and each run is reduced to its median,
    so one odd sample counts for nothing and a step (a level that changes once and stays) shows
    in exactly one difference between neighbouring medians. The largest difference, up or down,
    is dropped with the time it spans, and what is left is the rate at which the level kept
    moving: a steady leak shows in every difference and stays, a step followed by a plateau does
    not. (A step down is set aside too, or it would hide a leak that goes on beside it.) Returns (rate per hour, growth left, hours it spans, the step set aside, signed),
    or None when the rows are too few for three windows of three samples.
    """
    k = min(windows, len(rows) // 3)
    if k < 3:
        return None
    size = len(rows) // k
    cuts = [rows[i * size:(i + 1) * size if i < k - 1 else len(rows)] for i in range(k)]
    level = [statistics.median(r[key] for r in c) for c in cuts]
    when = [statistics.median(r["t"] for r in c) / 3600 for c in cuts]
    rises = [level[i + 1] - level[i] for i in range(k - 1)]
    gaps = [when[i + 1] - when[i] for i in range(k - 1)]
    big = max(range(k - 1), key=lambda i: abs(rises[i]))
    left = sum(r for i, r in enumerate(rises) if i != big)
    hours = sum(g for i, g in enumerate(gaps) if i != big)
    step = rises[big]
    return (left / hours if hours else 0.0), left, hours, step


def spike_hold(rows, soft):
    """Longest stretch (seconds, first to last sample) of consecutive samples above `soft`."""
    best, start = 0, None
    for r in rows:
        if r["threads"] > soft:
            if start is None:
                start = r["t"]
            best = max(best, r["t"] - start)
        else:
            start = None
    return best


def verdict(rows, args):
    # The kernel may swap pages of a quiet process out and back in on a busy host, which moves
    # the resident size in steps that have nothing to do with sacdfs's allocations (#247). The
    # memory checks therefore judge resident plus swapped (`VmSwap`), which a reclaim leaves
    # alone; the TSV keeps both columns.
    swapped = max((r.get("swap_kb", 0) for r in rows), default=0)
    rows = [dict(r, rss_kb=r["rss_kb"] + r.get("swap_kb", 0), anon_kb=r["anon_kb"] + r.get("swap_kb", 0))
            for r in rows]
    after = [r for r in rows if r["t"] >= args.warmup]
    if len(after) < 6:
        return False, ["too few samples after the warm-up"]
    late = after[len(after) // 2:]
    ok, lines = True, []
    for key in ("rss_kb", "anon_kb"):
        base = statistics.median(r[key] for r in after[:3])
        peak = max(r[key] for r in after)
        allowed = (args.cache_mb + args.noise_mb) * 1024
        # Memory moves in steps of a few MiB, both ways, that nothing in the run explains
        # (#247). A line through the second half takes a step in it for a climb, so the trend
        # is the rate left after the largest single step is set aside. A short second half
        # makes even that noisy: growth under --trend-floor-mb passes too (a 2-hour run: a
        # little under 5 MiB/h).
        trend = step_trend(late, key)
        if trend is None:
            slope = slope_per_hour(late, key) / 1024
            hours, step = (late[-1]["t"] - late[0]["t"]) / 3600, 0.0
        else:
            slope, hours, step = trend[0] / 1024, trend[2], trend[3] / 1024
        limit = max(args.max_slope, args.trend_floor_mb / hours if hours else args.max_slope)
        good = peak - base <= allowed and slope <= limit
        ok &= good
        lines.append(f"{key}: warm {base / 1024:.1f} MiB, peak {peak / 1024:.1f} MiB (+{(peak - base) / 1024:.1f}, "
                     f"allowed {allowed / 1024:.0f}), second-half trend {slope:+.2f} MiB/h "
                     f"(allowed {limit:.1f}; largest step set aside {step:+.1f} MiB; resident plus swapped"
                     f"{f', up to {swapped / 1024:.1f} MiB of it swapped out' if swapped else ''}) "
                     f"{'ok' if good else 'FAIL'}")
    # Threads are judged by level, not by the maximum of a half: the staller makes sacdfs spawn
    # spare threads on purpose (#167/#168) and one sample can land in a stall. A run fails if the
    # median or the end level of the second half is above the first half's median by more than
    # 4 threads (threads that accumulate; the end level is the median of the last sixth of the
    # half, at least 5 samples: three samples sat at 5 above in a healthy 2-hour cut of a 6-hour
    # run). Spare threads end 10 s after their work and a stall
    # lasts well under a minute, so a sample above --thread-spike over that median is allowed
    # when it is short: the run fails if the samples above it span --spike-hold seconds, or if
    # one passes twice --thread-spike. Sampled once a minute, the peak of a healthy run has
    # reached the old single ceiling on some rigs (#247), where one more thread failed it.
    base = statistics.median(r["threads"] for r in after[: len(after) // 2])
    mid = statistics.median(r["threads"] for r in late)
    end = statistics.median(r["threads"] for r in late[-max(5, len(late) // 6):])
    top = max(r["threads"] for r in after)
    soft, hard = base + args.thread_spike, base + 2 * args.thread_spike
    held = spike_hold(after, soft)
    good = mid <= base + 4 and end <= base + 4 and top <= hard and held < args.spike_hold
    ok &= good
    lines.append(f"threads: first-half median {base:g}, second-half median {mid:g}, end {end:g}, "
                 f"peak {top} (spike line {soft:g}, held {held} s of {args.spike_hold:g} allowed; "
                 f"hard ceiling {hard:g}) {'ok' if good else 'FAIL'}")
    early = max(r["fds"] for r in after[: len(after) // 2])
    top = max(r["fds"] for r in late)
    good = top <= early + 8
    ok &= good
    lines.append(f"fds: first-half max {early}, second-half max {top} {'ok' if good else 'FAIL'}")
    slope = slope_per_hour(late, "cache_kb") / 1024
    good = slope <= args.max_slope
    ok &= good
    lines.append(f"cache dir: {late[-1]['cache_kb'] / 1024:.1f} MiB, second-half trend {slope:+.2f} MiB/h "
                 f"{'ok' if good else 'FAIL'}")
    with LOCK:
        c = dict(COUNTS)
    lines.append("work: " + ", ".join(f"{k} {v}" for k, v in c.items()))
    # Read errors: expected ones come from the churner and the staller (see classify); any other
    # read error, on a stable file, is a failure of its own.
    lines.append(f"read errors: {c['read_errors']} of {c['reads'] + c['read_errors']} reads: "
                 f"{c['err_churn']} churned away, {c['err_stall']} in a stall, {c['err_tags']} renamed track, "
                 f"{c['err_sidecar']} replaced sidecar, "
                 f"{c['err_unexpected']} unexpected")
    with LOCK:
        for (cls, name, area), n in sorted(ERRORS.items()):
            lines.append(f"  read errors {cls} {name} ({area}): {n}")
        # A failed read returns at once and the reader tries again after --read-pause, so the
        # counts above are attempts; these are how many different files failed.
        lines.append("  distinct files that failed, by class: "
                     + ", ".join(f"{k} {len(v)}" for k, v in sorted(FAILED_PATHS.items())))
        for name, where in SAMPLES:
            lines.append(f"  unexpected sample: {name} {where}")
    if c["err_unexpected"]:
        ok = False
        lines.append("FAIL: unexpected read errors")
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
    ap.add_argument("--trend-floor-mb", type=float, default=3,
                    help="second-half growth (by the trend, a step set aside) that passes whatever the slope")
    ap.add_argument("--thread-spike", type=int, default=16,
                    help="threads above the first half's median that a short spike may reach; "
                         "twice that is the hard ceiling")
    ap.add_argument("--spike-hold", type=float, default=120,
                    help="seconds the thread count may stay above the spike line (consecutive samples)")
    ap.add_argument("--sample-paths", action="store_true",
                    help="print the paths of unexpected read errors (off: only their top folder; "
                         "leave it off for a library whose names must not be published)")
    ap.add_argument("--smaps", action="store_true",
                    help="also write <out>.smaps.tsv: each sample's larger mappings (#247)")
    ap.add_argument("--malloc-info-every", type=float, default=0,
                    help="every this many seconds, write glibc's malloc_info XML (via gdb) to <out>.malloc-info/")
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
    TIMES["listed"] = time.monotonic()
    state = {"files": files_under(args.work / "mnt")}
    threads = [threading.Thread(target=reader, args=(i, args, state), daemon=True) for i in range(args.readers)]
    threads += [threading.Thread(target=f, args=a, daemon=True) for f, a in (
        (refresher, (args, state)), (churner, (args, images, keys)), (staller, (args, images[0])),
        (remounter, (args,)))]
    for t in threads:
        t.start()

    keys_out = ["t", "rss_kb", "hwm_kb", "anon_kb", "swap_kb", "threads", "fds", "cache_kb"] + list(COUNTS)
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
    with LOCK, open(args.out.with_suffix(".errors.tsv"), "w") as f:
        f.write("class\terrno\tarea\tcount\n")
        for (cls, name, area), n in sorted(ERRORS.items()):
            f.write(f"{cls}\t{name}\t{area}\t{n}\n")
    log("PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)


def run(args, main_mount, keys_out, rows, t0):
    with open(args.out, "w") as out:
        out.write("\t".join(keys_out) + "\n")
        maps_out = open(args.out.with_suffix(".smaps.tsv"), "w") if args.smaps else None
        if maps_out:
            maps_out.write("t\tstart\tperms\tsize_kb\trss_kb\tanon_kb\tname\n")
        mi_dir = args.out.with_suffix(".malloc-info")
        mi_next = 0.0
        if args.malloc_info_every:
            mi_dir.mkdir(exist_ok=True)
        while time.monotonic() - t0 < args.hours * 3600 and not STOP.is_set():
            STOP.wait(args.interval)
            if main_mount.proc.poll() is not None:
                log("the main sacdfs exited")
                break
            r = {"t": round(time.monotonic() - t0)} | sample(main_mount.pid, args.work / "cache")
            with LOCK:
                r |= COUNTS
            rows.append(r)
            if maps_out:
                try:
                    for m in sample_maps(main_mount.pid, r["t"]):
                        maps_out.write("\t".join(str(x) for x in m) + "\n")
                    maps_out.flush()
                except OSError:
                    pass
            if args.malloc_info_every and r["t"] >= mi_next:
                mi_next = r["t"] + args.malloc_info_every
                malloc_info(main_mount.pid, str((mi_dir / f"{r['t']:06d}.xml").resolve()))
            out.write("\t".join(str(r[k]) for k in keys_out) + "\n")
            out.flush()
            log(f"rss {r['rss_kb'] // 1024} MiB anon {r['anon_kb'] // 1024} MiB threads {r['threads']} "
                f"fds {r['fds']} cache {r['cache_kb'] // 1024} MiB reads {r['reads']}")


if __name__ == "__main__":
    main()
