# sacdfs on QNAP

**Release binary: works from 0.5.0. Container Station: untested.** Checked on QuTScloud 5.2.9
in a VM, not on QNAP hardware. Storage setup and app stores weren't exercised. QTS and
QuTS hero weren't checked and may differ. The checks used the 0.5.0 release.

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

**Not checked:** a non-root user (QuTScloud has no `su`), SMB (no Samba server ran), sharing
through QNAP's own shared-folder and NFS settings, a client on another machine, and `autorun.sh`
starting sacdfs by itself at boot (the VM forgets the "run user defined processes" setting at every
restart; run by hand, the script worked). For starting at boot and stopping, see
[the NAS steps](nas-binary.md#starting-it-at-boot-with-the-nass-own-tools).

## Container Station

**Not tested.** Container Station needs a storage pool, and QuTScloud makes one only through
its first-run web setup, which wasn't run. Until that setup runs, the web interface reports
`mediaReady` 0, the storage command-line tools point into a partition that isn't mounted, and
there is no volume to install apps on. QuTScloud may also need a paid licence. The Compose
file for every NAS container manager is [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml).
It was run with Docker Compose on ASUSTOR ADM ([asustor.md](asustor.md#compose)) and on Podman,
not on QNAP. In Container Station, create an application from the file (the menu names were not
seen) and change the one marked line to the path of your images. QNAP's own file services use ports
445 and 2049, which is why the file publishes 10445 and 12049. Whether Container Station allows
`/dev/fuse` and `SYS_ADMIN` is unknown.

Reports from real QNAP hardware are welcome: open an issue that mentions
[#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how sacdfs was
started, and what worked.
