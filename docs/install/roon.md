# Roon: install sacdfs

Roon is a music player with its own library. This guide lets Roon Server read your SACD images from sacdfs over SMB (a Windows-style network share). About 20 minutes.

Not tested on 0.6.0. Roon was last checked on 0.4.1-rc.1 (Roon 2.74) as an app on the same TrueNAS machine as sacdfs; the steps below are that setup, not re-run for this guide *(untested)*.

## What you need

- sacdfs running as an app on your NAS, started with `SACDFS_FORMATS=dsf`, `SACDFS_FLATTEN=1`, `SMB_USER` and `SMB_PASSWORD` (Roon asks for a user name and password). For TrueNAS follow [truenas-scale.md](truenas-scale.md); other hosts, [docker-podman.md](docker-podman.md).
- Roon Server running as an app on the same NAS (its official image).
- A Roon account and the Roon app on a phone, tablet or computer.

## Steps

1. **Give sacdfs its own address.** Roon's "Add network share" has no port field: it needs SMB on port 445, and a NAS's own SMB service (TrueNAS) already holds 445. In the sacdfs app's YAML ([truenas-scale.md](truenas-scale.md)) add this, with a subnet nothing else on the NAS uses:

   ```yaml
   services:
     sacdfs:
       # ... as in truenas-scale.md, plus:
       networks:
         sacd:
           ipv4_address: 172.29.42.10
   networks:
     sacd:
       ipam:
         config:
           - subnet: 172.29.42.0/24
   ```

   *You should see:* the sacdfs app restarts and shows as running.

2. **Let Roon mount shares.** In the **Roon** app's YAML add `cap_add: [SYS_ADMIN, DAC_READ_SEARCH]` and `security_opt: [apparmor=unconfined]`: Roon mounts network shares itself, inside its container. Restart Roon.

3. **Add the share in Roon.** Settings, Storage, **Add Folder**, **Add network share**. Type `\\172.29.42.10\sacd` and the `SMB_USER` name and password.
   *You should see:* the share listed as watched.

## Check it worked

Roon's log shows `[roon/cifs] Connected`, and the albums appear as Roon imports them. Play one.

## If something goes wrong

- **You see:** `could not mount ... tried SMB versions 3, 2.1, 2.0, 1.0` in Roon's log. **Do this:** the Roon app is missing the `cap_add` and `security_opt` of step 2.
- **You see:** each disc twice, or the same album from two places. **Do this:** sacdfs must serve `dsf` only, and if you also have the discs as extracted files, remove those: Roon merges the two copies into one album and may play either.
- **You see:** the NAS very busy for a while after adding the share. **Do this:** that is Roon's audio analysis reading every track in full; wait, or lower the analysis speed in Roon's settings.

## Details

### What was checked

- Roon Server 2.x (its official image) as an app on the same NAS as sacdfs, over SMB. On 0.2.0: Roon imported the tracks, folder art, `Artwork/` trees and a relative-path playlist with all of its entries, picked up an upgrade by itself at its 4-hourly scan, and kept playing through a sacdfs restart.
- On 0.4.1-rc.1 (Roon 2.74 build 1719): the whole library, 4,902 DSF files on both 0.4.0 and 0.4.1 and no album lost; a Force Rescan afterwards took 3.2 s and reported no changes. Peak RSS over about 4 hours was 88 MB at the default cache.
- Not run: Roon on a machine other than the NAS, a restart during a scan. Full results: [clients.md](../clients.md#roon).

### Options and reasons

- **`SACDFS_FORMATS=dsf`.** With both formats every track shows twice; Roon plays DSD and converts it itself.
- **`SACDFS_FLATTEN=1`.** Each disc's `Stereo` and `Multichannel` folders then sit under the album.
- **`SMB_USER` and `SMB_PASSWORD`.** Roon asks for a user name and password.
- **Cache.** Leave `SACDFS_CACHE_MB` at its default (32 MiB, or 16 under a memory limit up to 512 MiB); the tests ran with 16, which also works.
- **A Docker network of its own for sacdfs.** Roon needs port 445; see step 1.
- **Playlists.** Write entries as paths relative to the share.
- **Multichannel areas.** Roon shows them as separate albums under its own canonical titles, so the "(Multichannel)" suffix is not visible there.

### More problems

- After upgrading sacdfs from 0.1 to 0.2, an existing Roon library saw every share track as modified once; Roon decides what to rescan from folder times ([restarts and rescans](../clients.md#restarts-and-rescans)). Moving from 0.4.0 to 0.4.1 renamed 241 tracks and added 359 `cover` images; Roon found them all.
