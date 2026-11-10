# Docker or Podman: install sacdfs

This starts sacdfs in a container on a Linux computer. It shares your SACD discs over SMB and
NFS, so any player on your network can see them as DSF and WAV files. About 10 minutes.

Checked on sacdfs 0.6.0 with rootful Podman and with Docker on Fedora 45. Steps marked
*(untested)* were not run on 0.6.0. See [Details](#details).

## What you need

- A Linux computer with Podman or Docker, and a terminal on it.
- A folder with your SACD images (`.iso` files). The examples use `/srv/music`.
- Ports 445 (SMB) and 2049 (NFS) not used by anything else on that computer.

## Steps

1. **Check that the ports are free.** Type:

   ```sh
   sudo ss -ltn | grep -E ':(445|2049) ' || echo free
   ```

   *You should see* `free`. If you see lines instead, something already uses these ports (often the
   computer's own file sharing). Use [other ports](#other-ports) in step 2.

2. **Start the container.** Replace `/srv/music` with your folder. Use the block for the tool you
   have. Run it as root (`sudo`): sharing over NFS needs it.

   Podman:

   ```sh
   sudo podman run -d --name sacdfs --restart unless-stopped \
     --device /dev/fuse --cap-add SYS_ADMIN --cap-add DAC_READ_SEARCH \
     --security-opt label=disable \
     -p 445:445 -p 2049:2049 \
     -v /srv/music:/images:ro \
     -v sacdfs-cache:/cache \
     --health-cmd /usr/local/bin/healthcheck.sh --health-interval 30s \
     --health-timeout 10s --health-start-period 300s --health-retries 3 \
     ghcr.io/aclater/sacdfs:0.6.0
   ```

   Docker:

   ```sh
   sudo docker run -d --name sacdfs --restart unless-stopped \
     --device /dev/fuse --cap-add SYS_ADMIN --cap-add DAC_READ_SEARCH \
     --security-opt label=disable \
     -p 445:445 -p 2049:2049 \
     -v /srv/music:/images:ro \
     -v sacdfs-cache:/cache \
     ghcr.io/aclater/sacdfs:0.6.0
   ```

   *You should see* a long hexadecimal ID. On Debian or Ubuntu, write `--security-opt apparmor=unconfined`
   in place of `--security-opt label=disable`. `:ro` keeps your discs read-only.

3. **Wait for it to be healthy.** Type this every few seconds, until it says `healthy` (up to a minute;
   a big library on a slow disk takes longer):

   ```sh
   sudo podman inspect -f '{{.State.Health.Status}}' sacdfs
   ```

   With Docker, write `docker` for `podman`. *You should see* `starting`, then `healthy`.

4. **Mount the share on the computer you listen at.** Follow [linux-client.md](linux-client.md)
   for Linux, or the [deployment guide](../deployment.md#mounting-the-shares-on-clients) for
   Windows and macOS. Use the server's name or address for `nas` there.

## Check it worked

Open the mounted share in your file manager or type `ls /mnt/sacd`. You should see one folder per
disc. Each holds `Stereo` and, on multichannel discs, `Multichannel`, with `.dsf` and `.wav` files.
Open one in your player.

To see the server's own view:

```sh
sudo podman logs sacdfs 2>&1 | grep -E 'image files|mounted|NFS SERVER INITIALIZED'
```

It shows how many image files it found, `mounted`, and the NFS server starting.

## If something goes wrong

- **You see** `cannot bind tcp port :445: address already in use` (Docker words it a little
  differently). **Do this:** something else uses that port. Remove the half-made container
  (`sudo podman rm -f sacdfs`) and use [other ports](#other-ports).
- **You see** `nil pointer evaluating *define.HealthCheckResults.Status` from `podman inspect`.
  **Do this:** the container was started without the five `--health-*` lines. Remove it
  (`sudo podman rm -f sacdfs`) and run step 2 again with them. (Docker needs none.)
- **You see** `sacdfs: /images: Permission denied` in `sudo podman logs sacdfs`, or the container
  stops at once. **Do this:** the security setting in step 2 doesn't fit your system. Use
  `label=disable` on Fedora and RHEL, and `apparmor=unconfined` on Debian and Ubuntu. Remove the
  container and run step 2 again.

More: [Details](#more-problems).

## Stopping, restarting and upgrading

**Never stop it with `kill -9` or `docker kill`.** That leaves a dead mount behind. Use:

```sh
sudo podman stop sacdfs
sudo podman start sacdfs
```

To upgrade, stop and remove the container (`sudo podman rm sacdfs`; the cache and your discs stay),
then run step 2 with the new version tag. For about 5 to 8 seconds SMB and NFS stop answering.
Linux and Windows clients carry on by themselves. **A macOS client loses the share and must mount it
again.**

## Details

### Other ports

If ports 445 and 2049 are taken, publish others, for example `-p 10445:445 -p 12049:2049` in step 2.
Clients then name the ports when they mount: see [linux-client.md](linux-client.md#on-other-ports).

### Options

- **`label=disable`** is for SELinux hosts (Fedora, RHEL): it leaves your folder's labels alone.
- **A password on the SMB share:** add `-e SMB_USER=music -e SMB_PASSWORD=...` to step 2. Windows 11
  needs one: it refuses guest shares. Without these two, the shares are open to the whole network, read-only.
- **Music servers that scan with FFmpeg** (Jellyfin, MPD) and **Roon:** add `-e SACDFS_FORMATS=dsf`.
  See [your player's guide](README.md#3-which-player-or-music-server).
- **Who may mount over NFS:** `-e NFS_CLIENTS=192.168.1.0/24` (default: everyone).
- **The `--health-*` lines:** the published image is in OCI format, which has no field for a health
  check, so Podman needs them. Docker reads the check from the image.
- **Rootless Podman** works for SMB only: add `-e NFS=0` and the `--health-*` options (NFS needs root,
  see [privileges](../deployment.md#privileges)).
- **Keep the SMB server's identity across a recreate:** add `--hostname sacdfs` ([SMB and restarts](../deployment.md#smb-and-restarts)).
- **Memory limit** (`--memory`): sacdfs sets its cache, decoder threads and listings from it. See
  [under a memory limit](../deployment.md#under-a-memory-limit).
- **Every setting and the reasons:** [deployment guide](../deployment.md).

### Compose

[deploy/nas-compose.yaml](../../deploy/nas-compose.yaml) is the same container as a Compose file, for any
host with Docker Compose or `podman compose`, and the file every NAS guide points to. It uses the published
image and publishes ports 10445 (SMB) and 12049 (NFS). Edit the one marked line, the path of your images; a
comment on each setting says what it is for. It carries the health check, so Podman needs no `--health-*`
options. On SELinux hosts, uncomment `label=disable`.

```sh
docker compose -f nas-compose.yaml up -d      # or: sudo podman compose -f nas-compose.yaml up -d
docker compose -f nas-compose.yaml ps         # healthy, within a minute
```

Checked with the published 0.5.0 image on a 457-track corpus: Docker Compose 5.7.0 against rootful Podman
(healthy after 33 s; SMB and NFSv4.2 mounts each matched the corpus manifest for all 457 tracks), against
rootless Podman on a second host (SMB only), and Docker Compose 2.24.7 on ASUSTOR ADM in a VM
([asustor.md](asustor.md)). Not re-run on 0.6.0.

### Stopping and restarting

With nothing running in it, `stop` and `restart` take under a second: the entry point stops the servers,
then sacdfs. Open files over SMB and NFS do not delay it. `stop_grace_period` in the Compose file is 30 s:
the entry point waits up to 10 s for the servers to let go of the mount, and sacdfs then exits within
`SACDFS_STOP_GRACE` (10 s) even if a process on the same computer still holds a file open (#276).
Raise the period if you raise the grace. In 0.5.0 and earlier, a stop within a few seconds of the start
sometimes took 10 s or more, and Podman, whose default stop timeout is 10 s, killed the container
(`StopSignal SIGTERM failed to stop container ... resorting to SIGKILL`); the container came back healthy
either way. The entry point of 0.6.0 no longer waits for a mount that is already gone. Why macOS must
mount again after a restart: [SMB and restarts](../deployment.md#smb-and-restarts).

### What was checked

- **0.6.0, steps 1 to 4 as written** with rootful Podman 6.1.3 on Fedora 45 (SELinux): healthy after
  about 40 s. NFS 4.2 and SMB 3.1.1 clients on the same host read all 22 files of two test images,
  identical to a reference made with the release binary. `podman restart` and `podman stop` each took
  under half a second.
- **0.6.0 with Docker** on the same host (the Docker block of step 2): healthy in about 10 s, an NFS mount listed all 22 files, `docker stop` took 0.3 s.
- The three problems above, as shown, were reproduced on 0.6.0 (port in use, no health options, no
  security option).
- Earlier: 0.5.0 under rootful and rootless Podman (rootless over SMB only), 0.4.0 with Docker 29 on
  Debian 13 (`apparmor=unconfined`), and Compose with Docker Compose 5.7.0 against Podman, and on
  ASUSTOR ADM ([asustor.md](asustor.md)). `podman-compose` was not run.
- Not checked: rootless Podman for NFS, and a macOS client re-mounting is from VMs on Apple hardware
  with no later re-test.

### More problems

- **`mount error(111): could not connect to ::1`** when mounting SMB on the server's own computer:
  harmless. The client tries IPv6 first, then connects.
- **`exited (needs --device /dev/fuse and CAP_SYS_ADMIN)`** (0.5.0 and earlier), or from 0.6.0
  `the container has no /dev/fuse`: add the `--device` and `--cap-add` lines of step 2.
- **`port 445 (SMB) is already in use here`** (0.6.0, with host networking): publish other ports.
- **Podman warns `StopSignal SIGTERM failed to stop container ... resorting to SIGKILL`** (0.5.0 and
  earlier, when stopped within seconds of the start): fixed in 0.6.0. See the
  [stop and restart notes](../deployment.md#smb-and-restarts).
