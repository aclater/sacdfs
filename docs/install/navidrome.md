# Navidrome

**Tested** with Navidrome 0.64.2 (official image) on sacdfs 0.4.0-rc.2 (and 0.3.0-rc.1), in a
container reading a sacdfs mount on its host: first scan 0.42 s, 93 of 93 tracks, folder art used, multichannel
areas as their own albums, streams and transcodes played, and a restart of sacdfs under the
running container was picked up. Details: [clients](../clients.md#navidrome).

## sacdfs settings

| Setting | Value | Why |
|---|---|---|
| `SACDFS_FORMATS` (`--format`) | `dsf`, or `dsf,wav` for both | both worked (186 of 186 with `dsf,wav`) |
| `SACDFS_FLATTEN` (`--flatten`) | `1` | each disc's `Stereo` and `Multichannel` folders sit under the album |

Leave `SACDFS_CACHE_MB` (`--cache-mb`) at its default (32 MiB, or 8 under a memory limit up to 512 MiB; the 0.4.0-rc.2 tests ran with 32, the 0.3.0-rc.1 tests with 16).

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
2. Set Navidrome's music folder to it: `-e ND_MUSICFOLDER=/media/sacd`.

## Check it works

The first scan lists every track; each multichannel area is its own album,
"… (Multichannel)". A re-scan with nothing changed reads nothing from sacdfs.

Not validated: Navidrome reading the NAS's SMB or NFS share (mounted per
[linux-client.md](linux-client.md)).
