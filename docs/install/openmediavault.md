# sacdfs on OpenMediaVault

**Validated** on OpenMediaVault 8.6 (Debian 13) in a VM, with the omv-extras Compose plugin
(`openmediavault-compose` 8.2) and Docker 29, running a sacdfs 0.4.0 build (`main`). The
steps were run through the same calls the web interface makes. The container turned healthy,
and NFS and SMB clients read every DSF track of two test discs, matching the reference hashes
(29 of 29 over each). OMV's own SMB and NFS services were off. Reference:
[deployment guide](../deployment.md).

1. **omv-extras.** If System, Plugins doesn't list `openmediavault-omvextrasorg`, install
   omv-extras. The test VM, a plain Debian 13, got OMV and omv-extras together from the
   [omv-extras install script](https://github.com/OpenMediaVault-Plugin-Developers/installScript).
2. **Compose plugin.** System, Plugins: install `openmediavault-compose`.
3. **Shared folders.** Storage, Shared Folders: one for Compose's files (for example
   `compose`), and the one that holds your SACD images (here `music`).
4. **Docker.** Services, Compose, Settings: choose the `compose` shared folder, Save, then
   **Reinstall Docker**.
5. **The file.** Services, Compose, Files, Add: name it `sacdfs` and paste
   [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml). Change the one marked line to the
   absolute path of your images' shared folder (Storage, Shared Folders shows it; `/srv/music` in the
   test). For a password on the SMB share, uncomment `SMB_USER` and `SMB_PASSWORD`; for Roon or
   MPD, set `SACDFS_FORMATS: dsf` (see your server's guide). The file publishes ports 10445 and
   12049 to keep clear of OMV's own SMB and NFS services; clients name them when they mount
   ([linux-client.md](linux-client.md#on-other-ports)).
6. Save, select the file and press **Up**.

## Check it works

- In a shell on the NAS, `sudo docker ps` shows the sacdfs container `(healthy)` within a
  minute.
- Its log shows the number of image files, `mounted`, the SMB share and
  `NFS SERVER INITIALIZED`.
- From a client: [linux-client.md](linux-client.md).

**Which file was run where.** The validation above used an earlier Compose file (the 0.4.0 image, ports
445 and 2049, OMV's SMB and NFS off). The current file, `deploy/nas-compose.yaml` (the 0.5.0 image,
ports 10445 and 12049), was not run through OMV's Compose plugin; the same file was run with Docker
Compose and Podman elsewhere ([asustor.md](asustor.md), [docker-podman.md](docker-podman.md)). Not
tried: running beside OMV's own SMB or NFS services on those ports.
