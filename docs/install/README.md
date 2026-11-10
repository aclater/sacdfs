# Install sacdfs

Pick one guide. Answer the questions in order and stop at the first that fits.
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
