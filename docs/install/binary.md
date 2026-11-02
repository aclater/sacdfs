# sacdfs without a container: release binary, crates.io or source

sacdfs runs on Linux (x86_64) and needs FUSE 3's `fusermount3` helper (package `fuse3` on
most distributions). Each [release](https://github.com/aclater/sacdfs/releases) has two
builds. From 0.4.0, sacdfs is also on crates.io (`cargo install sacdfs`,
[below](#from-source); #155). That path is available from 0.4.0 but not yet validated.
Running it is in the [user guide](../user-guide.md); the options are in
[options.md](../options.md).

## Binaries

- `sacdfs-<version>-x86_64-linux.tar.gz`: generic x86-64, for any x86_64 CPU. Its WAV
  converter (scarletbook-pcm) already uses AVX2 where the CPU has it.
- `sacdfs-<version>-x86_64-v3-linux.tar.gz`: for x86-64-v3 CPUs (AVX2, BMI2, FMA: most Intel
  and AMD CPUs since about 2015, the Intel N100 family included), with all of sacdfs built for
  them: DST areas read 10 to 18 % faster ([performance](../performance.md#straight-reads)).
  On an older CPU this build stops with an illegal-instruction error. `grep -qw avx2 /proc/cpuinfo && echo v3` tells.

Both need glibc 2.28 or later (RHEL 8, Debian 10, Ubuntu 20.04, Fedora 29 and later). The
generic build was tested on the oldest of each, with their own kernels (4.18 to 5.4) and
`fusermount3` (3.3 to 3.9): mounting in the foreground and in the background, with and
without `--allow-other`, reads identical to a current system, and a clean stop on SIGTERM
with a file open ([testing](../design.md#oldest-distributions-58)). Each
archive holds `sacdfs`, `COPYING`, `README.md` and `CHANGELOG.md`. To download, check and
install one:

```sh
v=0.3.0 a=x86_64          # or a=x86_64-v3
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

## From source

sacdfs needs Rust 1.85 or later and nothing else: it talks to the kernel through `/dev/fuse`
and mounts with `fusermount3`, without linking libfuse.

```sh
cargo install --locked sacdfs     # from crates.io, from 0.4.0 (not yet validated)
# before 0.4.0, from the release tag:
cargo install --locked --git https://github.com/aclater/sacdfs --tag v0.3.0
# or, in a clone:
cargo build --release && target/release/sacdfs --help
```

Running the tests is described in [CONTRIBUTING.md](../../CONTRIBUTING.md).
