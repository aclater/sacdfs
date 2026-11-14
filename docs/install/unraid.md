# Unraid: install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that clients read over SMB or NFS. On Unraid it runs as a Docker container, added from a template file. About 10 minutes.

**Not tested on Unraid.** Nobody has run sacdfs on Unraid and reported back, and no Unraid system was available for 0.6.0: every step below is *(untested)*. What was run elsewhere is in [Details](#what-was-checked).

## What you need

- An Unraid server with Docker turned on, and its web page.
- The share that holds your SACD images, for example `/mnt/user/music`.
- Ports 10445 and 12049 free (Unraid's own SMB and NFS use 445 and 2049, so sacdfs uses those two).

## Steps

1. **Open a terminal on the server.** In Unraid's web page, click the `>_` (Terminal) icon at the top right *(untested)*.
2. **Copy the template into Unraid.** A template is a ready-made set of container settings.

   ```sh
   wget -O /boot/config/plugins/dockerMan/templates-user/sacdfs.xml \
     https://raw.githubusercontent.com/aclater/sacdfs/main/deploy/unraid/sacdfs.xml
   ```

   *You should see* the download finish without an error. *(untested)*
3. **Add the container.** Docker, **Add Container**, and under **Template** pick `sacdfs` (in the "User templates" group). *(untested)*
4. **Fill in the form.** Set **SACD images** to your share (for example `/mnt/user/music`). Set **SMB user** and **SMB password**: without them the share is open, read-only, to the whole network. Set **NFS clients** (for example `192.168.1.0/24`) to limit NFS. Leave the rest. *(untested)*
5. **Apply.** *You should see* a container named `sacdfs` that shows a health status after its start period (up to 5 minutes). *(untested)*
6. **Stop it cleanly when you need to.** Use the container's Stop (or `docker stop sacdfs`), never a forced kill: the template gives it 30 seconds to unmount. *(untested)*

## Check it worked

In the container's logs, *you should see* the number of image files, `mounted`, the SMB share and `NFS SERVER INITIALIZED`. Then mount the shares from a computer, naming the ports 10445 and 12049: [linux-client.md](linux-client.md#on-other-ports). Open a folder and play a DSF or WAV file.

## If something goes wrong

- **You see:** the container stops at once, or the logs say `/dev/fuse` or `mount` is not permitted. **Do this:** the container needs the FUSE device and seven capabilities. The template passes them in **Extra Parameters**: `--device=/dev/fuse --cap-drop=ALL --cap-add=SYS_ADMIN --cap-add=DAC_OVERRIDE --cap-add=DAC_READ_SEARCH --cap-add=SETUID --cap-add=SETGID --cap-add=NET_BIND_SERVICE --cap-add=SETPCAP --security-opt no-new-privileges:true --security-opt apparmor=unconfined`. If you edited that field, put them back. Whether Unraid allows them is not known: please report it on [#82](https://github.com/aclater/sacdfs/issues/82).
- **You see:** the share is empty. **Do this:** the **SACD images** path is wrong or holds no `.iso` files.
- **You see:** clients can't connect. **Do this:** name the ports 10445 and 12049 when you mount, and check **NFS clients** covers the client's address.

## Details

### What was checked

*Not checked on Unraid at all:* loading the file in the Docker tab, whether Unraid lets the container use `/dev/fuse` and `SYS_ADMIN`, the Community Applications listing, the icon in Unraid, and NFS from the template. *Run elsewhere:* the template's container, with its Extra Parameters, ports, paths and variables, was started with rootful Podman on a Linux host (Fedora), with the **0.5.0** image and a test set of 135 image files (not 0.6.0). It turned healthy, and the SMB share on 10445 listed the folders and the 457 DSF files, as a guest. More on the template, the icon and the checks with the stores' tooling: [store-apps.md](store-apps.md#unraid-install-from-the-template-file).

### Options and reasons

- **Compose plugin instead of the template** *(untested on Unraid)*: [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml) is the one Compose file for every NAS container manager. It was run with Docker Compose and Podman on other systems ([docker-podman.md](docker-podman.md), [asustor.md](asustor.md#container-route)). In the Compose plugin, add a stack from the file (the menu names were not seen) and change the one marked line to the path of your images. It publishes ports 10445 and 12049 for the same reason.
- **Why not privileged:** the template passes only the device and the capabilities, not `--privileged`. Every setting is in the [deployment guide](../deployment.md).

Reports are welcome on [#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how the container was started, and what worked.
