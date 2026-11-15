#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Compare a Windows mount's files with the Linux mount's (#419, spike).

    compare.py LINUX_SHA256 MOUNT

LINUX_SHA256 is `sha256sum` output of every file of a Linux mount of the same source, with
paths relative to the mount. Each Linux name is mapped as the Windows front end maps it (the
SFM private-use characters, bare reserved names), then every file of MOUNT is hashed and both
lists must hold the same names with the same bytes. Exits 1 on any difference.
"""
import hashlib
import os
import sys

SFM = {'"': "", "*": "", ":": "", "<": "", ">": "", "?": "",
       "\\": "", "|": ""}
RESERVED = {"CON", "PRN", "AUX", "NUL"} | {f"COM{i}" for i in range(1, 10)} | {f"LPT{i}" for i in range(1, 10)}


def windows(name):
    out = "".join(chr(0xF000 + ord(c)) if 1 <= ord(c) <= 0x1F else SFM.get(c, c) for c in name)
    if out.endswith(" "):
        out = out[:-1] + ""
    elif out.endswith("."):
        out = out[:-1] + ""
    if out.upper() in RESERVED:
        out += "_"
    return out


def main():
    ref_file, mount = sys.argv[1], sys.argv[2]
    ref = {}
    with open(ref_file, encoding="utf-8") as f:
        for line in f:
            digest, path = line.rstrip("\n").split("  ", 1)
            path = path.removeprefix("./")
            ref["/".join(windows(c) for c in path.split("/"))] = digest
    got = {}
    for root, _, files in os.walk(mount):
        for name in files:
            full = os.path.join(root, name)
            h = hashlib.sha256()
            with open(full, "rb") as f:
                while chunk := f.read(1 << 20):
                    h.update(chunk)
            got[os.path.relpath(full, mount).replace(os.sep, "/")] = h.hexdigest()
    missing = sorted(set(ref) - set(got))
    extra = sorted(set(got) - set(ref))
    differ = sorted(p for p in ref if p in got and got[p] != ref[p])
    same = len(ref) - len(missing) - len(differ)
    print(f"Linux {len(ref)} files, Windows {len(got)}: {same} byte-identical")
    for label, items in (("missing on Windows", missing), ("only on Windows", extra), ("differ", differ)):
        for p in items[:20]:
            print(f"  {label}: {p}")
    return 0 if not (missing or extra or differ) and ref else 1


if __name__ == "__main__":
    sys.exit(main())
