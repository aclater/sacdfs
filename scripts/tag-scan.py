#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Reads each file's header and tags as a library scanner's tag reader does, and counts it (#266).

  scripts/tag-scan.py LIST

LIST holds one path per line (DSF and WAV files, for example on a client's SMB mount). For
each: the first 64 KiB, then the tag block. DSF: the ID3 block that the header's metadata
pointer gives (up to 64 KiB). WAV: the RIFF chunks walked by their 8-byte headers, and every
chunk but the audio read (up to 64 KiB each). Unbuffered `pread`s, so what reaches the server
beyond them is the client's own read-ahead. Prints the files, those without tags, the bytes
asked for and the wall time. Compare "asked" with the client's counters (Linux SMB:
`Bytes read` in /proc/fs/cifs/Stats) and the server's CPU.
"""
import os
import struct
import sys
import time

CHUNK = 65536


def scan(path: str) -> tuple[int, bool]:
    asked, tagged = 0, False
    fd = os.open(path, os.O_RDONLY)
    try:
        size = os.fstat(fd).st_size
        head = os.pread(fd, CHUNK, 0)
        asked += CHUNK
        if head[:4] == b"DSD ":
            meta = struct.unpack_from("<Q", head, 20)[0]
            if 0 < meta < size:
                n = min(size - meta, CHUNK)
                tagged = os.pread(fd, n, meta)[:3] == b"ID3"
                asked += n
        elif head[:4] in (b"RIFF", b"RF64"):
            off = 12
            while off + 8 <= size:
                hdr = os.pread(fd, 8, off)
                asked += 8
                cid, length = hdr[:4], struct.unpack_from("<I", hdr, 4)[0]
                if cid == b"data" and head[:4] == b"RF64":
                    break  # the real size is in ds64; the tags of an RF64 file are not walked
                if cid != b"data":
                    os.pread(fd, min(length, CHUNK), off + 8)
                    asked += min(length, CHUNK)
                    tagged |= cid in (b"LIST", b"id3 ", b"ID3 ")
                off += 8 + length + (length & 1)
    finally:
        os.close(fd)
    return asked, tagged


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    with open(sys.argv[1]) as f:
        paths = [line.rstrip("\n") for line in f if line.strip()]
    start, asked, untagged = time.monotonic(), 0, 0
    for path in paths:
        n, tagged = scan(path)
        asked += n
        untagged += not tagged
    print(f"scan files={len(paths)} untagged={untagged} asked={asked} wall={time.monotonic() - start:.2f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
