# sacdfs on TrueNAS SCALE

**Validated** on TrueNAS SCALE 25.10 as a custom app installed from the YAML below (through
the API call behind "Install via YAML"), with the published 0.4.0 image, on a library of 414
images. The app was running within 10 seconds and the logs showed what the checks below list.
The share listed over SMB on 10445, and a DSF file (starting `DSD `) and a WAV file (starting
`RIFF`) read in full. A stop took 2.1 s, a start 1.2 s, and the share answered 1.6 s after the
start returned. NFS on 12049 was validated with 0.4.0-rc.2 (a Linux client mounted it with
the command below and read stereo and multichannel files); with 0.4.0 only a successful NFS
RPC to the port was checked, because the host used for this run cannot mount NFS. Screen names
may differ between releases.
Reference: [deployment guide](../deployment.md#truenas-scale).

TrueNAS's own SMB and NFS services hold ports 445 and 2049, so this app publishes sacdfs's
shares on **10445** (SMB) and **12049** (NFS). Linux and macOS clients can use those ports;
Windows can only use 445 (see the [deployment guide](../deployment.md#truenas-scale) for the
other ways).

1. Note the path of the dataset that holds your SACD images, for example
   `/mnt/pool/music`.
2. Apps, Discover Apps, Custom App, **Install via YAML**. Name it `sacdfs` and paste, with
   your path, a password of your own and your network:

   ```yaml
   services:
     sacdfs:
       image: ghcr.io/aclater/sacdfs:0.4.0
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

   - The images are mounted read-only; sacdfs never writes to them.
   - `sacdfs-cache` keeps the seek index across updates (a few MB).
   - The default cache (`SACDFS_CACHE_MB`: 32 MiB, or 8 under a memory limit up to 512 MiB) suits a NAS; leave it out.
   - `NFS_CLIENTS` takes one address (`192.0.2.15`), a network (`192.0.2.0/24`), a
     comma-separated list of them, or `*`.
   - For Roon or MPD, add `SACDFS_FORMATS: dsf`; see your server's guide.
3. Save. The app turns **Running**.

## Check it works

- The app's logs show the number of image files, `mounted`, the SMB share, and
  `NFS SERVER INITIALIZED`. A few NFS-Ganesha `CRIT` lines about D-Bus and
  `Cannot register NFS V4 on UDP` are harmless. So are Samba's
  `parse_dfs_path_strict: Hostname <address> is not ours.` lines, logged when a client
  connects by IP address instead of by name.
- From a Linux client, mount it on the other ports:
  [linux-client.md](linux-client.md#on-other-ports).

TrueNAS's own app storage type "SMB share" (`cifs`) has no port field: its settings are
server, path, user name, password and domain (read from the catalog schema of the Plex app on
25.10.7). An app on the same NAS that mounts sacdfs through it therefore reaches 445 only, not the
published 10445. This was observed with the 0.4.0-rc.2 image, where Plex could mount sacdfs
only on 445 and a container address on a custom bridge network worked, as in [roon.md](roon.md);
it was not re-run with 0.4.0, because that would mean changing another app on the NAS.

Not validated in this form: a host-path dataset for the cache (the test used the named volume
above) and an alias address for port 445. Roon on the same NAS: [roon.md](roon.md).
