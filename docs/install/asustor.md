# sacdfs on ASUSTOR ADM

**Validated in a VM only**: ADM 5.1.4 (kernel 6.6) in a VM, not on ASUSTOR hardware, with the
0.4.1-rc.3 release binary and image. Storage setup, App Central and the ADM web interface were
not exercised. Run the commands as root over SSH. ADM's own tools are under
`/usr/builtin/bin` and `/usr/builtin/sbin`, which may not be on the PATH of an SSH session.

**Release binary.** `/dev/fuse` is present and there is no `fusermount3`; sacdfs mounted
directly as root.

```sh
./sacdfs /volume1/music /volume1/sacd
```

All 16 files (DSF and WAV) of two synthetic test images had the right SHA-256, and stopping
sacdfs with SIGTERM and mounting again gave the same hashes. A SMB share of the mount, made
with ADM's own Samba 4.20 and mounted with `mount.cifs`, read all 16 correctly; so did an NFSv3
loopback mount of a kernel NFS export (`/etc/exports` as in the
[user guide](../user-guide.md#sharing-the-mount-over-nfs), sacdfs with `--allow-other`). In the
VM the NFS and CIFS kernel modules had to be loaded by hand (`modprobe nfsd nfsv3 cifs`); on
hardware ADM may do that itself.

**Container (Docker CE 25.0.5 from ASUSTOR's package).** In the VM the package would not
install (`apkg --install` exited with status 22 and no message), so Docker was started with the
package's own `start-stop.sh start` after putting ADM's `/usr/builtin/sbin` on the PATH
(without it Docker stopped: "iptables not found"). That install step is untested; do not treat
it as a recommendation. With Docker running, the documented command worked, with no AppArmor
option, since ADM has none:

```sh
docker run -d --name sacdfs --restart unless-stopped \
  --device /dev/fuse --cap-add SYS_ADMIN --cap-add DAC_READ_SEARCH \
  -p 10445:445 -p 12049:2049 \
  -v /volume1/music:/images:ro -v sacdfs-cache:/cache \
  ghcr.io/aclater/sacdfs:0.4.1-rc.3
```

The container was healthy in under a minute, `docker restart sacdfs` brought it back healthy,
and all 16 files read inside it with the right SHA-256. Mounted from the NAS itself with
`mount.cifs` on port 10445, the SMB share read all 16 correctly. ADM's own SMB service uses port
445, so other ports were used. NFS to the container was not read: ADM's `mount` here reports
"NFSv4 not supported".

**Not tested:** the ADM Docker app's own interface (so which container settings a normal user can
set there is unknown), Compose, a client on another machine, and anything on hardware.

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).
