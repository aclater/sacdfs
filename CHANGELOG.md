# Changelog

All notable changes to sacdfs are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A container image for a NAS (`Containerfile`): sacdfs with Samba and NFS-Ganesha, sharing
  the mount read-only over SMB and NFSv4, configured with environment variables, with a
  health check and logs on standard output. Compose file in `deploy/`, guide in
  `docs/deployment.md` (Docker, Podman, Compose, TrueNAS SCALE, client mounts). NFS and SMB
  clients keep reading open files across a container restart.

### Documentation

- Seek latency over SMB (#27), client-side and server-side, in `docs/performance.md`.

### Documentation

- Credit Sebastian Gesemann's dsd2pcm (BSD-3-Clause) for the byte-lookup-table technique in the PCM converter's first stage.

### Changed

- SIGINT and SIGTERM detach a busy mount (open files, an NFS export) instead of logging an
  error and serving on: sacdfs stops when the last open file is closed. fuser already
  unmounted lazily for users other than root; now root behaves the same.
- Speaker layouts (#31): the WAV channel mask and the DSF channel type come from the area's
  loudspeaker layout on the disc (scarletbook's `Area::loudspeaker_layout` and
  `Area::channel_speakers`) instead of the channel count alone: stereo FL FR, 5-channel (3/2)
  FL FR FC BL BR ("5.0") and 5.1 FL FR FC LFE BL BR, in decoded order, so no reordering. An
  area without a defined layout keeps the layout of its channel count, logged once per disc.
  Output on discs with a defined layout is unchanged; the provisional count-based mapping and
  its `TODO(scarletbook#75)` are gone.
- scarletbook 0.4.1 (was 0.3.0). No change in decoded output; the seek indexes and cold
  seeks it adds are not used yet (#28).
- A read that ends within 256 KiB of where a file's first read of audio started no longer
  starts read-ahead (#32): that is the kernel's own read-ahead of the first read, which a
  tagger reading a DSF file's header causes too. A DSF tag walk reads 222 KB of the image per
  file instead of 1.18 MB. A DSF header read still decodes the track's first frames, since
  DSF audio starts at byte 92; why that stays is in docs/design.md.

- Read-ahead starts only after reads of audio, and not on a file's first one: reading a
  file's header, or walking its chunks (as taggers do), no longer decodes its first seconds in
  the background.

- Licence: GPL-3.0-or-later (was MIT OR Apache-2.0 before any release), so that code from
  GPL and LGPL projects can be adapted. SACD parsing and DST decoding still come only from
  scarletbook (MIT OR Apache-2.0).

### Added

- Persistent file identity and NFS export (#18): inode numbers come from what an entry is
  (a directory's or an image's relative path; an area's image and slot; a file's image,
  slot, track number, format and PCM options), so they are the same on every mount, with
  `--flatten` on or off. Track files carry a generation number that changes with their
  image's size or mtime and with the PCM options, so stale handles fail with `ESTALE`. FUSE
  export support is on, `.` and `..` are looked up, and an inode number from a file handle
  kept across a restart is found again from the names walked at mount: the kernel NFS server
  can export the mount, and its clients read on through a restart of sacdfs.
- Open image files are kept in a bounded cache (#26): an image stays open from the listing
  of its directory and between reads, so that a seek no longer opens it again (0.6 to 0.9 ms
  on an NFS share). `--max-open-images N` (default 64) bounds them, least recently used
  closed first; an image unused for 5 minutes is closed; an image whose size, modification
  time or file changed is opened again.
- Persistent seek index (#20): the frame positions sacdfs learns in each image are kept in
  `$XDG_CACHE_HOME/sacdfs` (`--index-cache DIR`, `--no-index-cache`), so that a seek after a
  remount needs about one read of the image instead of four to five; on a network share each
  is a round trip. One small file per image, named by a hash of its path, keyed by the
  image's size and mtime, versioned and checksummed, written atomically every 30 s if changed
  and at unmount, 32 MiB in all with LRU eviction. Every saved position is checked when used,
  so a stale or damaged cache only costs speed.
- `Disc::sector_bytes`, `disc::total_reads` (logged at `-v` when sacdfs stops) and
  `examples/seek_latency`, which measures calls, bytes and time per seek.
- PCM output, phase 1 (#11): the DSD-to-PCM converter, `sacdfs::pcm`, behind the WAV
  files below. 24-bit PCM at 176.4 kHz (default), 88.2 or 352.8 kHz; pass band to 40 kHz (default,
  at least 105 dB down from 48 kHz), 30 kHz, or 80 kHz (176.4/352.8 kHz only); a gain with
  clamping and clip counting. Stateless per frame (frame f from frames f-1, f, f+1, equal bit
  for bit to a whole-area conversion), aligned to the DSD sample, the same bytes on every
  CPU. Ported from pysacd's converter. Tests with a 5th-order sigma-delta modulator (ripple,
  stopband, aliasing, level, DC, THD+N, alignment, steps, clipping, determinism), a criterion
  benchmark (smoke-tested in CI) and `examples/pcm_track.rs` for comparisons. See
  docs/pcm-quality.md and the PCM sections of docs/design.md and docs/performance.md.
- `--flatten`: a chain of folders below the top level that holds a single image collapses
  into its outermost folder, which then holds the image's `Stereo`/`Multichannel` folders
  (#13). Decided from the names walked at mount, so mounting is as fast as without it.
- WAV files next to the DSF files (#7, #8, plan #11): each track also appears as
  `NN - title.wav`, 24-bit PCM converted from the DSD on demand. RIFF/WAVE with
  `WAVE_FORMAT_EXTENSIBLE` and a channel mask (from the area's loudspeaker layout, #31), RF64 for
  files over 4 GiB; the DSF file's ID3v2.4 tag in an `id3 ` chunk and a `LIST`/`INFO` chunk,
  after the audio, which is padded (`JUNK`) to start at 256 KiB and end on a page boundary.
  Exact sizes from the TOC; listing, `stat`, and reading the header or the tags never
  convert.
  Gapless: a track's edge frames are converted with the neighbouring audio of the area.
- Options `--format dsf|wav|dsf,wav` (default both), `--pcm-rate 88200|176400|352800`
  (default 176400), `--pcm-cutoff 40|30|wide` (default 40; `wide`, about 80 kHz, only at
  176.4 and 352.8 kHz), `--pcm-gain DB` (default 0, -60 to
  +24; clipped samples are clamped and logged once per file).
- Random access to WAV files: frame mapping with neighbour frames, converted frames in the
  shared `--cache-mb` cache, sequential reads that decode every DSD frame once, a pipeline that
  converts frames on the decoder pool while the next ones are demultiplexed, and read-ahead
  (2 s, or about 8 MiB for readers far faster than real time). A straight read takes 1.1 to
  1.4 times the DSF read; a cold seek about 2 ms.
- Tests for the WAV files: structure, sizes, tags, the area-stream (gapless) reference at every
  rate, random and per-frame reads, no decoding while listing, mounts with `--format` and
  `--pcm-*`.
- `sacdfs SOURCE MOUNTPOINT`: a read-only FUSE filesystem showing SACD images (one file, or a
  directory searched recursively for `*.iso`) as DSF files with ID3v2.4 tags, decoded on
  demand by scarletbook 0.3.0. Options `-f`, `--threads`, `--cache-mb`, `--allow-other`,
  `-v`; unmounts on SIGINT and SIGTERM.
- Frame locator with a learned index and an interpolating binary search, per-open-file
  sequential continuation, read-ahead, a bounded decoded-frame cache and parallel DST
  decoding.
- Tests against the CC0 conformance corpus, with and without mounting; proptest of the
  offset mapping.
- CI: fmt, clippy, tests with FUSE, MSRV, cargo-deny, CodeQL, zizmor, dependency review,
  OpenSSF Scorecard, coverage.
- `Disc::sector_reads`: calls to the image for audio sectors, for diagnostics.
- A test that reads every frame of every corpus file on its own from a fresh reader.

### Changed

- Smaller cold seeks (#20): probes read about two of the area's frames (was 16 sectors), the
  first read after a seek is sized for a 128 KiB request (was 64 sectors) and reuses the last
  probe's sectors, the search stops at about two frames on discs with large frames, and keeps
  interpolating after a probe that lands next to its frame. A cold seek on real discs needs
  4.5 calls to the image and 139 KiB on average (was 5.3 and 293 KiB); plain DSD discs 3.0
  calls.
- A straight read learns a seek point every 40 frames (was 75), so any frame of a stretch
  already read can be started at directly.
- scarletbook 0.3.0 (was 0.2.0), and its conformance corpus of 136 cases (was 129). The
  new frame-range API was evaluated and not adopted: it scans from the area start, so a
  seek at 72 % of a disc takes about 100 ms instead of 1 to 2 ms (see docs/design.md).
- The frame search aims its probes with the area TOC's track start LSNs (as hints only) and
  slightly past each interpolated estimate: a cold seek on real discs needs 4.6 to 6.1
  calls to the image on average instead of 7.0 to 10.7.

### Fixed

- A seek next to a frame that repeats an earlier frame's timecode (a hostile disc) could
  return that copy instead of the original, unlike a straight read. The search now starts
  before the target frame.
