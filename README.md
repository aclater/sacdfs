<img src="https://raw.githubusercontent.com/dsd-toolbox/sacdfs/main/docs/assets/icon-256.png" alt="" width="96">

# sacdfs

[![CI](https://github.com/dsd-toolbox/sacdfs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/dsd-toolbox/sacdfs/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/dsd-toolbox/sacdfs/branch/main/graph/badge.svg)](https://codecov.io/gh/dsd-toolbox/sacdfs)
[![CodeQL](https://github.com/dsd-toolbox/sacdfs/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/dsd-toolbox/sacdfs/actions/workflows/codeql.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/dsd-toolbox/sacdfs/badge)](https://scorecard.dev/viewer/?uri=github.com/dsd-toolbox/sacdfs)
[![crates.io](https://img.shields.io/crates/v/sacdfs)](https://crates.io/crates/sacdfs)
[![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-orange)](Cargo.toml)
[![Licence: GPL-3.0-or-later](https://img.shields.io/badge/licence-GPL--3.0--or--later-blue)](COPYING)

sacdfs shows your Super Audio CD images (`.iso` files) as DSF and WAV tracks, with tags, to any
player or music server, with nothing extracted: keep the ISOs, delete the extracted copies. On a 2011 dual-core 32-bit Atom D2700 NAS, stereo discs play; multichannel DST was too slow there until the decoder got about 1.7 times faster, and has not been measured on that NAS since ([#456](https://github.com/dsd-toolbox/sacdfs/issues/456)).

## Platforms

| System | How | Guide |
|---|---|---|
| Linux, including NAS systems | Release builds for x86_64, x86_64-v3 and aarch64 (`.tar.gz` and `install.sh`), from 0.7.1 a static build for 32-bit x86 (`.tar.gz` only, [32-bit x86](docs/install/binary.md#32-bit-x86)), `.rpm` and `.deb` release files (x86_64 and aarch64; Fedora, RHEL 9 and 10, Debian 12 and 13, Ubuntu 22.04 and 24.04) and a COPR repository for dnf (Fedora 44 and 45, RHEL 9 and 10), and a container image. Needs glibc 2.17 or later; kernels older than 4.18 are untested | [install guide](docs/install/README.md) · [binary](docs/install/binary.md) · [packages](docs/install/packages.md) · [container](docs/install/docker-podman.md) |
| Windows 10 and 11, x64 | Release build since 0.6.5 (<!-- facts:release repo=dsd-toolbox/sacdfs fmt='`sacdfs-{version}-x86_64-windows.zip`' -->`sacdfs-0.8.0-x86_64-windows.zip`<!-- /facts -->), with [WinFsp](https://winfsp.dev) 2.1 or later; shows the images as a read-only drive letter. Not code-signed yet: signed once the certificate is in place. Checked on Windows 11 25H2 and Windows 10 22H2 in VMs | [windows.md](docs/install/windows.md) |
| macOS 11 or later | Release builds since 0.6.5 for Apple Silicon and Intel (<!-- facts:release repo=dsd-toolbox/sacdfs fmt='`sacdfs-{version}-aarch64-macos.tar.gz`' -->`sacdfs-0.8.0-aarch64-macos.tar.gz`<!-- /facts -->, `-x86_64-macos.tar.gz`); mounts through the Mac's own NFS client, with no macFUSE. Signed with a Developer ID and notarised by Apple from 0.8.0 (earlier releases are not). Apple Silicon checked on macOS 27.2 in a VM on Apple hardware; Intel only on the release's CI runner (macOS 15) | [macos.md](docs/install/macos.md) |

Any other system, and any Mac or Windows PC, can play the discs from a NAS or Linux computer
that runs sacdfs, over its SMB or NFS share ([clients](docs/clients.md)).

## Quick start

The steps below install sacdfs on a NAS or Linux computer; for a Windows PC or a Mac, see
[Platforms](#platforms).

You need a NAS or Linux computer you can log in to over SSH as an administrator, and the folder
on it that holds your `.iso` files. The commands use `/volume1/music`, a music folder on a
Synology NAS; type your own folder instead.

1. Install sacdfs and start it:

   <!-- docs-run: skip="installs sacdfs for real, which needs /dev/fuse and a mount; the docs-run container has neither" -->
   ```sh
   curl -fsSL https://github.com/dsd-toolbox/sacdfs/releases/latest/download/install.sh \
     | sudo sh -s -- --iso-dir /volume1/music
   ```

   The installer works out what it runs on and sets sacdfs up to match. It ends with `Done.` and
   where the tracks are: a folder, or, where Docker or Podman runs, SMB and NFS addresses to give
   your player (then skip to step 3).

2. Look at the tracks in that folder: `/srv/sacd` on a Linux computer, `/volume1/sacd` on a
   Synology NAS. On a Linux computer:

   <!-- docs-run: skip="needs the reader's own SACD images or a running mount, which the docs-run container does not have" -->
   ```sh
   ls -R /srv/sacd
   ```

   *You should see* a folder per disc, with `Stereo` and `Multichannel` folders of numbered
   `.dsf` and `.wav` tracks.

3. Play from your Macs, PCs and players over the network: share that folder with the system's
   own file sharing (SMB or NFS), or use the addresses the container route printed. Each
   [guide per NAS and player](docs/install/README.md#pick-a-guide) shows how.

Add `--dry-run` to the installer to see what it would change first, and `--uninstall` to remove
sacdfs again. No `curl`, or checking the download first: see the
[install guide](docs/install/README.md). On TrueNAS SCALE, install it from Apps, Discover Apps
([guide](docs/install/truenas-scale.md)). Other ways in: [container](docs/install/docker-podman.md)
· [release binary or source](docs/install/binary.md).

**Have a NAS?** Tell us how it went on real hardware: [beta testing](docs/beta-testing.md).

## Documentation

All documents: [docs/README.md](docs/README.md).

- [User guide](docs/user-guide.md): tags, [MusicBrainz lookup](docs/user-guide.md#your-own-tags---tags),
  sidecar files, mounting at boot, troubleshooting
- [Before you delete extracts](docs/user-guide.md#before-you-delete-extracts): `sacdfs verify`
- [Clients](docs/clients.md): players and music servers tested
- [Options](docs/options.md) (and `man sacdfs`) · [what the mount shows](docs/layout.md) · [limits](docs/limits.md)
- [Deployment reference](docs/deployment.md) · [small NAS sizing](docs/deployment.md#small-nas-sizing)
- [Design](docs/design.md) · [performance](docs/performance.md) · [PCM quality](docs/pcm-quality.md)
- [Changelog](CHANGELOG.md) · [releases](https://github.com/dsd-toolbox/sacdfs/releases) ·
  [contributing](CONTRIBUTING.md) · [releasing](docs/release.md)

## Licence and credits

The code is free software under the [GNU General Public License](COPYING), version 3 or (at your
option) any later version. The text, documentation, icons and data are licensed under
[CC BY 4.0](LICENSES/CC-BY-4.0.txt): reuse them with credit to the sacdfs authors.

How to cite: see [CITATION.cff](CITATION.cff), which GitHub's "Cite this repository" button reads.
From 0.8.0 on, each release is archived with a DOI on Zenodo.

- [scarletbook](https://github.com/aclater/scarletbook-rs) (MIT OR Apache-2.0) does the SACD
  parsing and DST decoding, through its public APIs.
- [dsd2pcm](https://crates.io/crates/dsd2pcm) (MIT OR Apache-2.0) does the DSD-to-PCM
  conversion behind the WAV files, with its `precise` filter by default and `fast`, which
  reproduces, byte for byte, scarletbook-rs's scarletbook-pcm 0.7.0 that sacdfs used before.
- [pysacd](https://github.com/dsd-toolbox/pysacd) (GPL-2.0-or-later) made the reference hashes the
  DSF files are checked against; sacdfs 0.1's converter was ported from it, with a first stage
  after Sebastian Gesemann's dsd2pcm (BSD-3-Clause).
- The test corpus in `tests/corpus/`, from [sacd-spec](https://github.com/aclater/sacd-spec), is
  public domain under CC0 1.0 ([licence](tests/corpus/LICENSE)).

Part of the [DSD Toolbox](https://github.com/dsd-toolbox/dsd-toolbox).
