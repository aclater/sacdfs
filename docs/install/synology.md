# sacdfs on Synology DSM

**Release binary: works, with one extra step. Container Manager: untested.** Checked on
DSM 7.4.1 in a VM, not on Synology hardware. Storage setup and app stores weren't exercised.
The checks used a build of `main` from before 0.5.0 (0.4.1-rc.3 earlier).

## Release binary

Follow [the NAS steps](nas-binary.md) with `/volume1`, as root (`sudo -i` after logging in
over SSH; not checked, because the VM's login was already root). The extra step on DSM: **FUSE isn't loaded.** Before step 7, and again after every
restart of the NAS:

```sh
modprobe fuse
ls -l /dev/fuse
```

*You should see* a line starting with `crw`. Without it, sacdfs stops with
`/dev/fuse not found. Try 'modprobe fuse'`. DSM has FUSE 2's `fusermount`, not FUSE 3's
`fusermount3`. Run as root, sacdfs needs neither.

What was checked on DSM 7.4.1 (kernel 4.4):

| Check | Result |
|---|---|
| Steps 1, 2 and 4 to 9 of [the NAS steps](nas-binary.md), as written | All 22 files of two test images identical to the reference |
| A non-root user reading the mount | "Permission denied" without `--allow-other`, files readable with it |
| NFSv3: `/volume1/sacd 127.0.0.1(ro,fsid=4242,no_subtree_check)` in `/etc/exports`, `systemctl start nfs-server`, mounted on the NAS itself with `mount -t nfs -o vers=3,nolock,ro` | All 22 files identical |
| NFSv4 the same way | `mount.nfs: Protocol not supported` |

**Not checked:** sharing through DSM's own Shared Folder and file-service settings (DSM's web
interface wasn't used, so whether DSM keeps or rewrites a hand-made `/etc/exports` there is
unknown), SMB (the VM had no `smbd`), a client on another machine, and starting sacdfs at boot.

## Container Manager

**Not tested.** Container Manager needs a DSM storage volume, and the VM couldn't make one.
`synopkg install` stops with "failed to get any volume for installation". DSM's own
`synostgpool --create` refused the VM's data disk with "Create space failed. Disk is already
used", even after its start was wiped. The VM's disks have no DSM slot or serial number.
The Compose file that worked under ASUSTOR's Docker is in [asustor.md](asustor.md#compose).
Container Manager runs Compose projects, but whether that file works there is unknown.

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).
