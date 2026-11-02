# Clients: what sacdfs has been tested with

This page lists the music servers, players and file tools that sacdfs has been tried with, the
version of each, how it was connected, what happened, and how to set sacdfs and the client up.
Everything here comes from the client-test reports on
[#82](https://github.com/aclater/sacdfs/issues/82) and the issues linked below; a result is
only stated where a test showed it, and a setting is only recommended where a test on the
version named ran with it. "Untested", "partial" and "not validated" mean what they say. For
the container, shares and mounting, see the [deployment guide](deployment.md); for the
options, the [user guide](user-guide.md#options).

The test library was small except where noted: 5 images (plain DSD, DST, hybrid, DST
stereo-only, raw), 93 tracks, so 186 files with `--format dsf,wav` (20 tracks of each format
are multichannel). No file was over 4 GiB, so RF64 WAV was not exercised.

## Summary

| Client | Version tested | sacdfs tested | Connection | Result | Notes |
|---|---|---|---|---|---|
| [Roon](#roon) | Roon 2.x (Roon Server official image) | 0.1.1, 0.2.0-rc.1 to rc.4, 0.2.0 final | SMB, same NAS | Works (0.2.0) | Use `dsf` only; needs the SMB-mount rights; art and playlists work; picks up upgrades by itself; multichannel areas are separate albums |
| [Plex](#plex) | Plex Media Server 1.43.5 | 0.1.1, 0.2.0-rc.4 | SMB (CIFS, read-only), same NAS; container with an `rslave` bind | Works with settings | 406 of 406 tracks found; turn on "Prefer local metadata"; folder art not used ([#95](https://github.com/aclater/sacdfs/issues/95)) |
| [Jellyfin](#jellyfin) | 12.4.0 (official image) | 0.1.1, 0.3.0-rc.1 | Container with an `rslave` bind of the mount's parent | Works with settings | 93 of 93 (`dsf`), 186 of 186 (`dsf,wav`); use `dsf`; leave LUFS normalization off; folder art used; multichannel areas are separate albums |
| [Navidrome](#navidrome) | 0.64.2 | 0.1.1, 0.3.0-rc.1 | Container with an `rslave` bind of the mount's parent | Works | 93 of 93 (`dsf`), 186 of 186 (`dsf,wav`); folder art used; multichannel areas are separate albums |
| [Lyrion / Logitech Media Server](#lyrion-logitech-media-server) | 9.1.1 | 0.1.1, 0.3.0-rc.1 | Container with an `rslave` bind of the mount's parent | Works with settings | 93 of 93 (`dsf`), 186 of 186 (`dsf,wav`); folder art used; needs a DSD-capable player; run LMS as root on a direct mount |
| [MPD](#mpd-moode-volumio) (so moOde, Volumio) | 0.24.15 | 0.1.1, 0.3.0-rc.1 | Container with an `rslave` bind of the mount's parent | Works with settings | Use `dsf`: the first scan is about 10 times faster; plays DSD natively; art only from files named `cover.*` |
| [MinimServer](#minimserver) | 2.2, update 275 | 0.1.1, 0.3.0-rc.1 | Container with an `rslave` bind of the mount's parent | Works | 93 of 93; folder art used; no transcoding without MinimStreamer |
| [Windows Explorer, PowerShell, Robocopy](#windows-explorer-and-robocopy) | Windows 11 build 26200 | 0.2.0-rc.1 | SMB 3, same host | Works | Copies are byte-identical; a second Robocopy pass skips every file |
| [foobar2000](#foobar2000) | 2.26.2 (portable) | 0.2.0-rc.1 | SMB 3 | Partial | Opened a DSF without errors; tags, art and seek not verified |
| [JRiver Media Center](#jriver-media-center) | 36 | none | SMB 3 | Untested | The trial was not run |
| [Linux NFS and SMB mounts](#linux-nfs-and-smb-mounts) | kernel clients | see [deployment](deployment.md) and [performance](performance.md) | NFSv4, SMB | Works | The container's own share tests; see below |

## How the 0.3.0-rc.1 tests were run

The Jellyfin, Navidrome, LMS, MPD and MinimServer results below were measured on
sacdfs 0.3.0-rc.1 (the release binary), on a test host with a 14-core Intel CPU. For each
server:

- sacdfs ran on the host as the user, with `--format dsf --flatten --cache-mb 16 --threads 4`
  (and `--format dsf,wav` where that is compared), and a fresh index cache for each run;
- the server ran in a rootless Podman container that bound the mount's **parent** directory
  with `ro,rslave`, as recommended under [General recommendations](#general-recommendations);
- the library was the 5 test images, laid out `Artist/Album/disc.iso`: 93 tracks in 7 album
  areas (5 Stereo, 2 Multichannel). 3 of the 5 discs have a cover beside the image (two named
  `folder.jpg`, one `cover.jpg`), so 4 of the 7 album areas have a cover, which sacdfs also
  places inside each area folder;
- scans were timed with sacdfs's FUSE request log on (`RUST_LOG=fuser::request=debug`), which
  is how opens, reads and bytes served were counted.

A pre-release build of `main` (commit 85316b6) gave the same counts and art results, and
times within a few percent.

## Roon

**Tested.** Roon Server's official image as a TrueNAS app on the same NAS as sacdfs, with the
share added as a network share (SMB). Roon imported folder art and `Artwork/` trees, and a
relative-path playlist with all of its entries. Re-checks of 0.2.0 release candidates on a NAS
found two Roon-visible bugs, both fixed in 0.2.0: Roon saw no change after an upgrade or a new
sidecar because folder times did not move ([#123](https://github.com/aclater/sacdfs/issues/123)),
and sidecars added after the mount were not listed ([#101](https://github.com/aclater/sacdfs/issues/101)).
**Final results on 0.2.0** (Roon 2.x, NAS container, SMB share): a relative-path `.m3u8` added
next to an image was listed at once and imported by a Force Rescan (adds 51, 5,820 files seen,
2.6 s); Roon's periodic scan (every 4 hours) picked up the 0.1 to 0.2 change unprompted
(3,211 modifications and 446 adds, the new per-area art); it shows 46 multichannel areas as
separate albums, under its own canonical titles, so the suffix is not visible there (Roon
already split them before [#97](https://github.com/aclater/sacdfs/issues/97)); and playback
continued through a sacdfs restart. A library import of 271 images and 3,216 tracks was replayed against 0.1.0 for #73's memory
figures; the deployment guide's figures are now the replay of #150 over the bench images.

**Recommended setup.**
- `SACDFS_FORMATS=dsf` (`--format dsf`): with both formats every track appears twice. Roon
  plays DSD and converts it itself.
- `SACDFS_FLATTEN=1`; the default `SACDFS_CACHE_MB` (32 since #150; these tests ran with 16,
  which also works).
- Give sacdfs a fixed address on a Docker network of its own so that Roon can reach SMB on
  port 445, and give Roon's container `cap_add: [SYS_ADMIN, DAC_READ_SEARCH]` and
  `security_opt: [apparmor=unconfined]` so it may mount the share. The
  [deployment guide](deployment.md#roon-on-the-same-nas) has the Compose file.
- Write playlist entries as paths relative to the share.
- Remove extracts of the same discs, or Roon may play either copy.
- Roon's audio analysis reads every track in full: expect a CPU burst after adding a large
  library, or lower the analysis speed in Roon.

**Limits.** After upgrading to 0.2.0, an existing Roon library sees every share track as
modified once (the files' output version changed). Roon decides what to rescan from folder
times; see [restarts and rescans](#restarts-and-rescans).

## Plex

**Tested.**
- *0.1.1*, a real install on the NAS over SMB, 35 images (32 SACD), 406 of 406 tracks found
  (343 two-channel and 63 six-channel), codec shown as DSD, durations within 12 ms of the
  TOC, no sacdfs-related warnings. Scan 50 s (125 s until quiet) at the default cache;
  45 s with `--cache-mb 16`. sacdfs RSS at the end 549 MB at 256 MiB against 126 MB at 16 MiB.
  First byte of a direct stream in 16 to 26 ms, seeks 2 to 6 ms; a transcode to MP3 started
  in 0.03 to 0.73 s. A sacdfs restart mid-test was recovered by Plex's CIFS mount by itself.
- *0.2.0-rc.4*, the whole tree (291 images) with `--cache-mb 16`, over SMB: scan about 725 s,
  2,080 CPU-s in sacdfs, peak RSS 86.5 MiB, 3,216 tracks in 309 albums, 8 warnings (all
  "missing frames (filled)"), no errors.
- A container reading the host's mount: its nightly deep analysis read every file once (55 GiB
  in 874 s, RSS 701 MiB).

**Recommended setup.**
- `--cache-mb 16`.
- Turn on **Prefer local metadata** (library settings, Advanced) or use the "Plex Personal
  Media" agent. Without it Plex's default agent gives both copies of a disc the online
  release's title and shows them as two albums with the same name; with it, "Album" and
  "Album (Multichannel)" ([#138](https://github.com/aclater/sacdfs/issues/138), tested on 10
  discs, 6 with a multichannel area). For an existing library, turn it on and run "Refresh
  All Metadata".
- For a Plex container on a host that mounts sacdfs, bind the mount's parent with `rslave`.
- Consider turning off the nightly deep analysis, or keep the 16 MiB cache.

**Limits.**
- Plex does not use the folder art from sacdfs ([#95](https://github.com/aclater/sacdfs/issues/95)):
  in the 0.2.0-rc.4 run 230 of 264 album bundles held a local-style poster and 71 an online one
  (approximate), and on 0.1.1 every poster of a 32-disc subset came from online sources.
- In the whole-tree 0.2.0-rc.4 run Plex still merged the Stereo and Multichannel copies of a
  disc into one album and showed 42 duplicate artist and title pairs; only 1 album was named
  "(Multichannel)". That run did not use "Prefer local metadata" ([#138](https://github.com/aclater/sacdfs/issues/138)).
  The 10-disc test above is the one with the setting.
- 20 of the 291 images were not seen in Plex's paths in that run, and the cause was not found.

## Jellyfin

**Tested on 0.3.0-rc.1.** Jellyfin 12.4.0 (official image, 2 CPUs), one music library.
- *Tracks:* 93 of 93 with `dsf`, 186 of 186 with `dsf,wav`; codec, channels (2, 5 and 6),
  track numbers and durations set for every file.
- *Albums:* 7, all names distinct; the 2 multichannel areas are their own albums,
  "… (Multichannel)". (The extracted copy, which has no suffix, gave 7 albums with only 5
  distinct names.)
- *Art:* the 4 album areas with a cover use it, the Multichannel one included (the image's
  path is the copy in the area folder). Jellyfin fetched online art for 2 of the other 3.
- *Cost of WAV:* during the first scan sacdfs served 17.9 MiB for the 93 DSF files (about
  200 KB each) and 648.5 MiB with `dsf,wav`: ffprobe reads about 7 MB of each WAV, which
  sacdfs must convert. sacdfs used 1 CPU-second and 24 MiB RSS with `dsf`, 24 to 26 CPU-seconds
  and 60 MiB with `dsf,wav`. Jellyfin's own work dominates its scan time (42 to 96 s with
  `dsf`, 51 to 104 s with `dsf,wav`, 38 s for the extracted `dsf,wav` copy, two runs each), so
  the times vary too much to compare closely.
- *Playback:* a 5.1 DST track and a stereo track transcoded to MP3 for their full length
  (sizes match their durations); a transcode started at 60 % of the track sent its first
  bytes in 6 to 9 ms.
- *LUFS normalization:* turned on for the `dsf` library and run as Jellyfin's
  "Audio Normalization" task, it opened every file twice and read 39.8 GiB from sacdfs for a
  library of 22.0 GiB of DSF files, in 464 s, decoding DST all the while (sacdfs averaged 1.7
  cores; RSS 35 MiB at `--cache-mb 16`).

**Recommended setup.**
- `--format dsf`: it saves sacdfs converting about 7 MB of PCM per WAV file on every first
  scan or rebuild ([#96](https://github.com/aclater/sacdfs/issues/96)).
- Leave LUFS normalization off (the default): with it on, Jellyfin reads about twice the
  whole library, and sacdfs must decode all of it.
- Bind the mount's parent directory with `rslave`.

## Navidrome

**Tested on 0.3.0-rc.1.** Navidrome 0.64.2 (official image). First scan 0.43 s with `dsf`
(Navidrome's figure), 93 of 93 files, no errors; 0.68 s and 186 of 186 with `dsf,wav`. A full
re-scan with nothing changed opened the files but read nothing from sacdfs (0 reads). Album
art: **4 of 4** album areas with a cover show it, the Multichannel one included (the copy in
each area folder); the other 3 get Navidrome's placeholder. 7 albums: the 2 multichannel
areas are separate albums named "… (Multichannel)". DSF and WAV of a track share an album.
Durations set for all files. sacdfs peaked at 47 MiB RSS. After sacdfs was stopped and started again under the running
container, the parent bind showed all 93 files again and a full scan went through. Playback:
a raw stream request with a range returned `206` and `audio/x-dsf` in 26 ms; an MP3
transcode (FFmpeg, on request) of a 5.1 DST track returned the whole track (10.1 MB) in
7.6 s, with sacdfs at 20 CPU-seconds.

**Recommended setup.** `--format dsf` (or `dsf,wav` if you want both), `--flatten`,
the default `--cache-mb` (32 since #150; tested with 16). Keep the default `(Multichannel)` album suffix if you want Stereo and
Multichannel as separate albums.

## Lyrion / Logitech Media Server

**Tested on 0.3.0-rc.1.** Lyrion Music Server 9.1.1 (official image), scanned and browsed through
JSON-RPC, with squeezelite 2.0 (Fedora package, DSD support) in the same pod as the player.
First scan (`wipecache`) 1.1 s for 93 of 93 files with `dsf`, and 1.1 s for 186 of 186
with `dsf,wav`; a `rescan` with nothing changed made 0 opens and 0 reads. Album art: **4 of 4**
album areas with a cover have it (the copy in the area folder); 7 albums, the 2 multichannel
areas as "… (Multichannel)". Durations set; DSF bitrate shows as 0. sacdfs peaked at 55 MiB
RSS. DSD playback is native: the stock `convert.conf` has only `dsf dsf` (pass-through) for
DSF, so a player without DSD gets nothing unless the third-party DSD Player plugin is added.
squeezelite received the stereo DSF and decoded it itself (DSD to PCM); the 5.1 DSF stopped
at once, because squeezelite is stereo-only (not sacdfs).

**Recommended setup.** `--format dsf` (or `dsf,wav`), `--flatten`. The official image runs
LMS as an unprivileged user (uid 499 by default), which cannot read a FUSE mount made without
`allow_other` ("Permission denied" in the test), and LMS refuses to run as root unless told
to (it falls back to `nobody`). For a rootless container reading a direct mount, set
`PUID=0`, `PGID=0` and `EXTRA_ARGS="--user root"` (root in a rootless container is the mount's
owner); that is what the test ran. Reading the container's SMB or NFS share instead was not
validated with LMS.

## MPD, moOde, Volumio

**Tested on 0.3.0-rc.1.** MPD 0.24.15 (Fedora 44 package, in a container), null output with
`sync yes`. moOde and Volumio use MPD, but neither was run itself. MPD's first scan (at start,
empty database) took **0.55 s** for 93 files with `dsf` and **5.6 s** for 186 with `dsf,wav`,
where sacdfs used 19 CPU-seconds converting WAV for FFmpeg's probe; with `dsf` it used
1.6 CPU-seconds. 7 albums, 2 of them "… (Multichannel)". `mpc update` with nothing changed:
0.05 s, 0 opens. DSF plays as DSD (`audio: dsd64:2`, `dsd64:6`; DoP or native DSD to a
capable output, otherwise MPD converts); a seek to 100 s in a 5.1 DST DSF worked at once.
Album art: `albumart` found the cover in **1 of 4** album areas with one: the disc whose
cover is named `cover.jpg`. MPD looks only for files named `cover.*` in the track's folder,
so `folder.jpg` is not used.

**Recommended setup.**
- `--format dsf`: the first scan was 10 times as fast as with `dsf,wav`. The WAV cost is
  FFmpeg's 5,000,000-byte probe of each WAV file ([#96](https://github.com/aclater/sacdfs/issues/96)).
  If another client needs WAV, run a second container or mount with `dsf,wav` for it.
- `--flatten`; bind the mount's parent with `rslave` if MPD is in a container.
- For art, name the cover beside each image `cover.jpg` (or `cover.png`).

## MinimServer

**Tested on 0.3.0-rc.1.** MinimServer 2.2 (official image, starter licence), browsed with UPnP
ContentDirectory `Browse` calls. The scan finished within 1 s (every file opened in the same second; "93 audio files in
17 folders"); a restart with its cache made 0 opens and 0 reads. sacdfs peaked at 27 MiB RSS.
Album art: **4 of 4** album areas with a cover have `upnp:albumArtURI` on their items (63
of 93 items), served from the copy in the area folder; 7 albums, 2 of them
"… (Multichannel)". Items are served as `audio/x-dsf` with ranges: a whole 5.1 DST DSF
(447 MB) streamed in 4.5 s, and a 1 MiB range at mid-file answered `206` in 4 ms.

**Recommended setup.** `--format dsf`, `--flatten`. DSD is served as is; a player that needs
transcoding needs MinimStreamer, which was not tested. `dsf,wav` was not tried with
MinimServer on 0.3.0-rc.1.

## Windows Explorer and Robocopy

**Tested.** Windows 11 build 26200, SMB 3 to the container on the same host with user
authentication, sacdfs 0.2.0-rc.1, `dsf,wav`, `--flatten`, 93 DSF + 93 WAV (54.96 GB).
- *Explorer and PowerShell:* a recursive listing of 198 entries in 0.1 s; 14 of 186 names have
  SFM private-use characters, listed and opened normally (they show a placeholder glyph, as
  documented). The details pane reads WAV tags (artist, album, year, track, title, length);
  DSF shows size, date and type only, because Windows has no DSF property handler. Copies of one
  DSF (120 MB/s) and one WAV (91 MB/s) had a SHA-256 identical to the served file (2 of 2).
- *Robocopy `/MIR`:* run 1 copied 186 of 186 files with no failures in 899 s (65.6 MB/s, bound
  by DST decoding); sacdfs used 2,212 CPU-s and peaked at 507 MB RSS at the default cache.
  Run 2 copied 0 and skipped 186 in 0.1 s, because sizes and times are stable.

**Recommended setup.** Set `SMB_USER` and `SMB_PASSWORD`: Windows 11 24H2 and later refuse
guest access. See [Windows](deployment.md#windows) and
[file names over SMB](deployment.md#file-names-over-smb).

## foobar2000

**Tested (partial).** foobar2000 2.26.2 portable on Windows 11, SMB 3, sacdfs 0.2.0-rc.1.
The SACD/DSD component could not be installed unattended, so only the stock decoder was
tried. Adding one area folder and opening one DSF caused 25 opens and 65 reads and no errors in
sacdfs's log. **Not verified:** tags, durations, art, seek and multichannel naming (no GUI
evidence). This is neither a pass nor a fail.

## JRiver Media Center

**Untested.** The trial download failed in the test script (a bad URL) and no interactive trial
was done. It needs a manual run (JRiver Media Center 36 was the planned version).

## Linux NFS and SMB mounts

The container's share tests (SMB and NFS) pass on its published image, and sacdfs also runs
directly on a Linux client. Cold-seek and read-latency figures for NFS and SMB, server-side and
client-side, are in [performance](performance.md#client-side). The mount commands are in the
[deployment guide](deployment.md#linux). No further per-client report exists; macOS mounts are
documented but have no test report on #82.

## General recommendations

- **Deploy server-side on the NAS.** sacdfs runs next to the images in the container and the
  clients see ordinary files over SMB or NFS. A cold seek costs about 5 to 6 ms over SMB on a
  gigabit LAN ([deployment](deployment.md)).
- **The default cache suits a NAS.** Since #150 the default `--cache-mb` is 32 (was 256), and
  a Plex-like scan of the bench images peaks at 46 MiB RSS (0.3.0 at its default: 103 MiB), a
  Roon-like import replay at 45 MiB (134 MiB). The 0.3.0-rc.1 server tests ran with
  `--cache-mb 16`, with no errors and sacdfs at 24 to 60 MiB RSS; in MPD's first scan with
  `dsf,wav`, sacdfs peaked at 300 MiB RSS at the old default of 256 MiB against 58 MiB at 16, and
  the scan took the same 5.6 s. 16 still works; 32 keeps several fast readers of one image at
  full speed. See [Memory](deployment.md#memory) and
  [#94](https://github.com/aclater/sacdfs/issues/94).
- **Containers on a host that mounts sacdfs:** bind the mount's *parent* directory with
  `rslave` (`-v /srv/sacd:/media:ro,rslave`), not the mount point. Tested on 0.3.0-rc.1 with
  rootless Podman: after sacdfs was killed and started again, a container that bound the mount
  point, or the parent without `rslave`, failed every read ("Socket not connected"); after a
  clean stop, the old sacdfs process kept running to serve those containers, so they never saw
  the new one. The container with the `rslave` parent bind lost the mount while sacdfs was
  down and saw the new one as soon as it was up, and a full Navidrome scan through it worked
  ([deployment](deployment.md#media-servers-in-containers-on-a-client-host)). On an SELinux
  host the bind needs no relabelling option (`:z` cannot relabel a FUSE mount, and the test
  read files without one); the servers' own data volumes do need one.
- **Choose formats for the client.** `dsf` alone for servers that probe with FFmpeg (MPD,
  Jellyfin) and for Roon; `dsf,wav` only where a client needs WAV. Use `--flatten` so that
  each disc's `Stereo` and `Multichannel` folders sit under the album.
- **Folder art:** sacdfs 0.2.0 and later place a copy of the cover beside the image inside
  each area folder (`Stereo`, `Multichannel`; [#95](https://github.com/aclater/sacdfs/issues/95)).
  On 0.3.0-rc.1, Jellyfin, Navidrome, LMS and MinimServer used it for all 4 album areas with
  a cover. MPD uses only files named `cover.*`, so it found 1 of 4 (the one named
  `cover.jpg`). Roon used the art on 0.2.0; for Plex (0.2.0-rc.4) see [its section](#plex).
- **Multichannel copies:** sacdfs adds ` (Multichannel)` to the album tag by default
  ([#97](https://github.com/aclater/sacdfs/issues/97)). With it, Jellyfin, Navidrome, LMS, MPD
  and MinimServer showed the 2 multichannel areas as their own albums on 0.3.0-rc.1. Plex
  needs "Prefer local metadata" to show it. `--no-multichannel-album-suffix` turns it off; it
  was not tried with any client.
- **Options not validated with any client:** `--pauses kept`, `--tags DIR` and
  `--request-timeout` were left at their defaults in every client test, so this page makes no
  recommendation about them. What they do is in the [user guide](user-guide.md#options).
- <a id="restarts-and-rescans"></a>**Restarts and rescans.** Scanners that skip unchanged
  folders (Roon) decide from folder modification times. From 0.2.0, a new sidecar, tag or
  version change moves the times of the disc's folders and every folder above it, and new
  sidecars and images show up within a few seconds without a restart
  ([#101](https://github.com/aclater/sacdfs/issues/101),
  [#123](https://github.com/aclater/sacdfs/issues/123); tested with Roon on 0.2.0). Server
  rescans with nothing changed read nothing on 0.3.0-rc.1: 0 opens for MPD, MinimServer
  and LMS, and 0 reads for Navidrome.
- **Hard disks and simultaneous cold starts.** Once playing, no read took over 0.5 ms in the
  hard-disk test. Slow reads (116 to 156 ms worst) come from several players' first seeks
  queueing on one physical disk when four start at once (39 to 62 ms staggered by 2 s); it is
  bounded by the disk and no sacdfs setting changed it
  ([#71](https://github.com/aclater/sacdfs/issues/71), [performance](performance.md#client-side-where-the-slow-reads-come-from-71)).

## Reporting a client

Open an issue with the label `documentation`, mention #82, and title it
"Client: NAME VERSION". Include:

- the client and its version, and the sacdfs version (image tag or `sacdfs --version`);
- how it was connected: SMB, NFS, a direct bind, or a bind with `rslave`;
- the sacdfs options (`--format`, `--flatten`, `--cache-mb`, `--pauses`, `--tags`) and library
  size (images, tracks);
- scan time and tracks found against the files expected, re-scan time, sacdfs memory and CPU;
- tags, duration, art, multichannel naming, seeking and playback, each as seen or not checked;
- errors and warnings from the client and the sacdfs log (`-v` for more).

Leave out names of albums, artists, paths, hostnames and addresses. Say "untested" or
"partial" for anything not checked.

