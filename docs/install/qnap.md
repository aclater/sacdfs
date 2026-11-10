# QNAP: install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that any player can read. This page installs it from the release file and starts it by itself whenever the NAS starts. About 15 minutes.

Checked on 0.6.0 on QuTScloud 5.2.9 (in a VM, not on QNAP hardware; QTS and QuTS hero may differ). Steps marked *(untested)* were not run.

## What you need

- A QNAP NAS with an Intel or AMD chip (`uname -m` in step 1 prints `x86_64`).
- An administrator login over SSH. SSH is a way to type commands on the NAS from another computer.
- A folder of SACD images on the NAS, in a QNAP shared folder. This page uses a folder `music` in the shared folder `Multimedia`; change the paths to yours.
- About 100 MB free in the shared folder.

## Steps

1. **Log in over SSH as `admin`.** In QNAP's Control Panel, Network & File Services, Telnet / SSH, tick "Allow SSH connection" *(untested)*. On your computer (Windows: PowerShell; macOS: Terminal) type `ssh admin@your-nas-address`. On QNAP `admin` is root. Check:

   ```sh
   id -u && uname -m
   ```

   *You should see* `0` and `x86_64`.
2. **Check that FUSE is there.** FUSE is the part of the system that lets sacdfs show files. QNAP has it.

   ```sh
   ls -l /dev/fuse
   ```

   *You should see* a line starting with `crw`.
3. **Make the folders and go into the first.** Keep sacdfs in a shared folder: QNAP's system folders, `/root` included, are on a small RAM disk. `/share/CACHEDEV1_DATA/Multimedia/sacd` is where the DSF and WAV files will appear; anything already in it is hidden while sacdfs runs.

   ```sh
   mkdir -p /share/CACHEDEV1_DATA/Multimedia/sacdfs/cache /share/CACHEDEV1_DATA/Multimedia/sacd
   cd /share/CACHEDEV1_DATA/Multimedia/sacdfs
   ```
4. **Download sacdfs and its checksum file.**

   ```sh
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.0/sacdfs-0.6.0-x86_64-linux.tar.gz
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.0/sacdfs-0.6.0-x86_64-linux.tar.gz.sha256
   ```

   *You should see* two progress tables and no line starting `curl: (`.
5. **Check the download.**

   ```sh
   sha256sum -c sacdfs-0.6.0-x86_64-linux.tar.gz.sha256
   ```

   *You should see* `sacdfs-0.6.0-x86_64-linux.tar.gz: OK`.
6. **Unpack it and copy out the program and its start script.**

   ```sh
   tar -xzf sacdfs-0.6.0-x86_64-linux.tar.gz
   cp sacdfs-0.6.0-x86_64-linux/sacdfs sacdfs-0.6.0-x86_64-linux/sacdfs-ctl /share/CACHEDEV1_DATA/Multimedia/sacdfs/
   /share/CACHEDEV1_DATA/Multimedia/sacdfs/sacdfs --version
   ```

   *You should see* `sacdfs 0.6.0`.
7. **Write the settings file.** Change the first line to your images folder.

   ```sh
   cat > /share/CACHEDEV1_DATA/Multimedia/sacdfs/sacdfs.conf <<'EOF'
   SACDFS_IMAGES=/share/CACHEDEV1_DATA/Multimedia/music
   SACDFS_MOUNT=/share/CACHEDEV1_DATA/Multimedia/sacd
   SACDFS_BIN=/share/CACHEDEV1_DATA/Multimedia/sacdfs/sacdfs
   SACDFS_OPTIONS="--index-cache /share/CACHEDEV1_DATA/Multimedia/sacdfs/cache"
   EOF
   ```
8. **Start sacdfs.**

   ```sh
   SACDFS_CONF=/share/CACHEDEV1_DATA/Multimedia/sacdfs/sacdfs.conf /share/CACHEDEV1_DATA/Multimedia/sacdfs/sacdfs-ctl start
   ls /share/CACHEDEV1_DATA/Multimedia/sacd
   ```

   *You should see* `sacdfs started`, then one folder per image. Each holds `Stereo` and, on multichannel discs, `Multichannel`, with files like `01 - Title.dsf` and `01 - Title.wav`. sacdfs goes on running after you log out.
9. **Switch on "run user defined processes".** QNAP only runs start-up scripts when this is on. Type:

   ```sh
   setcfg Misc Autorun TRUE -f /etc/config/uLinux.conf
   ```

   (The same switch is in the QNAP settings, Control Panel, System, Hardware, General, "Run user defined processes during startup" *(untested)*.)
10. **Make it start with the NAS.** QNAP runs `autorun.sh` early at start-up, before the data volume is ready, so this script waits for the volume in the background and then starts sacdfs. The file lives on a small flash partition, which these commands open and close.

    ```sh
    mount $(/sbin/hal_app --get_boot_pd port_id=0)6 -t ext2 /tmp/config
    cat > /tmp/config/autorun.sh <<'EOF'
    #!/bin/sh
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

    QNAP has no shut-down hook; its own restart sends every program a polite stop signal, and sacdfs unmounts cleanly on it. (Never stop sacdfs with `kill -9`: it leaves a dead mount.)
11. **Restart the NAS, and look again.**

    ```sh
    reboot
    ```

    Wait 5 minutes, log in again, and run `ls /share/CACHEDEV1_DATA/Multimedia/sacd`. *You should see* the same folders.

To stop sacdfs by hand: `SACDFS_CONF=/share/CACHEDEV1_DATA/Multimedia/sacdfs/sacdfs.conf /share/CACHEDEV1_DATA/Multimedia/sacdfs/sacdfs-ctl stop`. Never use `kill -9`.

## Check it worked

```sh
ls /share/CACHEDEV1_DATA/Multimedia/sacd
```

*You should see* your images as folders. To play, share the `sacd` folder with QNAP's own file sharing (SMB or NFS) and open it from your player; see [the player guides](README.md).

## If something goes wrong

- **You see:** `` weak version `GLIBC_2.28' not found ``. **Do this:** that is sacdfs 0.4.1 or older, which needs a newer system library than QNAP has. Use 0.5.0 or newer (this page uses 0.6.0).
- **You see:** `sha256sum: ... no properly formatted SHA256 checksum lines found`, or `gzip: stdin: not in gzip format`. **Do this:** the download saved an error page. Delete the two downloaded files and run step 4 again later.
- **You see:** `Permission denied` when a share or an app opens the folder. **Do this:** you started sacdfs without being root. Run `id -u` (it must print `0`), stop sacdfs (see above) and start it again.

## Details

### What was checked

Checked on QuTScloud 5.2.9 (kernel 5.10) in a VM, not on QNAP hardware. QuTScloud wouldn't make a storage pool in the VM, so the data volume was an ext4 file system made by hand, mounted at `/share/CACHEDEV1_DATA` early in each start by a `crontab` line (the stand-in for a volume QNAP mounts itself), and the shared folder `Multimedia` was a folder on it.

*0.6.0 run, from a clean system:* steps 1 (the `id -u` check only) to 11 as written, with the published 0.6.0 archive, using three images of the conformance corpus (22 DSF and WAV files). *(restart result below)*

*Earlier versions:* with 0.5.0, steps 1 to 9 of the old guide (by hand) read all 22 files of two test images identical to the reference and survived logging out and in. With the pull request's build before that, QuTScloud's own NFS server (NFSv3, explicit `fsid=`), mounted on the same system, gave the same bytes as a current Linux system for two test images and a real DST disc. `autorun.sh` was checked on a 0.6.0 development build: `Autorun = TRUE` stayed set across restarts, sacdfs mounted by itself about 2.5 minutes after the VM started (16 files of two test images identical), and QNAP's power-off of the VM stopped it cleanly (`signal 15: unmounting`, file system clean at the next start).

**Not checked:** the QNAP web screens (including the "run user defined processes" menu), a non-root user (QuTScloud has no `su`), SMB (no Samba server ran), QNAP's own shared-folder and NFS settings, a client on another machine, and a volume made by QNAP's Storage Manager.

### Options and reasons

- **`/share/CACHEDEV1_DATA/Multimedia` and not `/share/Multimedia`:** `/share/Multimedia` is a link to the volume's real path, so `/proc/mounts` lists the mount under `/share/CACHEDEV1_DATA/...`. The real path is also what exists first at start-up.
- **Why the script waits:** `autorun.sh` runs before the data volume is mounted. It waits up to 10 minutes for `sacdfs-ctl` to appear.
- **QuTScloud is slow in the VM:** about 22 minutes to finish starting, because it waits for a licence check that has no answer. `autorun.sh` ran after about 2.5 minutes, but a restart or power-off asked for sooner waits until the 22 minutes are over (`reboot` and `qreboot` did nothing visible meanwhile). A real QNAP isn't expected to do this. The volume was also not QNAP's: the first-run setup partitions the disks but makes no volume, and `qcli_pool` stops with "read limitation error!" (no licence installed; activating one needs a QNAP account).
- **Every `sacdfs-ctl` command and setting:** [nas-binary.md](nas-binary.md#start-at-boot-and-stop-safely).

### Container route

Container Station 3.1.4 came from QNAP's App Center (`qpkg_cli -g container-station` downloads the package; `qpkg_cli -m <the downloaded file>` installed it, because `qpkg_cli -a` stayed "waiting for install") on the hand-made volume. It gave Docker 27.1.2 and Compose v5.1.1; its Docker engine wasn't running after the install, and `supervisorctl start docker` started it (the web interface's first-run step presumably does that; it wasn't used). [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml) ran as published, with only the images path changed, with `docker compose up -d` over SSH (QNAP's own file services use 445 and 2049, so the file publishes 10445 and 12049). With the published 0.5.0 image: healthy 18 s after `up` (pull included); SMB on 10445 (`smbclient` in a throwaway Alpine container on the host network) read all 16 files of two test images identical; `docker compose restart` healthy after 17 s; `docker compose stop` 0.3 s, exit status 0. Not checked: the Container Station screens, NFS, a client on another machine, and anything on hardware. See [docker-podman.md](docker-podman.md).

### More problems

- **You see:** `./sacdfs: not found`. **Do this:** use the full path, `/share/CACHEDEV1_DATA/Multimedia/sacdfs/sacdfs`.
- **You see:** `sacdfs: the folder to mount on, ..., does not exist.` **Do this:** `mkdir -p /share/CACHEDEV1_DATA/Multimedia/sacd` (step 3).
- **You see:** `umount: ... Device or resource busy`. **Do this:** something uses the folder (a share, an app, or a login whose current folder is inside it). Stop it or `cd /`, then try again.
- **You see:** `... a sacdfs mount of this user's is still mounted there ...`. **Do this:** sacdfs already runs on that folder; `sacdfs-ctl stop` first.
- **You see:** `Illegal instruction`. **Do this:** you used the `x86_64-v3` build on a CPU without AVX2; use the `x86_64` archive ([binary.md](binary.md#binaries)).

Reports from real QNAP hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how sacdfs was started, and what worked.
