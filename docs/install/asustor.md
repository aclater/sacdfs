# sacdfs on ASUSTOR ADM

**Validated in a VM only**: ADM 5.1.4 (kernel 6.6) in a VM, not on ASUSTOR hardware, with the
0.4.1-rc.3 release binary, and the 0.5.0 image for the Compose file. Storage setup, App Central and the ADM web interface were
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

**Container, Compose (Docker CE 25.0.5 and Docker Compose 2.24.7, both from ASUSTOR's package).**
In the VM the package would not install through App Central's tools (`apkg --install` exited with
status 22 and no message), so its files were unpacked by hand into
`/usr/local/AppCentral/docker-ce` and Docker was started with the package's own `start-stop.sh
start`, after putting ADM's `/usr/builtin/sbin` on the PATH (without it Docker stopped:
"iptables not found"). That install step is untested; do not treat it as a recommendation. The
package carries `docker compose`. With Docker running, the Compose file
[deploy/nas-compose.yaml](../../deploy/nas-compose.yaml) worked unchanged (its images path is
`/volume1/music`, which was the VM's folder; there is no AppArmor on ADM, and the file's
`apparmor=unconfined` does no harm):

```sh
docker compose -f nas-compose.yaml up -d
```

With the 0.5.0 release image, the container was healthy 32 s after `up`. Mounted from the NAS itself
(`mount.cifs` on port 10445, and NFSv4.2 on port 12049 from a Fedora 44 container with
`--network host`, since ADM's own `mount` has no NFSv4), both shares served all 914 files (DSF and
WAV of 457 tracks of the test corpus) with the same SHA-256 as the files read by a Linux client on
another system. ADM's own SMB service uses port 445, so the file publishes other ports. Of the stops
and restarts, see "Stopping" in [docker-podman.md](docker-podman.md#stopping-and-restarting).

**Not tested:** the ADM Docker app's own interface (so which container settings a normal user can
set there is unknown), a client on another machine, and anything on hardware.

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).
