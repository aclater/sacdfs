# Changelog

All notable changes to sacdfs are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Read-ahead starts only after a read of audio: reading a file's header (as taggers do) no
  longer decodes its first 2 s in the background.

- Licence: GPL-3.0-or-later (was MIT OR Apache-2.0 before any release), so that code from
  GPL and LGPL projects can be adapted. SACD parsing and DST decoding still come only from
  scarletbook (MIT OR Apache-2.0).

### Added

- `--flatten`: a chain of folders below the top level that holds a single image collapses
  into its outermost folder, which then holds the image's `Stereo`/`Multichannel` folders
  (#13). Decided from the names walked at mount, so mounting is as fast as without it.
- WAV files next to the DSF files (#7, #8, plan #11): each track also appears as
  `NN - title.wav`, 24-bit PCM converted from the DSD on demand. RIFF/WAVE with
  `WAVE_FORMAT_EXTENSIBLE` and a channel mask (from the channel count, provisionally), RF64 for
  files over 4 GiB; the DSF file's ID3v2.4 tag in an `id3 ` chunk and a `LIST`/`INFO` chunk,
  after the audio, which is padded (`JUNK`) to start at 256 KiB and end on a page boundary.
  Exact sizes from the TOC; listing, `stat`, header and tag reads never convert.
  Gapless: a track's edge frames are converted with the neighbouring audio of the area.
- Options `--format dsf|wav|dsf,wav` (default both), `--pcm-rate 88200|176400|352800`
  (default 176400), `--pcm-cutoff 40|30` (default 40), `--pcm-gain DB` (default 0, -60 to
  +24; clipped samples are clamped and logged once per file).
- Random access to WAV files: frame mapping with neighbour frames, converted frames in the
  shared `--cache-mb` cache, sequential reads that decode every DSD frame once, read-ahead that
  converts on the decoder pool.
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
