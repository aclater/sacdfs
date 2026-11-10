# OpenMediaVault: install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that clients read over SMB or NFS. On OpenMediaVault (OMV) it runs as a container, started from OMV's Compose plugin. About 20 minutes.

**Not tested on 0.6.0** (no OpenMediaVault system was available for this release): every step below is *(untested)* on 0.6.0. The same steps were validated on OMV 8.6 with a 0.4.0 build; see [Details](#what-was-checked).

## What you need

- OpenMediaVault 8 (Debian 13) and a login to its web page.
- A shared folder that holds your SACD images (here called `music`).
- A second shared folder for Compose's own files (here called `compose`).
- Ports 10445 and 12049 free (OMV's own SMB and NFS use 445 and 2049; sacdfs uses these two).

## Steps

1. **Install omv-extras, if it isn't there.** In System, Plugins, look for `openmediavault-omvextrasorg`. If it is missing, install omv-extras with its [install script](https://github.com/OpenMediaVault-Plugin-Developers/installScript). *(untested)*
2. **Install the Compose plugin.** System, Plugins: install `openmediavault-compose`. *(untested)*
3. **Make the shared folders.** Storage, Shared Folders: make `compose` and `music` if you don't have them. *(untested)*
4. **Set up Docker.** Services, Compose, Settings: choose the `compose` shared folder, Save, then **Reinstall Docker**. *(untested)*
5. **Add the file.** Services, Compose, Files, Add: name it `sacdfs` and paste the contents of [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml). Change the one marked line to the absolute path of your images' shared folder (Storage, Shared Folders shows it; it looked like `/srv/music` in the test). For a password on the SMB share, uncomment the `SMB_USER` and `SMB_PASSWORD` lines. *(untested)*
6. **Start it.** Save, select the file and press **Up**. *You should see* the file listed as running. *(untested)*
7. **Stop it cleanly when you need to.** Select the file and press **Down** (or `sudo docker compose stop` in a shell), never a forced kill. *(untested)*

## Check it worked

In a shell on the NAS, `sudo docker ps` should show the `sacdfs` container `(healthy)` within a minute. Its log shows the number of image files, `mounted`, the SMB share and `NFS SERVER INITIALIZED`. From a computer, mount the shares on ports 10445 and 12049: [linux-client.md](linux-client.md#on-other-ports). Open a folder and play a file.

## If something goes wrong

- **You see:** the container isn't healthy, or `Up` fails. **Do this:** read its log (the Logs button of the Compose file, or `sudo docker logs sacdfs`). A wrong images path in step 5 is the usual cause.
- **You see:** the share is empty. **Do this:** the path in step 5 isn't the folder that holds the `.iso` files; use the absolute path from Storage, Shared Folders.
- **You see:** `Reinstall Docker` fails or the Compose menu is missing. **Do this:** check steps 1 to 4 in order: omv-extras, then the plugin, then the `compose` folder chosen in Settings.

## Details

### What was checked

Validated on OpenMediaVault 8.6 (Debian 13) in a VM, with the omv-extras Compose plugin (`openmediavault-compose` 8.2) and Docker 29, running a sacdfs **0.4.0** build (`main`). The steps were run through the same calls the web interface makes. The container turned healthy, and NFS and SMB clients read every DSF track of two test discs, matching the reference hashes (29 of 29 over each). OMV's own SMB and NFS services were off. The test VM, a plain Debian 13, got OMV and omv-extras together from the install script above. Not run again for 0.5.0 or 0.6.0.

**Which file was run where.** That validation used an earlier Compose file (the 0.4.0 image, ports 445 and 2049, OMV's SMB and NFS off). The current file, `deploy/nas-compose.yaml` (ports 10445 and 12049), was not run through OMV's Compose plugin; the same file was run with Docker Compose and Podman elsewhere ([asustor.md](asustor.md#container-route), [docker-podman.md](docker-podman.md)). Not tried: running beside OMV's own SMB or NFS services on those ports.

### Options and reasons

- **Why ports 10445 and 12049:** to keep clear of OMV's own SMB and NFS services; clients name them when they mount.
- **For Roon or MPD,** set `SACDFS_FORMATS: dsf` in the file (see your server's guide).
- **Reference:** [deployment guide](../deployment.md); the general container route is [docker-podman.md](docker-podman.md).
