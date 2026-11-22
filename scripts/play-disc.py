#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""Writes a synthetic disc image for player replays (#506): one 6-channel DST area of 12
tracks, the first long enough to play for a while, the others long enough for their heads and
tails to be re-read. Its frames are the 35 DST frames of the conformance corpus's 6-channel
area in tests/corpus/images/dst-features-2ch-6ch.iso (CC0, generated test signals), in turn, so
decoding it costs about what a DST disc does. With --dsd the area is plain DSD of seeded noise
instead, which costs only the PCM conversion.

  scripts/play-disc.py OUT.iso [--first 60] [--others 10] [--tracks 12] [--dsd]

The layout follows the image builder of aclater/pysacd's tests (tests/sacdgen.py, same
author, GPL-2.0-or-later): master TOC at sector 510 (copies at 520 and 530), then the area's
TOC-1, its audio sectors and its TOC-2, one packet per frame per sector.
"""
import argparse
import os
import random
import struct

S = 2048
MASTER_TOC = 510
FIRST_AREA = 540
FRAME_RATE = 75
AREA_TOC_SECTORS = 6  # header, SACDTTxt, SACD_IGL x2, SACDTRL1, SACDTRL2
FRAME_BYTES = 4704  # per channel


def tc(frames):
    return bytes([frames // (60 * FRAME_RATE), frames // FRAME_RATE % 60, frames % FRAME_RATE])


def corpus_frames(path):
    """The audio frames of the 6-channel area of a corpus image."""
    with open(path, "rb") as f:
        img = f.read()
    toc = struct.unpack_from(">I", img, MASTER_TOC * S + 72)[0]
    first, last = struct.unpack_from(">II", img, toc * S + 72)
    frames, cur = [], None
    for lsn in range(first, last + 1):
        sec = img[lsn * S:(lsn + 1) * S]
        npk, nfi, dst = sec[0] >> 5, (sec[0] >> 2) & 7, sec[0] & 1
        pk = [(sec[1 + 2 * i] >> 7, (sec[1 + 2 * i] >> 3) & 7, (sec[1 + 2 * i] & 7) << 8 | sec[2 + 2 * i])
              for i in range(npk)]
        pos = 1 + 2 * npk + nfi * (4 if dst else 3)
        for st, t, n in pk:
            if t == 2:
                if st:
                    if cur is not None:
                        frames.append(bytes(cur))
                    cur = bytearray()
                cur += sec[pos:pos + n]
            pos += n
    frames.append(bytes(cur))
    return frames


def mux(frame_count, frame, dst):
    """Packs `frame_count` frames (`frame(i)`) into audio sectors; returns the sectors and, per
    frame, the index of the sector it starts in."""
    layout, first = [], []
    fi, off = 0, 0
    while fi < frame_count:
        packets, space, starts = [], S - 1, 0
        flen = len(frame(fi))
        while fi < frame_count and len(packets) < 7:
            start = off == 0
            info_len = (4 if dst else 3) if start else 0
            if start and starts == 7:
                break
            n = min(2047, space - 2 - info_len, flen - off)
            if n <= 0:
                break
            if start:
                first.append(len(layout))
                starts += 1
            packets.append((fi, start, off, n))
            space -= 2 + info_len + n
            off += n
            if off == flen:
                fi, off = fi + 1, 0
                flen = len(frame(fi)) if fi < frame_count else 0
        layout.append((packets, space))
    nsec = [0] * frame_count
    for packets, _ in layout:
        for f, _, _, _ in packets:
            nsec[f] += 1
    sectors = []
    for packets, space in layout:
        pk = [(st, 2, n) for _, st, _, n in packets]
        infos = [tc(f) + (bytes([nsec[f] << 2]) if dst else b"") for f, st, _, _ in packets if st]
        payload = b"".join(frame(f)[o:o + n] for f, _, o, n in packets)
        if space > 2 and len(pk) < 7:
            pk.append((0, 7, space - 2))  # a padding packet fills the sector
            payload += bytes(space - 2)
        hdr = bytes([len(pk) << 5 | len(infos) << 2 | (1 if dst else 0)])
        sec = hdr + b"".join(bytes([st << 7 | t << 3 | n >> 8, n & 0xFF]) for st, t, n in pk)
        sec += b"".join(infos) + payload
        assert len(sec) <= S
        sectors.append(sec.ljust(S, b"\0"))
    return sectors, first


def master_toc(toc1, toc2):
    m = bytearray(10 * S)
    m[0:8] = b"SACDMTOC"
    m[8:10] = bytes([1, 20])
    struct.pack_into(">HH", m, 16, 1, 1)
    m[24:40] = b"SACDFS-PLAY".ljust(16)
    m[40:44] = bytes([1, 0, 0, 25])
    struct.pack_into(">IIII", m, 64, 0, 0, toc1, toc2)
    struct.pack_into(">HH", m, 84, 0, AREA_TOC_SECTORS)
    m[88:104] = b"SACDFS-PLAY-1".ljust(16)
    m[104:108] = bytes([1, 0, 0, 25])
    struct.pack_into(">HBB", m, 120, 2026, 11, 20)
    m[128] = 1
    m[136:140] = b"en" + bytes([2, 0])
    for i in range(1, 9):
        m[i * S:i * S + 8] = b"SACDText"
    pos = 64
    for i, s in enumerate((b"Player replay disc", b"sacdfs synthetic")):
        m[S + pos:S + pos + len(s)] = s
        struct.pack_into(">H", m, S + 16 + i * 2, pos)
        struct.pack_into(">H", m, S + 16 + (8 + i) * 2, pos)
        pos += (len(s) + 1 + 3) & ~3
    m[9 * S:9 * S + 8] = b"SACD_Man"
    return bytes(m)


def area_toc(channels, tracks, track_lsns, audio, dst):
    a = bytearray(AREA_TOC_SECTORS * S)
    a[0:8] = b"MULCHTOC"
    a[8:10] = bytes([1, 20])
    struct.pack_into(">H", a, 10, AREA_TOC_SECTORS)
    struct.pack_into(">I", a, 16, 2822400 * channels // 8)
    a[20] = 4  # 64 x 44.1 kHz
    a[21] = 0 if dst else 2  # DST, or plain DSD 3-in-14
    a[32] = channels
    a[33] = 4  # 5.1
    a[34] = channels
    a[64:67] = tc(sum(tracks))
    a[69] = len(tracks)
    struct.pack_into(">II", a, 72, *audio)
    a[80] = 1
    a[88:92] = b"en" + bytes([2, 0])
    tt, pos = S, 8 + 2 * 255 + 2
    a[tt:tt + 8] = b"SACDTTxt"
    for i in range(len(tracks)):
        item = bytes([1, 0, 0, 0, 0x01, 0x20]) + f"Track {i + 1}".encode() + b"\0"
        struct.pack_into(">H", a, tt + 8 + i * 2, pos)
        a[tt + pos:tt + pos + len(item)] = item
        pos += (len(item) + 3) & ~3
    ig = 2 * S
    a[ig:ig + 8] = b"SACD_IGL"
    l1 = 4 * S
    a[l1:l1 + 8] = b"SACDTRL1"
    for i, (start, length) in enumerate(track_lsns):
        struct.pack_into(">I", a, l1 + 8 + i * 4, start)
        struct.pack_into(">I", a, l1 + 8 + 255 * 4 + i * 4, length)
    l2, t = 5 * S, 0
    a[l2:l2 + 8] = b"SACDTRL2"
    for i, n in enumerate(tracks):
        a[l2 + 8 + i * 4:l2 + 11 + i * 4] = tc(t)
        a[l2 + 8 + 255 * 4 + i * 4:l2 + 11 + 255 * 4 + i * 4] = tc(n)
        t += n
    return bytes(a)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out")
    ap.add_argument("--first", type=int, default=60, help="seconds of track 1")
    ap.add_argument("--others", type=int, default=10, help="seconds of each other track")
    ap.add_argument("--tracks", type=int, default=12)
    ap.add_argument("--dsd", action="store_true", help="plain DSD noise instead of the corpus's DST frames")
    ap.add_argument("--corpus", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tests",
                                                     "corpus", "images", "dst-features-2ch-6ch.iso"))
    a = ap.parse_args()
    channels = 6
    tracks = [a.first * FRAME_RATE] + [a.others * FRAME_RATE] * (a.tracks - 1)
    if a.dsd:
        rng = random.Random(506)
        pool = [rng.randbytes(channels * FRAME_BYTES) for _ in range(61)]
    else:
        pool = corpus_frames(a.corpus)
    sectors, first = mux(sum(tracks), lambda i: pool[i % len(pool)], not a.dsd)
    toc1 = FIRST_AREA
    audio_start = toc1 + AREA_TOC_SECTORS
    audio_end = audio_start + len(sectors) - 1
    toc2 = audio_end + 1
    starts, f = [], 0
    for n in tracks:
        starts.append(audio_start + first[f])
        f += n
    lsns = [(s, (starts[i + 1] if i + 1 < len(starts) else audio_end + 1) - s) for i, s in enumerate(starts)]
    toc = area_toc(channels, tracks, lsns, (audio_start, audio_end), not a.dsd)
    master = master_toc(toc1, toc2)
    with open(a.out, "wb") as o:
        o.write(bytes(MASTER_TOC * S))
        o.writelines(master for _ in range(3))
        o.write(toc)
        o.writelines(sectors)
        o.write(toc)


if __name__ == "__main__":
    main()
