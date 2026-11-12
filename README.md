# sacdfs

[![CI](https://github.com/aclater/sacdfs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/aclater/sacdfs/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/aclater/sacdfs/branch/main/graph/badge.svg)](https://codecov.io/gh/aclater/sacdfs)
[![CodeQL](https://github.com/aclater/sacdfs/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/aclater/sacdfs/actions/workflows/codeql.yml)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue)](COPYING)
[![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-orange)](Cargo.toml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/aclater/sacdfs/badge)](https://scorecard.dev/viewer/?uri=github.com/aclater/sacdfs)

sacdfs shows your Super Audio CD images (`.iso` files) as DSF and WAV tracks, with tags, to any
player or music server. Nothing is extracted: keep the ISOs, delete the extracted copies.

## Install

sacdfs runs on Linux: a NAS or a Linux computer. Log in to it over SSH and run (with the folder
that holds your `.iso` files); Macs and PCs then play from it over the network.

```sh
curl -fsSL https://github.com/aclater/sacdfs/releases/latest/download/install.sh \
  | sudo sh -s -- --iso-dir /volume1/music
```

It works out what it runs on and sets sacdfs up to match. Add `--dry-run` to see what it would
change first, and `--uninstall` to remove it again. No `curl`, checking the download first, or
TrueNAS: see the [install guide](docs/install/README.md).

Other ways in: [container](docs/install/docker-podman.md) ·
[release binary or source](docs/install/binary.md) · [guide per NAS and player](docs/install/README.md#pick-a-guide)

**Have a NAS?** Tell us how it went on real hardware: [beta testing](docs/beta-testing.md).

## Documentation

- [User guide](docs/user-guide.md): tags, [MusicBrainz lookup](docs/user-guide.md#your-own-tags---tags),
  sidecar files, mounting at boot, troubleshooting
- [Before you delete extracts](docs/user-guide.md#before-you-delete-extracts): `sacdfs verify`
- [Clients](docs/clients.md): players and music servers tested
- [Options](docs/options.md) · [what the mount shows](docs/layout.md) · [limits](docs/limits.md)
- [Deployment reference](docs/deployment.md) · [small NAS sizing](docs/deployment.md#small-nas-sizing)
- [Design](docs/design.md) · [performance](docs/performance.md) · [PCM quality](docs/pcm-quality.md)
- [Changelog](CHANGELOG.md) · [releases](https://github.com/aclater/sacdfs/releases) ·
  [contributing](CONTRIBUTING.md)

## Licence and credits

sacdfs is free software under the [GNU General Public License](COPYING), version 3 or (at your
option) any later version.

- [scarletbook](https://github.com/aclater/scarletbook-rs) (MIT OR Apache-2.0) does the SACD
  parsing, DST decoding and DSD-to-PCM conversion, through its public APIs.
- [pysacd](https://github.com/aclater/pysacd) (GPL-2.0-or-later) made the reference hashes the
  DSF files are checked against; sacdfs 0.1's converter was ported from it, with a first stage
  after Sebastian Gesemann's dsd2pcm (BSD-3-Clause).
- The test corpus in `tests/corpus/`, from [sacd-spec](https://github.com/aclater/sacd-spec), is
  public domain under CC0 1.0 ([licence](tests/corpus/LICENSE)).
