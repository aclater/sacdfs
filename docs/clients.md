# Clients: what sacdfs has been tested with

This page lists the music servers, players and file tools that sacdfs has been tried with, the
version of each, how it was connected, what happened, and how to set sacdfs and the client up.
Everything here comes from the client-test reports on
[#82](https://github.com/aclater/sacdfs/issues/82) and the issues linked below; a result is
only stated where a test showed it. "Untested" and "partial" mean what they say. For the
container, shares and mounting, see the [deployment guide](deployment.md); for the options,
the [user guide](user-guide.md#options).

The test library was small except where noted: 5 images (plain DSD, DST, hybrid, DST
stereo-only, raw), 93 tracks, so 186 files with `--format dsf,wav` (20 tracks of each format
are multichannel). No file was over 4 GiB, so RF64 WAV was not exercised.

## Summary

| Client | Version tested | sacdfs tested | Connection | Result | Notes |
|---|---|---|---|---|---|
| [Roon](#roon) | Roon 2.x (Roon Server official image) | 0.1.1, 0.2.0-rc.1 to rc.4, 0.2.0 final | SMB, same NAS | Works (0.2.0) | Use `dsf` only; needs the SMB-mount rights; art and playlists work; picks up upgrades by itself; multichannel areas are separate albums |
| [Plex](#plex) | Plex Media Server 1.43.5 | 0.1.1, 0.2.0-rc.4 | SMB (CIFS, read-only), same NAS; container with an `rslave` bind | Works with settings | 406 of 406 tracks found; turn on "Prefer local metadata"; folder art not used ([#95](https://github.com/aclater/sacdfs/issues/95)) |
| [Jellyfin](#jellyfin) | not recorded | 0.1.1 | Container with an `rslave` bind of the mount's parent | Works with settings | 186 of 186 found; use `dsf`; leave LUFS normalization off; folder art not used |
| [Navidrome](#navidrome) | 0.64.2 | 0.1.1 | Direct bind of the mount | Works | 186 of 186 in 0.51 s; finds folder art |
| [Lyrion / Logitech Media Server](#lyrion-logitech-media-server) | 9.1.1 | 0.1.1 | Direct bind of the mount | Works with limits | No folder art ([#95](https://github.com/aclater/sacdfs/issues/95)); needs a DSD-capable player |
| [MPD](#mpd-moode-volumio) (so moOde, Volumio) | 0.24.15 | 0.1.1 | Direct bind of the mount | Works with settings | Use `dsf`: the first scan is 15 times faster; plays DSD natively; no folder art |
| [MinimServer](#minimserver) | 2.2, update 275 | 0.1.1 | Direct bind of the mount | Works with limits | Lightest scan; no folder art; no transcoding without MinimStreamer |
| [Windows Explorer, PowerShell, Robocopy](#windows-explorer-and-robocopy) | Windows 11 build 26200 | 0.2.0-rc.1 | SMB 3, same host | Works | Copies are byte-identical; a second Robocopy pass skips every file |
| [foobar2000](#foobar2000) | 2.26.2 (portable) | 0.2.0-rc.1 | SMB 3 | Partial | Opened a DSF without errors; tags, art and seek not verified |
| [JRiver Media Center](#jriver-media-center) | 36 | none | SMB 3 | Untested | The trial was not run |
| [Linux NFS and SMB mounts](#linux-nfs-and-smb-mounts) | kernel clients | see [deployment](deployment.md) and [performance](performance.md) | NFSv4, SMB | Works | The container's own share tests; see below |

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
continued through a sacdfs restart. A library import of 271 images and 3,216 tracks was replayed against 0.1.0 for the memory
figures in the deployment guide.

**Recommended setup.**
- `SACDFS_FORMATS=dsf` (`--format dsf`): with both formats every track appears twice. Roon
  plays DSD and converts it itself.
- `SACDFS_FLATTEN=1`; `SACDFS_CACHE_MB=16` on a NAS.
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

**Tested.** sacdfs 0.1.1, `dsf,wav` with `--flatten`, 5 images, 186 files; the server was a
container binding the mount's parent directory with `rslave`. All 186 tracks found, and the
metadata identical to the extracted copy (0 differences). A rescan took 3 s with no file
reads. DSD and 5.1 transcoded correctly with full durations; cold seeks to 60 % of a file
answered in 20 to 32 ms. sacdfs peaked at 329 MiB during the scan.

**Recommended setup.**
- `--format dsf`: ffprobe reads about 5 MB of each stereo WAV and 12 to 14 MB of each 5.1 WAV,
  so the first scan was 27 to 58 % slower than on extracted files; a DSF costs about 200 KB
  ([#96](https://github.com/aclater/sacdfs/issues/96)).
- Leave LUFS normalization off (the default). With it on, Jellyfin read every file twice: 111
  GiB in 25 minutes, 688 MiB RSS.
- Bind the mount's parent directory with `rslave`.

**Limits.** Folder art is not taken from sacdfs ([#95](https://github.com/aclater/sacdfs/issues/95)).
Multichannel copies showed as two albums with the same name in the 0.1.1 test, before the
`(Multichannel)` album tag existed; not re-tested with Jellyfin since
([#97](https://github.com/aclater/sacdfs/issues/97)).

## Navidrome

**Tested.** Navidrome 0.64.2 (go-taglib) in a rootless container on a direct bind of the
mount, sacdfs 0.1.1 with `dsf,wav`, `--flatten`, `--threads 4`, default cache. First scan
0.51 s (Navidrome's figure), 186 of 186 files, no errors; sacdfs RSS 45 MiB. A quick re-scan
took 8.7 ms; a full one 0.31 s with no reads reaching sacdfs. Tags, track numbers, year, genre
and duration all came from the ID3 tag, for DSF and WAV. Folder art was found for 2 of 2
albums (Navidrome checks the parent folder). Playback is the raw DSF stream with ranges; a
transcode is FFmpeg on request (it pushed sacdfs to 304 MiB RSS at the default cache). Names
with `?`, `:`, `!`, accents and commas were all imported.

**Recommended setup.** `--format dsf` (or `dsf,wav` if you want both), `--flatten`,
`--cache-mb 16` on a NAS. If you want Stereo and Multichannel as separate albums, keep the
default `(Multichannel)` album suffix.

**Limits.** In the 0.1.1 test, before the suffix existed, Stereo and Multichannel merged into
one album with each track number up to four times ([#97](https://github.com/aclater/sacdfs/issues/97)).
Not re-tested on a later sacdfs.

## Lyrion / Logitech Media Server

**Tested.** Lyrion Music Server 9.1.1, scanned and browsed through JSON-RPC, with squeezelite
2.0 (DSD support) as the player; sacdfs 0.1.1, direct bind. First scan 2.1 s for 186 of 186
files; re-scans 1.0 s and 1.6 s with no opens. sacdfs RSS 62 MiB at the scan, but 432 MiB at
the default cache after streaming. Tags and durations right; DSF bitrate shows as 0. DSD
playback is native: the stock `convert.conf` only passes `dsf` through, so a player without
DSD gets nothing unless the third-party DSD Player plugin is added. A 5.1 DSF stalled on
squeezelite, which is stereo-only (not sacdfs).

**Recommended setup.** `--format dsf` (or `dsf,wav`), `--flatten`. LMS runs as `nobody` when it
cannot run as root, and `nobody` cannot read a FUSE mount without `allow_other`; the test ran
the container as root. Reading the container's SMB or NFS share instead avoids this.

**Limits.** No folder art: LMS looks only in the track's folder
([#95](https://github.com/aclater/sacdfs/issues/95)). In the 0.1.1 test, Stereo and
Multichannel became two albums with the same name ([#97](https://github.com/aclater/sacdfs/issues/97)).

## MPD, moOde, Volumio

**Tested.** MPD 0.24.15 (Fedora 44 package, in a container), sacdfs 0.1.1, direct bind.
moOde and Volumio use MPD, but neither was run itself. MPD's first scan of 186 files took
**9.0 s** with `dsf,wav` and **0.6 s** with `dsf` (93 files); sacdfs used 27 CPU-s and 324 MiB
in the first case. Updates were cheap: `mpc update` 0.13 s, `mpc rescan` 0.37 s, and an update
after an unmount and remount 0.18 s with no opens. DSF uses MPD's own plugin and plays as DSD
(`dsd64:2` or `dsd64:6`; DoP or native DSD to a capable output, otherwise MPD converts). WAV
goes through FFmpeg. Seeks worked at once on 5.1 DST in both formats.

**Recommended setup.**
- `--format dsf`. The WAV cost is FFmpeg's 5,000,000-byte probe: about 420 MiB of PCM
  converted for 93 files ([#96](https://github.com/aclater/sacdfs/issues/96)). If another
  client needs WAV, run a second container or mount with `dsf,wav` for it.
- `--flatten`; bind the mount's parent with `rslave` if MPD is in a container.

**Limits.** `albumart` finds nothing: MPD looks only in the track's folder
([#95](https://github.com/aclater/sacdfs/issues/95)).

## MinimServer

**Tested.** MinimServer 2.2 update 275 (official image, starter licence), browsed with UPnP
ContentDirectory `Browse` calls; sacdfs 0.1.1, direct bind. The scan finished in under 1 s
("186 audio files in 17 folders"); a restart with its cache did 0 opens. sacdfs RSS 39 MiB.
It reads the least per file (about 48 KiB for a WAV). Items are served as `audio/x-dsf` with
ranges; a whole 5.1 DST DSF streamed in 7.4 s. Names with odd characters are escaped in URLs
and play.

**Recommended setup.** `--format dsf` (or `dsf,wav`), `--flatten`. DSD is served as is; set up
MinimStreamer if a player needs transcoding.

**Limits.** No `albumArtURI` on any item, because MinimServer looks only in the track's folder
([#95](https://github.com/aclater/sacdfs/issues/95)). In the 0.1.1 test, Stereo and
Multichannel became two albums with the same name.

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
- **Use `--cache-mb 16` on a NAS.** The 256 MiB default uses much more memory than its size
  suggests during a scan: Plex on 0.1.1 ended at 549 MB against 126 MB with 16 MiB, with no
  cost to Plex ([#94](https://github.com/aclater/sacdfs/issues/94), [Memory](deployment.md#memory)).
- **Containers on a host that mounts sacdfs:** bind the mount's *parent* directory with
  `rslave` (`-v /srv/sacd:/media:ro,rslave`), not the mount point. After a sacdfs restart the
  container's copy of a bound mount point goes stale ("not connected"); the parent bind
  survives it ([deployment](deployment.md#media-servers-in-containers-on-a-client-host)).
- **Choose formats for the client.** `dsf` alone for servers that probe with FFmpeg (MPD,
  Jellyfin) and for Roon; `dsf,wav` only where a client needs WAV. Use `--flatten` so that
  each disc's `Stereo` and `Multichannel` folders sit under the album.
- **Folder art:** only Navidrome and Roon found it in the tests; LMS, MPD, MinimServer, Plex
  and Jellyfin did not ([#95](https://github.com/aclater/sacdfs/issues/95), closed in 0.2.0,
  which also places a copy of the cover inside `Stereo` and `Multichannel`; the per-server
  re-check is in the reports above only for Plex, where it did not help). Check your server.
- **Multichannel copies:** sacdfs adds ` (Multichannel)` to the album tag by default
  ([#97](https://github.com/aclater/sacdfs/issues/97)); `--no-multichannel-album-suffix`
  turns it off. Plex needs "Prefer local metadata" to show it.
- **Pauses and tags:** `--pauses kept` serves tracks with their pauses so that files compare
  as identical to extractors that keep them; `--tags DIR` overrides and adds tags (genres,
  MusicBrainz IDs, art). Neither was varied in the client tests, which used the defaults.
- <a id="restarts-and-rescans"></a>**Restarts and rescans.** Scanners that skip unchanged
  folders (Roon) decide from folder modification times. From 0.2.0, a new sidecar, tag or
  version change moves the times of the disc's folders and every folder above it, and new
  sidecars and images show up within a few seconds without a restart
  ([#101](https://github.com/aclater/sacdfs/issues/101),
  [#123](https://github.com/aclater/sacdfs/issues/123)). Server rescans with nothing changed
  read nothing: 0 opens for MPD, MinimServer and LMS, and 0 reads for Navidrome.
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
