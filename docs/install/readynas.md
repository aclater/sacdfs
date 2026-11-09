# sacdfs on Netgear ReadyNAS OS 6

**Release binary: works. Container route: untested.** Checked on ReadyNAS OS 6.10.10 in a VM, not
on the vendor's hardware. App stores weren't exercised, and the sacdfs checks ran on a plain ext4 volume at
`/data`, not on a volume made by the vendor's tools. (A later try with two extra SATA disks
made a RAID 1 volume with the vendor's `volume_util` command, which came up as a Btrfs `/data`; the
sacdfs checks were not repeated on it. The web interface was not set up or used.) Run the commands as root over SSH. The checks used the 0.5.0 release binary and the files
in [`deploy`](../../deploy) from the 0.6.0 development branch.

ReadyNAS OS 6 is Debian 8 (glibc 2.19, kernel 4.4, systemd 230). FUSE is built into the kernel, so
`/dev/fuse` is there and `modprobe fuse` isn't needed.

## Release binary

Follow [the NAS steps](nas-binary.md) with your data volume's path in place of `/volume1`. On a
real ReadyNAS that is `/data` or a share under it; the VM used `/data`. The download and
checksum steps (3 and 4) were not run on this system: the binary was copied in from another
machine.

| Check | Result |
|---|---|
| `sacdfs --version` | `sacdfs 0.5.0` (so 0.5.0 runs on glibc 2.19) |
| The systemd unit from [Start at boot and stop safely](nas-binary.md#with-systemd): start | Active; all 22 files of two test images identical to the reference. The first start took 15.3 s (cause not looked into) |
| `systemctl stop`, nothing open | Stopped at once (0.0 s), no mount left, exit status 0 |
| `systemctl stop`, a file on the mount held open | 30.2 s, then systemd killed sacdfs; no mount left |
| `kill -9` on sacdfs under the unit | Active again when looked at 7 s later; files identical |
| A dead mount left in place (sacdfs killed by hand), then `systemctl start` | Started at once; files identical |
| The VM restarted with the unit enabled | The service started on its own during boot (about 1.5 minutes into this VM's slow boot); the images were listed |
| `sacdfs-ctl` start, status, stop, restart; start after `kill -9`; stop with a file held open | As in the table in [nas-binary.md](nas-binary.md#without-systemd-sacdfs-ctl); the held-open stop took 31 s |

## Sharing with ReadyNAS's own Samba

ReadyNAS OS 6 runs Samba 4.8 and reads `/etc/frontview/samba/Shares.conf`. In the VM, appending

```ini
[sacd]
 path = /data/sacd-mount
 read only = yes
 guest ok = yes
```

(with the path of your mount folder) to that file and running `smbcontrol smbd reload-config` made
the share appear, and `smbclient //127.0.0.1/sacd -N` read one DSF file identical to the reference.
sacdfs ran with `--allow-other`. Not checked: whether the web interface rewrites `Shares.conf`
(it probably does when you change shares there), a client on another machine, and NFS.

**Not checked:** the vendor's web interface and its add-ons, volumes made by ReadyNAS itself,
and anything on hardware. Reports from real hardware are welcome on
[#82](https://github.com/aclater/sacdfs/issues/82).
