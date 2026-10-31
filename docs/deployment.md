# Deploying sacdfs on a NAS

sacdfs can run in two places ([#17](https://github.com/aclater/sacdfs/issues/17)):

- **On the NAS (server-side, the main deployment).** sacdfs runs next to the images, in a
  container, and shares its mount over SMB and NFS. Every client (Linux, macOS, Windows,
  phones, network players) sees ordinary DSF and WAV files. A seek costs sacdfs's few
  milliseconds on the NAS plus about one network round trip, because sacdfs's own reads of
  the image stay on the NAS.
- **On a Linux client (client-side).** sacdfs runs on the client and reads the images from a
  network share, as in the [README](../README.md). Every read of an image is then a network
  round trip.

This guide covers the first: the container image in this repository (`Containerfile`), how
to run it with Docker, Podman or Compose and on TrueNAS SCALE, and how clients mount its
shares.

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
  - The NFS export has the same fixed filesystem id (`Filesystem_Id = 101.1`) on every
    start, because FUSE has no stable device number. A changing id would also invalidate
    every client's handles.

## Building the image

```sh
git clone https://github.com/aclater/sacdfs && cd sacdfs
podman build --format docker -t sacdfs .      # or: docker build -t sacdfs .
```

`--format docker` keeps the health check, which the OCI image format has no field for. The
build compiles sacdfs for generic x86-64. On a NAS with an AVX2 CPU (most Intel and AMD CPUs
since about 2015, including the Intel N100 family), WAV conversion costs about 20 % less CPU with
`--build-arg RUSTFLAGS="-C target-cpu=x86-64-v3"`; see [performance.md](performance.md).
Don't use that build on an older CPU: it stops with an illegal-instruction error.

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
| Speed | faster in general | ample here: sacdfs's decoding, not the server, sets the pace (about 500 MB/s for DSF) |

**NFS-Ganesha is used**, because it asks nothing of the host, needs one capability instead
of a privileged container, and stops with the container. Both servers reopen files by NFS
file handle through `open_by_handle_at(2)` on the FUSE mount, so both depend equally on
sacdfs's persistent file identity ([#18](https://github.com/aclater/sacdfs/issues/18), see
[below](#nfs-and-restarts-pending-18)).

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
| `SACDFS_PCM_RATE` | (176400) | `--pcm-rate`: `88200`, `176400` or `352800` |
| `SACDFS_PCM_CUTOFF` | (40) | `--pcm-cutoff`: `40`, `30` or `wide` |
| `SACDFS_PCM_GAIN` | (0) | `--pcm-gain` in dB, -60 to +24 |
| `SACDFS_FLATTEN` | `0` | `1` for `--flatten` |
| `SACDFS_CACHE_MB` | `256` | `--cache-mb`: decoded-frame cache in MiB |
| `SACDFS_THREADS` | (one per CPU) | `--threads`: DST decoder threads. On a NAS that also serves other things, fewer threads keep decoding from crowding them out |
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

If the host already serves SMB or NFS itself, ports 445 and 2049 are taken. Either give the
container its own IP address (a `macvlan` network, or publish on an extra address of the
host: `-p 192.0.2.20:445:445 -p 192.0.2.20:2049:2049`, with the host's own services bound to
its main address), or publish other ports (`-p 10445:445 -p 12049:2049`). Linux and macOS
clients can use other ports; Windows can only use 445.

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
They are written from TrueNAS's documentation, not tested on a TrueNAS system; the names of
screens and fields may differ between releases.

1. **The image.** TrueNAS cannot build images. Build it elsewhere (above) and push it to a
   registry the NAS can reach, for example a private registry or your own GitHub Container
   Registry namespace, or load it from a file in a shell on the NAS (`docker load`).
2. **Addresses.** TrueNAS's own SMB and NFS services use ports 445 and 2049 on all its
   addresses. If you use them, give sacdfs an address of its own: add an alias IP address
   to the network interface (Network, Interfaces), then bind TrueNAS's SMB and NFS services
   to the main address only (in each service's settings, "Bind IP Addresses"), and publish
   sacdfs's ports on the alias (below). If you don't use TrueNAS's own SMB and NFS services,
   leave them stopped and publish on all addresses.
3. **Install it with YAML.** Apps, Discover Apps, Custom App, then "Install via YAML" (the
   form-based custom app may not offer every setting below). Paste a Compose file like
   this, with your dataset path and, if you use an alias address, the address in `ports`:

   ```yaml
   services:
     sacdfs:
       image: registry.example.org/you/sacdfs:latest
       restart: unless-stopped
       devices:
         - /dev/fuse
       cap_add:
         - SYS_ADMIN
         - DAC_READ_SEARCH
       security_opt:
         - apparmor=unconfined
       ports:
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
         start_period: 60s
   ```

   - The images' dataset is mounted read-only (`:ro`); sacdfs never writes to it.
   - The cache directory is a dataset or directory of its own (create it first); it holds
     the seek index, a few MB for a large library.
   - `SACDFS_THREADS` limits how many cores sacdfs may use for decoding while a client
     reads, if the NAS has other work to do.
4. **Check it.** The app's logs show sacdfs's image count, then `mounted`, then Samba and
   NFS-Ganesha starting. The app turns healthy within a minute.

The same YAML, without the TrueNAS paths, works on other NAS systems that run Docker
(Unraid, Synology Container Manager, OpenMediaVault), as far as they allow `/dev/fuse` and
the two capabilities.

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

## NFS and restarts (pending #18)

NFS clients keep file handles across server restarts. Today sacdfs's inode numbers are
stable for one mount's lifetime only. So **after the container restarts (a NAS reboot, an
update), NFS clients get "stale file handle" errors** on files and directories they had open
or cached, and must remount. A file whose kernel inode the container evicts under memory
pressure can also go stale while the container runs.

SMB is path-based and reconnects by itself. CI checks this: a client keeps reading a file it
opened before a restart.

Persistent file identity and FUSE export support
([#18](https://github.com/aclater/sacdfs/issues/18)) fix this. In CI, with that change merged
in, an NFSv4.2 client kept reading through the same open file across a container restart,
as the SMB client does. Until it is merged, prefer SMB, or remount NFS clients after a
restart.

## Troubleshooting

- **`sacdfs exited (needs --device /dev/fuse and CAP_SYS_ADMIN)`**: the device or the
  capability is missing, or (on AppArmor hosts) `apparmor=unconfined` is.
- **NFS clients get "permission denied" or I/O errors listing the share, and the log shows
  `vfs_open_by_handle ... Operation not permitted`**: `CAP_DAC_READ_SEARCH` is missing, or
  the container is rootless.
- **`SACDFS_SOURCE /images does not exist`**: the images volume isn't mounted.
- **A port is already in use**: see the notes on addresses under Docker and TrueNAS above.
- More detail: `SACDFS_VERBOSE=2`, `SMB_LOG_LEVEL=3`, `NFS_LOG_LEVEL=DEBUG`.

## Testing

CI (`.github/workflows/container.yml`) builds the image, starts it on the test corpus, and
mounts the shares with the kernel's SMB and NFS clients from a client container. It reads
every track the corpus manifest expects and checks each DSF file's per-channel SHA-256 and
each WAV file's header (`deploy/check-share.py`), over SMB and over NFS. A second run checks
user access (guests refused), `SACDFS_FORMATS=dsf` and `SACDFS_FLATTEN=1`. In between, a
client opens a file over SMB and over NFS, reads half of it, and reads the rest after the
container restarts. Until #18 is in, the NFS half is expected to fail, so that step reports
the result without failing the job.
