# Release test plan

CI proves that sacdfs does what its tests say. Real clients have repeatedly shown things CI could
not: Roon sending tens of thousands of extended-attribute requests
([#174](https://github.com/aclater/sacdfs/issues/174)), `ls -l` failing over NFS, Plex merging
album copies ([#138](https://github.com/aclater/sacdfs/issues/138)), a read stalling for hours on a
NAS ([#72](https://github.com/aclater/sacdfs/issues/72)). So a release is not ready until the
clients people actually use have run it.

This page says what is tested, on what, and when a result blocks a release. Results go in
[clients.md](clients.md), [performance.md](performance.md) and the release candidate's tracking
issue, as counts and characteristics, never disc or album names.

## When each level runs

| Level | Runs on | Contents | Blocks |
|---|---|---|---|
| **Pull request** | CI | Unit, corpus, mount, container share and NFS export tests; fuzz smoke; informational benchmark | The merge |
| **Change check** | The PR's build | Any client the change could affect, plus the [scenarios](#scenarios-for-this-release) it adds | The merge |
| **Release candidate** | Each `-rc` tag's published binary and image | Everything below: [clients](#clients), [behaviour](#behaviour-every-client), [protocols](#protocols-and-file-systems), [regressions](#regressions-that-must-stay-fixed), [upgrade](#upgrade-and-rollback), [performance](#performance-and-endurance) | The final tag |
| **After the tag** | The published artifacts | [Install channels](#install-channels) | Nothing; a failure means a new patch release |

A new candidate after a fix repeats what the fix could affect, and always the Tier 1 scan and
playback checks. The final tag must carry only the version change on top of the last candidate.

## Test libraries

All libraries use genuine discs. Synthetic discs are for CI's parsers only: the owner decided
that coverage gaps are filled by real discs as the library grows, because what breaks clients comes
from real pressings.

| Library | Size | Used for |
|---|---|---|
| **Small** | 5 images, 93 tracks: plain DSD, DST, hybrid, DST stereo-only, raw | Every client; results compare directly across releases |
| **Medium** | About 35 images, including discs with missing frames, untitled tracks and pauses | Roon and Plex, which behave differently at scale |
| **Whole library** | All images (over 400), over 4,800 DSF files, with the non-SACD images left in | Tree comparison, scan timing, Roon and Plex in daily use |
| **Large file** | Multichannel discs that pass 4 GiB per track at `--pcm-rate 352800` | RF64 WAV: no disc in the library passes 4 GiB at the default rate |

**What no test disc covers yet:** Japanese, Chinese or Korean disc text; plain-DSD multichannel;
3.0 and 4.0 layouts; multichannel-only discs; single-track discs; discs over 40 tracks. These
are stated as untested in release notes until a disc turns up. A candidate re-reads the coverage
report for discs that have arrived since the last release.

## Clients

**Tier 1 blocks a release** if something that worked in the previous release no longer does.
**Tier 2 is reported** and filed but does not block. **Tier 3** is tested when someone can, and
otherwise stated as untested.

| Client | Tier | Connection | How it runs | Last result before this plan |
|---|---|---|---|---|
| Roon | 1 | SMB, same NAS | Long-running NAS install | In daily use on 0.4.0; clients.md records 0.2.0 |
| Plex | 1 | SMB, same NAS; and rslave bind | Long-running NAS install, API checks. The scripts in the scripted-server harness do not cover Plex; its image was not run there | In daily use on 0.4.0; clients.md records 0.2.0-rc.4 |
| Jellyfin | 1 | rslave bind | Scripted container | 0.4.0-rc.2 |
| Navidrome | 1 | rslave bind | Scripted container | 0.4.0-rc.2 |
| Lyrion (LMS) | 1 | rslave bind | Scripted container | 0.4.0-rc.2 |
| MPD (so moOde, Volumio) | 1 | rslave bind | Scripted container | 0.4.0-rc.2 |
| MinimServer | 1 | rslave bind | Scripted container | 0.4.0-rc.2 |
| Windows 11 Explorer, PowerShell, Robocopy | 1 | SMB 3 | Unattended VM | 0.2.0-rc.1 |
| Linux NFS and SMB mounts (Fedora, RHEL, Debian, Ubuntu) | 1 | NFSv4, SMB 3 | VMs and containers | 0.4.0 on Fedora only |
| beets-pysacd | 1 | Local mount | Its CI, plus an import run | sacdfs main, each change |
| FFmpeg, sox, mediainfo, ExifTool, mpv, VLC | 2 | Mount and shares | Script: tags, duration, format, seek | Partly, through the servers above |
| foobar2000 | 2 | SMB 3 | Windows VM | Partial, 0.2.0-rc.1 |
| JRiver Media Center | 2 | SMB 3 | Windows VM, trial | **Never** |
| Kodi, Gerbera (DLNA), Strawberry | 2 | Mount and shares | Containers or VMs | **Never** |
| macOS 15 in a VM on Apple hardware (and the newest release when available): `mount_smbfs`, `cp`, `rsync`, Spotlight, Quick Look, CoreAudio tools; Finder and free players (library import, tags and art, with audio off) | 2 | SMB 3 | macOS 15 in a VM on Apple hardware, tests run over SSH | File tools on 0.4.0; Finder and players **never** |
| Sonos, using the music-library share | 3 | SMB | Needs the owner; record its format and protocol limits | **Never** |
| Phones and hardware streamers (WiiM, Bluesound, others) | 3 | SMB, NFS, UPnP | No hardware to automate | **Never** |

Client versions are recorded each run. Tier 1 images are refreshed to current at each candidate;
a result that changes after a client update is reported as such.

## Behaviour: every client

Each Tier 1 and Tier 2 client is judged on the same list. Anything not checked is reported as not
checked, never as passing.

1. **Found:** tracks found against the expected count, per format (`dsf`, `dsf,wav`).
2. **Metadata:** title, artist, album, track number, year, codec, channels, and duration within a
   small tolerance of the TOC.
3. **Albums:** count as expected, multichannel copies separate where documented, no merged or
   duplicate albums.
4. **Art:** folder art shown for the areas that have it.
5. **Playback:** starts, plays to the end, and for DST multichannel and plain DSD both. Time to
   first byte is recorded.
6. **Seek:** to 60 % of a track; time to next bytes recorded.
7. **Quiet rescan:** with nothing changed, a second scan reports no changes and no re-import.
8. **Library changes:** add, rename and remove an image, and add a sidecar. Each is seen, and
   nothing else is touched.
9. **Restart:** restart sacdfs during playback and during a scan. The client recovers by itself or
   the documented step works.
10. **Output version change:** an option change, such as `--pcm-cutoff`, is seen on the next scan.
11. **Load on sacdfs:** peak memory, CPU seconds, bytes served, and FUSE requests per kind. A request
    count that jumps is a failure until explained: that is how the Roon request storm was found.
12. **Logs:** no new errors or warnings on either side.

## Protocols and file systems

Run through each access path: FUSE mount, SMB 3, NFSv4 on its default and custom ports, and an
rslave bind into a container.

- `ls -l`, `stat`, `find`, `du`, `df` (non-zero sizes), `tree`.
- `getfattr`, `listxattr`, `getxattr`; the error each returns, and the client's reaction.
- Byte identity: a copy through each path hashes the same as through the mount, for DSF and WAV.
- Read-only: write, rename, delete and `touch` fail cleanly, with no hang.
- Reads: sequential, random, concurrent readers on one file, four players plus a tagger.
- Case and character handling in names: long names, non-ASCII, names illegal on Windows and macOS.
- macOS-specific: `._` files, `.DS_Store`, Spotlight indexing and Quick Look must not fail or cause
  a read storm.
- Windows-specific: a second Robocopy pass skips everything; file times survive.
- Stable identity: inodes and mtimes hold across a restart, and NFS handles stay valid.
- **RF64:** a real file over 4 GiB reads, seeks and hashes correctly through every path, and
  FFmpeg, sox and at least one server read it. Clients that can't are named in the docs.

## Regressions that must stay fixed

Each past client-visible bug gets its scenario re-run at every candidate.

| Issue | Check |
|---|---|
| [#62](https://github.com/aclater/sacdfs/issues/62) | `df` and SMB free space report a real size |
| [#64](https://github.com/aclater/sacdfs/issues/64), [#123](https://github.com/aclater/sacdfs/issues/123) | mtimes move when bytes change; Roon sees an upgrade and a new sidecar by itself |
| [#71](https://github.com/aclater/sacdfs/issues/71) | Worst read under four players and a tagger is within the documented target |
| [#72](https://github.com/aclater/sacdfs/issues/72) | No long stall reading 6-channel DST over SMB on the NAS; the soak's stall injection recovers |
| [#73](https://github.com/aclater/sacdfs/issues/73), [#150](https://github.com/aclater/sacdfs/issues/150) | Memory after an import replay and an hour of reads is within the documented figure |
| [#95](https://github.com/aclater/sacdfs/issues/95) | Folder art found by LMS, MPD and MinimServer |
| [#96](https://github.com/aclater/sacdfs/issues/96) | MPD's first scan cost, `dsf` against `dsf,wav` |
| [#101](https://github.com/aclater/sacdfs/issues/101) | Sidecars added after the mount appear |
| [#138](https://github.com/aclater/sacdfs/issues/138) | Plex with the documented setting keeps the album copies apart |
| [#174](https://github.com/aclater/sacdfs/issues/174) | `ls -l` over NFS works, **and** Roon's Force Rescan sends no extended-attribute storm. These two pulled against each other once, so they are always checked together. Checked in 0.4.1-rc.1 by `ls -lR`, `find` with `getfattr` and `ls -lRZ` over NFS and SMB from a Linux client, counting `GETXATTR` and `LISTXATTR` in sacdfs's FUSE request log against 0.4.0; the Roon half needs Roon |
| [#175](https://github.com/aclater/sacdfs/issues/175) | Folders with no SACD below them are hidden |
| `verify` | A file it can't check gives an ERROR row and a non-zero exit, and an interrupted run leaves nothing that reads as a finished report. Users delete extracts on its word |

## Install channels

Run on the release they describe, following the docs as written, in a clean environment.

- **Binary archive** on the oldest supported distributions (RHEL 8.10, Debian 10, Ubuntu 20.04, Fedora 29, each in a VM with its own kernel, 4.18 to 5.4) and on current Fedora, RHEL (UBI), Debian
  and Ubuntu (containers); checksum verified in the clean environment, attestation verified where `gh` is installed. RHEL 8 needs the UBI repositories for `fuse3` when the VM is not registered; UBI 9 has no `fuse3`; Debian 10's repositories moved to archive.debian.org.
- **Container,** x86-64 and arm64, anonymous pull, under Docker and rootful and rootless Podman.
  Rootless Podman runs SMB only (NFS needs a rootful container, documented). Podman has no
  health check from the published OCI image: give it `--health-*` options. Docker reads it from the
  image. arm64 runs natively under CI only: there is no arm64 NAS to test on, and the docs say so;
  on an x86-64 host with `qemu-user` binfmt, `podman run --platform linux/arm64` can start the
  arm64 image for a smoke test of SMB.
- **crates.io:** `cargo install --locked sacdfs` in a clean container. 0.4.1 is the first release
  published by the workflow rather than by hand, so also confirm the published version and its
  source.
- **NAS guides:** TrueNAS SCALE and OpenMediaVault are re-run. Synology, QNAP, Unraid and UGOS stay
  marked "not yet validated" until someone runs them on that system.
- **fstab and systemd** recipes in a VM.

## Upgrade and rollback

On the whole library, with an existing index cache and tags directory:

1. Previous release, scan in Roon and Plex to quiet.
2. Upgrade. Record what each client re-reads; compare with the upgrade notes. Unannounced re-imports
   are a failure.
3. A **tree comparison** of the whole mount against the previous release: every changed name is
   expected and explained, and the count of unchanged names is reported.
4. **Audio identity:** hashes of audio across a disc sample match the previous release, or the change
   is declared in the release notes and bumps the output version.
5. Roll back to the previous release. It starts, and clients return to a working state.

## Performance and endurance

Every report has a table with a column per release, flags regressions, and says when runs aren't
comparable. Every timed run is under the benchmark lock. Other jobs on the same host (a VM, a log follower) distort a run even so: record the load and, where the CPU has them, the thermal-throttle counters per run, alternate the builds, and repeat a round that was disturbed. Machines are separate tables.

- Reads: DST stereo, DST multichannel, plain DSD; DSF and WAV; 11 alternating rounds, medians.
- Whole library: mount, first listing, import-style scan.
- NAS: SMB and NFS throughput on the NAS itself.
- Memory: import replay and the default cache's peak.
- **Soak:** two hours with induced stalls (`scripts/soak.py`), passing its memory, thread, file and
  cache checks.
- Weekly leak checks and fuzzing are green on the release commit. (For 0.4.1-rc.1 the fuzz workflow was green on the tag commit; the `leaks` workflow had no run at all to read.)

## Scenarios for this release

### 0.4.1

- **Hidden folders (#175):** box sets mixing an SACD with DVD discs, DVD-only folders, nesting.
  No folder holding an SACD disappears (0 is the only pass). Roon and Plex drop the art-only entries
  without touching real albums.
- **`cover.jpg` (#148):** MPD shows art for every area with a cover. Other servers show one piece of
  art, not two.
- **Fallback names (#185 phase 1):** untitled tracks get `Track NN`, discs with no text take the
  folder or image name, and hand-edited tags still win. No titled disc changes: the tree comparison
  must show only expected names. Plex's blank-title albums are named after the upgrade.
- **Thread check (#179)** and **fuzz targets (#161)** ran on the candidate.

## Exit criteria

A final tag needs all of:

- every Tier 1 client passes, or has a failure that also exists in the previous release, and that
  failure is filed and in the docs;
- no regression in the [table above](#regressions-that-must-stay-fixed);
- audio identical, or the change declared;
- no unexplained performance regression over 10 %;
- the soak passes;
- [clients.md](clients.md) states the versions and results of this release, and anything not run is
  stated as untested.

If a gate cannot be run, the release report says so; it is never counted as passed.
