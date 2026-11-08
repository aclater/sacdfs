# sacdfs on OpenMediaVault

**Validated** on OpenMediaVault 8.6 (Debian 13) in a VM, with the omv-extras Compose plugin
(`openmediavault-compose` 8.2) and Docker 29, running a sacdfs 0.4.0 build (`main`). The
steps were run through the same calls the web interface makes. The container turned healthy,
and NFS and SMB clients read every DSF track of two test discs, matching the reference hashes
(29 of 29 over each). OMV's own SMB and NFS services were off. Not run again for 0.5.0 (no
OpenMediaVault system was available). Reference: [deployment guide](../deployment.md).

1. **omv-extras.** If System, Plugins doesn't list `openmediavault-omvextrasorg`, install
   omv-extras. The test VM, a plain Debian 13, got OMV and omv-extras together from the
   [omv-extras install script](https://github.com/OpenMediaVault-Plugin-Developers/installScript).
2. **Compose plugin.** System, Plugins: install `openmediavault-compose`.
3. **Shared folders.** Storage, Shared Folders: one for Compose's files (for example
   `compose`), and the one that holds your SACD images (here `music`).
4. **Docker.** Services, Compose, Settings: choose the `compose` shared folder, Save, then
   **Reinstall Docker**.
5. **The file.** Services, Compose, Files, Add: name it `sacdfs` and paste, with the absolute
   path of your images' shared folder (Storage, Shared Folders shows it; `/srv/music` in the
   test):

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
         - "445:445"
         - "2049:2049"
       volumes:
         - /srv/music:/images:ro
         - sacdfs-cache:/cache
       environment:
         SACDFS_FLATTEN: "1"
   volumes:
     sacdfs-cache:
   ```

   For a password on the SMB share add `SMB_USER` and `SMB_PASSWORD` under `environment`;
   for Roon or MPD, `SACDFS_FORMATS: dsf` (see your server's guide).
6. Save, select the file and press **Up**.

## Check it works

- In a shell on the NAS, `sudo docker ps` shows the sacdfs container `(healthy)` within a
  minute.
- Its log shows the number of image files, `mounted`, the SMB share and
  `NFS SERVER INITIALIZED`.
- From a client: [linux-client.md](linux-client.md).

Not validated: running beside OMV's own SMB or NFS service. Both want ports 445 and 2049; the
[TrueNAS guide](truenas-scale.md) shows the same container on other ports, which was not
tried on OMV.
