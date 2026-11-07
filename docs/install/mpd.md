# MPD (and moOde, Volumio)

**Tested** with MPD 0.24.15 (Fedora 44 package, in a container) on sacdfs 0.4.0-rc.2 (and
0.3.0-rc.1), reading a sacdfs mount on its host: first scan 0.34 s for 93 tracks with `dsf`,
against 3.6 s with `dsf,wav` (on a quiet desktop host); DSF played as DSD, stereo and 5.1,
and seeks worked. moOde and Volumio are built on MPD but were not run themselves. Details: [clients](../clients.md#mpd-moode-volumio).

## sacdfs settings

| Setting | Value | Why |
|---|---|---|
| `SACDFS_FORMATS` (`--format`) | `dsf` | MPD's FFmpeg probes about 5 MB of each WAV file, which sacdfs must convert: the first scan took 10 times as long with `dsf,wav` |
| `SACDFS_FLATTEN` (`--flatten`) | `1` | each disc's `Stereo` and `Multichannel` folders sit under the album |

Leave `SACDFS_CACHE_MB` (`--cache-mb`) at its default (32 MiB, or 16 under a memory limit up to 512 MiB; the 0.4.0-rc.2 tests ran with 32, the 0.3.0-rc.1 tests with 16). If another
client needs WAV, give it a second sacdfs container or mount with `dsf,wav`.

## Add the share

1. Mount sacdfs on the host, for example at `/srv/media/sacd`
   ([user guide](../user-guide.md); the tests ran `sacdfs --format dsf --flatten` as the
   user). Run the server in rootless Podman as that same user, and bind the folder **that
   holds** the mount, read-only with `rslave`, so that a restart of sacdfs shows up inside
   the container (binding the mount point itself leaves the container on a dead mount):

   ```sh
   podman run ... --security-opt label=disable -v /srv/media:/media:ro,rslave ...
   ```

   (Compose: `- /srv/media:/media:ro,rslave`.) `label=disable` is for SELinux hosts.
2. In `mpd.conf`: `music_directory "/media/sacd"`, then `mpc update`.
3. **Art:** MPD looks only for files named `cover.*` next to the tracks. Name the cover next
   to each image `cover.jpg` (or `.png`); sacdfs copies it into each area folder.

## Check it works

`mpc stats` counts every track; `mpc list album` shows each multichannel area as its own
album, "… (Multichannel)". While a DSF track plays, MPD's status shows `audio: dsd64:2` (or
`dsd64:6` for 5.1).

Not validated: MPD reading the NAS's SMB or NFS share.
