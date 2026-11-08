# Install guides

Short guides, one per NAS system or media server: numbered steps to copy, and a check that
it works. Each says what it was validated on. The full reference, with every setting and the
reasons behind it, is the [deployment guide](../deployment.md); what each client was tested
with is in [clients](../clients.md).

These guides were written against **sacdfs 0.4.0**, and each says what it was run on. For
0.5.0, the guides for Synology, QNAP, ASUSTOR, TerraMaster and UGREEN, and the Docker and
Podman guide, were run again, step by step as written, with the 0.5.0
release (the Docker and Podman guide with an image built from `main` with the same code). The NAS
systems ran in VMs, not on the vendors' hardware. Storage setup and app stores weren't
exercised there. The TrueNAS SCALE and OpenMediaVault guides were not run again for 0.5.0.
Image tags still name the release each guide was first checked with. The Compose file for every NAS
container manager, [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml), names 0.5.0; each NAS
guide says where it was run (Docker Compose on ASUSTOR ADM in a VM and on Podman; the other vendors'
container managers are untested). See [docker-podman.md](docker-podman.md#compose). A guide marked
*not validated* has no tested steps yet: nobody has run sacdfs there and reported back.
Reports are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).
Have a NAS? See [beta testing](../beta-testing.md) for how to report.

**New to SSH and Linux?** [The NAS steps](nas-binary.md) go one command at a time, each with
what you should see and what to do when something else appears. They end with
[starting sacdfs at boot and stopping it safely](nas-binary.md#start-at-boot-and-stop-safely)
(a systemd unit, or the `sacdfs-ctl` script where there is no systemd); never stop it with `kill -9`.

## Pick your setup

**1. Where sacdfs runs** (next to the SACD images, normally on the NAS):

| Your system | Guide | Status |
|---|---|---|
| TrueNAS SCALE 24.10 or later | [truenas-scale.md](truenas-scale.md) | Validated (TrueNAS SCALE 25.10, on 0.4.0; not run again for 0.5.0) |
| Any Linux host with Docker or Podman | [docker-podman.md](docker-podman.md) | Validated (Fedora 44 and 45, Podman) |
| OpenMediaVault 8 | [openmediavault.md](openmediavault.md) | Validated (OMV 8.6 on Debian 13, Compose plugin, on 0.4.0; not run again for 0.5.0) |
| Unraid | [unraid.md](unraid.md) | Not validated |
| Synology DSM | [synology.md](synology.md) | Binary checked in a VM (DSM 7.4.1; binary and NFSv3). Container Manager untested |
| QNAP QTS / QuTS hero | [qnap.md](qnap.md) | Binary checked in a VM (QuTScloud 5.2.9), from 0.5.0. Container Station untested |
| UGREEN UGOS Pro | [ugreen-ugos.md](ugreen-ugos.md) | Not validated (a kernel check only) |
| ASUSTOR ADM | [asustor.md](asustor.md) | Checked in a VM (ADM 5.1.4; binary, and Docker Compose) |
| TerraMaster TOS | [terramaster.md](terramaster.md) | Checked in a VM (TOS 7.0; binary, NFS, start at boot). Container untested |
| Netgear ReadyNAS OS 6 | [readynas.md](readynas.md) | Binary and start at boot checked in a VM (OS 6.10.10). Container untested |

Without a container, on any Linux host (x86_64, or aarch64 from the first release after 0.4.1, glibc 2.17 or later; as a user other than root, FUSE 3's `fusermount3`,
package `fuse3`, or from 0.5.0 FUSE 2's `fusermount`; root needs neither):
a [release binary](binary.md#binaries) ([step by step on a NAS](nas-binary.md)), or, from 0.4.0, `cargo install --locked sacdfs`
from crates.io (Rust 1.85 or later; tested on 0.4.0 in a clean Rust container). The [user guide](../user-guide.md) covers running it.

**2. How other computers reach it:**

| Client | Guide |
|---|---|
| Linux: NFS or SMB, by hand, in `/etc/fstab`, mounted by systemd | [linux-client.md](linux-client.md) |
| macOS, Windows | [deployment guide](../deployment.md#mounting-the-shares-on-clients) |

**3. Your music server:**

| Server | Guide | Tested with |
|---|---|---|
| Roon | [roon.md](roon.md) | Roon 2.x, sacdfs 0.2.0 |
| Plex | [plex.md](plex.md) | Plex Media Server 1.43.5, sacdfs 0.2.0-rc.4 |
| Jellyfin | [jellyfin.md](jellyfin.md) | Jellyfin 12.4.0, sacdfs 0.4.0-rc.2 (and 0.3.0-rc.1) |
| Navidrome | [navidrome.md](navidrome.md) | Navidrome 0.64.2, sacdfs 0.4.0-rc.2 (and 0.3.0-rc.1) |
| Lyrion (Logitech Media Server) | [lms.md](lms.md) | Lyrion 9.1.1, sacdfs 0.4.0-rc.2 (and 0.3.0-rc.1) |
| MPD (and moOde, Volumio) | [mpd.md](mpd.md) | MPD 0.24.15, sacdfs 0.4.0-rc.2 (and 0.3.0-rc.1) |

The music-server tests ran on the sacdfs versions named. The Jellyfin, Navidrome, Lyrion and
MPD tests ran again on 0.4.0-rc.2 at the default cache, 32 MiB since 0.4.0
([#150](https://github.com/aclater/sacdfs/issues/150)). The Roon and Plex tests are older than
that default; it was re-checked on 0.4.0: a scan that reads the start and end of every file
peaked at 48 MiB of memory.
