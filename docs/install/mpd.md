# MPD (and moOde, Volumio): install sacdfs

MPD is a music player that runs in the background; moOde and Volumio are built on it. This guide lets it play your SACD images through sacdfs. About 10 minutes.

Checked on 0.6.0 with MPD 0.24.18 and mpc 0.35 (Fedora 44 packages, in a container, with a null audio output) against a sacdfs SMB mount on Fedora Linux. moOde and Volumio were not run *(untested)*, and the packages were not installed on a bare host or started with systemd *(untested)*.

## What you need

- sacdfs running and mounted on the machine where MPD runs, at `/srv/media/sacd`. Follow [docker-podman.md](docker-podman.md) (start it with `-e SACDFS_FORMATS=dsf -e SACDFS_FLATTEN=1` added to the `podman run` command), then [linux-client.md](linux-client.md), using `/srv/media/sacd` in place of `/mnt/sacd`.
- MPD and its command-line client: `sudo dnf install mpd mpc` (Fedora, RHEL) or `sudo apt install mpd mpc` (Debian, Ubuntu).
- A sound output (a DAC or sound card) set up in `mpd.conf`.

## Steps

1. **Check the mount.**

   ```sh
   ls /srv/media/sacd
   ```

   *You should see:* your artist or disc folders.

2. **Point MPD at it.** Open `/etc/mpd.conf` with `sudo nano /etc/mpd.conf` and set this line (it is already there; replace its value):

   ```
   music_directory "/srv/media/sacd"
   ```

   On moOde or Volumio, set the music source in their web settings instead *(untested)*.

3. **Start MPD.**

   ```sh
   sudo systemctl enable --now mpd
   ```

   *You should see:* `systemctl status mpd` says `active (running)`.

4. **Build the database.**

   ```sh
   mpc update --wait
   mpc stats
   ```

   *You should see:* `Songs:` with your track count.

## Check it worked

```sh
mpc list album
mpc add "Artist/Disc/Stereo/01 - Title.dsf"   # one of the paths from: mpc listall
mpc play
mpc status
```

Each multichannel area is its own album, "Album (Multichannel)". While a DSF track plays, MPD's status shows `audio: dsd64:2` (`dsd64:6` for 5.1).

## If something goes wrong

- **You see:** `mpc stats` shows 0 songs. **Do this:** the mount is missing, or the user MPD runs as cannot read it. Run `findmnt /srv/media/sacd`, then `sudo -u mpd ls /srv/media/sacd`. If `findmnt` shows nothing, mount it again as in [linux-client.md](linux-client.md); if only the second fails, the mount is private to another user.
- **You see:** a very slow first update, or every track twice. **Do this:** sacdfs is serving WAV too. Start sacdfs with `SACDFS_FORMATS=dsf` ([docker-podman.md](docker-podman.md)).
- **You see:** no sound but MPD says it plays. **Do this:** the `audio_output` in `mpd.conf` is not your device. Check `mpc outputs` and the ALSA section of MPD's manual.

## Details

### What was checked

- 0.6.0 (published image), MPD 0.24.18 and mpc 0.35 (Fedora 44 packages) in a rootless Podman container as the package's `mpd` user, with a null output; sacdfs in a rootful Podman container, shares mounted with SMB 3.1.1. A 4-disc test set (13 tracks): `mpc update` found all 13 in 6 albums (2 named "... (Multichannel)"), `albumart` returned the cover for the two areas checked (`cover.png` and `folder.png` sources), a 5-channel DSF showed `audio: dsd64:5`, and sacdfs restarted under the running container with all 13 files still listed.
- Earlier, on 0.4.0-rc.2 and 0.4.1-rc.1 with the 5-disc test set: first scan 0.34 s for 93 tracks with `dsf` against 3.6 s with `dsf,wav` (quiet desktop host), `dsd64:2` and `dsd64:6` playing, seeks working. Full numbers: [clients.md](../clients.md#mpd-moode-volumio).
- Not run: moOde and Volumio, a real sound output, DoP or native DSD to a DAC, MPD reading the SMB or NFS share on a NAS from another machine.

### Options and reasons

- **`SACDFS_FORMATS=dsf`.** MPD's FFmpeg probes about 5 MB of each WAV file, which sacdfs must convert: the first scan took 10 times as long with `dsf,wav`. If another client needs WAV, give it a second sacdfs container or mount with `dsf,wav`.
- **`SACDFS_FLATTEN=1`.** Each disc's `Stereo` and `Multichannel` folders then sit under the album.
- **Cache.** Leave `SACDFS_CACHE_MB` at its default (32 MiB, or 16 under a memory limit up to 512 MiB).
- **Art.** MPD looks only for files named `cover.*` in a track's folder. Since 0.4.1 sacdfs also shows a folder's cover as `cover.jpg` or `cover.png` inside each area folder, so no renaming is needed. A cover named `Cover.PNG` (other case) counts as a cover and is not aliased; untested with MPD.
- **MPD in a container.** Share the folder that holds the mount, not the mount point, with `ro,rslave` (`-v /srv/media:/media:ro,rslave`, and `--security-opt label=disable` on SELinux hosts); then use `/media/sacd` as `music_directory`. Sharing the mount point itself leaves the container on a dead mount after sacdfs restarts.
- DSF plays as DSD: DoP or native DSD to a capable output, otherwise MPD converts it.

### More problems

- After `mpc update` with nothing changed, MPD opened no files and took 0.07 s in the earlier tests.
