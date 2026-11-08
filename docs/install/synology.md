# sacdfs on Synology DSM

**Partly validated, in a VM only.** What follows was run on DSM 7.4.1 (kernel 4.4.302) in a VM,
not on Synology hardware, with the 0.4.1-rc.3 release binary. Storage setup, packages and the
DSM web interface were not exercised, and the container route is untested on Synology.

**Release binary (works in the VM, one extra step).** As root over SSH:

1. In the VM, DSM had no `/dev/fuse` until the kernel module was loaded. Either of these worked:

   ```sh
   modprobe fuse
   insmod /usr/lib/modules/fuse.ko
   ```

   This may differ on hardware (the module may already be loaded, or loaded by a package).
   Only the old `fusermount` (FUSE 2) is installed, not `fusermount3`; sacdfs mounted anyway.
2. Install the binary as in [binary.md](binary.md#binaries) and mount your folder of images:

   ```sh
   ./sacdfs /volume1/music /volume1/sacd
   ```

   All 16 files (DSF and WAV) of two synthetic test images read back with the right SHA-256, and
   stopping sacdfs with SIGTERM unmounted cleanly, and mounting again gave the same hashes.
3. NFS (v3 only was tried): with `/etc/exports` holding
   `/volume1/sacd 127.0.0.1(ro,fsid=4242,no_subtree_check)` and DSM's `exportfs` and `mountd`,
   a loopback NFSv3 mount (`mount -t nfs -o vers=3,nolock,ro`) read all 16 files correctly. sacdfs
   was started with `--allow-other` (see the [user guide](../user-guide.md#sharing-the-mount-over-nfs)).

**Container Manager (Compose): untested on Synology.** Container Manager did not install in the
VM: `synopkg install` stopped with "failed to get any volume for installation", because the VM has
no DSM storage volume, so this says nothing about sacdfs. The Compose file is
[deploy/nas-compose.yaml](../../deploy/nas-compose.yaml), the one file for every NAS container
manager; it was run with Docker Compose and Podman on other systems (see
[docker-podman.md](docker-podman.md) and [asustor.md](asustor.md)), not on DSM. In Container
Manager, create a project from the file (the menu names were not seen) and change the one marked
line to the path of your images. DSM's own file services use ports 445 and 2049, which is why the file publishes
10445 and 12049. Whether Container Manager's Docker allows `/dev/fuse` and `SYS_ADMIN` is not known.

**Not tested:** SMB (no `smbd` was found in the VM), NFSv4, a client on another machine, and the
container route on DSM.

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).
