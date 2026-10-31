# sacdfs

[![CI](https://github.com/aclater/sacdfs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/aclater/sacdfs/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/aclater/sacdfs/branch/main/graph/badge.svg)](https://codecov.io/gh/aclater/sacdfs)
[![CodeQL](https://github.com/aclater/sacdfs/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/aclater/sacdfs/actions/workflows/codeql.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#licence)
[![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-orange)](Cargo.toml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/aclater/sacdfs/badge)](https://scorecard.dev/viewer/?uri=github.com/aclater/sacdfs)

A read-only FUSE filesystem that shows Super Audio CD disc images as DSF files, decoded on
demand by [scarletbook](https://github.com/aclater/scarletbook-rs). Point a player, a tagger
or a file manager at the mount point and every track of every disc is there, with tags,
without extracting anything first.

```sh
sacdfs ~/music/sacd-images /mnt/sacd
ls /mnt/sacd/"Some Disc"/Stereo
fusermount3 -u /mnt/sacd
```

## Usage

```
sacdfs [OPTIONS] SOURCE MOUNTPOINT
```

`SOURCE` is one image file, or a directory that is searched recursively for `*.iso` files
(any case). `MOUNTPOINT` is an existing directory.

| Option | Meaning |
|---|---|
| `-f`, `--foreground` | Stay in the foreground and log to standard error. Without it sacdfs returns once the filesystem is mounted and keeps running in the background. |
| `--threads N` | DST decoder threads; 0 (the default) means one per CPU. |
| `--cache-mb N` | Size of the cache of decoded frames, in MiB (default 256). |
| `--allow-other` | Let other users read the mount. Needs `user_allow_other` in `/etc/fuse.conf`. |
| `-v` | Log more: `-v` info, `-vv` debug. `RUST_LOG` overrides it. |

Unmount with `fusermount3 -u MOUNTPOINT`. SIGINT (Ctrl-C) and SIGTERM unmount and stop sacdfs
too.

## Layout

```
MOUNTPOINT/
  <directory of the image, relative to SOURCE>/<image file name without .iso>/
    Stereo/
      01 - <title>.dsf
      02 - <title>.dsf
    Multichannel/
      01 - <title>.dsf
```

- `Stereo` and `Multichannel` appear only for the areas a disc has.
- File names come from the track titles. `/`, NUL and control characters become `_`; a track
  without a title is `NN.dsf`; repeated names get ` (2)`, ` (3)`.
- A file that is not an SACD image is left out. Directories with no `*.iso` file anywhere below
  them are left out too.
- A track the disc cannot play (an empty track, or one that runs past the end of the image) is
  left out; the reason is logged at `-v`.

Each file is a standard DSF file (2.8224 MHz, 1 bit, 4096-byte blocks) with an ID3v2.4 tag:
title, performer, album artist, album, track n/N, disc n/N for multi-disc sets, composer, date,
ISRC, genre, publisher and copyright, as far as the disc has them. The audio is the track's
frames as the TOC gives them, bit-identical to what the reference extractors (pysacd's
`pysacd-extract`, scarletbook's `bench decode`) write; the pauses between tracks are not
included.

## Performance

A straight read of a DST stereo disc runs at 1.2x the time scarletbook's own `bench decode`
takes for the same area on an idle machine (about 500 MB/s), and at 1.4x on a heavily loaded
one; a read at a random offset returns in about 4 ms on a fresh mount, with about five reads of
the image. See [docs/performance.md](docs/performance.md).

## Limits

- Linux only for now, and read-only.
- DSF output only: no DSDIFF (DFF), no PCM (WAV, FLAC).
- The directory tree is read once, at mount time. Images added to or removed from `SOURCE`
  later need a remount.
- Each image is read through one file handle; reads of one disc's sectors are serialised.
  Decoding runs in parallel.
- A damaged disc image shows up as missing directories or files, filled audio (missing frames as
  0x69, invalid DST frames as 0x55, as the reference extractors do, with a warning in the log)
  or an I/O error on one file. It never stops the mount.

## Building

sacdfs needs Rust 1.85 or later and nothing else: it talks to the kernel through `/dev/fuse`
and mounts with the `fusermount3` helper of FUSE 3 (package `fuse3` on most distributions),
without linking libfuse.

```sh
cargo build --release
target/release/sacdfs --help
```

Run the tests with `cargo test`. The mount tests need `/dev/fuse` and `fusermount3` and pass
silently without them; set `SACDFS_FUSE_TESTS=1` to make their absence a failure. See
[CONTRIBUTING.md](CONTRIBUTING.md).

## How it works

[docs/design.md](docs/design.md) describes the layout, the DSF mapping, the frame locator,
caching and concurrency.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. The test corpus in `tests/corpus/` is dedicated to
the public domain under CC0 1.0 (see its [licence](tests/corpus/LICENSE)).
