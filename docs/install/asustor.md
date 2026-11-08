# sacdfs on ASUSTOR ADM

**Release binary and Docker Compose: work.** Checked on ADM 5.1.4 in a VM, not on ASUSTOR
hardware. Storage setup and app stores weren't exercised. The checks used the 0.5.0 code (a build
of `main` that differs from the release only in its version number) and an image built from it (0.4.1-rc.3 earlier). Run the commands as root
over SSH. ADM keeps some of its tools in `/usr/builtin/bin` and `/usr/builtin/sbin`, which may
not be on an SSH session's PATH.

## Release binary

Follow [the NAS steps](nas-binary.md) with `/volume1`. Nothing extra was needed: `/dev/fuse`
is there, and sacdfs mounts as root without `fusermount3`, which ADM doesn't have.

| Check | Result |
|---|---|
| Steps 1, 2 and 4 to 9 of [the NAS steps](nas-binary.md), as written | All 22 files of two test images identical to the reference |
| A Samba guest share of `/volume1/sacd` (ADM's Samba 4.20, a minimal hand-made `smb.conf` on port 4450), mounted on the NAS with `mount.cifs` | All 22 files identical with `--allow-other`. Without it, the mount failed with "Permission denied" |
| Earlier, with 0.4.1-rc.3: an NFSv3 export, mounted on the same system | Correct, after `modprobe nfsd nfsv3 cifs` |

`umount /volume1/sacd` reports "Device or resource busy" while a share serves the folder:
stop the share first.

## Compose

ADM's Docker package (Docker CE 25.0.5, with Compose v2.24.7) ran this file as a Compose
project. Put it in a folder of its own, for example `/volume1/docker/sacdfs/compose.yaml`.
Change `/volume1/music` to the folder that holds your images.

```yaml
services:
  sacdfs:
    image: ghcr.io/aclater/sacdfs:0.5.0
    container_name: sacdfs
    restart: unless-stopped
    devices:
      - /dev/fuse
    cap_add:
      - SYS_ADMIN
      - DAC_READ_SEARCH
    security_opt:
      - apparmor=unconfined
    ports:
      - "10445:445"
      - "12049:2049"
    healthcheck:
      test: ["CMD", "/usr/local/bin/healthcheck.sh"]
      interval: 30s
      timeout: 10s
      start_period: 300s
      retries: 3
    stop_grace_period: 20s
    volumes:
      - /volume1/music:/images:ro
      - sacdfs-cache:/cache
volumes:
  sacdfs-cache:
```

- **Ports 10445 and 12049.** ADM's own SMB service holds port 445, so the container's SMB and
  NFS are published on 10445 and 12049. Clients name the port:
  [linux-client.md](linux-client.md#on-other-ports).
- **`apparmor=unconfined`** is for NAS systems that use AppArmor. ADM has no AppArmor, and
  Docker accepted the option there anyway, so the same file can be used on both kinds.
- **The `healthcheck:` lines** make the health check work under any runtime, whatever format
  the image was saved in.

Then, in that folder:

```sh
docker compose up -d
docker inspect -f '{{.State.Health.Status}}' sacdfs
```

*You should see* `healthy` within a minute or so (31 to 40 s in the checks, the first time
including the download of the image). Before that,
`starting`. `docker logs sacdfs` shows the number of image files, `mounted`, the SMB share and
`NFS SERVER INITIALIZED`.

| Check | Result |
|---|---|
| `docker compose up -d`, the file exactly as above, with the published 0.5.0 image | Pulled and healthy 40 s after `up`; `sacdfs --version` in the container printed 0.5.0 |
| SMB on port 10445 with that image, mounted on the NAS with `mount.cifs` | All 22 files identical |
| The same file with an image built from `main` (same code) | Healthy after 32 s |
| SMB on port 10445, mounted on the NAS with `mount.cifs` (`port=10445,guest,ro,vers=3.0`) | All 22 files identical |
| `docker compose restart` | Back in 1 s, healthy 31 s later, all 22 files identical again |
| NFS | Not read: ADM's `mount` (BusyBox) has no NFSv4, and the VM had no second machine |

**How Docker was started in the VM, which is not a recommendation:** the App Central package
wouldn't install there (`apkg --install` exited with status 22 and no message). Docker was
started with the package's own `CONTROL/start-stop.sh start`. Without ADM's
`/usr/builtin/sbin` on the PATH it stopped with "iptables not found", and without the
package's `bin` folder with `invalid userland-proxy-path`. Whether App Central does all this on
a real NAS wasn't checked.

**Not checked:** the ADM Docker app's own screens (which settings it lets you set), a client
on another machine, NFS to the container, and anything on hardware.

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).
