# Performance

Measured on 2026-10-31 with sacdfs 0.1.0 (release build, generic x86-64) and scarletbook
0.3.0, on a 16-core (32-thread) desktop with 125 GB of RAM, the image on a local NVMe disk.
All figures are means of 3 runs, taken under the benchmark lock.

**Machine load.** Unlike the earlier figures (below), these were taken while a virtual
machine on the same host kept about 14 to 15 of the 32 hardware threads busy throughout
(measured from its CPU time during each run). The machine never went idle long enough
for an unloaded run. Absolute times are therefore about 1.8 to 2.2 times those of an idle
machine, and the ratios are less favourable to sacdfs, whose decoder threads compete with
the guest's.

## Targets (from the [design](design.md))

| What | Target | Measured | |
|---|---|---|---|
| Straight read of a whole area, vs `bench decode` at the same thread count | within 1.5x | **1.43x** (machine loaded) | met |
| First bytes of a read at a random offset, fresh mount | under 100 ms | **3.4 ms** (image in the page cache), **3.7 ms** (image evicted) | met |

## Straight read

One DST stereo disc of about 50 minutes: 14 tracks, 2.12 GB of DSF files. Each round ran
`bench decode`, then sacdfs 0.1 as it was before this update (scarletbook 0.2.0), then the
current build, one after the other.

- `bench decode IMAGE --area 2ch --threads 0 --sink raw DIR` (scarletbook 0.3.0's own
  tool, generic build `bench-0.3.0-x86_64-linux`, writing to tmpfs): 6.18, 6.27, 6.06 s;
  **mean 6.17 s**.
- `cat MOUNT/<disc>/Stereo/*.dsf > /dev/null` on a fresh mount at default settings (one
  decoder thread per CPU, 256 MiB frame cache):
  - current: 9.30, 8.66, 8.52 s; **mean 8.82 s**, about 240 MB/s; ratio **1.43**;
  - before (scarletbook 0.2.0): 8.80, 8.51, 9.02 s; mean 8.77 s; ratio 1.42.

The image was in the page cache throughout. Peak resident memory of sacdfs during the read
was about 305 MiB, of which up to 256 MiB is the frame cache. The two sacdfs builds are equal
within the noise: sacdfs decodes one DST frame per job with `DstDecoder`, which is the same
in both scarletbook versions for a single frame. Grouping frames for scarletbook 0.3.0's
lockstep `DstDecoder::decode_many` was also measured. In a separate loaded run of 3 rounds,
the means were 7.93 s with lockstep always on, 7.86 s with it only when there are frames
enough for every thread, and 7.99 s without it, against 5.83 s for `bench decode`. That is
no measurable gain, so it was not kept (see the [design](design.md#reading-a-range-of-a-dsf-file)).

## Seek

A fresh mount each time (empty frame index and frame cache), then one 128 KiB `dd` read
inside a track, at 37 %, 81 % and 55 % of three different tracks. The time is the whole `dd`,
which includes opening the file:

- image in the page cache: 3.57, 3.59, 3.05 ms; **mean 3.4 ms** (before: 2.4 ms);
- image evicted from the page cache first (`posix_fadvise(POSIX_FADV_DONTNEED)`): 3.48,
  3.71, 3.93 ms; **mean 3.7 ms** (before: 3.4 ms).

From a local disk these times are mostly FUSE, opening the file and decoding, and they vary
by about 1 ms from run to run on the loaded machine. What the search changes is the number
of reads it makes, which is what counts on a network share, where each read is a round trip.
Calls to the image per cold seek (`Disc::sector_reads`, including reading the 14 frames
found), mean of 200 seeks spread evenly over each area of each disc, a fresh disc each time:

| Disc | 0.1 search | + TOC track starts as hints | + aim past the estimate (current) |
|---|---|---|---|
| plain DSD stereo | 10.74 | 10.86 | **4.58** |
| DST stereo and 6 channels | 9.36 | 7.80 | **6.11** |
| DSD stereo with DST 5 channels | 9.39 | 9.12 | **5.32** |
| DST stereo, 2064-byte sectors | 8.21 | 7.12 | **5.47** |
| plain DSD stereo, 2064-byte sectors | 7.03 | 6.79 | **5.17** |

(The "0.1 search" column already includes the fix of starting before the target frame,
which costs at most one frame's sectors.)

For comparison, scarletbook 0.3.0's frame-range cursor, which scans from the area start, took
21 to 35 ms, 45 to 78 ms and 90 to 132 ms for the first 14 frames at 16 %, 42 % and 72 % of
the same disc's stereo area (page cache, in one process, two sets of 3 runs). The locator
took 0.9 to 1.8 ms.

## Correctness on real discs

The same per-channel SHA-256 check as the corpus tests, run locally over the `Stereo` and
`Multichannel` directories of five real discs mounted from one directory (plain DSD stereo;
DST stereo and 6 channels; DSD stereo with DST 5 channels; DST stereo in 2064-byte raw
sectors; another 2064-byte raw image), against reference hashes made from `pysacd-extract`
output: 93 of 93 tracks in 7 areas identical, with no warnings logged.

## Earlier figures (scarletbook 0.2.0, idle machine)

On an idle machine, with scarletbook 0.2.0: `bench decode` 3.35 s, `cat` 4.09 s (ratio
1.22), seek 2.8 ms (page cache) and 4.0 ms (evicted).

## Reproducing

```sh
# whole-area read
bench decode IMAGE --area 2ch --threads 0 --sink raw /dev/shm/out
sacdfs -f IMAGE /mnt/x &
time cat /mnt/x/*/Stereo/*.dsf > /dev/null
# seek
fusermount3 -u /mnt/x; sacdfs -f IMAGE /mnt/x &
time dd if=/mnt/x/<disc>/Stereo/05*.dsf of=/dev/null bs=128K count=1 skip=$OFFSET iflag=skip_bytes
```
