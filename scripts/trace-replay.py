#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Replays the opens, reads and closes of a read trace against a mount (#456).

  scripts/trace-replay.py TRACE MOUNT --play R [--before 3] [--secs 20] [--prebuf 0.5]
  scripts/trace-replay.py TRACE --play R            # the trace's own numbers, no replay
  scripts/trace-replay.py TRACE --anonymise OUT.gz  # without names, paths or process ids

TRACE is a log with the read trace on (`-vv`, docs/user-guide.md "Tracing reads"). R is the
reader number (`r=`) of the playing file. The replay takes every file opened from BEFORE seconds
before R's open to SECS after it, and sends their requests as the trace's client did: each read
or close of a file the same time after the last reply it had seen for that file (after the open,
if none) and not before the read before it; each open at its time, but not before the close
that came before it. A faster mount gets the next request sooner; the client's own delays stay.
Reads use O_DIRECT, so the mount sees the trace's requests, not the page cache's.

Each traced file maps to a file under MOUNT of the same format and channel count, by track:
the played track to the longest such file, the others in turn, a track's DSF and WAV files
to one track. Offsets keep their place: the header in the header, audio at the same byte of
the audio (the trace's WAV header is inferred from its `start-ahead` lines, or given with
--wav-audio-start), and reads in the file's second half at the same distance from its end.

For R it prints its reads' latency and a simulated player: the bytes before R's first read are
in the player's buffer already (an earlier open read them into the page cache), playback starts
once PREBUF seconds of audio are there, and it stalls whenever it has played all it got.
"""
import argparse
import collections
import gzip
import json
import mmap
import os
import re
import threading
import time

LINE = re.compile(r"(?:^|sacdfs::trace\] )([\d.]+) (open|read|close|start-ahead|ahead) (.*)")
OPEN = re.compile(r'fh=(\d+) r=(\d+) pid=(\d+) size=(\d+) "(.*)"')
READ = re.compile(r"fh=(\d+) off=(\d+) len=(\d+)(?: got=\d+)? (hit|miss) q=([\d.]+) sw=([\d.]+) dec=([\d.]+)")
NAME = re.compile(r"#(\d+) (\d+)ch \.(dsf|wav)$")
FIRST = re.compile(r"r=(\d+) no: first audio read at frame (\d+)")


class Handle:
    def __init__(self, r, fh, t, size, name):
        self.r, self.fh, self.t_open, self.size, self.name = r, fh, t, size, name
        m = NAME.search(name)
        self.track, self.channels, self.ext = (int(m[1]), int(m[2]), m[3]) if m else (0, 0, "")
        self.reads = []  # (arrival ms, offset, length, trace latency ms)
        self.t_close = None
        self.first_frame = None


def opener(path, mode="rt"):
    return (gzip.open if path.endswith(".gz") else open)(path, mode, errors="replace")


def parse(path):
    handles, by_fh = {}, {}
    with opener(path) as f:
        for line in f:
            m = LINE.search(line)
            if not m:
                continue
            t, ev, rest = float(m[1]), m[2], m[3]
            if ev == "open":
                o = OPEN.match(rest)
                h = Handle(int(o[2]), int(o[1]), t, int(o[4]), o[5])
                handles[h.r] = by_fh[h.fh] = h
            elif ev == "read":
                o = READ.match(rest)
                h = by_fh.get(int(o[1])) if o else None
                if h:
                    h.reads.append((t, int(o[2]), int(o[3]), float(o[5]) + float(o[6]) + float(o[7])))
            elif ev == "close":
                h = by_fh.get(int(rest.split()[0][3:]))
                if h:
                    h.t_close = t
            elif ev == "start-ahead":
                o = FIRST.match(rest)
                if o and int(o[1]) in handles:
                    handles[int(o[1])].first_frame = int(o[2])
    return handles


def wav_audio_start(handles, rate_of):
    """The trace's WAV header length: the range each `first audio read at frame K` allows."""
    lo, hi = 0, None
    for h in handles.values():
        if h.ext != "wav" or not h.first_frame or not h.reads:
            continue
        fb = rate_of(h) // 75
        for _, off, _, _ in h.reads:
            a, b = off - (h.first_frame + 1) * fb + 1, off - h.first_frame * fb
            if b >= 0:
                lo, hi = max(lo, a), b if hi is None else min(hi, b)
                break
    return hi


def chunks(path):
    """Where a DSF or WAV file's audio starts, its channels and bytes per second. Reads only
    the chunk headers it needs, with O_DIRECT: a read of audio would make the mount decode."""
    fd = os.open(path, os.O_RDONLY | os.O_DIRECT)
    buf = mmap.mmap(-1, 4096)

    def at(off, n):
        got = os.preadv(fd, [memoryview(buf)[:n]], off)
        return bytes(buf[:got])

    try:
        riff = at(0, 12)[:4] == b"RIFF"
        pos, out = (12 if riff else 0), {}
        while True:
            h = at(pos, 12)
            cid = h[:4]
            size = int.from_bytes(h[4:8] if riff else h[4:12], "little")
            if cid == b"fmt ":
                f = at(pos, 40)
                if riff:
                    out["channels"] = int.from_bytes(f[10:12], "little")
                    out["rate"] = int.from_bytes(f[16:20], "little")
                else:
                    out["channels"] = int.from_bytes(f[24:28], "little")
                    out["rate"] = int.from_bytes(f[28:32], "little") * out["channels"] // 8
            if cid == b"data":
                out["audio"] = pos + (8 if riff else 12)
                return out
            if size == 0:
                raise ValueError(f"{path}: no data chunk")
            pos += (8 + size + (size & 1)) if riff else size
    finally:
        os.close(fd)


def local_files(mount):
    files = collections.defaultdict(list)
    for d, _, fs in os.walk(mount):
        for f in sorted(fs):
            if f.endswith((".dsf", ".wav")):
                p = os.path.join(d, f)
                c = chunks(p)
                files[(c["channels"], f[-3:])].append((p, os.path.getsize(p), c["audio"], c["rate"]))
    return files


def trace_rate(h):
    """Bytes per second of a traced file: DSF 2.8224 MHz, WAV 24-bit at 176.4 kHz (the default)."""
    return h.channels * (2822400 // 8 if h.ext == "dsf" else 176400 * 3)


def numbers(reads, first_off, audio_start, rate, prebuf):
    """A player's view of one file's reads: [(sent s, replied s, offset, length)], in sent order."""
    if not reads:
        return {}
    t0 = reads[0][0]
    lat = sorted((b - a) * 1000 for a, b, _, _ in reads)
    # The player has what comes before its first read of audio (header reads come before it).
    first_off = next((off for _, _, off, n in reads if off + n > audio_start), first_off)
    # Bytes in order from the start, as replies come.
    have, pending, events = first_off, [], []
    for _, b, off, n in sorted(reads, key=lambda x: x[1]):
        pending.append((off, off + n))
        pending.sort()
        while pending and pending[0][0] <= have:
            have = max(have, pending.pop(0)[1])
        events.append((b - t0, max(0.0, (have - audio_start) / rate)))
    audio_by = lambda s: max([a for t, a in events if t <= s] + [max(0.0, (first_off - audio_start) / rate)])
    # Playback from the first moment PREBUF seconds are there.
    start = next((t for t, a in events if a >= prebuf), None)
    stalled, stalls, pos, clock = 0.0, 0, 0.0, start
    if start is not None:
        for t, a in events:
            if t <= start:
                continue
            avail = audio_by(t - 1e-9)
            if pos + (t - clock) > avail:  # it ran out before this reply
                ran_out = clock + (avail - pos)
                stalled, stalls = stalled + (t - ran_out), stalls + 1
                pos = avail
            else:
                pos += t - clock
            clock = t
    return {
        "reads": len(reads),
        "worst_ms": round(lat[-1], 1),
        "p95_ms": round(lat[int(len(lat) * 0.95)], 1),
        "over_250ms": sum(1 for x in lat if x > 250),
        "audio_by_1s": round(audio_by(1.0), 2),
        "audio_by_2s": round(audio_by(2.0), 2),
        "audio_by_5s": round(audio_by(5.0), 2),
        "first_audio_s": None if start is None else round(start, 2),
        "stalled_s": round(stalled, 2),
        "stalls": stalls,
    }


def anonymise(src, out):
    """Writes the trace's open, read, close and decode-ahead lines without the log's prefix,
    file names (kept: track number, channels and format), paths or process ids."""
    with opener(src) as f, opener(out, "wt") as o:
        o.write("# A read trace, anonymised by scripts/trace-replay.py --anonymise.\n")
        for line in f:
            m = LINE.search(line)
            if not m:
                continue
            rest = m[3]
            if m[2] == "open":
                g = OPEN.match(rest)
                n = NAME.search(g[5])
                rest = f'fh={g[1]} r={g[2]} pid=0 size={g[4]} "{n[0] if n else ""}"'
            o.write(f"{m[1]} {m[2]} {rest}\n")


def window(handles, play, before, secs):
    p = handles[play]
    t0, t1 = p.t_open - before * 1000, p.t_open + secs * 1000
    return sorted((h for h in handles.values() if t0 <= h.t_open <= t1), key=lambda h: h.t_open), t0, t1


def replay(hs, t0, t1, mapping):
    """Sends the requests of the handles `hs`; returns {r: [(sent, replied, offset, length)]}."""
    start = time.monotonic() + 0.5
    at = lambda ms: start + (ms - t0) / 1000
    done_close = {h.r: threading.Event() for h in hs}
    close_time = {}
    results = collections.defaultdict(list)
    lock = threading.Lock()

    def sleep_until(t):
        d = t - time.monotonic()
        if d > 0:
            time.sleep(d)

    def run(h):
        path, lmap = mapping[h.r]
        # Not before the replayed close that came last before this open in the trace.
        prev = max((g for g in hs if g.t_close is not None and g.t_close <= h.t_open and g is not h),
                   key=lambda g: g.t_close, default=None)
        sleep_until(at(h.t_open))
        if prev is not None:
            done_close[prev.r].wait()
            sleep_until(close_time[prev.r] + (h.t_open - prev.t_close) / 1000)
        fd = os.open(path, os.O_RDONLY | os.O_DIRECT)
        opened = time.monotonic()
        reads = [x for x in h.reads if x[0] <= t1]
        replies = [None] * len(reads)
        sent = [None] * len(reads)
        evs = [threading.Event() for _ in reads]
        threads = []

        def one(i, off, n):
            buf = mmap.mmap(-1, max(n, 4096))
            s = time.monotonic()
            try:
                os.preadv(fd, [memoryview(buf)[:n]], off)
            except OSError:
                pass
            e = time.monotonic()
            replies[i] = e
            evs[i].set()
            with lock:
                results[h.r].append((s - start, e - start, off, n))

        def when(i, arrival):
            """When to send a request that arrived at `arrival` (trace ms) after reads [0, i): as
            long after the last reply the client had then as in the trace, not before the read
            before it, and as long after the open if it had none."""
            seen = [j for j in range(i) if reads[j][0] + reads[j][3] <= arrival]
            if seen:
                j = max(seen, key=lambda j: reads[j][0] + reads[j][3])
                evs[j].wait()
                t = replies[j] + (arrival - reads[j][0] - reads[j][3]) / 1000
            else:
                t = opened + (arrival - h.t_open) / 1000
            return max(t, sent[i - 1]) if i else t

        for i, (a, off, n, _) in enumerate(reads):
            loff, ln = lmap(off, n)
            sleep_until(when(i, a))
            sent[i] = time.monotonic()
            th = threading.Thread(target=one, args=(i, loff, ln))
            th.start()
            threads.append(th)
        end = h.t_close if h.t_close is not None and h.t_close <= t1 else t1
        sleep_until(when(len(reads), end))
        for th in threads:
            th.join()
        os.close(fd)
        close_time[h.r] = time.monotonic()
        done_close[h.r].set()

    ts = [threading.Thread(target=run, args=(h,)) for h in hs]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    return results


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("trace")
    ap.add_argument("mount", nargs="?")
    ap.add_argument("--play", type=int, help="the playing reader's number (r=)")
    ap.add_argument("--anonymise", metavar="OUT", help="write TRACE without names, paths or pids to OUT (.gz: gzip)")
    ap.add_argument("--before", type=float, default=3.0, help="seconds before its open to start")
    ap.add_argument("--secs", type=float, default=20.0, help="seconds after its open to stop")
    ap.add_argument("--prebuf", type=float, default=0.5, help="seconds of audio before playback starts")
    ap.add_argument("--wav-audio-start", type=int, help="bytes before the audio of the traced WAV files")
    ap.add_argument("--reads", help="write the played file's reads here (TSV)")
    a = ap.parse_args()
    if a.anonymise:
        anonymise(a.trace, a.anonymise)
        return
    if a.play is None:
        ap.error("--play is needed")

    handles = parse(a.trace)
    hs, t0, t1 = window(handles, a.play, a.before, a.secs)
    p = handles[a.play]
    wav_start = a.wav_audio_start or wav_audio_start(handles, trace_rate) or 0
    trace_start = lambda h: 92 if h.ext == "dsf" else wav_start
    out = {"play": f"{p.channels}ch .{p.ext}", "files": len({h.name for h in hs}), "opens": len(hs),
           "reads": sum(len(h.reads) for h in hs), "wav_audio_start": wav_start}

    if a.mount is None:
        reads = [(x[0] / 1000, (x[0] + x[3]) / 1000, x[1], x[2]) for x in p.reads if x[0] <= t1]
        out["trace"] = numbers(reads, p.reads[0][1], trace_start(p), trace_rate(p), a.prebuf)
        print(json.dumps(out))
        return

    local = local_files(a.mount)
    tracks = {}  # (channels, track) -> index into the local list of that channel count
    used = collections.defaultdict(int)
    order = [p] + [h for h in hs if h is not p]
    for h in order:
        key = (h.channels, h.track)
        if key in tracks:
            continue
        cands = local[(h.channels, h.ext)]
        if not cands:
            raise SystemExit(f"no {h.channels}ch .{h.ext} file under the mount")
        if h is p:
            tracks[key] = max(range(len(cands)), key=lambda i: cands[i][1])
        else:
            taken = set(tracks.values())
            free = [i for i in range(len(cands)) if i not in taken] or list(range(len(cands)))
            tracks[key] = free[used[h.channels] % len(free)]
            used[h.channels] += 1

    def mapper(h):
        cands = local[(h.channels, h.ext)]
        path, size, audio, _ = cands[tracks[(h.channels, h.track)] % len(cands)]
        ua = trace_start(h)

        def lmap(off, n):
            if off > h.size // 2:
                lo = size - (h.size - off)
            elif off + n > ua:
                lo = audio + off - ua
            else:
                lo = off * audio // max(ua, 1)
            lo = max(0, min(lo, size))
            return lo, n
        return path, lmap

    mapping = {h.r: mapper(h) for h in hs}
    t = time.monotonic()
    res = replay(hs, t0, t1, mapping)
    out["wall_s"] = round(time.monotonic() - t, 1)
    path, lmap = mapping[a.play]
    lp = local[(p.channels, p.ext)][tracks[(p.channels, p.track)]]
    reads = sorted(res[a.play])
    first_local = lmap(p.reads[0][1], p.reads[0][2])[0]
    out["replay"] = numbers(reads, first_local, lp[2], lp[3], a.prebuf)
    if a.reads:
        with open(a.reads, "w") as f:
            for s, e, off, n in reads:
                f.write(f"{s:.4f}\t{e:.4f}\t{off}\t{n}\t{(e - s) * 1000:.1f}\n")
    print(json.dumps(out))


if __name__ == "__main__":
    main()
