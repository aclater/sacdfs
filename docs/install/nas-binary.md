# Any other NAS or Linux box with SSH: install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that any player can read. This page installs the release file by hand on a NAS that has no guide of its own, or on a Linux box you reach over SSH. If your NAS has a guide ([Synology](synology.md), [QNAP](qnap.md), [ASUSTOR](asustor.md), [TerraMaster](terramaster.md), [UGREEN](ugreen-ugos.md), [ReadyNAS](readynas.md)), use that: it also starts sacdfs at boot. With a container app instead, see [docker-podman.md](docker-podman.md). About 10 minutes.

Checked on 0.6.0 on a Linux system with UGREEN's kernel (in a VM, not on a vendor's hardware): steps 1 to 7 and 9, and the systemd lines of [Start at boot and stop safely](#start-at-boot-and-stop-safely). Step 8 (start at boot) is checked per system in the guides listed above, not here. Steps marked *(untested)* were not run.

From the release after 0.6.0, [the installer](README.md#the-installer-one-line) does these steps for you in one line; this page is the way to do them by hand.

## What you need

- **An SSH login with administrator rights.** SSH is a way to type commands on the NAS from another computer. Turn it on in your NAS's settings, then log in from a terminal (Windows: PowerShell; macOS: Terminal) with `ssh youradmin@your-nas-address`. Your NAS maker's help covers the settings page.
- **Root.** The commands below run as root, the all-powerful account. `id -u` must print `0`. If it prints another number, type `sudo -i`, enter your password, and check again.
- **Your data volume's path.** The examples use `/volume1` (Synology, ASUSTOR, UGREEN). TerraMaster uses `/Volume1`, ReadyNAS `/data`, and QNAP a shared folder such as `/share/CACHEDEV1_DATA/Multimedia`. Replace `/volume1` everywhere with yours.
- **A folder of images.** The examples use `/volume1/music`; sacdfs looks for `.iso` files there and in every folder below it.
- **An Intel or AMD chip:** `uname -m` prints `x86_64`. (`aarch64` archives exist; no ARM NAS was checked.)

## Steps

1. **Check that FUSE is there.** FUSE is the part of the system that lets sacdfs show files.

   ```sh
   ls -l /dev/fuse
   ```

   *You should see* a line starting with `crw`. If you see `No such file or directory`, type `modprobe fuse` and check again.
2. **Make the folders and go into the first.** Keep sacdfs on the data volume, not in `/root` or `/usr/local/bin` (on some NAS systems those are small or read-only). `/volume1/sacd` is where the DSF and WAV files will appear; anything already in it is hidden while sacdfs runs.

   ```sh
   mkdir -p /volume1/sacdfs/cache /volume1/sacd
   cd /volume1/sacdfs
   ```
3. **Download sacdfs and its checksum file.**

   ```sh
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.0/sacdfs-0.6.0-x86_64-linux.tar.gz
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.0/sacdfs-0.6.0-x86_64-linux.tar.gz.sha256
   ```

   *You should see* two progress tables and no line starting `curl: (`.
4. **Check the download.**

   ```sh
   sha256sum -c sacdfs-0.6.0-x86_64-linux.tar.gz.sha256
   ```

   *You should see* `sacdfs-0.6.0-x86_64-linux.tar.gz: OK`.
5. **Unpack it and copy out the program and its start script.** (Use `cp`: on TerraMaster `install` is a different tool.)

   ```sh
   tar -xzf sacdfs-0.6.0-x86_64-linux.tar.gz
   cp sacdfs-0.6.0-x86_64-linux/sacdfs sacdfs-0.6.0-x86_64-linux/sacdfs-ctl /volume1/sacdfs/
   /volume1/sacdfs/sacdfs --version
   ```

   *You should see* `sacdfs 0.6.0`.
6. **Write the settings file.** Change the first line to your images folder.

   ```sh
   cat > /volume1/sacdfs/sacdfs.conf <<'EOF'
   SACDFS_IMAGES=/volume1/music
   SACDFS_MOUNT=/volume1/sacd
   SACDFS_BIN=/volume1/sacdfs/sacdfs
   SACDFS_OPTIONS="--index-cache /volume1/sacdfs/cache"
   EOF
   ```
7. **Start sacdfs.**

   ```sh
   SACDFS_CONF=/volume1/sacdfs/sacdfs.conf /volume1/sacdfs/sacdfs-ctl start
   ls /volume1/sacd
   ```

   *You should see* `sacdfs started`, then one folder per image. Each holds `Stereo` and, on multichannel discs, `Multichannel`, with files like `01 - Title.dsf` and `01 - Title.wav`. sacdfs goes on running after you log out. As root it lets every user on the NAS read the files.
8. **Make it start with the NAS, and stop cleanly.** Until you do this, sacdfs does not come back after a restart. How depends on the system: use the guide of your NAS's make, or [Start at boot and stop safely](#start-at-boot-and-stop-safely) below for any system with systemd. *(Untested on systems other than the six above.)*
9. **To stop sacdfs,** type the line below. Never use `kill -9`: it leaves a dead mount, and everything that touches the folder fails with `Transport endpoint is not connected` until someone runs `umount -l` on it.

   ```sh
   SACDFS_CONF=/volume1/sacdfs/sacdfs.conf /volume1/sacdfs/sacdfs-ctl stop
   ```

   *You should see* it return within a second or two. A file still open makes it wait up to 30 seconds.

## Check it worked

```sh
SACDFS_CONF=/volume1/sacdfs/sacdfs.conf /volume1/sacdfs/sacdfs-ctl status
ls /volume1/sacd
```

*You should see* `sacdfs is running ... mounted on /volume1/sacd` and your images as folders. To play, share `/volume1/sacd` with your NAS's own file sharing (SMB or NFS; [NFS notes](../user-guide.md#sharing-the-mount-over-nfs)) and open it from your player: [the player guides](README.md).

## If something goes wrong

- **You see:** `sacdfs: FUSE is not loaded: /dev/fuse does not exist.` **Do this:** `modprobe fuse`, then step 7 again.
- **You see:** `sha256sum: ... no properly formatted SHA256 checksum lines found`, or `gzip: stdin: not in gzip format`. **Do this:** the download saved an error page (GitHub may have been down). Delete the two downloaded files and run step 3 again later.
- **You see:** `sacdfs: the folder to mount on, /volume1/sacd, does not exist.` **Do this:** `mkdir -p /volume1/sacd` (step 2), then step 7 again.

## Details

### What was checked

*This page, 0.6.0:* steps 1 to 7 and 9 as written (`/volume1`, root over SSH, the release downloaded from GitHub) on a Linux system with UGREEN's kernel in a VM: checksum `OK`, `sacdfs 0.6.0`, `sacdfs-ctl status` printed `running` (exit 0) and, after `stop`, `not running` (exit 3), and all 22 files of three conformance-corpus images were identical to a reference. Then the systemd lines in [Start at boot and stop safely](#start-at-boot-and-stop-safely) as written: `active`, same 22 files. Not run: step 8 as such, and the page on any system but that one.

*Each guide* was followed as written, as root over SSH, from a clean system, with the 0.6.0 release downloaded from GitHub, in a VM, not on the vendor's hardware; storage setup and app stores weren't exercised. The results for each system are in its guide: [Synology DSM 7.4.1](synology.md#what-was-checked), [QNAP QuTScloud 5.2.9](qnap.md#what-was-checked), [ASUSTOR ADM 5.1.4](asustor.md#what-was-checked), [TerraMaster TOS 7.0](terramaster.md#what-was-checked), [UGREEN's kernel (not UGOS itself)](ugreen-ugos.md#what-was-checked), [ReadyNAS OS 6.10.10](readynas.md#what-was-checked).

Earlier, with 0.5.0 on the first five, the checksum was `OK`, `--version` printed `sacdfs 0.5.0`, and all 22 DSF and WAV files of two synthetic test images read back identical to a reference; only the path changed per system, and Synology also needed `modprobe fuse`. 0.5.0 is the first release that runs on QNAP's older system (glibc 2.17 and up; [binary.md](binary.md#binaries)); on Synology, ASUSTOR and TerraMaster 0.4.1 also works.

**Not checked:** the vendors' settings pages (SSH, file sharing), `sudo -i`, a client on another machine, and any ARM NAS.

### Options and reasons

- **`--allow-other`:** it lets users other than root read the files. Without it only root can: an SMB guest login and a non-root user both got "Permission denied" in the 0.5.0 checks, and NFS needs it too. From 0.6.0 it is the default when sacdfs runs as root, so the settings file doesn't name it.
- **`--index-cache`:** a boot script has no `HOME`, so without it sacdfs logs "no seek index cache" and works without one. The cache folder keeps seeks fast after a remount.
- **Settings file next to the program:** `/etc/default` isn't kept across restarts on all NAS systems (QNAP's is a RAM disk), so `SACDFS_CONF` names this file. The systemd unit reads `/etc/default/sacdfs` instead.
- **`SACDFS_OPTIONS`** takes any other [options](../options.md), separated by spaces. Keep the file to plain `NAME=value` lines: systemd reads it too.
- **After a restart,** a mount that sacdfs left behind is cleared by the next start. A client on macOS must mount its share again after any sacdfs restart ([deployment guide](../deployment.md#smb-and-restarts)).
- **Containers instead:** [docker-podman.md](docker-podman.md) and the NAS guides' container sections.

### Start at boot and stop safely

**Never stop sacdfs with `kill -9` or `killall -9`.** That signal gives sacdfs no chance to unmount: the mount stays behind with nobody serving it, and every program that touches the folder (a file manager, a share, a player) fails with `Transport endpoint is not connected` until someone runs `umount -l` on it. A plain `kill` (or `killall sacdfs`) is safe: sacdfs unmounts and exits. If a file on the mount is still open, sacdfs detaches the mount at once and exits when the last file closes, which can take forever (a stuck share, a paused player). So the unit and the script wait up to 30 seconds, then kill sacdfs and clear the mount. (In the version after 0.6.0 sacdfs itself exits `--stop-grace` seconds, 10 by default, after SIGTERM even with a file open; if you raise it, raise `TimeoutStopSec` and `SACDFS_STOP_WAIT` too. That change isn't in 0.6.0.) A new sacdfs also clears a dead mount when it starts.

Both routes read one settings file (`SACDFS_IMAGES`, `SACDFS_MOUNT`, `SACDFS_OPTIONS`; the example is `sacdfs.conf.example` in the archive and in [`deploy`](../../deploy)).

**With systemd** (`systemctl --version` works: TerraMaster, UGREEN, ReadyNAS OS 6, Debian-based systems). The archive holds `sacdfs.service`; it expects the program at `/usr/local/bin/sacdfs` and the settings at `/etc/default/sacdfs`:

```sh
mkdir -p /usr/local/bin
cp sacdfs-0.6.0-x86_64-linux/sacdfs /usr/local/bin/sacdfs
cp sacdfs-0.6.0-x86_64-linux/sacdfs.service /etc/systemd/system/sacdfs.service
cp sacdfs-0.6.0-x86_64-linux/sacdfs.conf.example /etc/default/sacdfs   # then edit the two paths
systemctl daemon-reload
systemctl enable --now sacdfs
systemctl is-active sacdfs
```

Day to day: `systemctl stop sacdfs` (unmounts, waits up to 30 s), `systemctl restart sacdfs`, `journalctl -u sacdfs` (the log). The unit loads FUSE if `/dev/fuse` is missing, makes the folder to mount on, restarts sacdfs if it crashes, and clears any mount left behind after a stop. It uses only settings that systemd 215 (Debian 8) has, and none that hide `/dev` or the file system from the mount.

**Without systemd** (QNAP, ASUSTOR, Synology): `sacdfs-ctl`, a short script for BusyBox `sh`, used in the steps above:

| Command | What it does | Exit code |
|---|---|---|
| `sacdfs-ctl start` | Loads FUSE if needed, makes the folder, starts sacdfs with a pid file (`/var/run/sacdfs.pid`) and a log (`/var/log/sacdfs.log`). Refuses if sacdfs already runs. Clears a leftover mount first. | 0 started, 1 failed |
| `sacdfs-ctl stop` | Sends SIGTERM and waits up to 30 s for the process to exit and the mount to go. Only then detaches the mount and uses SIGKILL. | 0 stopped, 1 failed |
| `sacdfs-ctl status` | Says whether sacdfs runs and the folder is mounted. | 0 running, 3 not running, 1 half-way (for example a dead mount) |
| `sacdfs-ctl restart` | stop, then start. | as for the two |

`SACDFS_PIDFILE` and `SACDFS_LOG` in the settings file move the pid file and log. One `sacdfs-ctl` runs at a time (a lock folder next to the pid file): a second `start` or `stop` waits for the first, which matters on ASUSTOR, whose start-up runs the script twice. The NAS's own start-at-boot tool then runs the script, with `SACDFS_CONF` set:

| System | The tool | Guide |
|---|---|---|
| Synology DSM | Task Scheduler boot-up and shut-down tasks | [synology.md](synology.md) |
| QNAP | `autorun.sh`, once "run user defined processes" is on | [qnap.md](qnap.md) |
| ASUSTOR ADM | `S99sacdfs` and `K01sacdfs` in `/usr/local/etc/init.d` | [asustor.md](asustor.md) |

What was checked for the unit and the script (0.5.0 binary, in VMs): the unit on TerraMaster TOS, UGREEN's kernel and ReadyNAS OS 6 read all 22 files of two test images identically; start under 1 s (ReadyNAS 15.3 s the first time); idle stop under 0.1 s; with a file held open the stop took 30.1 to 30.3 s, then systemd killed sacdfs and no mount was left; after `kill -9` the unit had sacdfs running again 7 s later; a dead mount was cleared at the next start. `sacdfs-ctl` on ADM, QuTScloud, DSM, ReadyNAS and a Debian 8 container: files identical; start and idle stop 1 s; held-open stop 31 to 32 s, then forced, no mount left; start after `kill -9` cleared the leftover mount; exit codes 0, 1 and 3 as in the table. Not checked: the unit on systemd 215 (the oldest checked was 230).

### More problems

The messages are as they appeared in the checks. From 0.6.0 sacdfs checks these things itself before it starts and says what to type; where a message changed, both are given.

| You see | Why | What to do |
|---|---|---|
| `./sacdfs: No such file or directory` or `not found` | `./` means "in this folder", and sacdfs is in another | Use the full path: `/volume1/sacdfs/sacdfs` |
| `sacdfs: a user other than root needs FUSE's mount helper, fusermount3, and it isn't installed ...` | You aren't root, and the NAS has no mount helper for other users | `sudo -i`, then step 7 again |
| `Permission denied` when a share, an app or another user opens the folder | sacdfs 0.5.0 or earlier was started without `--allow-other` (or 0.6.0 with `--no-allow-other`) | `umount /volume1/sacd`, then start it again as in step 7 |
| `curl: (28) Operation timed out` | GitHub didn't answer | Try again later |
| `umount: ... Device or resource busy` | Something is using the folder: a share, an app, or a login whose current folder is inside it | Stop that, or `cd /`, then `umount` again |
| `... a sacdfs mount of this user's is still mounted there ...` | sacdfs already runs on that folder | `sacdfs-ctl stop` or `umount /volume1/sacd` first. Before 0.6.0 the message suggests `fusermount3 -u`, which some NAS systems don't have; as root, `umount` does the same |
| `flag provided but not defined: -m` (TerraMaster) | On TOS, `install` is TerraMaster's own tool | Use `cp`, as in step 5 |
| `` weak version `GLIBC_2.28' not found `` (QNAP) | sacdfs 0.4.1 or earlier | Use 0.5.0 or later |
| `Illegal instruction` | The `x86_64-v3` build on a CPU without AVX2 | Use the `x86_64` build ([binary.md](binary.md#binaries)) |
