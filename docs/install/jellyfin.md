# Jellyfin: install sacdfs

Jellyfin is a free media server. This guide lets it play your SACD images through sacdfs. About 15 minutes.

Checked on 0.6.0 with Jellyfin 12.4.0 (official image) in rootless Podman on Fedora Linux. The library was set up through Jellyfin's web API; the clicks in step 4 were not run in a browser *(untested)*.

## What you need

- sacdfs running and mounted on the Linux machine where Jellyfin will run, at `/srv/media/sacd`. Follow [docker-podman.md](docker-podman.md) (start it with `-e SACDFS_FORMATS=dsf -e SACDFS_FLATTEN=1` added to the `podman run` command), then [linux-client.md](linux-client.md), using `/srv/media/sacd` in place of `/mnt/sacd`.
- Podman or Docker on that machine.
- A web browser.

## Steps

1. **Check the mount.**

   ```sh
   ls /srv/media/sacd
   ```

   *You should see:* your artist or disc folders.

2. **Start Jellyfin.** The folder that holds the mount (`/srv/media`) is shared into the container, not the mount point itself, with `ro,rslave`. This keeps Jellyfin working when sacdfs restarts.

   ```sh
   podman run -d --name jellyfin --security-opt label=disable -p 8096:8096 \
     -v jellyfin-config:/config -v jellyfin-cache:/cache \
     -v /srv/media:/media:ro,rslave \
     docker.io/jellyfin/jellyfin:latest
   ```

   *You should see:* `podman ps` lists `jellyfin` as `Up`, `healthy` after about a minute. With Docker, write `docker` for `podman` and drop `--security-opt label=disable`.

3. **Open Jellyfin** at `http://localhost:8096` and finish its first-run wizard (language, an admin user and password).

4. **Add the library.** Dashboard, Libraries, **Add Media Library**. Content type **Music**, then **Folders**, **+**, and type `/media/sacd`. Under the library's settings leave **LUFS audio normalization** off. Click **OK**.
   *You should see:* a scan starts and the albums appear.

## Check it worked

Open **Music** on the home page. Each disc is an album; a disc with a multichannel area also shows "Album (Multichannel)". Click a track to play it.

## If something goes wrong

- **You see:** no albums after the scan. **Do this:** run `podman exec jellyfin ls /media/sacd`. If it lists nothing, the mount is missing on the host (step 1) or `/srv/media` was shared without `rslave`. Fix the mount, then run Dashboard, Libraries, **Scan All Libraries**.
- **You see:** every track twice, or a very slow first scan. **Do this:** sacdfs is serving WAV too. Start sacdfs with `SACDFS_FORMATS=dsf` ([docker-podman.md](docker-podman.md)).
- **You see:** a SELinux "Permission denied" in Jellyfin's log. **Do this:** keep `--security-opt label=disable` in step 2.

## Details

### What was checked

- 0.6.0 (published image), Jellyfin 12.4.0, rootless Podman on Fedora; sacdfs in a rootful Podman container, shares mounted with SMB 3.1.1. A 4-disc test set (13 tracks): all 13 found, 6 albums (2 named "... (Multichannel)"), cover art picked up for 4 albums, a stream and an MP3 transcode returned data, and sacdfs restarted under the running container with all 13 files still listed. The library was created and scanned through the API.
- Earlier, on 0.4.0-rc.2 and 0.4.1 with the 5-disc test set: 93 of 93 tracks with `dsf` (186 of 186 with `dsf,wav`), transcoded playback and seeks. Full numbers: [clients.md](../clients.md#jellyfin).
- Not run: Jellyfin reading the SMB or NFS share on a NAS from another machine, and the web-interface clicks.

### Options and reasons

- **`SACDFS_FORMATS=dsf`.** Jellyfin's ffprobe reads about 7 MB of each WAV file, which sacdfs must convert: 648.5 MiB served for the first scan against 17.9 MiB with `dsf`.
- **`SACDFS_FLATTEN=1`.** Each disc's `Stereo` and `Multichannel` folders then sit under the album.
- **Cache.** Leave `SACDFS_CACHE_MB` at its default (32 MiB, or 16 under a memory limit up to 512 MiB).
- **LUFS normalization off.** With it on, Jellyfin read the library about twice over (40.0 GiB from a 22.0 GiB library), and sacdfs had to decode all of it.
- **Share the folder that holds the mount, with `ro,rslave`.** Sharing the mount point itself leaves the container on a dead mount after sacdfs restarts. `label=disable` is for SELinux hosts.
- Run Jellyfin as the same user that made the mount if you mount sacdfs directly (without Docker or Podman for sacdfs): see [user-guide.md](../user-guide.md).

### More problems

- A first scan right after Jellyfin's first start may show 0 albums if the library is added while Jellyfin's own startup scan runs. Wait until Dashboard shows no running tasks, then scan again.
