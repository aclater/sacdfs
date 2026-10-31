# Performance

How fast sacdfs is, against the targets of [#6](https://github.com/aclater/sacdfs/issues/6),
in each of the three ways it is deployed: images on a local disk, sacdfs in a container on
the NAS that holds the images (server-side), and sacdfs on a Linux client reading images
from a network share (client-side). The design behind these figures is in
[design.md](design.md).

**Where the figures come from.** Each figure is the latest measurement of its kind, from the
pull request that made it, named in brackets (`#25`). They were taken on different days'
code (scarletbook 0.3.0 to 0.5.0, the converter before and after #40 and #42) and under
different machine loads, each against a "before" taken in the same session. A single round
on the release code will replace them (#37).

**The machine.** A 16-core (32-thread) desktop with a Zen 5 CPU and 125 GB of RAM, the images
on a local NVMe disk unless noted. Release builds for generic x86-64 (`-C target-cpu=x86-64`)
unless noted. Every run is under a benchmark lock, so that no other benchmark runs at the
same time, and figures are means of 3 runs. A virtual machine on the same host was not always
idle: its load is given, in hardware threads busy, where it mattered (from its CPU time,
`/proc/<pid>/stat` fields 14 and 15).

**The discs.** Five real discs: plain DSD stereo; DST stereo and 6 channels; DSD stereo with
DST 5 channels (a hybrid disc); and two DST stereo images in 2064-byte raw sectors. Seven
areas, 93 tracks. "Plain DSD" is a disc stored without DST compression, which sacdfs only
demultiplexes; DST areas are decoded too.

## Local images

The images on a local disk, in the page cache unless noted; the targets of #6.

| What | Target | Measured | Source | |
|---|---|---|---|---|
| Straight read of an area's DSF files, against scarletbook's `bench decode` at the same thread count | within 1.5x | **1.43x** (DST stereo, machine loaded: about 14 threads busy); 1.22x on an idle machine with scarletbook 0.2.0 | #4 | met |
| Straight read of an area's WAV files, against its DSF files | within 1.5x | **1.09x** DST stereo, **1.20x** plain DSD stereo, **1.16x** DST 6 channels | #42 | met |
| Cold seek: first 128 KiB at a random offset, fresh mount, empty seek index | < 20 ms | DSF **0.81 ms** (DST stereo), **0.24 ms** (plain DSD); WAV **1.8 ms** (DST stereo, worst 2.3 ms) | #50, #15 | met |
| The same, image evicted from the page cache | < 20 ms | DSF **1.78 ms** (DST stereo), **1.32 ms** (plain DSD) | #25 | met |
| Seek with a warm seek index (after a remount) | | DSF **1.40 ms** (DST stereo), **0.40 ms** (plain DSD) | #25 | |
| DSD-to-PCM conversion, stereo, 176.4 kHz, default 40 kHz filter, one core | at least 100x real time | **305x** (generic build, AVX2 code chosen at run time); 243x with SSE2 only | #40 | met |
| The same with the wide (80 kHz) filter | at least 100x real time | **147x**; 106x with SSE2 only | #40 | met |
| `ls -l`, `stat`, reading tags | no decoding or conversion | `stat` of all 186 files: 10 ms. WAV header and tags: no read of the image; all 93 WAV files' chunk walks in 84 ms. A DSF file's header read decodes the track's first frames (222 KB of the image per file, [why](design.md#read-ahead)) | #15, #36 | met for WAV; DSF headers decode |
| Memory | bounded by `--cache-mb` | peak resident memory **168 to 342 MiB** with the default 256 MiB cache, during 4 players and a tagger; 305 MiB during a straight read | #46, #4 | met |
| 4 players and a tagger walking the library | no read over 50 ms | worst read **27.0 ms** (players on different discs, WAV), 99th percentile 0.2 to 2.9 ms; the tagger's worst file 14.5 ms | #46 | met |

The cold seeks of #50 time `open`, one `pread` and `close` inside one process; the WAV seek of
#15 the same. A `dd` of the same read also times starting a process (3.4 ms in #4, on a
loaded machine).

## NAS container (server-side)

sacdfs runs in the container of [deployment.md](deployment.md) next to the images, and a
client reads the DSF and WAV files over the network. Target (#6): < 25 ms for a cold seek,
sacdfs's own time plus about one round trip.

Measured in #34 with both ends in containers on the machine above: Samba 4.24 (SMB 3.1.1) on
an image directory on the NVMe disk, `tc netem` emulating a gigabit LAN (0.19 ms each way at
1 Gbit/s; a TCP connect takes 0.67 ms), and a client reading with libsmbclient as a player
would: `stat`, then `open`, a seek, one 128 KiB read and `close`, each on a freshly started
container (sacdfs's frame cache and seek index cold).

| What | Target | Measured | Source | |
|---|---|---|---|---|
| Cold seek over SMB, DSF, server's page cache evicted | < 25 ms | **4.7 ms** plain DSD, **5.4 ms** DST stereo (worst 6.1 ms) | #34 | met |
| Cold seek over SMB, WAV, the same | < 25 ms | **5.1 ms** plain DSD, **5.5 ms** DST stereo (worst 6.2 ms) | #34 | met |
| The same with the server's page cache warm | | DSF 3.2 / 5.0 ms, WAV 4.0 / 5.1 ms | #34 | |
| Cold seek over NFS | < 25 ms | not measured; CI checks that the NFS export serves every corpus track | | |

On the emulated LAN the open takes 0.6 ms (one round trip) and the read 2 to 4.4 ms: sacdfs's
cold seek on the server's own disk (0.8 to 2.4 ms without the network) plus a round trip and
128 KiB at gigabit speed (about 1.1 ms). The image reads stay on the server, so a NAS with
hard disks adds its disk seeks to the server's time only; not measured on one yet (#21).

## Client-side

sacdfs runs on a Linux client, the images on a network share. Every read of an image is a
request to the server, so the number of reads per seek matters most here.

**NFS** (#25, #35, #46): an NFS 4.2 share on a NAS on gigabit Ethernet (ping 0.38 ms), its pool
on hard disks. The client mounts it with FS-Cache, so for these runs sacdfs was started with
a small `LD_PRELOAD` shim (not part of sacdfs) that adds `O_DIRECT` to its opens of the
images: every read then goes to the server, which the NFS READ counts in
`/proc/self/mountstats` confirmed. *First touch:* a part of the image not read before, which
the NAS most likely reads from its disks; *NAS cache warm:* the same seek again.

**SMB** (#34): the emulated gigabit LAN of the server-side runs, the images on the SMB
server's NVMe disk. A kernel SMB mount needs root, which the test account doesn't have, so
sacdfs read the images through a `LD_PRELOAD` shim (measurement only) that sends its reads to
libsmbclient, one SMB READ per read and no client cache, as a `cache=none` mount does.

**Method:** every seek on a fresh mount, timing `open`, one 128 KiB `pread` and `close`, 3
rounds of 3 seeks, each round at other positions. *Cold:* an empty seek index. *Warm:* a
remount whose seek index the previous mount saved after the same seek.

| What | Target | Measured | Source | |
|---|---|---|---|---|
| NFS, cold seek, first touch | < 50 ms | **18.6 ms** plain DSD, **26.8 ms** DST (mean; worst single seek 37 ms) | #25 | met |
| NFS, cold seek, NAS cache warm | < 50 ms | 3.6 ms plain DSD, 4.5 ms DST | #25 | met |
| NFS, warm seek index | within 2x local + one round trip + the first read's transfer: about 2.9 ms plain DSD, 4.1 ms DST | **2.0 ms** plain DSD (image kept open), **2.7 ms** DST | #35, #25 | met |
| SMB, cold seek, server's cache evicted | < 80 ms | **5.2 ms** plain DSD, **6.1 ms** DST (worst 7.4 ms); on a NAS's hard disks an estimated 20 to 55 ms | #34 | met on SSD |
| SMB, warm seek index | the same formula: about 2.8 ms plain DSD, 4.9 ms DST | **3.4 ms** plain DSD, **3.6 ms** DST | #34 | met for DST; plain DSD measured before images were kept open (#35), which saves the open's round trip (0.7 ms here) |
| NFS, 4 players and a tagger | no read over 50 ms | tagger's worst file **7 to 13 ms**; players' 99th percentile **2.1 to 10.2 ms**; players' worst read 19 to 120 ms | #46 | met but for a player's first read of a part of the image the NAS reads from its disks (a single such read took up to 104 ms) |

The warm-seek target counts the time to move the first read's bytes (about 170 KiB for plain
DSD, 1.45 ms at gigabit speed), as #6 was revised to. A warm seek is one read of the image;
what remains is that transfer and the round trip.

## Calls and bytes per seek

What a cold seek costs in requests to the image, measured without FUSE
(`examples/seek_latency`): 200 seeks of 128 KiB spread over each area, each on a freshly
opened disc with an empty seek index (#25).

| Area | Calls per cold seek | KiB per cold seek |
|---|---|---|
| plain DSD stereo | 3.04 | 193 |
| DST stereo | 5.04 | 106 |
| DST 6 channels (same disc) | 4.90 | 159 |
| DSD stereo (hybrid disc) | 3.00 | 192 |
| DST 5 channels (same disc) | 5.58 | 118 |
| DST stereo, 2064-byte sectors | 5.11 | 104 |
| DST stereo, 2064-byte sectors (another disc) | 4.84 | 101 |
| **all** | **4.50** | **139** |
| first 128 KiB of every track (93 tracks) | 2.90 | 119 |
| warm: the same seek after a remount | 1.06 | 109 |
| warm: a seek into a track played through before a remount | 1.05 | 108 |

The seek index cache of these five discs, after every track was played and 1 400 cold seeks,
took about 130 KB.

**scarletbook's own seeking, for comparison** (#47; `examples/library_seek`, scarletbook 0.4.1,
the same positions): its cold seek took **8.84 calls and 373 KiB** per seek with its default
options (7.22 and 260 tuned), against sacdfs's 4.31 and 136; from a complete exact index
2.68 calls and 172 KiB (K = 64) against sacdfs's warm 1.01 and 103. Over NFS a cold seek took
21 to 29 ms through scarletbook against 7 to 15 ms with sacdfs's locator. Why sacdfs keeps its
own: [design](design.md#why-not-scarletbooks-seeking).

## Straight reads

`cat` (1 MiB reads) of every file of an area on a fresh mount, `--no-index-cache`, images in
the page cache (#42; the virtual machine 0.1 to 0.5 threads busy):

| Area | DSF / WAV bytes | DSF | WAV | WAV/DSF | sacdfs CPU for the WAV read |
|---|---|---|---|---|---|
| DST stereo, 50 min | 2.12 / 3.19 GB | 4.41 s | 4.79 s | 1.09 | 118 s |
| plain DSD stereo, 79 min | 3.34 / 5.01 GB | 1.73 s | 2.07 s | 1.20 | 34 s |
| DST 6 channels, 50 min | 6.36 / 9.55 GB | 13.40 s | 15.55 s | 1.16 | 378 s |

- A WAV file at 176.4 kHz holds 1.5 times the bytes of the DSF file. Plain DSD needs no
  decoding (2 s of CPU for the DSF read of 79 minutes, about 2 GB/s through FUSE), so the copy
  sets the pace; on DST areas decoding dominates, and conversion adds about 20 % CPU.
- With scarletbook 0.5.0 the DSF read of the DST stereo area took **4.33 s** (4.47 s with 0.4.1
  in the same session, #50), about 490 MB/s.
- Against scarletbook's own `bench decode` of the same area at the same thread count, on a
  machine loaded by the virtual machine (about 14 of 32 threads busy), sacdfs took 1.43 times
  as long (8.82 s against 6.17 s; #4).

## Converter throughput

The DSD-to-PCM converter alone: `cargo bench --bench pcm` (criterion, one frame at a time with
both neighbours, noise-like DSD), pinned to one core whose other hardware thread was idle, 3
rounds (#40; the virtual machine 0.2 to 4.5 threads busy). A generic x86-64 build, with each
of its three code paths forced by `SACDFS_SIMD`: *baseline* (SSE2), *v3* (AVX2, what runs on
an AVX2 CPU by default) and *v4* (AVX-512, built and tested but not the default). µs per frame
(1/75 s), and in brackets the multiple of real time per stereo stream; for 6 channels per
stereo pair.

| Ch | Filter | Rate | baseline | **v3 (default)** | v4 |
|---|---|---|---|---|---|
| 2 | 40 kHz | 88.2 kHz | 43.8 (304x) | **36.9 (361x)** | 49.6 (269x) |
| 2 | 40 kHz | 176.4 kHz | 54.9 (243x) | **43.7 (305x)** | 55.4 (241x) |
| 2 | 40 kHz | 352.8 kHz | 66.6 (200x) | **53.4 (250x)** | 59.8 (223x) |
| 2 | 30 kHz | 88.2 kHz | 66.5 (201x) | **51.7 (258x)** | 57.4 (232x) |
| 2 | 30 kHz | 176.4 kHz | 95.8 (139x) | **68.7 (194x)** | 74.2 (180x) |
| 2 | 30 kHz | 352.8 kHz | 108.1 (123x) | **78.8 (169x)** | 76.9 (173x) |
| 2 | wide 80 kHz | 176.4 kHz | 125.5 (106x) | **90.5 (147x)** | 98.0 (136x) |
| 2 | wide 80 kHz | 352.8 kHz | 117.9 (113x) | **86.5 (154x)** | 98.0 (136x) |
| 6 | 40 kHz | 88.2 kHz | 131.8 (303x) | **114.8 (348x)** | 136.6 (293x) |
| 6 | 40 kHz | 176.4 kHz | 165.3 (242x) | **130.8 (306x)** | 152.3 (263x) |
| 6 | 40 kHz | 352.8 kHz | 199.7 (200x) | **164.1 (244x)** | 184.7 (217x) |
| 6 | 30 kHz | 88.2 kHz | 198.2 (202x) | **149.6 (267x)** | 167.5 (239x) |
| 6 | 30 kHz | 176.4 kHz | 287.0 (139x) | **205.7 (194x)** | 216.4 (185x) |
| 6 | 30 kHz | 352.8 kHz | 328.9 (122x) | **236.8 (169x)** | 239.0 (167x) |
| 6 | wide 80 kHz | 176.4 kHz | 377.7 (106x) | **272.6 (147x)** | 285.2 (140x) |
| 6 | wide 80 kHz | 352.8 kHz | 355.7 (112x) | **258.8 (155x)** | 297.6 (134x) |

- Every path meets the 100x bar of #6, the SSE2 path included, and all give the same bytes.
- **v4 is slower than v3** in every case but one, and much slower with both hardware threads of
  the core converting, as the decoder pool does under load (176.4 kHz stereo, µs per frame on
  one thread: 40 kHz 71.9 v3 against 97.0 v4; 30 kHz 102.6 against 133.6; wide 142.8 against
  178.1). So v3 is the default even on this AVX-512 CPU.
- **Runs of frames** (#42): plain DSD frames are converted four at a time. The converter alone
  then costs about the same per frame (stereo, 176.4 kHz: 46.9 µs alone, 47.6 µs in runs of
  four); through the mount the gain is fewer, larger jobs on the pool: the WAV read of the plain
  DSD area took 15 % less time and 29 % less CPU.
- Making a converter (designing its filters) takes 27 to 55 µs, once per worker thread (#16).
- Where the time goes (the default filter at 176.4 kHz, #16): stage 3 about 40 %, stage 1 (the
  byte tables) about 30 %, stage 2 about 10 %, copying a channel out of the interleaved frame
  and writing the 24-bit samples about 7 % each.
- The 30 kHz filter is not a half-band, so its last stage costs about 2.5 times the default's;
  the wide filter at 176.4 kHz is the costliest (a 337-tap stage).

## Concurrency and memory

The scenario of #6, for 30 s: four players each reading 128 KiB at a time in real time with a
2 s buffer from a random position, and a tagger walking the whole mount over and over (`stat`,
the first 4 KiB, the tags), on a fresh mount with `--no-index-cache`; 3 rounds. Worst read and
99th percentile in ms; peak memory is sacdfs's peak resident set (`VmHWM`). From #46 (the
virtual machine 0.1 to 0.5 threads busy).

**Local images** (page cache):

| Players | Players' reads, worst / p99, per round | Tagger's worst file | Peak memory |
|---|---|---|---|
| four areas of three discs, WAV | 24.8 / 0.22, 23.0 / 0.21, 27.0 / 0.49 | 7.2 to 14.5 | 340 to 342 MiB |
| four tracks of one DST stereo area, WAV | 16.3 / 2.90, 15.1 / 2.19, 21.0 / 1.61 | 7.9 to 13.3 | 218 MiB |
| four tracks of one DST stereo area, DSF | 14.8 / 4.98, 10.7 / 4.77, 9.8 / 2.82 | 4.7 to 5.4 | 168 to 169 MiB |

**Images on NFS** (the share and shim of [client-side](#client-side)), DSF files of plain DSD
images; each set ran twice, in both orders of the two builds of #46, since the NAS caches what
the first read:

| Players | Players' reads, worst / p99, per round | Tagger's worst file |
|---|---|---|
| four tracks of one image | 103 / 3.4, 39 / 2.1, 21 / 3.2; 21 / 3.2, 49 / 10.2, 87 / 4.8 | 7.2 to 12.1 |
| four images | 24 / 10.2, 20 / 3.4, 120 / 10.7; 103 / 31.7, 84 / 3.2, 19 / 3.7 | 9.0 to 12.7 |

- The worst reads on NFS are each player's first read of a part of the image the NAS must
  read from its hard disks; a single such read took up to 104 ms. Before #46, a tagger's or
  another player's read also waited behind those.
- Peak memory on NFS: 144 to 146 MiB.
- Memory stays bounded by `--cache-mb` (default 256 MiB) plus a fixed amount: decoding runs
  on sacdfs's own pool, and no library cursor holds a read-ahead window of its own.

## Mount, listing and tag-walk cost

| What | Measured | Source |
|---|---|---|
| Mount of a library on an NFS share (138 images in 216 directories; `sacdfs` returns once mounted, NFS attribute cache warm) | **101 ms** (median of 6); of which building the index of inode keys 0.15 ms | #33 |
| Fresh mount of the five discs, walk and `stat` of all 198 entries (the first lookups, which open the images and read their TOCs) | **10.8 ms** (median of 15) | #33 |
| `stat` of all 186 files (93 DSF, 93 WAV) on a fresh mount | **10 ms** | #15 |
| Header and tag reads of all 186 files, as a tagger walks the chunks | **84 ms**, 0.16 s of sacdfs CPU, 20.5 MB of image reads, all of them for DSF headers | #15 |
| DSF tag walk of the 93 DSF files (open, first 4 KiB, the tag, close) | **0.07 s**: 342 calls to the image (3.7 a file), 20.7 MB (222 KB a file) | #36 |

Mounting reads no image: only the names of the source tree are walked. An image's TOCs are read
when its directory is first listed. A library of 10 000 images would add a few milliseconds of
key computation to a mount whose walk of the names takes seconds on a network share.

## History

What each improvement changed, one line each, with the pull request that measured it.

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
