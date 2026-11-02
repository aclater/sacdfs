# sacdfs on TrueNAS SCALE

**Validated** on TrueNAS SCALE 25.10 as a custom app installed from the YAML below (through
the API call behind "Install via YAML"), with the published 0.3.0 image: the 0.4.0 image was
not published yet when this guide was written. The app was running within seconds, served a
library of 291 images, and a Linux client mounted both shares on the other ports and read
them; a stop and start took 3 s. The 0.4.0 settings themselves were validated with a 0.4.0
build in [Docker or Podman](docker-podman.md). Screen names may differ between releases.
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
         start_period: 60s
   volumes:
     sacdfs-cache:
   ```

   - The images are mounted read-only; sacdfs never writes to them.
   - `sacdfs-cache` keeps the seek index across updates (a few MB).
   - The default cache (`SACDFS_CACHE_MB`, 32) suits a NAS; leave it out.
   - For Roon or MPD, add `SACDFS_FORMATS: dsf`; see your server's guide.
3. Save. The app turns **Running**.

## Check it works

- The app's logs show the number of image files, `mounted`, the SMB share, and
  `NFS SERVER INITIALIZED`. A few NFS-Ganesha `CRIT` lines about D-Bus and
  `Cannot register NFS V4 on UDP` are harmless.
- From a Linux client, mount it on the other ports:
  [linux-client.md](linux-client.md#on-other-ports).

Not validated in this form: the 0.4.0 image on TrueNAS itself, a host-path dataset for the
cache (the test used the named volume above), and an alias address for port 445. Roon on the
same NAS: [roon.md](roon.md).
