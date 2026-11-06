# sacdfs

[![CI](https://github.com/aclater/sacdfs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/aclater/sacdfs/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/aclater/sacdfs/branch/main/graph/badge.svg)](https://codecov.io/gh/aclater/sacdfs)
[![CodeQL](https://github.com/aclater/sacdfs/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/aclater/sacdfs/actions/workflows/codeql.yml)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue)](COPYING)
[![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-orange)](Cargo.toml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/aclater/sacdfs/badge)](https://scorecard.dev/viewer/?uri=github.com/aclater/sacdfs)

sacdfs is a read-only FUSE filesystem for Linux that shows Super Audio CD disc images (ISOs)
as DSF files, and as 24-bit WAV files next to them, decoded on demand. Players, taggers and
music servers see every track of every disc, with tags, without extracting anything: you
keep the ISOs and can delete the extracted copies.

## Quick start

**On a NAS or any Linux host with Podman or Docker:** the container shares the mount over
SMB and NFS. Put your folder of `.iso` files in place of `/srv/music`:

```sh
sudo podman run -d --name sacdfs --restart unless-stopped \
  --device /dev/fuse --cap-add SYS_ADMIN --cap-add DAC_READ_SEARCH \
  --security-opt label=disable \
  -p 445:445 -p 2049:2049 \
  -v /srv/music:/images:ro \
  -v sacdfs-cache:/cache \
  ghcr.io/aclater/sacdfs:0.4.0
```

The [Docker and Podman guide](docs/install/docker-podman.md) explains each line and how to
check it works.

**Directly on Linux:** build it (Rust 1.85 or later; the [binary guide](docs/install/binary.md)
has release downloads), then mount. You need `fusermount3` (package `fuse3`).

```sh
cargo build --release
mkdir -p ~/sacd
target/release/sacdfs ~/music/sacd-images ~/sacd    # returns once mounted
ls ~/sacd                                           # a folder per disc, with Stereo/ (and Multichannel/)
fusermount3 -u ~/sacd
```

`~/music/sacd-images` is one `.iso` file or a folder searched recursively for them. All options:
[docs/options.md](docs/options.md).

## Install on your system

- [Install guides](docs/install/README.md): TrueNAS SCALE, OpenMediaVault, Unraid,
  Synology, QNAP, UGREEN, Docker/Podman, and Roon, Plex, Jellyfin, Navidrome, Lyrion, MPD.
- [Release binary, crates.io or source](docs/install/binary.md).
- [Clients](docs/clients.md): what sacdfs has been tested with, and how to set each one up.

## Before you delete extracted files

`sacdfs verify` checks, file by file, whether sacdfs serves the same audio as your extracted
DSF or DFF files. It never deletes or changes anything.

```sh
sacdfs verify -o report.tsv ~/music/sacd-images
```

Statuses, extracts in their own tree, and keeping their tags:
[user guide](docs/user-guide.md#before-you-delete-extracts).

## Documentation

- [User guide](docs/user-guide.md): options in detail, tags, sidecar files, fstab and systemd, troubleshooting
- [Options](docs/options.md), [what the mount shows](docs/layout.md) and [limits](docs/limits.md)
- [Deployment guide](docs/deployment.md): the container reference, every setting and why; [Small NAS: sizing](docs/deployment.md#small-nas-sizing) for memory and cores
- [Design](docs/design.md) (how it works, and why), [performance](docs/performance.md), [PCM quality](docs/pcm-quality.md)
- [Changelog](CHANGELOG.md), [releases](https://github.com/aclater/sacdfs/releases), and
  what each minor version brought: [RELEASES.md](https://github.com/aclater/sacd/blob/main/RELEASES.md)
- [Contributing](CONTRIBUTING.md)

## Licence and credits

sacdfs is free software: you can redistribute it and/or modify it under the terms of the
[GNU General Public License](COPYING) as published by the Free Software Foundation, either
version 3 of the License, or (at your option) any later version.

- **[scarletbook](https://github.com/aclater/scarletbook-rs)** (MIT OR Apache-2.0) does all
  SACD parsing and DST decoding, and its `scarletbook-pcm` the DSD-to-PCM conversion; sacdfs
  uses their public APIs only.
- **[pysacd](https://github.com/aclater/pysacd)** (GPL-2.0-or-later): its `pysacd-extract`
  made the reference hashes the DSF files are checked against. sacdfs 0.1's own converter
  (replaced by scarletbook-pcm, #84) was ported from its `csrc/dsp.c`, with a first stage
  using the byte lookup-table technique of Sebastian Gesemann's dsd2pcm (BSD-3-Clause).
- The test corpus in `tests/corpus/`, from [sacd-spec](https://github.com/aclater/sacd-spec),
  is dedicated to the public domain under CC0 1.0 (see its [licence](tests/corpus/LICENSE)).
