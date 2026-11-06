# Install guides

Short guides, one per NAS system or media server: numbered steps to copy, and a check that
it works. Each says what it was validated on. The full reference, with every setting and the
reasons behind it, is the [deployment guide](../deployment.md); what each client was tested
with is in [clients](../clients.md).

These guides were written against **sacdfs 0.4.0**. Each says what it was run on, and that version
is kept as is: the guides were not re-run on 0.4.1, except for the checks named in them (the rootless
Podman health check in [docker-podman.md](docker-podman.md), run on a 0.4.1 candidate). Their image tags
still name 0.4.0, which stays published. A guide marked *not validated* has no steps yet:
nobody has run sacdfs there and reported back. Reports are welcome on
[#82](https://github.com/aclater/sacdfs/issues/82).

## Pick your setup

**1. Where sacdfs runs** (next to the SACD images, normally on the NAS):

| Your system | Guide | Status |
|---|---|---|
| TrueNAS SCALE 24.10 or later | [truenas-scale.md](truenas-scale.md) | Validated (TrueNAS SCALE 25.10) |
| Any Linux host with Docker or Podman | [docker-podman.md](docker-podman.md) | Validated (Fedora 44, Podman) |
| OpenMediaVault 8 | [openmediavault.md](openmediavault.md) | Validated (OMV 8.6 on Debian 13, Compose plugin) |
| Unraid | [unraid.md](unraid.md) | Not validated |
| Synology DSM | [synology.md](synology.md) | Not validated |
| QNAP QTS / QuTS hero | [qnap.md](qnap.md) | Not validated |
| UGREEN UGOS Pro | [ugreen-ugos.md](ugreen-ugos.md) | Not validated |

Without a container, on any Linux host (x86_64; FUSE 3's `fusermount3`, package `fuse3`):
a [release binary](binary.md#binaries), or, from 0.4.0, `cargo install --locked sacdfs`
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
