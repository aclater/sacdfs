# TerraMaster TOS: install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that any player can read. This page installs it from the release file and starts it by itself whenever the NAS starts. About 15 minutes.

Checked on 0.6.0 on TOS 7.0 (in a VM, not on TerraMaster hardware). Steps marked *(untested)* were not run.

From the release after 0.6.0, [the installer](README.md#the-installer-one-line) does these steps for you in one line; this page is the way to do them by hand.

## What you need

- A TerraMaster NAS with an Intel or AMD chip (`uname -m` in step 1 prints `x86_64`).
- An administrator login over SSH. SSH is a way to type commands on the NAS from another computer.
- A folder of SACD images on the NAS. This page uses `/Volume1/music`; change it to yours.
- About 100 MB free on `/Volume1`.

## Steps

1. **Log in over SSH as root.** In TOS's Control Panel, Terminal, switch on SSH *(untested)*. On your computer (Windows: PowerShell; macOS: Terminal) type `ssh youradmin@your-nas-address`, then `sudo -i` and your password *(untested: the VM login was already root)*. Check:

   ```sh
   id -u && uname -m
   ```

   *You should see* `0` and `x86_64`.
2. **Check that FUSE is there.** FUSE is the part of the system that lets sacdfs show files. TOS has it, and `fusermount3`.

   ```sh
   ls -l /dev/fuse
   ```

   *You should see* a line starting with `crw`.
3. **Make the folders and go into the first.** `/Volume1/sacd` is where the DSF and WAV files will appear; anything already in it is hidden while sacdfs runs.

   ```sh
   mkdir -p /Volume1/sacdfs /Volume1/sacd
   cd /Volume1/sacdfs
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
6. **Unpack it and put the program and the start-up unit in place.** The unit is the file that tells systemd (the part of the system that starts services) how to run sacdfs.

   ```sh
   tar -xzf sacdfs-0.6.0-x86_64-linux.tar.gz
   mkdir -p /usr/local/bin
   cp sacdfs-0.6.0-x86_64-linux/sacdfs /usr/local/bin/sacdfs
   cp sacdfs-0.6.0-x86_64-linux/sacdfs.service /etc/systemd/system/sacdfs.service
   /usr/local/bin/sacdfs --version
   ```

   *You should see* `sacdfs 0.6.0`.
7. **Write the settings file.** Change the first line to your images folder.

   ```sh
   cat > /etc/default/sacdfs <<'EOF'
   SACDFS_IMAGES=/Volume1/music
   SACDFS_MOUNT=/Volume1/sacd
   EOF
   ```
8. **Start sacdfs, and make it start with the NAS.**

   ```sh
   systemctl daemon-reload
   systemctl enable --now sacdfs
   systemctl is-active sacdfs
   ls /Volume1/sacd
   ```

   *You should see* `active`, then one folder per image. Each holds `Stereo` and, on multichannel discs, `Multichannel`, with files like `01 - Title.dsf` and `01 - Title.wav`. sacdfs goes on running after you log out.
9. **Restart the NAS, and look again.** systemd stops sacdfs cleanly on a restart. (Never stop sacdfs with `kill -9`: it leaves a dead mount that fails with `Transport endpoint is not connected` until someone runs `umount -l` on it.)

   ```sh
   reboot
   ```

   Wait about 3 minutes, log in again, and run `systemctl is-active sacdfs` and `ls /Volume1/sacd`. *You should see* `active` and the same folders.

To stop sacdfs by hand: `systemctl stop sacdfs` (it waits up to 30 seconds for open files, then clears the mount). To start it again: `systemctl start sacdfs`. Never use `kill -9`.

## Check it worked

```sh
systemctl is-active sacdfs
ls /Volume1/sacd
```

*You should see* `active` and your images as folders. To play, share `/Volume1/sacd` with TerraMaster's own file sharing (SMB or NFS) and open it from your player; see [the player guides](README.md).

## If something goes wrong

- **You see:** `sacdfs: FUSE is not loaded: /dev/fuse does not exist.` **Do this:** `modprobe fuse`, then `systemctl restart sacdfs`. The unit also does this by itself at start-up.
- **You see:** `sha256sum: ... no properly formatted SHA256 checksum lines found`, or `gzip: stdin: not in gzip format`. **Do this:** the download saved an error page. Delete the two downloaded files and run step 4 again later.
- **You see:** `systemctl is-active sacdfs` prints `failed` or `activating`. **Do this:** type `journalctl -u sacdfs -n 20` and read the last lines. The usual cause is a wrong path in `/etc/default/sacdfs` (step 7); fix it and type `systemctl restart sacdfs`.

## Details

### What was checked

Checked on TOS 7.0 in a VM, not on TerraMaster hardware. For the release binary, storage setup (a pool and volume made with TOS's own Storage Manager) and the App Center weren't exercised; the data volume was mounted before TOS started, not by TOS's storage manager.

*0.6.0 run, from a clean system:* steps 1 (the `id -u` check only) to 9 as written, with the published 0.6.0 archive, using three images of the conformance corpus (22 DSF and WAV files). The checksum was `OK`, `--version` printed `sacdfs 0.6.0`, `systemctl is-active` printed `active`, and all 22 files hashed identically to a reference read by a Linux host with the same release. After `reboot` the service was `active` when the VM was 1 minute up, with the same 22 files identical; `systemctl stop` while idle took under 0.1 s and left no mount. This run found that `/usr/local/bin` doesn't exist on TOS, which is why step 6 has `mkdir -p /usr/local/bin`.

*Earlier versions:* with 0.5.0 the old steps (by hand, `/Volume1`) read all 22 files of two test images identical to the reference. NFS (`/Volume1/sacd 127.0.0.1(ro,fsid=4242,no_subtree_check)` in `/etc/exports`, `exportfs -ra`, mounted on the NAS itself as NFSv3 and NFSv4) read all 22 files identical over each. With 0.4.1-rc.3 a Samba share of the mount (one read-only guest share) read with `smbclient` gave correct files. The unit was checked on a 0.5.0 binary: `systemctl stop` while idle unmounted at once; a sacdfs killed with `kill -9` was started again by the unit 7 s later; a dead mount left behind was cleared at the next start; after a restart of the VM the service was active; stopping with a file held open took the full 30 s, after which systemd killed sacdfs and no mount was left.

**Not checked:** whether the unit waits for the volume on real hardware, whether a TOS update keeps the unit file, TOS's own NFS and SMB settings, and a client on another machine.

### Options and reasons

- **Why `cp` and not `install`:** for root on TOS, `install` is TerraMaster's own tool (`/usr/sbin/install`), which stops with `flag provided but not defined: -m`.
- **A line added to `/etc/exports` by hand doesn't last:** it stayed while NFS ran, but every restart of the NFS service left the file empty (which TOS tool empties it wasn't traced). Use TOS's own NFS settings *(untested)*.
- **Why `/usr/local/bin`:** the unit expects the program there (`mkdir -p` makes the folder where it is missing, as on TOS). Keep the settings in `/etc/default/sacdfs`.

### Container route

TerraMaster's Container Manager and Docker Engine come from the App Center and need a volume made with TOS's Storage Manager first; the Docker Engine is a separate App Center entry, so install both (in the VM: Docker 29.4.0, Compose v5.1.2). [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml) ran as published, with only the images path changed to `/Volume1/music`, from `/Volume1/docker/sacdfs/compose.yaml`, with `docker compose up -d` over SSH (TOS's own file services use 445 and 2049, so the file publishes 10445 and 12049). With the published 0.5.0 image: healthy 8 s after `up`; SMB on 10445 (`smbclient`), a 457-track corpus (914 files) matched the reference checksums, 0 failures; `docker compose restart` healthy again, same SHA-256; `docker compose stop` 2 s, exit 0; `docker stop` 2 s after a fresh start, 5 times: the first took 11 s (the 0.5.0 image's stop delay, fixed in 0.6.0), the others under 0.8 s. `mount.cifs` stopped with "not installed setuid root" even as root, cause unknown, so `smbclient` was used. Not checked: the Container Manager screens, NFS, a client on another machine, hardware. See [docker-podman.md](docker-podman.md).

### More problems

- **You see:** `Permission denied` when a share or an app opens the folder. **Do this:** sacdfs must run as root, as the unit does. A sacdfs you started by hand as another user, without `--allow-other`, is the usual cause: `systemctl stop sacdfs`, `umount /Volume1/sacd` if it is still mounted, then `systemctl start sacdfs`.
- **You see:** `umount: ... Device or resource busy`. **Do this:** something uses the folder (a share, an app, or a login whose current folder is inside it). Stop it or `cd /`, then try again.
- **You see:** `Illegal instruction`. **Do this:** you used the `x86_64-v3` build on a CPU without AVX2; use the `x86_64` archive ([binary.md](binary.md#binaries)).
- **The unit, step by step, and what it does when sacdfs crashes:** [nas-binary.md](nas-binary.md#start-at-boot-and-stop-safely).

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).

