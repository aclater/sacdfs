# Plex

**Tested** with Plex Media Server 1.43.5 reading the share over SMB (CIFS, read-only) on the
same NAS: 406 of 406 tracks found on sacdfs 0.1.1, and the whole 291-image tree scanned on
0.2.0-rc.4 without errors. Details: [clients](../clients.md#plex).

## sacdfs settings

| Setting | Value | Why |
|---|---|---|
| `SACDFS_FLATTEN` | `1` | each disc's `Stereo` and `Multichannel` folders sit under the album |
| `SACDFS_CACHE_MB` | default (32 MiB, or 16 under a memory limit below 512 MiB) | the Plex tests ran with 16; the 0.4.0 default of 32 is close and was measured at 48 MiB peak for a Plex-like scan |

## Add the share

1. Give Plex the files, one of two tested ways:
   - **Plex as an app on the same TrueNAS:** in the Plex app's storage, add an SMB (CIFS)
     entry, read-only, with sacdfs's address, share `sacd`, and `SMB_USER` and its password.
     Plex reaches port 445 on sacdfs's own Docker network address, as in [roon.md](roon.md).
     Mount it at, say, `/data/sacd`.
   - **Plex in a container on the Linux host where sacdfs is mounted** (sacdfs run on that
     host): bind the folder **that holds** the mount, read-only with `rslave`, so that a
     restart of sacdfs shows up in the container: `-v /srv/media:/data:ro,rslave` for a mount
     at `/srv/media/sacd`.
2. In Plex, add a Music library with the folder `/data/sacd` (both ways above).
3. Before the first scan, edit the library: Advanced, turn on **Prefer local metadata**.
   Without it, Plex names both copies of a multichannel disc after the release it matches
   online, and shows two albums with the same name. For an existing library, turn it on and
   run "Refresh All Metadata".

## Check it works

The library lists each multichannel disc twice, as "Album" and "Album (Multichannel)", and
tracks play.

**Limits:** Plex does not use sacdfs's folder art; it fetches art online
([#95](https://github.com/aclater/sacdfs/issues/95)). Its nightly deep analysis reads every
file once; turn it off if the NAS has little CPU to spare.
