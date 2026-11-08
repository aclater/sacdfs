# sacdfs on QNAP

**Release binary: runs on QuTScloud 5.2.9, from the first release after 0.4.1 (#244). Container
route: untested.** Checked on QuTScloud 5.2.9 in a VM, not on QNAP hardware. Storage setup and app
stores weren't exercised. QTS and QuTS hero were not checked and may differ.

- **Release binary: works.** QuTScloud 5.2.9 has glibc 2.21 and kernel 5.10. The release binaries
  need glibc 2.17 or later from the first release after 0.4.1 ([binary.md](binary.md)); 0.4.1 and
  earlier needed 2.28 and stopped at start on QuTScloud with:

  ```
  ./sacdfs: /lib/libc.so.6: weak version `GLIBC_2.28' not found (required by ./sacdfs)
  ./sacdfs: relocation error: ./sacdfs: symbol statx, version GLIBC_2.28 not defined in file libc.so.6 with link time reference
  ```

  What was checked, with the release workflow's own x86_64 build (generic and x86-64-v3) of the
  pull request that made the change: `sacdfs --version`; mounting as `admin` (uid 0), where sacdfs
  mounts by itself (QuTScloud has no `fusermount3`); reading every DSF and WAV file of three discs
  (two test images and a real DST disc, 44 files), each identical to the same files served on a
  current Linux system; unmounting with `umount`. The mount, exported read-only by QuTScloud's own
  NFS server (NFSv3, with an explicit `fsid=`) and mounted over NFS on the same system, served the
  same bytes. SMB was not checked: the system had no Samba server running.

  Run it as `admin` with `sacdfs SOURCE MOUNTPOINT`, as in the [user guide](../user-guide.md), and
  unmount with `umount MOUNTPOINT`. Where to keep the images and the mount point on a real QNAP
  volume was not checked (the checked system had no storage pool).
- **Container (Container Station): untested on QNAP.** The checked system had no Docker, and
  Container Station could not be installed there. The Compose file is
  [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml), the one file for every NAS container
  manager. It was run with Docker Compose and Podman on other systems (see
  [docker-podman.md](docker-podman.md) and [asustor.md](asustor.md)), not on QNAP. In Container
  Station, create an application from the file (the menu names were not seen) and change the one
  marked line to the path of your images. QNAP's own file
  services use ports 445 and 2049, which is why the file publishes 10445 and 12049. sacdfs's
  container needs `/dev/fuse` and the capability `SYS_ADMIN` (and `DAC_READ_SEARCH` for NFS);
  whether Container Station allows them is not known. Every setting is in the
  [deployment guide](../deployment.md).

Reports from real QNAP hardware are welcome: open an issue that mentions
[#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how sacdfs was
started, and what worked.
