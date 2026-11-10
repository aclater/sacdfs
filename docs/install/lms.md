# Lyrion Music Server (Logitech Media Server): install sacdfs

Lyrion Music Server (LMS) streams your music to Squeezebox-type players. This guide lets it play your SACD images through sacdfs. About 15 minutes.

Checked on 0.6.0 with Lyrion 9.1.1 (the `lmscommunity/lyrionmusicserver` image) in rootless Podman on Fedora Linux. The setting was saved by posting the settings page's own form, and the scan was started through LMS's JSON-RPC interface; the clicks in step 3 were not run in a browser, and no player was connected on 0.6.0 *(untested)*.

## What you need

- sacdfs running and mounted on the Linux machine where Lyrion will run, at `/srv/media/sacd`. Follow [docker-podman.md](docker-podman.md) (start it with `-e SACDFS_FORMATS=dsf -e SACDFS_FLATTEN=1` added to the `podman run` command), then [linux-client.md](linux-client.md), using `/srv/media/sacd` in place of `/mnt/sacd`.
- Podman or Docker on that machine.
- A player that can play DSD (see Details). A web browser.

## Steps

1. **Check the mount.**

   ```sh
   ls /srv/media/sacd
   ```

   *You should see:* your artist or disc folders.

2. **Start Lyrion.** The folder that holds the mount (`/srv/media`) is shared into the container, not the mount point itself, with `ro,rslave`. This keeps Lyrion working when sacdfs restarts.

   ```sh
   podman run -d --name lyrion --security-opt label=disable -p 9000:9000 \
     -v lyrion-config:/config -v /srv/media:/media:ro,rslave \
     docker.io/lmscommunity/lyrionmusicserver:latest
   ```

   *You should see:* `podman ps` lists `lyrion` as `Up`. With Docker, write `docker` for `podman` and drop `--security-opt label=disable`.

3. **Set the music folder.** Open `http://localhost:9000`, then Settings (the gear), **Basic Settings**, **Media Folders**. Replace `/music` with `/media/sacd` and click **Apply**.
   *You should see:* the page shows the new folder.

4. **Scan.** On the same page, in the list next to the **Rescan** button choose **Clear library and rescan everything**, then click **Rescan**.
   *You should see:* the scan ends within a minute or so.

## Check it worked

Open **My Music**, **Albums**. Each disc is an album; a multichannel area is its own album, "Album (Multichannel)". Pick a player (top right) and play a track.

## If something goes wrong

- **You see:** no albums after the scan. **Do this:** run `podman exec lyrion ls /media/sacd`. If it lists nothing, the mount is missing on the host (step 1) or `/srv/media` was shared without `rslave`. Check that Media Folders (step 3) says `/media/sacd`, then rescan.
- **You see:** a track starts and stops at once, or nothing plays. **Do this:** DSF files are passed to the player as DSD, so the player must play DSD. A stereo-only player (squeezelite) stops on 5.1 files. See Details.
- **You see:** "Permission denied" for `/media/sacd` in `podman logs lyrion`. **Do this:** you probably mounted sacdfs directly instead of with Docker or Podman; see Details for the root setting.

## Details

### What was checked

- 0.6.0 (published image), Lyrion 9.1.1, rootless Podman on Fedora; sacdfs in a rootful Podman container, shares mounted with SMB 3.1.1. A 4-disc test set (13 tracks): all 13 found in 6 albums (2 named "... (Multichannel)"), artwork for 4 albums, and sacdfs restarted under the running container with all 13 files still listed. No player was run, so playback was not checked on 0.6.0.
- Earlier, on 0.4.0-rc.2 and 0.4.1 with the 5-disc test set and squeezelite 2.0 (with DSD support): 93 of 93 tracks with `dsf` (186 of 186 with `dsf,wav`), first scan 1.1 s, a rescan with nothing changed read nothing, native DSD playback. Full numbers: [clients.md](../clients.md#lyrion--logitech-media-server).
- Not run: Lyrion reading the SMB or NFS share on a NAS from another machine, a DSD plugin for players without DSD, and the web-interface clicks.

### Options and reasons

- **`SACDFS_FORMATS=dsf`** (or `dsf,wav`): both worked.
- **`SACDFS_FLATTEN=1`.** Each disc's `Stereo` and `Multichannel` folders then sit under the album.
- **Cache.** Leave `SACDFS_CACHE_MB` at its default (32 MiB, or 16 under a memory limit up to 512 MiB).
- **Share the folder that holds the mount, with `ro,rslave`.** Sharing the mount point itself leaves the container on a dead mount after sacdfs restarts. `label=disable` is for SELinux hosts.
- **Players.** The stock `convert.conf` passes DSF through unchanged, so the player must play DSD. A player without DSD gets nothing unless a DSD plugin converts it (not tested). squeezelite is stereo only, so it stopped on 5.1 files.
- **A direct mount instead of Docker or Podman.** The image's own user (uid 499) cannot read a FUSE mount made without `allow_other`. For a rootless container reading such a mount, add `-e PUID=0 -e PGID=0 -e EXTRA_ARGS="--user root"` to step 2 (root in a rootless container is the mount's owner). Both `PUID=0` and `EXTRA_ARGS` are needed: with `PUID=0` alone LMS falls back to `nobody` and cannot write its `/config`. This was run on 0.6.0 and LMS scanned all 13 files. With the SMB mount of the Docker or Podman guide the default user works, which was also run on 0.6.0.

### More problems

- `podman stop lyrion` waited the full timeout and then killed it (Podman's warning "resorting to SIGKILL"). That is Lyrion's container, not sacdfs; add `-t 30` to `podman stop` or accept it.
