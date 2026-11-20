#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""The player stall gate (#506): replays player profiles and traces against two builds of sacdfs
on a CPU limited to about a slow NAS's speed, and fails when head stalls or waits longer than
base in the first seconds of a track.

  scripts/stall-gate.py --base BIN --head BIN --source DIR [--rounds 5] SCENARIO...

SCENARIO is a profile (tests/fixtures/players/NAME.json, acted out by
`scripts/trace-replay.py --player`) or TRACE:R (reader R of a trace, replayed). Each round mounts
SOURCE afresh for each scenario and build (`--threads 4 --cache-mb 64`, no index cache), with the
builds alternating and the order swapping every round. sacdfs runs on CPUS under a CPU quota,
the client on the other CPUs. The quota is either given (--quota) or found from base (--target):
the share of CPUS at which base converts a 6-channel WAV file at TARGET times real time, as a
4-thread Atom does at about 1.07 to 1.1.

For each scenario and build the gate takes, over the rounds, the playing file's worst read and
the time playback stalled in the first SECS seconds after it opened. It fails when head's median
is above the highest base had in any round by more than both a relative and an absolute margin:
worst read 1.25 times and 250 ms, stall 1.25 times and 1 s. *Why base's highest, not its
median:* some scenarios' worst read is bimodal on shared runners (0.6 or 1.5 s for the same
build), and comparing medians failed a build against itself. docs/performance.md, "The CI stall
gate", says how the margins were chosen. The table goes to standard output, and to the job
summary in CI.
"""
import argparse
import json
import os
import signal
import statistics
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPLAY = os.path.join(HERE, "trace-replay.py")
WAV_RATE = 6 * 176400 * 3  # bytes per second of a 6-channel 24-bit 176.4 kHz WAV file
DEFAULT_WRAP = ("systemd-run --user --scope -q -p CPUQuota={quota}% -p CPUQuotaPeriodSec=10ms "
                "taskset -c {cpus}")


class Mount:
    """SOURCE mounted with BIN under the CPU limit, for a `with` block."""

    def __init__(self, a, binary, quota, keep=None):
        self.a, self.binary, self.quota, self.keep = a, binary, quota, keep

    def __enter__(self):
        self.dir = tempfile.mkdtemp(prefix="stall-gate.")
        self.log = open(self.dir + ".log", "w")
        env = dict(os.environ, RUST_LOG="info,sacdfs::trace=debug") if self.a.log_dir else None
        wrap = self.a.wrap.format(quota=self.quota, cpus=self.a.cpus, uid=os.getuid(), gid=os.getgid()).split()
        cmd = wrap + [self.binary, "-f", "--no-index-cache", "--threads", "4", "--cache-mb", "64",
                      *self.a.sacdfs_arg, self.a.source, self.dir]
        self.proc = subprocess.Popen(cmd, stdout=self.log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, env=env)
        deadline = time.monotonic() + 30
        while not os.path.ismount(self.dir):
            if self.proc.poll() is not None or time.monotonic() > deadline:
                self.proc.kill()
                sys.exit(f"mount failed: {' '.join(cmd)}\n{open(self.dir + '.log').read()}")
            time.sleep(0.05)
        # A player's library lists the folder first.
        for d, _, fs in os.walk(self.dir):
            for f in fs:
                os.stat(os.path.join(d, f))
        return self.dir

    def __exit__(self, *exc):
        for _ in range(50):
            if subprocess.run(["fusermount3", "-u", self.dir], stderr=subprocess.DEVNULL).returncode == 0:
                break
            time.sleep(0.2)
        deadline = time.monotonic() + 30
        while True:
            pid, _, ru = os.wait4(self.proc.pid, os.WNOHANG)
            if pid:
                self.proc.returncode = 0
                self.cpu_s = ru.ru_utime + ru.ru_stime
                break
            if time.monotonic() > deadline:
                self.proc.kill()
            time.sleep(0.05)
        self.log.close()
        os.rmdir(self.dir)
        if self.a.log_dir and self.keep:
            os.replace(self.dir + ".log", os.path.join(self.a.log_dir, self.keep + ".log"))
        else:
            os.unlink(self.dir + ".log")


def client(a):
    return ["taskset", "-c", a.client_cpus, sys.executable, "-I", REPLAY] if a.client_cpus else [sys.executable, "-I", REPLAY]


def run(a, binary, quota, scenario, label=None):
    mount = Mount(a, binary, quota, label)
    with mount as m:
        if scenario.endswith(".json"):
            cmd = client(a) + ["--player", scenario, m]
        else:
            trace, r = scenario.rsplit(":", 1)
            cmd = client(a) + [trace, m, "--play", r, "--before", "3"]
            if a.wav_audio_start:
                cmd += ["--wav-audio-start", str(a.wav_audio_start)]
        out = subprocess.run(cmd + ["--secs", str(a.secs)], capture_output=True, text=True, check=False)
    if out.returncode:
        sys.exit(f"{scenario} with {binary} failed:\n{out.stdout}{out.stderr}")
    n = json.loads(out.stdout.strip().splitlines()[-1])["replay"]
    n["sacdfs_cpu_s"] = round(mount.cpu_s, 2)
    return n


def speed(a, binary, quota):
    """How many times real time `binary` converts 6-channel WAV at `quota`: 24 MiB of the longest
    WAV file, read in order from its start on a fresh mount."""
    with Mount(a, binary, quota) as m:
        wav = max((os.path.join(d, f) for d, _, fs in os.walk(m) for f in fs if f.endswith(".wav")),
                  key=os.path.getsize)
        n, got, t = 24 << 20, 0, time.monotonic()
        with open(wav, "rb", buffering=0) as f:
            while got < n:
                b = f.read(1 << 20)
                if not b:
                    break
                got += len(b)
        return got / WAV_RATE / (time.monotonic() - t)


def calibrate(a, binary):
    """The quota at which `binary` converts 6-channel WAV at about --target times real time.
    *Why measured at the quota itself:* the speed is not proportional to the quota (below the
    start burst, sacdfs plans its decoding differently), so each step measures again."""
    q = 50
    for _ in range(5):
        x = speed(a, binary, q)
        if abs(x / a.target - 1) < 0.08:
            break
        q = max(5, min(400, round(q * a.target / x)))
    return q, x


def name(scenario):
    s = os.path.basename(scenario)
    return s[:-5] if s.endswith(".json") else s.replace(".trace.gz", "")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base", required=True)
    ap.add_argument("--head", required=True)
    ap.add_argument("--source", required=True, help="the folder sacdfs serves (scripts/play-disc.py)")
    ap.add_argument("--rounds", type=int, default=5)
    ap.add_argument("--secs", type=float, default=15.0)
    ap.add_argument("--quota", type=int, help="CPU quota for sacdfs, percent of one CPU")
    ap.add_argument("--target", type=float, default=1.25, help="6-channel WAV speed to find the quota for")
    ap.add_argument("--cpus", default="0,1", help="CPUs sacdfs runs on")
    ap.add_argument("--client-cpus", default="2,3", help="CPUs the client runs on ('' for any)")
    ap.add_argument("--wrap", default=DEFAULT_WRAP, help="command sacdfs runs under ({quota} {cpus} {uid} {gid})")
    ap.add_argument("--sacdfs-arg", action="append", default=[], help="more sacdfs options")
    ap.add_argument("--wav-audio-start", type=int, default=268864, help="for trace replays (#456 traces)")
    ap.add_argument("--worst-ratio", type=float, default=1.25)
    ap.add_argument("--worst-ms", type=float, default=250.0)
    ap.add_argument("--stall-ratio", type=float, default=1.25)
    ap.add_argument("--stall-s", type=float, default=1.0)
    ap.add_argument("--json", help="write every run's numbers here")
    ap.add_argument("--log-dir", help="keep each run's sacdfs log, with the read trace, here")
    ap.add_argument("scenarios", nargs="+")
    a = ap.parse_args()
    # A stopped gate unmounts what it mounted: SIGTERM ends it as an exception would.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit("stall gate: stopped"))

    started = time.monotonic()
    if a.quota is None:
        a.quota, x = calibrate(a, a.base)
        print(f"calibration: base converts 6-channel WAV at {x:.2f}x real time at {a.quota}% of one CPU"
              f" (on CPUs {a.cpus}; target {a.target}x)", flush=True)
    builds = {"base": a.base, "head": a.head}
    res = {s: {"base": [], "head": []} for s in a.scenarios}
    for r in range(a.rounds):
        order = ("base", "head") if r % 2 == 0 else ("head", "base")
        for s in a.scenarios:
            for b in order:
                n = run(a, builds[b], a.quota, s, f"{name(s)}-{b}-{r + 1}")
                res[s][b].append(n)
                print(f"round {r + 1} {name(s)} {b}: worst {n['worst_ms']:.0f} ms, stalled {n['stalled_s']:.2f} s,"
                      f" sacdfs CPU {n['sacdfs_cpu_s']:.1f} s", flush=True)
    if a.json:
        with open(a.json, "w") as f:
            json.dump({"quota": a.quota, "results": res}, f)

    med = lambda runs, k: statistics.median(x[k] for x in runs)
    top = lambda runs, k: max(x[k] for x in runs)
    lines = [f"## Player stall gate ({a.rounds} rounds, sacdfs at {a.quota}% of one CPU on CPUs {a.cpus})", "",
             "| Scenario | Worst read, base → head (ms) | Stalled, base → head (s) | |", "|---|---|---|---|"]
    failed = []
    for s in a.scenarios:
        bw, hw = med(res[s]["base"], "worst_ms"), med(res[s]["head"], "worst_ms")
        bs, hs = med(res[s]["base"], "stalled_s"), med(res[s]["head"], "stalled_s")
        bwt, bst = top(res[s]["base"], "worst_ms"), top(res[s]["base"], "stalled_s")
        bad = []
        if hw > bwt * a.worst_ratio and hw > bwt + a.worst_ms:
            bad.append("worst read")
        if hs > bst * a.stall_ratio and hs > bst + a.stall_s:
            bad.append("stall")
        if bad:
            failed.append(f"{name(s)} ({', '.join(bad)})")
        rng = lambda b, k: f"{min(x[k] for x in res[s][b]):g}-{max(x[k] for x in res[s][b]):g}"
        lines.append(f"| {name(s)} | {bw:.0f} → {hw:.0f} ({rng('base', 'worst_ms')} / {rng('head', 'worst_ms')}) "
                     f"| {bs:.2f} → {hs:.2f} ({rng('base', 'stalled_s')} / {rng('head', 'stalled_s')}) "
                     f"| {'**WORSE**: ' + ', '.join(bad) if bad else 'ok'} |")
    lines += ["", f"Medians (ranges base / head) of the first {a.secs:g} s after the playing file opened; "
              f"measured in {time.monotonic() - started:.0f} s. Head fails where its median is above base's "
              f"highest by more than {a.worst_ratio}x and {a.worst_ms:g} ms (worst read) or {a.stall_ratio}x and "
              f"{a.stall_s:g} s (stall)."]
    text = "\n".join(lines) + "\n"
    print(text)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as f:
            f.write(text)
    if failed:
        sys.exit("stall gate: head is worse than base in " + "; ".join(failed))


if __name__ == "__main__":
    main()
