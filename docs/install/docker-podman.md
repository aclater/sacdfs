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

Run again as written above with the 0.5.0 code (an image built from `main` that differs from
the release only in its version number), under rootful
Podman 6.1.3 on Fedora 45: healthy after 31 s; NFS 4.2 and SMB clients on the same host read
all 22 files of two test images identical to the reference; `podman restart` brought it back
healthy (Podman warned that SIGTERM hadn't stopped the container within its default 10 s and
used SIGKILL).

Compose: [openmediavault.md](openmediavault.md) has a validated Compose file, and
[asustor.md](asustor.md#compose) one for other ports with the health check written in. Not validated
here: rootless Podman (SMB only; NFS needs root, see the
[deployment guide](../deployment.md#privileges)).
