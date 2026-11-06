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
  them (mount propagation to the host is not supported on TrueNAS). Serving from inside the
  container depends on nothing in the host but Docker or Podman.
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
    start, because FUSE has no stable device number. A changing id would also invalidate
    every client's handles.

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
| `SACDFS_CACHE_MB` | (32, or 16 under a limit below 512 MiB) | `--cache-mb`: decoded-frame cache in MiB. The default suits a NAS; see [Memory](#memory) |
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

The default cache of 32 MiB (`SACDFS_CACHE_MB`, `--cache-mb`, since #150) suits a NAS, and is 16 MiB under a memory limit below 512 MiB ([Under a memory limit](#under-a-memory-limit)). The cache holds
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
`MemoryMax=` or `MemoryHigh=`. It sets four things from it. With no limit, none of this applies,
and nothing changes from earlier versions.

| What | With a limit |
|---|---|
| Cache of frames (`--cache-mb`) | 16 MiB below 512 MiB, else 32 |
| Decoder threads (`--threads`) | one per CPU, at most one per 64 MiB of the limit, and at least 2 |
| Memory for what listed images hold (`--listing-mb`) | 3/8 of the limit, at least 8 MiB |
| Memory watch | see below |

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
memory limit 256 MiB: listing all 5000 images in memory would take about 325 MiB, so 64 MiB of listings are kept and images not used lately are read again when next used; going through the whole library again will be slower; to avoid that, raise the limit to about 406 MiB
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

**What was measured.** sacdfs mounted directly under a memory limit, with its client in the same
limit, listing and stating every file while a paced reader played a 6-channel WAV. Synthetic
libraries of links to real discs, about 25 files per image; single runs on a 20-thread machine that
was busy with other work, so the times only show scale. Peak is sacdfs's anonymous memory.

| Limit | Images | 0.4.0 | 0.4.1-rc.2 | With the budget |
|---:|---:|---|---|---|
| 256 MiB | 1,600 | 128 MiB | 125 MiB | 101 MiB |
| 256 MiB | 5,000 | killed after about 91,000 of 123,974 files | killed after about 90,000 | 126 MiB |
| 256 MiB | 20,000 | killed after about 83,000 of 495,874 files | killed after about 80,000 | 177 MiB |
| 512 MiB | 5,000 | 310 MiB | 314 MiB | 228 MiB |

The player had no late read in any of these, and nothing was refused. Restarting and listing 5,000
images at 256 MiB, with the seek index kept between runs, was killed 4 times out of 4 on 0.4.1-rc.2,
and worked 3 times out of 3 here.

Over a library past its budget, a second pass reads TOCs again, so it takes longer: at 5,000 images
and 512 MiB, 24 s against 17 s on 0.4.1-rc.2. Those TOCs were in the page cache. On a slow disk with
nothing cached a second pass could take about as long as the first; that hasn't been measured. A
library that fits its budget isn't affected: at 1,600 images and 256 MiB, second passes took 5 to 6
s, and 5 s on 0.4.1-rc.2, measured side by side.

**Lower limits.** With 5,000 images, listing worked at 128 and 96 MiB. At 64 MiB, one run in three
was killed; the startup warning says this can happen at that size. Pressure from something else in the limit also
works as described: with another process holding 205 MiB of a 256 MiB limit, new listings failed with
`ENOMEM`, sacdfs and the player carried on, and once that process ended every file listed again.
Not tested: the memory watch under Samba and NFS clients at once, or `--tags`.

### Small NAS: sizing

What a small NAS needs for the container, measured on 0.4.1-rc.1 (single runs), before listings had a budget; the container runs of the budget are in [Under a memory limit](#under-a-memory-limit), and CPU limits weren't re-run for it. The container
was limited to 2 or 4 CPUs of an Intel N100-class chip (Gracemont E-cores at about 2.6 GHz) and
to the memory below; SMB was over loopback, and the library was on network-attached disks. The
figures are container limits, not whole-box RAM: a NAS's own services need their share, and
whole-box sizes were not tested. The current `main` has #224, which matched or beat these
memory figures in what was re-measured (a real library listed in 33.8 MiB against 38.5; 5,000
images at 274 MiB against 275), but the limits below were not re-run on it.

| | CPUs | Limit for sacdfs plus Samba | Library |
|---|---|---|---|
| Minimum | 2 N100-class cores | 256 MiB | up to about 1,800 images in memory; larger libraries work, and read TOCs again ([Under a memory limit](#under-a-memory-limit)) |
| Recommended | 4 cores | 512 MiB | up to about 3,600 images in memory, and larger libraries the same way |

Add about 40 MiB if NFS is on (`NFS=1`, the default). To set such a limit,
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

**Playback** on 2 E-cores and 256 MiB: stereo and 6-channel DST, DSF and WAV, with one or two
players, had no late reads with a 0.25 s buffer. The headroom for 6-channel at cache 32 was 11.8
times real time for DSF and 9.7 times for WAV.

**Known limit.** A whole-file WAV scan starves a 6-channel WAV player on 2 cores: the player
fell about 13 s behind
([#230](https://github.com/aclater/sacdfs/issues/230)). A smaller cache helps, at a cost. On 4
cores, `SACDFS_CACHE_MB=8` left the player 0.10 s behind, against 11.2 s with four scanners at the
default cache; but a whole-file WAV scan dropped from about 61 to 22 MB/s, and the 6-channel WAV
headroom from 18 times to 7 times real time. Memory at the floors is
[#231](https://github.com/aclater/sacdfs/issues/231).

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
clients can use other ports. Windows 11 build 26200 also mounted a share on another port with `net use` and `/TCPPORT:` (see the [client notes](clients.md#windows-explorer-and-robocopy); other Windows builds were not tried).

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
     `net use ... /TCPPORT:` (tested on 0.4.1-rc.1, [client notes](clients.md#windows-explorer-and-robocopy));
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
         SACDFS_CACHE_MB: "16"
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
   - `SACDFS_CACHE_MB: "32"`, the default, keeps a scan that reads only the ends of files under 50 MiB of memory on a small library (the bench discs), and whole-file reads up to about 100 MiB at about 400 images; more images take more ([Memory](#memory), [Small NAS: sizing](#small-nas-sizing)).
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

In Compose: `- /srv/sacd:/media:ro,rslave`.

### macOS

- **SMB:** Finder, Go, Connect to Server (⌘K), `smb://nas/sacd`; "Connect as Guest", or the
  `SMB_USER` name and password.
- **NFS** (macOS speaks NFS 4.0): `sudo mount -t nfs -o vers=4,ro,resvport nas:/sacd /Volumes/sacd`
  after `sudo mkdir /Volumes/sacd`; or Finder's Connect to Server with `nfs://nas/sacd`.

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

## NFS and restarts

NFS clients keep file handles across server restarts. sacdfs gives every file and
directory the same inode number on every mount, and supports NFS export (#18). So after the
container restarts (a NAS reboot, an update), NFS clients go on reading the files they had
open, and their cached directories stay valid. SMB is path-based and reconnects by itself.

CI checks both: a client opens a file over SMB and over NFS (4.2), reads half of it, the
container restarts, and the client reads the rest through the same open file.

An image whose file changed (size or modification time) gets new file handles, so a client
holding an old one gets "stale file handle" instead of other data.

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
  **The other images are safe only for a while:** if clients keep retrying through a long hang,
  each retry holds one more thread, and once 64 are held, reads of every image fail too until
  the source answers. With 8 clients at the default timeout on 2 decoder threads, that took about
  6 minutes ([#241](https://github.com/aclater/sacdfs/issues/241)). Lowering the timeout makes it
  come sooner.
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
user access (guests refused), `SACDFS_FORMATS=dsf` and `SACDFS_FLATTEN=1`. In between, a
client opens a file over SMB and over NFS, reads half of it, and reads the rest through the
same open file after the container restarts.
