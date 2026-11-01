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
| DSD-to-PCM conversion, stereo, 176.4 kHz, default 40 kHz filter, one core | at least 100x real time | **298x** (scarletbook-pcm 0.5.3, #84; sacdfs 0.1's own converter: 308x) | 273x | met |
| The same with the wide (80 kHz) filter | at least 100x real time | **210x** (0.1: 149x) | 190x | met |
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
| Cold seek over NFS and SMB, the published container on the owner's NAS (hard disks) | < 25 ms | **15 to 22 ms** with the NAS's cache warm; **31 to 61 ms** (worst 92 ms) when the audio comes from its hard disks ([below](#on-a-nas-server-side-21-part-2)) | met from the NAS's cache; not met from its hard disks |

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

These are sacdfs 0.1.0's figures (its own locator, scarletbook 0.5.0). Since 0.2.0 seeking is
scarletbook's: see [Seeking through scarletbook](#seeking-through-scarletbook-020-28).

## Seeking through scarletbook (0.2.0, #28)

sacdfs 0.2.0 seeks with scarletbook 0.5.2 ([design](design.md#seeking-scarletbooks-28)). Both
builds below are scarletbook 0.5.2, generic x86-64: **(a)** sacdfs's own frame locator (0.1.0's
code), **(b)** scarletbook's provisional readings and seek hints. Measured on 2026-11-01 under
the benchmark lock, (a) and (b) interleaved, their order swapped every other round. *Not a quiet
machine:* other agents' work kept the load average at 5 to 20 between parts, and the cleanroom VM
averaged 2.5 threads busy; the NAS was serving a library import over SMB at the same time. Times
are noisier than in the 0.1.0 round; calls and bytes are not affected.

**Calls and bytes per seek** (`examples/seek_latency`, 200 seeks of 128 KiB per area on a freshly
opened disc, the 7 areas of the real discs):

| Area | Cold (a) | Cold (b) | Warm, same seek after a remount (a) | (b) |
|---|---|---|---|---|
| plain DSD stereo | 3.05 / 193 | 2.72 / 183 | 1.33 / 213 | 1.00 / 150 |
| DST stereo | 4.86 / 103 | 3.02 / 105 | 1.01 / 75 | 1.00 / 57 |
| DST 6 channels (same disc) | 4.82 / 156 | 3.17 / 128 | 1.04 / 96 | 1.00 / 76 |
| DSD stereo (hybrid disc) | 3.00 / 192 | 3.00 / 191 | 1.00 / 170 | 1.00 / 141 |
| DST 5 channels (same disc) | 5.46 / 114 | 3.35 / 106 | 1.03 / 68 | 1.00 / 54 |
| DST stereo, 2064-byte sectors | 4.83 / 100 | 3.05 / 105 | 1.00 / 72 | 1.00 / 55 |
| DST stereo, 2064-byte sectors (another disc) | 5.05 / 103 | 3.13 / 107 | 1.02 / 75 | 1.00 / 55 |
| **all** | **4.44 / 137** | **3.06 / 132** | **1.06 / 110** | **1.00 / 84** |

calls / KiB per seek. A track played through before a remount, then a seek into it: (a) 2.28 /
265, (b) 2.29 / 178 (20 seeks per area).

**Through the mount, local images** (open, one 128 KiB `pread`, close, on a fresh mount; 3 seeks
x 3 rounds per row; ms, mean):

| | Cold (a) | Cold (b) | Warm (a) | Warm (b) |
|---|---|---|---|---|
| plain DSD stereo, page cache | 0.49 | 0.54 | 0.44 | 0.41 |
| plain DSD stereo, image evicted | 1.03 | 1.02 | 0.65 | 0.73 |
| DST stereo, page cache | 1.38 | 1.36 | 1.31 | 1.17 |
| DST stereo, image evicted | 1.65 | 1.78 | 1.31 | 1.45 |

**Client-side NFS** (the share and `O_DIRECT` shim of [client-side](#client-side), three images
of each kind, 3 rounds; ms, mean and median; *first touch*: a position not read before that day,
(b) 0.02 of the track after (a)'s):

| | (a) mean / median | (b) mean / median | calls (a) / (b) |
|---|---|---|---|
| plain DSD, cold, first touch | 37.3 / 34.4 | 47.8 / 26.3 | 3.00 / 3.17 |
| plain DSD, cold, NAS cache warm | 2.2 / 2.2 | 2.4 / 2.4 | |
| plain DSD, warm seek index | 1.5 / 1.5 | 1.5 / 1.6 | 1 / 1 |
| DST, cold, first touch | 52.0 / 41.0 | 36.9 / 29.6 | 5.33 / 3.22 |
| DST, cold, NAS cache warm | 3.2 / 3.2 | 2.8 / 2.7 | |
| DST, warm seek index | 2.0 / 2.0 | 2.0 / 1.9 | 1 / 1 |

18 seeks per row (two runs of 9 at other positions). First touches are disk seeks on the NAS
while it served the import: the worst plain DSD first touches, 279 and 128 ms with (b), were
in the first run, at the same call counts as (a)'s; in the second, quieter run (b) was faster
on both kinds (plain DSD 15.6 against 28.4 ms, DST 21.9 against 38.2). On DST images (b) makes
two calls fewer per cold seek, which is what #71 asked for: its first-touch DST seek is about a
quarter faster here, and its worst one depends on the NAS's disks.

**In-process time per seek** (the same 200 seeks as the calls table, including decoding, means of
3 runs): cold (a) 0.64 ms, (b) 0.65 ms; warm after a remount (a) 0.83 ms,
(b) 0.86 ms. (A first measurement of warm seeks, 0.82 against 0.98 ms, found the cost of
building the saved form of every open disc's index at each flush of the persistent seek index;
sacdfs now builds it only for discs whose cursors recorded something since, and the second
figures are with that.)

**Concurrency** (#6 scenario, 30 s x 3 rounds, worst / p99 ms of the players, peak memory):

| Scenario | (a) | (b) |
|---|---|---|
| four tracks of one DST stereo area, DSF, local | 7.3 / 2.30, 21.3 / 3.00, 14.0 / 4.64; 169 MiB | 10.2 / 3.42, 17.6 / 2.12, 9.0 / 4.08; 169 MiB |
| four areas of three discs, WAV, local | 24.8 / 0.34, 19.4 / 0.23, 15.4 / 0.26; 357 MiB | 20.2 / 0.76, 27.2 / 1.04, 26.0 / 0.66; 357 MiB |
| four plain DSD images on NFS, DSF | 472 / 63.5, 97 / 5.1, 379 / 92.7; 147 MiB | 326 / 79.3, 79 / 10.1, 278 / 14.0; 147 MiB |

The tagger's worst file stayed under 20 ms locally in every round of both. The NFS rounds are
dominated by the NAS's disks, busy with the import (#71's figures were 75 to 170 ms worst on a
quiet NAS).

**A library import's reads** (#78: per file, on one open, the first 768 KiB in 128 KiB reads, 36
KiB at the midpoint, the last 33 KiB; every file of an area, fresh mount, no seek index, local
images in the page cache; ms per file, mean of 3 rounds, and the image calls and MiB per file):

| Area | (a) | (b) |
|---|---|---|
| plain DSD stereo (24 files) | 1.37 ms; 21.3 calls, 2.30 MiB | 1.43 ms; 34.0 calls, 2.15 MiB |
| DST stereo (14 files) | 5.8 ms; 15.9 calls, 0.90 MiB | 5.7 ms; 16.0 calls, 0.76 MiB |
| DST 6 channels (14 files) | 12.9 ms; 22.6 calls, 1.85 MiB | 12.1 ms; 31.1 calls, 1.71 MiB |

- The 6-channel DST file costs about 7 ms more than a stereo one here, in both builds, so seeking
  is not where #78's extra time goes. The same bytes of a 6-channel file are a third as many
  frames, each about three times the decoding, and sacdfs decodes one frame per job: probably
  fewer, longer jobs at the head of each file. On the NAS's slower CPU (#21: about 40x real time
  for 6-channel DST) that would scale up toward #78's 0.09 s; not measured there.
- (b) reads fewer bytes per file but makes more, smaller calls on plain DSD and on 6 channels
  (read-ahead's reads, sized to its ranges). Locally that costs nothing measurable; on a network
  share each call is a round trip (about 0.1 ms on this LAN). Whether read-ahead past the 768 KiB
  head is wasted for this pattern was not measured; read-ahead is unchanged.

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
  scarletbook's DST decoder is then compiled for AVX2 too; the converter chooses its AVX2 code
  at run time in either build ([below](#converter-throughput)). Plain DSD WAV reads are the
  same in both.

## Converter throughput

Since #84 the converter is scarletbook-rs's `scarletbook-pcm` (0.5.3); the figures of
sacdfs's own converter, which it replaced, are kept for comparison.

**How it was measured** (`examples/pcm_compare.rs speed` of #85):
- 60 s of a real stereo area, decoded and held in memory;
- converted through `sacdfs::pcm::Converter`, which reads the frames in place and writes 24-bit bytes;
- in runs of 4 frames with both neighbours, as the WAV path converts plain DSD;
- best of 3 on one thread, then 32 threads at once;
- under the benchmark lock, on 2026-11-01.

Multiples of real time per stereo stream:

| Filter | Rate | scarletbook-pcm, generic build | old converter, generic build (AVX2 at run time) | change | scarletbook-pcm, v3 build | old converter, v3 build | change |
|---|---|---|---|---|---|---|---|
| 40 kHz | 88.2 kHz | **405x** | 375x | +8 % | 354x | 369x | -4 % |
| 40 kHz | 176.4 kHz | **298x** | 315x | -5 % | 273x | 316x | -14 % |
| 40 kHz | 352.8 kHz | **233x** | 257x | -9 % | 219x | 257x | -15 % |
| 30 kHz | 88.2 kHz | **363x** | 286x | +27 % | 326x | 282x | +16 % |
| 30 kHz | 176.4 kHz | **259x** | 200x | +30 % | 240x | 201x | +19 % |
| 30 kHz | 352.8 kHz | **211x** | 175x | +21 % | 198x | 175x | +13 % |
| wide 80 kHz | 176.4 kHz | **210x** | 151x | +39 % | 190x | 150x | +27 % |
| wide 80 kHz | 352.8 kHz | **189x** | 158x | +20 % | 173x | 159x | +9 % |

- **All 32 threads**, 176.4 kHz, 40 kHz filter: 3641x in total (generic build) and 3118x (v3 build), against the old converter's 3939x and 3985x.
- **The 100x bar of #6** is met everywhere, with 89 % margin or more.
- **The v3 build is slower than the generic one**, by 6 to 13 %. scarletbook-pcm chooses AVX2 and FMA at run time, so a generic build already runs its fastest code, and the x86-64-v3 build compiles it differently. The v3 build still helps sacdfs overall: DST decoding gets 10 to 18 % faster ([straight reads](#straight-reads)).
- **Frame by frame** (no runs), 176.4 kHz with the 40 kHz filter: 287x (generic) and 265x (v3).

**Through the mount.** The method:
- WAV files read with `cat` on a fresh mount with `--no-index-cache`, images in the page cache, means of 3;
- cold WAV seeks as above, 40 per mount on 3 mounts;
- the same build with either converter (#85's switch), at 176.4 kHz with the 40 kHz filter.

| What | Generic build: old converter | Generic build: scarletbook-pcm | v3 build: old converter | v3 build: scarletbook-pcm |
|---|---|---|---|---|
| WAV straight read, DST stereo | 5.06 s, 117 s CPU | 5.33 s (+5 %), 121 s CPU | 4.70 s, 108 s | 4.86 s (+3 %), 113 s |
| WAV straight read, plain DSD stereo | 2.30 s, 35 s CPU | 2.53 s (+10 %), 39 s CPU | 2.34 s, 36 s | 2.50 s (+7 %), 41 s |
| WAV straight read, DST 6 channels | 16.42 s, 377 s CPU | 16.84 s (+3 %), 386 s CPU | 14.08 s, 319 s | 14.62 s (+4 %), 334 s |
| Cold WAV seek, DST stereo, mean (worst) | 0.95 ms (1.63) | 0.97 ms (1.62) | 0.93 ms (1.95) | 0.96 ms (2.31) |
| Cold WAV seek, plain DSD stereo, mean (worst) | 0.40 ms (1.16) | 0.41 ms (0.93) | 0.38 ms (0.90) | 0.42 ms (1.11) |

Everything is within the owner's 15 % leeway. The largest gap is the plain DSD WAV read, where conversion is the only CPU work: +10 %.
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
real time.

### On a NAS, server-side (#21 part 2)

The same NAS, with the published image `ghcr.io/aclater/sacdfs:0.1.0` running as a TrueNAS SCALE
25.10 custom app, set up as [deployment.md](deployment.md#truenas-scale) describes. The library
(154 images) was read-only on local ZFS (hard disks), and clients read over the LAN through the
container's own SMB and NFS servers.

- **The setup:**
  - **Ports:** the NAS's own SMB and NFS services hold 445 and 2049, so the container published
    10445 and 12049.
  - **The client:** a desktop on 2.5 Gbit/s Ethernet, 0.25 ms round trip to the NAS.
  - **User-space clients:** a kernel mount needs root, which the client had not. NFS went
    through libnfs (NFS 4.0) and SMB through libsmbclient (seeks) and Samba's `smbclient` (straight
    reads), both SMB3.
- **Cold seek:** `stat`, `open`, one 128 KiB read at a random position and `close`, each on a
  freshly started container with `SACDFS_INDEX_CACHE=off`. There were 3 rounds, with 2 stereo
  areas per protocol and format and 1 six-channel area per protocol in each round, every seek in
  an image not read before.
  - *From the disks:* the first touch of that part of the image. The TOCs were already in the
    NAS's cache from an earlier listing.
  - *NAS cache warm:* the same seeks again on fresh containers.
  - *Warm:* the same seek repeated at once on the same container.
- **The NAS's cache:** the ZFS cache (ARC) can't be dropped from an app. Each figure says
  whether its data came from the disks or the cache.
- **CPU and memory:** a sidecar in the app's PID namespace sampled sacdfs's CPU time and every
  container process's resident memory once a second.

| What | Target | NFS | SMB | |
|---|---|---|---|---|
| Cold seek, stereo DSF, from the disks | < 25 ms | **56.7 ms** (worst 91.5) | **37.3 ms** (worst 59.4) | not met |
| Cold seek, stereo WAV, from the disks | < 25 ms | **40.9 ms** (worst 55.5) | **31.3 ms** (worst 49.9) | not met |
| Cold seek, 6-channel DSF, from the disks | < 25 ms | **51.5 ms** (worst 55.5) | **61.4 ms** (worst 82.6) | not met |
| Cold seek, stereo DSF / WAV, NAS cache warm | < 25 ms | **15.1 / 18.2 ms** (worst 20.6) | **17.3 / 14.5 ms** (worst 23.6) | met |
| Cold seek, 6-channel DSF, NAS cache warm | < 25 ms | **22.3 ms** (worst 27.7) | **21.9 ms** (worst 25.4) | met on average |
| The same seek again, warm | | 3.4 to 4.3 ms | 3.8 to 4.6 ms | |
| Straight read, stereo DSF | | **289 MB/s** cached, 268 from the disks | **286 MB/s** cached, 151 from the disks | the client's 2.5 Gbit/s link |
| Straight read, stereo WAV 176.4 kHz / 24-bit | | **291 MB/s** cached, 234 from the disks | **286 MB/s** cached, 237 from the disks | the link |
| Straight read, 6-channel DST as DSF | | **83.2 MB/s** | **64.6 MB/s** | DST decoding |
| 4 players and a tagger | no read over 50 ms | players' worst read **52 to 133 ms**, 99th percentile 1.8 to 5.1 ms; tagger's worst file 79 to 162 ms | | not met from the disks |
| Container memory (all its processes) | | 241 to 244 MiB in the player rounds; up to 911 MiB after an hour of straight reads | | |

- **Where a cold seek's time goes:**
  - *The disks:* from the hard disks, the read itself takes 22 to 51 ms on average. The
    cold seek reads the image 3 to 5 times (see [calls per seek](#calls-and-bytes-per-seek)),
    and each read is a disk seek on the NAS.
  - *From the cache:* `open`, the read and `close` take 5 to 13 ms. The `stat` takes 8 to
    11 ms of that 15 to 22 ms: the first lookup of an image in a fresh container loads its TOCs
    on the NAS's CPU. A player that lists the directory first (which loads the TOCs) pays that
    once per image, not per seek.
  - *Lookups:* the user-space clients look up the whole path on every call, which a kernel
    client caches.
- **Straight reads:**
  - *Stereo:* stereo areas read at the client's link speed, and from the disks at 151 to
    268 MB/s.
  - *Cached:* repeated reads cost sacdfs almost no CPU, because the NAS's kernel keeps the
    decoded files in its page cache.
  - *From the disks:* a first pass costs 12.6 to 14.4 s of sacdfs CPU per stereo DSF area
    (1.4 to 1.7 GB) and 67 s per stereo WAV area (2.6 GB, about 27 ms of CPU per second of
    audio).
  - *6 channels:* the 6-channel DST area costs 560 to 605 s of CPU per pass (4.6 to 4.9 GB),
    which is about 9.6 of the NAS's 12 threads: the same DST limit as in part 1.
  - *Read order:* NFS with one read in flight, in order, reached 83 MB/s. Four reads in flight
    split across the file reached only 43 MB/s, because decoding then runs out of order.
- **4 players and a tagger:** the scenario of [concurrency](#concurrency-and-memory), over NFS,
  WAV files of four images not read before, 30 s per round, 3 fresh containers. The slow reads
  are first touches of the hard disks, as on the [client side](#client-side) (#71). A
  server-side deployment on hard disks does not meet the 50 ms worst-read target either. The 99th
  percentile stays at 1.8 to 5.1 ms.
- **Memory:**
  - *sacdfs:* 172 to 176 MiB at its peak in a player round, and 783 to 791 MiB after an hour of
    straight reads of stereo and 6-channel areas with 12 decoder threads, at the default
    `--cache-mb 256`.
  - *The servers:* smbd took 28 to 75 MiB and NFS-Ganesha 37 to 52 MiB.
- **The app:** a restart (stop and start through the TrueNAS API) took 4 to 6 s, and the shares
  answered 6 to 7 s after it began. sacdfs found the 154 images and mounted within about a second.
- **A stall** ([#72](https://github.com/aclater/sacdfs/issues/72)): once, an SMB straight read
  of a 6-channel DST file waited about 6 hours while sacdfs used no CPU. It was not reproduced
  in 6 more reads of the same area or in the player rounds.

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
| #85 | WAV conversion by scarletbook-pcm 0.5.3, sacdfs's converter removed (#84) | converter alone, 176.4 kHz stereo, 40 kHz filter: 315x → 298x (wide 151x → 210x); WAV straight reads +3 to +10 %; cold WAV seeks within 3 % (generic build) |
| #88 | Seeking moved to scarletbook 0.5.2, the locator removed | cold seek 4.44 → 3.06 calls (DST 4.8–5.5 → 3.0–3.4), 137 → 132 KiB; warm 1.06 → 1.00 calls, 110 → 84 KiB; NFS first-touch DST cold seek median 41 → 30 ms; through the mount within ±12 % locally |
| (#73) | One malloc arena; the frame cache reuses evicted frames' buffers within its cap | library-import replay, 60 images, peak memory at `--cache-mb` 256 / 16: 473 → 271 MiB, 125 → 58 MiB; after straight reads 965–1 066 → 327 MiB; 6-channel DST straight read DSF 11.11 → 11.55 s, WAV 13.52 → 13.98 s, plain DSD 1.69 → 1.59 s |
| (#78) | No read-ahead within a file's first 1 MiB (a scanner's head read) | library-import pattern, 12 decoder threads, per 6-channel DST file: CPU 141–177 → 42–44 ms, wall 17.4–21.6 → 10.3–11.1 ms; stereo DST CPU 60–84 → 46–50 ms |

## Reproducing

```sh
# straight read of an area, against scarletbook's own decoder
bench decode IMAGE --area 2ch --threads 0 --sink raw /dev/shm/out
sacdfs -f --no-index-cache IMAGE /mnt/x &
time cat /mnt/x/*/Stereo/*.dsf > /dev/null      # or *.wav
fusermount3 -u /mnt/x
# calls, bytes and time per seek without FUSE, cold and with a warm seek index
cargo run --release --example seek_latency -- --seeks 200 --cache /tmp/idx IMAGE...
# the converter alone (criterion); CI runs every case once (-- --test)
RUSTFLAGS="-C target-cpu=x86-64" cargo bench --bench pcm
```

Seeks are one `pread` of 128 KiB on a fresh mount (with `--no-index-cache` for cold seeks);
the concurrency scenario is four real-time readers and a tagger as described above.
