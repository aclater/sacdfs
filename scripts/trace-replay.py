#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Replays the opens, reads and closes of a read trace against a mount (#456).

  scripts/trace-replay.py TRACE MOUNT --play R [--before 3] [--secs 20] [--prebuf 0.5]
  scripts/trace-replay.py TRACE --play R            # the trace's own numbers, no replay
  scripts/trace-replay.py TRACE --anonymise OUT.gz  # without names, paths or process ids
  scripts/trace-replay.py TRACE --play R --profile  # R's player profile, as JSON
  scripts/trace-replay.py --player PROFILE MOUNT    # a player that profile describes, on a mount

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

A profile (#506, tests/fixtures/players) is what a trace shows of one player's start: its probe
handles' reads of the file it then plays, where the playing handle's first read lands, its read
size, how fast it reads at the start (`rate`, times real time) and how far ahead of playback it
fills its buffer (`lead`, seconds), and the heads and tails of the folder's other files it
re-reads while it starts (`rescan`). `--player` acts it out against a mount as a closed loop:
the playing handle asks for its next read when it holds less than `lead` seconds more than it
has played and less than `rate` times the time since its first read, so a slower mount gets
fewer requests, as a player would send. A rescan that takes `turns` with the playing handle
sends one request at a time over the same connection, as the Zidoo trace shows; otherwise it
runs beside it. Offsets are kept as in a replay: header reads in the header, audio reads at the
same byte of the audio (`a`), tail reads at the same distance from the end (`e`).
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
        # Only a file opened before this one: two that open and close in the same millisecond
        # would otherwise wait for each other.
        prev = max((g for g in hs[:hs.index(h)] if g.t_close is not None and g.t_close <= h.t_open),
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


# Bytes after the audio start that a read may end within and still be a re-read of a head
# (#503's HEAD_SCAN is 2 MiB; the Zidoo's DSF heads end at 1.6 MiB).
RESCAN_HEAD = 5 << 19
# Reads this close to a file's end are reads of its tail (the DSF tag, the bytes after WAV data).
TAIL = 1 << 16


def place(off, n, size, audio):
    """A traced read as [kind, offset, length]: in the header (`h`), in the audio from its
    start (`a`), or from the file's end (`e`, offset negative)."""
    if off >= size - TAIL:
        return ["e", off - size, n]
    if off >= audio:
        return ["a", off - audio, n]
    return ["h", off, n]


def located(where, size, audio, trace_audio):
    """The local offset of a [kind, offset, length] for a file of `size` bytes whose audio
    starts at `audio` (the trace's at `trace_audio`)."""
    kind, off, n = where
    if kind == "e":
        lo = size + off
    elif kind == "a":
        lo = audio + off
    else:
        lo = off * audio // max(trace_audio, 1)
    return max(0, min(lo, size)), n


def profile(handles, play, before, secs, wav_start):
    """What the trace shows of the player of reader `play`, as a profile for --player."""
    p = handles[play]
    hs, t0, t1 = window(handles, play, before, secs)
    audio_of = lambda h: 92 if h.ext == "dsf" else wav_start
    first = p.reads[0][0]
    probes = []
    for h in hs:
        if h is p or h.name != p.name or h.t_open > p.t_open or not h.reads:
            continue
        probes.append([place(off, n, h.size, audio_of(h)) for _, off, n, _ in h.reads])
    reads = [x for x in p.reads if x[0] <= t1]
    rate = trace_rate(p)
    # Audio held over time, by request arrival: the player's demand where the mount kept up.
    audio = audio_of(p)
    held0 = max(0.0, (reads[0][1] - audio) / rate)
    have, pending, ev = reads[0][1], [], []
    for t, off, n, _ in sorted(reads, key=lambda x: x[0]):
        if off >= p.size - TAIL:
            continue
        pending.append((off, off + n))
        pending.sort()
        while pending and pending[0][0] <= have:
            have = max(have, pending.pop(0)[1])
        ev.append(((t - first) / 1000, max(0.0, (have - audio) / rate) - held0))
    # Its rate over the first 5 s, and the furthest it got ahead of real time.
    burst = max([a for t, a in ev if t <= 5.0] + [0.0]) / 5.0
    lead = max((a - t for t, a in ev), default=0.0)
    others = [h for h in hs if h.name != p.name and h.reads]
    rescan = None
    heads = [h for h in others if all(off >= h.size - TAIL or off + n <= audio_of(h) + RESCAN_HEAD
                                      for _, off, n, _ in h.reads)]
    if heads:
        ends = collections.defaultdict(list)
        for h in heads:
            ends[h.ext].append(max(off + n - audio_of(h) for _, off, n, _ in h.reads if off < h.size - TAIL))
        lens = collections.Counter(n for h in heads for _, _, n, _ in h.reads)
        # Turns: no rescan read arrived while a read of the playing file was in progress.
        busy = [(t, t + lat) for t, _, _, lat in reads]
        overlap = sum(1 for h in heads for t, _, _, _ in h.reads if any(a < t < b for a, b in busy))
        rescan = {
            "files": len({h.name for h in heads}),
            "exts": sorted(ends),
            "head": {e: int(sorted(v)[len(v) // 2]) for e, v in ends.items()},
            "tail": max((n for h in heads for off, n in ((o, n) for _, o, n, _ in h.reads) if off >= h.size - TAIL),
                        default=0),
            "read": lens.most_common(1)[0][0],
            "start_s": round((min(h.t_open for h in heads) - p.t_open) / 1000, 2),
            "turns": overlap <= 0.05 * sum(len(h.reads) for h in heads),
        }
    return {
        "file": {"ext": p.ext, "channels": p.channels},
        "probes": probes,
        "play_after_s": round((p.t_open - min([p.t_open] + [h.t_open for h in hs if h.name == p.name])) / 1000, 2),
        "play": {
            "from": place(reads[0][1], 0, p.size, audio)[:2],
            "read": collections.Counter(n for _, _, n, _ in reads).most_common(1)[0][0],
            "rate": round(max(burst, 1.0), 2),
            "lead": round(max(lead, 0.5), 1),
        },
        "rescan": rescan,
    }


def act(prof, mount, secs, prebuf):
    """Acts out a profile on a mount; returns the playing handle's numbers, and the rescan's."""
    want = (prof["file"]["channels"], prof["file"]["ext"])
    local = local_files(mount)
    if not local[want]:
        raise SystemExit(f"no {want[0]}ch .{want[1]} file under the mount")
    path, size, audio, rate = max(local[want], key=lambda c: c[1])
    trace_audio = prof.get("trace_audio", audio)
    buf = mmap.mmap(-1, 1 << 20)

    def pread(fd, off, n):
        return os.preadv(fd, [memoryview(buf)[:n]], off)

    scan = []
    rs = prof.get("rescan")
    if rs:
        d = os.path.dirname(path)
        names = [f for f in sorted(os.listdir(d))
                 if f.endswith(tuple("." + e for e in rs["exts"])) and os.path.join(d, f) != path]
        for f in names[:rs["files"]]:
            q = os.path.join(d, f)
            qs, head = os.path.getsize(q), chunks(q)["audio"] + rs["head"].get(f[-3:], 0)
            scan += [(q, o, rs["read"]) for o in range(0, min(head, qs), rs["read"])]
            if rs["tail"]:
                scan.append((q, (qs - rs["tail"]) & ~4095, rs["tail"]))
    play = prof["play"]
    play_at = prof.get("play_after_s", 0.0)
    turns = bool(scan) and rs["turns"]
    sbuf = mmap.mmap(-1, 1 << 20)
    scan_fds, scan_done = {}, [None]
    reads = []
    start = time.monotonic()
    end_at = start + play_at + secs

    def scan_one():
        q, o, n = scan.pop(0)
        f = scan_fds.get(q) or scan_fds.setdefault(q, os.open(q, os.O_RDONLY | os.O_DIRECT))
        os.preadv(f, [memoryview(sbuf)[:n]], o)
        if not scan:
            scan_done[0] = time.monotonic() - start - play_at

    def side():
        time.sleep(max(0.0, start + play_at + rs.get("start_s", 0.0) - time.monotonic()))
        while scan and time.monotonic() < end_at:
            scan_one()

    t_scan = None
    if scan and not turns:
        t_scan = threading.Thread(target=side)
        t_scan.start()
    for probe in prof["probes"]:
        fd = os.open(path, os.O_RDONLY | os.O_DIRECT)
        for where in probe:
            pread(fd, *located(where, size, audio, trace_audio))
        os.close(fd)
    while turns and scan and time.monotonic() < start + play_at:
        if time.monotonic() >= start + play_at + rs.get("start_s", 0.0):
            scan_one()
        else:
            time.sleep(0.005)
    time.sleep(max(0.0, start + play_at - time.monotonic()))
    fd = os.open(path, os.O_RDONLY | os.O_DIRECT)
    p0 = time.monotonic()
    off = located(play["from"] + [0], size, audio, trace_audio)[0]
    first_off, held0 = off, max(0.0, (off - audio) / rate)
    begun = [None]

    def wants(now):
        held = max(0.0, (off - audio) / rate)
        if begun[0] is None:
            if held < prebuf + held0:
                return True
            begun[0] = now
        return held - held0 < play["rate"] * (now - p0) + prebuf and held < held0 + (now - begun[0]) + play["lead"]

    while True:
        now = time.monotonic()
        if now >= end_at:
            break
        if wants(now):
            a = time.monotonic()
            n = pread(fd, off, play["read"])
            reads.append((a - p0, time.monotonic() - p0, off, play["read"]))
            if n <= 0:
                break
            off += n
        elif not turns or not scan:
            time.sleep(0.005)
        if turns and scan:
            scan_one()
    os.close(fd)
    if t_scan:
        t_scan.join()
    for f in scan_fds.values():
        os.close(f)
    out = numbers(reads, first_off, audio, rate, prebuf)
    out["rescan_done_s"] = None if scan_done[0] is None else round(scan_done[0], 2)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("trace", nargs="?")
    ap.add_argument("mount", nargs="?")
    ap.add_argument("--play", type=int, help="the playing reader's number (r=)")
    ap.add_argument("--anonymise", metavar="OUT", help="write TRACE without names, paths or pids to OUT (.gz: gzip)")
    ap.add_argument("--before", type=float, default=3.0, help="seconds before its open to start")
    ap.add_argument("--secs", type=float, default=20.0, help="seconds after its open to stop")
    ap.add_argument("--prebuf", type=float, default=0.5, help="seconds of audio before playback starts")
    ap.add_argument("--wav-audio-start", type=int, help="bytes before the audio of the traced WAV files")
    ap.add_argument("--reads", help="write the played file's reads here (TSV)")
    ap.add_argument("--profile", action="store_true", help="print R's player profile (JSON) and stop")
    ap.add_argument("--player", metavar="PROFILE", help="act out the player of a profile on MOUNT")
    a = ap.parse_args()
    if a.player:
        mount = a.mount or a.trace
        if not mount or (a.mount and a.trace):
            ap.error("--player PROFILE takes a MOUNT and no TRACE")
        with open(a.player) as f:
            prof = json.load(f)
        out = {"player": prof.get("name", os.path.basename(a.player)), "secs": a.secs}
        out["replay"] = act(prof, mount, a.secs, a.prebuf)
        print(json.dumps(out))
        return
    if a.anonymise:
        anonymise(a.trace, a.anonymise)
        return
    if a.play is None:
        ap.error("--play is needed")

    handles = parse(a.trace)
    hs, t0, t1 = window(handles, a.play, a.before, a.secs)
    p = handles[a.play]
    wav_start = a.wav_audio_start or wav_audio_start(handles, trace_rate) or 0
    if a.profile:
        prof = profile(handles, a.play, a.before, a.secs, wav_start)
        prof["trace_audio"] = 92 if p.ext == "dsf" else wav_start
        print(json.dumps(prof))
        return
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
