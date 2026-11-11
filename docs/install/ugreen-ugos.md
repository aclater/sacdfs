# UGREEN UGOS Pro: install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that any player can read. This page installs it from the release file and starts it by itself whenever the NAS starts. About 15 minutes.

**Not tested on UGOS Pro.** Checked on 0.6.0 on a Linux system with UGREEN's kernel (in a VM, not UGOS Pro and not UGREEN hardware). Steps marked *(untested)* were not run.

From the release after 0.6.1, [the installer](README.md#the-installer-one-line) does these steps for you in one line; this page is the way to do them by hand.

## What you need

- A UGREEN NAS running UGOS Pro with an Intel or AMD chip (`uname -m` in step 1 prints `x86_64`).
- An administrator login over SSH. SSH is a way to type commands on the NAS from another computer.
- A folder of SACD images on the NAS. This page uses `/volume1/music`; change it to yours.
- About 100 MB free on `/volume1`.

## Steps

1. **Log in over SSH as root.** In UGOS Pro's settings, switch on SSH (Control Panel, Terminal) *(untested)*. On your computer (Windows: PowerShell; macOS: Terminal) type `ssh youradmin@your-nas-address`, then `sudo -i` and your password *(untested: the VM login was already root)*. Check:

   ```sh
   id -u && uname -m
   ```

   *You should see* `0` and `x86_64`.
2. **Check that FUSE is there.** FUSE is the part of the system that lets sacdfs show files. UGREEN's kernel has it.

   ```sh
   ls -l /dev/fuse
   ```

   *You should see* a line starting with `crw`.
3. **Make the folders and go into the first.** `/volume1/sacd` is where the DSF and WAV files will appear; anything already in it is hidden while sacdfs runs.

   ```sh
   mkdir -p /volume1/sacdfs /volume1/sacd
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
6. **Unpack it and put the program and the start-up unit in place.** The unit is the file that tells systemd (the part of the system that starts services) how to run sacdfs.

   ```sh
   tar -xzf sacdfs-0.6.1-x86_64-linux.tar.gz
   mkdir -p /usr/local/bin
   cp sacdfs-0.6.1-x86_64-linux/sacdfs /usr/local/bin/sacdfs
   cp sacdfs-0.6.1-x86_64-linux/sacdfs.service /etc/systemd/system/sacdfs.service
   /usr/local/bin/sacdfs --version
   ```

   *You should see* `sacdfs 0.6.1`.
7. **Write the settings file.** Change the first line to your images folder.

   ```sh
   cat > /etc/default/sacdfs <<'EOF'
   SACDFS_IMAGES=/volume1/music
   SACDFS_MOUNT=/volume1/sacd
   EOF
   ```
8. **Start sacdfs, and make it start with the NAS.**

   ```sh
   systemctl daemon-reload
   systemctl enable --now sacdfs
   systemctl is-active sacdfs
   ls /volume1/sacd
   ```

   *You should see* `active`, then one folder per image. Each holds `Stereo` and, on multichannel discs, `Multichannel`, with files like `01 - Title.dsf` and `01 - Title.wav`. sacdfs goes on running after you log out.
9. **Restart the NAS, and look again.** systemd stops sacdfs cleanly on a restart. (Never stop sacdfs with `kill -9`: it leaves a dead mount that fails with `Transport endpoint is not connected` until someone runs `umount -l` on it.)

   ```sh
   reboot
   ```

   Wait about 3 minutes, log in again, and run `systemctl is-active sacdfs` and `ls /volume1/sacd`. *You should see* `active` and the same folders.

To stop sacdfs by hand: `systemctl stop sacdfs` (it waits up to 30 seconds for open files, then clears the mount). To start it again: `systemctl start sacdfs`. Never use `kill -9`.

## Check it worked

```sh
systemctl is-active sacdfs
ls /volume1/sacd
```

*You should see* `active` and your images as folders. To play, share `/volume1/sacd` with UGREEN's own file sharing (SMB or NFS) and open it from your player; see [the player guides](README.md).

## If something goes wrong

- **You see:** `sacdfs: FUSE is not loaded: /dev/fuse does not exist.` **Do this:** `modprobe fuse`, then `systemctl restart sacdfs`. The unit also does this by itself at start-up.
- **You see:** `sha256sum: ... no properly formatted SHA256 checksum lines found`, or `gzip: stdin: not in gzip format`. **Do this:** the download saved an error page. Delete the two downloaded files and run step 4 again later.
- **You see:** `systemctl is-active sacdfs` prints `failed` or `activating`. **Do this:** type `journalctl -u sacdfs -n 20` and read the last lines. The usual cause is a wrong path in `/etc/default/sacdfs` (step 7); fix it and type `systemctl restart sacdfs`.

## Details

### What was checked

**Not checked on UGOS Pro itself.** The check ran on a Linux system with UGREEN's kernel and packages in a VM, without the UGOS apps, web interface or Docker. It says that UGREEN's kernel runs sacdfs; it says nothing about UGOS Pro's own tools. Nobody has run sacdfs on UGOS Pro and reported back.

*0.6.0 run:* steps 1 (the `id -u` check only) to 8 as written on that Debian system, then step 9 by shutting the VM down and starting it again, using three images of the conformance corpus (22 DSF and WAV files). The checksum was `OK`, `--version` printed `sacdfs 0.6.0`, `systemctl is-active` printed `active`, and all 22 files hashed identically to a reference read by a Linux host with the same release. After the restart the service was `active` with the same 22 files identical. (In the VM a plain `reboot` didn't mount the data disk again, so the unit started with no images folder and stopped; a real NAS mounts its volume at start-up. Whether UGOS Pro's volume is ready before the unit starts wasn't checked.)

*Earlier versions:* with 0.5.0 the old steps read all 22 files of two test images identical to the reference; 0.4.1-rc.3 served NFSv3, NFSv4 and SMB loopback mounts correctly there.

**Not checked:** UGOS Pro's SSH switch and settings screens, `sudo -i` on UGOS, where UGOS keeps its volume (UGOS Pro normally uses `/volume1`; check yours with `ls /`), whether UGOS keeps `/etc/systemd/system` and `/usr/local/bin` across updates, and containers.

### Options and reasons

- **Why systemd:** UGOS Pro is Debian-based and uses systemd, like the checked system.
- **Container route (UGOS Pro's Docker app), untested:** sacdfs's container needs `/dev/fuse` and the capabilities `SYS_ADMIN` (and `DAC_READ_SEARCH` for NFS); whether UGOS Pro's Docker app allows that isn't known, and the VM has no Docker. The Compose file for every NAS container manager is [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml); it was run on ASUSTOR ADM ([asustor.md](asustor.md#container-route)) and on Podman. In the Docker app, create a project from the file (menu names not seen) and change the one marked line to your images folder. It publishes ports 10445 and 12049 because UGOS's own file services use 445 and 2049. See [docker-podman.md](docker-podman.md) and the [deployment guide](../deployment.md).

### More problems

- **You see:** `Permission denied` when a share or an app opens the folder. **Do this:** sacdfs must run as root, as the unit does. A sacdfs you started by hand as another user, without `--allow-other`, is the usual cause: `systemctl stop sacdfs`, `umount /volume1/sacd` if it is still mounted, then `systemctl start sacdfs`.
- **You see:** `umount: ... Device or resource busy`. **Do this:** something uses the folder (a share, an app, or a login whose current folder is inside it). Stop it or `cd /`, then try again.
- **You see:** `Illegal instruction`. **Do this:** you used the `x86_64-v3` build on a CPU without AVX2; use the `x86_64` archive ([binary.md](binary.md#binaries)).
- **The unit, step by step, and what it does when sacdfs crashes:** [nas-binary.md](nas-binary.md#start-at-boot-and-stop-safely).

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).

