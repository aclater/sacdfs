# sacdfs on QNAP

**Release binary: works from 0.5.0, also started at boot by `autorun.sh`. Container Station: Compose file works.** Checked on QuTScloud 5.2.9 in a VM, not on QNAP hardware.
QuTScloud wouldn't make a storage pool in the VM (see below), so the volume was an ext4 file system made by hand,
and the Container Station package came from the App Center. QTS and QuTS hero weren't checked and may differ. The
checks used the 0.5.0 release.

## Release binary

Follow [the NAS steps](nas-binary.md) as `admin`, which is root on QNAP (`id -u` prints `0`).
In place of `/volume1`, use a shared folder: the examples here use `/share/Multimedia`.

```sh
mkdir -p /share/Multimedia/sacdfs /share/Multimedia/sacd
/share/Multimedia/sacdfs/sacdfs --allow-other /share/Multimedia/music /share/Multimedia/sacd
```

- **Use 0.5.0 or later.** QuTScloud 5.2.9 has glibc 2.21. From 0.5.0 the release binaries
  need glibc 2.17 ([binary.md](binary.md)). 0.4.1 and earlier stopped at start with
  `` weak version `GLIBC_2.28' not found ``.
- **Keep sacdfs in a shared folder.** QNAP's system folders, `/root` included, are on a
  450 MB RAM disk.
- **Check with `ls`.** `/share/Multimedia` is a link to the volume's real path, so
  `/proc/mounts` lists the mount under that path (`/share/CACHEDEV1_DATA/...`), not under
  `/share/Multimedia`.

What was checked on QuTScloud 5.2.9 (kernel 5.10). The VM had no storage pool (see below), so
an ext4 filesystem on its data disk was mounted at `/share/CACHEDEV1_DATA`, with
`/share/Multimedia` linked to a folder on it, the way QNAP lays out shared folders:

| Check | Result |
|---|---|
| Steps 1 to 9 of [the NAS steps](nas-binary.md), as written, download included | All 22 files of two test images identical to the reference |
| Logging out of SSH and back in | Still mounted, same files |
| Earlier, with the pull request's build: QuTScloud's own NFS server (NFSv3, explicit `fsid=`), mounted on the same system | Same bytes as on a current Linux system, for two test images and a real DST disc |

### Starting at boot with `autorun.sh`

The `autorun.sh` from [the NAS steps](nas-binary.md#starting-it-at-boot-with-the-nass-own-tools) was
installed exactly as written there, with "run user defined processes" on (`Misc Autorun = TRUE`,
set with `setcfg`, not with the menu, which wasn't seen), and the VM was restarted. The data volume
was the by-hand ext4 file system, mounted early in each start by a `crontab` line (the stand-in for a
volume QNAP mounts itself).

| Check | Result |
|---|---|
| Start at boot: `autorun.sh` after a VM restart, sacdfs not running before | sacdfs mounted by itself about 2.5 minutes after the VM started (the script waited for the volume), `sacdfs-ctl` said "started" |
| Files after that start | All 16 files of two test images identical to the reference |
| The setting after the restart | `Autorun = TRUE` was still there (the earlier check, made before the first-run setup, lost it at every restart) |
| Stop: QNAP's own power-off of the VM (the power-button handler) with sacdfs running | `signal 15: unmounting` in the sacdfs log; the next start of the VM mounted the file system with no journal recovery message, sacdfs started again, the same 16 files identical |

Two things to know. **QuTScloud finishes starting up slowly in the VM**: about 22 minutes after power-on, because
it waits for a licence check that has no answer. `autorun.sh` ran after about 2.5 minutes, but a restart or
power-off asked for in that time waits until the 22 minutes are over. The command-line restart (`reboot`,
`qreboot`) did nothing visible during that wait either. So the stop above was a power-off, not a restart from the
menu. And **the volume was not QNAP's**: the first-run setup partitions the disks but makes no volume, and
`qcli_pool` stops with "read limitation error!" (details below).

**Not checked:** a non-root user (QuTScloud has no `su`), SMB (no Samba server ran), sharing
through QNAP's own shared-folder and NFS settings, a client on another machine, the "run user defined
processes" menu, and a volume made by QNAP's Storage Manager. For stopping, see
[the NAS steps](nas-binary.md#starting-it-at-boot-with-the-nass-own-tools).

## Container Station

**Compose file: works in the VM, with caveats.** Container Station 3.1.4 came from QNAP's App
Center (`qpkg_cli -g container-station` downloads the package; `qpkg_cli -m <the downloaded file>`
installed it, because `qpkg_cli -a` stayed "waiting for install"). It installed on the hand-made
ext4 volume from above, not on a volume QNAP made: QuTScloud in the VM wouldn't make a storage pool
(the first-run setup partitions the disks but makes no volume, and every `qcli_pool` call stops with
"read limitation error!"; no licence was installed, and activating one needs a QNAP account).
Container Station gave Docker 27.1.2 and Compose v5.1.1. Its Docker engine was not running after the
install; `supervisorctl start docker` (Container Station's own supervisor) started it, because the web
interface's first-run step, which presumably does that, wasn't used.

[deploy/nas-compose.yaml](../../deploy/nas-compose.yaml) ran as published, with only the images
path changed to `/share/CACHEDEV1_DATA/Multimedia/music`, with `docker compose up -d` over SSH.
QNAP's own file services use ports 445 and 2049, which is why the file publishes 10445 and 12049.

| Check | Result |
|---|---|
| `docker compose up -d` with the published 0.5.0 image, pulled from the registry | Healthy 18 s after `up` (includes the pull) |
| SMB on port 10445 (`smbclient` in a throwaway Alpine container on the host network), two test images | All 16 files identical to the reference |
| `docker compose restart` | Healthy again after 17 s, the same 16 files identical |
| `docker compose stop` | 0.3 s, exit status 0; `down` left no container |

**Not checked:** the Container Station screens (this used the command line, not the web interface,
so the menu names for creating an application from a Compose file are still unseen), NFS, a client on
another machine, a volume made by QNAP's Storage Manager, and anything on hardware. In an application
created from the file in the web interface, change the one marked line to the path of your images. Whether
Container Station's web interface allows `/dev/fuse` and `SYS_ADMIN` wasn't checked: the command line did.

Reports from real QNAP hardware are welcome: open an issue that mentions
[#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how sacdfs was
started, and what worked.
