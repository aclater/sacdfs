# sacdfs on TerraMaster TOS

**Validated in a VM only, binary route**: TOS 7.0.1422 (kernel 6.12, glibc 2.35) in a VM, not on
TerraMaster hardware, with the 0.4.1-rc.3 release binary. Storage setup, the App Center and the
TOS web interface were not exercised. Run the commands as root over SSH.

1. `fusermount3` and `/dev/fuse` are present. Install the binary as in
   [binary.md](binary.md#binaries) and mount:

   ```sh
   ./sacdfs /Volume1/music /Volume1/sacd
   ```

   All 16 files (DSF and WAV) of two synthetic test images had the right SHA-256, and stopping
   sacdfs with SIGTERM and mounting again gave the same hashes.
2. NFS: TOS has the kernel NFS server. With sacdfs started `--allow-other` and
   `/etc/exports` holding `/Volume1/sacd 127.0.0.1(ro,fsid=4242,no_subtree_check)`, loopback
   mounts as NFSv3 and NFSv4 both read all 16 files correctly (see the
   [user guide](../user-guide.md#sharing-the-mount-over-nfs)).
3. SMB: a Samba share of the mount (4.15, a minimal `smb.conf` of one read-only guest share)
   read all 16 files correctly with `smbclient`. `mount.cifs` stopped with "not installed setuid
   root" even as root; the cause is unknown.

**Not tested:** the container route (no Docker package that could be downloaded directly was
found; TerraMaster's Docker Engine comes from its App Center), a client on another machine, and
anything on hardware.

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).
