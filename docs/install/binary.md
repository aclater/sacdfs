[sacdfs](../../README.md) › [Documentation](../README.md) › [Install](README.md) › Linux without a container: install sacdfs

# Linux without a container: install sacdfs

This puts sacdfs on a Linux computer and shows your SACD images as a folder of DSF and WAV files
you can open with any player. About 10 minutes.

On a NAS you reach over SSH, use
[the NAS steps](nas-binary.md) instead. To share the discs with other computers, use
[Docker or Podman](docker-podman.md).

Checked on sacdfs 0.6.0 on Fedora 45 (a normal user, FUSE 3), and steps 2 to 6 again on 0.7.2.
See [Details](#details).

## What you need

- A Linux computer (x86_64 or aarch64; 32-bit x86: [see below](#32-bit-x86)) and a terminal on it.
- A folder with your SACD images (`.iso` files). The examples use `~/music/sacd-images`.
- `fusermount3`, which lets a normal user mount (package `fuse3`). Root needs nothing.

## Steps

1. **Install the mount helper.** Fedora or RHEL:

   ```sh
   sudo dnf install fuse3
   ```

   Debian or Ubuntu:

   ```sh
   sudo apt install fuse3
   ```

2. **Download sacdfs and its checksum.** `uname -m` prints your CPU. The commands are for
   `x86_64`; on `aarch64`, write `aarch64` for `x86_64` in every file name of steps 2 to 4.

   ```sh
   cd ~
   curl -fLO https://github.com/dsd-toolbox/sacdfs/releases/download/v0.7.2/sacdfs-0.7.2-x86_64-linux.tar.gz
   curl -fLO https://github.com/dsd-toolbox/sacdfs/releases/download/v0.7.2/sacdfs-0.7.2-x86_64-linux.tar.gz.sha256
   ```

3. **Check the download.**

   ```sh
   sha256sum -c sacdfs-0.7.2-x86_64-linux.tar.gz.sha256
   ```

   *You should see* `sacdfs-0.7.2-x86_64-linux.tar.gz: OK`.

4. **Unpack it and put the program in your own `bin` folder.**

   ```sh
   tar -xzf sacdfs-0.7.2-x86_64-linux.tar.gz
   mkdir -p ~/.local/bin
   install -m 0755 sacdfs-0.7.2-x86_64-linux/sacdfs ~/.local/bin/
   ~/.local/bin/sacdfs --version
   ```

   *You should see* `sacdfs 0.7.2`.

5. **Make an empty folder to mount on, and start sacdfs.** While it runs, this folder shows the
   DSF and WAV files; anything already in it is hidden.

   ```sh
   mkdir -p ~/sacd
   ~/.local/bin/sacdfs ~/music/sacd-images ~/sacd
   ```

   *You should see* nothing: the prompt comes back and sacdfs keeps running in the background.

6. **To stop it,** type:

   ```sh
   fusermount3 -u ~/sacd
   ```

   Never stop sacdfs with `kill -9`: the mount stays behind with nobody serving it, and every
   program that touches the folder fails with `Transport endpoint is not connected`. If that happens, run
   `fusermount3 -u ~/sacd` (or `sudo umount -l ~/sacd`).

## Check it worked

```sh
ls ~/sacd
```

*You should see* one folder per disc. Each holds `Stereo` and, on multichannel discs, `Multichannel`,
with files such as `01 - Title.dsf` and `01 - Title.wav`. Open one in your player.

## If something goes wrong

- **You see** `a user other than root needs FUSE's mount helper, fusermount3, and it isn't installed`.
  **Do this:** step 1.
- **You see** `the folder to mount on ... does not exist`. **Do this:** `mkdir -p ~/sacd` (step 5).
- **You see** `Illegal instruction`. **Do this:** you have the `x86_64-v3` build on an older CPU. Use
  the `x86_64` file names of step 2.

More: [the user guide](../user-guide.md) (running it, fstab, systemd, troubleshooting).

## Details

### Binaries

Each [release](https://github.com/dsd-toolbox/sacdfs/releases) has these builds:

- `sacdfs-<version>-x86_64-linux.tar.gz`: generic x86-64, for any x86_64 CPU. Its WAV converter
  already uses AVX2 where the CPU has it.
- `sacdfs-<version>-x86_64-v3-linux.tar.gz`: for x86-64-v3 CPUs (AVX2, BMI2, FMA: most Intel and
  AMD CPUs since about 2015, the Intel N100 family included), with all of sacdfs built for them:
  DST areas read 10 to 18 % faster ([performance](../performance.md#straight-reads)). On an older
  CPU it stops with an illegal-instruction error. `grep -qw avx2 /proc/cpuinfo && echo v3` tells.
- `sacdfs-<version>-aarch64-linux.tar.gz`: 64-bit ARM.
- `sacdfs-<version>-i686-linux.tar.gz`: 32-bit x86, from 0.7.1 ([32-bit x86](#32-bit-x86)).

Each archive holds `sacdfs`, `sacdfs-ctl`, `sacdfs.service`, `sacdfs.conf.example`, `COPYING`,
`README.md` and `CHANGELOG.md` (0.5.0 and earlier: no `sacdfs-ctl`, `sacdfs.service` or
`sacdfs.conf.example`). Since 0.6.2 it also holds the installer, `install.sh`,
and `nas-compose.yaml`: `sudo sh install.sh --iso-dir ...` in the unpacked folder uses the files
next to it and downloads nothing ([the installer](README.md#the-installer-one-line)). The released
`install.sh` verifies the files beside it against checksums built into it when the release was
packed, and uses only verified copies; if one is missing or doesn't match, it uses none of them and
downloads the release. If the binary there doesn't start on this machine (the x86-64-v3 build on an
older CPU, say), it says so and downloads the release for this machine instead. The
installer is also a release asset of its own, `install.sh`, with its `.sha256` and attestation.

**What it needs.** glibc 2.17 or later (0.4.1 and earlier needed 2.28); every release build is
checked in CI to need no newer symbol. As a user other than root, FUSE 3's `fusermount3`, or from
0.5.0 FUSE 2's `fusermount` where that is all there is. Root needs neither: it mounts without a
helper. Kernels older than 4.18 (RHEL 7's 3.10, for instance) are untested.

**Where it was checked.** The x86_64 builds mount the test corpus in a UBI 7 container (glibc 2.17)
with the same bytes as on a current system, and on QNAP's QuTScloud 5.2.9 (glibc 2.21;
[qnap.md](qnap.md)). The 0.4.1 builds (glibc 2.28) were tested on the oldest of RHEL 8, Debian 10,
Ubuntu 20.04 and Fedora 29 with their own kernels (4.18 to 5.4) and `fusermount3` (3.3 to 3.9):
mounting in the foreground and background, with and without `--allow-other`, identical reads, and a
clean stop on SIGTERM with a file open ([testing](../design.md#oldest-distributions-58)).

**With `gh` instead of `curl`,** and checking where it was built (for the other builds, write
`x86_64-v3` or `aarch64` for `x86_64`):

```sh
gh release download v0.7.2 -R dsd-toolbox/sacdfs -p 'sacdfs-0.7.2-x86_64-linux.tar.gz*'
sha256sum -c sacdfs-0.7.2-x86_64-linux.tar.gz.sha256
gh attestation verify sacdfs-0.7.2-x86_64-linux.tar.gz --owner aclater
```

The checksum shows the download is intact; the attestation (GitHub's build provenance, signed with
Sigstore) shows this repository's release workflow built it, and from which commit.
`gh attestation verify` also accepts the `.intoto.jsonl` bundle next to each archive, with `--bundle`,
to check offline. Releases up to 0.7.2 were built while the project lived under the `aclater`
account, so their attestations are found with `--owner aclater`; from 0.8.0 use
`-R dsd-toolbox/sacdfs`. On TerraMaster TOS, `install` is a different program: use `cp`
([the NAS steps](nas-binary.md)).

### 32-bit x86

From 0.7.1, `sacdfs-<version>-i686-linux.tar.gz` is a static build for 32-bit x86 CPUs from the
Pentium 4 and Atom on (SSE2), such as Atom-based NAS units where `uname -m` prints `i686`. It needs
no C library, so the glibc 2.17 floor above doesn't apply; FUSE is needed as above. Its archive has
no `install.sh` or `nas-compose.yaml`: the installer and the container image are 64-bit only, so
install it by hand with [the NAS steps](nas-binary.md), `i686` in place of `x86_64` in the file
names. From 0.7.1, the installer run on 32-bit x86 (an `i686` kernel, or an x86-64 kernel with
32-bit programs) stops before downloading anything, names this archive and links here. DST
decoding takes about 2.3 to 2.5 times the CPU time of the x86_64 build on the same core. If MPD
or Jellyfin scans the mount, use `--format dsf`: at Atom speed their first scan costs about 25 to
40 s of the whole CPU per disc with WAV files and about 1 s without
([deployment](../deployment.md#players-that-scan-the-library-mpd-moode-volumio-jellyfin-plex)).

On a kernel older than 3.16 with system call auditing on (`auditd` running), starting any current
static program can crash the kernel (CVE-2014-4508), and sacdfs is no exception: check that no
`auditd` runs before the first start.

*Checked:* in CI, the whole test suite on every pull request, and on each release the packaged
binary serves the test corpus with the x86_64 build's bytes. Before 0.7.1, the same build on an
i9-13900H served the corpus (910 files) and three real discs (94 DSF and WAV files) with the bytes
of the x86_64 build. Test builds of the same recipe ran on an ASUSTOR AS-302T (Atom D2700, ADM) in
[#456](https://github.com/dsd-toolbox/sacdfs/issues/456), and on CentOS 7 i386 (kernel 3.10) in a VM,
where the audit crash above was seen. Not checked: other 32-bit systems, and multichannel WAV
playing in real time on an Atom.

### From source

sacdfs needs Rust 1.85 or later and nothing else: it talks to the kernel through `/dev/fuse` and, as
a user other than root, mounts with `fusermount3` (or FUSE 2's `fusermount`), without linking libfuse.

```sh
cargo install --locked sacdfs     # from crates.io, from 0.4.0
# before 0.4.0, from the release tag:
cargo install --locked --git https://github.com/dsd-toolbox/sacdfs --tag v0.3.0
# or, in a clone:
cargo build --release && target/release/sacdfs --help
```

Tested on 0.4.0: `cargo install --locked sacdfs` in a clean `rust:1.99.1` container, with no other
packages, installs a working `sacdfs`. Running the tests: [CONTRIBUTING.md](../../CONTRIBUTING.md).

### What was checked

0.6.0 release archive, steps 1 to 6 as written, as a normal user on Fedora 45 (FUSE 3.18):
checksum `OK`, `--version` printed `sacdfs 0.6.0`, 22 of 22 files of two test images identical to a
reference. 0.7.2 release archive (x86_64), steps 2 to 6 on Fedora 45 as a normal user: checksum `OK`,
`--version` printed `sacdfs 0.7.2`, the test corpus (455 DSF and 455 WAV files) listed, one disc's files
identical to those served by the 0.7.2 container. Not checked on 0.6.0: aarch64, the `x86_64-v3` build, `cargo install`, and other
distributions. Options: [options.md](../options.md).

## See also

- [Linux packages](packages.md)
- [NAS over SSH](nas-binary.md)
- [User guide](../user-guide.md)
- [Options](../options.md)

Part of the [DSD Toolbox](https://github.com/dsd-toolbox/dsd-toolbox).
