# TrueNAS SCALE: install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that clients read over SMB or NFS. On TrueNAS SCALE it runs as a custom app, pasted in as a short text file (YAML). About 10 minutes.

**Not tested on 0.6.0** (no TrueNAS system was available for this release; nothing below was run on 0.6.0, so every step is *(untested)* on 0.6.0). The same steps were validated on TrueNAS SCALE 25.10 with the 0.4.0 image: see [Details](#what-was-checked).

## What you need

- TrueNAS SCALE 24.10 or later, and a login to its web page.
- The path of the dataset (TrueNAS's name for a storage folder) that holds your SACD images, for example `/mnt/pool/music`.
- Your network, written as `192.168.1.0/24` (your NAS's address with the last number replaced by `0/24`), if you want to limit who may use NFS.

## Steps

1. **Find the dataset's path.** In Datasets, click the dataset that holds your images; the path is shown (for example `/mnt/pool/music`). *You should see* a path starting `/mnt/`.
2. **Open the custom app form.** Apps, Discover Apps, Custom App, **Install via YAML**. Name the app `sacdfs`.
3. **Paste the file.** Change the `/mnt/pool/music` line to your path, `change-me` to a password of your own (the SMB login for `music`), and `192.0.2.0/24` to your network.

   ```yaml
   services:
     sacdfs:
       image: ghcr.io/aclater/sacdfs:0.6.1
       restart: unless-stopped
       devices:
         - /dev/fuse
       cap_add:
         - SYS_ADMIN
         - DAC_READ_SEARCH
       security_opt:
         - apparmor=unconfined
       ports:
         - "10445:445"
         - "12049:2049"
       volumes:
         - /mnt/pool/music:/images:ro
         - sacdfs-cache:/cache
       environment:
         SACDFS_FLATTEN: "1"
         SMB_USER: music
         SMB_PASSWORD: change-me
         NFS_CLIENTS: 192.0.2.0/24
       healthcheck:
         test: ["CMD", "/usr/local/bin/healthcheck.sh"]
         interval: 30s
         timeout: 10s
         start_period: 300s
   volumes:
     sacdfs-cache:
   ```

   The images are mounted read-only; sacdfs never writes to them. TrueNAS's own SMB and NFS hold ports 445 and 2049, so this app's shares are on **10445** (SMB) and **12049** (NFS).
4. **Save.** *You should see* the app turn **Running** within about 10 seconds.
5. **Stop it cleanly when you need to.** Use the app's Stop button, never a forced kill: stop and start take about 2 seconds, and a forced kill can leave a dead mount.

## Check it worked

Open the app's Logs. *You should see* the number of image files, `mounted`, the SMB share and `NFS SERVER INITIALIZED`. Then mount it from a computer, naming the ports: [linux-client.md](linux-client.md#on-other-ports) (Windows: `net use ... /TCPPORT:10445`; macOS: see the [deployment guide](../deployment.md#truenas-scale)). Open a folder and play a DSF or WAV file.

## If something goes wrong

- **You see:** the app stays **Deploying** or turns **Stopped**. **Do this:** open its Logs. A wrong path in the YAML (step 3) is the usual cause: fix the `/mnt/...` line and save again.
- **You see:** the share is empty. **Do this:** the dataset path is wrong or holds no `.iso` files, or the images are in a folder the app can't read. Check step 1 and the dataset's permissions.
- **You see:** clients can't connect. **Do this:** name the ports 10445 and 12049 when you mount ([linux-client.md](linux-client.md#on-other-ports)), and check `NFS_CLIENTS` covers the client's address.

## Details

### What was checked

Validated on TrueNAS SCALE 25.10 as a custom app installed from the YAML above (through the API call behind "Install via YAML"), with the published **0.4.0** image, on a library of 414 images. The app was running within 10 seconds and the logs showed what the check above lists. The share listed over SMB on 10445, and a DSF file (starting `DSD `) and a WAV file (starting `RIFF`) read in full. A stop took 2.1 s, a start 1.2 s, and the share answered 1.6 s after the start returned. NFS on 12049 was validated with 0.4.0-rc.2 (a Linux client mounted it and read stereo and multichannel files); with 0.4.0 only a successful NFS RPC to the port was checked, because the host used for that run cannot mount NFS. Screen names may differ between releases. Not run again for 0.5.0 or 0.6.0; the YAML above names the 0.6.0 image, which was not run on TrueNAS.

### Options and reasons

- **Settings:** `sacdfs-cache` keeps the seek index across updates (a few MB). The default cache (`SACDFS_CACHE_MB`: 32 MiB, or 16 under a memory limit up to 512 MiB) suits a NAS; leave it out. `NFS_CLIENTS` takes one address (`192.0.2.15`), a network (`192.0.2.0/24`), a comma-separated list of them, or `*`. For Roon or MPD add `SACDFS_FORMATS: dsf` (see your server's guide). Every setting: [deployment guide](../deployment.md#truenas-scale).
- **Catalog app:** a TrueNAS apps-catalog definition exists but isn't listed yet and can't be installed until it is: [store-apps.md](store-apps.md#truenas-the-catalog-app).
- **Players on the same NAS, without SMB:** a player installed as a custom app (Navidrome, Jellyfin, ...) can read sacdfs's mount directly: give sacdfs a dataset of its own (for example `pool/apps/sacdfs-mnt`) bound over `/srv` with `rshared`, and bind it into the player with `ro,rslave`; the library is then `/music/sacd`. Validated on TrueNAS SCALE 25.10.7 with the 0.4.1 image, as one app with Navidrome and as two apps, through restarts, updates and a crash of sacdfs. The YAML, what happens on a restart and what to avoid (catalog apps can't set `rslave`) are in the [deployment guide](../deployment.md#players-on-the-same-host-no-smb-hop). Not validated in this form: a host-path dataset for the cache and an alias address for port 445. Roon on the same NAS: [roon.md](roon.md).

### More problems

- **Harmless log lines:** a few NFS-Ganesha `CRIT` lines about D-Bus and `Cannot register NFS V4 on UDP`, and Samba's `parse_dfs_path_strict: Hostname <address> is not ours.` (logged when a client connects by IP address instead of by name).
- **Another app on the same NAS can mount sacdfs through TrueNAS's "SMB share" (`cifs`) storage type on port 445 only:** that type has no port field (read from the Plex app's catalog schema on 25.10.7), so it can't reach 10445. Plex could mount sacdfs only on 445 and a container address on a custom bridge network worked ([roon.md](roon.md)); observed with the 0.4.0-rc.2 image, not re-run with 0.4.0.
