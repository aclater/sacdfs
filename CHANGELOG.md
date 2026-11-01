# Changelog

All notable changes to sacdfs are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

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
