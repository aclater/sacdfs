# Navidrome: install sacdfs

Navidrome is a small music server for your own library. This guide lets it play your SACD images through sacdfs. About 10 minutes.

Checked on 0.6.0 with Navidrome 0.64.2 (official image) in rootless Podman on Fedora Linux. The scan and album list were checked through Navidrome's API; the web page itself was not opened in a browser *(untested)*.

## What you need

- sacdfs running and mounted on the Linux machine where Navidrome will run, at `/srv/media/sacd`. Follow [docker-podman.md](docker-podman.md) (start it with `-e SACDFS_FORMATS=dsf -e SACDFS_FLATTEN=1` added to the `podman run` command), then [linux-client.md](linux-client.md), using `/srv/media/sacd` in place of `/mnt/sacd`.
- Podman or Docker on that machine.
- A web browser.

## Steps

1. **Check the mount.**

   ```sh
   ls /srv/media/sacd
   ```

   *You should see:* your artist or disc folders.

2. **Start Navidrome.** The folder that holds the mount (`/srv/media`) is shared into the container, not the mount point itself, with `ro,rslave`. This keeps Navidrome working when sacdfs restarts.

   ```sh
   podman run -d --name navidrome --security-opt label=disable -p 4533:4533 \
     -v navidrome-data:/data -v /srv/media:/media:ro,rslave \
     -e ND_MUSICFOLDER=/media/sacd \
     docker.io/deluan/navidrome:latest
   ```

   *You should see:* `podman ps` lists `navidrome` as `Up`. With Docker, write `docker` for `podman` and drop `--security-opt label=disable`.

3. **Open Navidrome** at `http://localhost:4533` and create the admin user when it asks. It scans the folder by itself.
   *You should see:* albums within a few seconds.

## Check it worked

Open **Albums**. Each disc is an album; a multichannel area is its own album, "Album (Multichannel)". Click a track to play it. A second scan with nothing changed reads nothing from sacdfs.

## If something goes wrong

- **You see:** no albums. **Do this:** run `podman exec navidrome ls /media/sacd`. If it lists nothing, the mount is missing on the host (step 1) or `/srv/media` was shared without `rslave`. Fix it, then start a full scan from Navidrome's Activity panel *(untested)* or wait for its hourly scan.
- **You see:** the page asks to create a user again after a restart. **Do this:** keep the `-v navidrome-data:/data` line from step 2; without it the database is lost with the container.
- **You see:** a SELinux "Permission denied" in `podman logs navidrome`. **Do this:** keep `--security-opt label=disable` in step 2.

## Details

### What was checked

- 0.6.0 (published image), Navidrome 0.64.2, rootless Podman on Fedora; sacdfs in a rootful Podman container, shares mounted with SMB 3.1.1. A 4-disc test set (13 tracks): all 13 found in 6 albums (2 named "... (Multichannel)"), cover art for 4 albums, a full scan took 0.24 s, and sacdfs restarted under the running container with all 13 files still listed. The admin user and scan were done through the API.
- Earlier, on 0.4.0-rc.2 and 0.4.1 with the 5-disc test set: 93 of 93 tracks (186 of 186 with `dsf,wav`), folder art used, renamed and removed images followed, streams and transcodes played. Full numbers: [clients.md](../clients.md#navidrome).
- Not run: Navidrome reading the SMB or NFS share on a NAS from another machine, and the web-interface clicks.

### Options and reasons

- **`SACDFS_FORMATS=dsf`** (or `dsf,wav` for both): both worked (186 of 186 with `dsf,wav`). `dsf` alone is smaller and faster.
- **`SACDFS_FLATTEN=1`.** Each disc's `Stereo` and `Multichannel` folders then sit under the album.
- **Cache.** Leave `SACDFS_CACHE_MB` at its default (32 MiB, or 16 under a memory limit up to 512 MiB).
- **Share the folder that holds the mount, with `ro,rslave`.** Sharing the mount point itself leaves the container on a dead mount after sacdfs restarts. `label=disable` is for SELinux hosts.
- **`ND_MUSICFOLDER`** points Navidrome at the music folder; its default is `/music`.
- Keep the default "(Multichannel)" album suffix if you want Stereo and Multichannel as separate albums.

### More problems

- With a direct sacdfs mount (no Docker or Podman for sacdfs), run Navidrome in rootless Podman as the user who made the mount: see [user-guide.md](../user-guide.md).
