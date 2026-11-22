# Install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that players read.
It runs next to the images, normally on the NAS that holds them. There are two ways to install it:

- **The installer:** one line typed on the NAS or Linux computer. Start here.
- **A guide for your system:** the same steps by hand. Use one if the installer doesn't fit
  ([pick a guide](#pick-a-guide)).

## The installer: one line

The installer finds out what it runs on and sets sacdfs up the way the guide for that system
does. About 5 minutes.

### What you need

- A NAS or computer running Linux. The installer doesn't set up a Mac or a Windows PC: each has
  its own guide ([windows.md](windows.md): a release build; [macos.md](macos.md): a release build
  since 0.6.5), and either can play the discs over the network from a NAS
  ([platforms](../../README.md#platforms)).
- A login over SSH (a way to type commands on the NAS from another computer) as an administrator.
- The folder that holds your SACD images. This page uses `/volume1/music`; change it to yours.

### Steps

1. **Log in over SSH.** On your computer (Windows: PowerShell; macOS: Terminal) type
   `ssh youradmin@your-nas-address`.
2. **See what it would do.** Type this, with your folder. It changes nothing:

   ```sh
   curl -fsSL https://github.com/aclater/sacdfs/releases/latest/download/install.sh \
     | sudo sh -s -- --iso-dir /volume1/music --dry-run
   ```

   *You should see* `sacdfs installer: ... detected (dry run: nothing is changed)`, followed
   by the changes it would make. Leave out `sudo` if you are already root (`id -u` prints `0`;
   on QNAP the `admin` login is).
3. **Install.** The same line without `--dry-run`:

   ```sh
   curl -fsSL https://github.com/aclater/sacdfs/releases/latest/download/install.sh \
     | sudo sh -s -- --iso-dir /volume1/music
   ```

   *You should see* a line starting `Done.` It names either the folder that holds the DSF and
   WAV files (for example `/volume1/sacd`) or, with Docker or Podman, the SMB and NFS addresses
   to give a player. sacdfs then starts by itself whenever the NAS starts.
4. **On TrueNAS or Unraid** it changes nothing and names the guide to follow instead:
   [truenas-scale.md](truenas-scale.md) or [unraid.md](unraid.md).

### Check it worked

- **It named a folder:** list it, for example `ls /volume1/sacd`. *You should see* one folder
  per disc. Share that folder with the NAS's own file sharing (SMB or NFS), as you share any
  other folder.
- **It named SMB and NFS addresses:** mount one from another computer
  ([Linux](linux-client.md#on-other-ports), [Windows and macOS](../deployment.md#mounting-the-shares-on-clients)).

Then set up your player: [which player](#3-which-player-or-music-server).

### If something goes wrong

- **You see:** `sacdfs is installed as root, and you aren't`. **Do this:** type the line it
  prints underneath, which adds `sudo`. Nothing was changed.
- **You see:** `curl: command not found`. **Do this:** download it with `wget`, then run it
  (don't pipe `wget` into `sh`):

  ```sh
  wget -O install.sh https://github.com/aclater/sacdfs/releases/latest/download/install.sh && sudo sh install.sh --iso-dir /volume1/music
  ```
- **You see:** it stops with a block of system details and a link. **Do this:** sacdfs doesn't
  run on that system yet, and nothing was changed. Paste the block into the
  [unsupported system form](https://github.com/aclater/sacdfs/issues/new?template=unsupported-system.yml).

To remove sacdfs again, run the install line with `--uninstall` at the end. Your images stay.
More about the installer, and where it was checked: [Details](#the-installer-in-detail).

## Pick a guide

Each guide does by hand what the installer does. Answer the questions in order and stop at the
first that fits. Each guide is one page: follow it from top to bottom.

## 1. What will sacdfs run on?

sacdfs runs next to your SACD images (`.iso` files), normally on the NAS that holds them.

**Is it one of these NAS systems?**

| Your NAS | Guide |
|---|---|
| Synology DSM | [synology.md](synology.md) |
| QNAP QTS or QuTS hero | [qnap.md](qnap.md) |
| ASUSTOR ADM | [asustor.md](asustor.md) |
| TerraMaster TOS | [terramaster.md](terramaster.md) |
| UGREEN UGOS Pro | [ugreen-ugos.md](ugreen-ugos.md) |
| Netgear ReadyNAS OS 6 | [readynas.md](readynas.md) |
| TrueNAS SCALE | [truenas-scale.md](truenas-scale.md) |
| Unraid | [unraid.md](unraid.md) |
| OpenMediaVault | [openmediavault.md](openmediavault.md) |

**Not on the list, but the computer runs Linux and has Docker or Podman?**
Use [docker-podman.md](docker-podman.md). It is the simplest route, and it shares the
discs over SMB and NFS for you.

**Linux without a container (a PC, a Raspberry Pi, a NAS with only SSH)?**
Use [binary.md](binary.md) to run sacdfs on your own computer, or
[nas-binary.md](nas-binary.md) to run it on a NAS you reach over SSH.
On Fedora, RHEL, Debian or Ubuntu, [packages.md](packages.md) installs it as an `.rpm` or
`.deb` package with a systemd service.

**A Mac, with the images on it or on a share it has mounted?**
Use [macos.md](macos.md). It downloads the macOS release build (Apple Silicon or Intel, since
0.6.5) and mounts the discs on the Mac itself through its own NFS client, with no macFUSE.

**A Windows PC that holds the images?**
Use [windows.md](windows.md): `sacdfs-win`, from the release zip (Windows 10 and 11, x64, with
WinFsp), shows them as a read-only drive letter on that PC.

## 2. How do other computers reach the discs?

sacdfs shares them over SMB (Windows, macOS, Linux) and NFS (Linux). The guide in step 1 sets
that up. On the computer you listen at:

- **Linux:** [linux-client.md](linux-client.md).
- **Windows or macOS:** [the deployment guide](../deployment.md#mounting-the-shares-on-clients).

## 3. Which player or music server?

| Player | Guide |
|---|---|
| Roon | [roon.md](roon.md) |
| Plex | [plex.md](plex.md) |
| Jellyfin | [jellyfin.md](jellyfin.md) |
| Navidrome | [navidrome.md](navidrome.md) |
| Lyrion (Logitech Media Server) | [lms.md](lms.md) |
| MPD, moOde or Volumio | [mpd.md](mpd.md) |
| Something else | [clients](../clients.md) lists what was tried |

## Before you upgrade

A new image, a new binary or a restart interrupts SMB and NFS for about 5 to 8 seconds.
Linux and Windows clients carry on by themselves. **A macOS client loses the SMB share and
must mount it again** after every upgrade or restart. Never stop sacdfs with `kill -9`: use the
stop step in your guide ([why](nas-binary.md#start-at-boot-and-stop-safely)).

Upgrading from 0.4.0 with albums catalogued by beets-pysacd: follow
[these steps](../user-guide.md#tagging-behaviour) ("Upgrading from 0.4.0 with beets-pysacd") before any `beet update`.

## Details

- **Where each guide was checked** is at the end of that guide, under Details. Guides say
  "not tested" for steps that were not run on the release they name.
- **Every setting and the reasons for it:** [deployment guide](../deployment.md). **What each
  player was tested with:** [clients](../clients.md).
- **Without a container, on any Linux host:** a [release binary](binary.md#binaries), or
  `cargo install --locked sacdfs` ([from source](binary.md#from-source)).
- **Unraid and TrueNAS app-store entries:** [store-apps.md](store-apps.md).
- **Have a NAS we have not run on?** Reports from real hardware are welcome on
  [#82](https://github.com/aclater/sacdfs/issues/82); see [beta testing](../beta-testing.md).

### The installer in detail

The installer has been in every release since 0.6.2.

**What it does:**

- **Docker or Podman with Compose answers:** runs the container from
  [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml) with your folder, and shares the discs
  over SMB on port 10445 and NFS on port 12049 (`--share smb`, `nfs` or `both`, the default).
- **Synology, QNAP, ASUSTOR without containers:** the release binary and `sacdfs-ctl` in a
  folder at the top of the images' volume (`sacdfs`; on QNAP the hidden `.sacdfs`), started at
  boot by the NAS's own hook (Task Scheduler, `autorun.sh`, `init.d`), as in each NAS's guide;
  see [where the files go](#where-the-files-go).
- **TerraMaster, UGREEN, ReadyNAS and other Linux with systemd:** the release binary and the
  systemd unit, as in [terramaster.md](terramaster.md).
- **TrueNAS and Unraid:** nothing. Their apps belong in their own screens; it points to
  [truenas-scale.md](truenas-scale.md) or [unraid.md](unraid.md).

It downloads the release archive it was published with (`--version` picks another), checks it
against the checksum built into the release's `install.sh` (since 0.6.4; see below), and tries
the binary before installing anything. It never writes
under `--iso-dir`. A second run changes nothing, `--uninstall` removes what it installed (your
images stay), and `--help` lists the rest (`--mount`, `--prefix`, `--share-name`, `--method`,
`--report`). Not root, it stops before changing anything and prints the exact line to run with
`sudo`.

On QNAP, the installer switches on "Run user defined processes" if it is off, so that
`autorun.sh` starts sacdfs at boot. Since 0.6.3, `--uninstall` switches it off
again only if the installer switched it on and `autorun.sh` runs nothing else; otherwise it leaves
it on and says why (0.6.2's `--uninstall` always leaves it on). *(Checked on QuTScloud in a VM,
not on the vendor's hardware.)*

#### Where the files go

Root runs sacdfs's files at every start, so since 0.6.4 the installer puts them where only root
can change them, and checks that before it writes anything
([GHSA-wfjg-pqhm-4mm2](https://github.com/aclater/sacdfs/security/advisories/GHSA-wfjg-pqhm-4mm2)):

| System | sacdfs's files (`--prefix`) | Folder to mount on (`--mount`) | Why there |
|---|---|---|---|
| QNAP | `/share/CACHEDEV1_DATA/.sacdfs` | `/share/CACHEDEV1_DATA/sacd` | The top of the data volume, next to QNAP's own `.qpkg`: kept across restarts and firmware updates, and not a shared folder. QNAP's system folders are a RAM disk. Up to 0.6.3 this was a `sacdfs` folder inside the images' shared folder, where share users could swap it. |
| Synology, ASUSTOR, UGREEN | `/volume1/sacdfs` | `/volume1/sacd` | The top of the data volume, kept across firmware updates; `/root` and `/etc/default` are small or not kept. Unchanged. |
| TerraMaster | `/usr/local/bin/sacdfs`, `/etc/default/sacdfs` (systemd) | `/Volume1/sacd` | As in its guide. Unchanged. |
| ReadyNAS | `/usr/local/bin/sacdfs`, `/etc/default/sacdfs` (systemd) | `/data/sacd` | As in its guide. Unchanged. |
| Other Linux | `/usr/local/bin`, `/etc/default` (systemd) or `/opt/sacdfs` | `/srv/sacd` | Unchanged. |

- **The check:** that folder, the folder to mount on, and every folder above them up to `/`
  (through symbolic links) must be owned by root and not writable by their group or by other
  users (a folder with the sticky bit, such as `/tmp`, is fine), and the folder to mount on
  must not be a link. A folder it would make must not go straight into a folder other users can
  write to, even one with the sticky bit (they could make it first). Otherwise it stops, changes nothing, and names the folder and the fix: a
  `chmod go-w` or `chown 0`, or another `--prefix` or `--mount`. There is no option to skip it.
  A shared folder you make for the DSF and WAV files may itself be writable: only the folders
  above it matter, since nobody but root can then replace it.
- **Folder names:** the images folder, `--mount` and `--prefix` may hold letters (any
  language), digits, spaces and `. _ - + , @ = & ( ) [ ]`. Anything else (`;`, `$`, quotes,
  `#`, `\`, ...) stops it before anything is written; rename the folder. The values are written
  in single quotes, and `sacdfs-ctl` reads its settings as data, never as shell code.
- **Upgrading from 0.6.3 or earlier:** run the installer again as you first did. On QNAP, and
  wherever the start-at-boot hook or the running sacdfs points to another folder, it installs
  into the new place, points the hook there, stops the old sacdfs without running anything
  from the old folder, starts the new one, and removes the old files. An old folder that is a
  symbolic link, or is reached through one, is left alone with a warning: removing files
  through a link could hit another folder. On QNAP the DSF and WAV
  files then appear in `/share/CACHEDEV1_DATA/sacd`: share that folder and point players at
  it. `--uninstall` finds both places.
- **Checksums:** each release's `install.sh` carries the SHA-256 of that release's archives,
  and is itself attested (`gh attestation verify install.sh -R aclater/sacdfs`). A download
  that doesn't match stops it. With `--version` for another release, it can only compare with
  the `.sha256` from the same server: it says so, and then needs `curl` (HTTPS only, TLS 1.2
  or later) or GNU Wget, which check the server's certificate; BusyBox's `wget` doesn't, so
  there it stops. `--archive` with a release's own archive is checked against the built-in
  value too.
- **Release files next to `install.sh`:** run from an unpacked release archive, `install.sh`
  verifies release files beside it against built-in checksums: it copies `sacdfs`,
  `sacdfs-ctl`, `sacdfs.service` and `nas-compose.yaml` into its temporary folder, checks each
  copy against the SHA-256 the release workflow built into it, and uses only those copies. If
  one is missing or doesn't match, it says which, uses none of them and downloads the release
  as the one-liner does. Only a released `install.sh` has these checksums: a copy of
  `install.sh` from the repository (deploy/ in a clone, or main) always downloads. The
  `install.sh` inside an archive takes that archive's files; the one published on its own
  takes only the files of the archive it would download for this machine. A local binary
  that doesn't start here (the x86-64-v3 build on an older CPU, say) is set aside with the
  other local files, and the release for this machine is downloaded instead of stopping.

*(Checked on a 0.6.4 development build: in the BusyBox and Fedora 44 test containers that CI
runs (deploy/ci-install-security-steps.sh: refusals, the folder names above, settings files of
0.6.3, a wrong or missing built-in checksum, the move from a QNAP shared folder and from an
ASUSTOR `--prefix` in one, `--uninstall` of both); and in VMs, not on the vendor's hardware, on
QNAP QuTScloud 5.2.9 (a hand-made ext4 volume: a 0.6.3 install in the shared folder moved,
`autorun.sh` rewritten and run as at start-up, a fresh install from an `R&B` folder,
`--uninstall`), TerraMaster TOS 7.0 (a volume made by TOS: the systemd route upgraded from 0.6.3,
`Rock & Roll` and `Live (1972)` read through systemd's settings file, a 0.6.3 `--prefix` in the
TOS shared folder `public` refused and then moved, `--uninstall`) and Synology DSM 7.4.1 (a
0.6.3 install with `--prefix` in a shared folder moved, both Task Scheduler tasks replaced, a
restart that started the new one, `--uninstall`). Each time all 28 DSF and WAV files of three
corpus images were identical to a reference. Also on ReadyNAS OS 6.10.10 (systemd 230; Debian's
`/usr/local` and `/usr/local/bin` are `root:staff` 2775 there; those two are accepted only while
the `staff` group has no members, listed or as a primary group, including directory-service
ones, and otherwise refused with the members named): the
systemd route upgraded from 0.6.3, single-quoted `Rock & Roll` and `Live (1972)` read by systemd,
a fresh install, `--uninstall`; and on ASUSTOR ADM 5.1.4: a 0.6.3 install with `--prefix` in a
shared folder moved through `S99sacdfs`, the boot scripts and a restart starting the new one, a
fresh install from `R&B`, `--uninstall`. Not checked: a restart on QNAP (QuTScloud in the VM
didn't keep its settings across one), QNAP's own volumes and their folder modes, UGREEN and
OpenMediaVault in VMs, the container route on a NAS, and systemd 215 itself (its source reads
single-quoted values the same way, for the characters the installer allows).)*

**To read it before you run it,** download it, check it, and run it:

```sh
curl -fLO https://github.com/aclater/sacdfs/releases/latest/download/install.sh
curl -fLO https://github.com/aclater/sacdfs/releases/latest/download/install.sh.sha256
sha256sum -c install.sh.sha256      # install.sh: OK
less install.sh
sudo sh install.sh --iso-dir /volume1/music --dry-run
sudo sh install.sh --iso-dir /volume1/music
```

`gh attestation verify install.sh -R aclater/sacdfs` also shows that this repository's release
workflow built it. *(Checked on 0.6.2 from the `latest` address in both forms above, and the
attestation passes; see [the installer on 0.6.2](../test-plan.md#results-installer-on-062).)*

**Why not `wget | sh`:** if the download fails, `sh` gets an empty script and ends without an
error, having done nothing. With `wget -O install.sh ... && sh install.sh`, a failed download
stops with `wget`'s error before anything runs. BusyBox's own `wget`, on systems that have no
other, failed against GitHub with a TLS error in our BusyBox 1.37 test image (and it doesn't
check the server's certificate): there this form stops with status 1. Then download
`install.sh`, the release archive for your CPU and its `.sha256` on another computer, copy all
three to the NAS, and run `sh install.sh --iso-dir ... --archive sacdfs-<version>-x86_64-linux.tar.gz`
(checked on 0.6.2-rc.1 in that BusyBox image).
*(Checked on 0.6.2 as a dry run with the NAS's own `wget`, GNU Wget, on QNAP QuTScloud, Synology
DSM, TerraMaster TOS, ReadyNAS OS, Debian 12 with UGREEN's kernel and OpenMediaVault 8, in VMs,
not on the vendor's hardware; and with BusyBox 1.37's `wget` in a test container, where it stops
with status 1 and runs nothing.)*

**A download cut short:** since 0.6.3, the installer stops with an error, and changes nothing,
when a download piped into `sh` is cut short. *(Checked with BusyBox 1.37 `sh`, dash and bash, in
the BusyBox and Fedora 44 test containers that CI runs, and from the 0.6.3-rc.1 release on QNAP
QuTScloud and Synology DSM in VMs, not on the vendor's hardware; 0.6.2's installer doesn't have this
check.)*

**A system sacdfs can't run on** makes it stop before it downloads anything, since 0.6.4: a
processor other than 64-bit x86-64 or aarch64 (also for the container route), a 64-bit kernel
that runs 32-bit programs (Raspberry Pi OS 32-bit on a Pi 4 or 5 reports `aarch64`), a glibc
older than 2.17 or musl instead of glibc (for the binary; the container brings its own), no
`/dev/fuse` that `modprobe fuse` can load, or `--method compose` with no Docker or Podman that
answers. It then prints a short block of system details and a link to the
[unsupported system form](https://github.com/aclater/sacdfs/issues/new?template=unsupported-system.yml):
paste the block there to ask for that system. After a download, it still tries the binary
before installing it, and stops the same way if it doesn't start. A Mac or Windows PC gets the
"Linux only" message, with no block. On 32-bit x86 (an `i686` kernel, or an x86-64 kernel with
32-bit programs) it stops the same way, but from 0.7.1 prints no block or form: it names the
release's `i686` archive, with its download link, to install by hand, and links
[32-bit x86](binary.md#32-bit-x86).

**`--report`** prints the same block on any Linux system and changes nothing. It needs neither
root nor `--iso-dir`:

```sh
curl -fsSL https://github.com/aclater/sacdfs/releases/latest/download/install.sh | sh -s -- --report
```

The block holds the installer's version, the check that stopped it, the processor (`uname -m`,
32- or 64-bit programs, model, cores), memory, the OS and NAS firmware version, kernel, C
library, `/dev/fuse`, systemd and the container engine. It holds no host name, address, serial
number, user name or folder, and nothing is sent anywhere: it is only printed.
*(0.6.3 and older don't have `--report`. Checked with BusyBox 1.37 `sh`, dash and bash, in the
BusyBox and Fedora 44 test containers that CI runs, with a fake `uname`, `getconf` and `ldd`:
armv7l, aarch64 with 32-bit programs, glibc 2.12 and musl each stop with status 1, the
block and the link, i686 and x86-64 with 32-bit programs with the `i686` archive's name and
link instead (from 0.7.1), and a local server the installer was pointed at got no request. The same
cases, `--report` and the whole install cycle with the 0.6.3 release were also run on QNAP
QuTScloud 5.2.9 (BusyBox, no systemd; a hand-made ext4 volume) and TerraMaster TOS 7.0
(systemd), checked in VMs, not on the vendor's hardware. The 32-bit x86 stop was also run in a
CentOS 7 i386 userland with no network, under `linux32` and without it. Not run: an i686 kernel,
32-bit ARM or musl systems, other NAS systems.)*

**Where the installer was checked,** on the 0.6.2 release from GitHub, in both forms (the
one-liner, and download, check, run): a dry run that changed nothing, the install, all 28 DSF and
WAV files of three conformance-corpus images identical to a reference, a second run that changed
nothing, `--uninstall` back to the start (and again, with nothing to remove), and the images
folder unchanged. Details: [the installer on 0.6.2](../test-plan.md#results-installer-on-062).

| System | Route it took | Result |
|---|---|---|
| Synology DSM 7.4.1, in a VM | `sacdfs-ctl`, Task Scheduler tasks | pass |
| QNAP QuTScloud 5.2.9, in a VM (a hand-made ext4 volume) | `sacdfs-ctl`, `autorun.sh` | pass |
| ASUSTOR ADM 5.1.4, in a VM | `sacdfs-ctl`, `init.d` | pass |
| TerraMaster TOS 7.0, in a VM (a volume made by TOS) | systemd | pass |
| ReadyNAS OS 6.10.10, in a VM (a volume made by the vendor's tool) | systemd | pass |
| Debian 12 with UGREEN's kernel, in a VM (not UGOS Pro) | systemd | pass |
| OpenMediaVault 8.6 on Debian 13, in a VM (a file system made by OpenMediaVault) | systemd | pass |
| TrueNAS SCALE 25.10, on a NAS running it, as a normal user | points to the Apps screen, changes nothing | pass |
| Fedora 44 for aarch64, in a container under `qemu-user` | `sacdfs-ctl` (the aarch64 archive) | pass |

The VMs ran each vendor's OS, not on the vendor's hardware. On 0.6.2-rc.1 the same cycle also
passed on Fedora 45 (systemd and Docker Compose) and in a UBI 7 container
([results](../test-plan.md#results-062-rc1-gate)), and CI runs it on every change to `deploy/`
in BusyBox, Fedora and UBI 7 containers and on its runner with systemd and Docker Compose. With the 0.6.3 release files, a full power cycle after the install was run on Synology DSM,
ASUSTOR ADM, TerraMaster TOS, ReadyNAS OS and Debian 12 with UGREEN's kernel, and the container
route with Docker on ASUSTOR ADM, in VMs: each time sacdfs came back by itself with the same files
([results](../test-plan.md#results-installer-restarts-and-the-container-route-on-nas-vms-main-063-release-files)).
Not checked: Unraid, the container route on other NAS systems (QNAP Container Station and
Synology Container Manager don't install in the VMs), a QNAP restart, aarch64 hardware, real NAS
hardware, and QTS or UGOS Pro themselves.
