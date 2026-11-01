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
extracting anything first. Seeks return in a few milliseconds, locally or over the network.

## Install

sacdfs runs on Linux (x86_64) and needs FUSE 3's `fusermount3` helper (package `fuse3` on
most distributions). Each [release](https://github.com/aclater/sacdfs/releases) has two
builds, and the container image for a NAS is on GitHub's registry. Nothing is on crates.io
yet: sacdfs's engine, scarletbook, isn't on crates.io, and crates.io refuses a crate that
depends on a git repository. `cargo install` from crates.io comes once scarletbook is there.

### Binaries

- `sacdfs-<version>-x86_64-linux.tar.gz`: generic x86-64, for any x86_64 CPU. Its WAV
  converter already uses AVX2 where the CPU has it.
- `sacdfs-<version>-x86_64-v3-linux.tar.gz`: for x86-64-v3 CPUs (AVX2, BMI2, FMA: most Intel
  and AMD CPUs since about 2015, the Intel N100 family included), with all of sacdfs built for
  them: DST areas read 10 to 18 % faster ([performance](docs/performance.md#straight-reads)).
  On an older CPU this build stops with an illegal-instruction error. `grep -qw avx2 /proc/cpuinfo && echo v3` tells.

Both need glibc 2.28 or later (RHEL 8, Debian 10, Ubuntu 20.04, Fedora 29 and later). The
generic build was tested on the oldest of each, with their own kernels (4.18 to 5.4) and
`fusermount3` (3.3 to 3.9): mounting in the foreground and in the background, with and
without `--allow-other`, reads identical to a current system, and a clean stop on SIGTERM
with a file open ([testing](docs/design.md#oldest-distributions-58)). Each
archive holds `sacdfs`, `COPYING`, `README.md` and `CHANGELOG.md`. To download, check and
install one:

```sh
v=0.1.0 a=x86_64          # or a=x86_64-v3
gh release download "v$v" -R aclater/sacdfs -p "sacdfs-$v-$a-linux.tar.gz*"
sha256sum -c "sacdfs-$v-$a-linux.tar.gz.sha256"
gh attestation verify "sacdfs-$v-$a-linux.tar.gz" -R aclater/sacdfs
tar -xzf "sacdfs-$v-$a-linux.tar.gz"
install -m 0755 "sacdfs-$v-$a-linux/sacdfs" ~/.local/bin/
```

Without `gh`, download the files from the release page (or with `curl -LO`). The checksum
shows the download is intact; the attestation (GitHub's build provenance, signed with
Sigstore) shows that this repository's release workflow built it, from which commit.
`gh attestation verify` also accepts the `.intoto.jsonl` bundle next to each archive, with
`--bundle`, to check offline.

### Container image

```sh
docker pull ghcr.io/aclater/sacdfs:0.1.1        # or :latest
gh attestation verify oci://ghcr.io/aclater/sacdfs:0.1.1 -R aclater/sacdfs
```

It runs sacdfs with Samba and NFS-Ganesha for a NAS, for x86_64 and arm64; how to run it
is in [docs/deployment.md](docs/deployment.md).

### From source

sacdfs needs Rust 1.85 or later and nothing else: it talks to the kernel through `/dev/fuse`
and mounts with `fusermount3`, without linking libfuse.

```sh
cargo install --locked --git https://github.com/aclater/sacdfs --tag v0.1.1
# or, in a clone:
cargo build --release && target/release/sacdfs --help
```

Running the tests is described in [CONTRIBUTING.md](CONTRIBUTING.md).

## Quick start

```sh
mkdir -p ~/sacd
sacdfs ~/music/sacd-images ~/sacd       # returns once mounted
ls ~/sacd/"Some Disc"/Stereo
mpv ~/sacd/"Some Disc"/Stereo/"01 - First Track.wav"
fusermount3 -u ~/sacd
```

`~/music/sacd-images` is one `.iso` image or a directory searched recursively for them. Add
`-f` to stay in the foreground and see the log, `--flatten` to drop the folders that hold
nothing but one image, and `--format dsf` if a library scanner should see only the DSF files.
The [user guide](docs/user-guide.md) has more.

## Options

```
sacdfs [OPTIONS] SOURCE MOUNTPOINT
```

`SOURCE` is an SACD image file, or a directory searched recursively for `*.iso` files (any
case). `MOUNTPOINT` is an existing directory, normally empty, and not `SOURCE` or a directory
above it.

| Option | Meaning |
|---|---|
| `-f`, `--foreground` | Stay in the foreground and log to standard error. Without it sacdfs returns once the filesystem is mounted and keeps running in the background. |
| `--threads N` | DST decoder and PCM converter threads; `0` (the default) means one per CPU. |
| `--cache-mb N` | Size of the cache of decoded and converted frames, in MiB (default `256`). DSF and WAV reads share it. On a NAS, `16` is recommended ([deployment guide](docs/deployment.md#memory)). |
| `--format LIST` | Which files to show: `dsf`, `wav`, or both, `dsf,wav` (the default). |
| `--pcm-rate HZ` | Sample rate of the WAV files: `88200`, `176400` (the default) or `352800`. Always 24-bit. |
| `--pcm-cutoff KHZ` | Low-pass cutoff of the conversion to PCM: `40` (the default) or `30` kHz, or `wide` (about 80 kHz; with `--pcm-rate 176400` or `352800` only). |
| `--pcm-gain DB` | Gain of the conversion to PCM, from -60 to +24 dB (default `0`). Samples that would clip are clamped, and the log says so once per file. |
| `--flatten` | Collapse each chain of folders that holds a single image into its outermost folder ([Layout](#layout)). Top-level folders of `SOURCE` are never collapsed. |
| `--sidecars LIST` | Files passed through from `SOURCE`, byte for byte, next to the images: extensions, and folder names ending in `/` whose whole content passes. `default` (the default) is `jpg,jpeg,png,webp,m3u,m3u8,Artwork/,Scans/`; `default,cue` adds to it, `default,-webp` removes from it, `none` passes nothing ([Layout](#layout)). |
| `--allow-other` | Let other users read the mount. Needs `user_allow_other` in `/etc/fuse.conf`. |
| `--index-cache DIR` | Keep the persistent seek index in `DIR` (default: `$XDG_CACHE_HOME/sacdfs`, else `~/.cache/sacdfs`). It remembers where frames are in each image, so that seeks after a remount skip most of their search. |
| `--no-index-cache` | Keep no persistent seek index: every mount starts with an empty one. |
| `--max-open-images N` | Most image files kept open at once, 1 to 1024 (default `64`). The least recently used is closed first, and any image unused for 5 minutes is closed. |
| `-v`, `--verbose` | Log more: `-v` info, `-vv` debug. `RUST_LOG` overrides it. |
| `-h`, `--help` | Print help (`-h` for a summary). |
| `-V`, `--version` | Print the version. |

Unmount with `fusermount3 -u MOUNTPOINT`; SIGINT (Ctrl-C), SIGTERM and SIGHUP unmount too. If files are
still open, the mount is detached instead, and sacdfs stops when the last one is closed. What
each option does in detail, and the seek index cache, are in the
[user guide](docs/user-guide.md#options).

## Before you delete extracted files

Already have DSF or DFF files extracted from your images? `sacdfs verify` checks, file by
file, whether sacdfs serves the same audio, so that you can delete the extracts and keep only
the images. It never deletes or changes anything.

```sh
sacdfs verify -o report.tsv ~/music/sacd-images                 # extracts next to the images
sacdfs verify -o report.tsv --map ~/music/dsf ~/music/sacd-images \
    ~/music/sacd-images ~/music/dsf                              # extracts in a tree of their own
```

Each file gets a status, IDENTICAL, DIFFERENT, NO-MATCH, UNSUPPORTED (such as DST-compressed
DFF), ERROR, or NOT-COMPARABLE (FLAC and WAV), with the reason; the summary gives the space
that deleting the IDENTICAL files would free, and which of them carry tags sacdfs doesn't
serve (MusicBrainz IDs, embedded art, ReplayGain, others). Tags, padding and the container
don't count: only the audio is compared. Details are in the
[user guide](docs/user-guide.md#before-you-delete-extracts).

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

- Each track is there twice, as a DSF and a WAV file of the same name; `--format` shows one
  kind only. `Stereo` and `Multichannel` appear only for the areas a disc has.
- File names come from the track titles (`NN.dsf` for a track without one); repeated names
  get ` (2)`, ` (3)`.
- Files that are not SACD images, folders with no `*.iso` file below them, and tracks the
  disc cannot play are left out.
- **Sidecar files** next to the images, `folder.jpg`, `cover.png`, an `Artwork/` or `Scans/`
  folder, `.m3u`/`.m3u8` playlists, are shown where they are in `SOURCE`, read-only and byte
  for byte, with their own size and times: players and file managers find the folder art,
  and you can delete extracted copies without losing it. `--sidecars` sets which
  ([user guide](docs/user-guide.md#sidecar-files-art-scans-playlists)). Hidden files are never
  passed, nor a link that points outside `SOURCE`.

With `--flatten`, a folder whose only content is one image, directly or through a chain of
folders that each hold nothing else, holds that image's `Stereo` and `Multichannel` folders
itself. Folders directly in `SOURCE` (usually artists) are always kept. For example

```
SOURCE/Artist/2001 - Album [SACD]/Artist - Album (rip)/Album/disc.iso
```

shows as `MOUNTPOINT/Artist/2001 - Album [SACD]/Stereo/01 - Title.dsf` instead of
`MOUNTPOINT/Artist/2001 - Album [SACD]/Artist - Album (rip)/Album/disc/Stereo/01 - Title.dsf`.
It is decided from the file names alone, so it does not slow down mounting. The sidecar files
of the collapsed folders all show in the one that is kept, next to `Stereo`.

## Formats and quality

The **DSF files** (DSD64, 2.8224 MHz, 1 bit) hold each track's audio bit-identical to what
the reference extractors (pysacd's `pysacd-extract`, scarletbook's `bench decode`) write, with
an ID3v2.4 tag from the disc's text. The **WAV files** are the same tracks converted to 24-bit
PCM at 176.4 kHz (or 88.2 or 352.8 kHz), band-limited to 40 kHz by default (30 kHz, or about
80 kHz with `--pcm-cutoff wide`), gapless from one track to the next, with the same tag in an
`id3 ` chunk and a `LIST`/`INFO` chunk; channel masks follow the disc's speaker layout
(stereo, 5.0, 5.1), and files over 4 GiB are RF64. The converter's pass band is flat to
±0.00003 dB, its stopband at least 114.9 dB down, and it adds nothing measurable above the
24-bit step ([docs/pcm-quality.md](docs/pcm-quality.md)). Sizes are exact; listing, `stat`
and reading a WAV file's header or tags never decode anything. Details, and how taggers and
scanners are served: [user guide](docs/user-guide.md#formats).

## Deployment

- **Locally:** run sacdfs on the Linux machine that holds the images, as in the
  [quick start](#quick-start). A cold seek returns in about 2 ms, a WAV seek in under 3 ms.
- **On a NAS (the main deployment):** the [container image](docs/deployment.md) runs sacdfs
  next to the images and shares its mount over SMB and NFS, so any client (Linux, macOS,
  Windows, phones, network players) sees the DSF and WAV files. sacdfs's reads of the images
  stay on the NAS, so a seek costs about one network round trip more than locally: 5 to 6 ms
  over SMB on a gigabit LAN. Docker, Podman, Compose and TrueNAS SCALE are covered.
- **On a Linux client, images on a network share:** sacdfs runs on the client, and each read
  of an image is a round trip to the server. A cold seek over NFS from a NAS's hard disks takes
  about 22 ms for plain DSD and 37 ms for DST on average, and about 2 ms once the seek index
  knows that part of the disc. The mount can also be re-exported by the kernel NFS server
  ([user guide](docs/user-guide.md#sharing-the-mount-over-nfs)).

How fast each one is, against the project's targets: [docs/performance.md](docs/performance.md).

## Limits

- Linux only, and read-only; other systems read the files over SMB or NFS from
  [the container](docs/deployment.md).
- DSF and WAV output only: no DSDIFF (DFF), no FLAC, no DoP, no 44.1/48 kHz or 16-bit PCM.
- The directory tree is read once, at mount time: images added to or removed from `SOURCE`
  later need a remount.
- A DSF file's header read decodes the track's first quarter second or so (a WAV file's
  decodes nothing); a program that reads every audio file (fingerprinting, loudness scans)
  converts every WAV file, so use `--format dsf` for those.
- A damaged image shows up as missing directories or files, filled audio (with a warning in
  the log) or an I/O error on one file. It never stops the mount.

## How it works

[docs/design.md](docs/design.md) describes the architecture and the reason for each decision:
the layout, the DSF and WAV mappings, seeking and the seek index, caching, concurrency,
the converter, the container and the release builds.

## Licence and credits

sacdfs is free software: you can redistribute it and/or modify it under the terms of the
[GNU General Public License](COPYING) as published by the Free Software Foundation, either
version 3 of the License, or (at your option) any later version.

- **[scarletbook](https://github.com/aclater/scarletbook-rs)** (MIT OR Apache-2.0) does all
  SACD parsing and DST decoding; sacdfs uses its public API only.
- **[pysacd](https://github.com/aclater/pysacd)** (GPL-2.0-or-later): the DSD-to-PCM
  converter's structure is ported from its `csrc/dsp.c`, and its `pysacd-extract` made the
  reference hashes the DSF files are checked against.
- **dsd2pcm** by Sebastian Gesemann (BSD-3-Clause): the converter's first stage uses its
  technique of filter lookup tables indexed by whole DSD bytes.
- The test corpus in `tests/corpus/`, from [sacd-spec](https://github.com/aclater/sacd-spec),
  is dedicated to the public domain under CC0 1.0 (see its [licence](tests/corpus/LICENSE)).
