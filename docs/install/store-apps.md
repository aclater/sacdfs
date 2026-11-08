# Unraid and TrueNAS app-store entries

sacdfs has an Unraid Community Applications template and a TrueNAS catalog app definition in
the repository. **Neither is listed in a store yet** (listing is a separate step, after
review by each store's maintainers). This page says what is in the repository, how to use
the Unraid template today, and what was checked where.

Both entries run the same published image as [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml):
`ghcr.io/aclater/sacdfs:0.5.0`, with the FUSE device, the capabilities `SYS_ADMIN` and
`DAC_READ_SEARCH`, AppArmor unconfined, SMB on port 10445 and NFS on 12049 (the NAS's own
services hold 445 and 2049), the images mounted read-only at `/images`, and the index cache at
`/cache`. Every setting is in the [deployment guide](../deployment.md).

## Unraid: install from the template file

File: [deploy/unraid/sacdfs.xml](../../deploy/unraid/sacdfs.xml).

1. On the Unraid server, open a terminal (the web UI's Terminal) and copy the file into
   Unraid's user templates, where **Docker, Add Container** lists it:

   ```sh
   wget -O /boot/config/plugins/dockerMan/templates-user/sacdfs.xml \
     https://raw.githubusercontent.com/aclater/sacdfs/main/deploy/unraid/sacdfs.xml
   ```

2. Docker, **Add Container**, and under **Template** pick `sacdfs` (the "User templates"
   group).
3. Set **SACD images** to the share that holds your images (for example `/mnt/user/music`),
   and leave the rest, or set **SMB user** and **SMB password** (without them the share is
   open, read-only, to the whole network) and **NFS clients**.
4. **Apply.** The container appears as `sacdfs`, with a health status after its start period.
5. Mount the shares from a client on the ports above ([linux-client.md](linux-client.md)).

The template does not make the container privileged. It passes the device and capabilities
in **Extra Parameters** (`--device=/dev/fuse --cap-add=SYS_ADMIN --cap-add=DAC_READ_SEARCH
--security-opt apparmor=unconfined`), plus a 20 second stop timeout and the health check.
If you edit that field, keep those.

**What was checked.** The template's container, with its Extra Parameters, ports, paths and
variables, was started with rootful Podman on a Linux host (Fedora) from the 0.5.0 image
and a test set of 135 image files. It turned healthy, and the SMB share on 10445 listed the
folders and the 457 DSF files, as a guest. **Not checked:** loading the file in Unraid's
Docker tab, whether Unraid lets the container use `/dev/fuse` and `SYS_ADMIN`, the Community
Applications listing, the icon in Unraid, and NFS from the template. Treat the Unraid
steps above as untested until someone reports them on [#82](https://github.com/aclater/sacdfs/issues/82).

## TrueNAS: the catalog app

Directory: [deploy/truenas/sacdfs/](../../deploy/truenas/sacdfs/), in the layout of the
`ix-dev/community/<app>` folders of the TrueNAS apps catalog (`app.yaml`, `ix_values.yaml`,
`questions.yaml`, `templates/docker-compose.yaml`, test values). The form asks for the folder
that holds your images (read-only), the ports, an optional SMB user and password, NFS clients,
formats, and storage for the index cache.

**You can't install it from this directory.** TrueNAS installs catalog apps only from its
catalog, so until sacdfs is accepted there, use the [TrueNAS SCALE guide](truenas-scale.md)
("Install via YAML"), which has the same settings and was run on TrueNAS.

**What was checked.** With the catalog's own tooling (its render and metadata scripts, run
on a Linux host), the app renders for two test value sets (guest access, and a user with
password, NFS clients and one folder per disc) and its metadata and port checks pass. The
rendered Compose file was then started with rootful Podman, from the 0.5.0 image and the test
set above: healthy, the SMB share on 10445 listed the files as a guest, and the user and
password set in the second value set worked. **Not checked:** the form in TrueNAS's web UI,
installing on TrueNAS, the permissions helper container of the catalog on TrueNAS's datasets
(on the test host it ran only on a plain folder), NFS from the app, and updates.
That nothing was run on TrueNAS or Unraid is why neither guide above is marked validated.

Two things the catalog's maintainers will weigh, noted here so nobody is surprised:

- The app runs as root with `SYS_ADMIN`, `DAC_READ_SEARCH` and `/dev/fuse`. sacdfs
  cannot mount without them ([why](../deployment.md)). The catalog's inclusion policy accepts
  only apps that stay within their container's boundaries; whether a FUSE mount does is their
  call.
- The catalog normally drops all capabilities and adds back a few. Samba and NFS-Ganesha
  fail to start that way (`smbd` aborts), so the template keeps the container runtime's
  default set and adds the two. This was found by running it.

## Before a store listing

What a listing needs from the project (an icon, screenshots, an account, a support thread)
is a submission step and is not done. The icon in the Unraid template is
[deploy/icon.png](../../deploy/icon.png), a placeholder drawn for this purpose.
