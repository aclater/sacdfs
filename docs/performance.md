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
| DSD-to-PCM conversion, stereo, 176.4 kHz, default filter, one core (#6) | at least 100x real time | **209x** generic x86-64, **264x** x86-64-v3 ([below](#pcm-conversion)) | met |
| Straight read of WAV files vs the same area's DSF files (#6) | within 1.5x | **1.09x** to **1.37x** ([below](#wav-files-6-7-8)) | met |
| WAV: first byte at a random offset, fresh mount (#6) | under 20 ms | **1.8 ms** | met |
| WAV: 4 players and a tagger (#6) | no read over 50 ms | worst **30.6 ms** | met |

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
| Client-side, warm seek index | within 2x local + one round trip | 2.7 ms against 2 x 1.4 + 0.4 = 3.2 ms (DST); plain DSD 2.0 ms with images kept open (#26, [below](#keeping-images-open-26); 2.7 ms before) against 2 x 0.5 + 0.4 = 1.4 ms: not met, since moving the 170 KiB it reads takes about 1.45 ms on gigabit Ethernet, and even 128 KiB take 1.1 ms | met for DST |
| Client-side, images on SMB, cold | < 80 ms | 5.2 / 6.1 ms (plain DSD / DST, emulated gigabit LAN, server's disk an NVMe SSD; [below](#seeks-over-smb-issue-27)) | met, but not yet on a NAS's hard disks |

**SMB** is measured [below](#seeks-over-smb-issue-27).

## Seeks over SMB (issue #27)

Measured on 2026-10-31 on the same machine, under the benchmark lock, with the virtual machine
on the host nearly idle (0.2 of a hardware thread busy during the client-side run, 1.3 during
the server-side run, from its CPU time). Same five real discs as above; the figures use the
plain DSD stereo disc and the DST stereo disc.

**Setup.** Both ends run in containers on this machine (rootless Podman, one bridge network):
- *Server:* the image of [deployment.md](deployment.md) (Samba 4.24, SMB 3.1.1), sharing
  either the image files themselves (client-side runs) or its sacdfs mount (server-side runs).
  The images are on a local NVMe SSD.
- *Network:* `tc netem` on both containers' interfaces, 0.19 ms each way at 1 Gbit/s,
  emulates a gigabit LAN. A TCP connect then takes 0.67 ms (0.03 ms without netem), a little
  slower than the LAN of the NFS figures above (ping 0.38 ms). "Loopback" is the same without
  netem.
- *Client:* sacdfs (client-side) or a player-like reader (server-side).

**Why no `mount.cifs`.** A kernel SMB mount needs real root, which this machine's test account
doesn't have (rootless Podman can't mount CIFS). Client-side, sacdfs reads the images instead
through a small `LD_PRELOAD` shim, used only for this measurement and not part of sacdfs:
- it sends sacdfs's `open`, `read`, `lseek` and `close` on the image files to libsmbclient;
- `stat` sees a local sparse file of the same size;
- each `read` sacdfs makes is one SMB READ request, with no client cache.

That is what a kernel mount with `cache=none` does, and what #25 forced on NFS with `O_DIRECT`.
The CI job (`container` workflow) shows that kernel CIFS and NFS mounts of the share work.

**Method,** as for NFS above: every seek is on a fresh mount, timing `open`, one 128 KiB
`pread` and `close`.
- *Cold:* an empty seek index. *Warm:* a remount whose seek index the previous mount saved
  after the same seek.
- 3 rounds of 3 seeks per disc, tracks 5, 9 and 12, at a different position in each round
  (37/81/55 %, 21/63/44 %, 72/15/90 %). Each figure is the mean of the round means.
- *Server cache evicted:* the image files dropped from the server's page cache
  (`posix_fadvise(POSIX_FADV_DONTNEED)`) before the cold seek, so its reads come from the SSD.
  *Warm:* a repeat of the same seek with nothing evicted.

### Client-side: sacdfs on the client, images on the SMB share

| Time to the first 128 KiB, ms | cold, server cache evicted | cold, server cache warm | warm index | calls to the image, cold / warm | SMB READs per mount, cold / warm |
|---|---|---|---|---|---|
| local, plain DSD (reference, same harness) | 1.12 | 0.36 | 0.33 to 0.39 | 3 / 1 | |
| local, DST (reference) | 2.01 | 1.76 | 1.47 to 1.83 | 5 / 1 | |
| SMB loopback, plain DSD | 1.69 | 1.67 | 0.98 to 1.00 | 3 / 1 | 9 / 7 |
| SMB loopback, DST | 2.03 | 1.66 | 1.43 to 1.53 | 5 / 1 | 13 / 9 |
| **SMB gigabit LAN, plain DSD** | **5.21** (worst 5.55) | 4.48 | **3.34 to 3.41** | 3 / 1 | 9 / 7 |
| **SMB gigabit LAN, DST** | **6.10** (worst 7.44) | 5.52 | **3.52 to 3.71** | 5 / 1 | 13 / 9 |

- "Calls" are sacdfs's reads of the image for the seek, from its exit log. The SMB READs
  include the 6 to 8 reads of each mount's TOCs, which come before the seek.
- Calls are the same as over NFS (#25: 3 for plain DSD, 5.7 for DST cold, 1 warm): each
  call is one request, whatever the protocol.
- On the emulated LAN a cold seek costs about 1.0 to 1.4 ms per call over the local seek: one
  round trip plus moving the call's bytes (35 to 64 KiB at gigabit speed).
- A warm seek is opening the image over SMB (one round trip) plus one read of about 170 KiB
  (1.4 ms at gigabit speed), as on NFS.
- An earlier full run gave the same LAN figures within 0.1 ms (5.32 / 6.12 cold, evicted;
  3.36 to 3.61 warm).

These figures predate keeping image files open between reads (#26, `--max-open-images`):
the warm seeks include opening the image over SMB, about one round trip (0.7 ms here),
which a mount that already has the image open no longer pays.

**Not measured: a NAS with hard disks.** On the NFS NAS above, a cold seek into a part of a disc
it had not read recently ("first touch") cost 18.6 / 26.8 ms, mostly the NAS's disk seeks:
about one per call. Over SMB the same disk seeks come on top of the figures here. With 3 to 5
calls at roughly 5 to 10 ms each, that is about 20 to 55 ms, still within the < 80 ms target,
but only an estimate until it is measured on such a NAS (#21).

### Server-side: sacdfs on the server, a client reads the DSF and WAV files over SMB

The client (libsmbclient, as a player would) connects and stats the file first, as a player
listing the folder does. Then it times `open`, a seek, one 128 KiB read and `close`. Each
seek is on a freshly started container, so sacdfs's mount, frame cache and seek index
(`SACDFS_INDEX_CACHE=off`) are all cold.

| Time to the first 128 KiB, ms | DSF, cache evicted | DSF, cache warm | WAV, cache evicted | WAV, cache warm |
|---|---|---|---|---|
| loopback, plain DSD | 1.58 | 0.79 | 2.03 | 1.24 |
| loopback, DST | 2.17 | 1.74 | 2.36 | 2.00 |
| **gigabit LAN, plain DSD** | **4.68** (worst 5.33) | 3.17 | **5.14** (worst 5.53) | 3.95 |
| **gigabit LAN, DST** | **5.35** (worst 6.07) | 4.96 | **5.48** (worst 6.20) | 5.06 |

- On the LAN, the open takes 0.6 ms (one round trip) and the read 2 to 4.4 ms.
- The read is sacdfs's cold seek on the server's own disk (the loopback figures, 0.8 to
  2.4 ms) plus a round trip and moving 128 KiB at gigabit speed (about 1.1 ms).
- The image reads stay on the server, so a NAS with hard disks adds its disk seeks to the
  server's time only. That is about 3 to 5 seeks for a cold seek, which client-side SMB pays
  the same way, plus one round trip each.

### Against the targets of issue #6

| Deployment | Target | Measured (emulated gigabit LAN, server disk an SSD) | |
|---|---|---|---|
| Server-side, client over SMB | < 25 ms | 4.7 to 5.5 ms cold (DSF and WAV); worst 6.2 ms | met |
| Client-side, images on SMB, cold | < 80 ms | 5.2 / 6.1 ms (plain DSD / DST); worst 7.4 ms | met here; on hard disks, an estimated 20 to 55 ms |
| Client-side, warm seek index | within 2x local + one round trip | DST: 3.6 ms against 2 x 1.65 + 0.67 = 4.0 ms. Plain DSD: 3.4 ms against 2 x 0.36 + 0.67 = 1.4 ms | met for DST, not for plain DSD, as on NFS: the open and one 170 KiB transfer take longer than a local seek |

**Reproducing.** Build the image ([deployment.md](deployment.md)). Then run a Samba container
on the image directory, and a client container with `--cap-add NET_ADMIN` that applies
`tc qdisc add dev eth0 root netem delay 0.19ms rate 1gbit`, as the server does. The rest is
as above. With root, a kernel mount
(`mount -t cifs //server/share /mnt -o ro,guest,cache=none`) replaces the shim.

### Keeping images open (#26)

Measured on 2026-10-31 as above (fresh mount, `O_DIRECT` shim, the NFS READ counts), on the
same three plain DSD stereo images on the NAS, 3 rounds of one seek per image, each round at
another position; the virtual machine on the host used 5 s of CPU over the 40 s run (about
0.1 hardware thread). "Warm" is a remount whose seek index cache a cold seek at the same
position just saved, so the NAS has those sectors cached. "Before" is `main`, "after" keeps
the handle the TOC was read through (the listing just before the seek) instead of opening
the image again on the first audio read.

| Time to the first 128 KiB, ms (mean; min to max) | before | after |
|---|---|---|
| NFS, plain DSD, warm seek index | 3.05 (2.50 to 3.65) | **2.02** (1.82 to 2.29) |
| the same, 2 s between the listing and the seek | 4.59 (3.96 to 5.19) | **3.38** (3.06 to 3.99) |

Both make one call to the image (170 KiB) and the same 7 NFS READs (6 of them the TOCs);
the gain is the open, about 1.0 to 1.2 ms. A warm seek is now the one read: 170 KiB take
about 1.45 ms on gigabit Ethernet, plus a round trip (0.35 to 0.45 ms by ping), which is
what remains. The pause costs both builds the same 1.3 ms; locally (the plain
DSD disc, page cache, 3 seeks each) a 2 s pause adds 0.2 to 0.4 ms (mean 0.43 to 0.84 ms
before, 0.47 to 0.65 ms after), so most of it is the network and the NAS waking up, not
sacdfs: the handle check once a second is a `stat` that this share (mounted `nocto`,
attributes cached 600 s) answers locally.

**Against #6** (warm seek index within 2x local plus one round trip, 2 x 0.5 + 0.4 = 1.4 ms
for plain DSD): **not met, and not reachable on gigabit Ethernet.** The 128 KiB the read
returns are plain DSD, so at least 128 KiB of the image must cross the network: 1.1 ms of
wire time at about 117 MB/s, plus the round trip, is 1.5 ms before sacdfs, FUSE or the NAS
do anything. The target's formula leaves out transfer time, which on a local disk is
negligible and over a network is not. The read could be about 30 KiB smaller (it covers two
frames and the search's stop distance more than a 128 KiB read needs, which a cursor started
at a known point does not need; a possible follow-up), which would save about 0.25 ms.

## Concurrent reads (#28)

Measured on 2026-10-31 under the benchmark lock, the virtual machine on the host about idle
(0.1 to 0.5 hardware threads, once 6.2), release builds. "Before" is `main` with one
`Mutex<Image>` per open image; "after" reads through positional reads, an `Image` clone per
read ([design](design.md#concurrent-reads-of-one-image)). The scenario of #6: for 30 s, four
players each reading 128 KiB at a time in real time with a 2 s buffer from a random position,
and a tagger walking the whole mount over and over (stat, the first 4 KiB, the tags), on a
fresh mount with `--no-index-cache`; 3 rounds each, worst read / 99th percentile in ms. Peak
memory is the sacdfs process's peak resident set (`VmHWM`) at the end of the round.

**Local images** (page cache), the five discs; "different": players on four areas of three
discs; "same": four players on four tracks of the DST stereo area.

| | Players, before | Players, after | Tagger worst, before / after | Peak RSS before / after |
|---|---|---|---|---|
| different discs, WAV | 16.0 / 0.71, 17.8 / 0.25, 28.8 / 0.58 | 24.8 / 0.22, 23.0 / 0.21, 27.0 / 0.49 | 5.8 to 11.7 / 7.2 to 14.5 | 339 to 343 / 340 to 342 MiB |
| same disc, WAV | 21.5 / 2.71, 19.7 / 1.51, 14.3 / 1.84 | 16.3 / 2.90, 15.1 / 2.19, 21.0 / 1.61 | 6.7 to 16.5 / 7.9 to 13.3 | 218 to 219 / 218 MiB |
| same disc, DSF | 11.5 / 5.62, 12.5 / 4.00, 16.6 / 3.55 | 14.8 / 4.98, 10.7 / 4.77, 9.8 / 2.82 | 5.8 to 10.4 / 4.7 to 5.4 | 168 to 169 / 168 to 169 MiB |

Locally both meet the #6 target (no read over 50 ms) and are equal within the noise: a read
from the page cache held the lock only for a copy of up to 128 KiB.

**Images on NFS** (the share of [client-side seek latency](#client-side-seek-latency-issue-20),
with the `O_DIRECT` shim, so every read goes to the server), DSF files of plain DSD stereo
images: "same": four players on four tracks of one image; "different": four players on four
images. Each set ran twice, before then after (A), and after then before at other positions
(B), since the NAS caches what the first build read.

| | Players worst / p99, before | after | Tagger worst, before | after |
|---|---|---|---|---|
| same image, A | 155 / 118, 135 / 35, 110 / 38 | 103 / 3.4, 39 / 2.1, 21 / 3.2 | 163, 85, 134 | 7.2, 10.8, 12.0 |
| same image, B | 76 / 12.8, 88 / 34.9, 55 / 20.3 | 21 / 3.2, 49 / 10.2, 87 / 4.8 | 120, 89, 46 | 12.1, 7.2, 8.6 |
| different images, A | 133 / 21.5, 133 / 3.7, 87 / 13.1 | 24 / 10.2, 20 / 3.4, 120 / 10.7 | 64, 38, 104 | 10.0, 11.6, 9.2 |
| different images, B | 154 / 9.8, 41 / 2.5, 20 / 2.7 | 103 / 31.7, 84 / 3.2, 19 / 3.7 | 61, 66, 51 | 9.0, 12.7, 11.9 |

- **The tagger:** its worst file went from 38 to 163 ms to 7 to 13 ms in every round. Its
  reads of a disc a player was reading waited for the player's round trips; now they run
  beside them.
- **Players on one image:** the 99th percentile went from 12.8 to 118 ms to 2.1 to 10.2 ms.
  The worst reads after (up to 103 ms) are each player's first reads at a part of the image
  the NAS must read from its disks (a single such read took up to 104 ms in both builds); before,
  every player also queued behind those.
- **Players on different images** did not share a lock before either, and are the same within
  the noise; the worst reads are first touches on the NAS.
- **Memory:** unchanged (144 to 146 MiB on NFS, figures above locally). Decoding stays on
  sacdfs's own pool and scarletbook is built without its `parallel` feature, so no library
  cursor holds a read-ahead window of its own (up to about 48 MB with it, scarletbook 0.4.0).

The players and the tagger are those of the WAV benchmarks' concurrency scenario
[below](#wav-files-6-7-8), with the four players on one area for "same".

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

The WAV files of the same discs (2026-10-31, with the converter of #16):
- All 93 equal their positions of a reference stream of the whole area, converted frame by
  frame with a fresh converter (`real_discs` in `tests/wav.rs`, compared by SHA-256). That is
  every join between tracks (gapless). 16 random ranges of up to 3 MiB per file equal the
  straight read.
- FFmpeg 8.1 (`ffprobe`) and Python's `wave` read all 93 at 88.2, 176.4 and 352.8 kHz, and with
  `--pcm-cutoff wide` at 352.8 kHz: duration, rate, channels, layout (stereo, 5.0, 5.1),
  `pcm_s24le` and tags. libsndfile reads all 457 WAV files of the corpus, its samples equal to
  the raw bytes.
- An RF64 file (no local track is long enough, so an ignored test writes a sparse 4.76 GB
  six-channel file with sacdfs's header and trailer code) reads in FFmpeg as 1500 s, 5.1, and
  in libsndfile as RF64 PCM_24.
- Peaks of a DST stereo track are about -5.6 dBFS at 0 dB gain, as expected for SACD's
  reference level, with no clipped samples logged.

## PCM conversion

The DSD-to-PCM converter alone (phase 1 of #11; the WAV files are [below](#wav-files-6-7-8)), measured 2026-10-31 with
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

## WAV files (#6, #7, #8)

Measured on 2026-10-31 with the WAV files of this change (`--format dsf,wav`, the defaults:
176.4 kHz, 24-bit, 40 kHz filter, 0 dB, 256 MiB cache, one decoder thread per CPU), release
build for generic x86-64 (`-C target-cpu=x86-64`, what most distributions build), on the
same 16-core (32-thread) machine, images in the page cache, under the benchmark lock, with
`--no-index-cache` so that every mount starts cold. Means of 3.

**Machine load.** The virtual machine on the host was mostly idle this time: 0.1 to 2.6 of
the 32 hardware threads busy during most runs (from its CPU time, `/proc/<pid>/stat` fields 14
and 15), but 9.0 and 3.6 during the last two six-channel runs; the slowest DSF run (18.7 s)
is the one with 9.0 threads busy. The ratios below include it.

| What (#6) | Target | Measured | |
|---|---|---|---|
| Straight read of a WAV area vs the same area's DSF files, through the mount | within 1.5x | **1.09x** DST stereo, **1.37x** plain DSD stereo, **1.10x** DST 6 channels | met |
| First byte at a random offset, fresh mount, page cache | under 20 ms | **1.8 ms** (WAV), 1.6 ms (DSF) | met |
| `ls -l`, `stat`, header and tag reads | no decoding or conversion | WAV: none; 186 files' headers and tags in 84 ms with 0.16 s of CPU in all | met |
| 4 players and a tagger walking the library | no read over 50 ms | worst read **30.6 ms** (players), 19.0 ms (tagger); p99 0.7 ms | met |

**Straight read.** `cat` (1 MiB reads) of every file of an area, DSF then WAV, each on a fresh
mount:

| Area | Bytes DSF / WAV | DSF (s) | WAV (s) | WAV/DSF | sacdfs CPU DSF / WAV |
|---|---|---|---|---|---|
| DST stereo, 50 min | 2.12 / 3.19 GB | 4.52, 4.75, 4.55: **4.61** | 4.99, 4.89, 5.14: **5.00** | **1.09** | 102 / 124 s |
| plain DSD stereo, 79 min | 3.34 / 5.01 GB | 1.71, 1.74, 1.70: **1.72** | 2.37, 2.33, 2.35: **2.35** | **1.37** | 2 / 49 s |
| DST 6 channels, 50 min | 6.36 / 9.55 GB | 13.71, 13.97, 18.70: **15.46** | 16.61, 16.26, 17.93: **16.94** | **1.10** | 318 / 397 s |

A WAV file at 176.4 kHz holds 1.5 times the bytes of the DSF file, so where reading DSF costs
nothing but copying (plain DSD: 2 s of CPU for 79 minutes, about 2 GB/s through FUSE), the
copy alone puts WAV near 1.5x; the conversion is hidden behind it. On DST areas decoding
dominates either way, and the conversion adds about 20 % CPU (22 s for the stereo area at
about 0.12 s per minute of stereo audio).

How the plain DSD figure got there (same method, means of 3, WAV time with DSF at about
1.65 s): demultiplex, then convert, 2 s read-ahead: 3.42 s (2.05x); with the
demultiplex-and-convert pipeline: 2.90 s; and an 8 MiB read-ahead window: 2.29 s (1.39x). The
first pipeline version also decoded DST 16 frames at a time and made DST reads 1.79x and
1.92x; DST frames are now decoded in one parallel batch (see the
[design](design.md#reading-a-range-of-a-wav-file)).

**Seek.** A fresh mount each time, then `open`, one 128 KiB `pread` and `close`, at 37 %, 81 %
and 55 % of three tracks of the DST stereo disc, 3 rounds: WAV 2.3, 1.6, 1.7, 1.7, 2.0, 1.6,
2.2, 1.7, 1.8 ms (mean **1.8 ms**, worst 2.3 ms); DSF mean 1.6 ms. A WAV seek decodes two DSD
frames more than the DSF one (its neighbours) and converts about 10 frames.

**Metadata and tags.** A fresh mount: `os.walk` with `stat` of all 186 files of the five
discs (93 DSF, 93 WAV) in 10 ms; then, for every file, a chunk walk like mutagen's (header,
chunk headers, the bodies of `LIST` and `id3 `) or the DSF tag: 84 ms in all, 0.16 s of sacdfs
CPU, and 20.5 MB of image reads, all from DSF files (a DSF header read still decodes its first
frames, since its audio starts at byte 92; see [below](#dsf-header-reads-32)). Before the padding and read-ahead fixes in this
change, the same walk read 388 MB of the images and took 28 s of CPU.

**Concurrency.** For 30 s, four players on four different areas (DST stereo, plain DSD
stereo, DST 6 channels, DSD+DST 5 channels), each starting at a random position and reading
128 KiB at a time in real time with a 2 s buffer, while a tagger walks the whole mount over and
over (stat, the first 4 KiB, the tags). Per round, worst / 99th percentile:

| Round | Players' reads | First read of each player | Tagger's files | VM threads busy |
|---|---|---|---|---|
| 1 | 25.4 / 0.19 ms (1936 reads) | 6.8 ms | 19.0 / 0.13 ms (675 180) | 5.0 |
| 2 | 20.9 / 0.20 ms | 12.1 ms | 15.2 / 0.07 ms (843 138) | 0.3 |
| 3 | 30.6 / 0.70 ms | 7.5 ms | 15.7 / 0.06 ms (864 342) | 0.1 |

With the 8 MiB read-ahead for every reader, and before the first-audio-read rule, the
players' worst reads were 114, 51 and 48 ms: a tagger touching each file started 8 s of
conversion, and players converted in long bursts on the shared pool. A reader now gets the
long window only when it reads far faster than real time.

**Reproducing.** The same scenarios as the DSF ones above, with the `.wav` files:
`cat MOUNT/<disc>/Stereo/*.wav > /dev/null` on a fresh mount; one `pread` of 128 KiB on a
fresh mount for the seek; under `benchlock`, with `--no-index-cache`.

## Persistent file identity (#18)

The cost of computing inode numbers from paths, against `main` before the change. Measured on
a library on an NFS share (138 images in 216 directories), release builds, under the bench
lock.

| | before | after |
|---|---|---|
| Mount (`sacdfs` returns once mounted, warm NFS cache), median of 6, alternating | 95 ms | 101 ms |
| of which: building the index of keys (`Ids::new`, measured alone) | - | 0.15 ms (354 entries, about 0.4 µs each) |
| Fresh mount of five real discs, walk and `stat` of all 198 entries (first lookups, which open the images), median of 15, two rounds | 10.4 / 10.9 ms | 10.8 / 11.0 ms |

- The index itself costs 0.15 ms for the whole library: 0.15 % of the 100 ms that walking the
  names over NFS takes. The 6 ms between the two mount medians is not in the index; it is
  within the run-to-run spread of the mount (the first, cold run took 4.9 s and 0.9 s).
  Extrapolated, a library of 10,000 images adds a few milliseconds to a mount that walks
  their names in seconds.
- First lookups are as fast as before within the noise: they are dominated by reading the
  images' TOCs, and the hash-map node table and the generation (one SHA-256 per file lookup)
  cost well under a microsecond each.
- The DSF gate on the five real discs is 93/93 with the change.

## DSF header reads (#32)

Measured on 2026-10-31, the five discs mounted from a local disk (`--format dsf`,
`--no-index-cache`), under the benchmark lock, the virtual machine on the host about idle
(0.1 to 0.4 hardware threads). A tag walk over the 93 DSF files on a fresh mount, as a
tagger does it: open, read the first 4 KiB, seek to the ID3 tag the header points to, read
it, close. The kernel's requests come from fuser's debug log; the image reads from
`-v`'s exit line.

| | before | after |
|---|---|---|
| kernel requests | 279: per file 128 KiB at 0, 128 KiB at 128 KiB, the tag's page | the same |
| calls to the image | 1 022 (11.0 a file) | **342** (3.7 a file) |
| bytes of the image | 109.5 MB (1.18 MB a file) | **20.7 MB** (222 KB a file) |
| time for the walk | 0.26 s | **0.07 s** |

The first 4 KiB alone cost nearly all of it (1 016 calls, 108.7 MB before; 341, 20.6 MB
after): the tag's page holds only the last 92 bytes of the last channel's last block, which
are padding unless that block is nearly full. Before, the kernel's second request (its read-ahead of the first) counted as a
sequential read and started sacdfs's 2 s read-ahead: about 950 KB a file. The 222 KB left
are the frames in the kernel's 256 KiB, read at the track's start; on a network share that
is about 3.7 round trips a file with an empty seek index. Why that rest stays: [design](design.md#reading-a-range-of-a-dsf-file),
read-ahead. With `max_readahead` at 4, 16 or 64 KiB the kernel's requests did not change
(the same 279 requests, 24 180 KiB); at 0 it sent 3 069 requests of 4 KiB.

The guarantees, with the same scenarios as [above](#wav-files-6-7-8) (before / after, 3
rounds): seek on a fresh mount, DSF 1.4 / 1.6 ms, WAV 1.8 / 1.9 ms (within the run-to-run
noise); header and tag walk of all 186 files with the files closed in between, as there,
20.5 MB of image reads both (the kernel's background request dies with the closed file);
four players and a tagger, players' worst read 23.0, 18.3, 17.9 / 25.0, 24.7, 27.4 ms
(target 50 ms), the tagger's 8.8 to 9.6 / 10.6 to 13.2 ms. Straight read, `cat` of an area's DSF
files on a fresh mount: plain DSD stereo 1.98, 1.57, 1.59 / 1.61, 1.70, 1.57 s (mean 1.71 /
1.63 s), DST stereo 4.19, 4.18, 4.60 / 4.35, 4.56, 4.39 s (mean 4.32 / 4.43 s), within the
noise. The DSF gate: 93 of 93 tracks identical.

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
