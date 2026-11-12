# Clients: what sacdfs has been tested with

This page lists the music servers, players and file tools that sacdfs has been tried with, the
version of each, how it was connected, what happened, and how to set sacdfs and the client up.
Everything here comes from the client-test reports on
[#82](https://github.com/aclater/sacdfs/issues/82) and the issues linked below; a result is
only stated where a test showed it, and a setting is only recommended where a test on the
version named ran with it. "Untested", "partial" and "not validated" mean what they say. For
the container, shares and mounting, see the [deployment guide](deployment.md); for the
options, the [user guide](user-guide.md#options).
What is run for each release, and what blocks one, is in the [test plan](test-plan.md).

The test library was small except where noted: 5 images (plain DSD, DST, hybrid, DST
stereo-only, raw), 93 tracks, so 186 files with `--format dsf,wav` (20 tracks of each format
are multichannel). No file was over 4 GiB, so RF64 WAV was not exercised.

## Summary

| Client | Version tested | sacdfs tested | Connection | Result | Notes |
|---|---|---|---|---|---|
| [Roon](#roon) | Roon 2.x (Roon Server official image); 2.74 build 1719 on 0.4.1-rc.1 | 0.1.1, 0.2.0-rc.1 to rc.4, 0.2.0 final, 0.4.1-rc.1 | SMB, same NAS | Works (0.4.1-rc.1: no regression, no extended-attribute requests in a Force Rescan) | Use `dsf` only; needs the SMB-mount rights; art and playlists work; picks up upgrades by itself; multichannel areas are separate albums |
| [Plex](#plex) | Plex Media Server 1.43.5 | 0.1.1, 0.2.0-rc.4, 0.4.1-rc.1 | SMB (CIFS, read-only), same NAS; container with an `rslave` bind | Works with settings | 0.4.1-rc.1: healthy, upgrade and rollback from 0.4.0 fine, 279 untitled tracks named, 3 albums (26 tracks) still blank in Plex (cause unknown); 0.1.1: 406 of 406 tracks found; turn on "Prefer local metadata"; folder art not used ([#95](https://github.com/aclater/sacdfs/issues/95)) |
| [Jellyfin](#jellyfin) | 12.4.0 (official image) | 0.1.1, 0.3.0-rc.1, 0.4.0-rc.2, 0.4.1-rc.1, 0.4.1, 0.5.0-rc.1 | Container with an `rslave` bind of the mount's parent | Works with settings | 93 of 93 (`dsf`), 186 of 186 (`dsf,wav`); use `dsf`; leave LUFS normalization off; folder art used; multichannel areas are separate albums |
| [Navidrome](#navidrome) | 0.64.2 | 0.1.1, 0.3.0-rc.1, 0.4.0-rc.2, 0.4.1-rc.1, 0.5.0-rc.1 | Container with an `rslave` bind of the mount's parent | Works | 93 of 93 (`dsf`), 186 of 186 (`dsf,wav`); folder art used; multichannel areas are separate albums; renamed and removed images followed |
| [Lyrion / Logitech Media Server](#lyrion-logitech-media-server) | 9.1.1 | 0.1.1, 0.3.0-rc.1, 0.4.0-rc.2, 0.4.1-rc.1, 0.5.0-rc.1 | Container with an `rslave` bind of the mount's parent | Works with settings | 93 of 93 (`dsf`), 186 of 186 (`dsf,wav`); folder art used; needs a DSD-capable player; run LMS as root on a direct mount |
| [MPD](#mpd-moode-volumio) (so moOde, Volumio) | 0.24.15 | 0.1.1, 0.3.0-rc.1, 0.4.0-rc.2, 0.4.1-rc.1, 0.5.0-rc.1 | Container with an `rslave` bind of the mount's parent | Works with settings | Use `dsf`: the first scan is about 10 times faster; plays DSD natively; folder art found in 6 of 6 areas with a cover once sacdfs also shows it as `cover.*` (after 0.4.0: 1 of 6; [#148](https://github.com/aclater/sacdfs/issues/148)) |
| [MinimServer](#minimserver) | 2.2, update 275 | 0.1.1, 0.3.0-rc.1, 0.4.0-rc.2, 0.4.1-rc.1, 0.5.0-rc.1 | Container with an `rslave` bind of the mount's parent | Works | 93 of 93; folder art used; no transcoding without MinimStreamer |
| [Windows Explorer, PowerShell, Robocopy](#windows-explorer-and-robocopy) | Windows 11 build 26200 | 0.2.0-rc.1, 0.4.1-rc.1, 0.6.0-rc.1 | SMB 3, SMB 3.1.1, same host | Works | 0.4.1-rc.1: 186 of 186 audio copies byte-identical, a second Robocopy pass skips all 196 files; RF64 reads and seeks past 4 GiB; another SMB port works (`/TCPPORT`) |
| [foobar2000](#foobar2000) | 2.26.2 (portable), foo_input_sacd 2.0.27 | 0.2.0-rc.1, 0.4.1-rc.1 | SMB 3, SMB 3.1.1 | Works (0.4.1-rc.1) | 186 files found with tags and length; stereo, 5ch, 6ch start in under 0.4 s; seek to 60 % works; audio not heard |
| [JRiver Media Center](#jriver-media-center) | 36.0.38 (trial) | 0.4.1-rc.1 | SMB 3.1.1 | Partial | 186 files, 7 albums, tags, art for 3 of 5 artists; plays; seek, multichannel and rescan not checked |
| [Linux NFS and SMB mounts](#linux-nfs-and-smb-mounts) | Fedora 44, RHEL 10.1 and 9.7, Debian 13 and 12, Ubuntu 26.04 and 24.04 kernels | 0.4.1-rc.1, 0.5.0-rc.1 | NFSv4.2, SMB 3.1.1 | Works | All checks pass; Ubuntu 24.04 cloud image needs `linux-modules-extra` for non-ASCII names over SMB; after a Podman container restart NFS needs a remount |
| [FFmpeg, sox, mediainfo, ExifTool, mpv, VLC](#ffmpeg-sox-mediainfo-exiftool-mpv-and-vlc) | 7.1.5, 14.4.2, 25.04, 13.25, 0.40.0, VLC 3 | 0.4.1-rc.1 | Mount | Works | sox and ExifTool do not read DSF; this VLC cannot decode DSD (also true of a plain copy) |

## 0.6.0-rc.1 checks

Run on the v0.6.0-rc.1 release binary and the published `0.6.0-rc.1` image; 0.5.0 was the baseline, run side by side. The table above keeps each client's last full run.

- **Scripted servers,** the same images as for 0.5.0 (Jellyfin 12.4.0, Navidrome 0.64.2, Lyrion 9.1.1, MPD 0.24.15, MinimServer 2.2): the same tracks (93), albums (7), art and bytes as 0.5.0, with `dsf` and `dsf,wav`. Navidrome's restart and full rescan found 93 tracks in 7 folders on both. No ERROR or WARN lines on either side.
- **Linux NFS 4.2 and SMB 3.1.1 mounts,** 22 checks per client: Fedora 44, RHEL 10.1, RHEL 9.7, Debian 13, Debian 12 and Ubuntu 26.04 pass all 22. Ubuntu 24.04 passes 21: `stat` sizes over SMB fail for the files with non-ASCII names, the known `linux-modules-extra` problem ([below](#linux-nfs-and-smb-mounts)), the same as on 0.5.0.
- **FUSE requests** (counted from sacdfs's request log), whole scripted-server session, 0.5.0 then 0.6.0-rc.1: Navidrome `dsf` 3,477 to 1,204; Lyrion `dsf` 400 to 214; MPD `dsf` 309 to 234; MinimServer 311 to 231; Jellyfin `dsf` 1,900 to 1,713. Directory reads (READDIR and READDIRPLUS) fall most; Jellyfin `dsf` LOOKUP rose from 284 to 304 (+7 %, within run-to-run spread).
- **Fresh Fedora 44 client, server restarted before each workload, requests caused by the workload alone** (216-file test tree):

  | Workload | 0.5.0 | 0.6.0-rc.1 |
  |---|---|---|
  | SMB `ls -lR` | 253 | 175 |
  | SMB `find` | 241 | 163 |
  | NFS `ls -lR` | 1,753 (LISTXATTR 238, GETXATTR 1) | 1,753 (the same, kind for kind) |
  | NFS `find` | 206 | 206 (the same, kind for kind) |
  | NFS `getfattr -d -R` | 2,052 (LISTXATTR 476, GETXATTR 1) | 2,052 (the same, kind for kind) |

  NFS is unchanged in every kind, so there is no extended-attribute storm: `ls -lR` still sends 1 GETXATTR and 238 LISTXATTR for 206 files, as in 0.4.1 and 0.5.0, and all three NFS commands exit 0 with no error output.
- **macOS 27.2 in a VM on Apple hardware** (SMB 3.x, `mount_smbfs`, command-line tools; headless, no audio, no Finder GUI), against the published 0.6.0-rc.1 image with an SMB share, flattened layout, 12,434 files (4,911 DSF, 4,911 WAV) in 1,113 folders. The VM has no GPU and no audio path, and no Finder or DSD player app was driven, so those are not covered.

  | Check | Result |
  |---|---|
  | Mount (`mount_smbfs`, password) | Works; read-only, listing at once |
  | Restart of the container | The client drops the mount by itself within seconds; `ls` fails or is empty until a remount, and a remount works at once. This is the documented answer on #204 (macOS needs a remount after a restart or upgrade). Nothing hung. |
  | Browse (`ls -lR`, `stat`, `df`, `find`) | 16,883 lines in 6 s; sizes right; no hangs |
  | `._` files and `.DS_Store` | None served; probes for them return "No such file" |
  | Spotlight | Indexing is disabled on the SMB mount (as on 0.4.0); `mdls`, `mdimport` read both formats; `mdfind` finds nothing on the share |
  | Quick Look | Thumbnails produced for DSF and WAV; preview opened without a hang |
  | Read-only | cp, mv, rm, mkdir, touch, chmod, ln, truncate, xattr (including Finder tags) all fail cleanly with "Read-only file system" |
  | Byte identity | cp of a DSF and a WAV: hashes equal the mount's. rsync of a whole 6.9 GB album (19 files): all 19 hashes identical; a second rsync copied 0 files |
  | Odd names | 107,258 name components, 411 non-ASCII, 340 with characters macOS treats specially; all 13,547 names open by their listed name. sacdfs and Samba serve names precomposed (NFC) and macOS lists them decomposed (NFD). Of the 357 non-ASCII file names, 111 have a different NFC and NFD spelling (the rest, such as `ø`, have one spelling). A precomposed (NFC) lookup of those 111 fails once the macOS client has cached the folder's listing, and works before it; a decomposed one always works. The failing lookup never reaches the server (a capture shows no request for it), and the client setting `dir_cache_off=yes` in `nsmb.conf` made all 357 open by both spellings (#308) |
  | RF64 over 4 GiB | A 7.4 GB six-channel 352.8 kHz track (needs `SACDFS_PCM_RATE=352800`): `afinfo` reads RF64, 1161 s; seeks at 0 to 99 % and across 4 GiB return in about 0.1 to 0.2 s; reading 256 MiB across the 4 GiB mark took 12.9 s |
  | Tags | `afinfo` reads the WAV; it cannot open a DSF (no CoreAudio DSD support, as before); `mdls` gives kind and title, other audio attributes are null |

  Not run on macOS: request counts per kind (the test server was not logging FUSE requests), Finder views, DSD player apps, sound or playback.
- **Windows 11 (Explorer, PowerShell, Robocopy):** build 26200 in a VM, SMB 3.1.1 to the published 0.6.0-rc.1 image on the same host, user authentication, `dsf,wav`, `--flatten`, the same 93 DSF + 93 WAV (54.96 GiB) plus art as the 0.4.1-rc.1 run; the VM used a virtio-net card (see [#225](https://github.com/aclater/sacdfs/issues/225)). The 0.4.1-rc.1 result holds, with fewer requests:
  - A recursive listing of 213 entries took 107 FUSE requests from a cold mount (0.4.1-rc.1: 321). A repeat listing took 72.
  - 14 names have SFM private-use characters (12 colon, 2 question mark); all 14 open and read.
  - The shell reads WAV tags (title, artist, album, length) on 93 files from 268 reads; DSF shows no tags, as before. folder.jpg has a thumbnail, audio files have none. A thumbnail pass over all 196 files took 1,802 requests, linear in the file count (about 800 are directory opens and releases), no storm. 0.4.1-rc.1 counted 809 for the same pass. Run on 0.4.1, 0.5.0 and 0.6.0 in one VM (virtio-net), it counts 921 to 941, 508 to 514 and 2,104 to 2,166 requests; the listing alone is 327, 108 and 107. The extra 0.6.0 requests are 800 `OPENDIR` and `RELEASEDIR` pairs, from the container's `smb3 directory leases = no` (#205), not from how sacdfs answers `opendir`: with leases on, 0.6.0 counts 496 (details and the time on a larger tree in [deployment.md](deployment.md); [#312](https://github.com/aclater/sacdfs/issues/312)).
  - Robocopy `/MIR` copied 196 of 196 files with no failures. All 186 audio copies hash identically to the served files (and the served files hash the same as on 0.4.1); 21 of 21 files sampled by `Get-FileHash` also match. Run 1 needed two Robocopy invocations because the test harness stopped the first one at its 60-minute limit; the second copied the remaining 47 files and skipped 149. About 449,000 requests in all (0.4.1-rc.1: 465,000). A second `/MIR` pass copied 0 and skipped 196 in 0.1 s with 77 requests (0.4.1-rc.1: 342); sizes and times were equal for all 196.
  - RF64: one WAV of 4.07 GiB at `--pcm-rate 352800` has an RF64 header, reads at 4 GiB + 12,345 and hashes identically to the server's (one file over 4 GiB; 0.4.1-rc.1 used two larger ones).
  - A non-445 port works with `/TCPPORT` (18445 and 18446, one at a time).
  - Speed: the whole-share copy ran at about 15 MB/s here, against 65.8 MB/s on 0.4.1-rc.1, in an environment that was slower in general. A same-VM control (one 4.1 GiB DST disc, `/MT:1`) shows no version difference: rc.1 241 s and 267 s, 0.4.1 247 s, 0.5.0 256 s, with identical reads (33,515). So the slowness is the test VM (user-mode network), not 0.6.0-rc.1.
  - Not run: read-only write attempts, restart with a held handle, Explorer's large-icon view in a real window, non-ASCII CJK names, library changes. The 0.5.0 whole-share baseline was skipped on instruction.
- **Not run on 0.6.0-rc.1:** Roon and Plex at scale, foobar2000, JRiver and the Tier 2 tools.

## 0.5.0-rc.1 checks

Run on main at commit 826fa50, which is the 0.5.0-rc.1 code apart from the version and a test-only
fix (#272). These are the Tier 1 results for 0.5.0; 0.4.1 was the baseline, side by side. The table
above keeps each client's last full run.

- **Scripted servers,** the same images as for 0.4.1 (Jellyfin 12.4.0, Navidrome 0.64.2, Lyrion
  9.1.1, MPD 0.24.15, MinimServer 2.2): the same tracks, albums, art and bytes as 0.4.1.
- **Linux NFS 4.2 and SMB 3.1.1 mounts,** 22 checks per client: Fedora 44, RHEL 10.1, RHEL 9.7,
  Debian 13, Debian 12 and Ubuntu 26.04 pass all 22. Ubuntu 24.04 passes 21: `stat` sizes over SMB
  fail for the files with non-ASCII names, the known `linux-modules-extra` problem
  ([below](#linux-nfs-and-smb-mounts)), the same as on 0.4.1.
- **FUSE requests** (counted from sacdfs's request log): a scan by the scripted servers sends 2 to 20 %
  fewer than on 0.4.1. Over SMB, a first `ls -lR` of the 216-file test tree takes 226 requests,
  against 722 on 0.4.1 (#205).
- **Real discs** (main at the rc.1 code; a real library, read only):
  - `scripts/real-discs.sh` passes 3 of 3 on 40 images (12 multichannel, 28 stereo only), with every area's tracks equal to the area stream.
  - 466 DSF and WAV files from 12 discs are byte-identical to 0.4.0.
  - 5 multichannel WAV tracks of 4.2 to 7.9 GiB at 352.8 kHz are RF64 and byte-identical to 0.4.0. FFmpeg reads each correctly.
  - The whole library lists the same 4,911 DSF files as 0.4.0. The only differences are intended:
    - untitled tracks are named `NN - Track NN`, as since 0.4.1;
    - cover images were added;
    - folders that hold only art are hidden (#175).
- **Not run on 0.5.0:** Windows 11 (Explorer, PowerShell, Robocopy: the 0.4.1-rc.1 result stands),
  macOS, Roon and Plex at scale, foobar2000, JRiver and the Tier 2 tools.
- **Release binaries:** the x86_64 build targets glibc 2.17. Run in a UBI 7 container, the mount and
  the bytes read match those of a host mount, and the corpus check passes 457 of 457. The aarch64 build
  also targets glibc 2.17; its tests run natively in CI.

## 0.4.1-rc.3 checks

rc.3 changed where WAV reads are demultiplexed, so the clients that read WAV were re-run on it. These
are the only client results on rc.3. The table above keeps each client's last full run.

- **Linux NFS 4.2 and SMB 3.1.1 mounts,** 206 DSF and WAV files, 22 checks per client, including hashes
  of 19 files and ranges across 4 GiB of an RF64 WAV: Fedora 44, RHEL 10.1, RHEL 9.7, Debian 13, Debian
  12 and Ubuntu 26.04 pass all 22. Ubuntu 24.04 passes 21; `stat` sizes over SMB fail for 4 files with
  non-ASCII names, the known problem.
- **Navidrome, Lyrion and MPD,** `dsf,wav`: 186 files and 7 albums each, matching rc.1.
- **Jellyfin: not verified on rc.3.** The test harness never created the library, so the run says
  nothing about sacdfs. It was re-run on the 0.4.1 release instead and matched; see [Jellyfin](#jellyfin).
- **Roon and Plex:** their test deployments run the rc.3 image and the share lists the library. No
  player was driven on rc.3.
- **Not run on rc.3:** Windows, macOS, foobar2000, JRiver, MinimServer and the Tier 2 tools.

## How the 0.4.0-rc.2 tests were run

The Jellyfin, Navidrome, LMS, MPD and MinimServer results below were measured on
sacdfs 0.4.0-rc.2 (the release binary), on a test host with a 14-core Intel CPU, the host of
the 0.3.0-rc.1 tests, with the same client versions. For each server:

- sacdfs ran on the host as the user, with the documented options `--format dsf --flatten`
  and `--threads 4` (and `--format dsf,wav` where that is compared), at the **default cache**
  (32 MiB since 0.4.0; the 0.3.0-rc.1 tests set `--cache-mb 16`), and a fresh index cache for
  each run;
- the server ran in a rootless Podman container that bound the mount's **parent** directory
  with `ro,rslave`, as recommended under [General recommendations](#general-recommendations);
- the library was the 5 test images, laid out `Artist/Album/disc.iso`: 93 tracks in 7 album
  areas (5 Stereo, 2 Multichannel). 3 of the 5 discs have a cover beside the image (two named
  `folder.jpg`, one `cover.jpg`), so 4 of the 7 album areas have a cover, which sacdfs also
  places inside each area folder;
- scans were timed with sacdfs's FUSE request log on (`RUST_LOG=fuser::request=debug`), which
  is how opens, reads and bytes served were counted.

Other jobs shared the test host during most of these runs, so its times are given only where
the host was quiet or where load does not change the result: Navidrome's scans and LMS's
`dsf` scan ran on a quiet host; the MPD scan times were measured again on a quiet 16-core AMD
desktop. Times taken under load are marked as such; counts, art, names, playback and memory do
not depend on it.

## How the 0.4.1-rc.1 tests were run

The five scripted servers were run on the release binary of 0.4.1-rc.1 and, alternating, on the
0.4.0 release binary, with the same images (no client changed since the 0.4.0-rc.2 tests: Jellyfin
12.4.0, Navidrome 0.64.2, Lyrion 9.1.1, MPD 0.24.15, MinimServer 2.2) and the same setup as
[above](#how-the-040-rc2-tests-were-run): the 5 test images, 93 tracks, 7 album areas, `dsf` and
`dsf,wav`, `--flatten`, the default cache, a fresh index cache, a rootless Podman container with
the mount's parent bound `ro,rslave`. The first pass ran while other jobs shared the test host;
the timings below are from a repeat on a quiet host, in the order 0.4.0, rc.1, rc.1, 0.4.0 (the
first 0.4.0 run and the first rc.1 run are marked where they differ).

Every server is judged against the 12 behaviours of the [test plan](test-plan.md#behaviour-every-client).
What the scripts check, and so what is "not checked":

| Behaviour | Result on rc.1 | Checked by the script? |
|---|---|---|
| 1 Found | 93 of 93 (`dsf`), 186 of 186 (`dsf,wav`) on Jellyfin, Navidrome, Lyrion, MPD; 93 of 93 on MinimServer | Yes |
| 2 Metadata | Codec, channels (2, 5, 6) and a non-zero duration on every file; track numbers and titles on Jellyfin and Navidrome (title and year on the rest were not compared) | Yes for the first three; counts only for the others |
| 3 Albums | 7 albums, the 2 multichannel areas as "… (Multichannel)", no merged or duplicate albums (the extracted copy gives 5 distinct names, as before) | Yes |
| 4 Art | Areas with a cover: MPD 4 of 4 (0.4.0: 1 of 4), MinimServer 4 of 4, Lyrion 4 of 7 albums, Navidrome one cover per album (identical bytes to 0.4.0, although both `cover.jpg` and `folder.jpg` now appear in the areas), Jellyfin 5 or 6 of 7 (it fetches online art for some) | Yes |
| 5 Playback | MPD plays DSD (`dsd64:2`, `dsd64:6`); Lyrion starts DSF as DSD to PCM; Navidrome streams raw and transcodes a 5.1 track to MP3; Jellyfin transcodes whole tracks; MinimServer streams a 447 MB 5.1 DST file in full | Partly: MPD and Lyrion for seconds, not to the end |
| 6 Seek | MPD to 100 s; Jellyfin transcode from 60 %; a 1 MiB range at mid-file of MinimServer in 3.4 ms; Navidrome range read in 22 ms | Partly: not Lyrion |
| 7 Quiet rescan | 0 opens and 0 reads on Lyrion, MPD and MinimServer after a restart or an update; Navidrome full rescan after a sacdfs restart 2.0 s | Yes |
| 8 Library changes | Navidrome followed a renamed image and a removed one (93 files, 18 marked missing, 7 then 6 albums); no server was shown a new image or a new sidecar (the sidecar case is in the file-system checks) | Partly |
| 9 Restart | A sacdfs restart under a running Navidrome and a bound parent directory: listing and reads work, the direct bind of the mount point does not recover (documented); not during playback or a scan | Partly |
| 10 Output version change | Not checked with a server; the file-system check shows `--pcm-cutoff` changes the WAV files' size and time and no DSF file | **Not checked** |
| 11 Load on sacdfs | Peak RSS 46 to 47 MiB (MinimServer, Navidrome), 0.4.0: 43; CPU 20 to 25 s per scan; Jellyfin's LUFS task reads 39.6 to 41 GiB of 22 GiB, as before | Totals only: not the requests by kind |
| 12 Logs | No WARN or ERROR line in sacdfs's log in any run; the servers' own logs were not read | sacdfs's side only |

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

**0.4.1-rc.1** (Roon 2.74 build 1719, same NAS, SMB, whole library). Moving from 0.4.0 changed 241 track names, retagged 30 files, hid 11 art-only folder trees holding no audio and added 359 `cover` images. Roon's scan matched a tree comparison of the two versions: 4,902 DSF files on both and no album lost. A Force Rescan afterwards took 3.2 s, reported no changes and sent 19,324 FUSE requests, none for extended attributes. A periodic scan 4 hours later also reported no changes. Playback continued through a sacdfs restart. Peak RSS over about 4 hours was 88 MB with the default cache, in light use. Not tested: a restart during a scan, rollback.

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

**0.4.1-rc.1** (Plex 1.43.5, same NAS, SMB, WAV, flattened, 5,132 tracks in 466 albums). Upgrading from 0.4.0 named all 279 untitled tracks, and "Various Artists" albums with no album name went from 7 to 0. 3 albums (26 tracks) still show a blank album title in Plex although sacdfs serves one; the cause is not established. Plex re-read the changed files once: 140 s against 20 s for a scan with no changes, 18 GiB served, 643 CPU-seconds and 160 MB peak RSS in sacdfs. A later scan with no changes read almost nothing. Renamed tracks get new Plex items. Rolling back to 0.4.0 and forward again restored every count and path, and the 2 tracks with play counts kept them; none of the renamed tracks had plays, so whether a play carries across a rename is untested. Multichannel copies stayed separate with the documented setting. Playback through a Plex client was not tested on this candidate.

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

**0.4.1** (release binary, generic x86-64; same image 12.4.0, 2 CPUs, same 5 images): two fresh scans per format on a quiet host gave the same counts, albums, art and bytes served as 0.4.0-rc.2. 93 of 93 with `dsf` and 186 of 186 with `dsf,wav`; 7 albums, each multichannel area its own "… (Multichannel)" album; 6 of 7 with art. The first scan read 17.9 MiB with `dsf` and 648.5 MiB with `dsf,wav`. sacdfs peaked at 30 MiB RSS with `dsf` and 52 MiB with `dsf,wav`, with no errors or warnings in its log. Scans took 43 to 47 s with `dsf` and 45 to 53 s with `dsf,wav`. A 5.1 DST track and a stereo track transcoded to MP3 for their full length; a transcode started at 60 % sent its first bytes in under 10 ms, and a direct read of a file's first 4 MiB took 4 to 17 ms. LUFS normalization was not re-measured. The earlier "0 albums" runs were a race in the test script: it added the library while Jellyfin's own startup scan ran, and that scan dropped the refresh. The script now waits for Jellyfin to be idle, adds the library, then starts the scan itself.

**0.4.1-rc.1:** same image (12.4.0), 93 of 93 with `dsf`, 186 of 186 with `dsf,wav`, 7 albums with distinct names (2 "… (Multichannel)"), art for 5 or 6 of 7 (it fetches online art for some); first scan on a quiet host 52.5 s on both 0.4.0 and rc.1 (one run each); the LUFS task 508 s on rc.1 against 478 s on 0.4.0 (+6 %, one run each, 39.6 and 39.8 GiB served). Twice, 0.4.0 was the first Jellyfin start of a run and reported 0 albums after 8 s while sacdfs saw no reads; the same binary worked on its next run, so this is taken as a race in the test script, not a sacdfs result.


**Tested on 0.4.0-rc.2** (and 0.3.0-rc.1). Jellyfin 12.4.0 (official
image, 2 CPUs), one music library.
- *Tracks:* 93 of 93 with `dsf`, 186 of 186 with `dsf,wav`; codec, channels (2, 5 and 6),
  track numbers and durations set for every file.
- *Albums:* 7, all names distinct; the 2 multichannel areas are their own albums,
  "… (Multichannel)". (The extracted copy, which has no suffix, gave 7 albums with only 5
  distinct names.)
- *Art:* the 4 album areas with a cover use it, the Multichannel one included (the image's
  path is the copy in the area folder). Jellyfin fetched online art for 2 of the other 3.
- *Cost of WAV:* during the first scan sacdfs served 17.9 MiB for the 93 DSF files (about
  200 KB each) and 648.5 MiB with `dsf,wav`: ffprobe reads about 7 MB of each WAV, which
  sacdfs must convert. The bytes are the same as on 0.3.0-rc.1. sacdfs peaked at 30 MiB RSS
  with `dsf` and 53 MiB with `dsf,wav` at the default cache, and used about 1 CPU-second
  against 33. Jellyfin's own work dominates its scan time (51 to 53 s with `dsf`, 57 to 61 s
  with `dsf,wav`, about 45 s for the extracted copy, two runs each, on a shared host), so the
  times vary too much to compare closely.
- *Playback:* a 5.1 DST track and a stereo track transcoded to MP3 for their full length
  (sizes match their durations at 256 kbit/s); a transcode started at 60 % of the track sent
  its first bytes in 5 to 9 ms.
- *LUFS normalization:* turned on for the `dsf` library and run as Jellyfin's
  "Audio Normalization" task, it opened every file twice and read 40.0 GiB from sacdfs for a
  library of 22.0 GiB of DSF files, decoding DST all the while; sacdfs peaked at 52 MiB RSS at
  the default cache. It took 774 s on the shared host (464 s on a quiet one on 0.3.0-rc.1;
  the same read pattern costs the same CPU on 0.3.0 and 0.4.0-rc.2 on a quiet host).

**Recommended setup.**
- `--format dsf`: it saves sacdfs converting about 7 MB of PCM per WAV file on every first
  scan or rebuild ([#96](https://github.com/aclater/sacdfs/issues/96)).
- Leave LUFS normalization off (the default): with it on, Jellyfin reads about twice the
  whole library, and sacdfs must decode all of it.
- Bind the mount's parent directory with `rslave`.

## Navidrome

**0.4.1-rc.1:** same image (0.64.2), 93 of 93 and 186 of 186, 7 albums, a cover for every album with the same bytes as on 0.4.0 (the area folders now hold both `cover.jpg` and `folder.jpg`, and Navidrome serves one picture); a renamed image and a removed one were followed (93 files, 18 marked missing, 6 albums present); a full rescan after a sacdfs restart took 2.0 s. In the first pass, on a shared host, a raw range read of 1 MiB took 22 ms and an MP3 transcode of a 5.1 track 11.7 s (0.4.0: 50 ms and 12.4 s); sacdfs peaked at 47 MiB RSS (0.4.0: 43).


**Tested on 0.4.0-rc.2** (and 0.3.0-rc.1). Navidrome 0.64.2 (official image). First scan
0.42 s with `dsf` (Navidrome's figure), 93 of 93 files, no errors; 0.54 s and 186 of 186
with `dsf,wav`. Album art: **4 of 4** album areas with a cover show it, the Multichannel one
included (the copy in each area folder); the other 3 get Navidrome's placeholder. 7 albums:
the 2 multichannel areas are separate albums named "… (Multichannel)". DSF and WAV of a track
share an album. Durations set for all files. sacdfs peaked at 18 MiB RSS with `dsf` and
24 MiB with `dsf,wav`, at the default cache. After sacdfs was stopped and started again under
the running container, the parent bind showed all 93 files again and a full scan went through. On
0.3.0-rc.1, a full re-scan with nothing changed opened the files but read nothing
from sacdfs (0 reads); that was not counted again.

*Renamed and removed images* ([#153](https://github.com/aclater/sacdfs/issues/153), new in
0.4.0), on 0.4.0-rc.2 under the running container: an image's folder renamed on the source
showed under its new name inside the container within 5 s, and a full scan then listed 93 files
with none missing; a disc's folder removed from the
source was gone within 5 s, and the next full scan marked its 18 files missing and left 6
albums. Afterwards sacdfs held 4 images open, not the removed one, and the renamed one under its new
path; it stayed at 18 to 19 MiB RSS.

*Streaming* (on 0.4.0-rc.2, under load): a raw stream request with a range returned `206` and
`audio/x-dsf` in 41 to 46 ms (26 ms on a quiet host on 0.3.0-rc.1); an MP3 transcode (FFmpeg,
on request) of a 5.1 DST track returned `200` and an MP3 stream. On 0.3.0-rc.1 a whole 5.1
track (10.1 MB of MP3) took 7.6 s.

**Recommended setup.** `--format dsf` (or `dsf,wav` if you want both), `--flatten`,
the default `--cache-mb` (32 since #150; tested with it on 0.4.0-rc.2, and with 16 on
0.3.0-rc.1). Keep the default `(Multichannel)` album suffix if you want Stereo and
Multichannel as separate albums.

## Lyrion / Logitech Media Server

**0.4.1-rc.1:** same image (9.1.1), 93 of 93 and 186 of 186, 7 albums, artwork for 4 of 7 (the same as 0.4.0); a first scan took 1.1 s in both and 101 opens and 192 reads (`dsf`); a rescan with nothing changed made 0 opens and 0 reads; DSF started as DSD-to-PCM for 2 and 5 channels.


**Tested on 0.4.0-rc.2** (and 0.3.0-rc.1). Lyrion Music Server 9.1.1 (official image), scanned
and browsed through JSON-RPC, with squeezelite 2.0 (Fedora package, DSD support) in the same
pod as the player. First scan (`wipecache`) 1.1 s for 93 of 93 files with `dsf`; 186 of 186
with `dsf,wav` (1.8 to 2.4 s on a shared host; 1.1 s on a quiet one on 0.3.0-rc.1); a
`rescan` with nothing changed made 0 opens and 0 reads. Album art: **4 of 4** album areas with
a cover have it (the copy in the area folder); 7 albums, the 2 multichannel areas as
"… (Multichannel)". Durations set; DSF bitrate shows as 0. sacdfs peaked at 42 MiB RSS with
`dsf` and 46 MiB with `dsf,wav`, at the default cache. DSD playback is native: the stock
`convert.conf` has only `dsf dsf` (pass-through) for DSF, so a player without DSD gets nothing
unless the third-party DSD Player plugin is added. squeezelite received the stereo DSF and
decoded it itself (DSD to PCM); the 5.1 DSF stopped at once, because squeezelite is
stereo-only (not sacdfs).

**Recommended setup.** `--format dsf` (or `dsf,wav`), `--flatten`. The official image runs
LMS as an unprivileged user (uid 499 by default), which cannot read a FUSE mount made without
`allow_other` ("Permission denied" in the test), and LMS refuses to run as root unless told
to (it falls back to `nobody`). For a rootless container reading a direct mount, set
`PUID=0`, `PGID=0` and `EXTRA_ARGS="--user root"` (root in a rootless container is the mount's
owner); that is what the test ran. Reading the container's SMB or NFS share instead was not
validated with LMS.

## MPD, moOde, Volumio

**0.4.1-rc.1:** MPD 0.24.15 in the same container. 93 of 93 and 186 of 186, 7 albums, 2 "… (Multichannel)". **Art ([#148](https://github.com/aclater/sacdfs/issues/148)): `albumart` found the cover in 4 of 4 album areas that have one, against 1 of 4 on the 0.4.0 release binary** (the 3 areas without a cover still have none). First scan with `dsf`: 0.47 s on rc.1, 0.46 s on 0.4.0; `mpc update` with nothing changed 0.05 s, 0 opens. DSF plays as `dsd64:2` and `dsd64:6`, and `seekcur 100` worked in both.


**Tested on 0.4.0-rc.2** (and 0.3.0-rc.1). MPD 0.24.15 (Fedora 44 package, in a container),
null output with `sync yes`. moOde and Volumio use MPD, but neither was run itself. MPD's
first scan (at start, empty database), measured on a quiet desktop host (three runs each,
alternating): **0.34 s** for 93 files with `dsf` and **3.6 s** for 186 with `dsf,wav`, where
sacdfs used about 12 CPU-seconds converting WAV for FFmpeg's probe and peaked at 53 MiB RSS;
with `dsf` it used under 0.1 CPU-seconds and 12 MiB. (0.3.0-rc.1, on the test host: 0.55 s
and 5.6 s.) 7 albums, 2 of them "… (Multichannel)". `mpc update` with nothing changed:
0.07 s, 0 opens. DSF plays as DSD (`audio: dsd64:2`, `dsd64:6`; DoP or native DSD to a
capable output, otherwise MPD converts); a seek to 100 s in a 5.1 DST DSF worked at once.
Album art: on 0.4.0-rc.2, `albumart` found the cover in **1 of 4** album areas with one: the
disc whose cover is named `cover.jpg`. MPD looks only for files named `cover.*` in the track's
folder, so `folder.jpg` is not used. **After [#148](https://github.com/aclater/sacdfs/issues/148)**
(tested on the branch that adds it, built from the 0.4.1 work; MPD 0.24.15 in the same
container setup, 5 test discs, 93 tracks, 7 album areas of which 6 have a cover, one named
`folder.jpg` on two discs, `front.png` on one and `cover.jpg` on one): the 0.4.0 release
binary gave art for 1 of 6 areas, the branch for **6 of 6**, and the area of the disc with no
cover still has none. When the art has no `cover.*`, sacdfs shows one of its images also as
`cover.jpg` (or `cover.png`, `cover.webp`) inside each area folder, as the same file.

**Recommended setup.**
- `--format dsf`: the first scan was about 10 times as fast as with `dsf,wav`. The WAV cost is
  FFmpeg's 5,000,000-byte probe of each WAV file ([#96](https://github.com/aclater/sacdfs/issues/96)).
  If another client needs WAV, run a second container or mount with `dsf,wav` for it.
- `--flatten`; bind the mount's parent with `rslave` if MPD is in a container.
- Art needs no renaming: sacdfs shows a folder's cover also as `cover.*` in the areas. (A file
  named `Cover.PNG`, in other case, counts as a cover and is not aliased; untested with MPD.)

## MinimServer

**0.4.1-rc.1:** 2.2 (the same image), 93 audio files in 17 folders, art on 63 of 93 items (4 of 4 album areas with a cover), 7 albums; a restart with its cache made 0 opens and 0 reads; a 1 MiB range at mid-file answered in 3.4 ms, and a whole 5.1 DST DSF (447 MB) streamed in 5.6 s on rc.1 and 5.3 s on 0.4.0 (quiet host, one run each); sacdfs peaked at 46 MiB RSS (0.4.0: 43).


**Tested on 0.4.0-rc.2** (and 0.3.0-rc.1). MinimServer 2.2 (official image, starter licence),
browsed with UPnP ContentDirectory `Browse` calls. The scan finished within 1 s (every file
opened in the same second; "93 audio files in 17 folders"); a restart with its cache made 0
opens and 0 reads. sacdfs peaked at 44 MiB RSS at the default cache. Album art: **4 of 4**
album areas with a cover have `upnp:albumArtURI` on their items (63 of 93 items), served
from the copy in the area folder; 7 albums, 2 of them "… (Multichannel)". Items are served as
`audio/x-dsf` with ranges: a 1 MiB range at mid-file answered `206` in 4 to 8 ms, and a whole
5.1 DST DSF (447 MB) streamed in 6.4 to 9.3 s on the shared host (4.5 s on a quiet one on
0.3.0-rc.1).

**Recommended setup.** `--format dsf`, `--flatten`. DSD is served as is; a player that needs
transcoding needs MinimStreamer, which was not tested. `dsf,wav` was not tried with
MinimServer.

## Windows Explorer, PowerShell and Robocopy

**Tested.**

*0.2.0-rc.1:* Windows 11 build 26200, SMB 3 to the container on the same host with user
authentication, sacdfs 0.2.0-rc.1, `dsf,wav`, `--flatten`, 93 DSF + 93 WAV (54.96 GB).
- *Explorer and PowerShell:* a recursive listing of 198 entries in 0.1 s; 14 of 186 names have
  SFM private-use characters, listed and opened normally (they show a placeholder glyph, as
  documented). The details pane reads WAV tags (artist, album, year, track, title, length);
  DSF shows size, date and type only, because Windows has no DSF property handler. Copies of one
  DSF (120 MB/s) and one WAV (91 MB/s) had a SHA-256 identical to the served file (2 of 2).
- *Robocopy `/MIR`:* run 1 copied 186 of 186 files with no failures in 899 s (65.6 MB/s, bound
  by DST decoding); sacdfs used 2,212 CPU-s and peaked at 507 MB RSS at the default cache.
  Run 2 copied 0 and skipped 186 in 0.1 s, because sizes and times are stable.

*0.4.1-rc.1:* Windows 11 build 26200, SMB 3.1.1 to the container on the same host with user authentication, sacdfs 0.4.1-rc.1, `dsf,wav`, `--flatten`, 93 DSF + 93 WAV (54.96 GiB) plus 13 art files.

- Explorer and PowerShell: a recursive listing of 213 entries in 0.1 s (321 requests). 14 names have SFM private-use characters (12 colon, 2 question mark); all open and read. The shell reads WAV tags (artist, album, year, track, title, length) from 3 reads per file; DSF shows size, date and type only, because Windows has no DSF property handler. folder.jpg has a thumbnail; audio files have none. A thumbnail pass over all 196 files cost 809 requests (278 reads); no request storm.
- Copies and hashes: 186 of 186 audio files copied by Robocopy hash identically to the served files (93 DSF, 93 WAV); 20 of 20 sampled files read by Get-FileHash also match.
- Robocopy /MIR: run 1 copied 196 of 196 files in 896 s (65.8 MB/s) with no failures, 465,000 requests; the sacdfs process peaked at 110 MB RSS (default cache) and the container used about 2,500 CPU-s. Run 2 copied 0 and skipped 196 in 0.1 s (342 requests); sizes and times were equal to the source for all 196.
- Read-only: create, write, rename, move, delete and attribute changes fail in under 50 ms; two (delete folder, set read-only) report "an unexpected network error" instead of access denied.
- RF64: two WAV files of 8.49 GB and 6.15 GB at `--pcm-rate 352800` read, seek past 4 GiB and hash identically to the server's.
- Restart: a handle open during a sacdfs restart fails; the mapping works again without remounting.
- Windows can use another SMB port: `net use S: \\host\sacd /user:... /TCPPORT:18445` worked on this build (one port per server name). The deployment guide said Windows can only use 445; that is wrong for this build and is corrected there.

Not checked: Explorer's large-icon view in a real window, a real audio output, Japanese, Chinese or Korean names, library changes, an output-version change.

**Robocopy speed ([#225](https://github.com/aclater/sacdfs/issues/225)).** Robocopy from a Windows VM ran at one of two speeds, 3 to 4.5 times apart. The test VM's emulated network card caused it, not sacdfs, Samba, Robocopy or the sacdfs version. Windows 11 25H2 ran in a VM (4 vCPUs configured, of which Windows used 2; 8 GB) and the container in a Fedora 44 VM (8 vCPUs) on one host's libvirt bridge. Each run copied one DST disc (61 files, 19.8 GiB) with `robocopy /MIR /MT:1` from a fresh container over a new SMB connection:

| Windows VM network card | Runs | Robocopy time | Slow runs |
|---|---|---|---|
| e1000e (QEMU's emulated Intel 82574L), receive-side scaling (RSS) on, the Windows default | 29 (3 with other Robocopy flags, below) | 94-152 s or 349-483 s | 15 of 29 |
| e1000e with RSS off (`Disable-NetAdapterRss`) | 6 | 83-87 s | none |
| virtio-net (Red Hat NetKVM driver, virtio-win 0.1.285) | 6 | 80-89 s | none |

The same comparison on a quiet host, the versions alternated and the order flipped each round, 3 rounds (seconds per run):

| Windows VM network card | 0.4.0 | 0.4.1 |
|---|---|---|
| e1000e, RSS on | 406, 93, 92 | 405, 407, 92 |
| e1000e, RSS off | 85, 85, 85 | 84, 85, 84 |
| virtio-net | 81, 81, 81 | 80, 80, 80 |

Both versions land in both groups on the e1000e card with RSS on, and they match within 1 s on the other two cards. The container used 513 to 554 CPU seconds a run with either version.

- **The Windows client never went serial.** In fast and slow runs alike, Windows kept 7 of 8 reads of 1 MiB outstanding (the SMB client's queue length), with no credit stalls, a read-write-handle lease on the audio files and one channel. The earlier "one read at a time" was inferred from the order of sacdfs's reads, not seen on the wire.
- **In a slow run the server was waiting for the network.** The server's TCP connection was sending 99% of the time with about 2 MB queued. Samba had 0.1 to 0.2 reads waiting on sacdfs on average (about 2 in a fast run), and sacdfs used 1.0 to 1.8 decoder threads because the requests came slowly. Windows's first CPU spent 75-93% of its time in network receive (deferred procedure calls, DPCs) in both kinds of run, but moved about 5 times less data in a slow one.
- **sacdfs isn't involved.** A plain Samba share of 20 files (6.3 GB, in the server's page cache) on the same server VM showed the same split: 22-26 s or 132-136 s, 8 of 20 runs slow.
- **Robocopy flags don't change it.** `/J` (unbuffered), no `/MT` and `/MT:8` sent the same pipelined 1 MiB reads, and a slow run stayed at about 60 MB/s with `/MT:8` and 60 reads outstanding. Host load didn't decide it either: slow runs happened with the host's load average under 4 and over 40.
- **With RSS off or a virtio-net card, no run was slow**, and sacdfs's DST decoding became the limit (about 5.5 decoder threads busy, 238-267 MB/s). The plain share took 36-42 s on the e1000e card with RSS off and 8-9 s over virtio-net.

The runs point to the RSS queue that a new TCP connection lands on as what sets the speed: turning RSS off removed the slow runs, and in slow runs of the plain share Windows's second CPU ran about 90 DPCs a second against 450-490 in fast ones. The emulator's queue handling wasn't traced further. **Recommendation for timing Windows copies in a VM:** give the VM a virtio-net card with the NetKVM driver, or turn RSS off on an emulated e1000e card. Nothing needs to change in sacdfs, Samba or Robocopy. Not run on a physical Windows client or network, so whether a physical card shows anything like it is unknown. Over QEMU's user-mode network, runs of both versions also stalled for minutes at about 3 MB/s with the server idle; avoid that network when timing copies.

**Recommended setup.** Set `SMB_USER` and `SMB_PASSWORD`: Windows 11 24H2 and later refuse
guest access. See [Windows](deployment.md#windows) and
[file names over SMB](deployment.md#file-names-over-smb).

## foobar2000

**Tested.** *0.4.1-rc.1:* foobar2000 2.26.2 portable with foo_input_sacd 2.0.27, Windows 11 build 26200, SMB 3.1.1, sacdfs 0.4.1-rc.1, driven through foo_beefweb 0.10. Adding the whole share found 186 files in 4 s (1,494 requests, 186 opens, 345 reads) with title, artist, album and length on every file; DSF decode as DSD64 and WAV as PCM 176.4 kHz 24-bit; stereo, 5ch and 6ch are recognised. folder.jpg art is shown where the folder has it. Playback (emulated sound device with no output, so audio was not heard) starts in 0.33 to 0.38 s for stereo, 5ch and 6ch; seek to 60% works and playback continues; a track played to its end and the next began. Not checked: audio quality and DST multichannel output.

*0.2.0-rc.1* (partial): foobar2000 2.26.2 portable on Windows 11, SMB 3, sacdfs 0.2.0-rc.1.
The SACD/DSD component could not be installed unattended, so only the stock decoder was
tried. Adding one area folder and opening one DSF caused 25 opens and 65 reads and no errors in
sacdfs's log. **Not verified:** tags, durations, art, seek and multichannel naming (no GUI
evidence). This is neither a pass nor a fail.

## JRiver Media Center

**Partly tested.** *0.4.1-rc.1:* JRiver Media Center 36.0.38 trial (installs and runs without an account), Windows 11 build 26200, SMB 3.1.1, sacdfs 0.4.1-rc.1. Importing the share with default options found 186 audio files (7 albums, 5 artists) in 14 s (1,958 requests: 384 opens, 91 reads) with artist, album, title and track tags; artist art shows for 3 of 5 artists. A DSF starts playing (converted to PCM; the emulated sound card needed the output rate lowered from 352.8 kHz to 176.4 kHz). Not checked: seek, playing to the end, multichannel, and a quiet rescan.

*0.2.0-rc.1:* untested. The trial download failed in the test script (a bad URL) and no interactive trial
was done. It needs a manual run (JRiver Media Center 36 was the planned version).
## FFmpeg, sox, mediainfo, ExifTool, mpv and VLC

**Tested on 0.4.1-rc.1** (the release binary; Debian 13 packages in a container with the mount's
parent bound `ro,rslave`): FFmpeg and ffprobe 7.1.5, sox 14.4.2, MediaInfo 25.04, ExifTool 13.25,
mpv 0.40.0, VLC 3.x (`cvlc`, dummy interface). The test library was the 5 test images, with
`--format dsf,wav --flatten`: 93 DSF and 93 WAV files; one track from each of the 7 areas was
checked in both formats (14 files), against the duration in the DSF header.

| Tool | DSF | WAV |
|---|---|---|
| ffprobe | Duration within 0.5 s of the header, title, artist, album and track tags, codec `dsd_lsbf_planar` | Same duration (within 0.6 s), tags present |
| ffmpeg | Seek to 60 % and decode 5 s: no errors on all 7 | Same, 7 of 7 |
| MediaInfo | Duration and tags, 7 of 7 | 7 of 7 |
| mpv | Seek to 60 % and play 3 s: 7 of 7 | 7 of 7 |
| sox | Not supported (`no handler for file extension dsf`) | `soxi` duration, tags and `trim` seek, 7 of 7 |
| ExifTool | "Unknown file type": it has no DSF reader, and gives the same on a plain copy | Tags and duration, 7 of 7 |
| VLC | This build cannot decode DSD (the same on a plain copy of a file) | Plays 3 s from 60 %: 7 of 7 |

Nothing here is specific to sacdfs, except that every result is the same as for a plain copy of
the file. Not checked: Windows and macOS builds of these tools, and 0.4.0 side by side (the
results do not depend on the sacdfs version, since the bytes of the files are unchanged).

## Linux NFS and SMB mounts

**Tested on 0.4.1-rc.1** (re-run on 0.5.0-rc.1, [above](#050-rc1-checks), with the same result; the published image, rootful Podman in a Fedora 44 VM; 0.4.0 was
the baseline for the restart test). The server held the 5 test images and one multichannel
disc whose WAV file passes 4 GiB (`SACDFS_PCM_RATE=352800`): 103 DSF and 103 WAV files, 206 in
all. Each client mounted NFSv4.2 on the default port and SMB 3.1.1 on a custom port, in a VM
with its own kernel and `nfs-utils` and `cifs-utils`:

| Client | Kernel | Result |
|---|---|---|
| Fedora 44 | 6.19 | All checks pass |
| RHEL 10.1 | 6.12 | All checks pass |
| RHEL 9.7 | 5.14 | All checks pass |
| Debian 13 | 6.12 | All checks pass |
| Debian 12 | 6.1 | All checks pass |
| Ubuntu 26.04 | 7.0 | All checks pass |
| Ubuntu 24.04 | 6.8 | All pass except that non-ASCII names are shown in ISO-8859-1 over SMB (below) |

The checks, on each of NFS and SMB: `find` lists all 206 files; `ls -lR` exits 0 with no
error output (the NFS half of [#174](https://github.com/aclater/sacdfs/issues/174));
`stat` sizes of all 206 files equal the server's; `df` reports a non-zero size; `du` works; `getfattr -d` on a file returns
at once with no attributes; `touch`, `mkdir`, `rm`, `mv`, `truncate` and an append all fail
at once, none hangs; 19 sample files (DSF, WAV and the over-4 GiB WAV) hash like the server's
read; five 1 MiB ranges of the over-4 GiB WAV, at, across and past 4 GiB, equal the server's, and
its header is `RF64`; four readers of one file plus a tagger reading the head and tail of 40
files all finish (slowest reader 0.2 to 1.0 s for 60 MiB, over a virtual network, cold).

- **Ubuntu 24.04 over SMB:** the cloud image has no `nls_utf8` module (it is in
  `linux-modules-extra-$(uname -r)`), so the kernel's CIFS client falls back to ISO-8859-1 and
  shows a name with a non-ASCII character (an acute accent) as a single byte, which a path from
  the server then does not match (`stat` fails for 4 of 206 files). With the package installed
  the 4 files are found. Ubuntu 26.04 was fine. This is the client's packaging, not sacdfs.
- **Read-ahead of the kernel SMB client** ([#266](https://github.com/aclater/sacdfs/issues/266), 0.6.2,
  Fedora 44 client, kernel 6.19, against a NAS): by default the client reads 4 MiB at a time and
  reads ahead up to 8 MiB, so a library scan that wants each file's header and tags makes sacdfs
  read, decode and serve about 1.8 MB per file. `rsize=262144` on the mount brings that to 262 KB,
  with faster copies and unchanged player reads; `cache=none` to 99 KB, with each player read one
  round trip. `rasize` can't go below `rsize` (the kernel refuses the mount). Figures:
  [performance](performance.md#client-settings-on-a-nas-062); advice:
  [deployment](deployment.md#linux).
- **Extended attributes** ([#174](https://github.com/aclater/sacdfs/issues/174)), counted from
  sacdfs's FUSE request log (the published 0.4.0 and 0.4.1-rc.1 images under rootful Podman,
  alternating, two runs each, Fedora 44 client): `ls -lR` over NFS, which exited 0 with no
  errors, caused 1 `GETXATTR` and 238 `LISTXATTR` requests for the 206 files, the same on both
  versions; over SMB, `ls -lR` caused none of either. There is no request storm. (Roon's own
  Force Rescan was not part of this run.)
- **RF64** (a real 6-channel WAV of 4.07 GiB, `--pcm-rate 352800`, one disc, the smallest of the
  11 in the library that pass 4 GiB): the file hashes the same through FUSE as inside the container
  and over NFS and SMB (above). Read through the FUSE mount: ffprobe, FFmpeg (seek to 680 s and
  decode), sox 14.4.2 (`soxi`, `trim` past 4 GiB), MediaInfo, ExifTool, mpv and VLC (decode from
  680 s) all handled it, and Navidrome imported it (size 4,374,335,960 bytes, duration 688.8 s).
  A full FFmpeg decode of the whole file was not run. Jellyfin, Lyrion, MPD and MinimServer
  were not tried on it.
- **fstab and systemd** (Fedora 44, 0.4.1-rc.1): the documented fstab line for a local mount
  (with `mount.fuse` linked) mounted with `mount -a` and after a reboot; the documented
  `Type=notify` unit started, survived a reboot, and stopped without a failed state (a
  non-root user gets "Permission denied" on its mount without `allow_other`, as documented);
  the NFS and SMB lines of [the Linux client guide](install/linux-client.md) (with `port=` for
  SMB) mounted, were mounted after a reboot with the server up, the boot finished in 56 s with
  the server down, and the SMB automount worked once it was back.
- **Restart of the container:** [NFS and restarts](deployment.md#nfs-and-restarts) and, including macOS, which needs a remount after every upgrade or restart (the documented answer on #204), [SMB and restarts](deployment.md#smb-and-restarts).
- **Container channels:** see [the Docker and Podman guide](install/docker-podman.md).

Cold-seek and read-latency figures for NFS and SMB, server-side and client-side, are in
[performance](performance.md#client-side). The mount commands are in the
[deployment guide](deployment.md#linux). macOS mounts are documented but have no test report
on #82.

## General recommendations

- **Deploy server-side on the NAS.** sacdfs runs next to the images in the container and the
  clients see ordinary files over SMB or NFS. A cold seek costs about 5 to 6 ms over SMB on a
  gigabit LAN ([deployment](deployment.md)).
- **The default cache suits a NAS.** Since #150 the default `--cache-mb` is 32 (was 256; since 0.5.0, 16 under a memory limit up to 512 MiB), and
  a Plex-like scan of the bench images peaks at 46 MiB RSS (0.3.0 at its default: 103 MiB), a
  Roon-like import replay at 45 MiB (134 MiB). The 0.4.0-rc.2 server tests ran at the default,
  with no errors and sacdfs at 18 to 53 MiB RSS (Jellyfin's LUFS pass: 52 MiB). On
  0.3.0-rc.1, MPD's first scan with `dsf,wav` peaked at 300 MiB RSS at the old default of
  256 MiB against 58 MiB at 16, in the same 5.6 s. 16 still works (the 0.3.0-rc.1 tests used
  it); 32 keeps several fast readers of one image at full speed. See
  [Memory](deployment.md#memory) and [#94](https://github.com/aclater/sacdfs/issues/94).
- **Containers on a host that mounts sacdfs:** bind the mount's *parent* directory with
  `rslave` (`-v /srv/sacd:/media:ro,rslave`), not the mount point. Tested with rootless
  Podman on 0.3.0-rc.1, and the clean stop again on 0.4.0-rc.2: after sacdfs was killed and
  started again (0.3.0-rc.1), a container that bound the mount point, or the parent without
  `rslave`, failed every read ("Socket not connected"); after a
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
  On 0.4.0-rc.2 (as on 0.3.0-rc.1), Jellyfin, Navidrome, LMS and MinimServer used it for all
  4 album areas with a cover. MPD uses only files named `cover.*`, so it found 1 of 4 (the one named
  `cover.jpg`) until sacdfs showed the cover under that name as well ([#148](https://github.com/aclater/sacdfs/issues/148), tested on the 0.4.1 branch: 6 of 6). Roon used the art on 0.2.0; for Plex (0.2.0-rc.4) see [its section](#plex).
- **Multichannel copies:** sacdfs adds ` (Multichannel)` to the album tag by default
  ([#97](https://github.com/aclater/sacdfs/issues/97)). With it, Jellyfin, Navidrome, LMS, MPD
  and MinimServer showed the 2 multichannel areas as their own albums on 0.4.0-rc.2 and
  0.3.0-rc.1. Plex
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
  [#123](https://github.com/aclater/sacdfs/issues/123); tested with Roon on 0.2.0). 0.4.1 moves
  the time of every folder with a disc below it once, and only the times of files of discs without text (the
  [user guide](user-guide.md#tagging-behaviour); untested with Roon, Plex and the other servers on
  0.4.1 at the time of writing, see the CHANGELOG). Server
  rescans with nothing changed read nothing: 0 opens for MPD, MinimServer and LMS on
  0.4.0-rc.2 and 0.3.0-rc.1, and 0 reads for Navidrome on 0.3.0-rc.1. From 0.4.0 a renamed
  or removed image is also freed in sacdfs ([#153](https://github.com/aclater/sacdfs/issues/153));
  Navidrome followed both on 0.4.0-rc.2 ([its section](#navidrome)).
- **Hard disks and simultaneous cold starts.** Once playing, no read took over 0.5 ms in the
  hard-disk test. Slow reads (116 to 156 ms worst) come from several players' first seeks
  queueing on one physical disk when four start at once (39 to 62 ms staggered by 2 s); it is
  bounded by the disk and no sacdfs setting changed it
  ([#71](https://github.com/aclater/sacdfs/issues/71), [performance](performance.md#client-side-where-the-slow-reads-come-from-71)).

## Reported by users

Results from users on their own hardware. We didn't run these ourselves, so they don't appear in
the summary table above. The sacdfs version is given where the report gives it.

- **Netgear ReadyNAS Pro Pioneer**, ReadyNAS OS 6.10.10, Intel Pentium Dual E2160 (2 cores,
  1.8 GHz), sacdfs built from source, files shared through the NAS's own Samba
  ([#287](https://github.com/aclater/sacdfs/issues/287)). Playback was reported as perfect for:
  - **Eversolo DMP-A6ME** (firmware 1.6.15): stereo and multichannel DSF and WAV.
  - **HEOS** on a Marantz AV10 and NR1200: stereo DSF and WAV.
  - **JRiver Media Center 36**: stereo and multichannel WAV, and stereo and multichannel DSF
    sent to the Eversolo.
  - **USB Audio Player Pro 7.1.3.1** on a Pixel 8 Pro (Android 17), into several DACs: stereo DSF
    and WAV.

  The NAS has 4 GB of memory, with about 660 MB in use by programs on the whole NAS. With 1,891
  ISO files, the user reported the library loaded in under 3 seconds. Not reported: sacdfs's own
  memory use, client scan times and the sacdfs options.

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

