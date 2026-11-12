# Install sacdfs

## The installer: one line

Since 0.6.2, an installer finds out what it runs on and sets sacdfs up the way
the guide for that system does. Log in to the NAS or Linux computer over SSH, and type (change
the path to the folder that holds your SACD images, the `.iso` files):

```sh
curl -fsSL https://github.com/aclater/sacdfs/releases/latest/download/install.sh \
  | sudo sh -s -- --iso-dir /volume1/music
```

Leave out `sudo` if you are already root (`id -u` prints `0`; on QNAP the `admin` login is). Add
`--dry-run` first to see every change it would make, and make none.

**Without `curl`,** download it with `wget` first and then run it. Don't pipe `wget` into `sh`:
if the download fails, `sh` gets an empty script and ends without an error, having done nothing.

```sh
wget -O install.sh https://github.com/aclater/sacdfs/releases/latest/download/install.sh && sudo sh install.sh --iso-dir /volume1/music
```

A failed download then stops with `wget`'s error before anything runs. BusyBox's own `wget`, on
systems that have no other, failed against GitHub with a TLS error in our BusyBox 1.37 test
image (and it doesn't check the server's certificate): there this form stops with status 1. Then
download `install.sh`, the release archive for your CPU and its `.sha256` on another computer,
copy all three to the NAS, and run `sh install.sh --iso-dir ... --archive sacdfs-<version>-x86_64-linux.tar.gz`
(checked on 0.6.2-rc.1 in that BusyBox image).
*(Checked on 0.6.2 as a dry run with the NAS's own `wget`, GNU Wget, on QNAP QuTScloud, Synology
DSM, TerraMaster TOS, ReadyNAS OS, Debian 12 with UGREEN's kernel and OpenMediaVault 8, in VMs,
not on the vendor's hardware; and with BusyBox 1.37's `wget` in a test container, where it stops
with status 1 and runs nothing.)*

From the release after 0.6.2, the installer also stops with an error, and changes nothing, when
a download piped into `sh` is cut short. *(Checked with BusyBox 1.37 `sh`, dash and bash, in
the BusyBox and Fedora 44 test containers that CI runs; 0.6.2's installer doesn't have this
check.)*

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
On QNAP, the installer switches on "Run user defined processes" if it is off, so that
`autorun.sh` starts sacdfs at boot. From the release after 0.6.2, `--uninstall` switches it off
again only if the installer switched it on and `autorun.sh` runs nothing else; otherwise it leaves
it on and says why (0.6.2's `--uninstall` always leaves it on). *(Checked on QuTScloud in a VM,
not on the vendor's hardware.)*
Not root, it stops before changing anything and prints the exact line to run with `sudo`.
Afterwards it prints where the files are or the address to give a player. With the binary,
share that folder with the NAS's own file sharing ([player guides](#3-which-player-or-music-server)).

**Where the installer was checked,** on the 0.6.2 release from GitHub, in both forms above (the
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
in BusyBox, Fedora and UBI 7 containers and on its runner with systemd and Docker Compose. On ASUSTOR and
Synology a development version of the script was also followed by a restart: sacdfs was mounted
again by the boot hook it set up, with the same files. Not checked: Unraid, the container route
on a NAS, restarts on the other systems, aarch64 hardware, and QTS or UGOS Pro themselves.

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
