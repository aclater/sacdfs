# Changelog

All notable changes to sacdfs are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Licence: GPL-3.0-or-later (was MIT OR Apache-2.0 before any release), so that code from
  GPL and LGPL projects can be adapted. SACD parsing and DST decoding still come only from
  scarletbook (MIT OR Apache-2.0).

### Added

- Persistent seek index (#20): the frame positions sacdfs learns in each image are kept in
  `$XDG_CACHE_HOME/sacdfs` (`--index-cache DIR`, `--no-index-cache`), so that a seek after a
  remount needs about one read of the image instead of four to five; on a network share each
  is a round trip. One small file per image, named by a hash of its path, keyed by the
  image's size and mtime, versioned and checksummed, written atomically every 30 s if changed
  and at unmount, 32 MiB in all with LRU eviction. Every saved position is checked when used,
  so a stale or damaged cache only costs speed.
- `Disc::sector_bytes`, `disc::total_reads` (logged at `-v` when sacdfs stops) and
  `examples/seek_latency`, which measures calls, bytes and time per seek.
- `--flatten`: a chain of folders below the top level that holds a single image collapses
  into its outermost folder, which then holds the image's `Stereo`/`Multichannel` folders
  (#13). Decided from the names walked at mount, so mounting is as fast as without it.
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
