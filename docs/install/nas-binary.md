# sacdfs on a NAS without a container

The release binary is one file. You copy it onto the NAS and start it from an SSH login.
It mounts a folder that shows your SACD images (`.iso`) as DSF and WAV files. Use this route
on a NAS that has no container app, or if you'd rather not use one. With a container app,
[docker-podman.md](docker-podman.md) and your NAS's own guide cover that route instead.

**What was checked.** Every step below was run as written, as root over SSH, from a clean
system, with the 0.5.0 release downloaded in step 3, on: Synology DSM 7.4.1, QNAP QuTScloud
5.2.9, ASUSTOR ADM 5.1.4, TerraMaster TOS 7.0, and Debian 12 on UGREEN's 6.18 kernel (not
UGOS itself). Each ran in a VM, not on the vendor's hardware. Storage setup and app stores
weren't exercised. On each system the checksum was `OK`, `--version` printed `sacdfs 0.5.0`,
and all 22 DSF and WAV files of two synthetic test images read back identical to a reference.
Only the path was changed per system, and Synology also needed `modprobe fuse` (step 1).

Your system's guide lists any extra step and what else was checked there:
[Synology](synology.md), [QNAP](qnap.md), [ASUSTOR](asustor.md),
[TerraMaster](terramaster.md), [UGREEN](ugreen-ugos.md), [ReadyNAS](readynas.md).

## Before you start

- **An SSH login with administrator rights.** Turn SSH on in your NAS's settings, then
  log in from a terminal: on Windows, PowerShell; on macOS, Terminal. Type
  `ssh youradminuser@your-nas-address`. The vendors' settings pages weren't checked here,
  so your NAS maker's help is the guide for this part.
- **Root.** Every command below runs as root. Check with `id -u`, which should print `0`.
  If it prints another number, type `sudo -i`, enter your password, and check again. (The
  checked systems logged in as root, so `sudo -i` itself wasn't checked.)
- **Your data volume's path.** The examples use `/volume1`, as on Synology, ASUSTOR and
  UGREEN. TerraMaster uses `/Volume1`. On QNAP, use a shared folder's path, such as
  `/share/Multimedia`. Replace `/volume1` everywhere below with yours.
- **Where your images are.** The examples use `/volume1/music`. sacdfs looks for `.iso`
  files in that folder and in every folder below it.

## Steps

1. **Check that FUSE is there.** sacdfs is a FUSE filesystem.

   ```sh
   ls -l /dev/fuse
   ```

   *You should see* a line that starts with `crw`. If you see `No such file or directory`,
   run `modprobe fuse` and check again. Synology needed this; see [synology.md](synology.md).
2. **Make a folder for sacdfs on the data volume, and go into it.**

   ```sh
   mkdir -p /volume1/sacdfs
   cd /volume1/sacdfs
   ```

   Keep sacdfs on the data volume, not in `/root` or `/usr/local/bin`. On QNAP the system
   folders are on a small RAM disk. On Synology, `/root` says "FOR SYSTEM USE ONLY".
3. **Download the release and its checksum.** `uname -m` prints `x86_64` on most NAS
   models; use the matching archive. `aarch64` (64-bit ARM) builds exist, but no ARM NAS
   was checked.

   ```sh
   v=0.5.0 a=x86_64
   curl -fLO "https://github.com/aclater/sacdfs/releases/download/v$v/sacdfs-$v-$a-linux.tar.gz"
   curl -fLO "https://github.com/aclater/sacdfs/releases/download/v$v/sacdfs-$v-$a-linux.tar.gz.sha256"
   ```

   *You should see* two progress lines and no `curl: (..)` error. 0.5.0 is the first release
   that runs on QNAP's older system. On Synology, ASUSTOR and TerraMaster, 0.4.1 also works.
4. **Check the download.**

   ```sh
   sha256sum -c "sacdfs-$v-$a-linux.tar.gz.sha256"
   ```

   *You should see* `sacdfs-0.5.0-x86_64-linux.tar.gz: OK`.
5. **Unpack it, and put the program next to it.**

   ```sh
   tar -xzf "sacdfs-$v-$a-linux.tar.gz"
   cp "sacdfs-$v-$a-linux/sacdfs" /volume1/sacdfs/sacdfs
   /volume1/sacdfs/sacdfs --version
   ```

   *You should see* `sacdfs 0.5.0`.

   From 0.6.0 the archive also holds `sacdfs-ctl`, `sacdfs.service` and `sacdfs.conf.example`,
   the files used in "Start at boot and stop safely" below. The commands there download the same
   files from the repository, so they work with older archives too. This is untested on a
   published archive until 0.6.0 is released.
6. **Make an empty folder to mount on.** While sacdfs runs, this folder shows the DSF and
   WAV files, and anything already in it is hidden.

   ```sh
   mkdir -p /volume1/sacd
   ```

7. **Start sacdfs.**

   ```sh
   /volume1/sacdfs/sacdfs --allow-other /volume1/music /volume1/sacd
   ```

   *You should see* nothing: once the folder is mounted, the prompt comes back and sacdfs
   keeps running in the background. It goes on running after you log out of SSH.
   `--allow-other` lets users other than root read the files. Without it, only root can.
   In the checks, an SMB guest login and a non-root user both got "Permission denied" without
   it. NFS needs it too ([user guide](../user-guide.md#sharing-the-mount-over-nfs)). From
   0.6.0 it is the default when sacdfs runs as root, so the option does no harm but is no
   longer needed.
8. **Check it works.**

   ```sh
   ls /volume1/sacd
   ```

   *You should see* one folder per image. Each holds `Stereo` and, on multichannel discs,
   `Multichannel`, with files such as `01 - Title.dsf` and `01 - Title.wav`.
9. **To stop it,** see [Start at boot and stop safely](#start-at-boot-and-stop-safely)
   below. Don't use `kill -9`.

**After the NAS restarts,** sacdfs isn't running unless you set up
[Start at boot and stop safely](#start-at-boot-and-stop-safely). Synology also needs
`modprobe fuse` again; the unit and `sacdfs-ctl` do that.

**Sharing the folder.** Clients reach the files through the NAS's own SMB or NFS sharing
of `/volume1/sacd`, or of a folder above it. The vendors' sharing settings weren't checked.
What was checked on each system (hand-made NFS exports and a test Samba share) is in its
guide.

## Start at boot and stop safely

**Never stop sacdfs with `kill -9` or `killall -9`.** That signal gives sacdfs no chance to
unmount. The mount stays behind with nobody serving it, and every program that touches the
folder (a file manager, a share, a player) fails with `Transport endpoint is not connected`
until someone runs `umount -l` on it. A plain `kill` (or `killall sacdfs`, which sends SIGTERM)
is safe: sacdfs unmounts and exits. If a file on the mount is still open, sacdfs detaches the
mount at once and exits when the last file closes, and that can take forever (a stuck share, a
paused player). So the service and the script below wait up to 30 seconds, and only then kill
sacdfs and clear the mount. A new sacdfs also clears a dead mount when it starts.

Both routes read one settings file, `/etc/default/sacdfs`. The files below are in the
[`deploy`](../../deploy) folder of the repository, from 0.6.0:

```sh
mkdir -p /etc/default
curl -fL -o /etc/default/sacdfs https://raw.githubusercontent.com/aclater/sacdfs/main/deploy/sacdfs.conf.example
vi /etc/default/sacdfs        # set SACDFS_IMAGES and SACDFS_MOUNT to your paths
```

`SACDFS_IMAGES` is the folder with your images and `SACDFS_MOUNT` the folder to mount on (step 6).
`SACDFS_OPTIONS` takes any other [options](../options.md), separated by spaces. Keep the file to
plain `NAME=value` lines: systemd reads it too.

### With systemd

Use this where `systemctl --version` works: TerraMaster, UGREEN, and Debian-based systems such as
ReadyNAS OS 6. The unit expects the program at `/usr/local/bin/sacdfs`:

```sh
cp /volume1/sacdfs/sacdfs /usr/local/bin/sacdfs
curl -fL -o /etc/systemd/system/sacdfs.service https://raw.githubusercontent.com/aclater/sacdfs/main/deploy/systemd/sacdfs.service
systemctl daemon-reload
systemctl enable --now sacdfs
```

*You should see* `systemctl is-active sacdfs` print `active`, and `ls /volume1/sacd` list the
images, also after the NAS restarts. Day to day:

```sh
systemctl stop sacdfs       # unmounts, then exits; waits up to 30 s
systemctl restart sacdfs
journalctl -u sacdfs        # the log
```

The unit loads FUSE if `/dev/fuse` is missing, makes the folder to mount on, restarts sacdfs if it
crashes, and clears any mount left behind after a stop. It uses only settings that systemd 215
(Debian 8) has, and none that hide `/dev` or the file system from the mount.

### Without systemd: sacdfs-ctl

QNAP, ASUSTOR and Synology have no systemd. `sacdfs-ctl` is a short script for BusyBox `sh`:

```sh
curl -fL -o /volume1/sacdfs/sacdfs-ctl https://raw.githubusercontent.com/aclater/sacdfs/main/deploy/sacdfs-ctl
chmod +x /volume1/sacdfs/sacdfs-ctl
echo 'SACDFS_BIN=/volume1/sacdfs/sacdfs' >> /etc/default/sacdfs
/volume1/sacdfs/sacdfs-ctl start
```

| Command | What it does | Exit code |
|---|---|---|
| `sacdfs-ctl start` | Loads FUSE if needed, makes the folder, starts sacdfs with a pid file (`/var/run/sacdfs.pid`) and a log (`/var/log/sacdfs.log`). Refuses if sacdfs already runs. Clears a leftover mount first. | 0 started, 1 failed |
| `sacdfs-ctl stop` | Sends SIGTERM and waits up to 30 s for the process to exit and the mount to go. Only then detaches the mount and uses SIGKILL. | 0 stopped, 1 failed |
| `sacdfs-ctl status` | Says whether sacdfs runs and the folder is mounted. | 0 running, 3 not running, 1 half-way (for example a dead mount) |
| `sacdfs-ctl restart` | stop, then start. | as for the two |

Change where the pid file and log go with `SACDFS_PIDFILE` and `SACDFS_LOG` in the settings file.
On QNAP, `/etc` and `/var` are on a RAM disk that is empty after a restart. Keep the settings
file on the data volume and run the script with `SACDFS_CONF=/volume1/sacdfs/sacdfs.conf` in front.

One `sacdfs-ctl` runs at a time: a second `start` or `stop` waits for the first (a lock folder
next to the pid file). That matters on ASUSTOR, whose start-up ran the script twice; see below.

### Starting it at boot with the NAS's own tools

QNAP, ASUSTOR and Synology run your script at boot and, on two of them, at shutdown. Keep `sacdfs`,
`sacdfs-ctl` and the settings file together on the data volume (here `/volume1/sacdfs`; on QNAP
`/share/CACHEDEV1_DATA/Multimedia/sacdfs`) and give `SACDFS_CONF` the settings file, because
`/etc/default` isn't kept across restarts on all three.

**ASUSTOR ADM.** ADM runs every `S??*` file in `/usr/local/etc/init.d` with `start` at boot, and
every `K??*` file with `stop` at shutdown (it waits up to 60 s). One file serves for both:

```sh
cat > /usr/local/etc/init.d/S99sacdfs <<'EOF'
#!/bin/sh
export SACDFS_CONF=/volume1/sacdfs/sacdfs.conf
case $1 in
    start | stop | restart | status) exec /volume1/sacdfs/sacdfs-ctl "$1" ;;
esac
EOF
chmod +x /usr/local/etc/init.d/S99sacdfs
ln -s S99sacdfs /usr/local/etc/init.d/K01sacdfs
```

*You should see* sacdfs mounted about a minute after the NAS starts, and `signal 15: unmounting`
in the sacdfs log (add `-v` to `SACDFS_OPTIONS`) after a restart from ADM. In the VM, ADM ran
`S99sacdfs start` twice at each boot, a second apart or in the same second. The lock makes the
second one say "already running" and do nothing. Without it, 20 of 25 pairs of starts run at once
left two sacdfs processes, and one kept a mount that `sacdfs-ctl stop` didn't reach. Whether
`/usr/local/etc/init.d` survives an ADM update wasn't checked.

**QNAP (QuTScloud).** QNAP runs `autorun.sh`, kept on a small flash partition, early in the boot,
before the data volume is mounted, so the script waits for the volume in the background. It runs
only if "Run user defined processes during startup" is ticked in the QNAP settings (Control Panel,
System, Hardware, General; that menu wasn't seen). The ticked box is `Misc Autorun = TRUE` in
`/etc/config/uLinux.conf`.

```sh
mount $(/sbin/hal_app --get_boot_pd port_id=0)6 -t ext2 /tmp/config
cat > /tmp/config/autorun.sh <<'EOF'
#!/bin/sh
# The data volume isn't mounted yet: wait for it in the background, then start sacdfs.
(
  D=/share/CACHEDEV1_DATA/Multimedia/sacdfs
  i=0
  while [ ! -x $D/sacdfs-ctl ] && [ $i -lt 120 ]; do sleep 5; i=$((i + 1)); done
  SACDFS_CONF=$D/sacdfs.conf $D/sacdfs-ctl start
) > /tmp/sacdfs-autorun.log 2>&1 &
EOF
chmod +x /tmp/config/autorun.sh
umount /tmp/config
```

QNAP has no hook for shutdown. Its own restart sends SIGTERM to every process; sacdfs unmounted and
exited (`signal 15: unmounting`) and the file system was clean afterwards.

**Synology DSM.** Use a Task Scheduler task that runs on "Boot-up" (Control Panel, Task Scheduler,
Create, Triggered Task, User-defined script; user `root`, event Boot-up, the `start` line below as
the script). The same with the event "Shutdown" makes the stop task. Those menus weren't used; the
command that sits behind them was:

```sh
esynoscheduler --create task_name=sacdfs-start event=bootup enable=true owner='{"0":"root"}' \
  operation_type=script operation='SACDFS_CONF=/volume1/sacdfs/sacdfs.conf /volume1/sacdfs/sacdfs-ctl start'
esynoscheduler --create task_name=sacdfs-stop event=shutdown enable=true owner='{"0":"root"}' \
  operation_type=script operation='SACDFS_CONF=/volume1/sacdfs/sacdfs.conf /volume1/sacdfs/sacdfs-ctl stop'
esynoscheduler --list
```

DSM's boot-up tasks ran about two minutes after power-on. A task has no `HOME`: put
`--index-cache /volume1/sacdfs/cache` in `SACDFS_OPTIONS` (and make the folder), or sacdfs logs
"no seek index cache" and works without it. The shutdown task ran when DSM itself restarted
(`synopoweroff -r`, which the web interface uses); a plain `reboot` from a shell didn't run it, and
sacdfs was killed with no unmount. In the VM the folder `/usr/syno/etc/esynoscheduler` was missing
and had to be made with `mkdir -p` first; a DSM on real hardware is expected to have it.

### What was checked

Checked on each system in a VM, not on the vendor's hardware. Storage setup and app stores weren't exercised. The 0.5.0
release binary with the unit and script from this change.

| Where | What was run | Result |
|---|---|---|
| TerraMaster TOS 7.0, UGREEN's Debian 12 kernel, ReadyNAS OS 6.10.10 (systemd) | The unit: start; read all 22 files of two test images; `systemctl stop` while idle; stop with a file held open; `kill -9`; a dead mount left in place, then start; enable, restart the VM | All files identical. Start under 1 s (ReadyNAS 15.3 s the first time). Idle stop under 0.1 s, exit status 0, no mount left. Held-open stop 30.1 to 30.3 s, then systemd killed sacdfs; no mount left. After `kill -9` the unit had sacdfs running again 7 s later. The dead mount was cleared at the next start. The service was active after the restart |
| ADM 5.1.4, QuTScloud 5.2.9, DSM 7.4.1 (BusyBox or bash `sh`), ReadyNAS OS 6.10.10, Debian 8 container | `sacdfs-ctl`: status, start, a second start, read the files, stop while idle, stop with a file held open, start after `kill -9`, restart, stop twice | Files identical. Start 1 s. Idle stop 1 s. Held-open stop 31 to 32 s, then forced; no mount left. Start after `kill -9` said it was clearing a leftover mount and worked. Exit codes 0, 1 and 3 as in the table. On DSM, which had no `/dev/fuse`, the script loaded FUSE |

| ADM 5.1.4 (`/usr/local/etc/init.d`), the 0.6.0-line binary | Restarts of the VM: sacdfs mounted at boot, 18 files identical; restart from ADM: `signal 15: unmounting` | As described above |
| DSM 7.4.1 (Task Scheduler boot-up and shutdown tasks) | Restart of the VM; `synopoweroff -r`; plain `reboot` | Boot task started sacdfs (files identical); shutdown task stopped it in 1 s; plain `reboot` did not stop it |
| QuTScloud 5.2.9 (`autorun.sh`, installed as written above) | Restart of the VM with sacdfs not running; 16 files of two test images read; power-off of the VM with sacdfs running, then start | `Autorun = TRUE` stayed set across restarts. sacdfs was mounted by itself about 2.5 minutes after the VM started (the script waited for the volume), files identical. The power-off stopped sacdfs cleanly (`signal 15: unmounting`) and the next start found the file system clean. The data volume was an ext4 file system made by hand and mounted by a `crontab` line, because QuTScloud in the VM would not make a storage pool ([qnap.md](qnap.md#starting-at-boot-with-autorunsh)). QuTScloud takes about 22 minutes to finish starting in the VM, and a restart asked for before that waits |

Not checked: `autorun.sh` on a volume made by QNAP's own Storage Manager, the unit
on systemd 215 (the oldest checked was 230), and a client on another machine. A stop with a
file held open takes the full 30 s on every system; a file that is closed in time makes it quick.


## If something goes wrong

The messages are as they appeared in these checks, except for the last two rows: the glibc
message is from an earlier check of 0.4.1 on QNAP, and the last row is from [binary.md](binary.md#binaries).
From 0.6.0, sacdfs checks these things itself before it starts and says what to type, in one
line each; where a message changed, the row gives both.

| You see | Why | What to do |
|---|---|---|
| `./sacdfs: No such file or directory` or `./sacdfs: not found` | `./` means "in this folder", and sacdfs is in another one | Use the full path, as in step 7: `/volume1/sacdfs/sacdfs` |
| `sacdfs: /volume1/sacd: No such file or directory (os error 2)`; from 0.6.0 `sacdfs: the folder to mount on, /volume1/sacd, does not exist. Create it with mkdir -p /volume1/sacd ...` | The folder to mount on doesn't exist | Step 6: `mkdir -p /volume1/sacd` |
| `/dev/fuse not found. Try 'modprobe fuse'`, then `sacdfs: cannot mount on /volume1/sacd: No such file or directory`, twice; from 0.6.0 `sacdfs: FUSE is not loaded: /dev/fuse does not exist. Load it as root with modprobe fuse ...` | FUSE isn't loaded. Before 0.6.0 the message names the mount folder, but the missing thing is `/dev/fuse` | `modprobe fuse`, then step 7 again |
| `sacdfs: a user other than root needs FUSE's mount helper, fusermount3, and it isn't installed ...` (0.6.0; before, `cannot mount on /volume1/sacd: No such file or directory`) | You aren't logged in as root, and the NAS has no mount helper for other users | `sudo -i` (see [Before you start](#before-you-start)), then step 7 again |
| `Permission denied` when a share, an app or another user opens the folder | sacdfs 0.5.0 or earlier was started without `--allow-other` (or 0.6.0 with `--no-allow-other`) | `umount /volume1/sacd`, then step 7 as written |
| `sha256sum: ... no properly formatted SHA256 checksum lines found`, or `gzip: stdin: not in gzip format` | The download saved an error page instead of the file, for example while GitHub was down | Delete both files and run step 3 again later. `curl -f` (step 3) reports the error instead of saving it |
| `curl: (28) Operation timed out` | GitHub didn't answer | Try again later |
| `umount: ... Device or resource busy` | Something is using the folder: a share, an app, or a login whose current folder is inside it | Stop that, or `cd /` in that login, then `umount` again |
| `... a sacdfs mount of this user's is still mounted there ...` | sacdfs is already running on that folder | `umount /volume1/sacd` first. Before 0.6.0 the message suggests `fusermount3 -u`, which some NAS systems don't have; as root, `umount` does the same, and from 0.6.0 the message says `umount` |
| `flag provided but not defined: -m` (TerraMaster) | On TOS, `install` is TerraMaster's own tool, not the usual one | Use `cp`, as in step 5 |
| `` weak version `GLIBC_2.28' not found `` (QNAP) | sacdfs 0.4.1 or earlier, which needs a newer system library | Use 0.5.0 or later |
| `Illegal instruction` | The `x86_64-v3` build on a CPU without AVX2 | Use the `x86_64` build ([binary.md](binary.md#binaries)) |
