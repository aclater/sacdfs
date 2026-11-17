# sacdfs

[![CI](https://github.com/aclater/sacdfs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/aclater/sacdfs/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/aclater/sacdfs/branch/main/graph/badge.svg)](https://codecov.io/gh/aclater/sacdfs)
[![CodeQL](https://github.com/aclater/sacdfs/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/aclater/sacdfs/actions/workflows/codeql.yml)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue)](COPYING)
[![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-orange)](Cargo.toml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/aclater/sacdfs/badge)](https://scorecard.dev/viewer/?uri=github.com/aclater/sacdfs)

sacdfs shows your Super Audio CD images (`.iso` files) as DSF and WAV tracks, with tags, to any
player or music server. Nothing is extracted: keep the ISOs, delete the extracted copies.

## Platforms

| System | How | Guide |
|---|---|---|
| Linux, including NAS systems | Release builds for x86_64, x86_64-v3 and aarch64 (`.tar.gz` and `install.sh`), `.rpm` and `.deb` release files (x86_64 and aarch64; Fedora, RHEL 9 and 10, Debian 12 and 13, Ubuntu 22.04 and 24.04) and a COPR repository for dnf (Fedora 44 and 45, RHEL 9 and 10), and a container image. Needs glibc 2.17 or later; kernels older than 4.18 are untested | [install guide](docs/install/README.md) · [binary](docs/install/binary.md) · [packages](docs/install/packages.md) · [container](docs/install/docker-podman.md) |
| Windows 10 and 11, x64 | Release build since 0.6.5 (`sacdfs-<version>-x86_64-windows.zip`), with [WinFsp](https://winfsp.dev) 2.1 or later; shows the images as a read-only drive letter. Not code-signed. Checked on Windows 11 in a VM | [windows.md](docs/install/windows.md) |
| macOS 11 or later | Release builds since 0.6.5 for Apple Silicon and Intel (`sacdfs-<version>-aarch64-macos.tar.gz`, `-x86_64-macos.tar.gz`); mounts through the Mac's own NFS client, with no macFUSE. Not signed or notarised. Apple Silicon checked on macOS 27.2 in a VM on Apple hardware; Intel only on the release's CI runner (macOS 15) | [macos.md](docs/install/macos.md) |

Any other system, and any Mac or Windows PC, can play the discs from a NAS or Linux computer
that runs sacdfs, over its SMB or NFS share ([clients](docs/clients.md)).

## Quick start

The steps below install sacdfs on a NAS or Linux computer; for a Windows PC or a Mac, see
[Platforms](#platforms).

You need a NAS or Linux computer you can log in to over SSH as an administrator, and the folder
on it that holds your `.iso` files. The commands use `/volume1/music`, a music folder on a
Synology NAS; type your own folder instead.

1. Install sacdfs and start it:

   ```sh
   curl -fsSL https://github.com/aclater/sacdfs/releases/latest/download/install.sh \
     | sudo sh -s -- --iso-dir /volume1/music
   ```

   The installer works out what it runs on and sets sacdfs up to match. It ends with `Done.` and
   where the tracks are: a folder, or, where Docker or Podman runs, SMB and NFS addresses to give
   your player (then skip to step 3).

2. Look at the tracks in that folder: `/srv/sacd` on a Linux computer, `/volume1/sacd` on a
   Synology NAS. On a Linux computer:

   ```sh
   ls -R /srv/sacd
   ```

   *You should see* a folder per disc, with `Stereo` and `Multichannel` folders of numbered
   `.dsf` and `.wav` tracks.

3. Play from your Macs, PCs and players over the network: share that folder with the system's
   own file sharing (SMB or NFS), or use the addresses the container route printed. Each
   [guide per NAS and player](docs/install/README.md#pick-a-guide) shows how.

Add `--dry-run` to the installer to see what it would change first, and `--uninstall` to remove
sacdfs again. No `curl`, checking the download first, or TrueNAS: see the
[install guide](docs/install/README.md). Other ways in: [container](docs/install/docker-podman.md)
· [release binary or source](docs/install/binary.md).

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
