# Install sacdfs

## The installer: one line

From the release after 0.6.0, an installer finds out what it runs on and sets sacdfs up the way
the guide for that system does. Log in to the NAS or Linux computer over SSH, and type (change
the path to the folder that holds your SACD images, the `.iso` files):

```sh
curl -fsSL https://github.com/aclater/sacdfs/releases/latest/download/install.sh | sudo sh -s -- --iso-dir /volume1/music
```

Leave out `sudo` if you are already root (`id -u` prints `0`; on QNAP the `admin` login is). Without
`curl` (BusyBox systems): `wget -q -O - <the same address> | sh -s -- --iso-dir ...`. Add
`--dry-run` first to see every change it would make, and make none.

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
workflow built it. *(Untested: no release carries the installer yet. Downloading it from GitHub
and the attestation were not run; everything below was, with the script served from a local
copy of the release layout.)*

**What it does:**

- **Docker or Podman with Compose answers:** runs the container from
  [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml) with your folder, and shares the discs
  over SMB on port 10445 and NFS on port 12049 (`--share smb`, `nfs` or `both`, the default).
- **Synology, QNAP, ASUSTOR without containers:** the release binary and `sacdfs-ctl` in a
  `sacdfs` folder on the images' volume, started at boot by the NAS's own hook (Task Scheduler,
  `autorun.sh`, `init.d`), as in each NAS's guide.
- **TerraMaster, UGREEN, ReadyNAS and other Linux with systemd:** the release binary and the
  systemd unit, as in [terramaster.md](terramaster.md).
- **TrueNAS and Unraid:** nothing. Their apps belong in their own screens; it points to
  [truenas-scale.md](truenas-scale.md) or [unraid.md](unraid.md).

It downloads the release archive it was published with (`--version` picks another), checks it
against the release's checksum, and tries the binary before installing anything. It never writes
under `--iso-dir`. A second run changes nothing, `--uninstall` removes what it installed (your
images stay), and `--help` lists the rest (`--mount`, `--prefix`, `--share-name`, `--method`).
Not root, it stops before changing anything and prints the exact line to run with `sudo`.
Afterwards it prints where the files are or the address to give a player. With the binary,
share that folder with the NAS's own file sharing ([player guides](#3-which-player-or-music-server)).

**Where the installer was checked**, run as above (piped into `sh` from a local copy of the
release layout) with the 0.6.0 binary: a dry run that changed nothing, the install, all 28 DSF
and WAV files of three conformance-corpus images identical to a reference, a second run that changed nothing, `--uninstall` back to the start, and the images folder
unchanged.

| System | Route it took | Result |
|---|---|---|
| Synology DSM 7.4.1, in a VM | `sacdfs-ctl`, Task Scheduler tasks | pass |
| QNAP QuTScloud 5.2.9, in a VM (a hand-made ext4 volume) | `sacdfs-ctl`, `autorun.sh` | pass |
| ASUSTOR ADM 5.1.4, in a VM | `sacdfs-ctl`, `init.d` | pass |
| TerraMaster TOS 7.0, in a VM (a volume made by TOS) | systemd | pass |
| ReadyNAS OS 6.10.10, in a VM (a volume made by the vendor's tool) | systemd | pass |
| Debian 12 with UGREEN's kernel, in a VM (not UGOS Pro) | systemd | pass |
| Fedora 45, as root through `sudo` (SELinux enforcing) | `--method systemd`; and `--method compose` (Docker Compose, the 0.6.0 image) | pass |
| Containers: BusyBox only (no bash, no curl), Fedora 44, UBI 7 (glibc 2.17) | `sacdfs-ctl` | pass |
| Container with BusyBox and no C library | refused before changing anything | pass |

The VMs ran each vendor's OS, not on the vendor's hardware. On ASUSTOR and Synology a
development version of the script was also followed by a restart: sacdfs was mounted again by
the boot hook it set up, with the same files. Not checked: TrueNAS, Unraid and OpenMediaVault
themselves (only on simulated layouts: containers with each system's folders and identity
files, not its software, where detection and the whole cycle passed), the container route on a NAS, restarts on the other systems, aarch64, and QTS or
UGOS Pro themselves.

If the installer doesn't fit, each guide below does the same by hand.

## Pick a guide

Answer the questions in order and stop at the first that fits.
Each guide is one page: follow it from top to bottom.

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
