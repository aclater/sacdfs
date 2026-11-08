# sacdfs on TerraMaster TOS

**Release binary: works. Container route: untested.** Checked on TOS 7.0.1422 in a VM, not on
TerraMaster hardware. Storage setup and app stores weren't exercised. The checks used the 0.5.0 release (0.4.1-rc.3 earlier). Run the commands as root over SSH.

## Release binary

Follow [the NAS steps](nas-binary.md) with `/Volume1` (capital V) in place of `/volume1`.
`/dev/fuse` and `fusermount3` are there.

- **Don't use `install` to copy sacdfs.** For root on TOS, `install` is TerraMaster's own
  tool (`/usr/sbin/install`), which stops with `flag provided but not defined: -m`. The NAS
  steps use `cp`.

| Check | Result |
|---|---|
| Steps 1 to 9 of [the NAS steps](nas-binary.md), as written, download included, with `/Volume1` | All 22 files of two test images identical to the reference |
| NFS: `/Volume1/sacd 127.0.0.1(ro,fsid=4242,no_subtree_check)` added to `/etc/exports`, then `exportfs -ra`, mounted on the NAS itself as NFSv3 and NFSv4 | All 22 files identical over each |
| `systemctl restart nfs-server` afterwards | `/etc/exports` was empty again |
| Earlier, with 0.4.1-rc.3: a Samba share of the mount (a minimal `smb.conf`, one read-only guest share), read with `smbclient` | All files correct |

**A line added to `/etc/exports` by hand doesn't last.** It stayed while NFS ran, but every
restart of the NFS service left the file empty. A restart of the NAS starts that service again,
but its effect on the file wasn't checked. Which TOS tool empties it wasn't traced. Sharing through TOS's own NFS settings
wasn't checked.

### Starting at boot

TOS uses systemd, so use the shipped unit: see
[With systemd](nas-binary.md#with-systemd) in the NAS steps, with `/Volume1` in
`/etc/default/sacdfs`. In the VM the unit started sacdfs (22 files of two test images identical to
the reference), `systemctl stop` unmounted at once when nothing was open, a sacdfs killed with
`kill -9` was started again by the unit, and a dead mount left behind was cleared on the next start.
After a restart of the VM the service was active. Stopping with a file held open took the full 30 s,
after which systemd killed sacdfs and no mount was left. Never stop sacdfs with `kill -9` by hand;
the NAS steps say why. In the VM the data volume was mounted before TOS started, not by TOS's own
storage manager. So whether the unit waits for the volume on real hardware wasn't checked, and
neither was whether a TOS update keeps the unit file.

**Docker (Compose): not checked on TOS.** TerraMaster's Docker Engine comes from its App Center,
which wasn't exercised. The Compose file for every NAS container manager is
[deploy/nas-compose.yaml](../../deploy/nas-compose.yaml). It was run with Docker Compose on ASUSTOR
ADM ([asustor.md](asustor.md#compose)) and on Podman, not on TOS. In TOS's Docker app, create a
project from the file (the menu names were not seen) and change the one marked line to the path of
your images. The file publishes ports 10445 and 12049 because TOS's own file services use 445 and
2049.

**Not checked:** TOS's Docker app, a client on another machine, and anything on hardware.
`mount.cifs` stopped with "not installed setuid root" even as root, cause unknown.

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).
