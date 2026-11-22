# Unraid and TrueNAS app-store entries

sacdfs has an Unraid Community Applications template and a TrueNAS catalog app definition in
the repository. The TrueNAS app is listed in the community train of the TrueNAS apps catalog
(since 2026-11-18): install it from Apps, Discover Apps, as in the
[TrueNAS SCALE guide](truenas-scale.md#install-from-the-apps-screen). The Unraid template isn't
listed yet. This page says what is in the repository, how to use the Unraid template today,
and what was checked where.

Both entries run the same published image as [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml):
`ghcr.io/aclater/sacdfs:0.6.5`, with the FUSE device, all capabilities dropped but the seven
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
formats, and storage for the index cache. **Mount Folder** (off by default) binds a dataset over
`/srv` with `rshared`, so sacdfs's files appear in it for TrueNAS's own SMB share on port 445
([guide](truenas-scale.md#share-through-truenass-own-smb-port-445)); **SMB Share From The App** and
**NFS Export From The App** (on by default) turn the app's own servers, their ports and their
capabilities off. These three are not in the listed catalog app yet (#522).

**Install from the catalog, not from this directory.** TrueNAS installs catalog apps only
from its catalog: Apps, Discover Apps, search `sacdfs` ([guide](truenas-scale.md#install-from-the-apps-screen)).
The listed app is the catalog's copy and is updated there: new sacdfs releases reach it through
the catalog maintainers' automated update pull requests, so it can trail a release. Its form
differs from this directory's: SMB and NFS ports default to 30602 and 30603, and it has no
Formats or One Folder Per Disc options. For those, use "Install via YAML"
([guide](truenas-scale.md#or-install-with-yaml)).

**What was checked from the catalog (0.7.0).** On TrueNAS SCALE 25.10.8 in a VM, with its
standard catalog: sacdfs listed in the community train, installed as a guest through the API
calls behind the Apps screen, healthy 11.7 s after the install call; 910 DSF and WAV files of the
test corpus identical to a reference over SMB; stop and start; uninstall. Details and what wasn't
run: [truenas-scale.md](truenas-scale.md#what-was-checked).

**What was checked on TrueNAS (0.6.4).** On TrueNAS SCALE 25.10.8 in a VM, not on TrueNAS
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

Not run on 0.6.4: a TrueNAS restart, the web form, a real client mounting the shares, updates.

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

Two things the catalog's maintainers weighed before listing it:

- The app runs as root with `/dev/fuse` and seven capabilities, among them `SYS_ADMIN` and
  `DAC_READ_SEARCH`. sacdfs cannot mount without them, nor NFS-Ganesha serve
  ([why each one](../deployment.md#privileges)); each program holds only its own share. The catalog's inclusion policy accepts
  only apps that stay within their container's boundaries; whether a FUSE mount does is their
  call.
- The catalog drops all capabilities and adds back a few, and the template now keeps that
  default (#386): it adds back the seven above. Before 0.6.4 it kept the runtime's default set,
  because Samba aborted without `SETUID` and `SETGID`.

## The icon

The mark is an original drawing of a disc rising out of a folder, with a waveform on the folder
(no third-party logo or trademark). Sources and sizes:

- [deploy/icon.svg](../../deploy/icon.svg): the master drawing.
- [deploy/icon.png](../../deploy/icon.png): 512 x 512 PNG, 8-bit with transparency, exported from the SVG.
  Unraid's template and `ca_profile.xml` point to its raw URL on `main`.
- TrueNAS hosts catalog icons on its own CDN. The app's `icon:` and `icon_url:` already carry the
  CDN path the catalog uses (`.../apps/sacdfs/icons/icon.svg`), which serves the SVG since the
  listing.

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
- **0.7.1:** the image tag set to 0.7.1, `app_version` 0.7.1 and `version` 1.0.4, by hand as the catalog's `generate_metadata.py` sets them, pinned by digest to the release's multi-arch index (`0.7.1@sha256:51191f1c…`). The catalog tooling and a TrueNAS install were not re-run on 0.7.1.
- **0.6.5:** the image tag set to 0.6.5, pinned by digest (`0.6.5@sha256:73eb56cb…`, the release's multi-arch index), `app_version` 0.6.5 and `version` 1.0.3, by hand as the catalog's `generate_metadata.py` sets them; the catalog tooling and a TrueNAS install were not re-run on 0.6.5.
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

Submission is the owner's step and is not done. The repository is ready for it: icon, template,
`ca_profile.xml` (the repository profile Community Applications requires in the root), and the
catalog app directory. Still needed from outside: an Unraid forum account and support thread, and
a pull request to the TrueNAS apps repository (and an issue there first, as their template asks). In that pull request the
catalog's tooling sets `date_added`, and the reviewers upload the icon and fill the CDN and
`apps.truenas.com` addresses that `app.yaml` already names (they don't answer yet). The
generated notes list the app's user as "unknown", because it runs as root.
