# sacdfs

[![CI](https://github.com/aclater/sacdfs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/aclater/sacdfs/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/aclater/sacdfs/branch/main/graph/badge.svg)](https://codecov.io/gh/aclater/sacdfs)
[![CodeQL](https://github.com/aclater/sacdfs/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/aclater/sacdfs/actions/workflows/codeql.yml)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue)](COPYING)
[![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-orange)](Cargo.toml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/aclater/sacdfs/badge)](https://scorecard.dev/viewer/?uri=github.com/aclater/sacdfs)

A read-only FUSE filesystem that shows Super Audio CD disc images as DSF files, and as WAV
files (24-bit PCM) next to them, decoded and converted on demand by
[scarletbook](https://github.com/aclater/scarletbook-rs). Point a player, a tagger or a file
manager at the mount point and every track of every disc is there, with tags, without
extracting anything first.

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
| `--cache-mb N` | Size of the cache of decoded and converted frames, in MiB (default 256). DSF and WAV reads share it. |
| `--format LIST` | Which files to show: `dsf`, `wav`, or both, `dsf,wav` (the default). |
| `--pcm-rate HZ` | Sample rate of the WAV files: `88200`, `176400` (the default) or `352800`. Always 24-bit. |
| `--pcm-cutoff KHZ` | Low-pass cutoff of the conversion to PCM: `40` (the default) or `30` kHz, or `wide` (about 80 kHz, with `--pcm-rate 176400` or `352800` only; refused with `88200`). |
| `--pcm-gain DB` | Gain of the conversion to PCM, from -60 to +24 dB (default 0). Samples that would clip are clamped, and the log says so once per file. |
| `--flatten` | Collapse each chain of folders that holds a single image into its outermost folder (see below). Top-level folders of `SOURCE` are never collapsed. |
| `--allow-other` | Let other users read the mount. Needs `user_allow_other` in `/etc/fuse.conf`. |
| `--index-cache DIR` | Keep the persistent seek index in `DIR` (default: `$XDG_CACHE_HOME/sacdfs`, else `~/.cache/sacdfs`). It remembers where sacdfs found frames in each image, so that seeks after a remount or reboot skip most of their search: on a network share that saves round trips (see below). |
| `--no-index-cache` | Keep no persistent seek index: every mount starts with an empty one. |
| `-v` | Log more: `-v` info, `-vv` debug. `RUST_LOG` overrides it. |

Unmount with `fusermount3 -u MOUNTPOINT`. SIGINT (Ctrl-C) and SIGTERM unmount and stop sacdfs
too.

**The seek index cache** holds one small file per image (a few tens of KB for a disc played
through), named by a hash of the image's path, never the path itself, and is bounded to 32 MiB
in all (the least recently used files go first). It is written every 30 seconds if it changed
and at unmount, and never leaves the machine. An image whose size or modification time
changes misses its entry. sacdfs never trusts the cache: every saved position is checked when
it is used, so a stale or damaged cache can only make a seek slower, never wrong. Deleting the
directory is always safe.

## Layout

```
MOUNTPOINT/
  <directory of the image, relative to SOURCE>/<image file name without .iso>/
    Stereo/
      01 - <title>.dsf
      01 - <title>.wav
      02 - <title>.dsf
      02 - <title>.wav
    Multichannel/
      01 - <title>.dsf
      01 - <title>.wav
```

- `Stereo` and `Multichannel` appear only for the areas a disc has.
- File names come from the track titles. `/`, NUL and control characters become `_`; a track
  without a title is `NN.dsf`; repeated names get ` (2)`, ` (3)`. A track's DSF and WAV files
  have the same name but for the extension. `--format dsf` or `--format wav` shows one of them
  only.
- A file that is not an SACD image is left out. Directories with no `*.iso` file anywhere below
  them are left out too.
- A track the disc cannot play (an empty track, or one that runs past the end of the image) is
  left out; the reason is logged at `-v`.

With `--flatten`, a folder whose only content is one image, directly or through a chain of
folders that each hold nothing else, holds that image's `Stereo` and `Multichannel` folders
itself. Folders directly in `SOURCE` (usually artists) are always kept. For example

```
SOURCE/Artist/2001 - Album [SACD]/Artist - Album (rip)/Album/disc.iso
```

shows as `MOUNTPOINT/Artist/2001 - Album [SACD]/Stereo/01 - Title.dsf` instead of
`MOUNTPOINT/Artist/2001 - Album [SACD]/Artist - Album (rip)/Album/disc/Stereo/01 - Title.dsf`.
A folder with an image and another folder of images, or with two images, is kept as it is.
Flattening is decided from the file names alone, so it does not slow down mounting.

Each file is a standard DSF file (2.8224 MHz, 1 bit, 4096-byte blocks) with an ID3v2.4 tag:
title, performer, album artist, album, track n/N, disc n/N for multi-disc sets, composer, date,
ISRC, genre, publisher and copyright, as far as the disc has them. The audio is the track's
frames as the TOC gives them, bit-identical to what the reference extractors (pysacd's
`pysacd-extract`, scarletbook's `bench decode`) write; the pauses between tracks are not
included.

Each WAV file is the same track converted to 24-bit PCM at `--pcm-rate` (176.4 kHz by
default), band-limited to 40 kHz (or 30 kHz with `--pcm-cutoff 30`, or about 80 kHz with
`--pcm-cutoff wide`, which lets more of DSD's ultrasonic noise through), at 0 dB unless
`--pcm-gain` says otherwise:
- RIFF/WAVE with `WAVE_FORMAT_EXTENSIBLE`; a file over 4 GiB (a long multichannel track) is
  RF64 instead, which most current software reads.
- Channel order and mask: 1 FC; 2 FL FR; 3 FL FR FC; 4 FL FR BL BR; 5 FL FR FC BL BR; 6 FL FR
  FC LFE BL BR, as the DSF files' channel types. For now this comes from the channel count
  alone, not from the disc's loudspeaker configuration.
- Tags: the same ID3v2.4 tag as the DSF file in an `id3 ` chunk, and a `LIST`/`INFO` chunk
  (title, artist, album, track number, date, genre) for software that reads only that. Both
  come after the audio.
- The audio starts after 256 KiB of padding (a `JUNK` chunk) and ends on a 4 KiB boundary, so
  that reading the header or the tags converts nothing. A tagger that walks every chunk header
  converts the track's first frame or two (a few milliseconds).
- The audio is gapless: each track's first and last frames are converted together with the
  neighbouring tracks' audio, so the WAV files of an area played one after the other are one
  continuous stream.

Sizes are exact, and listing a directory, `stat` or reading the tags never decodes or
converts anything. A WAV file is converted when its audio is read; a player reading it in real
time, or `cp`, is kept ahead by read-ahead on all cores. A program that reads every audio file
of a library (fingerprinting, loudness scans) converts every WAV file too: use `--format dsf`
for those.

## Performance

A straight read of a DST stereo disc runs at 1.2x the time scarletbook's own `bench decode`
takes for the same area on an idle machine (about 500 MB/s), and at 1.4x on a heavily loaded
one; a read at a random offset returns in about 1 to 4 ms on a fresh mount from a local disk,
with four to five reads of the image (three for plain DSD discs), or one read when the seek
index cache already knows that part of the disc. With images on a network share, each read of
the image is a round trip, so these counts matter most there. Reading a WAV file straight through
takes 1.1 to 1.4 times as long as reading the same track's DSF file (the WAV file holds 1.5
times the bytes), and a seek in it returns in about 2 ms. See
[docs/performance.md](docs/performance.md).

## Limits

- Linux only for now, and read-only.
- DSF and WAV output only: no DSDIFF (DFF), no FLAC, no 44.1/48 kHz or 16-bit PCM. The converter is measured in
  [docs/pcm-quality.md](docs/pcm-quality.md).
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

[docs/design.md](docs/design.md) describes the layout, the DSF and WAV mappings, the frame
locator, caching and concurrency.

## Licence

sacdfs is free software: you can redistribute it and/or modify it under the terms of the
[GNU General Public License](COPYING) as published by the Free Software Foundation, either
version 3 of the License, or (at your option) any later version. The test corpus in `tests/corpus/` is dedicated to
the public domain under CC0 1.0 (see its [licence](tests/corpus/LICENSE)).
