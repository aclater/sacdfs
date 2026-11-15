# App-store entries

sacdfs has entries for five app stores in the repository: an Unraid Community Applications
template, a TrueNAS catalog app, a CasaOS and ZimaOS app-store entry, a Cosmos Cloud servapp and
an Umbrel app. **None is listed in a store yet** (listing is a separate step, after review by
each store's maintainers). Runtipi's store takes no new apps ([below](#runtipi)). This page says
what is in the repository, how to use each entry today, and what was checked where.

Every entry runs the same published image as [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml):
`ghcr.io/aclater/sacdfs:0.6.4`, with the FUSE device, all capabilities dropped but the seven
the programs need (`SYS_ADMIN`, `DAC_OVERRIDE`, `DAC_READ_SEARCH`, `SETUID`, `SETGID`,
`NET_BIND_SERVICE`, `SETPCAP`; [why each one](../deployment.md#privileges)), no new privileges,
AppArmor unconfined, SMB on port 10445 and NFS on 12049 (the NAS's own
services hold 445 and 2049), the images mounted read-only at `/images`, and the index cache at
`/cache`. Every setting is in the [deployment guide](../deployment.md).

## Unraid: install from the template file

File: [deploy/unraid/sacdfs.xml](../../deploy/unraid/sacdfs.xml). The steps (copy it into Unraid's user templates, then Docker, Add Container, pick `sacdfs`) are in [unraid.md](unraid.md); they are untested on Unraid.

The template does not make the container privileged. It passes the device and capabilities in **Extra Parameters** (`--device=/dev/fuse --cap-drop=ALL --cap-add=SYS_ADMIN --cap-add=DAC_OVERRIDE --cap-add=DAC_READ_SEARCH --cap-add=SETUID --cap-add=SETGID --cap-add=NET_BIND_SERVICE --cap-add=SETPCAP --security-opt no-new-privileges:true --security-opt apparmor=unconfined`), plus a 30 second stop timeout and the health check. If you edit that field, keep those.

**What was checked.** The template's container, with its Extra Parameters, ports, paths and variables, was started with rootful Podman on a Linux host (Fedora) from the 0.5.0 image and a test set of 135 image files. It turned healthy, and the SMB share on 10445 listed the folders and the 457 DSF files, as a guest. **Re-run on 0.6.3:** the template's Extra Parameters, read from the file, with its ports, paths and variables (an SMB user and password set), with rootless Podman on Fedora 45 and three corpus images: healthy in about 6 s; over SMB on the template's port, as the user, all 28 DSF and WAV files identical to a reference, and a wrong password refused; a stop took 0.5 s and the container was healthy again about 2 s after a start. NFS not checked. **Not checked:** loading the file in Unraid's Docker tab, whether Unraid lets the container use `/dev/fuse` and `SYS_ADMIN`, the Community Applications listing, the icon in Unraid, and NFS from the template. Treat the Unraid steps as untested until someone reports them on [#82](https://github.com/aclater/sacdfs/issues/82).

## TrueNAS: the catalog app

Directory: [deploy/truenas/sacdfs/](../../deploy/truenas/sacdfs/), in the layout of the
`ix-dev/community/<app>` folders of the TrueNAS apps catalog (`app.yaml`, `ix_values.yaml`,
`questions.yaml`, `templates/docker-compose.yaml`, test values). The form asks for the folder
that holds your images (read-only), the ports, an optional SMB user and password, NFS clients,
formats, and storage for the index cache.

**You can't install it from this directory.** TrueNAS installs catalog apps only from its
catalog (the app is submitted there, not yet accepted), so until then, use the [TrueNAS SCALE guide](truenas-scale.md)
("Install via YAML"), which has the same settings and was run on TrueNAS.

**What was checked on TrueNAS (0.6.4), as a catalog app.** On TrueNAS SCALE 25.10.8 in a VM, not on
TrueNAS hardware. The VM's catalog checkout pointed at a local copy of the TrueNAS apps catalog with the
app added and published by the catalog's own tool (`apps_catalog_update publish` and `update`). The app
then showed in the catalog like any other community app, and every step used the API calls behind the
Apps screen. The images were the 136 conformance-corpus images in a dataset, and the index cache was on
the app's own ixVolume.

| Check | Result |
|---|---|
| Install of the app at 0.6.3 (version 1.0.1), guest access | running and healthy 13.3 s after the install call, image pull included |
| Update to 0.6.4 (1.0.2) | the Apps screen's update was offered; healthy 12.2 s after the update call |
| Container settings | `cap_drop: ALL`, the seven capabilities, `no-new-privileges`, AppArmor unconfined, `/dev/fuse`, not privileged, image pinned by digest |
| Capabilities each program holds (from `/proc`) | sacdfs: `SYS_ADMIN`, `DAC_OVERRIDE`; smbd: `SETUID`, `SETGID`, `NET_BIND_SERVICE`; NFS-Ganesha: `DAC_READ_SEARCH`; all with no new privileges |
| SMB as a guest (kernel CIFS mount from a Linux machine) | all 914 DSF and WAV files byte-identical to a reference; creating, appending to and deleting a file refused |
| NFS (kernel NFSv4 mount from a Linux machine) | all 914 files identical; writes refused |
| An SMB user and password, set by editing the app | all 914 files identical as the user; a wrong password and a guest refused; NFS still identical |
| Windows 11 (VM), as the user | `robocopy /E` copied 331 folders and 914 files (189.8 MB) in 11.1 s, none failed; the content of every file identical to the reference; a write refused |
| Stop, start | stop 1.9 s, start 2.8 s; healthy 4.2 s after the start call; 914 files identical |
| TrueNAS restart | the app came back by itself, healthy 75 s after the restart call; 914 files identical over SMB and NFS |
| Uninstall, with images and volumes removed | 2.9 s; containers, app configuration and cache volume gone; the images dataset unchanged (136 files, same checksums) |
| Fresh install at 0.6.4 | healthy 14.5 s after the install call; 914 files identical as a guest |
| Resources during full reads | sacdfs peak RSS 81 MB after two full SMB reads; the app's containers used 1.7 s of CPU for one full read of the 914 files |

After the uninstall, the 0.6.3 image replaced by the update stayed on disk as an untagged image.
TrueNAS removes only the images of the version that is installed.

**What was checked on TrueNAS before (0.6.4, rendered files).** On TrueNAS SCALE 25.10.8 in a VM, not on TrueNAS
hardware, the app at 0.6.4 (version 1.0.2) was rendered by the catalog's own tooling for both test
value sets, and each rendered Compose file installed as a custom app, as for 0.6.3 below:

| Check | Guest access (`basic-values.yaml`) | User and password, NFS clients, one folder per disc (`auth-values.yaml`) |
|---|---|---|
| App running, image `ghcr.io/aclater/sacdfs:0.6.4` | 15.4 s after the create call, with the image pull | 6.3 s, image already pulled |
| Container settings | `cap_drop: ALL`, the seven capabilities above, `no-new-privileges`, not privileged | the same |
| Permissions helper | ran on the cache dataset, exit 0 | the same |
| SMB on 10445: 28 DSF and WAV files of three corpus images identical to a reference | pass, as a guest | pass, as the user; a wrong password lists nothing |
| NFS on 12049: the same 28 files | pass | pass |
| Stop, start | 1.7 s, 2.7 s; the share answered at once after the start | not run |

Not run in that check: a TrueNAS restart, the web form, a real client mounting the shares, updates (all run since, above).

**What was checked on TrueNAS (0.6.3).** On TrueNAS SCALE 25.10.8 in a VM, with the app
rendered by the catalog's own tooling for both test value sets and each rendered Compose file
installed as a custom app (the API call behind "Install via YAML"; a catalog app can't be
installed from outside the catalog), the images in a dataset and the cache in a second dataset:

| Check | Guest access (`basic-values.yaml`) | User and password, NFS clients, one folder per disc (`auth-values.yaml`) |
|---|---|---|
| App running | 15.5 s after the create call, with the image pull | 5.4 s, image already pulled |
| Permissions helper | ran on the cache dataset, exit 0 | the same |
| SMB on 10445: 28 DSF and WAV files of three corpus images identical to a reference | pass, as a guest | pass, as the user; a wrong password lists nothing |
| NFS on 12049: the same 28 files | pass | pass |
| Stop, start | 2.5 s, 2.5 s; the share answered at once after the start | 2.5 s, 2.5 s; the share answered 0.6 s after the start |
| TrueNAS restart | the app came back by itself, healthy; SMB answered 72 s after the restart began, 28 files identical over SMB and NFS | not run |

NFS was read with a user-space NFSv4 client (libnfs) in a container, not with a kernel mount.
**Not checked on TrueNAS:** the app's form in the web UI (it shows only once the app is in the
catalog), a real client mounting the shares, and updates.

**What was checked before, on a Linux host.** With the catalog's own tooling (its render and
metadata scripts), the app renders for two test value sets (guest access, and a user with
password, NFS clients and one folder per disc) and its metadata and port checks pass. The
rendered Compose file was then started with rootful Podman, from the 0.5.0 image and the test
set above: healthy, the SMB share on 10445 listed the files as a guest, and the user and
password set in the second value set worked. Nothing was run on Unraid, which is why the Unraid
steps stay marked untested.

Two things the catalog's maintainers will weigh, noted here so nobody is surprised:

- The app runs as root with `/dev/fuse` and seven capabilities, among them `SYS_ADMIN` and
  `DAC_READ_SEARCH`. sacdfs cannot mount without them, nor NFS-Ganesha serve
  ([why each one](../deployment.md#privileges)); each program holds only its own share. The catalog's inclusion policy accepts
  only apps that stay within their container's boundaries; whether a FUSE mount does is their
  call.
- The catalog drops all capabilities and adds back a few, and the template now keeps that
  default (#386): it adds back the seven above. Before 0.6.4 it kept the runtime's default set,
  because Samba aborted without `SETUID` and `SETGID`.

## CasaOS and ZimaOS: the app-store entry

Directory: [deploy/casaos/](../../deploy/casaos/), in the layout of the CasaOS (ZimaOS) app
store: `Apps/SACDFS/docker-compose.yml` with the store's `x-casaos` block, the icon, and the
store's `store-config.json` and `supported-languages.json`, so the directory works on its own as
a third-party store source. The Compose file is [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml)
in the store's format: the image pinned by digest, `/dev/fuse`, all capabilities dropped but the
seven, no new privileges, AppArmor unconfined, not privileged, SMB on 10445 and NFS on 12049. The
images folder defaults to `/DATA/Media/Music` (read-only), the store's usual music folder, and
the index cache goes to `/DATA/AppData/sacdfs/cache`. Both are on the install form, as are the
formats, the SMB user and password (empty: guest), and NFS clients. sacdfs has no web UI, so the
entry has no web port (`port_map: ""`, as for the store's Roon Server).

**Install it today, as a third-party source.** CasaOS and ZimaOS take a source as the URL of a
zip that holds an `Apps/` folder. The branch archive works:
`https://github.com/aclater/sacdfs/archive/refs/heads/store-apps.zip` (this address changes when
the entry moves to `main`). Add it as an app-store source, then install sacdfs from the store and
set the images folder on the form.

**What was checked (0.6.4).** On CasaOS 0.4.15, installed with its official script on Debian 12
(cloud image, kernel 6.1), and on ZimaOS 1.8.0 from its installer image, each in a VM on
Linux (KVM), not on IceWhale hardware. The store source was the branch archive above, added with
the API call behind the store's "add source" (on CasaOS through `casaos-cli`); the app was
installed with the API call the store's Install button sends, using the Compose file the store
served. A Linux host (Fedora, kernel 7.2) mounted the shares with the kernel SMB and NFS clients
and read every file. The 914 DSF and WAV files of the test corpus (136 images) were compared to
a reference made with the same image.

| Check | CasaOS 0.4.15 (Debian 12, kernel 6.1) | ZimaOS 1.8.0 (kernel 6.18) |
|---|---|---|
| Store source (branch zip) | accepted; sacdfs listed | accepted; sacdfs listed. A `store.json` URL is refused ("not a valid zip file"): ZimaOS 1.8.0 takes zips only |
| Install, healthy | 15.7 s after the install call, with the image pull | 20.6 s, with the pull |
| Container settings | `cap_drop: ALL`, the seven capabilities, `no-new-privileges`, AppArmor unconfined, not privileged, `/dev/fuse`, images read-only | the same |
| Capabilities held (`/proc/<pid>/status`) | sacdfs `SYS_ADMIN`, `DAC_OVERRIDE`; smbd `SETUID`, `SETGID`, `NET_BIND_SERVICE`; NFS-Ganesha `DAC_READ_SEARCH`; every process `NoNewPrivs: 1` | the same |
| SMB on 10445, as a guest | 914 of 914 identical | 914 of 914 identical |
| SMB as a user (set on the app's settings, then saved) | healthy 8.0 s after saving; 914 of 914 identical; a wrong password refused, guest refused | 6.7 s; the same |
| Writes (new file, append, delete) | refused over SMB and NFS | refused over SMB and NFS |
| NFS on 12049 | **fails**: the export mounts but every listing says "Unknown error 524" ([#439](https://github.com/aclater/sacdfs/issues/439)); with Debian's backports kernel (6.12) 914 of 914 identical | 914 of 914 identical |
| Stop, start (the app's buttons) | stopped in under 1 s, exit 0; healthy 5.9 s after start; 914 identical | the same; healthy 6.0 s after start; 914 identical |
| OS restart | came back by itself, healthy 25 s after the restart began; 914 identical | 47 s; 914 identical over SMB and NFS |
| Update 0.6.3 to 0.6.4 | listed as upgradable once the source carried 0.6.4; the update recreated the container on 0.6.4 (healthy 14 s, with the pull), kept the SMB user; 914 identical | the same (current 0.6.3, target 0.6.4); a read running during the update stopped with "Host is down" while the container was recreated |
| Uninstall | container, image, app network and `/DATA/AppData/sacdfs` removed; the images folder unchanged (checksums, owner, mode, time) | container and `/DATA/AppData/sacdfs` removed, the image kept; the images folder unchanged |
| sacdfs memory and CPU, reading every file over SMB | peak 69 MiB, 1.2 s of CPU (first read after a restart) | peak 76 MiB, 1.8 s (first read after install) |

For the update, the source was a local copy of the entry with the 0.6.3 image, then the same
copy with 0.6.4; CasaOS and ZimaOS reread their sources on a timer, which was triggered by
restarting their app-management service.

Platform notes:

- **Ports.** Both run their own Samba on 139 and 445; nothing used 2049, 10445 or 12049.
- **AppArmor** is on in both, so `apparmor=unconfined` stays. Runs without it were not tried.
- **The images folder** is a host path on the install form. ZimaOS keeps user folders under
  `/DATA` as CasaOS does.
- **ZimaOS** has SSH off by default and refuses root over SSH. The checks used the web user over
  SSH with `sudo`. ZimaOS's Docker (27.5) shows the image by tag; the Compose file it keeps has
  the digest, and the pulled digest is the pinned one.

**Checked with the store's own tooling.** In a copy of the store repository (`main`,
2026-11-15) with the app added as `Apps/SACDFS`: the store's `validate_compose.py` (its pull
request check, which runs `docker compose config -q`) reports 0 errors and 0 warnings with Docker
Compose 5.7. Its `./scripts/build_dist.sh` (build-appstore-action 1.1.2) builds the entry into a
v2 store (`store.json`, `index.json`, per-architecture Compose files), with the digest kept.
These ran in a Fedora container.

**Not checked:** the web UI screens (the API calls behind them were used), a Windows client,
ARM hardware, NFS on CasaOS with its own kernel (it fails, see above), and IceWhale hardware.

## Cosmos Cloud: the servapp

Directory: [deploy/cosmos/](../../deploy/cosmos/), in the layout of the Cosmos servapps store
(`servapps/sacdfs/`: `cosmos-compose.json`, `description.json`, `icon.png`, `screenshots/`),
plus `index.json`, a market source for this folder. The install form asks for the folder that
holds your images (a folder picker; it is mounted read-only), the formats, an optional SMB user
and password (the password field is filled with one Cosmos generates), and the NFS clients.
The cache is a named volume, `<name>-cache`. The app needs Cosmos 0.17.0 or later (the folder
picker).

**Install it today** from the market source: in Cosmos, Market, Sources, Add Source, with the
URL of `deploy/cosmos/index.json` as a raw file on GitHub. sacdfs then shows in the market
with the store's apps. It is not in the Cosmos store yet.

**What was checked on Cosmos Cloud (0.6.4).** Cosmos 0.24.0, installed with its documented
`docker run` command (the version tag pinned), on Debian 12.15 (kernel 6.1, Debian's Docker
20.10.24) in a VM with 4 GiB and 2 vCPUs, not on dedicated hardware. The test images were the
136 files of the test corpus in a folder on the VM's disk. The client was a Linux host (Fedora
44, kernel 7.2) mounting the shares with `mount.cifs` (SMB 3.1.1) through the VM's forwarded
ports.

The market source was added with the call the Sources dialog makes, and the market listed
sacdfs from it, with its icon, tags and architectures. The app was then installed with the
calls the install dialog makes: the template from that source, rendered as the 0.24.0 client
renders it (the same templating library, with the form's values), sent to `/api/docker-service`.
The dialog itself was not clicked through in a browser.

| Check | Guest access | SMB user and password, NFS clients set |
|---|---|---|
| Install, image `ghcr.io/aclater/sacdfs:0.6.4` pinned by digest | the create call returned after 12.8 s (with the image pull); healthy 42 s after it (the first health check runs 30 s after the start) | 2.0 s; healthy 32 s after it |
| Container settings, from `docker inspect` | Cosmos kept the template as written: `cap_drop: ALL`, the seven capabilities, `no-new-privileges`, AppArmor unconfined, not privileged, `/dev/fuse`, the images folder read-only, the health check, a 30 s stop timeout | the same |
| Capabilities of each program (`/proc/<pid>/status`) | entry point: the seven; `sacdfs`: `SYS_ADMIN`, `DAC_OVERRIDE`; `smbd`: `SETUID`, `SETGID`, `NET_BIND_SERVICE`; NFS-Ganesha: `DAC_READ_SEARCH`; each with no new privileges and a seccomp filter | the same |
| SMB on 10445: the 914 DSF and WAV files of the corpus identical to a reference | pass (5.8 s) | pass, as the user (5.0 s) |
| Wrong password | - | refused; a guest is refused too |
| Read-only share | creating, appending to and deleting a file refused | the same |
| NFS on 12049 | **fails**: the client mounts, then every read gets error 524 (not supported); see below | not run |
| Stop, start (Cosmos's buttons' calls) | 0.6 s, 0.5 s; healthy 30 s after the start; 914 files identical | not run |
| VM restart | the app came back by itself (restart policy `unless-stopped`), healthy 49 s after the restart began; 914 files identical | not run |
| Upgrade 0.6.3 to 0.6.4 | not run | see below: pass |
| Uninstall (stop, then the delete dialog: container, network, volume) | 1.8 s; no container, volume, network or FUSE mount left; the images folder unchanged (the same SHA-256 of all 136 files as before the install) | 2.3 s; the same |

**Resources.** Over a full SMB read of the 914 files right after a start (4.8 s), `sacdfs` used
1.1 s of CPU and peaked at 76 MB resident.

**Upgrade.** Cosmos has no version update for an installed market app: its Update button pulls
the same image reference again, and a reference pinned by digest never changes. The way to
move to a new version is the image field on the container's Docker tab. The app installed from
the same template with the 0.6.3 image: 914 files identical over SMB as the user. Then the
image was set to 0.6.4 there (the call that tab makes). It took 1.6 s, and the app was healthy
32 s later, running 0.6.4. Every setting was kept (capabilities, security options, device,
mounts, ports, health check, restart policy) and the cache volume was reused. The 914 files were
identical as the user, and a wrong password was refused.

**NFS on Debian 12's kernel.** On this VM the NFS export can't be read: Linux clients mount it,
then get error 524 (NFS4ERR_NOTSUPP), and NFS-Ganesha logs `Failed to get attrs for referral
... Operation not supported`. It isn't Cosmos: a kernel client inside the VM fails the same
way, and so does the container with seccomp unconfined. The same image serves NFS on Fedora's
kernel 7.2 and on TrueNAS. Tracked in [#439](https://github.com/aclater/sacdfs/issues/439). SMB is not affected.

**Platform notes.**

- Cosmos itself runs privileged, on the host network, with the Docker socket and the host's `/`
  mounted, as its documented command has it. It uses port 80 (and 443 with HTTPS), so 10445 and
  12049 were free. Debian has no SMB or NFS server by default.
- Cosmos puts the app on its own network, `cosmos-sacdfs-default`, and publishes the two ports
  from there.
- Debian 12 loads AppArmor (12 profiles in enforce mode). Docker's default profile would
  forbid the FUSE mount, hence `apparmor=unconfined` in the template. Cosmos passes it through.
- The images folder is chosen in the form, default `/cosmos-storage/music` (Cosmos's default
  data path). Cosmos creates a bind folder that doesn't exist.
- The SMB password is an environment variable, so it shows on the container's Docker tab, as
  every environment variable does there.
- The delete dialog is offered only for a stopped app, and leaves the image in Docker's image
  store.

**Not checked on Cosmos:** clicking the market and install dialogs in a browser (the same API
calls were made instead), NFS (the failure above), a Windows client, Cosmos's HTTPS and
Constellation, its automatic updates, and arm64.

**Checked with the store's own tooling.** In a copy of the store (`azukaar/cosmos-servapps-official`,
cloned 2026-11-15) with the app in `servapps/sacdfs`, the store's validator
(`.github/scripts/validate-servapps.js sacdfs`, Node 24.21 in a UBI 10 container) passes with no
warning: structure, `description.json`, the image and its architectures (amd64 and arm64)
checked against the registry, the template rendered and parsed, `minVersion` against the
features it uses. `index.js` builds the store index with the app. The validator requires the
`cosmos-icon` label to point at the store's own address, so the label carries that address; until the
app is in the store, Cosmos shows its default icon on the container, and the market listing
(from `index.json`) shows the real one.

## Umbrel: the app package

Directory: [deploy/umbrel/sacdfs/](../../deploy/umbrel/sacdfs/), in the layout of an app folder of the
official Umbrel App Store (`umbrel-app.yml`, `docker-compose.yml`, `data/`). **It is not in the
Umbrel App Store**, and Umbrel installs apps only from an app store source, so there is no
supported way yet to install it on umbrelOS. A community app store would need a repository of its
own (umbrelOS reads `umbrel-app-store.yml` at the root of a repository's default branch); none
exists.

The package runs two services:

- `server`: the published image `ghcr.io/aclater/sacdfs:0.6.4`, pinned by digest, with the FUSE
  device, all capabilities dropped but the seven the programs need
  ([why each one](../deployment.md#privileges)), no new privileges, AppArmor unconfined, not
  privileged. SMB on port 10445 and NFS on 12049. The images are mounted read-only at `/images`,
  the index cache is in the app's data folder.
- `web`: a static page (Red Hat UBI 10 nginx image, pinned by digest) with the addresses of the
  shares, because the Umbrel App Store asks every app to open to a page in the browser and sacdfs
  has none of its own. umbrelOS puts its login in front of it.

SMB asks for the user `umbrel` and the password umbrelOS shows for the app. The folder of images
is the app's **SACD images** setting (umbrelOS 2.0's folder access, read-only), by default the
Downloads folder; the format, the folder layout and the NFS clients are settings too.

**What was checked on umbrelOS (0.6.3 and 0.6.4).** On umbrelOS 2.0.1 (amd64) in a VM, not on
an Umbrel device, with the package copied into the device's App Store source as Umbrel's own test
instructions describe, and every step run through umbrelOS's API (the calls behind the App Store
and the app's settings). The 136 images of the test corpus were in a folder in Home.

| Check | Result |
|---|---|
| Umbrel's App Store linter (`lint:apps --check-images`) | no errors once `submission` holds a pull request link (it must, before a pull request exists the linter reports it empty); warnings for the device, the capabilities and the security options, which the store asks to justify in the pull request |
| Install (0.6.3 package) | install call returned in 2.8 s; the SMB and NFS container healthy 7.8 s after it, images already pulled |
| Status page | opens through umbrelOS's app gateway after login, over HTTP and HTTPS; without login the gateway redirects to it |
| Images folder | set to a folder in Home in the app's settings: the app restarted with that folder mounted read-only |
| Container settings | `cap_drop: ALL`, the seven capabilities, `/dev/fuse`, `no-new-privileges`, `apparmor=unconfined`, not privileged, as written in the package |
| Capabilities of each program (0.6.4, from `/proc`) | sacdfs: `SYS_ADMIN`, `DAC_OVERRIDE`; smbd: `SETUID`, `SETGID`, `NET_BIND_SERVICE`; NFS-Ganesha: `DAC_READ_SEARCH`; no new privileges for all |
| Update 0.6.3 to 0.6.4 | umbrelOS offered the update once the 0.6.4 package was in the source; the update ran (healthy on 0.6.4 16 s after the call), the images folder setting and the index cache kept |
| SMB as the user: all 914 DSF and WAV files identical to a reference | pass (on 0.6.3 and on 0.6.4) |
| SMB as a guest (user name cleared in the app's settings) | pass, 914 files |
| NFS on 12049 | pass, 914 files |
| Wrong password; guest while a user is set | both refused |
| Read-only (new file, append, delete) | refused over SMB and NFS |
| Stop, start, restart in umbrelOS | stop 0.9 s; healthy 8 s after a start; 914 files identical after each |
| umbrelOS restart | the app came back by itself, healthy about 25 s after the restart call; 914 files identical over SMB and NFS |
| Uninstall | 1.2 s; containers and the app's data folder (with the cache) removed; the images folder unchanged (size, time and SHA-256 of every file) |
| sacdfs memory and CPU, all 914 files over SMB | at most 94 MiB resident; under a second of CPU |

The client was Fedora 45 (kernel SMB 3.1.1 and NFSv4 mounts) on the VM's host, through the VM's
port forwarding.

**Not checked on umbrelOS:** an Umbrel device or arm64; the App Store screens and the settings
screen themselves (the same calls were made through umbrelOS's API); a community app store; a
Windows or macOS client; umbrelOS's own file sharing switched on at the same time (it uses port
445, the app 10445).

## Runtipi

Not built. The Runtipi app store takes no new apps: its README says the repository still gets
updates but no new applications, and asks for bug fixes only. Its maintainers point new apps to
custom app stores. Nothing in the store's format would stop sacdfs: its compose schema has the
capabilities, devices, security options, read-only volumes and extra ports the app needs. So a
custom app store, or Runtipi's "Add custom app" with [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml),
is the way to run sacdfs on Runtipi. Neither was tested.

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
  Run on a Linux host with rootless Podman standing in for Docker. Up to 0.6.3 one message was
  logged on every render: "No cap_drop for service: sacdfs", because the app kept the runtime's
  default capabilities; from 0.6.4 the app drops them all (see above).
- **Re-run for 0.6.4 (#386):** `ci.py --render-only` with the catalog's library 2.3.18 renders
  both test value sets with `cap_drop: [ALL]`, the seven capabilities in `cap_add`, and
  `no-new-privileges`. Run on Fedora 44 in a VM with Docker. The app itself was not installed on
  TrueNAS.
- **Re-run for 0.6.4 (catalog `main` cloned 2026-11-15, library 2.3.18):** with the image tag set
  to 0.6.4, pinned by digest as the catalog's other apps do (`0.6.4@sha256:2039b43f…`, the
  release's multi-arch index, which the TrueNAS install below pulled), the catalog's `generate_metadata.py` set `app_version` to 0.6.4 and raised `version` to
  1.0.2; the committed `app.yaml` is that output, and a second run changes nothing. `ci.py
  --render-only` renders both test value sets with no warning, its `docker compose config` step
  included (Docker Compose 2.32.4 behind rootless Podman); `check_metadata.py`,
  `port_validation.py` and `apps_dev_charts_validate validate` pass. The catalog's CI also runs
  `ci.py` without `--render-only`, which starts the app with Docker Compose and waits for it to
  turn healthy; that run needed the test value sets to let the images folder be created
  (`create_host_path: true`), and then passed for both on Fedora 44 in a VM with Docker 29.9 and
  Compose 5.7 (SELinux permissive, as the catalog's CI hosts have none). The rendered files were
  installed on TrueNAS (above).
- **Re-run for 0.6.3 (catalog `main` cloned 2026-11-14):** with the image tag and `app_version`
  set to 0.6.3, the catalog's `generate_metadata.py` raised `version` to 1.0.1 and reworded three
  capability descriptions; the committed `app.yaml` is that output. `ci.py --render-only` renders
  both test value sets with no warning; `check_metadata.py`, `port_validation.py` and
  `apps_dev_charts_validate validate` pass. Rootless Podman stood in for Docker, so the last
  `docker compose config` print of `ci.py` didn't run; the rendered files were installed on
  TrueNAS instead (above).
- **Unraid Community Applications:** `deploy/unraid/sacdfs.xml` and `ca_profile.xml` are well
  formed and pass a check of the elements and `Config` attributes that the community CA starter
  kit's validator requires (written out by hand here, because Unraid publishes no validator).
  Unraid's own validate and scan on `ca.unraid.net/submit` has not been run: it needs the owner's
  account.

## Before a store listing

Listing is up to each store's maintainers. The TrueNAS app is submitted to the TrueNAS apps
repository and waits for review there. Nothing is submitted to the other stores yet.

- **Unraid:** the repository has the icon, the template and `ca_profile.xml` (the repository
  profile Community Applications requires in the root). Still needed: an Unraid forum account and
  a support thread.
- **TrueNAS:** in the pull request the catalog's tooling sets `date_added`, and the reviewers
  upload the icon and fill the CDN and `apps.truenas.com` addresses that `app.yaml` already names
  (they don't answer yet). The generated notes list the app's user as "unknown", because it runs
  as root.
- **CasaOS and ZimaOS:** a pull request to the store adding `Apps/SACDFS`; the icon address moves
  to the store's own CDN path.
- **Cosmos Cloud:** a pull request to the servapps store adding `servapps/sacdfs`; regenerate
  `index.json` for `main` first (`deploy/cosmos/make-index.py main`).
- **Umbrel:** a pull request to the Umbrel App Store with the pull request's link in
  `submission`, screenshots, and the reasons for the device, capabilities and security options
  that the store asks for. arm64 was not checked on an Umbrel device.
