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

File: [deploy/unraid/sacdfs.xml](../../deploy/unraid/sacdfs.xml). The steps (copy it into Unraid's user templates, then Docker, Add Container, pick `sacdfs`) are in [unraid.md](unraid.md); they are untested on Unraid.

The template does not make the container privileged. It passes the device and capabilities in **Extra Parameters** (`--device=/dev/fuse --cap-add=SYS_ADMIN --cap-add=DAC_READ_SEARCH --security-opt apparmor=unconfined`), plus a 30 second stop timeout and the health check. If you edit that field, keep those.

**What was checked.** The template's container, with its Extra Parameters, ports, paths and variables, was started with rootful Podman on a Linux host (Fedora) from the 0.5.0 image and a test set of 135 image files. It turned healthy, and the SMB share on 10445 listed the folders and the 457 DSF files, as a guest. **Not checked:** loading the file in Unraid's Docker tab, whether Unraid lets the container use `/dev/fuse` and `SYS_ADMIN`, the Community Applications listing, the icon in Unraid, and NFS from the template. Treat the Unraid steps as untested until someone reports them on [#82](https://github.com/aclater/sacdfs/issues/82).

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

## The icon

The mark is an original drawing of a disc rising out of a folder, with a waveform on the folder
(no third-party logo or trademark). Sources and sizes:

- [deploy/icon.svg](../../deploy/icon.svg): the master drawing.
- [deploy/icon.png](../../deploy/icon.png): 512 x 512 PNG, 8-bit with transparency, exported from the SVG.
  Unraid's template and `ca_profile.xml` point to its raw URL on `main`.
- TrueNAS hosts catalog icons on its own CDN. The app's `icon:` and `icon_url:` already carry the
  CDN path the catalog uses (`.../apps/sacdfs/icons/icon.svg`); the catalog's reviewers upload the
  file from the pull request and confirm the URL. The SVG is the file to attach.

Requirements this was checked against. The apps catalog's contribution guide says icons and
screenshots go on the TrueNAS CDN and are uploaded by the reviewer from the pull request; most
catalog apps use SVG (267 of 500 in the checked-out catalog) or PNG (217). For Unraid, the
Community Applications submission pages give no icon rule; the community starter kit for CA
repositories asks for a square PNG of 256 to 512 pixels at a raw URL on `main`, which the file
meets. Unraid's own documentation was not reachable, so the icon size rule is not confirmed by
Unraid.

## Checked with the stores' own tooling

- **TrueNAS apps catalog**, current `main` (cloned 2026-11-09), with the app copied to
  `ix-dev/community/sacdfs`: `ci.py --render-only` renders both test value sets
  (`basic-values.yaml`, `auth-values.yaml`) and `docker compose config` parses the result;
  `generate_metadata.py` and `check_metadata.py` report the metadata up to date;
  `port_validation.py` passes; the catalog's own `apps_dev_charts_validate validate` (the
  `ghcr.io/truenas/apps_validation` image) exits 0 over the whole catalog with the app in place.
  Run on a Linux host with rootless Podman standing in for Docker. One message is logged on every
  render and is not an error exit: "No cap_drop for service: sacdfs", because the app keeps the
  runtime's default capabilities (see above). Reviewers may ask about it.
- **Unraid Community Applications:** `deploy/unraid/sacdfs.xml` and `ca_profile.xml` are well
  formed and pass a check of the elements and `Config` attributes that the community CA starter
  kit's validator requires (written out by hand here, because Unraid publishes no validator).
  Unraid's own validate and scan on `ca.unraid.net/submit` has not been run: it needs the owner's
  account.

## Before a store listing

Submission is the owner's step and is not done. The repository is ready for it: icon, template,
`ca_profile.xml` (the repository profile Community Applications requires in the root), and the
catalog app directory. Still needed from outside: an Unraid forum account and support thread, and
a pull request to the TrueNAS apps repository (and an issue there first, as their template asks).
