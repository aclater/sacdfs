# Changelog

All notable changes to sacdfs are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `sacdfs verify SOURCE [EXTRACTS]...` (#80): checks extracted DSF and DFF files against
  what sacdfs serves, before they are deleted. It compares the raw DSD audio (ignoring tags,
  padding, bit order and container) with the matching track of the same disc, gives each file
  a status (IDENTICAL, DIFFERENT, NO-MATCH, UNSUPPORTED, ERROR, or NOT-COMPARABLE for FLAC and
  WAV) with the reason, lists the tags a deletion would lose (MusicBrainz IDs, art,
  ReplayGain, others), and writes a TSV report and a summary of the reclaimable space.
  `--map` handles extracts kept in a tree of their own, `--resume` continues an earlier run.
  It never deletes anything. See the user guide, "Before you delete extracts".

- Folder art (the jpg, jpeg, png and webp sidecars) also shows inside each area folder
  (`Stereo/`, `Multichannel/`), as the same file with a second name and inode number, so
  Lyrion Music Server, MPD, MinimServer and Plex find it (#95). It follows `--sidecars`; no
  audio file changes.
- New and removed images and sidecar files show up without a remount (#101): a folder is read
  again, at most every 2 seconds, when its modification time in `SOURCE` changed. Unchanged
  entries keep their inode numbers. The kernel remembers the names of such a folder for 2
  seconds instead of 24 hours.

### Changed

- **Multichannel files get their own album name** (#97). The album tag of the DSF and WAV files of a disc's multichannel area ends in ` (Multichannel)`; stereo files are unchanged. `--no-multichannel-album-suffix` (`SACDFS_MC_ALBUM_SUFFIX=0` in the container) turns it off.
  - *Why:* the owner's decision, after client tests (#82) showed Navidrome, LMS, MPD and MinimServer all treating a disc's Stereo and Multichannel copies as one album. Only the audio has to match an extract's, not the tag bytes, so a tag change is allowed. Alternatives rejected: changing nothing; a separate album-artist or comment tag (servers group by album name); hiding an area (loses content).
  - *What changes:* the tag bytes of multichannel files, so the output versions move: DSF to 3 and WAV to 4 (#89's rule). Their mtimes move forward one version step and NFS generations change. Audio is unchanged (the pinned PCM hashes keep their values; only their recorded WAV version moved), `sacdfs verify` still compares audio only, and the DSF gate compares audio, so it holds.
- `sacdfs verify` recognises pause-kept extracts (#93): a served track with pauses before and/or
  after it, or a track cut short, is `IDENTICAL-PAUSE` when everything extra is silence (the DSD
  idle pattern, or residue at or below -60.2 dB), and otherwise `NO-MATCH` with the length and
  level of the extra audio. Serving pauses is not part of this change.
- `sacdfs verify` looks for images up to two levels below the extract's folder when none is
  beside or above it, trying every candidate (#98).
- `sacdfs verify` no longer lists `TRCK` as lost when the extract's `n/N` matches the served
  track number `n` and `N` is the area's track count (#99).

- **WAV conversion is scarletbook-rs's `scarletbook-pcm` (scarletbook 0.5.3)** (#84). sacdfs's own converter (ported from pysacd's) is deleted, together with its `SACDFS_SIMD` override and the `multiversion` and `rustversion` dependencies.
  - *Why:* the owner's decision was to switch once scarletbook's clean converter was within the 15 % offload leeway and met sacdfs's targets, and 0.5.3 does both. One thread, 176.4 kHz, 40 kHz filter: 298x real time against 315x (generic build), 273x against 316x (x86-64-v3 build). The 30 kHz and wide filters are 13 to 39 % faster. Stopband at least 109 dB from 48, 36 and 88.2 kHz. Ripple ±0.004 dB. DSD silence is exact zero, area edges included.
  - *What changes:* the WAV files' samples. They differ from 0.1's by more than 72 dB below the signal in the audio band, an inaudible ripple difference. So the WAV output version moves to 3 (#89's rule): WAV files' mtimes move forward one more version step (25,203 s) and their NFS generations change, so copies and library scanners pick up the new bytes. This change leaves the DSF files as they are (gate 93/93).
  - Details and measurements: `docs/pcm-quality.md` and `docs/performance.md`.

- Memory stays near `--cache-mb` (#73): one malloc arena, and the frame cache reuses the
  buffers of evicted frames within its cap. A library import's reads over 60 images peak at
  271 MiB with the default 256 MiB cache (was 473 MiB) and 58 MiB at 16 MiB (was 125 MiB);
  straight reads peak at 327 MiB (was about 1 GiB). DST straight reads take about 4 % longer.

- Seeking is scarletbook's (#28): a read that does not continue an open file's cursor starts
  a scarletbook provisional (cold) reading or, after a remount, one from the saved seek hints;
  sacdfs's own frame locator is gone. Cold seeks make fewer calls to the image (3.06 a seek on
  average on the real discs, was 4.44; DST areas 3.0 to 3.4, were 4.8 to 5.5), warm ones one
  call with fewer bytes (84 KiB, was 110). Seek times through the mount are within 15 % of
  0.1's, and output is unchanged (DSF gate 93/93). Areas whose layout scarletbook finds
  suspect are read exactly. Details and figures: `docs/design.md`, "Seeking", and
  `docs/performance.md`.
- **Output change:** DSF files are written by scarletbook (`output::Layout`, its tag from
  `TagSet::from_toc`), and WAV files carry that same ID3 tag in their `id3 ` chunk, their
  `INFO` chunk built from the same items; sacdfs's own DSF header, block layout and ID3 writer
  are gone. The audio is unchanged (DSF gate 93/93, WAV real-disc test). Of the 93 real-disc
  tracks, 53 DSF and 53 WAV files are byte-identical; the other 40 of each differ only in
  `TCOP`, now the year, a space and the copyright, as ID3v2.4 asks (5 bytes longer). Both
  output versions moved to 2, so every DSF and WAV file shows a new mtime and NFS generation.
- The persistent seek index holds scarletbook's seek hints (format version 2): a cache written
  by sacdfs 0.1 is ignored and rewritten, and 0.1 ignores 0.2's.

### Documentation

- CI: a "no private names" check of commit identities and tracked files (`scripts/check-private-names.sh`, adapted from pysacd's). The Roon example in `docs/deployment.md` uses an arbitrary container subnet.

## [0.1.1] - 2026-11-01

Folder art, scans and playlists for players such as Roon, and far fewer requests per file. These come from a real-world Roon test on a NAS. scarletbook 0.5.2. The audio files are byte-identical to 0.1.0's.

### Added

- Sidecar files are passed through (#79, #75): folder art (`.jpg`, `.jpeg`, `.png`,
  `.webp`), `Artwork/` and `Scans/` folders with all their content, and `.m3u`/`.m3u8`
  playlists next to the images show at their relative path, read-only and byte for byte,
  with their source file's size and times. Players such as Roon find the folder art, and
  playlists can live on the share, so extracted copies can go without losing either. With
  `--flatten`, a collapsed chain's sidecars show in the collapsed folder. They have stable
  inode numbers (the same with and without `--flatten`) and NFS handles that survive a
  restart and go stale when the file changes. `--sidecars LIST` (`SACDFS_SIDECARS` in the
  container) widens, narrows or turns off the list (`default,cue`, `default,-webp`, `none`).
  Hidden files are never passed, and a link is followed only to a file inside SOURCE and
  outside the mount point. The audio files are unchanged.

### Changed

- scarletbook 0.5.2 (was 0.5.0), the engine for 0.2.0: sacdfs's output is unchanged (DSF
  files bit-identical to the reference extractor on all 93 tracks of the real discs, the same
  WAV bytes). One test changed: on the hostile corpus image `area-audio-past-end`,
  scarletbook's provisional (cold) readings now all equal its exact ones, where 0.5.0's
  differed, and no corpus image gives a differing one on 0.5.2. A new test makes one (a copy
  of a corpus image with late audio sectors written again near the area's start) and checks
  that every provisional reading that differs is refuted.

- Cheaper probes (#61): `getxattr` and `listxattr` answer `ENOSYS`, so the kernel stops
  asking (a second `ls -l` of a 48-file area sent 49 `LISTXATTR` requests, now none), and a
  missing name gets a negative entry cached for 24 hours (repeated `stat`s of `cover.jpg`,
  `desktop.ini` and the like reached sacdfs every time, now once). Extended attribute calls
  now fail with "Operation not supported" instead of "No data available".
- A track file's modification time moves when its bytes do (#64): WAV files with a
  non-default `--pcm-cutoff` or `--pcm-gain` show their image's time plus a fixed offset per
  option set (up to 7 hours), and a future output-version bump moves the times of that
  format forward again, so that rsync and library scanners see the change. With the default
  options, the times stay the images' own; NFS generations are unchanged.

### Fixed

- `df` and SMB clients saw a 0-byte filesystem (#62): `statfs` now reports the size of the
  filesystem holding SOURCE, with nothing free.

### Documentation

- `docs/deployment.md`, Roon: discs also present as extracted files are merged by Roon,
  which may play either copy. Folder art and playlists now work (#79, #75).
- Server-side figures from the owner's NAS (#21 part 2), in `docs/performance.md`, "On a NAS,
  server-side". The published 0.1.0 image ran as a TrueNAS SCALE 25.10 app, serving over NFS
  and SMB on the LAN:
  - **Cold seeks:** 15 to 22 ms from the NAS's cache, and 31 to 61 ms (worst 92 ms) from its
    hard disks, against a target of 25 ms.
  - **Straight reads:** stereo at the client's 2.5 Gbit/s link, and 6-channel DST at 65 to
    83 MB/s.
  - **4 players and a tagger over NFS:** worst reads 52 to 133 ms, all of them first touches
    of the disks.
  - **A stall:** one SMB read waited about 6 hours, not reproduced since (#72).
- `docs/deployment.md`, TrueNAS steps:
  - **Tested:** they were tested on a real system and worked as written.
  - **Ports:** when the NAS's own SMB and NFS services hold 445 and 2049, publish other ports
    (the simplest) or give the app an alias address. Client mounts on other ports are added.
  - **Logs:** NFS-Ganesha's harmless D-Bus and UDP lines are explained.
  - **libsmbclient:** a note on how clients built on libsmbclient show mapped names.
  - **Roon on the same NAS:** tested. sacdfs gets a fixed address on its own Docker network,
    so Roon reaches its SMB share on port 445 without clashing with the NAS's own SMB.
    Roon's container needs `SYS_ADMIN`, `DAC_READ_SEARCH` and AppArmor unconfined to mount
    the share. DSF only, so tracks don't appear twice.

## [0.1.0] - 2026-11-01

The first release.

### Added

- `sacdfs SOURCE MOUNTPOINT`: a read-only FUSE filesystem showing SACD images (one file, or a
  directory searched recursively for `*.iso`) as DSF files with ID3v2.4 tags, and WAV files
  next to them, decoded on demand by scarletbook 0.5.0. Options `-f`, `--threads`,
  `--cache-mb`, `--allow-other`, `-v`; unmounts on SIGINT, SIGTERM and SIGHUP, and detaches a busy
  mount (open files, an NFS export), stopping when the last open file is closed.
- WAV files (#7, #8, plan #11): each track also appears as `NN - title.wav`, 24-bit PCM
  converted on demand. RIFF/WAVE with `WAVE_FORMAT_EXTENSIBLE`, RF64 for files over 4 GiB; the
  DSF file's tag in an `id3 ` chunk and a `LIST`/`INFO` chunk, after the audio, which is padded
  to start at 256 KiB and end on a page boundary, so that listing, `stat`, and reading the
  header or the tags never convert. Gapless: a track's edge frames are converted with the
  neighbouring audio of the area. Options `--format dsf|wav|dsf,wav` (default both),
  `--pcm-rate 88200|176400|352800` (default 176400), `--pcm-cutoff 40|30|wide` (default 40;
  `wide`, about 80 kHz, only at 176.4 and 352.8 kHz), `--pcm-gain DB` (default 0, -60 to +24;
  clipped samples are clamped and logged once per file).
- The DSD-to-PCM converter, `sacdfs::pcm` (#5, #16): stateless per frame (frame f from frames
  f-1, f, f+1, equal bit for bit to a whole-area conversion), aligned to the DSD sample, the
  same bytes on every CPU. Pass band flat to ±0.00003 dB, stopband at least 114.9 dB. Ported
  from pysacd's converter; its first stage uses the byte-lookup-table technique of Sebastian
  Gesemann's dsd2pcm (BSD-3-Clause). Its code is chosen at run time (#23): AVX2 where the CPU
  has it, SSE2 otherwise (`SACDFS_SIMD=baseline|v3|v4` forces one), so a generic build
  converts 176.4 kHz stereo at about 300 times real time per core. Plain DSD frames are
  converted four at a time (`Converter::convert_run`, #24).
- Speaker layouts (#31): the WAV channel mask and the DSF channel type come from the area's
  loudspeaker layout on the disc: stereo, 5-channel 3/2 ("5.0") and 5.1. An area without a
  defined layout gets the layout of its channel count, logged once per disc.
- Frame locator: a learned index and an interpolating search aimed with the TOC's track
  starts, sized reads, per-open-file sequential continuation, read-ahead, a bounded cache of
  decoded and converted frames (`--cache-mb`) and parallel DST decoding. A cold seek costs 3
  to 5.6 reads of the image (#4, #20).
- Persistent seek index (#20): the frame positions sacdfs learns are kept in
  `$XDG_CACHE_HOME/sacdfs` (`--index-cache DIR`, `--no-index-cache`), so that a seek after a
  remount needs about one read of the image instead of four to five. Named by a hash of the
  image's path, keyed by its size and mtime, checksummed, 32 MiB in all; every saved position
  is checked when used, so a stale or damaged cache only costs speed.
- Open image files are kept in a bounded cache (#26): `--max-open-images N` (default 64), least
  recently used closed first, closed after 5 minutes unused, opened again if the file changed.
- Concurrent reads of one image (#28): positional reads, one image clone per read, so players,
  taggers and read-ahead on one disc don't wait for each other's round trips on a network
  share.
- `--flatten` (#13): a chain of folders below the top level that holds a single image
  collapses into its outermost folder. Decided from the names walked at mount, so mounting is
  as fast as without it.
- Persistent file identity and NFS export (#18): inode numbers come from what an entry is, the
  same on every mount and with `--flatten` on or off; generation numbers change with the
  image and the PCM options, so stale handles fail with `ESTALE`. The kernel NFS server can
  export the mount, and its clients read on through a restart of sacdfs.
- A container image for a NAS (#19, `Containerfile`): sacdfs with Samba and NFS-Ganesha,
  sharing the mount read-only over SMB and NFSv4, configured with environment variables, with
  a health check, logs on standard output, and NFS and SMB clients reading on across a
  restart. Built for generic x86-64. Compose file in `deploy/`, guide in
  `docs/deployment.md` (Docker, Podman, Compose, TrueNAS SCALE, client mounts).
- Release workflow (#44): on a version tag, Linux x86_64 binaries (generic and x86-64-v3,
  glibc 2.28 or later, stripped, with COPYING, README and CHANGELOG), SHA-256 checksums,
  build provenance attestations, release notes from this file, and the container image on
  `ghcr.io/aclater/sacdfs` (version and `latest`) after the share test passed on it. Run by
  hand, it is a dry run that publishes nothing. Not on crates.io while scarletbook is a git
  dependency.
- Diagnostics and measurement: `Disc::sector_reads`, `Disc::sector_bytes`, the total reads of
  the images logged at `-v` when sacdfs stops, `examples/seek_latency`,
  `examples/library_seek`, `examples/pcm_track.rs`, and a criterion benchmark of the converter.
- Tests against the CC0 conformance corpus, with and without mounting; proptest of the offset
  mapping, the WAV mapping and the seek index format; scarletbook's exact seek index as a
  reference for every frame the locator finds; concurrent reads; NFS export and restarts in
  CI; the container's SMB and NFS shares in CI.
- CI: fmt, clippy, tests with FUSE, MSRV, cargo-deny, CodeQL, zizmor, dependency review,
  OpenSSF Scorecard, coverage, a check that the README's options table matches
  `sacdfs --help`, and a link check of the Markdown files.

### Documentation

- The oldest distributions the README names were tested (#58): RHEL 8.10, Debian 10,
  Ubuntu 20.04 and Fedora 29, each on its own kernel (4.18 to 5.4) with its own
  `fusermount3` (3.3.0 to 3.9.0), using the generic release build. Mounting with `-f` and
  in the background, with and without `--allow-other`, reads identical to a current
  system's, SIGTERM with a file open and `fusermount3 -u` all pass; nothing needed fixing
  and the README's claim stands. Results in `docs/design.md`, "Oldest distributions".
- README as a user guide (install, quick start, options, layout, formats, deployment,
  limits, credits); `docs/user-guide.md` (options in detail, formats, tagging behaviour,
  sharing over NFS, troubleshooting); `docs/design.md` with the current design and the reason
  for each decision; `docs/performance.md` by deployment against the targets of #6, with a
  history of each improvement (#37).

### Fixed

- A sacdfs restarted on the same mount point while the previous one was still exiting lost
  its mount (#51). fuser 0.18.0 unmounts by path when a session ends, and its check that the
  mount still exists always passes, so a sacdfs unmounted with `fusermount3 -u` unmounted the
  path again on its way out, removing the new mount (every time the old one exited after the
  new one had mounted). sacdfs now serves the session without fuser's unmount, and unmounts
  or detaches on a signal only while the mount at its path is its own (by device number).
  A sacdfs started while the old mount is still there waits for it to go (up to 10 s) instead
  of mounting over it, where the old one's unmount removed the new mount (as root) or the
  restart failed (as a user). Regression tests in `tests/mount.rs`.
- A SOURCE path holding a comma could not be mounted (#55): fuser 0.18 passes the
  filesystem name to `fusermount3` unescaped, so `Bach, J.S.` failed with "unknown option",
  and a name such as `x,allow_other` would have added the option. The mount is now always
  named `sacdfs` (in `df`, `mount`, `/proc/mounts`); `-v` logs the source.
- Mounting at or above SOURCE (`sacdfs X X`, `sacdfs X/sub X`) hung the mount on its first
  access, beyond SIGKILL (#56): sacdfs read its images through its own mount. It now refuses
  to start when SOURCE is or lies inside MOUNTPOINT, when MOUNTPOINT lies inside SOURCE and
  holds images, and when `--index-cache` lies inside MOUNTPOINT (the default cache directory
  there only turns the cache off, with a warning). A non-empty MOUNTPOINT is allowed, as in
  FUSE 3, with a warning.
- After sacdfs was killed or crashed, a restart on the same mount point waited 10 s and then
  failed with "already a FUSE mount (another sacdfs?)" (#57). A dead sacdfs mount of the same
  user (ENOTCONN, found with a bounded `statfs`) is now detached at once and the restart
  mounts in milliseconds; another dead mount, or another user's FUSE mount, fails at once
  with an accurate message.
- A SIGINT, SIGTERM or SIGHUP right after start could kill sacdfs with its mount in place,
  leaving a dead mount (#57; 7 of 100 starts with a SIGTERM 0 to 30 ms after the start): the
  signal handlers were set up after mounting. They are now set up before. SIGHUP's behaviour
  (unmount and stop, like SIGTERM) is documented.

[Unreleased]: https://github.com/aclater/sacdfs/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/aclater/sacdfs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/aclater/sacdfs/releases/tag/v0.1.0
