# Deploying sacdfs on a NAS

> **Installing?** Start with the short [install guides](install/README.md): one per NAS system
> and music server, with numbered steps and a check that it works. This page is the
> reference behind them: every setting, and why.

sacdfs can run in two places ([#17](https://github.com/aclater/sacdfs/issues/17)):

- **On the NAS (server-side, the main deployment).** sacdfs runs next to the images, in a
  container, and shares its mount over SMB and NFS. Every client (Linux, macOS, Windows,
  phones, network players) sees ordinary DSF and WAV files. A seek costs sacdfs's few
  milliseconds on the NAS plus about one network round trip, because sacdfs's own reads of
  the image stay on the NAS: 5 to 6 ms for a cold seek over SMB on a gigabit LAN
  ([performance](performance.md#nas-container-server-side)). On a NAS with hard disks it is
  15 to 22 ms when the NAS has that part of the image cached, and 31 to 61 ms when it must
  read it from its disks ([on a NAS](performance.md#on-a-nas-server-side-21-part-2)).
- **On a Linux client (client-side).** sacdfs runs on the client and reads the images from a
  network share, as in the [user guide](user-guide.md). Every read of an image is then a
  network round trip; [over NFS from hard disks](#client-side-images-on-hard-disks-over-nfs),
  what is slow is starting playback, not playing. A cold seek over NFS from a NAS's hard
  disks takes about 22 ms for plain DSD and 37 ms for DST on average, and about 2 ms once the
  seek index knows that part of the disc. The mount can also be re-exported by the kernel NFS
  server ([user guide](user-guide.md#sharing-the-mount-over-nfs)).

sacdfs also runs **locally**, on the Linux machine that holds the images, as in the
[user guide's quick start](user-guide.md#quick-start): a cold seek returns in about 2 ms, a
WAV seek in under 3 ms. How
fast each way is, against the project's targets: [performance.md](performance.md).

This guide covers the first: the container image in this repository (`Containerfile`), how
to run it with Docker, Podman or Compose and on TrueNAS SCALE, and how clients mount its
shares. The clients tested so far, with the settings that worked for each, are in
[clients](clients.md).

## What the container does

```
 host                          container
 /path/to/images  ──(ro)──►  /images ──► sacdfs (FUSE) ──► /srv/sacd ──┬─► smbd         :445  SMB share "sacd"
                                                                      └─► NFS-Ganesha  :2049 NFSv4 export "/sacd"
 volume           ────────►  /cache   (sacdfs's persistent seek index)
```

- The images come in **read-only** from the host. sacdfs mounts them at `/srv/sacd` inside
  the container, and Samba and NFS-Ganesha share that mount, read-only.
- **Why the servers are inside the container:** an appliance NAS's own SMB and NFS services
  share only its own datasets, and a FUSE mount inside an app container is not visible to
  them by default. Serving from inside the container depends on nothing in the host but
  Docker or Podman. The mount can be made visible on the host and to other containers with
  shared mount propagation (it works on TrueNAS SCALE 25.10), which is how players on the
  same host read it without SMB: see [Players on the same host](#players-on-the-same-host-no-smb-hop).
  Sharing it through the NAS's own SMB or NFS service that way was not tested.
- Everything logs to standard output (`docker logs sacdfs`).
- A health check (`/usr/local/bin/healthcheck.sh`) passes while the mount answers and every
  server is running and listening. If sacdfs, smbd or NFS-Ganesha stops, the container stops
  the others and exits, so the runtime's restart policy brings it back.
- **Order of start and stop:**
  - On start, sacdfs mounts first, and only then do Samba and NFS-Ganesha start.
  - On `SIGTERM` (`docker stop`), the servers stop first. The entry point waits until no
    process but sacdfs uses the mount (NFS-Ganesha keeps the export's root open until it
    has exited), then stops sacdfs, which unmounts and writes the seek index cache.
  - *Why:* a server is never left serving an empty directory or a mount that is going
    away, and the unmount isn't refused as busy.
  - A restart never mounts over the old mount (#51): sacdfs is started only once per
    container, each container start has a new mount namespace, and the entry point clears a
    stale mount before starting sacdfs. sacdfs itself also waits for a FUSE mount still on
    its mount point to go before mounting, and detaches a dead sacdfs mount there at once
    (#57).
  - The NFS export has the same fixed filesystem id (`Filesystem_Id = 101.1`) on every
    start, and its file handles carry no device number (`fsid_type = Major64`), because FUSE
    has no stable device number: see [NFS and restarts](#nfs-and-restarts).

## The published image

Each release publishes the image to GitHub's container registry, as one multi-arch tag for **x86_64 and arm64** (Synology ARM models, Raspberry Pi 5, Ampere
hosts; the runtime picks the right one), each built for a generic CPU by the release workflow
after the share test (SMB and NFS) passed on that very image, on a native runner of that
architecture:

```sh
docker pull ghcr.io/aclater/sacdfs:0.3.0       # a release; :latest is the newest
gh attestation verify oci://ghcr.io/aclater/sacdfs:0.3.0 -R aclater/sacdfs
```

The attestation shows which commit and workflow built it. In the commands below, use
`ghcr.io/aclater/sacdfs:<version>` where they say `sacdfs`, or build the image yourself.

## Building the image

```sh
git clone https://github.com/aclater/sacdfs && cd sacdfs
podman build --format docker -t sacdfs .      # or: docker build -t sacdfs .
```

`--format docker` keeps the health check, which the OCI image format has no field for. The
published image is OCI, so for Podman give the check on `podman run` (see the
[Docker and Podman guide](install/docker-podman.md)); Docker reads it from the image. On arm64
the build uses the toolchain's default target and the WAV converter (scarletbook-pcm) picks the
code its CPU supports. On x86_64 the build compiles sacdfs for generic x86-64, whose converter
already uses AVX2 where the CPU has it. On a NAS with an AVX2 CPU (most Intel and AMD CPUs since about 2015, including the
Intel N100 family), `--build-arg RUSTFLAGS="-C target-cpu=x86-64-v3"` builds all of sacdfs
for it ([performance.md](performance.md#converter-throughput)). Don't use that build on an
older CPU: it stops with an illegal-instruction error.

To install the image on a NAS that cannot build, push it to a registry the NAS can pull from,
or move it as a file: `podman save sacdfs | gzip > sacdfs.tar.gz`, then
`docker load < sacdfs.tar.gz` on the NAS.

### Bases, and why

- **Build stage: UBI 10** (`registry.access.redhat.com/ubi10/ubi`), with RHEL 10's Rust
  toolchain from UBI's AppStream repository (Rust 1.92; sacdfs needs 1.85). Red Hat first.
- **Runtime stage: Fedora 44** (`registry.fedoraproject.org/fedora:44`), for Samba,
  NFS-Ganesha and `fusermount3`. What was tried first, and why it doesn't work:
  - *UBI 10:* its repositories carry `fuse3`, but neither Samba nor an NFS server (no
    `samba`, `nfs-utils` or `nfs-ganesha`).
  - *RHEL 10's full repositories:* they have Samba and `nfs-utils`, but need a subscription.
    A public image built from them couldn't be redistributed, and neither CI nor most NAS
    owners can build it.
  - *NFS-Ganesha* is not in RHEL at all.
  - Fedora carries all three; CentOS was not needed.
- sacdfs links only glibc and libgcc_s, so the binary built on UBI 10 (glibc 2.39) runs on
  Fedora's newer glibc.
- The image brings its own `fusermount3` (Fedora 44's); only the FUSE side of the kernel
  is the host's. The release binary was tested on kernels back to 4.18 (RHEL 8's and Fedora
  29's) with their own old `fusermount3` ([design.md](design.md#oldest-distributions-58));
  the container itself was tested on current kernels only.
- Both bases are pinned by digest in `Containerfile`, so a rebuild gets the same base until
  the pin is updated (Dependabot proposes updates).

### The NFS server: NFS-Ganesha, and why not the kernel's

Samba is the only choice for SMB. For NFS there are two servers:

| | Kernel NFS server (`nfsd`) | NFS-Ganesha (user space) |
|---|---|---|
| Needs from the host | the `nfsd` kernel module already loaded (a container cannot load modules) | nothing |
| Privileges | rootful container, `CAP_SYS_ADMIN` to mount `/proc/fs/nfsd`, in practice `--privileged` | `CAP_DAC_READ_SEARCH` only |
| Coexists with the host's own NFS service | kernel state per network namespace; fragile on appliances that manage `nfsd` themselves | fully independent |
| Stops with the container | kernel threads must be shut down explicitly, or they outlive it | an ordinary process |
| Speed | faster in general | ample here: sacdfs's decoding, not the server, sets the pace (about 490 MB/s for the DSF files of a DST stereo disc) |

**NFS-Ganesha is used**, because it asks nothing of the host, needs one capability instead
of a privileged container, and stops with the container. Both servers reopen files by NFS
file handle through `open_by_handle_at(2)` on the FUSE mount, so both rely on sacdfs's
persistent file identity ([#18](https://github.com/aclater/sacdfs/issues/18), see
[below](#nfs-and-restarts)).

## Privileges

| What | Why | When |
|---|---|---|
| `--device /dev/fuse` | sacdfs is a FUSE filesystem | always |
| `--cap-add SYS_ADMIN` | mounting the FUSE filesystem (`fusermount3` calls `mount(2)`) | always |
| `--cap-add DAC_READ_SEARCH` | NFS-Ganesha opens files by NFS file handle (`open_by_handle_at(2)`), which needs it; Docker's default seccomp profile also allows that call only with this capability | with NFS (the default); not needed with `NFS=0` |
| `--security-opt apparmor=unconfined` | Docker's default AppArmor profile forbids `mount(2)` even with `CAP_SYS_ADMIN`. Only on hosts that use AppArmor (Ubuntu, Debian, TrueNAS SCALE); SELinux hosts (Fedora, RHEL) don't need it | AppArmor hosts |

Nothing else: no `--privileged`, no host network, no host paths but the images and the
cache. Ports 445 and 2049 are below 1024, which a container may bind by default
(`CAP_NET_BIND_SERVICE` is in the default set).

**Rootful or rootless.** Docker on a NAS is rootful, and everything works there. Under
rootless Podman, SMB works, but NFS does not: `open_by_handle_at(2)` needs
`CAP_DAC_READ_SEARCH` in the host's user namespace, which a rootless container doesn't have.
Run it rootful (`sudo podman ...`) for NFS, or set `NFS=0`.

## Settings

All settings are environment variables.

| Variable | Default | Meaning |
|---|---|---|
| `SACDFS_SOURCE` | `/images` | where the images are inside the container (mount the host directory here, read-only) |
| `SACDFS_FORMATS` | `dsf,wav` | `--format`: `dsf`, `wav` or both |
| `SACDFS_PAUSES` | (dropped) | `--pauses`: `dropped`, or `kept` to serve each track with its pause (#112) |
| `SACDFS_PCM_RATE` | (176400) | `--pcm-rate`: `88200`, `176400` or `352800` |
| `SACDFS_PCM_CUTOFF` | (40) | `--pcm-cutoff`: `40`, `30` or `wide` |
| `SACDFS_PCM_GAIN` | (0) | `--pcm-gain` in dB, -60 to +24 |
| `SACDFS_FLATTEN` | `0` | `1` for `--flatten` |
| `SACDFS_MC_ALBUM_SUFFIX` | `1` | `0` for `--no-multichannel-album-suffix`: leave the album tag of the multichannel files as the disc has it, without ` (Multichannel)` (see [the client notes](#multichannel-copies-and-music-servers)) |
| `SACDFS_TAGS` | (none) | `--tags`: the tags directory inside the container, for example `/tags` (mount a host directory there, read-only is enough); see the [user guide](user-guide.md#your-own-tags---tags) |
| `SACDFS_SIDECARS` | (`default`) | `--sidecars`: which folder art, scans and playlists are passed through next to the images, for example `default,cue` or `none` ([user guide](user-guide.md#sidecar-files-art-scans-playlists)) |
| `SACDFS_CACHE_MB` | (32, or 8 under a limit up to 512 MiB) | `--cache-mb`: decoded-frame cache in MiB. The default suits a NAS; see [Memory](#memory) |
| `SACDFS_LISTING_MB` | (no bound, or 3/8 of the limit) | `--listing-mb`: memory kept for what listed images hold, in MiB; `0` for no bound. See [Under a memory limit](#under-a-memory-limit) |
| `SACDFS_THREADS` | (one per CPU, at most one per 64 MiB of a limit) | `--threads`: DST decoder threads. On a NAS that also serves other things, fewer threads keep decoding from crowding them out |
| `SACDFS_MAX_OPEN_IMAGES` | (64) | `--max-open-images`: image files kept open between reads |
| `SACDFS_REQUEST_TIMEOUT` | (30) | `--request-timeout` in seconds: a read, lookup or listing that waits longer for the images (a stalled disk or share) fails with an I/O error, and is logged, instead of hanging the SMB or NFS client; `0` waits for good. See [Troubleshooting](#troubleshooting) |
| `SACDFS_MOUNT_TIMEOUT` | `300` | seconds the container waits for sacdfs to mount (it lists every image first, so a large library on a slow disk takes longer) before it exits with an error; a whole number from 1 to 86400. Keep the health check's start period (300 s in the image, the compose file and the commands in this guide) at least as long if you raise it. See [Troubleshooting](#troubleshooting) |
| `SACDFS_INDEX_CACHE` | `/cache` | `--index-cache` directory (keep it on a volume so it survives updates); `off` for `--no-index-cache` |
| `SACDFS_VERBOSE` | `1` | `0` warnings only, `1` (`-v`), `2` (`-vv`) |
| `SMB` | `1` | `0` to turn the SMB server off |
| `NFS` | `1` | `0` to turn the NFS server off (then `CAP_DAC_READ_SEARCH` isn't needed) |
| `SHARE_NAME` | `sacd` | the SMB share name, and the NFS export path (`/sacd`) |
| `SMB_USER` | (none: guest access) | a user name: the share then needs it and `SMB_PASSWORD`, and refuses guests |
| `SMB_PASSWORD` / `SMB_PASSWORD_FILE` | | the password, or a file holding it (for Docker or Podman secrets) |
| `SMB_NAME_MAPPING` | `sfm` | how names with characters Windows forbids are shown; see [below](#file-names-over-smb) |
| `SMB_LOG_LEVEL` | `1` | Samba's `log level` |
| `SMB_EXTRA_GLOBAL` | | one extra line for smb.conf's `[global]` section (for example `server min protocol = SMB3`) |
| `NFS_CLIENTS` | `*` | who may mount over NFS: a comma-separated list of hosts, networks (`192.0.2.0/24`) or `*` |
| `NFS_LOG_LEVEL` | `EVENT` | NFS-Ganesha's log level (`WARN`, `EVENT`, `INFO`, `DEBUG`) |

The shares are always read-only. **Guest access** (the default) lets anyone on the network
read the files; NFS has no passwords (`sec=sys`), so limit it with `NFS_CLIENTS` or a
firewall, or turn it off. Every NFS client is mapped to the `nobody` user (`all_squash`),
which is all a read-only share needs.

### Memory

The default cache of 32 MiB (`SACDFS_CACHE_MB`, `--cache-mb`, since #150) suits a NAS, and is 8 MiB under a memory limit up to 512 MiB ([Under a memory limit](#under-a-memory-limit)). The cache holds
decoded frames so that a client re-reading the same part of a file does not decode it again;
players read ahead, so a small cache costs little. 32 MiB is the smallest size that reads,
seeks and scans within 5 % of a 256 MiB cache ([performance](performance.md#cache-size-150));
at 16, several fast readers of one image are about 10 % slower, and below that WAV straight
reads slow down several times.

Peak resident memory of sacdfs with the bench images (five discs, 186 files), measured on the
build of #150:

| `SACDFS_CACHE_MB` | import replay (Roon's reads: 768 KiB at the start, 36 KiB at the middle, 33 KiB at the end of each file) | Plex-like scan (the first and last 128 KiB of every file) |
|---|---|---|
| `8` | 21 MiB | 23 MiB |
| `32` (default) | 45 MiB | 46 MiB |
| `256` | 108 MiB | 66 MiB |
| `256` with 0.3.0 | 134 MiB | 103 MiB |

Peak resident memory by workload on the six synthetic bench discs (103 files per format, default
cache, a fresh mount, one run each, a 32-thread host), 0.4.0 against 0.4.1-rc.1 (#211):

| Workload | 0.4.0 | 0.4.1-rc.1 |
|---|---|---|
| Plex-like scan, WAV (first and last 128 KiB of each file) | 30 MiB | 32 MiB |
| Import replay, DSF | 47 MiB | 49 MiB |
| Import replay, WAV | 49 MiB | 49 MiB |
| Full read of the 14 stereo files of one DST disc as WAV (2.97 GiB), one reader | 64 MiB | 66 MiB |
| the same, 4 readers | 76 MiB | 85 MiB |
| Full read of the same files as DSF (1.98 GiB), 4 readers | 50 MiB | 54 MiB |

So on this small library a scan that only reads the ends of files stays under 50 MiB, but a scan that reads whole files,
which a media server does when it re-analyses or upgrades its library, does not: more readers at
once hold more frames, and WAV holds more than DSF. Real servers on a NAS showed more: 160 MB for
a Plex upgrade scan that read 18.25 GiB over 801 opened files, 85 MB for a Roon import after an
upgrade and 72 MB after 2.4 days of Roon running. These are for a small library: with the
default cache, one scanning server peaks near 100 MiB on a library of about 400 images, four
scanners at once add about 30 MiB (so about 130 MiB), and every image beyond about 400 adds about
53 KiB (see [Small NAS: sizing](#small-nas-sizing)). The rise from 0.4.0 here is 0 to 9 MiB
(single runs, within their noise), so it is not new in 0.4.1. On the 32-thread host of these
runs the process keeps 70 threads after a listing in both versions (the extra walker threads of
#175 are not among them); the count depends on the host: 23 to 27 on 2 to 4 CPUs
(0.4.1-rc.1, container limits). After 60 s without reads the cache and the decoders are given
back, and on these small libraries memory falls to 9 to 20 MiB.

The cache's memory is its limit (its frames live in their own memory mappings, which never pass
`SACDFS_CACHE_MB`), and the rest of sacdfs takes about 13 MiB here, a little more for each
image listed (its TOC, text and track lists) and for each frame decoded at once. After 60 s
without reads sacdfs gives the cache and its decoders back: about 11 MiB idle on a small
library like the bench discs (see
[design](design.md#memory-the-default-cache-and-the-fixed-overhead-150)). With no memory limit,
what the listing holds is not given back, so the idle size grows with the library: 39 MiB at 10
minutes idle on a 395-image library (0.4.1-rc.1). Limiting open images
(`SACDFS_MAX_OPEN_IMAGES`) makes no measurable difference: an open image costs about 30 KB.

### Under a memory limit

sacdfs reads the limit of its cgroup at startup: a container's `--memory`, or a systemd unit's
`MemoryMax=` or `MemoryHigh=`. It sets four things from it, and the container a fifth. With no
limit, none of this applies.

| What | With a limit |
|---|---|
| Cache of frames (`--cache-mb`) | 8 MiB up to 512 MiB, else 32 |
| Decoder threads (`--threads`) | one per CPU, at most one per 64 MiB of the limit, and at least 2 |
| Memory for what listed images hold (`--listing-mb`) | 3/8 of the limit, at least 8 MiB |
| Memory watch | see below |
| NFS-Ganesha's cache of entries, in the container | what an eighth of the limit holds at about 2 KiB each: 16,384 entries at 256 MiB, against its default of 100,000. From about 1.5 GiB, its default |

An option you set yourself always wins.

**Listings.** A listed image holds its TOC and text, and for each of its files a header, tag and
node: about 56 KiB per image with DSF and WAV shown. Past the budget, the images not used lately
are dropped. sacdfs keeps only their names, about 3.5 KiB per image, and reads the TOC again the
next time anything of the image is asked for. A client sees the same names, inode numbers, sizes
and times, and the same bytes. An image with a file open stays. So a smaller budget makes going
back over the library slower; it doesn't change what's served. See the
[design](design.md#memory-a-budget-for-listings-237) for why.

**At startup**, if the whole library listed in memory wouldn't fit, sacdfs says so in the log,
with the size that would fit:

```text
memory limit 256 MiB: listing all 5000 images in memory would take about 317 MiB, so 96 MiB of listings are kept and images not used lately are read again when next used; going through the whole library again will be slower; to avoid that, raise the limit to about 396 MiB
```

**The memory watch.** sacdfs reads the cgroup's anonymous and shared memory once a second, and
four times a second above half the limit. That read wakes no disk. It counts everything in the
limit, including Samba and NFS, and acts on what it finds:

| In use | What sacdfs does |
|---|---|
| over 75% | keeps listings to a quarter of their budget, until use falls below 75% |
| over 85% | also gives up the cache of frames and the decoders not in use; a reader decodes again |
| over 93% | also refuses new listings and opens with `ENOMEM`, logged as a warning at most once a minute. Files already open go on being read, and what is already listed stays listed |

A client that gets `ENOMEM` can try again; it isn't told that anything is missing, and the error
isn't remembered once memory is back. This is the last step before an out-of-memory kill, which
would end the service for every client.

**The memory watch has a limit.** It frees only what sacdfs holds. Something else in the same limit
that goes on taking whatever is freed can still get something killed: a test process that kept
refilling the limit to 94% was killed first, and in one run of two the servers were too, and the
container stopped.

**What was measured, sacdfs mounted directly.** sacdfs under a memory limit, with its client in the
same limit, listing and stating every file while a paced reader played a 6-channel WAV. Synthetic
libraries of links to real discs, about 25 files per image; single runs on busy machines, so the
times only show scale. Peak is sacdfs's anonymous memory. The 0.4.0 and 0.4.1-rc.2 columns were
measured on another machine than the other two.

| Limit | Images | 0.4.0 | 0.4.1-rc.2 | 0.4.1-rc.3 | With the budget |
|---:|---:|---|---|---|---|
| 256 MiB | 1,600 | 128 MiB | 125 MiB | 110 MiB | 93 MiB |
| 256 MiB | 5,000 | killed after about 91,000 of 123,974 files | killed after about 90,000 | killed | 119 MiB |
| 256 MiB | 20,000 | killed after about 83,000 of 495,874 files | killed after about 80,000 | killed after about 84,000 | 171 MiB |
| 512 MiB | 5,000 | 310 MiB | 314 MiB | 316 MiB | 203 MiB |

The player had no late read in the runs with the budget, and nothing was refused. Starting again and
listing 5,000 images at 256 MiB with the seek index kept between runs worked 3 times out of 3, and
was killed 4 times out of 4 on 0.4.1-rc.2.

Over a library past its budget, a second pass reads TOCs again, so it takes longer: at 5,000 images
and 512 MiB, 3.0 s against 2.8 s on 0.4.1-rc.3. Those TOCs were in the page cache. On a slow disk with
nothing cached a second pass could take about as long as the first; that hasn't been measured. A
library that fits its budget isn't affected: at 1,600 images and 256 MiB, second passes took 0.2 s,
and 0.4 s on 0.4.1-rc.3.

**The container, 256 MiB, default settings.** With a client in user space over SMB and a paced
6-channel WAV reader over SMB, NFS on but not in use, on a busy machine:

| Images | Files listed | Errors | Peak of the whole limit | Time | Player's worst read |
|---:|---:|---:|---:|---:|---:|
| 5,000 | 123,974 | 0 | 146 MiB | about 3 min | 141 ms |
| 20,000 | 495,874 | 0 | 195 MiB | about 11 min | 56 ms |

With the same client, the 0.4.1-rc.3 image at 5,000 images was killed after about 61,000 files.

**Fast readers.** 16 or 64 unpaced readers of 6-channel WAVs during a listing, with the readers' own
memory outside the limit, sacdfs mounted directly at 256 MiB, 3 rounds each. Peak of the whole limit:

| Images | 16 readers | 64 readers |
|---:|---:|---:|
| 5,000 | 146 to 148 MiB | 159 to 162 MiB |
| 20,000 | 193 to 197 MiB | 193 to 197 MiB |

No errors and no kills. Reading a WAV on its request thread (0.4.1-rc.3) made no difference beyond the
spread here, measured at a cache of 16 MiB. The 20,000-image runs passed 75% once, and listings shrank. With the readers' own memory
inside the limit, which took about 150 MiB, new listings failed with `ENOMEM` for a while, and nothing
was killed.

**`--tags`.** With a text tags file for each of 382 discs and 5,000 images at 256 MiB, the tree, numbers,
sizes and times, and a file's bytes read after its image was dropped, were the same as with no bound. With
a 400 KiB cover in every disc's tags as well, listing worked with no errors, at a peak of 218 MiB, but took about 68 s
a pass against about 3 s without tags, on a busy machine: dropped images read their covers again and copy
them into each file's tag. Every file holds a copy of its cover, so an image of 25 files holds about 10 MiB
more, and few images stay listed. Under a limit with `--tags`, sacdfs keeps blocks from 128 KiB up
in mappings of their own, since the heap otherwise kept over 100 MiB of freed covers.

**After a kill.** With `--restart=always` and a client listing without a pause, killing sacdfs in a container at 256 MiB
with 5,000 images restarted it once. It served again about 2 s later, and stayed up through about 70 s more of the same
listing, with no restart loop. The kill was a `kill -9` for the test, not one for lack of memory: at 40 MiB, with the startup warning
saying it could be killed, 18 passes of a client listing the library (no reads) got no kill to test with.

**Lower limits.** With 5,000 images, listing worked at 128 and 96 MiB. At 64 MiB, of three runs one worked,
one stopped short with `ENOMEM` for its listings, and one was killed, as the startup warning says can
happen at that size. Pressure from something else in the limit also works as described: with
another process holding 205 MiB of a 256 MiB limit, new listings failed with `ENOMEM`, sacdfs and the
player carried on, and once that process ended every file listed again.

**Real SMB and NFS clients at once.** Linux kernel clients, SMB 3.1.1 and NFS 4.2 mounted on the container's host, each
listing and stating the whole library at the same time at 256 MiB, with a paced 6-channel WAV reader over NFS. The libraries
were links to 5 discs, about 37 files per image; single runs on a busy machine.

| Images | Files per client | Errors | Peak of the whole limit | sacdfs / NFS-Ganesha / Samba | Player's worst read |
|---:|---:|---:|---:|---|---:|
| 5,000 (2 passes) | 186,000 | 0 | 180 MiB | 122 / 51 / 13 MiB | 63 ms |
| 20,000 | 744,000 | 0 | 193 MiB | 135 / 51 / 13 MiB | 63 ms |

Nothing was killed, the player had no late read, and a file read the same on both clients after its image had been dropped,
through a new open and through an NFS handle kept open.

**What a client sees when a request is refused.** `ENOMEM` reaches a local client as is. Samba and NFS-Ganesha pass it on as a
server error, and the Linux clients report `Remote I/O error` (`EREMOTEIO`), at once and with no retry. With something else holding
the limit above 93%, a listing of an image not yet listed failed that way on both clients, as did a new open over SMB. A new open over NFS still
worked, as NFS-Ganesha kept the file open already. In the run where the servers stayed up, a read of a file open before the shortage went on working
on both clients, and every request worked again once the memory was back. Other clients weren't tested.

**Many fast readers on real clients: a known limit.** Unpaced readers of 6-channel WAVs, half over each kernel client, at
256 MiB, 90 s, 3 rounds. 4 readers peaked at 120 MiB and 8 at 166 MiB, with no errors. With 16, the kernel killed Samba's process for
the client in every round: Samba held over 100 MiB of reads, which sacdfs can't free. Readers got errors, and the container stayed
up. The 0.4.1-rc.3 image at the same 16 readers stopped within seconds, 2 runs of 2. For that many fast readers, raise the limit. Many fast
SMB readers in a user-space client, up to 16, didn't show this.

### Small NAS: sizing

What a small NAS needs for the container, measured on 0.4.1-rc.1 (single runs), before listings had a budget; the container runs of the budget are in [Under a memory limit](#under-a-memory-limit), and CPU limits weren't re-run for it. The container
was limited to 2 or 4 CPUs of Gracemont E-cores (rig A below, about 2.7 GHz under load) and
to the memory below; SMB was over loopback, and the library was on network-attached disks. The
figures are container limits, not whole-box RAM: a NAS's own services need their share, and
whole-box sizes were not tested. The current `main` has #224, which matched or beat these
memory figures in what was re-measured (a real library listed in 33.8 MiB against 38.5; 5,000
images at 274 MiB against 275), but the limits below were not re-run on it.

| | CPUs | Limit for sacdfs plus Samba | Library |
|---|---|---|---|
| Minimum | 2 E-cores (rig A below) | 256 MiB | up to about 1,800 images in memory; larger libraries work, and read TOCs again ([Under a memory limit](#under-a-memory-limit)) |
| Recommended | 4 cores | 512 MiB | up to about 3,600 images in memory, and larger libraries the same way |

Add about 40 MiB if NFS is on (`NFS=1`, the default); under a limit, NFS-Ganesha peaked at 51 MiB with a client going through 20,000 images. To set such a limit,
for example `podman run --cpus 2 --memory 256m ...` (or `docker run`, with the same options).

**Sizing rule, with no bound on listings.** 100 MiB (the peak while scanning, up to about 400 images, with the default
cache of 32), plus 53 KiB for every image beyond those, plus 30 MiB if four scanners run at once.
The rule is an upper estimate: it came out 10 to 35 MiB above the measured sizes at 2,000, 3,500
and 5,000 images. `SACDFS_FORMATS=dsf` cuts the memory of the listing by about a third (89 to 61
MiB at 1,600 images).

**Where a limit breaks.**
- Through 0.4.1, at 256 MiB with the default cache: 2,000 images worked (151 MiB), 3,500 worked
  near the limit (231 MiB), and 5,000 were killed for lack of memory during the listing. An
  out-of-memory kill takes down the whole service (sacdfs, Samba and NFS), and the runtime restarts
  the container. From 0.5.0 listings are kept to a budget: see
  [Under a memory limit](#under-a-memory-limit).
- Floors, with 395 images on 2 E-cores: 128 MiB survived every scan (118 MiB at cache 32, 90 at
  cache 8). 96 MiB played, but a whole-file scan killed it. 64 MiB was killed during playback.
  Do not size a container to these floors.

**Playback.** Two rigs were measured, both on E-cores (Gracemont), and they differ by a constant
factor. Rig A is the E-cores of a mobile Intel Core i9 at about 2.7 GHz under load, with the
container on the same host and SMB over loopback (0.4.1-rc.1, and rc.3 for the scan cells below).
Rig B is the E-cores of a 15 W Intel Core i5 in a NAS, with a client on another host over wired SMB
(0.4.1-rc.2 and rc.3). Neither is an N100; the figures above were first described as N100-class,
which was wrong. Between rc.1 and rc.2 nothing changed on the read or decode path, and the headroom
on rig B is 0.77 to 0.78 times that of rig A in each of the four cells compared (2 and 4 cores, DSF and
WAV), so the difference is the CPU, not the release.

- **Late reads.** On 2 E-cores and 256 MiB, stereo and 6-channel DST, DSF and WAV, with one or two
  players and no other load, had no late reads with a 0.25 s buffer (rig A, rc.1). The second
  player of a pair re-read a position the first test had read, so for that case only the second
  player's reads are a first read.
- **Headroom** (one player reading as fast as it can, default cache, no other load; page cache does
  not change it, because a reader of that speed passes through far more data than the cache
  holds):

  | 6-channel | 2 cores, 256 MiB, rig A | 4 cores, 512 MiB, rig A | 4 cores, 512 MiB, rig B | 2 cores, 256 MiB, rig B |
  |---|---|---|---|---|
  | DSF | 11.8 times real time | 22.0 | 17.1 | 9.1 |
  | WAV | 9.7 | 18.3 | 14.1 | 7.5 |

  Rig A: rc.1, single runs. Rig B: rc.2, single runs.

**Known limit.** On a fresh cache, whole-file WAV scans starve a 6-channel WAV player. "Scan" means
a client that reads whole files, such as a library import that copies or fingerprints them; a scan
that reads only the start, middle and end of each file does not (the player stayed within 0.2 s).
It happens on 2 cores and on 4 cores, with the default cache of 32 MiB
([#230](https://github.com/aclater/sacdfs/issues/230)). Speed relative to real time is the figure to
read; "seconds behind" is capped by the 30 s test length. rc.3, a fresh container per condition, the
scanners never reading the player's track, one 30 s read of a position not read before, medians
and ranges of 3 rounds:

| 6-channel WAV player | Cache | 2 cores, 256 MiB, rig A | 4 cores, 512 MiB, rig A | 4 cores, 512 MiB, rig B |
|---|---|---|---|---|
| 1 whole-file scanner | 32 (default) | 0.47 times real time (range 0.46 to 0.48) | 1.00 (0.96 to 1.00), 3.4 s behind | 0.66 (0.65 to 0.93) |
| | 16 | 0.46 (0.46 to 0.52) | 0.99 (0.95 to 1.00), 2.8 s behind | 0.66 (0.65 to 0.66) |
| | 8 | 1.00 (1.00 to 1.00) | 1.00, 0.0 s behind | 1.00 |
| 4 whole-file scanners | 32 (default) | 0.28 (0.26 to 0.31) | 0.43 (0.39 to 0.45) | 0.38 (0.29 to 0.39) |
| | 16 | 0.36 (0.29 to 0.80) | 0.94 (0.77 to 1.00) | 0.58 (0.56 to 0.59) |
| | 8 | 0.97 (0.95 to 0.98), 1.3 s behind | 1.00, 0.1 s behind | 1.00, 0.1 s behind |

A stereo DSF player with 4 whole-file scanners on 4 cores stayed at 1.01 times real time and at
most 0.3 s behind at every cache size, on both rigs. Rig B's runs had a heavily loaded client
(load average 40 to 135 on the client host), so the default-cache cells there carry more spread;
the cache-8 cells at 1.00 do not depend on it. Out-of-memory kills: none. Rig B on 2 cores was not run on rc.3; on rc.2 at the default cache it
was 0.45 times real time with one scanner and 0.28 with four, and other caches were not tried.

A smaller cache removes the starvation in these runs: `SACDFS_CACHE_MB=8` (`--cache-mb 8`) kept the
player at real time with 1 and with 4 scanners on 2 and on 4 cores, on both rigs. `16` is not
enough on rig B or on 2 cores. It costs speed for everyone else. Measured on rc.3 on rig A, 3 rounds each, fresh container, medians
(range within 2 for scans and 0.4 for headroom): with 4 cores, whole-file WAV scans run at 61 MB/s
at cache 32 and 16 and 23 MB/s at cache 8, and a 6-channel WAV reader reading as fast as it can
reaches 18.9 times real time at 32, 18.8 at 16 and 7.0 at 8. With 2 cores: scans 34, 33 and 14 MB/s;
headroom 9.9, 9.9 and 4.3 times real time. Cache 16 therefore costs nothing measurable, but
starvation is only partly cured by it (table above). On rc.1 the same figures were about 61 and 22
MB/s and 18 and 7 times real time.
Memory at the floors is [#231](https://github.com/aclater/sacdfs/issues/231). The earlier version of
this section gave 0.31 s behind with one scanner and 11.2 s with four on 4 cores at the default
cache; both were taken with a player re-reading a position it had read before, and the 0.31 s was
wrong (see the table).

Not re-run on rc.3: the no-load headroom table above (rc.1 and rc.2 figures), rig B's cost of cache 8, and the 128 MiB and 96 MiB memory floors.

**Idle.** sacdfs used 0.007 % of a CPU and woke 1.5 times a second, and 5 minutes of `strace`
showed no image reads and no metadata calls, so sacdfs itself does not keep a disk awake. Not
tested: a connected client that polls listings, and `--tags`. The cache and decoder memory is
given back after 60 to 75 s of idle; the memory of the listing is not, beyond its budget under a
limit (39 MiB idle at 10 minutes on a 395-image library).

**Mount time.** On an emulated single disk (one request at a time, 15 ms per seek, 120 MB/s)
with nothing cached, mounting a library of 122,676 files took 194 s. That is a model, not a real
disk, but it is longer than the 60 s the container used to wait. The container now waits for
`SACDFS_MOUNT_TIMEOUT` seconds (default 300; [#232](https://github.com/aclater/sacdfs/issues/232)).

## Running it

### Docker or Podman

```sh
docker run -d --name sacdfs --restart unless-stopped \
  --device /dev/fuse --cap-add SYS_ADMIN --cap-add DAC_READ_SEARCH \
  --security-opt apparmor=unconfined \
  -p 445:445 -p 2049:2049 \
  -v /path/to/images:/images:ro \
  -v sacdfs-cache:/cache \
  sacdfs
```

With Podman on an SELinux host, add `:z` to the images volume only if the directory may be
relabelled (`-v /path/to/images:/images:ro,z`); don't relabel a directory that other
services or a network share use. Use `--security-opt label=disable` instead to leave labels
alone.

For a health check under Podman, add `--health-cmd /usr/local/bin/healthcheck.sh
--health-interval 30s --health-timeout 10s --health-start-period 300s --health-retries 3`
(Docker reads it from the image); see the [Docker and Podman guide](install/docker-podman.md).

If the host already serves SMB or NFS itself, ports 445 and 2049 are taken. Either give the
container its own IP address (a `macvlan` network, or publish on an extra address of the
host: `-p 192.0.2.20:445:445 -p 192.0.2.20:2049:2049`, with the host's own services bound to
its main address), or publish other ports (`-p 10445:445 -p 12049:2049`). Linux and macOS
clients can use other ports. Windows 11 build 26200 also mounted a share on another port with `net use` and `/TCPPORT:` (see the [client notes](clients.md#windows-explorer-powershell-and-robocopy); other Windows builds were not tried).

### Compose

`deploy/compose.yaml` builds and runs the image with the settings above:

```sh
SACD_IMAGES=/path/to/images docker compose -f deploy/compose.yaml up -d --build
docker compose -f deploy/compose.yaml logs -f
```

Edit its `environment:` section for the settings. It works with `podman compose` too
(rootful, for NFS).

### TrueNAS SCALE

These steps follow TrueNAS SCALE 24.10 ("Electric Eel") and later, whose apps run on Docker.
They were tested on TrueNAS SCALE 25.10 with the published 0.1.0 image (#21). The app was
installed through the API call behind "Install via YAML", with the YAML below and other ports
(step 2). Both shares served a 154-image library, and the app turned healthy within a minute.
The names of screens and fields may differ between releases.

1. **The image.** Use the published one, `ghcr.io/aclater/sacdfs:<version>`. TrueNAS
   cannot build images: for a build of your own, build it elsewhere (above) and push it to
   a registry the NAS can reach, or load it from a file in a shell on the NAS
   (`docker load`).
2. **Ports.** TrueNAS's own SMB and NFS services hold ports 445 and 2049 on all its
   addresses while they run, so an app can't publish those ports as well. There are three
   ways around it:
   - **Other ports** (the simplest; changes nothing else on the NAS): publish, for example,
     `"10445:445"` and `"12049:2049"` in `ports`. Linux clients then mount with `port=12049`
     (NFS) or `port=10445` (SMB), and macOS with `port=` for NFS or `smb://nas:10445/sacd` for
     SMB ([below](#on-other-ports)). Windows 11 build 26200 mounted one on another port with
     `net use ... /TCPPORT:` (tested on 0.4.1-rc.1, [client notes](clients.md#windows-explorer-powershell-and-robocopy));
     if your Windows build refuses, use one of the other two ways.
   - **An address of its own:** add an alias IP address to the network interface (Network,
     Interfaces), and bind TrueNAS's SMB and NFS services to the main address only (in each
     service's settings, "Bind IP Addresses"). Then publish sacdfs's ports on the alias, as in
     the YAML below. This changes the NAS's network and service settings.
   - **If you don't use TrueNAS's own SMB and NFS services:** leave them stopped and publish
     `"445:445"` and `"2049:2049"`.
   - **For apps on the same NAS only** (Roon, for example; see
     [below](#roon-on-the-same-nas)): give the app's container a fixed address on a Docker
     network of its own. Another app on the NAS reaches sacdfs there on port 445 itself, and
     nothing clashes with TrueNAS's services. LAN clients can't reach that address; for them,
     use one of the ways above.
3. **Install it with YAML.** Apps, Discover Apps, Custom App, then "Install via YAML" (the
   form-based custom app may not offer every setting below). Paste a Compose file like
   this, with your dataset path and, if you use an alias address, the address in `ports`:

   ```yaml
   services:
     sacdfs:
       image: ghcr.io/aclater/sacdfs:0.3.0
       restart: unless-stopped
       devices:
         - /dev/fuse
       cap_add:
         - SYS_ADMIN
         - DAC_READ_SEARCH
       security_opt:
         - apparmor=unconfined
       ports:                       # with an alias address; or "10445:445", "12049:2049"
         - "192.0.2.20:445:445"
         - "192.0.2.20:2049:2049"
       volumes:
         - /mnt/pool/music/sacd:/images:ro
         - /mnt/pool/apps/sacdfs/cache:/cache
       environment:
         SACDFS_FORMATS: dsf,wav
         SACDFS_FLATTEN: "1"
         SMB_USER: music
         SMB_PASSWORD: change-me
         NFS_CLIENTS: 192.0.2.0/24
       healthcheck:
         test: ["CMD", "/usr/local/bin/healthcheck.sh"]
         interval: 30s
         timeout: 10s
         start_period: 300s
   ```

   - The images' dataset is mounted read-only (`:ro`); sacdfs never writes to it.
   - The cache directory is a dataset or directory of its own (create it first); it holds
     the seek index, a few MB for a large library.
   - `SACDFS_CACHE_MB` is left out: the default, 32 MiB (8 under a memory limit up to 512 MiB, [Under a memory limit](#under-a-memory-limit)), keeps a scan that reads only the ends of files under 50 MiB of memory on a small library (the bench discs), and whole-file reads up to about 100 MiB at about 400 images; more images take more ([Memory](#memory), [Small NAS: sizing](#small-nas-sizing)).
   - `SACDFS_THREADS` limits how many cores sacdfs may use for decoding while a client
     reads, if the NAS has other work to do.
4. **Check it.** The app's logs show sacdfs's image count, then `mounted`, then Samba and
   NFS-Ganesha starting (`NFS SERVER INITIALIZED`). The app turns healthy within a minute.
   NFS-Ganesha also logs a few `CRIT` lines about D-Bus, `Cannot register NFS V4 on UDP` and
   a warning about `PR_SET_IO_FLUSHER`. They are harmless: the container has no D-Bus (it is
   used only for Ganesha's admin interface), NFS 4 runs over TCP, and the flusher setting
   matters only to a server that is written to.
   - `NFS_CLIENTS` with the clients' own addresses works with published ports: Docker keeps
     the client's address.
   - A restart (stop, start) takes about 5 s, and the shares answer within about 7 s.

The same YAML, without the TrueNAS paths, works on other NAS systems that run Docker
(Unraid, Synology Container Manager, OpenMediaVault), as far as they allow `/dev/fuse` and
the two capabilities.

#### Roon on the same NAS

Tested with Roon Server's official image (`ghcr.io/roonlabs/roonserver`) as a TrueNAS app with
host networking, on the same NAS as sacdfs.

- **Why a container-network address:** Roon's "Add network share" has no port field, so it
  needs SMB on port 445, which TrueNAS's own SMB service already holds. Give sacdfs a fixed
  address on a Docker network of its own instead. Roon, on the host network, reaches it there
  on 445:

  ```yaml
  services:
    sacdfs:
      # ... as above, without the 445 port mapping ...
      networks:
        sacd:
          ipv4_address: 172.29.42.10
      environment:
        SACDFS_FORMATS: dsf        # Roon plays DSD and converts to PCM itself
        SACDFS_FLATTEN: "1"
        SMB_USER: music
        SMB_PASSWORD: change-me
  networks:
    sacd:
      ipam:
        config:
          - subnet: 172.29.42.0/24
  ```

  Pick a subnet that nothing else on the NAS uses. In Roon, add the network share
  `\\172.29.42.10\sacd` with `SMB_USER` and its password.
- **Roon's container needs the right to mount SMB.** Roon on Linux mounts a network share with
  the kernel's CIFS client inside its own container. Without the right to mount, it fails with
  `could not mount ...; tried SMB versions 3, 2.1, 2.0, 1.0` and `Unable to apply new
  capability set`.
  - Fix it by adding `cap_add: [SYS_ADMIN, DAC_READ_SEARCH]` and
    `security_opt: [apparmor=unconfined]` to the **Roon** app. Roon then mounts the share and
    logs `[roon/cifs] Connected`.
  - This gives Roon's container the same rights as sacdfs's. That's a trade-off: it is
    Roon's own way of reading network shares.
- **Show DSF only.** With both formats, every track appears twice in Roon. Roon plays DSD
  natively and converts it to PCM where an endpoint needs that.
- **Discs you have also extracted.** If the same disc is also in your library as extracted
  files, Roon merges the two into one album and plays whichever copy it makes primary,
  hiding the other. In the owner's test it chose sacdfs's copy for some albums and the
  library's for others. So beside existing extracts you aren't always playing from sacdfs.
  Removing the extracts (the space sacdfs saves) removes the ambiguity.
- **Folder art** works: `folder.jpg`, `cover.jpg` and the other sidecar files next to the
  images are on the share, byte for byte, so Roon shows your own art instead of falling back
  to its online art (#79). With `SACDFS_FLATTEN: "1"` the art of a disc's folders sits next
  to its `Stereo` and `Multichannel` folders. `SACDFS_SIDECARS` changes which files pass.
  The cover images are also inside each `Stereo` and `Multichannel` folder, so servers that
  look only beside the audio find them (#95); a cover that is not named `cover.*` is also shown as
  `cover.jpg` (or `.png`, `.webp`) there, which MPD needs (#148). Images, covers and playlists added to or removed
  from the share later show up within a few seconds, with no restart (#101).
- **Playlists** work: an `.m3u` or `.m3u8` file next to the images is on the share, so Roon,
  which imports playlist entries only from inside the same storage location, imports it
  (#75). Write its entries as relative paths inside the share, as sacdfs names its files
  (`Album/Stereo/01 - Title.dsf` with `--flatten`, for a playlist in the artist's folder);
  sacdfs never rewrites them.
- **Roon's background audio analysis** reads every track in full, so sacdfs decodes each one
  as Roon adds it. On a NAS-class CPU, multichannel DST areas decode at about 40× real time.
  Expect a burst of CPU load after adding a large library, or set the analysis speed lower in
  Roon's settings.

### Players on the same host (no SMB hop)

A player that runs on the same host as sacdfs's container (Jellyfin, Navidrome, Lyrion, MPD as
containers, or as TrueNAS apps) can read sacdfs's mount directly, with no SMB or NFS in between
([#186](https://github.com/aclater/sacdfs/issues/186)). The mount is shared through a directory on
the host:

```
 host directory /mnt/pool/apps/sacdfs-mnt
   sacdfs container:  bound over /srv, rshared   -> sacdfs mounts /srv/sacd, which appears on the host
   player container:  bound at /music, ro,rslave -> the player reads /music/sacd
```

- **sacdfs's container** binds an empty host directory over `/srv` with `rshared` propagation.
  sacdfs mounts at `/srv/sacd` as always, and the mount appears in the host directory as `sacd`.
  From 0.5.0 the container makes `sacd` there itself. With 0.4.1 and earlier images, make it first
  (`mkdir /mnt/pool/apps/sacdfs-mnt/sacd`), or the container stops with an error.
- **The player** binds the **same directory** (the parent of `sacd`), read-only, with `rslave`
  propagation, and its library is `/music/sacd`. Each new mount of sacdfs (after a restart, an
  update, a crash) then shows up in the player by itself.
- **Don't bind `sacd` itself, nor the directory without `rslave`.** Either way the player keeps the
  mount it saw when it started. When sacdfs restarts, that mount is dead in the player
  (`Transport endpoint is not connected`, or files not found) until the player is restarted. It also
  holds the old sacdfs up: its stop then waits for the runtime's stop timeout (10 s), so every
  restart of sacdfs takes that long. Tested with a container on TrueNAS bound like a catalog app's
  host path (Docker's default propagation) and on Podman.
- **SMB and NFS still work** from the same container, for other clients; turn them off with
  `SMB=0` and `NFS=0` if only same-host players read it.
- **Permissions.** The container's sacdfs runs with `--allow-other`, and every file is
  `r--r--r--` and every folder `r-xr-xr-x`, so a player running as any user can read them: tested
  as root, uid 1000 and TrueNAS's apps user (568).
- **Rootful only.** The host directory must be on a mount with shared propagation (`findmnt -o
  TARGET,PROPAGATION /home` shows `shared` on Fedora; a TrueNAS dataset under `/mnt` worked).
  Under rootless Podman the mount reached neither the host nor another rootless container of the
  same user, so this needs rootful Docker or Podman (`sudo podman`), as on a NAS.

With Docker or Podman (on SELinux hosts add `--security-opt label=disable` to both). These options
were run with `sudo podman run` on Fedora 45, with Jellyfin as the player; Docker was run through the
Compose file below, on TrueNAS:

```sh
mkdir -p /srv/sacdfs-mnt
docker run -d --name sacdfs --restart unless-stopped \
  --device /dev/fuse --cap-add SYS_ADMIN --cap-add DAC_READ_SEARCH \
  --security-opt apparmor=unconfined \
  -e SACDFS_FORMATS=dsf -e SACDFS_FLATTEN=1 \
  -v /path/to/images:/images:ro -v sacdfs-cache:/cache \
  -v /srv/sacdfs-mnt:/srv:rshared \
  sacdfs
docker run -d --name jellyfin ... -v /srv/sacdfs-mnt:/music:ro,rslave jellyfin/jellyfin
# library folder in the player: /music/sacd
```

**Start sacdfs before the player.** A player started first sees an empty `sacd` folder (or none,
before sacdfs's first start), and some players act on that (below). In one Compose file, make the
player wait for sacdfs's health check:

```yaml
services:
  sacdfs:
    image: ghcr.io/aclater/sacdfs:0.4.1
    # 0.4.1 and earlier only (0.5.0 makes /srv/sacd itself); or make sacd in the host directory first
    entrypoint: ["bash", "-c", "mkdir -p /srv/sacd 2>/dev/null; exec /usr/local/bin/entrypoint.sh"]
    restart: unless-stopped
    devices:
      - /dev/fuse
    cap_add:
      - SYS_ADMIN
      - DAC_READ_SEARCH
    security_opt:
      - apparmor=unconfined
    volumes:
      - /mnt/pool/music:/images:ro
      - sacdfs-cache:/cache
      - /mnt/pool/apps/sacdfs-mnt:/srv:rshared
    environment:
      SACDFS_FORMATS: dsf
      SACDFS_FLATTEN: "1"
    healthcheck:
      test: ["CMD", "/usr/local/bin/healthcheck.sh"]
      interval: 30s
      timeout: 10s
      start_period: 300s
      start_interval: 2s
  navidrome:
    image: deluan/navidrome:latest
    restart: unless-stopped
    depends_on:
      sacdfs:
        condition: service_healthy
    ports:
      - "4533:4533"
    volumes:
      - /mnt/pool/apps/sacdfs-mnt:/music:ro,rslave
      - nd-data:/data
    environment:
      ND_MUSICFOLDER: /music/sacd
      ND_DATAFOLDER: /data
      ND_SCANSCHEDULE: "0"         # no periodic scans: see below
volumes:
  sacdfs-cache:
  nd-data:
```

`start_interval` makes the health check run every 2 s while sacdfs starts, so the player starts a few
seconds after the mount instead of after the first 30 s interval. This file, with a small test library
(8 images, 230 DSF files), was installed on TrueNAS SCALE
25.10.7 as one custom app (the API call behind "Install via YAML") with the 0.4.1 image: sacdfs
mounted, Navidrome was ready 4 s later, and its first scan found all 230 files. After the app was
stopped and started, its first scan found all 230 again. On TrueNAS, changing sacdfs's image in that app recreated both containers.

**Two separate TrueNAS apps** work the same way (tested: a sacdfs app and a player app binding the
same dataset), but TrueNAS doesn't order separate apps at boot. TrueNAS's catalog apps (Plex,
Jellyfin, Navidrome from Discover Apps) have no propagation setting for host paths (read from their
catalog schemas on 25.10.7), so they bind as Docker's default does: they need a restart after each
restart of sacdfs, as above. Install the player as a custom app with `ro,rslave` instead. Put the
host directory in a dataset of its own (for example `pool/apps/sacdfs-mnt`), not one that TrueNAS
shares. Roon and Plex were not tested this way.

**When sacdfs restarts.** What a player bound with `ro,rslave` saw, on TrueNAS SCALE 25.10.7
(Docker, the 0.4.1 image, and 0.4.0 for the downgrade) and on Fedora 45 (rootful Podman 6.1, this
release's entry point):

| What happened to sacdfs | What the player saw | Without the tree | Player restart |
|---|---|---|---|
| Restart, recreate, or a new image (`podman restart`; TrueNAS redeploy, image change 0.4.1 to 0.4.0 and back) | an empty `sacd` folder, then the tree | 0.3 s (Podman); 2.2 to 2.5 s (TrueNAS) | not needed |
| Stopped, then started (TrueNAS app stop and start) | an empty folder while stopped | while stopped (7 s) | not needed |
| Killed (`SIGKILL` to sacdfs; the restart policy started the container again) | `Transport endpoint is not connected`, then the tree; sacdfs detached the dead mount at start | 0.4 to 2 s (Podman, 3 rounds); 2.7 s (TrueNAS) | not needed |
| Restart while a file is open (a stream playing) | the open file reads on until the runtime's stop timeout (10 s) kills sacdfs, then fails with an I/O error; the folder is empty meanwhile | 10.3 s (Podman); 12.2 s (TrueNAS) | not needed, but the track stops and must be started again |

So the tree comes back by itself every time, but an open file doesn't survive a restart: over the
direct mount, an update while something plays cuts that track, where the
[Linux SMB and NFS clients](#smb-and-restarts) read on. A Navidrome stream (raw DSF) was cut that way,
and a stream started after the restart was byte-identical to one from before. If the container is
removed by a kill and not started again, a dead mount stays in the host directory until sacdfs
starts there again, or `umount -l` removes it.

**What players did while sacdfs was away:**

- **Navidrome 0.64.2** (TrueNAS): a scan while the folder was empty removed every track and album
  from its lists. The next scan with sacdfs back brought all 230 back with the same ids, and a star
  and a play count set before were kept. A scan while the mount was dead failed (`transport endpoint is
  not connected`) and left the library as it was. Keep periodic scans off, or rare, and start
  Navidrome after sacdfs.
- **Jellyfin 12.4** (rootful Podman, as uid 1000): a library scan while the folder was empty kept
  all 93 tracks and 7 albums, and so did scans after a restart. Jellyfin refuses to add a library
  folder that doesn't exist (`400 Bad Request`); started before sacdfs's first start, it needs `sacd`
  made in the host directory first. Started first with an empty `sacd`, it found all 93 tracks once
  sacdfs ran and the library was scanned again.

**Direct mount or SMB on the same NAS: what it costs.** The direct mount imports faster, and the
NAS does far less work for it. Navidrome 0.64.2 imported a library and a reader read whole files, once
through the direct mount and once through sacdfs's own SMB share, mounted by the Linux kernel client
(`ro,guest,vers=3.1.1`, defaults otherwise) in a container on the same NAS. NAS: TrueNAS SCALE
25.10.7, 10 cores, hard-disk pool. Image: 0.4.1, `SACDFS_FLATTEN=1`, DSF and WAV. Medians of 3 rounds,
alternating direct and SMB, with the range in brackets.

| Library, 416 images (9,822 DSF and WAV files, 442 albums) | Direct mount | SMB |
|---|---|---|
| Full import (Navidrome scan of an empty database) | **45.1 s** (44.3 to 45.1) | 61.8 s (61.6 to 62.8) |
| Data sacdfs served for it | **5.4 MB** | 5,802 MB (5,415 to 5,802) |
| Read by sacdfs from the images for it | **5.6 MB** | 3,917 MB |
| CPU time: sacdfs / Samba / Navidrome | **3.2 / 0 / 83 s** | 223 / 27 / 95 s |
| First byte of a stereo DSF track (open and first 64 KiB; DSD and DST discs) | **1.6 ms** (0.7 to 2.0) | 25.7 ms (24.8 to 27.8) |
| Whole stereo DSF track | **85 MB/s** (80 to 91) | 54 MB/s (53 to 56) |
| First byte of a 6-channel WAV track | **0.5 ms** (0.3 to 0.5) | 12.1 ms (8.0 to 15.9) |
| Whole 6-channel WAV track | **126 MB/s** (121 to 128) | 102 MB/s (96 to 103) |
| sacdfs CPU time per GB read (the WAV's decoding dominates) | **84 s** | 103 s, and Samba 1.4 s |

| Test set, 18 images (460 files, 20 albums) | Direct mount | SMB |
|---|---|---|
| Full import | **2.29 s** (2.29 to 2.29) | 3.32 s (3.31 to 3.57) |
| Data sacdfs served for it / CPU time of sacdfs and Samba | **0.2 MB / 0.15 s** | 271 MB / 11.0 s and 1.3 s |
| First byte, stereo DSF from a plain DSD disc / 6-channel WAV from a DST disc | **1.3 / 0.8 ms** | 12.6 / 7.5 ms |
| Whole file, the same | **241 / 119 MB/s** | 200 / 98 MB/s |

- **Why SMB costs so much more on import.** A player reads only a file's header and tags to import
  it, a few hundred bytes. Through the direct mount sacdfs serves just that. The kernel SMB client
  reads ahead in large blocks, so for each file sacdfs reads and serves about 600 KB, and decodes it
  for the WAV and DST files: 1,000 times the data and 70 times sacdfs's CPU time for the import.
  Smaller SMB read sizes may lower that; not tested.
- **Playback.** Both routes read faster than any track plays: a stereo DSF track at 64 fs plays at
  about 0.7 MB/s, a 6-channel WAV (24-bit, 176.4 kHz, the default) at about 3.2 MB/s. Through the direct mount a
  file opens and starts 10 to 25 times sooner, which shows as a quicker start and seek in players.
- **How it was measured.** Every run used a new app: a new sacdfs container with an empty index
  cache, a new Navidrome with an empty database, and (SMB) a new kernel mount, so no page cache of
  sacdfs's files carried over. On the library, measurements began once sacdfs was idle after
  mounting (it works in the background for up to about 75 s after mounting a large library); on the
  test set, 10 s after. Each run read different tracks, never
  read before in the test. The NAS's ZFS cache was not dropped: a warm-up import before the rounds put
  the images' headers in it for every run alike, and the tracks read came from the disks. CPU time and
  bytes are from `/proc` of the sacdfs, smbd and Navidrome processes; the kernel SMB client's own CPU
  time is not counted. The NAS ran its other apps, idle (Roon and Plex read nothing during the runs). Load
  before each library run was 1.3 to 2.6, with 90 s between runs; the test set's runs ran back to
  back (load up to 5.6, mostly from the run before).
- **Not measured:** Plex, Roon and Jellyfin this way; playback through a player's own streaming (the
  reads here are file reads); 0.5.0's change to SMB listings (#205), which these 0.4.1 numbers
  predate. Over SMB from another machine the LAN adds its own round trip on top of the SMB figures.

### Multichannel copies and music servers

Navidrome, Lyrion (LMS), MPD and MinimServer all showed a disc's Stereo and Multichannel
copies as one album: merged, with each track up to four times, or as two albums with the same
name. So sacdfs adds ` (Multichannel)` to the album tag of the multichannel files by default
(DSF and WAV; stereo files are unchanged), and the servers list two albums. Set
`SACDFS_MC_ALBUM_SUFFIX: "0"` to keep the disc's album name; then rescan, since the files'
modification times do not move when you switch. Changing this release's tags moved the files'
mtimes and NFS generations once (output versions DSF 3, WAV 4), so servers rescan them.

**Plex: turn on "Prefer local metadata".** Plex keeps the two copies as two albums, since they
are in different folders, but its default music agent (Plex Music) replaces each album's title
with the title of the release it matches online. Both copies match the same release, so they
show as two albums with the same name, and the ` (Multichannel)` suffix is not seen. In a
Plex 1.43.5 test of 10 discs (6 with a multichannel area, #138), the default library showed
each of the 6 titles twice. With "Prefer local metadata" on (library settings, Advanced; the
library pref `respectTags`), or with the "Plex Personal Media" agent, all 6 showed as "Album"
and "Album (Multichannel)", with the right tracks in each, and the Plex Music agent still
fetches the art and reviews. For an existing library, turn the setting on and run "Refresh
All Metadata" on the library: the titles change without a rescan. sacdfs needs no other
setting for this; the stereo and multichannel files already have different album tags.

## Mounting the shares on clients

`nas` is the address or name of the container (or of its alias address). The SMB share is
`sacd` and the NFS export `/sacd` (`SHARE_NAME`).

### Linux

NFS (package `nfs-utils` or `nfs-common`):

```sh
sudo mkdir -p /mnt/sacd
sudo mount -t nfs4 -o ro,vers=4.2 nas:/sacd /mnt/sacd
# /etc/fstab:
# nas:/sacd  /mnt/sacd  nfs4  ro,vers=4.2,_netdev,nofail  0 0
```

SMB (package `cifs-utils`):

```sh
sudo mount -t cifs //nas/sacd /mnt/sacd -o ro,guest,vers=3.1.1
# with SMB_USER: keep the password in a file readable only by root
#   /root/.sacd-smb:  username=music
#                     password=...
sudo mount -t cifs //nas/sacd /mnt/sacd -o ro,credentials=/root/.sacd-smb,vers=3.1.1,uid=$(id -u)
```

File managers open `smb://nas/sacd` directly.

#### On other ports

If the container publishes other ports (TrueNAS, step 2), give them to the client:

```sh
sudo mount -t nfs4 -o ro,vers=4.2,port=12049 nas:/sacd /mnt/sacd
sudo mount -t cifs //nas/sacd /mnt/sacd -o ro,credentials=/root/.sacd-smb,vers=3.1.1,port=10445
smbclient //nas/sacd -p 10445 -U music        # or smb://nas:10445/sacd in a file manager
```

On macOS, NFS takes `port=12049` in its `-o` options and SMB takes `smb://nas:10445/sacd`.

### Media servers in containers on a client host

If sacdfs runs on the host and a container (Jellyfin, Plex, MPD) reads its mount, do not
bind-mount the mount point itself, nor the parent without `rslave`: the container keeps its own
copy of the mount. If sacdfs crashes and is started again, every read in that container fails
with "Socket not connected"; if it is stopped cleanly, the old sacdfs process keeps running to
serve that container, which never sees the new one. Bind the parent directory with `rslave`
propagation, so the new mount shows up inside the container (tested with rootless Podman on
0.3.0-rc.1, see [clients](clients.md#general-recommendations)):

```sh
# sacdfs mounted at /srv/sacd/music; bind /srv/sacd, not /srv/sacd/music
podman run -v /srv/sacd:/media:ro,rslave ...
```

In Compose: `- /srv/sacd:/media:ro,rslave`. With sacdfs in its own container on the same host,
see [Players on the same host](#players-on-the-same-host-no-smb-hop).

### macOS

- **SMB:** Finder, Go, Connect to Server (⌘K), `smb://nas/sacd`; "Connect as Guest", or the
  `SMB_USER` name and password.
- **NFS** (macOS speaks NFS 4.0): `sudo mount -t nfs -o vers=4,ro,resvport nas:/sacd /Volumes/sacd`
  after `sudo mkdir /Volumes/sacd`; or Finder's Connect to Server with `nfs://nas/sacd`.
- **After a restart of the container** an SMB mount on macOS drops and has to be mounted again: see
  [SMB and restarts](#smb-and-restarts).

### Windows

Explorer: `\\nas\sacd`, or map it to a drive letter: `net use S: \\nas\sacd /persistent:yes`
(add `/user:music *` to be asked for the `SMB_USER` password).

Current Windows versions refuse guest access to SMB shares (Windows 11 24H2 and later, and
the Enterprise and Education editions of Windows 10 and 11). Set `SMB_USER` and
`SMB_PASSWORD` for Windows clients rather than relaxing that policy.

### Players that scan the library (MPD, moOde, Volumio, Jellyfin, Plex)

Run the container with `SACDFS_FORMATS=dsf` (`--format dsf`) for a library that MPD scans. MPD
plays DSF natively, as DoP or native DSD depending on the output. MPD's first database scan
(and any rebuild with a cold cache) opens every file through FFmpeg, which probes about 5 MB
of each WAV file. Each WAV file must then be converted from the SACD image for those 5 MB,
although its tags and duration are in the first bytes. In a test of 93 tracks on 0.3.0-rc.1
the first scan took 5.6 s with `dsf,wav` and 0.55 s with `dsf`, about 10 times as long, and
sacdfs used 19 CPU-seconds against 1.6. Incremental updates are unaffected (0 opens). If some other client needs WAV files, run
a second container (or mount) with `dsf,wav` for it.

Jellyfin behaves the same way: in its first scan on 0.3.0-rc.1 sacdfs served 17.9 MiB for 93
DSF files and 648.5 MiB with `dsf,wav`, because its ffprobe reads about 7 MB of each WAV file
(sacdfs used 1 CPU-second against 24 to 26). Use `dsf` unless it needs WAV files. Jellyfin's
LUFS normalization reads every file about twice (39.8 GiB for 22.0 GiB of DSF files in the
same test), and Plex's nightly deep analysis reads every file once (on 0.1.1): turn them off,
or use a 16 MiB `--cache-mb` as on a NAS ([clients](clients.md#jellyfin)).

### File names over SMB

Track titles may contain characters Windows doesn't allow in file names: `" * : < > ? \ |`.
Over NFS the names are exactly sacdfs's. Over SMB, `SMB_NAME_MAPPING` chooses how they look:

- `sfm` (the default): each such character is sent as the private-use character that macOS
  and the Linux kernel client (its default `mapposix` option) turn back into the real one,
  so they show the real name. Windows shows a placeholder glyph instead of the character,
  and opens the file normally.
- `fullwidth`: each is shown as its full-width look-alike (`：` for `:`), which reads well
  everywhere. But a title that already holds one of those look-alikes, as Japanese titles
  often do, then cannot be opened, so it is not the default.
- `none`: Samba's own fallback, a short mangled name (`0JL0SQ~I.DSF`).

Clients built on Samba's libsmbclient (`smbclient`, and many players and file managers) don't
map them back. With `sfm` they show the private-use character, as Windows does, and must ask
for a name in that same form: a name typed with the real `:` is not found.

### Read-only and free space

The share can't be written. What clients are told about it (#217), measured on the Linux kernel
client and gvfs 1.62 with `smbclient` and Samba 4.24:

| What | Over SMB | Over NFS |
|---|---|---|
| A file or folder | the read-only attribute (`R`), from its mode: `-r-xr-xr-x` on the kernel client, `access::can-write` FALSE for files in gvfs | no write bits, `-r--r--r--` |
| The volume | `FILE_READ_ONLY_VOLUME`, because the FUSE mount is read-only | read-only export |
| Size, used, free | size and used are the total of the image files found at mount, rounded to a block size; 0 free | the same |

Limits of the clients, which the server can't change:

- gvfs's `filesystem::readonly` stays FALSE: libsmbclient never sets the flag it comes from. gvfs reports
  `access::can-write` for regular files only, so a file manager may still offer to paste into a folder;
  the write is refused.
- `df -i` over SMB shows 0 inodes: the kernel client reports 0 for SMB, with any server.
- `df` shows 100 % used, as for an ISO or any read-only medium: there is no space to write to.

A file copied from the kernel client with plain `cp` keeps the missing write bit (`chmod u+w` it to
edit the copy). **Untested** on Windows and macOS, including how each shows the attribute and whether
copies keep it, and with Roon and Plex, whichever platform they run on. To go back to no
attribute: `SMB_EXTRA_GLOBAL=map readonly = no`.

### What a listing costs over SMB

Samba serves the share from the FUSE mount, so every SMB request becomes one or more requests to
sacdfs. Up to 0.4.1 a listing cost about three of them per file or folder, most of them the same
question asked again (#205). Counted with the kernel's `fuse:fuse_request_send` tracepoint, on 95 images
(284 folders, 2,412 entries), the container's Samba 4.24.7, and the Linux kernel client (SMB 3.1.1,
Fedora 45, kernel 7.2) over loopback:

| Listing | 0.4.1 | Unreleased |
|---|---|---|
| `ls -lR`, first | 6,897 (2.86 per entry) | 1,765 (0.73) |
| `ls -lR`, again after 30 s | 6,469 (2.68) | 1,555 (0.64) |
| `find` | 2,515 (1.04) | 1,425 (0.59) |
| `smbclient`, `recurse ON; ls` | 3,330 (1.38) | 1,542 (0.64) |
| `stat -f` (free space), 200 times | 200 | 0 |

- **Access checks:** gone (2,713 per `ls -lR`). Samba changes directory for nearly every request, and each
  one was an `ACCESS` request. The kernel now checks the modes itself.
- **A name per file:** gone (2,412). Each entry's attributes come with the folder's listing, and a
  track's name is kept for a day instead of 2 s, so a listing repeated later doesn't ask again.
- **Free space:** the container's Samba asks sacdfs at most once a minute (`dfree cache time = 60`).
- **What's left is per folder:** an open and a release, two reads of the listing, a name lookup for a folder
  that is checked for changes (every 2 s), and one `STATFS`. Samba makes that `STATFS` when it reopens a
  folder's path handle; no Samba setting was found that avoids it.
- **Time:** on an idle machine, a local `ls -lR` of the same mount (the kernel, no Samba; 7 alternating
  rounds, median) took 0.32 s instead of 0.40 s the first time, and 0.07 s instead of 0.12 s when repeated
  after 3 s. Times through Samba were taken on a loaded machine only, so none is given here.
- **The client's own requests don't change:** 3.5 SMB2 requests per entry for `ls -l` on the kernel client,
  on either version. What the client asks is up to the client.

**If you run your own Samba** on a sacdfs mount instead of the container, put the container's settings in
`smb.conf`'s `[global]` section, as the container does:

```ini
ea support = no
store dos attributes = no
map archive = no
kernel change notify = no
dfree cache time = 60
```

Measured on the same tree with the unreleased sacdfs: without these five, `ls -lR` costs 7,195 requests
(2.98 per entry) instead of 1,765, mostly two `LISTXATTR` per entry, and `stat -f` reaches sacdfs every
time. They were measured as a group, not one by one. Adding `vfs objects = streams_xattr` with them changed
nothing (1,743). Measured on Linux clients only: the macOS client that showed the highest counts in #205,
Windows, and the scans of Roon and Plex have not been run with these changes.

## Client-side: images on hard disks over NFS

This guide is about running sacdfs on the NAS. If you run it on a Linux client instead, over
an NFS share of images on hard disks, these are the measured findings (#71,
[performance](performance.md#client-side-where-the-slow-reads-come-from-71)):

- **Once a player is playing, its reads are fast:** under 0.5 ms each, with four players and
  a tagger at once, in every setting tried. sacdfs decodes 1 to 2 s ahead of each player and
  the NFS client reads the image ahead of that.
- **Starting playback is what the disks slow down.** A cold seek reads the image several
  times, one disk seek each. When four players start at the same instant, their seeks queue
  on the disks, and a player's 2 s buffer is full only after 0.27 to 0.31 s, single reads
  taking up to 116 to 156 ms. Players started 2 s apart took 87 to 126 ms to fill their
  buffers. Players buffer more than that, so it delays the start of playback, not the music
  once it plays.
- **Keep the defaults.** None of these improved the start-up, and some made it worse:
  - a larger NFS client read-ahead (`read_ahead_kb` of the mount: 1 MiB made the worst read
    up to 313 ms);
  - `rsize` below the default 1 MiB, and `nconnect=4`;
  - `--threads 4` (it made every start-up read slower), and `--cache-mb 1024`.
- **What does help:**
  - keep the seek index (the default; `--no-index-cache` turns it off): a seek that the
    index knows reads the image once instead of 3 to 5 times;
  - more disks working at once, or a NAS cache that holds the images;
  - best of all, run sacdfs on the NAS ([above](#deploying-sacdfs-on-a-nas)), where sacdfs's
    reads of the image don't cross the network.

## SMB and restarts

What clients did when the container restarted (about 8 s):

| Client | What happened | Run on |
|---|---|---|
| Linux kernel client, SMB 3.1.1 | an open file reads on, `stat` and `ls` work, without a remount | 0.4.1-rc.1, by `restart` and by recreating the container |
| Linux gvfs 1.62 | the next listing works, without a remount | same |
| Windows 11 build 26200 | a handle open during the restart fails; the mapped drive then works, with sizes and times unchanged | 0.4.1-rc.1, by restart |
| Plex on a kernel CIFS mount | recovered by itself | 0.1.1, a restart mid-test ([clients](clients.md#plex)) |
| macOS, `mount_smbfs`, in VMs on Apple hardware | **the mount drops and doesn't come back**, with no hang. Mounting it again works | 15.7 on 0.4.0 and 0.4.1-rc.1; 27.2 on 0.4.1-rc.1; restart of about 8 s |

So on macOS, mount the share again after an upgrade or restart. Whether the server can change this is
still open ([#204](https://github.com/aclater/sacdfs/issues/204)). What's known about the shipped
Samba configuration:

- **Durable handles can't survive a restart, under any setting here.** Samba grants them (they're on by
  default), and a raw SMB2 client reconnected one after dropping its connection. After a restart it gets
  "not found", because Samba empties its table of open files when it starts afresh, even with the file
  kept on disk. The server's log then shows `smb2srv_open_recreate_fn: ... NT_STATUS_NOT_FOUND`: this is
  expected. `durable handles = no` and `kernel oplocks = yes` only stop them being granted, and
  `posix locking = no` changes nothing. The Linux kernel client recovered the same way with
  `durable handles = no` or `smb2 leases = no`.
- **The server's identity can change.** Samba's SMB server GUID is made from its NetBIOS name, which is
  the container's host name by default. It stays the same on a restart and changes when the
  container is recreated. Linux clients don't mind either way. `SMB_EXTRA_GLOBAL=netbios name = sacdfs`
  keeps it the same on recreate, and Linux clients recovered with that too.

None of these settings has been run on macOS, so none is recommended for it.

## NFS and restarts

An NFS client keeps its file handles across a restart of the container. So after a NAS reboot
or an update, NFS clients go on reading the files they had open, and `stat` and `ls` of the
same paths work, with the same inode, modification time and size, without a remount. SMB clients differ, and macOS needs a remount: see
[SMB and restarts](#smb-and-restarts).

Run on the Linux kernel client with NFS 4.0, 4.1 and 4.2 under rootful Podman 6.1, and 4.2
under Docker 29, for `restart`, for stop and start, and for removing and recreating the
container (Podman) with the same image volume. The same in CI on every pull request, with the
versions the runner has: see [Testing](#testing).

Two things do give "stale file handle":

- **An image whose file changed** (size or modification time) gets new file handles for its
  files, so a client holding an old one gets that error instead of other data. Other images'
  files aren't affected. The path itself works again at once.
- **Moving to the first release with this fix, once.** 0.4.1 and earlier put the mount's device
  number in each handle, so clients of those releases get "stale file handle" when the first
  fixed container starts, even if their handles survived restarts before. Remount, or
  reopen the file if the mount still answers. Later restarts and updates keep the handles.

*Why it depended on the runtime before (#218):* each handle from NFS-Ganesha holds its own id for
the backing filesystem. A FUSE mount has none, so Ganesha took the mount's device number, which is
the lowest number free when the mount is made. Under Podman the number changed on most restarts here
(157 to 159, say), and every handle went stale. Under Docker it stayed the same, which was luck:
when the test took the number away on purpose, Docker's clients went stale as well. `Filesystem_Id` doesn't help: it's only what clients see as the filesystem id.

The container speaks only NFSv4 (`Protocols = 4`); NFSv3 can't be tried against it.

## Troubleshooting

- **`sacdfs exited (needs --device /dev/fuse and CAP_SYS_ADMIN)`**: the device or the
  capability is missing, or (on AppArmor hosts) `apparmor=unconfined` is.
- **NFS clients get "permission denied" or I/O errors listing the share, and the log shows
  `vfs_open_by_handle ... Operation not permitted`**: `CAP_DAC_READ_SEARCH` is missing, or
  the container is rootless.
- **`SACDFS_SOURCE /images does not exist`**: the images volume isn't mounted.
- **A port is already in use**: see the notes on addresses under Docker and TrueNAS above.
- **A client gets I/O errors, and the log shows `... has waited 30.0 s (running on
  sacdfs-read-3 for 30.0 s): failing it with EIO after the 30 s request timeout`**: the
  images did not answer for 30 s (a disk in error recovery, a hung network share under
  `SACDFS_SOURCE`). sacdfs gives up on that request so that the client is not left waiting
  (an SMB client would otherwise wait for hours), and keeps serving the other images. The
  line says what was asked (the image, track and offset of a read, or the inode of a
  listing), and whether it was running or still queued. Once the images answer again,
  requests succeed again; nothing needs restarting. A client may see a few times the timeout
  before its error, because the kernel asks again for the same data. Raise
  `SACDFS_REQUEST_TIMEOUT` if the images are on disks that spin down and take longer than
  30 s to wake.
  **If clients keep retrying through a long hang,** a failed read keeps its thread until the
  source answers. Once 4 reads of one image are blocked past the timeout, the log says
  `<image>: 4 reads still blocked after the 30 s request timeout: failing its new reads with
  EIO at once until they finish`, and new reads of that image fail at once, without a thread,
  so that the other images keep their threads
  ([#241](https://github.com/aclater/sacdfs/issues/241)). `<image>: its blocked reads finish:
  reading it again` follows when the source answers. A hang of many images at once (a whole
  share) can still use every read thread, and then reads of every image fail until it answers.
- **`sacdfs did not mount within 300 s`**: sacdfs lists every image before it mounts, and a
  large library on a slow disk can take longer than the container waits. Raise
  `SACDFS_MOUNT_TIMEOUT` (seconds, 1 to 86400, default 300) and the health check's start
  period with it, so that the runtime does not mark the container unhealthy while it waits.
  A value that is not a whole number of seconds stops the container at once with a message.
- More detail: `SACDFS_VERBOSE=2`, `SMB_LOG_LEVEL=3`, `NFS_LOG_LEVEL=DEBUG`.

## Testing

CI (`.github/workflows/container.yml`) builds the image, starts it on the test corpus, and
mounts the shares with the kernel's SMB and NFS clients from a client container. It reads
every track the corpus manifest expects and checks each DSF file's per-channel SHA-256 and
each WAV file's header (`deploy/check-share.py`), over SMB and over NFS. A second run checks
user access (guests refused), `SACDFS_FORMATS=dsf` and `SACDFS_FLATTEN=1`. In between, under
Docker and then Podman (`deploy/ci-restart-test.sh`), a client opens a file over SMB and over
NFS 4.2, reads half of it, and reads the rest through the same open file after the container
restarts, and stats and lists the same paths. A second round per runtime takes the mount's device
number away first, so the test doesn't depend on the number happening to stay the same. Then
(`deploy/ci-same-host-test.sh`, Docker) the container shares its mount through an empty host directory
bound over `/srv` with `rshared`, and a reader container, as uid 1000, binds that directory with `rslave`
and checks every DSF track of the corpus: at start, after a restart, and after sacdfs in the container
is killed and the restart policy starts it again, without the reader being restarted.
