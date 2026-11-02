# Jellyfin

**Tested** with Jellyfin 12.4.0 (official image) on sacdfs 0.3.0-rc.1, in a container reading
a sacdfs mount on its host: 93 of 93 tracks with `dsf` (186 of 186 with `dsf,wav`), the
multichannel areas as their own albums, folder art used, transcoded playback and seeks.
Details: [clients](../clients.md#jellyfin).

## sacdfs settings

| Setting | Value | Why |
|---|---|---|
| `SACDFS_FORMATS` (`--format`) | `dsf` | Jellyfin's ffprobe reads about 7 MB of each WAV file, which sacdfs must convert: 648.5 MiB served for the first scan against 17.9 MiB with `dsf` |
| `SACDFS_FLATTEN` (`--flatten`) | `1` | each disc's `Stereo` and `Multichannel` folders sit under the album |

Leave `SACDFS_CACHE_MB` (`--cache-mb`) at its default (32; the tests ran with 16).

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
2. In Jellyfin, add a **Music** library with the folder `/media/sacd`.
3. Leave **LUFS audio normalization** off (the default): with it on, Jellyfin read the library
   about twice over, and sacdfs had to decode all of it.

## Check it works

The library shows every disc, the multichannel ones also as "Album (Multichannel)", and tracks
play.

Not validated: Jellyfin reading the NAS's SMB or NFS share (mounted per
[linux-client.md](linux-client.md)).
