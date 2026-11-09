#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Reads an RF64 file with code that shares nothing with sacdfs (#220).

    python3 -I scripts/check_rf64.py FILE CHANNELS RATE SAMPLES

Walks the chunks of FILE as EBU TR 3306 describes them, seeking over the data chunk (so a sparse
multi-GB file costs nothing), and exits 1 with a message unless: the file is RF64 with a ds64
chunk; the ds64 sizes agree with the file's real size, with each other and with the data
chunk's position; fmt says CHANNELS and RATE; the data holds SAMPLES per channel; and the chunks
after the data end exactly at the end of the file.
"""
import struct
import sys

def fail(msg):
    print(f"check_rf64: {msg}", file=sys.stderr)
    sys.exit(1)

def main():
    path, channels, rate, samples = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4])
    with open(path, "rb") as f:
        f.seek(0, 2)
        size = f.tell()
        f.seek(0)
        head = f.read(12)
        if head[:4] != b"RF64" or head[8:] != b"WAVE":
            fail(f"not RF64/WAVE: {head!r}")
        if struct.unpack("<I", head[4:8])[0] != 0xFFFFFFFF:
            fail("RIFF size field is not 0xFFFFFFFF")
        ds64 = fmt = None
        data_at = None
        pos = 12
        while pos < size:
            f.seek(pos)
            hdr = f.read(8)
            if len(hdr) < 8:
                fail(f"truncated chunk header at {pos}")
            cid, clen = hdr[:4], struct.unpack("<I", hdr[4:])[0]
            body = pos + 8
            if cid == b"ds64":
                if clen < 28:
                    fail("ds64 chunk too short")
                ds64 = struct.unpack("<QQQI", f.read(28))
            elif cid == b"fmt ":
                fmt = f.read(clen)
            elif cid == b"data":
                if ds64 is None or fmt is None:
                    fail("data before ds64 or fmt")
                if clen != 0xFFFFFFFF:
                    fail(f"data size field is {clen:#x}, expected 0xFFFFFFFF in RF64")
                data_at, clen = body, ds64[1]
            if body + clen > size:
                fail(f"chunk {cid!r} at {pos} runs past the end of the file")
            pos = body + clen + (clen & 1)
        if pos not in (size, size + 1):
            fail(f"chunks end at {pos}, file is {size} bytes")
        if ds64 is None or fmt is None or data_at is None:
            fail("missing ds64, fmt or data chunk")
        riff, data, nsamples, _ = ds64
        if riff + 8 != size:
            fail(f"ds64 riff size {riff} + 8 != file size {size}")
        tag, ch, sr, _, align, bits = struct.unpack("<HHIIHH", fmt[:16])
        if (ch, sr) != (channels, rate):
            fail(f"fmt says {ch} channels at {sr} Hz, expected {channels} at {rate}")
        if data % align or data // align != samples or nsamples != samples:
            fail(f"data holds {data // align} samples (ds64 sample count {nsamples}), expected {samples}")
    print(f"check_rf64: ok: {size} bytes, {ch} ch, {sr} Hz, {samples} samples, tag {tag:#x}, {bits} bits")

main()
