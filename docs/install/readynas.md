# Netgear ReadyNAS OS 6: install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that any player can read. This page installs it from the release file and starts it by itself whenever the NAS starts. About 15 minutes.

Checked on 0.6.0 on ReadyNAS OS 6.10.10 (in a VM, not on the vendor's hardware). Steps marked *(untested)* were not run.

Since 0.6.2, [the installer](README.md#the-installer-one-line) does these steps for you in one line; this page is the way to do them by hand.

## What you need

- A Netgear ReadyNAS running ReadyNAS OS 6 with an Intel or AMD chip (`uname -m` in step 1 prints `x86_64`).
- An administrator login over SSH. SSH is a way to type commands on the NAS from another computer.
- A folder of SACD images on the NAS. This page uses `/data/music`; change it to yours.
- About 100 MB free on `/data`.

## Steps

1. **Log in over SSH as root.** In the ReadyNAS web page, System, Settings, Services, switch on SSH *(untested)*. On your computer (Windows: PowerShell; macOS: Terminal) type `ssh youradmin@your-nas-address`, then `sudo -i` and your password *(untested: the VM login was already root)*. Check:

   ```sh
   id -u && uname -m
   ```

   *You should see* `0` and `x86_64`.
2. **Check that FUSE is there.** FUSE is the part of the system that lets sacdfs show files. ReadyNAS OS 6 has it built in.

   ```sh
   ls -l /dev/fuse
   ```

   *You should see* a line starting with `crw`.
3. **Make the folders and go into the first.** `/data/sacd` is where the DSF and WAV files will appear; anything already in it is hidden while sacdfs runs.

   ```sh
   mkdir -p /data/sacdfs /data/sacd
   cd /data/sacdfs
   ```
4. **Download sacdfs and its checksum file.**

   ```sh
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.3/sacdfs-0.6.3-x86_64-linux.tar.gz
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.3/sacdfs-0.6.3-x86_64-linux.tar.gz.sha256
   ```

   *You should see* two progress tables and no line starting `curl: (`.
5. **Check the download.**

   ```sh
   sha256sum -c sacdfs-0.6.3-x86_64-linux.tar.gz.sha256
   ```

   *You should see* `sacdfs-0.6.3-x86_64-linux.tar.gz: OK`.
6. **Unpack it and put the program and the start-up unit in place.** The unit is the file that tells systemd (the part of the system that starts services) how to run sacdfs.

   ```sh
   tar -xzf sacdfs-0.6.3-x86_64-linux.tar.gz
   mkdir -p /usr/local/bin
   cp sacdfs-0.6.3-x86_64-linux/sacdfs /usr/local/bin/sacdfs
   cp sacdfs-0.6.3-x86_64-linux/sacdfs.service /etc/systemd/system/sacdfs.service
   /usr/local/bin/sacdfs --version
   ```

   *You should see* `sacdfs 0.6.3`.
7. **Write the settings file.** Change the first line to your images folder.

   ```sh
   cat > /etc/default/sacdfs <<'EOF'
   SACDFS_IMAGES=/data/music
   SACDFS_MOUNT=/data/sacd
   EOF
   ```
8. **Start sacdfs, and make it start with the NAS.**

   ```sh
   systemctl daemon-reload
   systemctl enable --now sacdfs
   systemctl is-active sacdfs
   ls /data/sacd
   ```

   *You should see* `active`, then one folder per image. Each holds `Stereo` and, on multichannel discs, `Multichannel`, with files like `01 - Title.dsf` and `01 - Title.wav`. sacdfs goes on running after you log out.
9. **Restart the NAS, and look again.** systemd stops sacdfs cleanly on a restart. (Never stop sacdfs with `kill -9`: it leaves a dead mount that fails with `Transport endpoint is not connected` until someone runs `umount -l` on it.)

   ```sh
   reboot
   ```

   Wait about 3 minutes, log in again, and run `systemctl is-active sacdfs` and `ls /data/sacd`. *You should see* `active` and the same folders.

To stop sacdfs by hand: `systemctl stop sacdfs` (it waits up to 30 seconds for open files, then clears the mount). To start it again: `systemctl start sacdfs`. Never use `kill -9`.

## Check it worked

```sh
systemctl is-active sacdfs
ls /data/sacd
```

*You should see* `active` and your images as folders. To play, share `/data/sacd` with ReadyNAS's own file sharing (SMB or NFS) and open it from your player; see [the player guides](README.md).

## If something goes wrong

- **You see:** `sacdfs: FUSE is not loaded: /dev/fuse does not exist.` **Do this:** `modprobe fuse`, then `systemctl restart sacdfs`. The unit also does this by itself at start-up.
- **You see:** `sha256sum: ... no properly formatted SHA256 checksum lines found`, or `gzip: stdin: not in gzip format`. **Do this:** the download saved an error page. Delete the two downloaded files and run step 4 again later.
- **You see:** `systemctl is-active sacdfs` prints `failed` or `activating`. **Do this:** type `journalctl -u sacdfs -n 20` and read the last lines. The usual cause is a wrong path in `/etc/default/sacdfs` (step 7); fix it and type `systemctl restart sacdfs`.

## Details

### What was checked

Checked on ReadyNAS OS 6.10.10 (Debian 8: glibc 2.19, kernel 4.4, systemd 230) in a VM, not on the vendor's hardware. The data volume was a plain ext4 volume at `/data`, not one made by the vendor's tools. (A later try with two extra SATA disks made a RAID 1 volume with the vendor's `volume_util`, which came up as a Btrfs `/data`; the sacdfs checks were not repeated on it.) The web interface was not set up or used; app stores weren't exercised.

*The unit with its hardening options (0.6.4, #387):* the 0.6.3 binary under the old unit and under the new one, on ReadyNAS OS 6.10.10 (systemd 230) from a clean system, in a VM, not on the vendor's hardware. Each started; the mount was visible on the NAS and read by another user; all 914 files of the corpus hashed identically to a reference; a restart, a stop (mount gone), the index cache and a restart after `kill -9` all worked. Under the new unit sacdfs held only the capabilities to mount and to read (from `/proc`). systemd 230 doesn't know most of the newer options: it logs `Unknown lvalue` for each and runs the unit without them. See [systemd unit](../deployment.md#systemd-unit).

*0.6.0 run, from a clean system:* steps 1 (the `id -u` check only) to 9 as written, with the published 0.6.0 archive, using three images of the conformance corpus (22 DSF and WAV files). Downloading and checking the archive on the NAS itself worked (an earlier check had copied the binary in). The checksum was `OK`, `--version` printed `sacdfs 0.6.0`, `systemctl is-active` printed `active`, and all 22 files hashed identically to a reference read by a Linux host with the same release. After `reboot` the service was `active` when the VM was 2 minutes up, with the same 22 files identical.

*Earlier versions:* 0.5.0 ran on glibc 2.19. The unit was checked on a 0.5.0 binary: the first start took 15.3 s (cause not looked into); all 22 files of two test images identical to the reference; `systemctl stop` while idle took 0.0 s with no mount left; with a file held open it took 30.2 s, then systemd killed sacdfs, no mount left; after `kill -9` the unit had it running again 7 s later; a dead mount was cleared at the next start; after a restart of the VM the service started on its own during boot (about 1.5 minutes into this VM's slow boot). `sacdfs-ctl` worked here too ([nas-binary.md](nas-binary.md#start-at-boot-and-stop-safely)).

**Not checked:** the vendor's web interface and add-ons, volumes made by ReadyNAS itself, a client on another machine, hardware, and the container route.

### Options and reasons

- **Why systemd and the unit:** ReadyNAS OS 6 is Debian 8 and uses systemd. FUSE is built into the kernel, so `/dev/fuse` is there and `modprobe fuse` isn't needed.
- **Your data volume's path:** on a real ReadyNAS it is `/data` or a share under it (`/data/music`); the VM used `/data`.
- **Sharing with ReadyNAS's own Samba** (Samba 4.8, reads `/etc/frontview/samba/Shares.conf`): in the VM, appending

  ```ini
  [sacd]
   path = /data/sacd
   read only = yes
   guest ok = yes
  ```

  to that file and running `smbcontrol smbd reload-config` made the share appear, and `smbclient //127.0.0.1/sacd -N` read one DSF file identical to the reference (sacdfs ran as root, with `--allow-other`). Not checked: whether the web interface rewrites `Shares.conf` (it probably does when you change shares there), a client on another machine, and NFS.
- **Containers:** untested on ReadyNAS; see [docker-podman.md](docker-podman.md).

### More problems

- **You see:** `Permission denied` when a share or an app opens the folder. **Do this:** sacdfs must run as root, as the unit does. A sacdfs you started by hand as another user, without `--allow-other`, is the usual cause: `systemctl stop sacdfs`, `umount /data/sacd` if it is still mounted, then `systemctl start sacdfs`.
- **You see:** `umount: ... Device or resource busy`. **Do this:** something uses the folder (a share, an app, or a login whose current folder is inside it). Stop it or `cd /`, then try again.
- **You see:** `Illegal instruction`. **Do this:** you used the `x86_64-v3` build on a CPU without AVX2; use the `x86_64` archive ([binary.md](binary.md#binaries)).
- **The unit, step by step, and what it does when sacdfs crashes:** [nas-binary.md](nas-binary.md#start-at-boot-and-stop-safely).

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).

