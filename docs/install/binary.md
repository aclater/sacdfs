# Linux without a container: install sacdfs

This puts sacdfs on a Linux computer and shows your SACD images as a folder of DSF and WAV files
you can open with any player. About 10 minutes. On a NAS you reach over SSH, use
[the NAS steps](nas-binary.md) instead. To share the discs with other computers, use
[Docker or Podman](docker-podman.md).

Checked on sacdfs 0.6.0 on Fedora 45 (a normal user, FUSE 3). See [Details](#details).

## What you need

- A Linux computer (x86_64 or aarch64) and a terminal on it.
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

2. **Download sacdfs and its checksum.** `uname -m` prints your CPU: `x86_64` or `aarch64`.
   Set `a` to that.

   ```sh
   v=0.6.0 a=x86_64
   cd ~
   curl -fLO "https://github.com/aclater/sacdfs/releases/download/v$v/sacdfs-$v-$a-linux.tar.gz"
   curl -fLO "https://github.com/aclater/sacdfs/releases/download/v$v/sacdfs-$v-$a-linux.tar.gz.sha256"
   ```

3. **Check the download.**

   ```sh
   sha256sum -c "sacdfs-$v-$a-linux.tar.gz.sha256"
   ```

   *You should see* `sacdfs-0.6.0-x86_64-linux.tar.gz: OK`.

4. **Unpack it and put the program in your own `bin` folder.**

   ```sh
   tar -xzf "sacdfs-$v-$a-linux.tar.gz"
   mkdir -p ~/.local/bin
   install -m 0755 "sacdfs-$v-$a-linux/sacdfs" ~/.local/bin/
   ~/.local/bin/sacdfs --version
   ```

   *You should see* `sacdfs 0.6.0`.

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
  `a=x86_64` in step 2.

More: [the user guide](../user-guide.md) (running it, fstab, systemd, troubleshooting).

## Details

### Binaries

Each [release](https://github.com/aclater/sacdfs/releases) has three builds:

- `sacdfs-<version>-x86_64-linux.tar.gz`: generic x86-64, for any x86_64 CPU. Its WAV converter
  already uses AVX2 where the CPU has it.
- `sacdfs-<version>-x86_64-v3-linux.tar.gz`: for x86-64-v3 CPUs (AVX2, BMI2, FMA: most Intel and
  AMD CPUs since about 2015, the Intel N100 family included), with all of sacdfs built for them:
  DST areas read 10 to 18 % faster ([performance](../performance.md#straight-reads)). On an older
  CPU it stops with an illegal-instruction error. `grep -qw avx2 /proc/cpuinfo && echo v3` tells.
- `sacdfs-<version>-aarch64-linux.tar.gz`: 64-bit ARM.

Each archive holds `sacdfs`, `sacdfs-ctl`, `sacdfs.service`, `sacdfs.conf.example`, `COPYING`,
`README.md` and `CHANGELOG.md` (0.5.0 and earlier: no `sacdfs-ctl`, `sacdfs.service` or
`sacdfs.conf.example`). From the release after 0.6.0 it also holds the installer, `install.sh`,
and `nas-compose.yaml`: `sudo sh install.sh --iso-dir ...` in the unpacked folder uses the files
next to it and downloads nothing ([the installer](README.md#the-installer-one-line)). The
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

**With `gh` instead of `curl`,** and checking where it was built:

```sh
v=0.6.0 a=x86_64          # or a=x86_64-v3, or a=aarch64
gh release download "v$v" -R aclater/sacdfs -p "sacdfs-$v-$a-linux.tar.gz*"
sha256sum -c "sacdfs-$v-$a-linux.tar.gz.sha256"
gh attestation verify "sacdfs-$v-$a-linux.tar.gz" -R aclater/sacdfs
```

The checksum shows the download is intact; the attestation (GitHub's build provenance, signed with
Sigstore) shows this repository's release workflow built it, and from which commit.
`gh attestation verify` also accepts the `.intoto.jsonl` bundle next to each archive, with `--bundle`,
to check offline. On TerraMaster TOS, `install` is a different program: use `cp`
([the NAS steps](nas-binary.md)).

### From source

sacdfs needs Rust 1.85 or later and nothing else: it talks to the kernel through `/dev/fuse` and, as
a user other than root, mounts with `fusermount3` (or FUSE 2's `fusermount`), without linking libfuse.

```sh
cargo install --locked sacdfs     # from crates.io, from 0.4.0
# before 0.4.0, from the release tag:
cargo install --locked --git https://github.com/aclater/sacdfs --tag v0.3.0
# or, in a clone:
cargo build --release && target/release/sacdfs --help
```

Tested on 0.4.0: `cargo install --locked sacdfs` in a clean `rust:1.99.1` container, with no other
packages, installs a working `sacdfs`. Running the tests: [CONTRIBUTING.md](../../CONTRIBUTING.md).

### What was checked

0.6.0 release archive, steps 1 to 6 as written, as a normal user on Fedora 45 (FUSE 3.18):
checksum `OK`, `--version` printed `sacdfs 0.6.0`, 22 of 22 files of two test images identical to a
reference. Not checked on 0.6.0: aarch64, the `x86_64-v3` build, `cargo install`, and other
distributions. Options: [options.md](../options.md).
