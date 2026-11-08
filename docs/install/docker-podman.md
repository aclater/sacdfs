# sacdfs with Docker or Podman (any Linux host)

**Validated** with sacdfs 0.4.0 (a build of `main`) under rootful Podman on Fedora 44: the
container turned healthy in about 30 s; NFS and SMB clients read every DSF track of five test
discs, matching the reference hashes (93 of 93 over each), and a Fedora 44 VM mounted both
shares. The same command with `docker run` and `apparmor=unconfined` (step 3) was validated
with Docker 29 on Debian 13: healthy in 8 s, both shares mounted. Reference:
[deployment guide](../deployment.md).

1. Pick the folder that holds your SACD images (`.iso`), for example `/srv/music`.
2. Make sure nothing on the host already uses ports 445 (SMB) and 2049 (NFS):

   ```sh
   sudo ss -ltn | grep -E ':(445|2049) ' || echo free
   ```

   If they are taken, publish other ports instead (`-p 10445:445 -p 12049:2049`) and see
   [linux-client.md](linux-client.md#on-other-ports).
3. Start the container (as root: NFS needs a rootful container). With Docker, write
   `docker run` for `sudo podman run`:

   ```sh
   sudo podman run -d --name sacdfs --restart unless-stopped \
     --device /dev/fuse --cap-add SYS_ADMIN --cap-add DAC_READ_SEARCH \
     --security-opt label=disable \
     -p 445:445 -p 2049:2049 \
     -v /srv/music:/images:ro \
     -v sacdfs-cache:/cache \
     --health-cmd /usr/local/bin/healthcheck.sh --health-interval 30s \
     --health-timeout 10s --health-start-period 300s --health-retries 3 \
     ghcr.io/aclater/sacdfs:0.4.0
   ```

   - The four `--health-*` options give Podman the health check. The published image is in OCI
     format, which has no field for one, so without them Podman has no health check and
     `podman inspect` below fails. Docker reads the check from the image: leave the options out
     there.
   - `label=disable` is for SELinux hosts (Fedora, RHEL); it leaves your folder's labels alone.
   - On AppArmor hosts (Debian, Ubuntu) use `--security-opt apparmor=unconfined` instead.
   - For a password on the SMB share, add `-e SMB_USER=music -e SMB_PASSWORD=...`. Windows 11
     needs one: it refuses guest shares.
   - For music servers that scan with FFmpeg (Jellyfin, MPD) or for Roon, add
     `-e SACDFS_FORMATS=dsf`; see your server's guide.

## Check it works

```sh
sudo podman inspect -f '{{.State.Health.Status}}' sacdfs     # healthy, within a minute (needs the --health-* options)
sudo podman logs sacdfs 2>&1 | grep -E 'memory limit|image files|mounted|NFS SERVER INITIALIZED'
```

The log shows the number of image files, `mounted`, and the SMB and NFS servers starting. With
`--memory` set, it also shows what sacdfs set from the limit: cache, decoder threads and listings
([Under a memory limit](../deployment.md#under-a-memory-limit)). Then mount it from a client:
[linux-client.md](linux-client.md).

Rootless Podman, SMB only (`-e NFS=0`; the `--health-*` options as above), was run with the
0.4.1-rc.1 image: `podman inspect` showed `healthy` within 25 s. Without the options it fails
with a nil-pointer error, because the image has no health check for Podman to run.

Not validated here: rootless Podman for NFS (NFS needs root, see the
[deployment guide](../deployment.md#privileges)).

## Compose

[deploy/nas-compose.yaml](../../deploy/nas-compose.yaml) is the same container as a Compose file,
for any host with Docker Compose or `podman compose`, and the file every NAS guide points to. It
uses the published `ghcr.io/aclater/sacdfs:0.5.0` image and publishes ports 10445 (SMB) and 12049
(NFS). The one line to edit is the path of your images; a comment on each setting says what it is
for. It carries the health check, so Podman needs no `--health-*` options. On SELinux hosts uncomment
`label=disable`.

```sh
docker compose -f nas-compose.yaml up -d      # or: sudo podman compose -f nas-compose.yaml up -d
docker compose -f nas-compose.yaml ps         # healthy, within a minute
```

Checked with the published 0.5.0 image, on its 457-track test corpus:

- Docker Compose 5.7.0 against rootful Podman 6.1 on Fedora (images path changed to the corpus,
  `label=disable` added): `config` accepted the file, the container was healthy 33 s after `up`,
  and an SMB mount (port 10445, `vers=3.1.1`) and an NFSv4.2 mount (port 12049) on the same host
  each matched the corpus manifest for all 457 tracks and were byte-identical to each other (914
  files, SHA-256).
- Docker 25.0.5 with Compose 2.24.7 on ASUSTOR ADM, in a VM: [asustor.md](asustor.md).
- Docker Compose 5.7.0 against rootless Podman 6.1 on a second Fedora host (SELinux, so with
  `label=disable`): healthy in 31 s, six files picked across the corpus read over SMB on port 10445
  with `smbclient` had the right SHA-256, and `restart` and `stop` took under a second. NFS was not
  mounted there (a mount needs root).
- `podman-compose` was not run.

### Stopping and restarting

With nothing running in it, `stop` and `restart` take under a second: the entry point stops the
servers, then sacdfs. Open files over SMB and NFS do not delay it (checked on Podman with one file
held open over each): the servers close them. `stop_grace_period` in the file is 20 s, the most the entry
point can wait (10 s for the servers to let go of the mount, 10 s for the unmount).

In 0.5.0 and earlier, a stop within a few seconds of the start (a restart right after `up`, say)
sometimes took 10 s or more: 11 of 60 stops 2 s after the start on Fedora with Podman, and 3 of 15
on ADM with Docker. Podman, whose default stop timeout is 10 s, then killed the container and warned
"StopSignal SIGTERM failed to stop container ... resorting to SIGKILL"; Docker with the file's 20 s
waited it out and the container exited normally (status 0). The cause is in the entry point: it waits
up to 10 s for the servers to let go of the mount, with `fuser -m`, and in those runs the mount was
already gone (sacdfs had received its own SIGTERM and unmounted; what sent it was not found), so
`fuser -m` listed every process on the container's root filesystem and the wait always ran out. The
entry point now waits only while the mount exists. With it (the fixed script mounted over the 0.5.0
image), none of the stops took more than a second: 0 of 60 on Fedora and 0 of 15 on ADM. The
image carries the fix from the release after 0.5.0; until then `stop_grace_period: 20s` in the file
makes the slow stop a clean exit instead of a kill. The container came back healthy either way.

