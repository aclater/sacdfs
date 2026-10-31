# Performance

How fast sacdfs is, against the targets of [#6](https://github.com/aclater/sacdfs/issues/6),
in each of the three ways it is deployed: images on a local disk, sacdfs in a container on
the NAS that holds the images (server-side), and sacdfs on a Linux client reading images
from a network share (client-side). The design behind these figures is in
[design.md](design.md).

## How these figures were taken

**One round, on the release code** (#37): `main` at 49f114c, the code for 0.1.0 (scarletbook
0.5.0), measured on the night of 2026-10-31 to 11-01.

- **The machine:** a 16-core (32-thread) desktop with a Zen 5 CPU and 125 GB of RAM, the images
  on a local NVMe disk unless noted.
- **Two builds**, as released: generic x86-64 (`-C target-cpu=x86-64`) and x86-64-v3. Figures
  are for the generic build unless a column says otherwise.
- **Means of 3** runs, each on a fresh mount with `--no-index-cache` unless noted, under a
  benchmark lock so that no other benchmark ran at the same time.
- **A quiet machine:** each part waited until the load average was under 2 (and under 4 at its
  start, by the lock). A virtual machine that sometimes shares the host was shut off for the
  whole round; its load was recorded anyway (matching its process exactly, from
  `/proc/<pid>/stat` fields 14 and 15) and was 0 throughout. A watcher looked for compilers and
  fuzzers every 2 seconds during each part; none ran. A part disturbed by either would have
  been run again.
- **The discs:** five real discs: plain DSD stereo; DST stereo and 6 channels; DSD stereo with
  DST 5 channels (a hybrid disc); and two DST stereo images in 2064-byte raw sectors. Seven
  areas, 93 tracks. "Plain DSD" is a disc stored without DST compression, which sacdfs only
  demultiplexes; DST areas are decoded too. Both builds passed the DSF gate (all 93 tracks
  bit-identical to the reference extractor) before the round.
- **Time per seek** is `open`, one 128 KiB `pread` and `close`, timed inside one process.

The figures of earlier pull requests, each against its own "before", are in the
[history](#history).

## Local images

The images on a local disk, in the page cache unless noted; the targets of #6.

| What | Target | Generic build | x86-64-v3 build | |
|---|---|---|---|---|
| Straight read of an area's DSF files, against scarletbook 0.5.0's `bench decode` of the same area, same build, all threads | within 1.5x | DST stereo **1.36x**, plain DSD **1.37x**, DST 6 ch. **1.50x** | 1.50x, 1.42x, 1.23x | met |
| Straight read of an area's WAV files, against its DSF files | within 1.5x | DST stereo **1.07x**, plain DSD **1.23x**, DST 6 ch. **1.17x** | 1.09x, 1.35x, 1.21x | met |
| Cold seek, DSF, fresh mount, empty seek index, page cache | < 20 ms | DST stereo **2.09 ms**, plain DSD **1.45 ms** (worst 5.8 ms) | | met |
| The same, image evicted from the page cache | < 20 ms | DST stereo **2.20 ms**, plain DSD **1.48 ms** | | met |
| Cold seek, WAV (DST stereo) | < 20 ms | **2.7 ms** (worst 3.3 ms); the same seeks in DSF 1.7 ms | 2.5 ms | met |
| Seek with a warm seek index (after a remount) | | DST stereo **1.61 ms**, plain DSD **0.48 ms** | | |
| DSD-to-PCM conversion, stereo, 176.4 kHz, default 40 kHz filter, one core | at least 100x real time | **308x** (AVX2 code, chosen at run time); 247x with SSE2 only | 309x | met |
| The same with the wide (80 kHz) filter | at least 100x real time | **149x**; 108x with SSE2 only | 149x | met |
| `ls -l`, `stat`, reading tags | no decoding or conversion | `stat` of all 186 files: 13 ms. WAV header and tags: no read of the image. A DSF file's header read decodes the track's first frames (222 KB of the image per file, [why](design.md#read-ahead)) | | met for WAV; DSF headers decode |
| Memory | bounded by `--cache-mb` | peak resident memory **169 to 356 MiB** with the default 256 MiB cache, during 4 players and a tagger | 169 to 356 MiB | met |
| 4 players and a tagger walking the library | no read over 50 ms | worst read **36.2 ms**, 99th percentile 0.24 to 3.2 ms; the tagger's worst file 26.9 ms | worst 23.6 ms | met |

- The cold-seek means include a few slow seeks (up to 5.8 ms, in three of the six rounds); the
  fastest plain DSD seeks took 0.29 ms. Inside one process without FUSE, the in-process seeks
  of the same scenario took 1.06 ms (DST stereo) and 0.24 ms (plain DSD), 0.89 and 0.22 ms with
  the v3 build.
- `bench decode` writes the decoded area to a RAM disk; sacdfs's read copies every byte
  through FUSE. The DST 6-channel area and the v3 DST stereo area are at the 1.5x line.

## NAS container (server-side)

sacdfs runs in the container of [deployment.md](deployment.md) next to the images, and a
client reads the DSF and WAV files over the network. Target (#6): < 25 ms for a cold seek,
sacdfs's own time plus about one round trip.

**Emulated LAN, this machine.** Both ends in containers on the machine above: the container
image built from the release code, sharing its mount over SMB (Samba, SMB 3.1.1), images on
the NVMe disk; `tc netem` emulating a gigabit LAN (0.19 ms each way at 1 Gbit/s; a TCP connect
took 0.68 ms); a client reading with libsmbclient as a player would: `stat`, then
`open`, a seek, one 128 KiB read and `close`, each on a freshly started container (sacdfs's
frame cache and seek index cold). 3 rounds of 3 seeks per disc, at other positions each round.

| What | Target | Measured | |
|---|---|---|---|
| Cold seek over SMB, DSF, server's page cache evicted | < 25 ms | **4.8 ms** plain DSD, **5.7 ms** DST stereo (worst 6.1 ms) | met |
| Cold seek over SMB, WAV, the same | < 25 ms | **5.4 ms** plain DSD, **6.1 ms** DST stereo (worst 6.6 ms) | met |
| The same with the server's page cache warm | | DSF 3.2 / 5.1 ms, WAV 4.4 / 5.5 ms | |
| Cold seek over NFS, and over SMB from a NAS's hard disks | < 25 ms | measured on the NAS after the v0.1.0 tag ([#21](https://github.com/aclater/sacdfs/issues/21) part 2); NFS-Ganesha in the container needs a rootful container runtime, which this machine doesn't have | |

On the emulated LAN the open takes 0.6 ms (one round trip) and the read 2.1 to 5.0 ms:
sacdfs's cold seek on the server's own disk (1.8 to 3.0 ms without netem, "loopback") plus a
round trip and 128 KiB at gigabit speed (about 1.1 ms). The image reads stay on the server, so
a NAS with hard disks adds its disk seeks to the server's time only.

## Client-side

sacdfs runs on a Linux client, the images on a network share. Every read of an image is a
request to the server, so the number of reads per seek matters most here.

**NFS:** an NFS 4.2 share on a NAS on gigabit Ethernet, its pool on hard disks. The client
mounts it with FS-Cache, so sacdfs was started with a small `LD_PRELOAD` shim (not part of
sacdfs) that adds `O_DIRECT` to its opens of the images: every read then goes to the server,
which the NFS READ counts in `/proc/self/mountstats` confirmed. Three plain DSD stereo images
and three DST stereo images, one seek each per round, 3 rounds, at positions not read before
that day. *First touch:* the NAS most likely reads those sectors from its disks; *NAS cache
warm:* the same seek again.

**SMB:** the emulated gigabit LAN of the server-side runs, the images on the SMB server's NVMe
disk. A kernel SMB mount needs root, so sacdfs read the images through a measurement-only
`LD_PRELOAD` shim that sends its reads to libsmbclient: one SMB READ per read and no client
cache, as a `cache=none` mount does. 3 rounds of 3 seeks per disc.

*Cold:* an empty seek index. *Warm:* a remount whose seek index the previous mount saved after
the same seek.

| What | Target | Measured | |
|---|---|---|---|
| NFS, cold seek, first touch | < 50 ms | **21.9 ms** plain DSD (worst 30.5 ms), **36.9 ms** DST (worst 64.7 ms) | met on average; the worst DST seek is over |
| NFS, cold seek, NAS cache warm | < 50 ms | **2.7 ms** plain DSD, **4.0 ms** DST | met |
| NFS, warm seek index | within 2x local + one round trip + the first read's transfer: about 4.7 ms plain DSD, 5.2 ms DST | **1.8 ms** plain DSD, **2.3 ms** DST | met |
| SMB, cold seek, server's page cache evicted | < 80 ms | **4.3 ms** plain DSD (worst 4.5 ms), **5.3 ms** DST (worst 6.2 ms); 3.4 / 4.8 ms with the server's cache warm | met on an SSD; a NAS's hard disks add a disk seek per call (#21) |
| SMB, warm seek index | the same formula, from this harness's local seeks (0.40 / 1.40 ms) and its round trip (0.69 ms): about 2.9 ms plain DSD, 4.1 ms DST | **2.4 ms** plain DSD, **2.6 ms** DST | met |
| NFS, 4 players and a tagger | no read over 50 ms | players' 99th percentile **5.3 to 22.2 ms**; the tagger's worst file 8 to 10 ms in five rounds of six (124 ms once); players' worst reads 75 to 170 ms | not met: a player's reads of parts of an image the NAS reads from its hard disks take up to 170 ms |

- Calls per seek: 3 (plain DSD) and 5 to 5.7 (DST) cold, 1 warm, over both protocols, each
  call one request; 9 and 13 NFS or SMB READs per mount cold (the TOCs take 6 to 8), 7 and 9
  warm.
- A warm seek is one read of the image: about 170 KiB for plain DSD, 75 KiB for DST, plus a
  round trip. The target counts that transfer (#6, revised).
- First touches are disk seeks on the NAS, one per call: a DST cold seek makes about two more
  calls than a plain DSD one, which is why it costs more there and why its worst case is over
  50 ms.

## Calls and bytes per seek

What a seek costs in requests to the image, without FUSE (`examples/seek_latency`): 200 seeks
of 128 KiB spread over each area, each on a freshly opened disc.

| Area | Cold: calls / KiB | Warm, the same seek after a remount: calls / KiB | Warm, a track played through before a remount |
|---|---|---|---|
| plain DSD stereo | 3.05 / 193 | | |
| DST stereo | 4.86 / 103 | | |
| DST 6 channels (same disc) | 4.82 / 156 | | |
| DSD stereo (hybrid disc) | 3.00 / 192 | | |
| DST 5 channels (same disc) | 5.46 / 114 | | |
| DST stereo, 2064-byte sectors | 5.05 / 103 | | |
| DST stereo, 2064-byte sectors (another disc) | 4.83 / 100 | | |
| **all** | **4.44 / 137** | **1.07 / 110** | **1.05 / 109** |

**scarletbook's own seeking, for comparison** (#47; `examples/library_seek`, scarletbook 0.4.1,
not measured again in this round): its cold seek took **8.84 calls and 373 KiB** per seek with
its default options (7.22 and 260 tuned), against sacdfs's 4.31 and 136 then; from a complete
exact index 2.68 calls and 172 KiB (K = 64) against sacdfs's warm 1.01 and 103. Over NFS a cold
seek took 21 to 29 ms through scarletbook against 7 to 15 ms with sacdfs's locator. Why sacdfs
keeps its own: [design](design.md#why-not-scarletbooks-seeking).

## Straight reads

`cat`-like reads (1 MiB) of every file of an area on a fresh mount, `--no-index-cache`, images
in the page cache; seconds, means of 3, and sacdfs's CPU time:

| Area | DSF / WAV bytes | DSF | WAV | WAV/DSF | CPU, DSF / WAV | `bench decode` |
|---|---|---|---|---|---|---|
| DST stereo, 50 min | 2.12 / 3.19 GB | **4.30 s** (3.87 v3) | **4.61 s** (4.23 v3) | 1.07 | 99 / 117 s | 3.17 s (2.58 v3) |
| plain DSD stereo, 79 min | 3.34 / 5.01 GB | **1.60 s** (1.46 v3) | **1.97 s** (1.97 v3) | 1.23 | 2.2 / 33 s | 1.17 s (1.03 v3) |
| DST 6 channels, 50 min | 6.36 / 9.55 GB | **13.30 s** (10.89 v3) | **15.51 s** (13.14 v3) | 1.17 | 314 / 382 s | 8.87 s (8.83 v3) |

- That is 490 MB/s for the DSF files of the DST stereo area and 2.1 GB/s for plain DSD; 690 MB/s
  and 2.5 GB/s for their WAV files.
- A WAV file at 176.4 kHz holds 1.5 times the bytes of the DSF file. Plain DSD needs no
  decoding, so the copy sets the pace; on DST areas decoding dominates, and conversion adds
  about 20 % CPU.
- **The x86-64-v3 build** reads DST areas 10 % (stereo) to 18 % (6 channels) faster, since
  scarletbook's DST decoder is then compiled for AVX2 too; its converter is the same AVX2 code
  a generic build already chooses at run time ([below](#converter-throughput)). Plain DSD WAV
  reads are the same in both.

## Converter throughput

The DSD-to-PCM converter alone: `cargo bench --bench pcm` (criterion, one frame at a time with
both neighbours, noise-like DSD, 1 s warm-up and 3 s measurement per case), pinned to one core
whose other hardware thread was idle, 3 rounds. The generic build with each of its three code
paths forced by `SACDFS_SIMD`: *baseline* (SSE2), *v3* (AVX2, what runs on an AVX2 CPU by
default) and *v4* (AVX-512, built and tested but not the default); and the x86-64-v3 build. µs
per frame (1/75 s), and in brackets the multiple of real time per stereo stream; for 6 channels
per stereo pair.

| Ch | Filter | Rate | baseline | **v3 (default)** | v4 | v3 build |
|---|---|---|---|---|---|---|
| 2 | 40 kHz | 88.2 kHz | 43.3 (308x) | **36.4 (367x)** | 44.2 (302x) | 36.1 (370x) |
| 2 | 40 kHz | 176.4 kHz | 54.0 (247x) | **43.3 (308x)** | 49.5 (269x) | 43.1 (309x) |
| 2 | 40 kHz | 352.8 kHz | 66.2 (201x) | **53.0 (252x)** | 58.1 (230x) | 53.0 (252x) |
| 2 | 30 kHz | 88.2 kHz | 64.6 (207x) | **48.5 (275x)** | 53.3 (250x) | 48.6 (274x) |
| 2 | 30 kHz | 176.4 kHz | 95.0 (140x) | **68.0 (196x)** | 66.4 (201x) | 67.7 (197x) |
| 2 | 30 kHz | 352.8 kHz | 107.5 (124x) | **78.4 (170x)** | 75.4 (177x) | 78.0 (171x) |
| 2 | wide 80 kHz | 176.4 kHz | 123.5 (108x) | **89.6 (149x)** | 92.4 (144x) | 89.5 (149x) |
| 2 | wide 80 kHz | 352.8 kHz | 115.4 (116x) | **85.9 (155x)** | 96.8 (138x) | 84.2 (158x) |
| 6 | 40 kHz | 88.2 kHz | 130.4 (307x) | **109.0 (367x)** | 132.6 (302x) | 108.7 (368x) |
| 6 | 40 kHz | 176.4 kHz | 161.7 (247x) | **129.5 (309x)** | 148.4 (270x) | 129.6 (309x) |
| 6 | 40 kHz | 352.8 kHz | 198.5 (201x) | **159.4 (251x)** | 174.0 (230x) | 159.1 (251x) |
| 6 | 30 kHz | 88.2 kHz | 193.1 (207x) | **145.2 (275x)** | 159.9 (250x) | 145.6 (275x) |
| 6 | 30 kHz | 176.4 kHz | 284.5 (141x) | **204.0 (196x)** | 199.4 (201x) | 203.0 (197x) |
| 6 | 30 kHz | 352.8 kHz | 321.8 (124x) | **235.1 (170x)** | 226.0 (177x) | 233.7 (171x) |
| 6 | wide 80 kHz | 176.4 kHz | 369.6 (108x) | **268.7 (149x)** | 276.8 (145x) | 268.6 (149x) |
| 6 | wide 80 kHz | 352.8 kHz | 345.3 (116x) | **257.7 (155x)** | 290.2 (138x) | 252.7 (158x) |

- Every path meets the 100x bar of #6, the SSE2 path included, and all give the same bytes.
- **v4 against v3:** faster only with the 30 kHz filter at 176.4 and 352.8 kHz (2 to 4 %),
  slower everywhere else (up to 22 %). With both hardware threads of a core converting, as
  the decoder pool does under load, v4 measured 25 to 35 % slower than v3 in #40 (not measured
  again). So v3 stays the default on this AVX-512 CPU.
- **The v3 build** converts at the speed of the generic build's v3 path, within 2 %: the
  converter's hot code is already compiled for AVX2 in every build.
- **Runs of frames** (#42): plain DSD frames are converted four at a time. The converter alone
  then costs 41.7 µs per frame against 43.3 µs one at a time (stereo, 176.4 kHz, v3 path);
  through the mount the gain is fewer, larger jobs on the pool.
- Making a converter (designing its filters) takes 27 to 41 µs (24 to 36 µs with the v3
  build), once per worker thread.
- The 30 kHz filter is not a half-band, so its last stage costs more than the default's; the
  wide filter at 176.4 kHz is the costliest (a 337-tap stage).

## Concurrency and memory

The scenario of #6, for 30 s: four players each reading 128 KiB at a time in real time with a
2 s buffer from a random position, and a tagger walking the whole mount over and over (`stat`,
the first 4 KiB, the tags), on a fresh mount with `--no-index-cache`; 3 rounds. Worst read and
99th percentile in ms, without each player's first read; peak memory is sacdfs's peak resident
set (`VmHWM`).

**Local images** (page cache):

| Players | Build | Players' reads, worst / p99, per round | Each player's first read, worst | Tagger's worst file | Peak memory |
|---|---|---|---|---|---|
| four areas of three discs, WAV | generic | 36.2 / 0.35, 24.2 / 0.24, 21.5 / 0.28 | 8.1 | 8.2 to 26.9 | 352 to 356 MiB |
| | v3 | 15.2 / 0.98, 20.9 / 0.34, 18.4 / 0.81 | 7.2 | 6.1 to 10.5 | 355 to 356 MiB |
| four tracks of one DST stereo area, WAV | generic | 18.1 / 1.93, 14.6 / 2.26, 18.0 / 1.33 | 4.8 | 5.9 to 6.8 | 224 MiB |
| | v3 | 15.0 / 2.09, 15.9 / 1.55, 23.6 / 1.46 | 3.1 | 4.7 to 10.0 | 223 to 226 MiB |
| four tracks of one DST stereo area, DSF | generic | 8.1 / 2.79, 11.9 / 3.17, 12.7 / 2.88 | 3.3 | 6.0 to 11.0 | 169 MiB |
| | v3 | 17.7 / 3.18, 16.0 / 4.45, 16.1 / 7.12 | 4.3 | 8.1 to 8.7 | 169 to 170 MiB |

**Images on NFS** (the share and shim of [client-side](#client-side)), DSF files of plain DSD
stereo images, generic build:

| Players | Players' reads, worst / p99, per round | Each player's first read, worst | Tagger's worst file | Peak memory |
|---|---|---|---|---|
| four tracks of one image | 89.9 / 7.3, 106.0 / 14.3, 112.7 / 22.2 | 41.9 to 102.1 | 124.1, 8.3, 8.8 | 146 to 147 MiB |
| four images | 170.1 / 6.9, 75.4 / 5.3, 158.6 / 19.2 | 52.4 to 163.2 | 8.2, 9.7, 8.9 | 137 to 146 MiB |

- Locally every read stays under the 50 ms target of #6.
- On NFS the slow reads are reads of parts of an image the NAS must read from its hard disks;
  the tagger's reads run beside them (8 to 10 ms) but for one file in one round.
- Memory stays bounded by `--cache-mb` (default 256 MiB) plus a fixed amount: decoding runs
  on sacdfs's own pool, and no library cursor holds a read-ahead window of its own.

## Mount, listing and tag-walk cost

| What | Measured |
|---|---|
| Mount of a library on an NFS share (154 images; `sacdfs` returns once mounted) | **0.12 s** (7 mounts, 0.118 to 0.122 s, with or without `--flatten`); 2.0 s for the first, with the client's NFS attribute cache cold |
| `stat` of all 186 files (93 DSF, 93 WAV) on a fresh mount, which opens the five images and reads their TOCs | **13 ms** |
| Header and tag reads of all 186 files, as a tagger walks the chunks | **113 ms** (100 ms v3), 0.22 s of sacdfs CPU, 20.5 MB of image reads, all of them for DSF headers |
| DSF tag walk of the 93 DSF files (open, first 4 KiB, the tag, close) | **0.09 s**: 342 calls to the image (3.7 a file), 20.7 MB (222 KB a file) |

Mounting reads no image: only the names of the source tree are walked. An image's TOCs are read
when its directory is first listed.

## On a NAS (#21)

Measured by #21 part 1 on the owner's NAS, a different machine from the rest of this page, so
kept apart: a 12th-gen Intel Core i5-1235U (2 performance and 8 efficiency cores, 12 threads,
AVX2) with 96 GB of RAM; a generic build of the release code (with #53) in a one-shot app;
images read-only on local ZFS (hard disks), 3 stereo areas and 1 six-channel area. The ZFS
cache can't be dropped from the container, so each area was read twice: from disk, then cached.

| What | Result |
|---|---|
| Straight read, stereo DSF (plain DSD) | 308 to 334 MB/s from disk, 365 to 407 MB/s cached (about 450 to 580x real time); 5.5 to 6.7 s of CPU per 1.7 GB area |
| Straight read, stereo WAV 176.4 kHz / 24-bit | 486 to 558 MB/s from disk, 531 to 565 MB/s cached (about 460 to 530x real time); 41 s of CPU per 2 500 s of audio, about 16 ms of CPU per second of audio |
| Straight read, 6-channel DST, DSF / WAV | 83 / 108 MB/s, the same from disk and cached, so limited by the CPU: about 40x real time, on about 9.6 of the 12 threads |
| Concurrent WAV readers (1, 2, 4, 8 at full speed) | about 280x real time for one reader, about 560x in all for 2 to 8 (a lower bound: the run's 1 s polling rounds the wall time up); no stall |
| Cold seek, first 128 KiB (local ZFS, no seek index) | DSF 7.7 ms, WAV 11.2 ms (target < 20 ms) |
| sacdfs's peak memory | 307 to 396 MB at the default `--cache-mb` |

No default needs to change for a NAS of this class: playback, browsing and seeking are far
inside the targets, and a bulk copy of multichannel DST is limited by DST decoding at about 40x
real time. Server-side through the published container over NFS and SMB is measured on the NAS
after the v0.1.0 tag (#21 part 2).

## History

What each improvement changed, one line each, with the pull request that measured it (each
against its own "before", on the code of its day).

| PR | Change | Before → after |
|---|---|---|
| #4 | Frame search aimed with the TOC's track starts, slightly past each estimate | calls per cold seek on real discs 7.0–10.7 → 4.6–6.1 |
| #15 | WAV pipeline: demultiplex and convert in parallel, 8 MiB read-ahead for fast readers | plain DSD WAV straight read 3.42 s → 2.29 s (2.05x → 1.39x the DSF read) |
| #25 | Persistent seek index; probes and first reads sized to the area's frames | cold seek 5.29 → 4.50 calls, 293 → 139 KiB; NFS cold seek, first touch, 27.8 → 18.6 ms (plain DSD), 36.9 → 26.8 ms (DST); NFS seek after a remount 2.7 ms |
| #35 | Image files kept open between reads (`--max-open-images`) | NFS warm seek, plain DSD 3.05 → 2.02 ms |
| #36 | The kernel's own read-ahead of a first read no longer starts sacdfs's | DSF tag walk 1 022 → 342 calls to the image, 109.5 → 20.7 MB, 0.26 → 0.07 s |
| #40 | Converter code chosen at run time (SSE2, AVX2, AVX-512) | generic build, 176.4 kHz stereo: 40 kHz filter 242x → 305x real time, wide filter 93x → 147x |
| #42 | Plain DSD frames converted four at a time | plain DSD WAV straight read 2.42 → 2.07 s, CPU 48 → 34 s, 1.40x → 1.20x the DSF read |
| #46 | Concurrent reads of one image (positional reads, an image clone per read) | NFS: tagger's worst file 38–163 → 7–13 ms; players on one image, 99th percentile 13–118 → 2–10 ms |
| #47 | scarletbook 0.4's seeking measured, sacdfs's locator kept | cold seek 8.84 calls / 373 KiB through scarletbook against 4.31 / 136 with the locator; NFS 21–29 ms against 7–15 ms |

## Reproducing

```sh
# straight read of an area, against scarletbook's own decoder
bench decode IMAGE --area 2ch --threads 0 --sink raw /dev/shm/out
sacdfs -f --no-index-cache IMAGE /mnt/x &
time cat /mnt/x/*/Stereo/*.dsf > /dev/null      # or *.wav
fusermount3 -u /mnt/x
# calls, bytes and time per seek without FUSE, cold and with a warm seek index
cargo run --release --example seek_latency -- --seeks 200 --cache /tmp/idx IMAGE...
# scarletbook's seeking against the locator
cargo run --release --example library_seek -- IMAGE...
# the converter alone; CI runs every case once (-- --test)
RUSTFLAGS="-C target-cpu=x86-64" cargo bench --bench pcm
```

Seeks are one `pread` of 128 KiB on a fresh mount (with `--no-index-cache` for cold seeks);
the concurrency scenario is four real-time readers and a tagger as described above.
