[sacdfs](../../README.md) › [Documentation](../README.md) › [Install](README.md) › TrueNAS SCALE: install sacdfs

# TrueNAS SCALE: install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that clients read over SMB or NFS. On TrueNAS SCALE, install it from the Apps screen: it is in the community train of the TrueNAS apps catalog. About 10 minutes.

A custom app pasted in as YAML is the alternative, for settings the catalog form doesn't have.

The catalog app was checked on 0.7.0 on TrueNAS SCALE 25.10.8, in a VM, through the API calls behind the Apps screen rather than the web screens; the YAML on 0.6.3. See [Details](#what-was-checked).

## What you need

- TrueNAS SCALE 24.10 or later, and a login to its web page.
- The path of the dataset (TrueNAS's name for a storage folder) that holds your SACD images, for example `/mnt/pool/music`.
<!-- docs-lint: ignore private-info -- an example address for the reader's own network, not a real one -->
- Your network, written as `192.168.1.0/24` (your NAS's address with the last number replaced by `0/24`), if you want to limit who may use NFS.

## Install from the Apps screen

1. **Find the dataset's path.** In Datasets, click the dataset that holds your images; the path is shown (for example `/mnt/pool/music`). *You should see* a path starting `/mnt/`.
2. **Find the app.** Apps, Discover Apps, search `sacdfs`. *You should see* sacdfs, train `community`. Click it, then Install.
3. **Fill in the form.**
   - **Images Storage:** Type **Host Path**, and the path from step 1. Leave the cache on its ixVolume.
   - **SMB User:** leave it empty for read-only guest access, or set a user and a password.
   - **NFS Clients:** your network, or empty to allow all.
   - **SMB Port** and **NFS Port:** set **10445** and **12049**, the ports the rest of this guide uses (the form suggests 30602 and 30603; any free port works if you mount with it).
4. **Install.** *You should see* the app turn **Running** within about 15 seconds.

The catalog takes a new sacdfs release through its maintainers' automated update pull requests, so the Apps screen offers an update some time after the release (0.7.0 was in the catalog the same day). The form has no Formats option: for DSF only (Roon, MPD), use the YAML below.

## Or: install with YAML

1. **Find the dataset's path,** as above.
2. **Open the custom app form.** Apps, Discover Apps, Custom App, **Install via YAML**. Name the app `sacdfs`.
3. **Paste the file.** Change the `/mnt/pool/music` line to your path, `change-me` to a password of your own (the SMB login for `music`), and `192.0.2.0/24` to your network.

   ```yaml
   services:
     sacdfs:
       # facts:release repo=dsd-toolbox/sacdfs fmt='image: ghcr.io/aclater/sacdfs:{version}'
       image: ghcr.io/aclater/sacdfs:0.7.2
       # /facts
       restart: unless-stopped
       devices:
         - /dev/fuse
       cap_drop:                    # only the capabilities it needs (docs/deployment.md#privileges)
         - ALL
       cap_add:
         - SYS_ADMIN
         - DAC_OVERRIDE
         - DAC_READ_SEARCH
         - SETUID
         - SETGID
         - NET_BIND_SERVICE
         - SETPCAP
       security_opt:
         - no-new-privileges:true
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

## Share through TrueNAS's own SMB (port 445)

Windows drive mapping, Roon's "Add network share" and the SMB storage of other TrueNAS apps (Plex) have no port field: they need SMB on port 445, which TrueNAS's own SMB service holds. Instead of the app's share on 10445, sacdfs can show its files in a dataset, and TrueNAS shares that dataset on 445 like any other. No network changes. About 10 more minutes.

1. **Make an empty dataset.** Datasets, Add Dataset, name `sacdfs`, preset **Generic**. *You should see* `/mnt/pool/sacdfs`.
2. **Share it, before sacdfs runs.** Shares, SMB, Add: path `/mnt/pool/sacdfs`, name `sacd`, **Export Read Only** on; under Advanced Options turn on **Use Apple-style Character Encoding** (track titles with `:` or `?` then keep their names). Turn the SMB service on if asked. TrueNAS refuses to save a share whose dataset holds a running sacdfs mount ("child mount is not a ZFS dataset"): to edit the share later, stop the sacdfs app first.
3. **Give SMB a user,** if you have none: Credentials, Users, Add, with SMB access.
4. **Install sacdfs with YAML,** as above, with these changes: no `ports`, `SMB: "0"`, and the dataset bound over `/srv` with shared propagation. The catalog app's form doesn't offer this yet.

   ```yaml
   services:
     sacdfs:
       # facts:release repo=dsd-toolbox/sacdfs fmt='image: ghcr.io/aclater/sacdfs:{version}'
       image: ghcr.io/aclater/sacdfs:0.7.2
       # /facts
       restart: unless-stopped
       devices:
         - /dev/fuse
       cap_drop:
         - ALL
       cap_add:
         - SYS_ADMIN
         - DAC_OVERRIDE
         - DAC_READ_SEARCH
         - SETPCAP
       security_opt:
         - no-new-privileges:true
         - apparmor=unconfined
       ports:
         - "12049:2049"
       volumes:
         - /mnt/pool/music:/images:ro
         - sacdfs-cache:/cache
         - type: bind
           source: /mnt/pool/sacdfs
           target: /srv
           bind:
             propagation: rshared
       environment:
         SACDFS_FLATTEN: "1"
         SMB: "0"
         NFS_CLIENTS: 192.0.2.0/24
       healthcheck:
         test: ["CMD", "/usr/local/bin/healthcheck.sh"]
         interval: 30s
         timeout: 10s
         start_period: 300s
   volumes:
     sacdfs-cache:
   ```

   *You should see* the app **Running**, and in Datasets nothing new: the files are in the folder `sacd` of `/mnt/pool/sacdfs`.
5. **Connect on port 445.** Windows: map `\\nas\sacd`; Roon: Add network share, `\\nas\sacd\sacd`; macOS: `smb://nas/sacd`. Log in as the user of step 3. The discs are in the folder `sacd`.

- **NFS:** TrueNAS's own NFS cannot export the mount (the kernel's NFS server needs options TrueNAS doesn't set), so NFS stays on the app, on 12049. Leave out `DAC_READ_SEARCH`, the NFS port and set `NFS: "0"` if nothing uses NFS.
- **Other apps on the same NAS:** a catalog app (Plex) reads it through its **SMB share** storage type: server = the NAS's own address, path `sacd`, the user of step 3, read-only; the discs are then in the folder `sacd` of that storage's mount path. Don't give a catalog app the dataset as a Host Path: the form binds it without `rslave`, so the app keeps an empty or dead folder after sacdfs restarts or starts later. A custom (YAML) app can bind it with `rslave`: [deployment guide](../deployment.md#players-on-the-same-host-no-smb-hop).
- **At boot**, TrueNAS's SMB starts before the apps. Until sacdfs has mounted (seconds), the folder `sacd` refuses access, so a player scanning then sees an error, not an empty library (since 0.7.1; 0.7.0 and earlier show an empty folder).

## Check it worked

Open the app's Logs. *You should see* the number of image files, `mounted`, the SMB share and `NFS SERVER INITIALIZED`. Then mount it from a computer, naming the ports: [linux-client.md](linux-client.md#on-other-ports) (Windows: `net use ... /TCPPORT:10445`; macOS: see the [deployment guide](../deployment.md#truenas-scale)). Open a folder and play a DSF or WAV file.

## If something goes wrong

- **You see:** the app stays **Deploying** or turns **Stopped**. **Do this:** open its Logs. A wrong path in the YAML (step 3) is the usual cause: fix the `/mnt/...` line and save again.
- **You see:** the share is empty. **Do this:** the dataset path is wrong or holds no `.iso` files, or the images are in a folder the app can't read. Check step 1 and the dataset's permissions.
- **You see:** clients can't connect. **Do this:** name the ports 10445 and 12049 when you mount ([linux-client.md](linux-client.md#on-other-ports)), and check `NFS_CLIENTS` covers the client's address.

## Details

### What was checked

**TrueNAS's own SMB share, on 0.7.0 (#522):** TrueNAS SCALE 25.10.8 in a VM, through the API calls behind the screens (not the web screens). sacdfs installed from a lab copy of the catalog app with the Mount Folder option of [deploy/truenas/sacdfs](../../deploy/truenas/sacdfs/), rendered by TrueNAS itself, which binds the dataset over `/srv` with `rshared` as the YAML above does; the 136 conformance-corpus images. The SMB share was created while sacdfs was stopped; with the mount present TrueNAS refused it. Over TrueNAS's SMB on 445, as a TrueNAS user with a user-space Linux client (not Windows, Roon or a kernel mount), all 910 DSF and WAV files were identical to a reference, with the Apple-style encoding on (without it, one title holding `:` had a mangled name); writes were refused. Throughput through the VM's user-mode network: 52 to 62 MB/s against 43 to 51 MB/s from the app's own share on 10445, same files. The same 910 files after an app stop and start, a redeploy (the container replaced, as in an update) and two TrueNAS restarts (all files on 445 83 s after the restart call); TrueNAS raised no alert. The Plex catalog app (stable train) with an SMB share storage to the NAS's own address read all 910 files identical, also after sacdfs restarts and TrueNAS restarts; with the dataset as a Host Path it saw `Transport endpoint is not connected` after a sacdfs restart, and an empty folder after a TrueNAS restart. A custom app binding the dataset with `rslave` (uid 568) read them all throughout. TrueNAS's NFS export of the dataset showed `sacd` empty, and of `sacd` itself was refused (NFSv3) or empty (NFSv4.2). The YAML of step 4, with the lab's paths and network, installed through the call behind Install via YAML: healthy, NFS on 12049 started, and the same 910 files identical over 445, in the `rslave` app and in Plex. The mode-000 mount point (above) was run with the entry point of this change bound into the 0.7.0 image. Not run: Windows, Roon or a real Plex scan, macOS, the web screens, a kernel NFS mount of the app's export.

**Catalog app, on 0.7.0:** TrueNAS SCALE 25.10.8 in a VM, not on a TrueNAS appliance, with its standard catalog (no local copy). After a catalog sync, sacdfs was listed in the community train at 0.7.0 (app version 1.0.1), image pinned by digest. It was installed through the API calls behind Apps, Discover Apps, Install (the web screens were not clicked through), as a guest, with the 136 conformance-corpus images in a host-path dataset and SMB on 10445. The app was healthy 11.7 s after the install call, image pull included, with all capabilities dropped but the seven it needs, no new privileges and the images read-only. Over SMB, read as a guest with a user-space client (not a kernel mount), all 910 DSF and WAV files were identical to a reference made with the same image, and a write was refused. A stop took 1.2 s, a start 0.8 s, and the app was healthy 2.9 s after the start, with the same 910 files. Uninstalling, with images and volumes removed, took 1.6 s and left the images unchanged. Not run on 0.7.0: the web screens, NFS, an SMB user, a TrueNAS restart, an update through the catalog, a Windows or macOS client.

**On 0.6.3:** TrueNAS SCALE 25.10.8 in a VM, not on a TrueNAS appliance. The YAML of step 3, with the 0.6.3 image, an images dataset holding three conformance-corpus images and `NFS_CLIENTS: "*"`, installed as a custom app through the API call behind "Install via YAML" (the web screens were not clicked through). The app was **Running** 22 s after the call, image pull included. The logs showed `3 image files`, `mounted`, the SMB share with its user and `NFS SERVER INITIALIZED`. Over SMB on 10445 all 28 DSF and WAV files were identical to a reference; over NFS on 12049 too, read with a user-space NFSv4 client (libnfs) in a container, not a kernel mount. A stop took 2.5 s, a start 1.5 s, and the share answered 0.6 s after the start. After a restart of TrueNAS the app came back by itself; SMB answered 72 s after the restart began, with the same 28 files over SMB and NFS. Not run on 0.6.3: the web screens, a Windows or macOS client, a kernel NFS mount, an update of the app.

**Earlier:** Validated on TrueNAS SCALE 25.10 as a custom app installed from the YAML above (through the API call behind "Install via YAML"), with the published **0.4.0** image, on a library of 414 images. The app was running within 10 seconds and the logs showed what the check above lists. The share listed over SMB on 10445, and a DSF file (starting `DSD `) and a WAV file (starting `RIFF`) read in full. A stop took 2.1 s, a start 1.2 s, and the share answered 1.6 s after the start returned. NFS on 12049 was validated with 0.4.0-rc.2 (a Linux client mounted it and read stereo and multichannel files); with 0.4.0 only a successful NFS RPC to the port was checked, because the host used for that run cannot mount NFS. Screen names may differ between releases. Not run for 0.5.0 to 0.6.2.

### Options and reasons

- **Settings:** `sacdfs-cache` keeps the seek index across updates (a few MB). The default cache (`SACDFS_CACHE_MB`: 32 MiB, or 16 under a memory limit below 256 MiB) suits a NAS; leave it out. `NFS_CLIENTS` takes one address (`192.0.2.15`), a network (`192.0.2.0/24`), a comma-separated list of them, or `*`. For Roon or MPD add `SACDFS_FORMATS: dsf` (see your server's guide). Every setting: [deployment guide](../deployment.md#truenas-scale).
- **Catalog app:** what is in this repository and what the listed app differs in: [store-apps.md](store-apps.md#truenas-the-catalog-app).
- **Players on the same NAS, without SMB:** a player installed as a custom app (Navidrome, Jellyfin, ...) can read sacdfs's mount directly: give sacdfs a dataset of its own (for example `pool/apps/sacdfs-mnt`) bound over `/srv` with `rshared`, and bind it into the player with `ro,rslave`; the library is then `/music/sacd`. Validated on TrueNAS SCALE 25.10.7 with the 0.4.1 image, as one app with Navidrome and as two apps, through restarts, updates and a crash of sacdfs. The YAML, what happens on a restart and what to avoid (catalog apps can't set `rslave`) are in the [deployment guide](../deployment.md#players-on-the-same-host-no-smb-hop). Not validated in this form: a host-path dataset for the cache and an alias address for port 445. Roon on the same NAS: [roon.md](roon.md).

### More problems

- **Harmless log lines:** a few NFS-Ganesha `CRIT` lines about D-Bus and `Cannot register NFS V4 on UDP`, and Samba's `parse_dfs_path_strict: Hostname <address> is not ours.` (logged when a client connects by IP address instead of by name).
- **Another app on the same NAS can mount sacdfs through TrueNAS's "SMB share" (`cifs`) storage type on port 445 only:** that type has no port field, so it can't reach 10445. Use [TrueNAS's own SMB share](#share-through-truenass-own-smb-port-445) of sacdfs's dataset.

## See also

- [Install guide](README.md)
- [The installer in detail](README.md#the-installer-in-detail)
- [Deployment reference](../deployment.md)
- [Beta testing](../beta-testing.md)

Part of the [DSD Toolbox](https://github.com/dsd-toolbox/dsd-toolbox).
