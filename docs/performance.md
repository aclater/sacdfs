# Performance

Measured on 2026-10-31 with sacdfs 0.1.0 (release build, generic x86-64) and scarletbook
0.2.0, on a 16-core (32-thread) desktop with 125 GB of RAM, the image on a local NVMe disk.
All figures are means of 3 runs; each run had the machine to itself.

## Targets (from the [design](design.md))

| What | Target | Measured | |
|---|---|---|---|
| Straight read of a whole area, vs `bench decode` at the same thread count | within 1.5x | **1.22x** | met |
| First bytes of a read at a random offset, fresh mount | under 100 ms | **2.8 ms** (image in the page cache), **4.0 ms** (image evicted) | met |

## Straight read

One DST stereo disc of about 50 minutes: 14 tracks, 2.12 GB of DSF files.

- `bench decode IMAGE --area 2ch --threads 0 --sink raw DIR` (scarletbook 0.2.0's own tool,
  generic build, writing to tmpfs): 3.32, 3.35, 3.39 s; **mean 3.35 s**.
- `cat MOUNT/<disc>/Stereo/*.dsf > /dev/null` on a fresh mount at default settings (one
  decoder thread per CPU, 256 MiB frame cache): 3.83, 4.19, 4.26 s; **mean 4.09 s**, about
  520 MB/s.

Ratio: 4.09 / 3.35 = **1.22**. The image was in the page cache for both. Peak resident memory
of sacdfs during the read was 292 MiB, of which up to 256 MiB is the frame cache.

## Seek

A fresh mount each time (empty frame index and frame cache), then one 128 KiB `dd` read
inside a track, at 37 %, 81 % and 55 % of three different tracks. The time is the whole `dd`,
which includes opening the file:

- image in the page cache: 2.90, 2.91, 2.63 ms; **mean 2.8 ms**;
- image evicted from the page cache first (`posix_fadvise(POSIX_FADV_DONTNEED)`): 3.65,
  3.80, 4.47 ms; **mean 4.0 ms**.

The locator needs about 20 probes of a few sectors each on a disc this size; on a local SSD
they cost microseconds each. On a slow network share each probe is a round trip, so the
first read after a seek could approach the 100 ms target there; the index learned from
earlier reads makes later seeks on the same disc cheaper.

## Correctness on real discs

The same per-channel SHA-256 check as the corpus tests, run locally over the `Stereo` and
`Multichannel` directories of five real discs mounted from one directory (plain DSD stereo;
DST stereo and 6 channels; DSD stereo with DST 5 channels; DST stereo in 2064-byte raw
sectors; another 2064-byte raw image), against reference hashes made from `pysacd-extract`
output: 93 of 93 tracks in 7 areas identical.

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
