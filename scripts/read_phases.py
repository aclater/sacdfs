#!/usr/bin/env python3
"""Image reads per phase, and a check against a previous release's.

Mounts a library read-only with a fresh index cache for each phase, runs the phase's reads,
unmounts, and takes the "image reads for audio: N calls, B bytes" line sacdfs logs at the end.
Phases (each on its own fresh mount):
  list      list the whole tree, unmount at once
  idle90    list, then 90 s with no read
  headtail  list, then read the first and the last 64 KiB of every file

  read_phases.py run --sacdfs BIN --library DIR --out RESULT.json [--format dsf] [--phases list,idle90,headtail]
  read_phases.py check --base BASE.json --head HEAD.json [--max-percent 1.0]

`check` exits 1 when a phase of HEAD reads more than MAX-PERCENT percent over BASE in bytes or
calls, and more than 1 MiB or 50 calls (a phase BASE did not run is skipped). A jump fails until it is explained: build the
commits between the two releases and run the failing phase on each (docs/test-plan.md,
"Read counts by phase"). Not a timing run: it needs no benchmark lock; run it at low priority.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time

PHASES = ("list", "idle90", "headtail")
SLACK = {"bytes": 1 << 20, "calls": 50}
LINE = re.compile(r"image reads for audio: (\d+) calls, (\d+) bytes")


def run_phase(binary, library, fmt, phase, work):
    mnt = tempfile.mkdtemp(prefix="mnt-", dir=work)
    idx = tempfile.mkdtemp(prefix="idx-", dir=work)
    log = os.path.join(work, f"log-{phase}.txt")
    if os.path.exists(log):
        os.remove(log)
    cmd = [binary, "-f", "-v", "--index-cache", idx, "--log-file", log]
    if fmt:
        cmd += ["--format", fmt]
    cmd += [library, mnt]
    p = subprocess.Popen(cmd)
    t0 = time.time()
    while not os.path.ismount(mnt):
        if p.poll() is not None or time.time() - t0 > 120:
            raise SystemExit(f"{phase}: the mount did not come up")
        time.sleep(0.2)
    files = [os.path.join(r, f) for r, _, fs in os.walk(mnt) for f in fs]
    if phase == "idle90":
        time.sleep(90)
    elif phase == "headtail":
        for f in sorted(files):
            try:
                fd = os.open(f, os.O_RDONLY)
                size = os.fstat(fd).st_size
                os.pread(fd, 65536, 0)
                if size > 65536:
                    os.pread(fd, 65536, max(0, size - 65536))
                os.close(fd)
            except OSError:
                pass
    subprocess.call(["fusermount3", "-u", mnt])
    p.wait()
    m = None
    with open(log, errors="replace") as f:
        for line in f:
            m = LINE.search(line) or m
    if not m:
        raise SystemExit(f"{phase}: no 'image reads for audio' line in {log}")
    return {"files": len(files), "calls": int(m.group(1)), "bytes": int(m.group(2))}


def cmd_run(a):
    out = {}
    with tempfile.TemporaryDirectory(prefix="read-phases-") as work:
        for ph in a.phases.split(","):
            if ph not in PHASES:
                raise SystemExit(f"unknown phase {ph}")
            out[ph] = run_phase(a.sacdfs, a.library, a.format, ph, work)
            print(f"{ph}: {out[ph]['calls']} calls, {out[ph]['bytes']} bytes, {out[ph]['files']} files", flush=True)
    with open(a.out, "w") as f:
        json.dump(out, f, indent=2)


def cmd_check(a):
    with open(a.base) as f:
        base = json.load(f)
    with open(a.head) as f:
        head = json.load(f)
    bad = 0
    for ph in PHASES:
        if ph not in base or ph not in head:
            continue
        for key in ("bytes", "calls"):
            b, h = base[ph][key], head[ph][key]
            slack = SLACK[key]  # room for the folder warmer's own timing, a few calls and KB
            pct = (h - b) * 100.0 / b if b else 0.0
            over = h - b > max(slack, b * a.max_percent / 100.0)
            flag = "FAIL" if over else "ok"
            bad += flag == "FAIL"
            print(f"{flag:4} {ph:9} {key:5} base {b} head {h} ({pct:+.2f} %)")
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sp = ap.add_subparsers(dest="cmd", required=True)
    r = sp.add_parser("run")
    r.add_argument("--sacdfs", required=True)
    r.add_argument("--library", required=True)
    r.add_argument("--out", required=True)
    r.add_argument("--format", default="dsf")
    r.add_argument("--phases", default=",".join(PHASES))
    c = sp.add_parser("check")
    c.add_argument("--base", required=True)
    c.add_argument("--head", required=True)
    c.add_argument("--max-percent", type=float, default=1.0)
    a = ap.parse_args()
    if a.cmd == "run":
        cmd_run(a)
        return 0
    return cmd_check(a)


if __name__ == "__main__":
    sys.exit(main())
