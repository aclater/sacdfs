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
| DSD-to-PCM conversion, stereo, 176.4 kHz, default filter, one core (#6; no WAV files yet) | at least 100x real time | **209x** generic x86-64, **264x** x86-64-v3 ([below](#pcm-conversion)) | met |

## Client-side seek latency (issue #20)

Measured on 2026-10-31 on the same machine, under the benchmark lock, with the virtual
machine on the host nearly idle this time (0.4 and 0.1 of a hardware thread busy during the
two runs, from its CPU time). "Before" is sacdfs as on `main` before this change; "after" is
the persistent seek index and the smaller cold reads.

**Method.** Every seek is on a fresh mount: open the file, read 128 KiB at an offset inside a
track with one `pread`, close; the time of those three calls (unlike the `dd` figures below,
it leaves out starting a process, about 1 ms). *Cold:* an empty seek index (for "before",
always). *Warm:* a remount whose seek index cache was saved by a previous mount that made
the same seek ("resume where I stopped"). Each figure is the mean of 3 rounds, each the mean
of 3 seeks: locally, tracks 5, 9 and 12 at 37 %, 81 % and 55 % of their length; on NFS, three
images, one seek each, at a different position in each round.

**Images over NFS.** The share is NFS 4.2 from a NAS on gigabit Ethernet (ping 0.38 ms), its
pool on hard disks. The client mounts it with FS-Cache, which keeps data read once on a local
disk, so dropping the page cache does not make reads cold. For these runs sacdfs was started
with a small `LD_PRELOAD` shim (not part of sacdfs) that adds `O_DIRECT` to opens on the
share: every read of the image then goes to the server, bypassing both the page cache and
FS-Cache, which the per-mount NFS READ counts in `/proc/self/mountstats` confirmed (each
mount also makes 6 to 8 READs for the TOCs). *First touch:* a part of the image not read
before in these runs, so the NAS most likely reads it from disk; *repeat:* the same seeks
again, with the NAS's cache warm.

| Time to the first 128 KiB, ms | before | after, cold | after, warm |
|---|---|---|---|
| local, DST stereo disc, page cache | 1.60 | 1.41 | 1.40 |
| local, DST stereo disc, evicted | 1.97 | 1.78 | 1.47 |
| local, plain DSD disc, page cache | 1.57 | 0.51 | 0.40 |
| local, plain DSD disc, evicted | 1.36 | 1.32 | 0.81 |
| NFS, plain DSD, first touch | 27.8 | 18.6 | 2.7 * |
| NFS, plain DSD, repeat | 5.18 | 3.57 | 2.76 |
| NFS, DST, first touch | 36.9 | 26.8 | 2.7 * |
| NFS, DST, repeat | 5.94 | 4.53 | 2.72 |

\* The warm seek follows the cold one at the same position, so the NAS has those sectors
cached; a warm seek into a part of the image the NAS must read from disk costs its one read's
disk seek (a single 170 KiB read there took about 25 ms).

Calls to the image per seek (from the exit log of `-v`, and on NFS from the READ counts less
the TOC reads): locally 4.0 cold for the DST disc and 3.0 for the plain DSD one; on NFS 5 →
3 (plain DSD) and 6.0 → 5.7 (DST) cold, and 1 warm. Locally the times are mostly FUSE,
opening the file and decoding; on NFS each call is a round trip, and on first touch usually
a disk seek on the NAS too, which is why single first-touch seeks vary from 9 to 37 ms
after (19 to 49 ms before). A warm seek
on NFS is the open of the image (0.6 to 0.9 ms over NFS) plus one read (about 1.4 ms for
170 KiB).

**Calls and bytes per cold seek**, without FUSE (`examples/seek_latency`): 200 seeks of
128 KiB spread over each area of five real discs, each on a fresh disc (an empty index):

| Disc | calls before | calls after | KiB before | KiB after |
|---|---|---|---|---|
| plain DSD stereo | 4.78 | **3.04** | 345 | **193** |
| DST stereo | 5.36 | **5.04** | 268 | **106** |
| DST 6 channels (same disc) | 5.67 | **4.90** | 277 | **159** |
| DSD stereo (hybrid disc) | 4.67 | **3.00** | 342 | **192** |
| DST 5 channels (same disc) | 5.92 | **5.58** | 285 | **118** |
| DST stereo, 2064-byte sectors | 5.46 | **5.11** | 271 | **104** |
| DST stereo, 2064-byte sectors (another disc) | 5.14 | **4.84** | 261 | **101** |
| **all** | 5.29 | **4.50** | 293 | **139** |
| first 128 KiB of every track (93 tracks) | 3.71 | **2.90** | 246 | **119** |
| warm: the same seek after a remount | | **1.06** | | **109** |
| warm: a seek into a track played through before a remount | | **1.05** | | **108** |

The bytes halve because probes and the first read are sized to the area's frames; the calls
drop most on discs with large frames (plain DSD), where the search now stops at about two
frames and keeps aiming after a probe that lands next to its frame. On DST discs, whose frame
sizes vary with the music, interpolation from the track starts is less accurate, and a cold
seek still needs four to five probes. The seek index cache of these five discs, after every
track of each was played and 1 400 cold seeks, took about 130 KB.

### Against the targets of issue #6

| Deployment | Target | Measured | |
|---|---|---|---|
| Local images (page cache) | < 20 ms | 0.4 to 1.6 ms | met |
| Client-side, images on NFS, cold | < 50 ms | 18.6 / 26.8 ms (plain DSD / DST, first touch, mean; worst single seek 37 ms); 3.6 / 4.5 ms with the NAS's cache warm | met |
| Client-side, warm seek index | within 2x local + one round trip | 2.7 ms against 2 x 1.4 + 0.4 = 3.2 ms (DST); against 2 x 0.5 + 0.4 = 1.4 ms for plain DSD it is not met, because opening the image over NFS (0.6 to 0.9 ms) and moving 170 KiB (1.4 ms) take longer than the local seek itself | met for DST |
| Client-side, images on SMB, cold | < 80 ms | not measured (see below) | |

**SMB** was not measured: mounting an SMB share needs root (`mount.cifs`), and there is no
user-space SMB client in the build image. Each call to the image is one SMB read request, so a
cold seek costs about its 3 to 5.7 calls times the share's per-request latency, plus the
transfer, and a warm one a single request.

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

## PCM conversion

The DSD-to-PCM converter alone (phase 1 of #11; no WAV files yet), measured 2026-10-31 with
`cargo bench --bench pcm` (criterion, one frame at a time as the filesystem will call it,
with both neighbours, noise-like DSD; 2 s warm-up, 5 s measurement per case), three rounds
under the benchmark lock on the same 16-core machine, one thread. Two builds: generic
x86-64 (SSE2, what `cargo build` gives on most distributions) and x86-64-v3 (AVX2, RHEL 10's
default target and `-C target-cpu=x86-64-v3`). The output is bit-identical between them.

**Machine load.** The virtual machine on the host kept 0.2, 3.5 and 0.2 hardware threads
busy during the three generic rounds and 1.5, 0.5 and 1.0 during the v3 rounds (from its
CPU time, `/proc/<pid>/stat` fields 14 and 15). One round of each build ran about 40 %
slower than the other two; the means below include it, so they are pessimistic. The best
round is given too.

**The bar (#6): at least 100 times real time per stereo stream per core at 176.4 kHz with
the default filter, that is at most 10 ms of CPU per second of audio.** Met: 4.8 ms (209×)
generic, 3.8 ms (264×) on x86-64-v3. For comparison, scarletbook 0.3.0 decodes stereo DST
at about 78× per core, so on a DST disc conversion adds less than half the decoding cost;
on a plain DSD disc it is the only CPU cost.

Per frame (1/75 s), mean of 3 rounds (best round in brackets); "× real time" is per stream,
and for 6 channels also per stereo pair (6 channels count as three stereo streams):

| Channels | Filter | Rate | Generic µs/frame | Generic × real time | v3 µs/frame | v3 × real time |
|---|---|---|---|---|---|---|
| 2 | 40 kHz | 88.2 kHz | 51.5 (43.6) | 259× | 47.5 (36.3) | 281× |
| 2 | 40 kHz | **176.4 kHz** | **63.9 (54.4)** | **209×** | **50.6 (43.7)** | **264×** |
| 2 | 40 kHz | 352.8 kHz | 77.0 (65.8) | 173× | 60.3 (52.3) | 221× |
| 2 | 30 kHz | 88.2 kHz | 75.0 (64.8) | 178× | 58.8 (51.6) | 227× |
| 2 | 30 kHz | 176.4 kHz | 108.5 (94.6) | 123× | 75.8 (69.3) | 176× |
| 2 | 30 kHz | 352.8 kHz | 123.0 (107.5) | 108× | 87.6 (78.1) | 152× |
| 2 | wide 80 kHz | 176.4 kHz | 141.8 (124.6) | 94× | 100.2 (90.8) | 133× |
| 2 | wide 80 kHz | 352.8 kHz | 131.6 (118.3) | 101× | 94.9 (86.7) | 141× |
| 6 | 40 kHz | 88.2 kHz | 155.6 (131.3) | 86×, 257× per pair | 123.3 (108.3) | 108×, 324× per pair |
| 6 | 40 kHz | 176.4 kHz | 191.1 (163.2) | 70×, 209× per pair | 147.5 (130.2) | 90×, 271× per pair |
| 6 | 40 kHz | 352.8 kHz | 233.7 (196.9) | 57×, 171× per pair | 176.4 (156.9) | 76×, 227× per pair |
| 6 | 30 kHz | 88.2 kHz | 223.6 (192.2) | 60×, 179× per pair | 156.1 (155.5) | 85×, 256× per pair |
| 6 | 30 kHz | 176.4 kHz | 318.1 (282.7) | 42×, 126× per pair | 232.1 (207.8) | 57×, 172× per pair |
| 6 | 30 kHz | 352.8 kHz | 364.1 (320.2) | 37×, 110× per pair | 267.9 (235.7) | 50×, 149× per pair |
| 6 | wide 80 kHz | 176.4 kHz | 479.8 (372.1) | 28×, 83× per pair | 308.2 (273.0) | 43×, 130× per pair |
| 6 | wide 80 kHz | 352.8 kHz | 422.8 (348.1) | 32×, 95× per pair | 291.9 (260.8) | 46×, 137× per pair |

Frames per second are 10⁶ / (µs per frame); a stream plays 75 frames per second.

- The default costs about the same at every rate: stage 1 and 2 are the same for all
  rates; 352.8 kHz adds the ×2 interpolation (about 20 %), and 88.2 kHz saves half of
  stage 3.
- The 30 kHz filter is not a half-band, so its stage 3 costs about 2.5 times the default's.
- The wide filter at 176.4 kHz is the costliest (its 337-tap stage, required to be fully
  attenuated by 88.2 kHz): 2.2 times the default. It is at the bar on a generic build
  (94× mean, 107× in the best round) and clears it on x86-64-v3 (133×).
- The 6-channel figures scale with the channel count: channels are converted one after the
  other with the same kernels.
- Making a converter (designing its filters) takes 38 to 55 µs generic, 27 to 42 µs v3; it
  is done once per worker thread.
- Where the time goes (perf profile of the default at 176.4 kHz, generic build): stage 3
  about 40 %, stage 1 (the byte tables) about 30 %, stage 2 about 10 %, copying the
  channel's window out of the interleaved frames and writing the 24-bit samples about 7 %
  each. Each frame also converts its
  neighbours' margins (up to about 270 bytes each side), about 8 % more than streaming.

Reproduce: `cargo bench --bench pcm`, and for the generic build
`RUSTFLAGS="-C target-cpu=x86-64" cargo bench --bench pcm`. CI runs every case once
(`-- --test`) as a smoke test.

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
# calls, bytes and time per seek without FUSE, cold and with a warm seek index
cargo run --release --example seek_latency -- --seeks 200 --cache /tmp/idx IMAGE...
```
