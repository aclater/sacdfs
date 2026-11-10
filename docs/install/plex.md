# Plex: install sacdfs

Plex Media Server is a media server with its own library. This guide lets it read your SACD images through sacdfs. About 20 minutes.

Not tested on 0.6.0. Plex was last checked on 0.4.1-rc.1 (Plex Media Server 1.43.5) over SMB on a NAS; the steps below are not re-run for this guide *(untested)*.

## What you need

- sacdfs running and mounted on the Linux machine where Plex will run, at `/srv/media/sacd`. Follow [docker-podman.md](docker-podman.md) (start it with `-e SACDFS_FLATTEN=1` added to the `podman run` command), then [linux-client.md](linux-client.md), using `/srv/media/sacd` in place of `/mnt/sacd`. Plex on the NAS itself: see Details.
- Podman or Docker on that machine, and a Plex account.

## Steps

1. **Check the mount.**

   ```sh
   ls /srv/media/sacd
   ```

   *You should see:* your artist or disc folders.

2. **Start Plex.** Share the folder that holds the mount (`/srv/media`), not the mount point itself, read-only with `rslave`. This keeps Plex working when sacdfs restarts.

   ```sh
   podman run -d --name plex --security-opt label=disable --network host \
     -v plex-config:/config -v /srv/media:/data:ro,rslave \
     docker.io/plexinc/pms-docker:latest
   ```

   With Docker, write `docker` for `podman` and drop `--security-opt label=disable`. *(untested)*

3. **Open Plex** at `http://localhost:32400/web`, sign in and finish its setup.

4. **Add the library.** **Add Library**, type **Music**, folder `/data/sacd`. Before you finish, open **Advanced** and turn on **Prefer local metadata**. Click **Add Library**.
   *You should see:* the scan starts and albums appear.

## Check it worked

Each multichannel disc is listed twice, as "Album" and "Album (Multichannel)", and tracks play.

## If something goes wrong

- **You see:** two albums with the same name for a multichannel disc. **Do this:** turn on **Prefer local metadata** (library settings, Advanced), then run **Refresh All Metadata**. Without it Plex names both copies after the release it matches online.
- **You see:** no albums. **Do this:** run `podman exec plex ls /data/sacd`. If it lists nothing, the mount is missing on the host (step 1) or `/srv/media` was shared without `rslave`.
- **You see:** the NAS or host busy every night. **Do this:** that is Plex's nightly deep analysis reading every file once; turn it off in Plex's scheduled tasks if CPU is short.

## Details

### What was checked

- 0.1.1: Plex Media Server 1.43.5 as an app on the same NAS over SMB (CIFS, read-only): 406 of 406 tracks found (35 images), codec shown as DSD, durations within 12 ms of the TOC; a sacdfs restart was recovered by Plex's CIFS mount by itself.
- 0.2.0-rc.4: the whole 291-image tree scanned without errors (3,216 tracks).
- 0.4.1-rc.1: 5,132 tracks in 466 albums; upgrading from 0.4.0 named all 279 untitled tracks; rolling back and forward restored every count. Playback through a Plex client was not tested on that candidate.
- Not run: Plex on 0.6.0, and 20 of the 291 images were not seen in one run (cause not found). Full results: [clients.md](../clients.md#plex).

### Options and reasons

- **`SACDFS_FLATTEN=1`.** Each disc's `Stereo` and `Multichannel` folders then sit under the album. Leave `SACDFS_FORMATS` at its default.
- **Cache.** The Plex tests ran with `SACDFS_CACHE_MB=16`; the default (32 MiB, or 16 under a memory limit up to 512 MiB) is close, measured at 48 MiB peak for a Plex-like scan.
- **Prefer local metadata.** Without it Plex's default agent gives both copies of a multichannel disc the online release's title ([#138](https://github.com/aclater/sacdfs/issues/138)); the "Plex Personal Media" agent also works.
- **Share the folder that holds the mount, with `ro,rslave`.** Sharing the mount point itself leaves the container on a dead mount after sacdfs restarts.
- **Plex as an app on the same TrueNAS.** In the Plex app's storage add an SMB (CIFS) entry, read-only, with sacdfs's address, share `sacd`, and `SMB_USER` and its password. Plex reaches port 445 on sacdfs's own Docker network address, as in [roon.md](roon.md). Mount it at, say, `/data/sacd`, then do steps 4 onward.

### More problems

- Plex does not use sacdfs's folder art; it fetches art online ([#95](https://github.com/aclater/sacdfs/issues/95)).
- Plex's nightly deep analysis read every file once in one test (55 GiB in 874 s, 701 MiB RSS).
- Three albums (26 tracks) showed a blank album title in Plex on 0.4.1-rc.1 although sacdfs serves one; the cause was not found.
