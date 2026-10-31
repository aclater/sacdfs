# Changelog

All notable changes to sacdfs are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

The first release.

### Added

- `sacdfs SOURCE MOUNTPOINT`: a read-only FUSE filesystem showing SACD images (one file, or a
  directory searched recursively for `*.iso`) as DSF files with ID3v2.4 tags, and WAV files
  next to them, decoded on demand by scarletbook 0.5.0. Options `-f`, `--threads`,
  `--cache-mb`, `--allow-other`, `-v`; unmounts on SIGINT and SIGTERM, and detaches a busy
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

- README as a user guide (install, quick start, options, layout, formats, deployment,
  limits, credits); `docs/user-guide.md` (options in detail, formats, tagging behaviour,
  sharing over NFS, troubleshooting); `docs/design.md` with the current design and the reason
  for each decision; `docs/performance.md` by deployment against the targets of #6, with a
  history of each improvement (#37).
