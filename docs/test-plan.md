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

**The rule for later candidates.** The full client matrix runs on the first release candidate. A
later candidate re-runs what its shipped-code changes could affect, plus the artifacts, memory, share
paths and the soak, and lists what changed and what carried over; a carried-over result is quoted
with the candidate that produced it. If a candidate changes decoding, naming, the file tree or
dependencies, the affected clients run again. The final tag must carry only the version change on
top of the last candidate.

## When a soak is needed

The soak ([performance and endurance](#performance-and-endurance)) takes two hours of a test host
and fails a release if it fails, so it runs when the shipped code it exercises has changed, not
by habit.

**A soak is needed when shipped code changed since the last soak that passed.** Check it, don't
judge it: compare `src/` and `Cargo.lock` between the last soaked tag and the candidate, ignoring
the version line (`git diff <soaked-tag> <candidate> -- src Cargo.lock`). An empty diff means no
soak. These don't need one:

- documentation, CHANGELOG, CI and test-only changes;
- a version bump;
- a final on the same code as a release candidate that passed;
- a container base image change, which the container CI checks cover.

**What the diff touches decides the rest.** Self-contained code outside the mount's read path, such
as `verify` exit codes or the `tags` commands, may rely on its own tests; the release PR records
that and the reason. Anything on the read path needs a soak: the read path itself, threads, the
cache, memory, the watchdog, the mount lifecycle, and the dependencies under them (scarletbook,
rayon, the FUSE library).

**How often.** Once per candidate, not once per PR. A comparison soak (two builds against each
other) must give each build the same load. A PR that is risky for the read path may also run a
shorter, stall-heavy soak before it merges; that run does not replace the candidate's soak.

**What the soak does not cover.** It does not exercise a long hang of one image's source with
clients that keep retrying, which could use every read thread of the mount up to 0.4.1
([#241](https://github.com/aclater/sacdfs/issues/241)). `tests/stall.rs` covers that case
(`retrying_clients_of_a_stalled_image_leave_threads_to_the_others`); a soak pass says nothing
about it.

**Status of this rule.** It is new in this revision and has not been applied to a release yet:
its first use is the decision on the 0.4.1 final, where it is untested as a process.

**Goals, not results.** The tables and lists on this page that say what is tested, on what, and how
(libraries, tiers, behaviours, protocols, install channels) are goals. Only the cells headed "Result"
or "Last result", and the results sections for [rc.2](#results-041-rc2-gate-2026-11-06) and [rc.3](#results-041-rc3-gate), say what was run. A
row with no result is untested.

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

| Client | Tier | Connection | How it runs | Last result recorded in [clients.md](clients.md) | 0.4.1-rc.2 | 0.5.0-rc.1 |
|---|---|---|---|---|---|---|
| Roon | 1 | SMB, same NAS | Long-running NAS install | 0.4.1-rc.1: Roon 2.74, whole library, Force Rescan sent no extended-attribute requests | Upgraded from rc.1 on the NAS app: names and counts identical, no re-read seen for 12 minutes. FUSE request counts per kind not obtained | Not run at scale on 0.5.0 |
| Plex | 1 | SMB, same NAS; and rslave bind | Long-running NAS install, API checks. The scripted-server harness does not cover Plex | 0.4.1-rc.1: upgrade and rollback fine, 3 albums still blank | Upgraded from rc.1 on the NAS app: names and counts identical, no re-read seen for 12 minutes. Request counts not obtained | Not run at scale on 0.5.0 |
| Jellyfin | 1 | rslave bind | Scripted container | 0.4.1-rc.1, against 0.4.0 | Carried over from rc.1 (see [results](#results-041-rc2-gate-2026-11-06)) | Same tracks, albums, art and bytes as 0.4.1 ([results](#results-050-gate)) |
| Navidrome | 1 | rslave bind | Scripted container | 0.4.1-rc.1, against 0.4.0 | Carried over from rc.1 (see [results](#results-041-rc2-gate-2026-11-06)) | Same tracks, albums, art and bytes as 0.4.1 ([results](#results-050-gate)) |
| Lyrion (LMS) | 1 | rslave bind | Scripted container | 0.4.1-rc.1, against 0.4.0 | Carried over from rc.1 (see [results](#results-041-rc2-gate-2026-11-06)) | Same tracks, albums, art and bytes as 0.4.1 ([results](#results-050-gate)) |
| MPD (so moOde, Volumio) | 1 | rslave bind | Scripted container | 0.4.1-rc.1, against 0.4.0 | Carried over from rc.1 (see [results](#results-041-rc2-gate-2026-11-06)) | Same tracks, albums, art and bytes as 0.4.1 ([results](#results-050-gate)) |
| MinimServer | 1 | rslave bind | Scripted container | 0.4.1-rc.1, against 0.4.0 | Carried over from rc.1 (see [results](#results-041-rc2-gate-2026-11-06)) | Same tracks, albums, art and bytes as 0.4.1 ([results](#results-050-gate)) |
| Windows 11 Explorer, PowerShell, Robocopy | 1 | SMB 3 | Unattended VM, not a physical client | 0.4.1-rc.1 | Carried over from rc.1 (see [results](#results-041-rc2-gate-2026-11-06)) | Not run on 0.5.0; the 0.4.1-rc.1 result stands |
| Linux NFS and SMB mounts (Fedora, RHEL, Debian, Ubuntu) | 1 | NFSv4, SMB 3 | VMs and containers | 0.4.1-rc.1 on seven distributions | SMB 3.1.1 and NFS 4.2 listings correct, 10 files byte-identical over both, writes refused, SMB reconnects after a restart. Which distributions: not recorded | 22 of 22 on six distributions, 21 of 22 on Ubuntu 24.04 (known), as on 0.4.1 ([results](#results-050-gate)) |
| beets-pysacd | 1 | Local mount | Its CI, plus an import run | sacdfs main, each change | CI on main green (2026-11-05); import run on the rc.2 mount: pass, see [results](#results-041-rc2-gate-2026-11-06). Run on 0.4.0 too, same result | Not run on 0.5.0 |
| FFmpeg, sox, mediainfo, ExifTool, mpv, VLC | 2 | Mount and shares | Script: tags, duration, format, seek | 0.4.1-rc.1 | Carried over from rc.1 (see [results](#results-041-rc2-gate-2026-11-06)) | Not run on 0.5.0 |
| foobar2000 | 2 | SMB 3 | Windows VM | 0.4.1-rc.1 | Carried over from rc.1 (see [results](#results-041-rc2-gate-2026-11-06)) | Not run on 0.5.0 |
| JRiver Media Center | 2 | SMB 3 | Windows VM, trial | 0.4.1-rc.1, partial | Carried over from rc.1 (see [results](#results-041-rc2-gate-2026-11-06)) | Not run on 0.5.0 |
| Kodi, Gerbera (DLNA), Strawberry | 2 | Mount and shares | Containers or VMs | **Never** | Never run | Not run on 0.5.0 |
| macOS 15 in a VM on Apple hardware (and the newest release when available): `mount_smbfs`, `cp`, `rsync`, Spotlight, Quick Look, CoreAudio tools; Finder and free players (library import, tags and art, with audio off) | 2 | SMB 3 | macOS 15 in a VM on Apple hardware, tests run over SSH | File tools on 0.4.0; no macOS results are in clients.md; Finder and players **never** | A baseline exists on 0.4.0 (restart and listing counts, `mount_smbfs`, in [#204](https://github.com/aclater/sacdfs/issues/204) and [#205](https://github.com/aclater/sacdfs/issues/205)); not run on rc.2. Those two issues also record an rc.1 re-run of the same two checks | Not run on 0.5.0 |
| Sonos, using the music-library share | 3 | SMB | Needs the owner; record its format and protocol limits | **Never** | Never run | Not run on 0.5.0 |
| Phones and hardware streamers (WiiM, Bluesound, others) | 3 | SMB, NFS, UPnP | No hardware to automate | **Never** | Never run | Not run on 0.5.0 |

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
- Windows-specific: a second Robocopy pass skips everything; file times survive. A Windows VM that
  times copies uses a virtio-net card (NetKVM driver) or turns receive-side scaling off on an emulated
  e1000e card: with RSS on, e1000e copies run at one of two speeds per connection, 3 to 4.5 times apart
  ([#225](https://github.com/aclater/sacdfs/issues/225), [client notes](clients.md#windows-explorer-powershell-and-robocopy)).
- Stable identity: inodes and mtimes hold across a restart, and NFS handles stay valid.
- **RF64:** a real file over 4 GiB reads, seeks and hashes correctly through every path, and
  FFmpeg, sox and at least one server read it. Clients that can't are named in the docs.

## Regressions that must stay fixed

Each past client-visible bug gets its scenario re-run at every candidate.

| Issue | Check | Result on 0.4.1 |
|---|---|---|
| [#62](https://github.com/aclater/sacdfs/issues/62) | `df` and SMB free space report a real size | rc.1: `df` non-zero over NFS and SMB on seven Linux clients. Carried over from rc.1 |
| [#64](https://github.com/aclater/sacdfs/issues/64), [#123](https://github.com/aclater/sacdfs/issues/123) | mtimes move when bytes change; Roon sees an upgrade and a new sidecar by itself | rc.1: Roon's scan after the move from 0.4.0 matched a tree comparison (changed names, retagged files, 359 added `cover` images). A new sidecar added while Roon ran: untested. Carried over from rc.1 |
| [#71](https://github.com/aclater/sacdfs/issues/71) | Worst read under four players and a tagger is within the documented target | rc.1: four readers plus a tagger all finished, slowest 0.2 to 1.0 s for 60 MiB over a virtual network, cold. Not compared with the documented target. Untested on rc.2 |
| [#72](https://github.com/aclater/sacdfs/issues/72) | No long stall reading 6-channel DST over SMB on the NAS; the soak's stall injection recovers | rc.2 60-minute soak: 19 stalls and 11 remounts, no unexpected errors. The 2-hour soak is still to report. The 6-channel DST read over SMB on the NAS: untested on 0.4.1 |
| [#73](https://github.com/aclater/sacdfs/issues/73), [#150](https://github.com/aclater/sacdfs/issues/150) | Memory after an import replay and an hour of reads is within the documented figure | rc.2, real library of 4,902 DSF files: 27.2 MiB listed, 72.7 after a head-and-tail read of every file, 33.0 after 10 minutes idle (0.4.0: 29.0, 79.5, 35.2; rc.1: 33.2, 95.1, 54.0). Import replay and the soak's memory trend (+1.4 MiB/h over 60 minutes): see [performance](performance.md) |
| [#95](https://github.com/aclater/sacdfs/issues/95) | Folder art found by LMS, MPD and MinimServer | rc.1: yes on all three. Carried over from rc.1 |
| [#96](https://github.com/aclater/sacdfs/issues/96) | MPD's first scan cost, `dsf` against `dsf,wav` | rc.1: first scan with `dsf` 0.47 s (0.4.0: 0.46 s). `dsf,wav` against `dsf` was not recorded on 0.4.1 |
| [#101](https://github.com/aclater/sacdfs/issues/101) | Sidecars added after the mount appear | rc.1: checked on the file system only; no server was shown a new sidecar. Carried over from rc.1 |
| [#138](https://github.com/aclater/sacdfs/issues/138) | Plex with the documented setting keeps the album copies apart | rc.1: multichannel copies stayed separate. Carried over from rc.1 |
| [#174](https://github.com/aclater/sacdfs/issues/174) | `ls -l` over NFS works, **and** Roon's Force Rescan sends no extended-attribute storm. These two pulled against each other once, so they are always checked together | rc.1, both halves. NFS half: `ls -lR` exited 0 with no error on seven Linux clients over NFS and SMB; against 0.4.0 in the FUSE request log the counts were the same: 1 `GETXATTR` and 238 `LISTXATTR` for 206 files over NFS, none over SMB. `find` with `getfattr` on a whole tree and `ls -lRZ` were not run; `getfattr -d` on one file returned at once with no attributes. Roon half: a Force Rescan on Roon 2.74 took 3.2 s with 19,324 FUSE requests, none for extended attributes. Carried over from rc.1 |
| [#175](https://github.com/aclater/sacdfs/issues/175) | Folders with no SACD below them are hidden | rc.1: on a real library 649 entries disappeared (4 top-level folders among them) and no DSF file. Carried over from rc.1 |
| `verify` | A file it can't check gives an ERROR row and a non-zero exit, and an interrupted run leaves nothing that reads as a finished report. Users delete extracts on its word | Tested against the fix on its pull request branch and in CI; not run on a published 0.4.1 candidate |

## Real-disc tests

Real images can't go into CI, so three `#[ignore]`d tests (`dsf_header::real_discs_head_reads`,
`library_index::real_discs`, `wav::real_discs`) run by hand. Before each release candidate the
coordinator runs, on a build of the candidate commit:

```sh
scripts/real-discs.sh DIR
```

`DIR` is the Small library's images ([Test libraries](#test-libraries)) or larger, SACDs only. The
release report records the counts (images, tests passed of 3, skipped) and the exit status, never
disc titles. Exit 0 passes. Exit 77 (no images given) is a skip and is reported as one: the gate
was not run, which [the exit criteria](#exit-criteria) do not count as passed. Details:
[development.md](development.md#real-disc-tests).

The run is checked to fail when it should: on a directory holding only an empty file, only random
bytes named `.iso`, or a real image next to random bytes, each of the three tests fails (images
that don't open, compare nothing or serve no file). It does not catch a truncated image, because
the sector count comes from the file's size. For a long run use `nice`; it is not timed, so it
needs no benchlock.

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
  cache checks. Pass criteria ([development.md](development.md#the-soak)): memory within the cache cap
  plus noise, and a trend of at most 4 MiB/h once the largest single step in the second half is set
  aside ([#247](https://github.com/aclater/sacdfs/issues/247): healthy runs step by 3 to 6.5 MiB, and a
  steady climb of 10 MiB/h still fails, a 2-hour run catching about 5 MiB/h and up); threads at the
  first half's median + 4 at the median and end, with a spike above median + 16 allowed for under two
  minutes and never above median + 32; open files and cache directory level. Whether a candidate needs one is decided by [When a soak is needed](#when-a-soak-is-needed).
  It does not cover a long hang with retrying clients ([#241](https://github.com/aclater/sacdfs/issues/241)),
  which `tests/stall.rs` covers.
- Weekly leak checks and fuzzing are green on the release commit. For 0.4.1 the fuzz workflow was
  green on the rc.1 tag commit. The `leaks` workflow's first run was on this release and it failed
  because its own checks were broken (a fix is [#226](https://github.com/aclater/sacdfs/pull/226));
  so no weekly leak check has passed on a 0.4.1 candidate.

## Scenarios for this release

### 0.4.1

- **Hidden folders (#175):** box sets mixing an SACD with DVD discs, DVD-only folders, nesting.
  No folder holding an SACD disappears (0 is the only pass). Roon and Plex drop the art-only entries
  without touching real albums.
- **`cover.jpg` (#148):** MPD shows art for every area with a cover. Other servers show one piece of
  art, not two.
- **Fallback names (#185 phase 1):** untitled tracks get `Track NN`, discs with no text take the
  folder or image name, and hand-edited tags still win. No titled disc changes: the tree comparison
  must show only expected names. Plex's blank-title albums are named after the upgrade (rc.1: 279 untitled tracks named, but 3 albums of 26 tracks still blank in Plex; cause not established).
- **Thread check (#179)** and **fuzz targets (#161):** the fuzz workflow was green on the rc.1 tag commit. The 60-minute soak on rc.2 passed; whether its thread check met an induced stall is not recorded.

## Results: 0.4.1-rc.2 gate (2026-11-06)

What was run on rc.2, and what was not. Anything missing here is untested on rc.2.

**What changed since rc.1, and so what carries over.** The shipped code differs between
`v0.4.1-rc.1` and `v0.4.1-rc.2` in `src/main.rs` (the malloc arena limit is set before the start-up
walk, [#224](https://github.com/aclater/sacdfs/pull/224)), `deploy/entrypoint.sh`,
`deploy/compose.yaml`, `Containerfile` (the container's mount limit `SACDFS_MOUNT_TIMEOUT` and the
health check's 300 s start period, [#232](https://github.com/aclater/sacdfs/issues/232)), the version
numbers, and a CI test script (`deploy/ci-share-test.sh`). `git diff v0.4.1-rc.1 v0.4.1-rc.2 -- src
deploy Containerfile Cargo.lock Cargo.toml` shows nothing else: no change to decoding, naming, the
file tree or dependencies. So, per the rule above, the client results in [clients.md](clients.md)
are rc.1 results and carry over, each stating rc.1; the re-run on rc.2 covers what the two changes
could affect: memory, mount time, the container, the share paths, the upgrade on the NAS apps and
the soak. The carry-over rests on that diff, not on a re-run; a client whose behaviour depends on
memory or start-up timing was not checked again.

- **Memory** (real library, 4,902 DSF files, 32-core machine), listed / after a head-and-tail read of
  every file / 10 minutes idle, MiB: 0.4.0 29.0 / 79.5 / 35.2; rc.1 33.2 / 95.1 / 54.0; rc.2
  27.2 / 72.7 / 33.0.
- **Whole-library mount** (warm NFS, fresh index cache, 5 alternating rounds, medians): 0.4.0
  5.00 s, rc.1 0.76 s, rc.2 1.02 s; first listing 1.55 / 1.36 / 1.42 s. rc.2 is about 0.26 s slower
  than rc.1, which is accepted; the arena limit is the suspected cause, not confirmed. Cold-cache
  mount: not measured on rc.2.
- **Soak:** the 60-minute run passed (224,491 reads, 686 GB, 0 unexpected errors, 19 stalls, 11
  remounts, +1.4 MiB/h). The 2-hour soak: still running when this was written.
- **Share paths:** SMB 3.1.1 and NFS 4.2 listings correct; 10 files byte-identical over both;
  writes refused; SMB reconnects after a restart.
- **Container:** `SACDFS_MOUNT_TIMEOUT` and the health check pass on the rc.2 image under Docker and
  rootful Podman. `docker compose` was not run. The timeout's expiry was shown on a local build of
  the same code, not on the published image.
- **Upgrade, rc.1 to rc.2, on the NAS apps:** identical names and counts; no re-read seen for 12
  minutes; FUSE request counts per kind not obtained.
- **beets-pysacd 0.2.0** (Tier 1, local mount; PyPI latest, released 2026-11-02). Its CI on main
  was green on every workflow on its last push (2026-11-05). Run on Python 3.14.9 with the project's
  hash-pinned lock file: beets 2.14.1, pysacd 0.5.0. Source: 3 images (one stereo plus 5.1 DST, one
  with no disc text, one plain stereo), mounted with `--format dsf,wav`; beets with its own config,
  `import: {copy: no, write: no}`, `-A` (no MusicBrainz), `fetchart` from the filesystem only.
  **Found:** 60 of 60 files (30 DSF, 30 WAV) in 4 albums, the 5.1 copy a separate album with 6
  channels. **Metadata:** title, artist, album and track number of all 30 DSF equal the mount's tags,
  and length of all 60 within 1 s: 0 mismatches. **Art:** found for the 2 albums whose image has a
  sidecar; the third has none to find. **Quiet rescan:** a second `import` and a `beet update`
  changed nothing (60 items, the same checksum of every field). **Plugin features:** `sacd-info` read
  all 3 images; `sacd-migrate --catalogue --mount` added 4 albums (30 items, `sacdfs_path` set) and a
  second run added none. For the no-text disc, `sacdfs_path` is a best-effort name and does not equal
  the served name (`01.dsf` against `01 - Track 01.dsf`), as the plugin's README says. **Logs:** no
  errors or warnings in sacdfs's log. **Memory:** sacdfs 32 MiB peak after the run. **Against 0.4.0:** the same 60 files
  and 4 albums, nothing lost; two differences, both in the changelog: the no-text disc has empty
  titles and album on 0.4.0 and `Track NN` plus a folder-derived album on rc.2, and the folder art
  is found only on rc.2 (#148). So nothing that worked on 0.4.0 stopped working. **Not checked:**
  autotag with MusicBrainz; `format: flac`; `sacd-tags-export`; the `auto` import of ISOs;
  playback; seek; add, rename and remove during a scan; a restart during an import; output-version
  change; FUSE request counts per kind; DST read time.
- **Not verified:** the arm64 image at run time; a cold-cache mount; the clients marked "Carried over
  from rc.1" above (rc.1 results, not re-run); the install channels other than the container (binary archive on the older
  distributions, crates.io, the NAS guides) on rc.2.

## Results: 0.4.1-rc.3 gate

rc.3 adds the fix for [#238](https://github.com/aclater/sacdfs/issues/238) (a hung image's WAV reads
held decoder threads), after the rc.2 2-hour soak failed on it. The shipped code changed since rc.2 only
where WAV reads are demultiplexed, so a full soak ran and the clients that read WAV were re-run. The
final 0.4.1 tag carries only the version change on top of rc.3, so under the
[soak rule](#when-a-soak-is-needed) it needs no soak of its own.

**Soak, rc.3 with rc.2 running beside it on the same machine under the same load.** Rig A is a 12-core
AMD laptop, rig B a 16-core AMD desktop, rig C a 14-core Intel machine. "Unexpected" counts read errors
not explained by the induced stall or churn.

| Rig | Length | rc.3 verdict | rc.3 memory trend | rc.3 unexpected | rc.2 verdict | rc.2 unexpected |
|---|---|---|---|---|---|---|
| C | 2 h | Pass | within limit | 0 | Fail | 14 |
| A | 2 h | **Fail, memory** | +8.02 MiB/h (limit 4.4) | 0 | Fail | 1 |
| A | 2 h, repeat | **Fail, memory** | +8.90 MiB/h (limit 4.4) | 0 | Pass | 0 |
| A | 6 h | Pass | +0.08 MiB/h (limit 4.0) | 0 | Fail | 5 |
| B | 8 h | Pass | +1.14 MiB/h (limit 4.0) | 0 | Fail | 3 |

**How the two failures were judged.** Both came from a single step of about 5 to 6.5 MiB inside the
2-hour run's short trend window. Before the long runs, the rule was set that a climb past the plateau
would mean a leak and a new candidate, and a flat line would mean a one-time step. Both long runs stayed
flat: on rig A at about 155 MiB from hour 3 to the end. The check itself was not changed, and the gate
was counted as passed on those long runs. Steps of this size also appear on rc.2 and 0.4.0, in both
directions. **Unexplained then, explained later:** the steps are the host's memory reclaim (see
[Soak memory steps (#247)](#soak-memory-steps-247)); the level gap on rig A is the malloc heap's live
data (same section). No soak covers a long hang with retrying clients
([#241](https://github.com/aclater/sacdfs/issues/241)).

**Clients re-run on rc.3,** details in [clients.md](clients.md#041-rc3-checks): seven Linux distributions
over NFS 4.2 and SMB 3.1.1 (the one failure is the known Ubuntu 24.04 non-ASCII name problem); Navidrome,
Lyrion and MPD with `dsf,wav`, the same counts as rc.1. The fix candidate read 218 files byte-identically
against rc.2, and used up to 16 MiB more memory (median) in a 60-run comparison with no kills.

**Not run on rc.3:** Jellyfin (the test harness never created the library, so this says nothing about
sacdfs; rc.1 is its last result); Windows, macOS, foobar2000, JRiver and MinimServer (carried over from
rc.1, or rc.2 where stated above); playback in Roon and Plex (their test deployments moved to the rc.3
image and the share lists the library, but no player was driven); the arm64 image at run time; a
cold-cache mount.

## Results: 0.5.0 gate

What was run on 0.5.0-rc.1 so far, and what was not. Anything missing here is untested on 0.5.0.
The final 0.5.0 tag carries only the version change on top of rc.1 and a test-only fix (#272).

**Clients (Tier 1),** on main at 826fa50, the same code as rc.1 apart from the version and that
fix; 0.4.1 was the baseline. Details in [clients.md](clients.md#050-rc1-checks).

- Jellyfin 12.4.0, Navidrome 0.64.2, Lyrion 9.1.1, MPD 0.24.15 and MinimServer 2.2: the same tracks,
  albums, art and bytes as on 0.4.1.
- Linux NFS 4.2 and SMB 3.1.1: 22 of 22 checks on Fedora 44, RHEL 10.1, RHEL 9.7, Debian 13, Debian 12
  and Ubuntu 26.04. Ubuntu 24.04 passes 21 of 22 (the known `linux-modules-extra` non-ASCII SMB `stat`
  problem), as on 0.4.1.
- FUSE requests per scan fell 2 to 20 % against 0.4.1 for the scripted servers. Over SMB a first
  `ls -lR` takes 226 requests, against 722 on 0.4.1.

**Release binaries.** The x86_64 build targets glibc 2.17. In a UBI 7 container the mount and byte
identity match a host mount, and the corpus check passes 457 of 457. The aarch64 build targets glibc
2.17 and its tests run natively in CI.

**Real-disc tests:** `scripts/real-discs.sh` passes 3 of 3 on 40 real images (12 multichannel), run on main at the rc.1 code. A byte comparison with 0.4.0 matches on 466 files from 12 discs, and on 5 RF64 WAV tracks of 4.2 to 7.9 GiB. A whole-library tree comparison with 0.4.0 shows no audio file missing or added; the only differences are the intended renames and art changes (see [clients](clients.md)).

**Soak:** `scripts/soak.py --hours 2 --sample-paths` on three Linux machines, the 0.5.0 code (main at the rc.1 code) side by side with 0.4.1 on the same images:
- **Load.** 675 GB to 1.0 TB read per run, 38 deliberate stalls and 23 remounts per run. No stall or remount failed.
- **Memory.** Flat: a second-half trend of at most +1.9 MiB/h (limit 4.9). The peak sits 3 to 7 MiB above the warm level (0.4.1: 10 to 12). The warm level is 7 to 11 MiB above 0.4.1's.
- **Resources.** No thread spike, at most 18 fds, and the cache stayed flat.
- **The failed check.** The "unexpected read errors" check failed on both versions, with 1 to 6 errors per run. All of them are designed behaviour on the art-churn album: `ESTALE` on replaced art and `ENOENT` on `cover.jpg`. The soak script doesn't yet count those as expected (#279). This is not a regression.

**Not run on 0.5.0:** Windows 11 (the 0.4.1-rc.1 result stands), macOS, Roon and Plex at scale,
foobar2000, JRiver, and the Tier 2 tools.

## Results: 0.6.0-rc.1 gate

What was run on 0.6.0-rc.1 (tag `v0.6.0-rc.1`), with 0.5.0 as the baseline. Anything missing here is untested on 0.6.0-rc.1.

**Clients (Tier 1),** on the v0.6.0-rc.1 release binary and image, 0.5.0 as the baseline (details in [clients.md](clients.md#060-rc1-checks)).

- Jellyfin 12.4.0, Navidrome 0.64.2, Lyrion 9.1.1, MPD 0.24.15 and MinimServer 2.2: the same tracks, albums, art and bytes as on 0.5.0. No ERROR or WARN lines in sacdfs's logs.
- Linux NFS 4.2 and SMB 3.1.1: 22 of 22 checks on Fedora 44, RHEL 10.1, RHEL 9.7, Debian 13, Debian 12 and Ubuntu 26.04. Ubuntu 24.04 passes 21 of 22 (the known `linux-modules-extra` non-ASCII SMB `stat` problem), as on 0.5.0.
- FUSE requests per scripted-server session fell against 0.5.0 (Navidrome `dsf` 3,477 to 1,204, Lyrion 400 to 214, MPD 309 to 234, MinimServer 311 to 231, Jellyfin 1,900 to 1,713). Over SMB a first `ls -lR` takes 175 requests against 253, and `find` 163 against 241. Over NFS the per-kind counts for `ls -lR`, `find` and `getfattr -d -R` are identical to 0.5.0 (no extended-attribute storm: 1 GETXATTR, 238 LISTXATTR for `ls -lR`).
- macOS 27.2 in a VM on Apple hardware (SMB, command-line tools, headless): mount, browse, read-only errors, no `._` or `.DS_Store` files, Quick Look thumbnails, byte-identical copies (a 6.9 GB album over rsync) and seeks across 4 GiB in an RF64 track all pass. Names are served precomposed (NFC) and macOS lists them decomposed (NFD); a precomposed lookup of the 111 names whose two spellings differ fails while the macOS client has the folder's listing cached, without a request to the server, and works with `dir_cache_off=yes` in `nsmb.conf` (#308). No Finder, DSD player or sound check. Details in [clients.md](clients.md#060-rc1-checks).
- Windows 11 Explorer, PowerShell, Robocopy: run on 0.6.0-rc.1 (published image, VM, SMB 3.1.1). Listing 213 entries in 107 requests (0.4.1-rc.1: 321); SFM names open and read (14 of 14); WAV tags read; thumbnail pass 1,802 requests, no storm (#312); Robocopy 186 of 186 audio files byte-identical, second pass skips all 196 in 77 requests (0.4.1-rc.1: 342); RF64 reads and seeks past 4 GiB; `/TCPPORT` works. No regression. Not run: write-refusal and restart checks, large-icon view, CJK names, 0.5.0 baseline. foobar2000 and JRiver not run.

**Release artifacts.** The release workflow succeeded. The release is a pre-release with the x86_64, x86_64-v3 and aarch64 archives, each with a `.sha256` and an `.intoto.jsonl`, and `SHA256SUMS`; all three checksums verify and `gh attestation verify` passes on the x86_64 archive. Each archive now holds `sacdfs`, `sacdfs-ctl`, `sacdfs.service` and `sacdfs.conf.example` (new in this release) next to `COPYING`, `README.md` and `CHANGELOG.md`. The image `ghcr.io/aclater/sacdfs:0.6.0-rc.1` exists (an image index) and `:latest` still points at the 0.5.0 manifest. The x86_64 build targets glibc 2.17. In a UBI 7 container (glibc 2.17, no `fusermount`) the release binary mounts the corpus and its 914 file hashes equal those of a host mount (`cmp`), and the host corpus check passes 457 of 457 tracks.

**Real-disc tests:** `scripts/real-discs.sh` passes 3 of 3 (0 skipped, exit 0) on 40 real images (12 multichannel), run on the v0.6.0-rc.1 commit: 307,968 random-range positions equal across the 40 images, 51 areas whose 543 WAV tracks equal the area stream, 4413 s for the WAV test (0.5.0: 4992 s on a different selection of 40 images, so not a like-for-like timing).

**Byte comparison with 0.5.0.** A byte comparison of 466 DSF and WAV files from 12 discs (including 12 multichannel areas) between 0.5.0 and 0.6.0-rc.1 on separate index caches: all 466 hashes equal. Five RF64 WAV tracks of 4.2 to 7.9 GiB (`--pcm-rate 352800`, 5 and 6 channels, ffprobe reads 352.8 kHz 24-bit): whole-file hashes, a 32 MiB read across the 4 GiB mark and the last 1000 bytes all equal. The read times (0.5.0 4.3 to 8.1 s, 0.6.0-rc.1 4.7 to 8.6 s) are not a timing comparison: the release binary read each file first, from a cold cache.

**Tree comparison and upgrade/rollback** (the whole library, 0.5.0 then 0.6.0-rc.1 then 0.5.0 again on the same index cache and tags directory, 13,948 entries): the trees are identical in name, type and size (0 differences, as the CHANGELOG states: names, sizes, times and bytes are unchanged), so all 13,948 names are unchanged. 0.5.0 started on the cache the release binary had used, and the rolled-back tree equals the first one. Audio identity across the three steps on a 3-disc sample: 154 DSF and WAV files hash the same on 0.5.0, 0.6.0-rc.1 and 0.5.0 again. Roon and Plex re-reads were not observed (see below).

**Soak:** `scripts/soak.py --hours 2 --sample-paths` on two Linux machines, 0.6.0-rc.1 side by side with 0.5.0 on the same images and the same load (a busy host with other jobs running, and an idle one). On the busy host both builds were built from their tags; on the idle host the published x86_64 release binaries were used.
- **Load.** 739 GB to 1.13 TB read per run, 38 to 39 deliberate stalls and 23 remounts per run. No stall or remount failed.
- **Memory.** Flat: a second-half trend of at most +0.72 MiB/h (limit 4.9). The warm level is equal to 0.5.0's or 1 to 2 MiB lower (178.8 against 180.9 MiB, and 163.3 against 164.3), and the peak sits 5.7 to 7.3 MiB above the warm level (0.5.0: 5.5 to 5.6).
- **Resources.** No thread spike (peaks 83 and 72 against 79 and 73 on 0.5.0, all under the spike line), at most 19 fds, and the cache stayed flat.
- **The failed check.** The "unexpected read errors" check failed on both versions, with 2 to 6 errors per run. All of them are `ESTALE` on replaced art on the art-churn album (#279), the designed behaviour; none was on an audio file. This is not a regression.

**Not run on 0.6.0-rc.1:** Roon and Plex at scale (the re-scan on upgrade and rollback in step 2 and 5 of [Upgrade and rollback](#upgrade-and-rollback) was run on the binary only, with no Roon or Plex), foobar2000, JRiver, and the Tier 2 tools.

## Soak memory steps (#247)

Steps of 3 to 9 MiB in a soak's resident size, in both directions, appeared on 0.4.0, 0.4.1-rc.2
and rc.3 and nobody could say what they were. Four 2.5-hour soaks of the same images were run side by side
(three runs of the 0.6.0 line, main at b46c5e0, and 0.4.1-rc.2) on one busy host with
`--smaps --malloc-info-every 300`. The host load stayed above 70 and its zram swap was in use.

**Finding.** Every run fell in the same two steps at the same minutes: about -4.5 MiB at minute 125
and -9 MiB at minute 132 (medians of six samples either side: -4.6 and -9.0, -4.6 and -9.6, -4.9 and -10.4 MiB for the three
runs of main, -4.1 and -6.4 for rc.2), almost all of it in the `[heap]` mapping, then crept back up
(+1.6 to +2.7 MiB at minute 138). In the same samples the malloc heap's size (`system current`,
186.2, 188.3, 187.9 and 177.6 MB) did not change, there was one arena, and its free bytes varied by a few MB from sample to
sample as before, with no step. Nothing was freed or trimmed; four unrelated processes, two of them a different
build, can't free memory at the same second. A run of `madvise(MADV_PAGEOUT)` on a running mount's heap took
31.8 MiB from `VmRSS` into `VmSwap`, and most of it returned within a minute. The steps are the
kernel swapping a quiet process's pages out and in. The earlier eight-hour runs on their own rigs
had 20 to 248 KiB in swap at the start; the steps on those isolated rigs were not
re-measured, so for them this is the likely cause, not a shown one.

**The rig A level difference.** On one host, the same images, one hour in: main sat 8 MiB above rc.2
(resident 186 against 177 MiB; anon 179 against 170). By mapping, the whole difference is in `[heap]` (112 against
104.5 MiB); the cache's anonymous mappings and thread stacks (64 MiB) match, and the files are 1 MiB
apart (the larger binary). In `malloc_info` the heap holds 165 MB in use for main and 153 for rc.2 and
about the same free bytes. So it is live data on the heap, not fragmentation, arenas or stacks. Which
allocation it is, a profile of the soak segment would show ([massif](development.md#heap-profile-of-a-soak-segment-massif));
that wasn't run. The 0.5.0 results already put the warm level 7 to 11 MiB above 0.4.1.

**Decision.** The soak records `VmSwap` and judges resident plus swapped memory (`swap_kb` column).
*Why:* the soak measures what sacdfs holds; reclaim changes where it sits and has no cause in sacdfs.
*Alternatives:* leave the steps to the step rule of #249 only (a step that is not a leak passes, but
a leak beside a big drop could still hide, and a swapped-out leak would show as a falling trend);
run soaks only on a host without swap (not always possible, and it hides nothing about sacdfs);
fail on swapped memory (it says nothing about sacdfs). *Trade-off:* a swapped-out leak is counted
(that is the point), and the check can't tell if pages came back from swap; it also trusts
`VmSwap`, which Linux gives. The step rule from #249 stays as a second guard for what is
not swap (a step that is not explained is still set aside, never a climb). Tests:
`scripts/test_soak.py` (reclaim to swap passes, a swapped-out leak of 10 MiB/h fails).

**The thread ceiling** was handled in #249: a spike line of median + 16 that may be passed for under
`--spike-hold` (120 s), and a hard ceiling of median + 32. In the four runs above the threads peaked at
81, 82, 81 and 87 with a first-half median of 73 and the spike line at 89: a margin of 2 to 8
threads where the single ceiling of the old rule would have sat at 89; rc.2 reached 87, 2 below it.

**Not run.** Isolated rigs (no other load, no swap) with `VmSwap` recorded: whether a quiet rig
still steps by itself. The soak CI job (20 minutes) wasn't re-run on this change beyond the unit tests.

## Exit criteria

A final tag needs all of:

- every Tier 1 client passes, or has a failure that also exists in the previous release, and that
  failure is filed and in the docs;
- no regression in the [table above](#regressions-that-must-stay-fixed);
- audio identical, or the change declared;
- the [real-disc tests](#real-disc-tests) pass on the last candidate (exit 0), or are reported as not run;
- no unexplained performance regression over 10 %;
- the soak passes on the last candidate, or the [soak rule](#when-a-soak-is-needed) shows none is
  needed (the final carries only the version change on top of a candidate that passed);
- [clients.md](clients.md) states the versions and results of this release, and anything not run is
  stated as untested.

If a gate cannot be run, the release report says so; it is never counted as passed.
