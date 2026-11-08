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

TOS uses systemd. This service started sacdfs again after the VM restarted. Create the file
`/etc/systemd/system/sacdfs.service`:

```ini
[Unit]
Description=SACD images as DSF and WAV files
RequiresMountsFor=/Volume1/music /Volume1/sacd
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
CacheDirectory=sacdfs
ExecStart=/Volume1/sacdfs/sacdfs -f --allow-other --index-cache %C/sacdfs /Volume1/music /Volume1/sacd
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Stop sacdfs if you started it by hand (`umount /Volume1/sacd`), then:

```sh
systemctl daemon-reload
systemctl enable --now sacdfs
systemctl is-active sacdfs
```

*You should see* `active`, and `ls /Volume1/sacd` lists the images, now and after a restart.
`journalctl -u sacdfs` shows its log. In the VM the data volume was mounted before TOS
started, not by TOS's own storage manager. So whether `RequiresMountsFor=` waits for the
volume on real hardware wasn't checked, and neither was whether a TOS update keeps the file.

**Not checked:** the container route (TerraMaster's Docker Engine comes from its App Center,
which wasn't exercised), a client on another machine, and anything on hardware.
`mount.cifs` stopped with "not installed setuid root" even as root, cause unknown.

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).
