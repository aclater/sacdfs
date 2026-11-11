# Synology DSM: install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that any player can read. This page installs it from the release file and starts it by itself whenever the NAS starts. About 15 minutes.

Checked on 0.6.0 on DSM 7.4.1 (in a VM, not on Synology hardware). Steps marked *(untested)* were not run.

From the release after 0.6.1, [the installer](README.md#the-installer-one-line) does these steps for you in one line; this page is the way to do them by hand.

## What you need

- A Synology NAS with an Intel or AMD chip (`uname -m` in step 1 prints `x86_64`).
- An administrator login over SSH. SSH is a way to type commands on the NAS from another computer.
- A folder of SACD images on the NAS. This page uses `/volume1/music`; change it to yours.
- About 100 MB free on `/volume1`.

## Steps

1. **Log in over SSH as root.** In DSM, Control Panel, Terminal & SNMP, tick "Enable SSH service" *(untested)*. On your computer (Windows: PowerShell; macOS: Terminal) type `ssh youradmin@your-nas-address`, then `sudo -i` and your password *(untested: the VM login was already root)*. Check:

   ```sh
   id -u && uname -m
   ```

   *You should see* `0` and `x86_64`.
2. **Load FUSE.** FUSE is the part of the system that lets sacdfs show files. DSM doesn't load it.

   ```sh
   modprobe fuse
   ls -l /dev/fuse
   ```

   *You should see* a line starting with `crw`.
3. **Make the folders and go into the first.** `/volume1/sacd` is where the DSF and WAV files will appear; anything already in it is hidden while sacdfs runs.

   ```sh
   mkdir -p /volume1/sacdfs/cache /volume1/sacd
   cd /volume1/sacdfs
   ```
4. **Download sacdfs and its checksum file.**

   ```sh
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.1/sacdfs-0.6.1-x86_64-linux.tar.gz
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.1/sacdfs-0.6.1-x86_64-linux.tar.gz.sha256
   ```

   *You should see* two progress tables and no line starting `curl: (`.
5. **Check the download.**

   ```sh
   sha256sum -c sacdfs-0.6.1-x86_64-linux.tar.gz.sha256
   ```

   *You should see* `sacdfs-0.6.1-x86_64-linux.tar.gz: OK`.
6. **Unpack it and copy out the program and its start script.**

   ```sh
   tar -xzf sacdfs-0.6.1-x86_64-linux.tar.gz
   cp sacdfs-0.6.1-x86_64-linux/sacdfs sacdfs-0.6.1-x86_64-linux/sacdfs-ctl /volume1/sacdfs/
   /volume1/sacdfs/sacdfs --version
   ```

   *You should see* `sacdfs 0.6.1`.
7. **Write the settings file.** Change the first line to your images folder.

   ```sh
   cat > /volume1/sacdfs/sacdfs.conf <<'EOF'
   SACDFS_IMAGES=/volume1/music
   SACDFS_MOUNT=/volume1/sacd
   SACDFS_BIN=/volume1/sacdfs/sacdfs
   SACDFS_OPTIONS="--index-cache /volume1/sacdfs/cache"
   EOF
   ```
8. **Start sacdfs.**

   ```sh
   SACDFS_CONF=/volume1/sacdfs/sacdfs.conf /volume1/sacdfs/sacdfs-ctl start
   ls /volume1/sacd
   ```

   *You should see* `sacdfs started`, then one folder per image. Each holds `Stereo` and, on multichannel discs, `Multichannel`, with files like `01 - Title.dsf` and `01 - Title.wav`. sacdfs goes on running after you log out.
9. **Make it start and stop with the NAS.** These two commands make DSM's Task Scheduler run the script at start-up and at shut-down. (The shut-down task matters: never stop sacdfs with `kill -9`, it leaves a dead mount.)

   ```sh
   mkdir -p /usr/syno/etc/esynoscheduler
   esynoscheduler --create task_name=sacdfs-start event=bootup enable=true owner='{"0":"root"}' \
     operation_type=script operation='SACDFS_CONF=/volume1/sacdfs/sacdfs.conf /volume1/sacdfs/sacdfs-ctl start'
   esynoscheduler --create task_name=sacdfs-stop event=shutdown enable=true owner='{"0":"root"}' \
     operation_type=script operation='SACDFS_CONF=/volume1/sacdfs/sacdfs.conf /volume1/sacdfs/sacdfs-ctl stop'
   esynoscheduler --list
   ```

   *You should see* both tasks listed.
10. **Restart the NAS the way DSM does, and look again.** Use this command, not `reboot`: a plain `reboot` skips the shut-down task and kills sacdfs without unmounting.

    ```sh
    synopoweroff -r
    ```

    Wait about 3 minutes, log in again, and run `ls /volume1/sacd`. *You should see* the same folders.

To stop sacdfs by hand: `SACDFS_CONF=/volume1/sacdfs/sacdfs.conf /volume1/sacdfs/sacdfs-ctl stop`. Never use `kill -9`.

## Check it worked

```sh
ls /volume1/sacd
```

*You should see* your images as folders. To play, share `/volume1/sacd` with DSM's own file sharing (SMB or NFS) and open it from your player; see [the player guides](README.md).

## If something goes wrong

- **You see:** `sacdfs: FUSE is not loaded: /dev/fuse does not exist.` **Do this:** `modprobe fuse` (step 2), then step 8 again. The start script does this by itself at start-up.
- **You see:** `sha256sum: ... no properly formatted SHA256 checksum lines found`, or `gzip: stdin: not in gzip format`. **Do this:** the download saved an error page. Delete the two downloaded files and run step 4 again later.
- **You see:** `Permission denied` when a share or an app opens the folder. **Do this:** you started sacdfs without being root. Run `id -u` (it must print `0`), stop sacdfs (see above) and start it again.

## Details

### What was checked

Checked on DSM 7.4.1 (kernel 4.4) in a VM, not on Synology hardware. Storage setup and app stores weren't exercised.

*0.6.0 run, from a clean system:* steps 1 (the `id -u` check only) to 10 as written, with the published 0.6.0 archive, using three images of the conformance corpus (22 DSF and WAV files). The checksum was `OK`, `--version` printed `sacdfs 0.6.0`, and all 22 files hashed identically to a reference read by a Linux host with the same release. After `synopoweroff -r`, the boot task had sacdfs mounted again when the VM was 2 minutes up, with the same 22 files identical; `sacdfs-ctl stop` while idle took 1.0 s and left no mount. The shut-down task's effect wasn't looked at separately on 0.6.0 (see below).

*Earlier versions:* with 0.5.0 the same steps (by hand, `/volume1`) gave all 22 files of two test images identical to the reference; a non-root user got "Permission denied" without `--allow-other` and could read with it; NFSv3 with `/volume1/sacd 127.0.0.1(ro,fsid=4242,no_subtree_check)` in `/etc/exports` and `mount -t nfs -o vers=3,nolock,ro` read all files, NFSv4 gave `mount.nfs: Protocol not supported`. The Task Scheduler tasks were checked on a 0.6.0 development build.

**Not checked:** DSM's web screens (Control Panel menus, Task Scheduler, Shared Folder and file-service settings), whether DSM keeps or rewrites a hand-made `/etc/exports`, SMB (the VM had no `smbd`), a client on another machine, and `sudo -i`.

### Options and reasons

- **Why `/volume1/sacdfs` and not `/root`:** `/root` says "FOR SYSTEM USE ONLY" on DSM and the system folders are small. Keep sacdfs on the data volume.
- **Why `modprobe fuse`:** DSM has FUSE 2's `fusermount` but not FUSE 3's `fusermount3`, and doesn't load FUSE. As root sacdfs needs neither tool. `sacdfs-ctl` loads FUSE at every start.
- **Why `--index-cache`:** a Task Scheduler task has no `HOME`, so without it sacdfs logs "no seek index cache" and works without one.
- **Task Scheduler instead of the commands:** Control Panel, Task Scheduler, Create, Triggered Task, User-defined script, user `root`, event Boot-up (and the same with Shut-down for the stop task) gives the same tasks *(the menus weren't seen)*. Boot-up tasks ran about two minutes after power-on. In the VM `/usr/syno/etc/esynoscheduler` was missing and `mkdir -p` made it; real DSM is expected to have it.
- **`sacdfs-ctl` and the settings file:** every command and setting is in [nas-binary.md](nas-binary.md#start-at-boot-and-stop-safely).
- **Container Manager** *(untested)*: it needs a DSM storage volume, which the VM couldn't make. The Compose file for every NAS container manager is [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml); it was run on ASUSTOR ADM ([asustor.md](asustor.md#container-route)) and on Podman, not on DSM. DSM's own file services use ports 445 and 2049, which is why the file publishes 10445 and 12049. Whether Container Manager allows `/dev/fuse` and `SYS_ADMIN` is unknown. See [docker-podman.md](docker-podman.md).

### More problems

- **You see:** `./sacdfs: not found`. **Do this:** use the full path, `/volume1/sacdfs/sacdfs`.
- **You see:** `sacdfs: the folder to mount on, /volume1/sacd, does not exist.` **Do this:** `mkdir -p /volume1/sacd` (step 3).
- **You see:** `umount: ... Device or resource busy`. **Do this:** something uses the folder (a share, an app, or a login whose current folder is inside it). Stop it or `cd /`, then try again.
- **You see:** `... a sacdfs mount of this user's is still mounted there ...`. **Do this:** sacdfs already runs on that folder; `sacdfs-ctl stop` first.
- **You see:** `Illegal instruction`. **Do this:** you used the `x86_64-v3` build on a CPU without AVX2; use the `x86_64` archive ([binary.md](binary.md#binaries)).

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).
