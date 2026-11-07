# Lyrion Music Server (Logitech Media Server)

**Tested** with Lyrion Music Server 9.1.1 (official image) on sacdfs 0.4.0-rc.2 (and 0.3.0-rc.1), in a container
reading a sacdfs mount on its host, with squeezelite 2.0 as the player: 93 of 93 tracks with
`dsf` (186 of 186 with `dsf,wav`), first scan 1.1 s, folder art used, multichannel areas as
their own albums, native DSD playback. Details:
[clients](../clients.md#lyrion--logitech-media-server).

## sacdfs settings

| Setting | Value | Why |
|---|---|---|
| `SACDFS_FORMATS` (`--format`) | `dsf`, or `dsf,wav` | both worked |
| `SACDFS_FLATTEN` (`--flatten`) | `1` | each disc's `Stereo` and `Multichannel` folders sit under the album |

Leave `SACDFS_CACHE_MB` (`--cache-mb`) at its default (32 MiB, or 16 under a memory limit up to 512 MiB; the 0.4.0-rc.2 tests ran with 32, the 0.3.0-rc.1 tests with 16).

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
2. Run LMS as root inside its rootless container, `-e PUID=0 -e PGID=0`: the image's own
   user (uid 499) gets "Permission denied" on a FUSE mount made without `allow_other`, and
   root in a rootless container is the mount's owner.
3. In LMS, set the music folder to `/media/sacd` and scan.

## Check it works

The scan finds every track; each multichannel area is its own album, "… (Multichannel)".

**Players:** LMS passes DSF through as DSD, so the player must play DSD (squeezelite with DSD
support did). A player without DSD gets nothing unless a DSD plugin converts it; that was not
tested. squeezelite is stereo only, so it stopped on 5.1 files.

Not validated: LMS reading the NAS's SMB or NFS share.
