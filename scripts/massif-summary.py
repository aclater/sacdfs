#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Summarises a valgrind massif profile of sacdfs (#151): the heap over time, the allocation
sites still live in the last detailed snapshot, and how each site grew between the first
detailed snapshot after the warm-up and the last.

  scripts/massif-summary.py massif.out [--warmup-fraction 0.25] [--top 15]

A site is the first sacdfs or scarletbook function on the allocation's stack, counted from
the allocator (so `Vec::push` inside `Tree::read_dir` counts for `Tree::read_dir`).
"""
import argparse
import re
from collections import defaultdict

OURS = re.compile(r"(sacdfs|scarletbook)[a-z_]*::")


def parse(path):
    snaps, cur, stack = [], None, []
    for line in open(path, errors="replace"):
        line = line.rstrip("\n")
        if line.startswith("snapshot="):
            cur = {"tree": None}
            snaps.append(cur)
            stack = []
        elif cur is not None and "=" in line and not line.startswith(" ") and not line.startswith("n"):
            k, _, v = line.partition("=")
            cur[k] = v
        elif cur is not None and re.match(r"^ *n\d+: ", line):
            depth = len(line) - len(line.lstrip(" "))
            m = re.match(r"^ *n\d+: (\d+) (.*)$", line)
            node = {"bytes": int(m.group(1)), "fn": m.group(2), "kids": []}
            del stack[depth:]
            if stack:
                stack[-1]["kids"].append(node)
            else:
                cur["tree"] = node
            stack.append(node)
    return snaps


def label(fn):
    # "0x4A1B2C: sacdfs::tree::Tree::read_dir (tree.rs:123)" -> "sacdfs::tree::Tree::read_dir"
    fn = re.sub(r"^0x[0-9A-Fa-f]+: ", "", fn)
    return re.sub(r" \(.*\)$", "", fn)


def sites(tree):
    out = defaultdict(int)

    def walk(node):
        if OURS.search(node["fn"]) and not node["fn"].startswith("(heap"):
            out[label(node["fn"])] += node["bytes"]
            return
        if not node["kids"]:
            out["(outside sacdfs and scarletbook)"] += node["bytes"]
            return
        for k in node["kids"]:
            walk(k)
        rest = node["bytes"] - sum(k["bytes"] for k in node["kids"])
        if rest > 0:
            out["(below massif's threshold)"] += rest

    if tree:
        for k in tree["kids"]:
            walk(k)
    return out


def mib(b):
    return f"{b / 1048576:8.2f} MiB"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("massif_out")
    ap.add_argument("--warmup-fraction", type=float, default=0.25)
    ap.add_argument("--top", type=int, default=15)
    a = ap.parse_args()
    snaps = parse(a.massif_out)
    if not snaps:
        raise SystemExit("no snapshots")
    t_end = int(snaps[-1]["time"])
    heap = [(int(s["time"]), int(s["mem_heap_B"])) for s in snaps]
    print(f"{len(snaps)} snapshots over {t_end / 60000:.1f} min; heap min {mib(min(h for _, h in heap))}, "
          f"max {mib(max(h for _, h in heap))}, last {mib(heap[-1][1])}")
    print("heap by tenth of the run:")
    for i in range(10):
        lo, hi = t_end * i / 10, t_end * (i + 1) / 10
        part = [h for t, h in heap if lo <= t <= hi]
        if part:
            print(f"  {i * 10:3d}-{i * 10 + 10:3d}%  max {mib(max(part))}")
    detailed = [s for s in snaps if s.get("heap_tree") in ("detailed", "peak") and s["tree"]]
    if len(detailed) < 2:
        raise SystemExit("fewer than two detailed snapshots")
    after = [s for s in detailed if int(s["time"]) >= t_end * a.warmup_fraction] or detailed
    first, last = after[0], detailed[-1]
    lf, ll = sites(first["tree"]), sites(last["tree"])
    print(f"\nlive at the last detailed snapshot ({int(last['time']) / 60000:.1f} min), top sites:")
    for k, v in sorted(ll.items(), key=lambda kv: -kv[1])[: a.top]:
        print(f"  {mib(v)}  {k}")
    print(f"\ngrowth from {int(first['time']) / 60000:.1f} min to {int(last['time']) / 60000:.1f} min:")
    diff = {k: ll.get(k, 0) - lf.get(k, 0) for k in set(lf) | set(ll)}
    for k, v in sorted(diff.items(), key=lambda kv: -abs(kv[1]))[: a.top]:
        if v:
            print(f"  {'+' if v > 0 else '-'}{mib(abs(v)).strip():>12}  {k}")
    print(f"  total {int(last['mem_heap_B']) - int(first['mem_heap_B']):+d} bytes")


if __name__ == "__main__":
    main()
