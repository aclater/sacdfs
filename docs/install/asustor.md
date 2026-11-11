# ASUSTOR ADM: install sacdfs

sacdfs shows your SACD images (`.iso` files) as folders of DSF and WAV files that any player can read. This page installs it from the release file and starts it by itself whenever the NAS starts. About 15 minutes.

Checked on 0.6.0 on ADM 5.1.4 (in a VM, not on ASUSTOR hardware). Steps marked *(untested)* were not run.

From the release after 0.6.1, [the installer](README.md#the-installer-one-line) does these steps for you in one line; this page is the way to do them by hand.

## What you need

- An ASUSTOR NAS with an Intel or AMD chip (`uname -m` in step 1 prints `x86_64`).
- An administrator login over SSH. SSH is a way to type commands on the NAS from another computer.
- A folder of SACD images on the NAS. This page uses `/volume1/music`; change it to yours.
- About 100 MB free on `/volume1`.

## Steps

1. **Log in over SSH as root.** In ADM, Settings, Services, Terminal, tick "Enable SSH service" *(untested)*. On your computer (Windows: PowerShell; macOS: Terminal) type `ssh youradmin@your-nas-address`, then `sudo -i` and your password *(untested: the VM login was already root)*. Check:

   ```sh
   id -u && uname -m
   ```

   *You should see* `0` and `x86_64`.
2. **Check that FUSE is there.** FUSE is the part of the system that lets sacdfs show files. ADM has it.

   ```sh
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
9. **Make it start and stop with the NAS.** ADM runs every `S??*` file in `/usr/local/etc/init.d` with `start` at start-up, and every `K??*` file with `stop` at shut-down (it waits up to 60 seconds). One file does both. (The stop matters: never stop sacdfs with `kill -9`, it leaves a dead mount.)

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
   ls -l /usr/local/etc/init.d
   ```

   *You should see* `S99sacdfs` and the link `K01sacdfs`.
10. **Restart the NAS, and look again.** Restart it from ADM's web page, or type:

    ```sh
    reboot
    ```

    Wait about 2 minutes, log in again, and run `ls /volume1/sacd`. *You should see* the same folders.

To stop sacdfs by hand: `SACDFS_CONF=/volume1/sacdfs/sacdfs.conf /volume1/sacdfs/sacdfs-ctl stop`. Never use `kill -9`.

## Check it worked

```sh
ls /volume1/sacd
```

*You should see* your images as folders. To play, share `/volume1/sacd` with ADM's own file sharing (SMB or NFS) and open it from your player; see [the player guides](README.md).

## If something goes wrong

- **You see:** `sacdfs: the folder to mount on, /volume1/sacd, does not exist.` **Do this:** `mkdir -p /volume1/sacd` (step 3), then step 8 again.
- **You see:** `sha256sum: ... no properly formatted SHA256 checksum lines found`, or `gzip: stdin: not in gzip format`. **Do this:** the download saved an error page. Delete the two downloaded files and run step 4 again later.
- **You see:** `Permission denied` when a share or an app opens the folder. **Do this:** you started sacdfs without being root. Run `id -u` (it must print `0`), stop sacdfs (see above) and start it again.

## Details

### What was checked

Checked on ADM 5.1.4 in a VM, not on ASUSTOR hardware. Storage setup and app stores weren't exercised: ADM's first-run setup, tried with two extra SATA disks, listed no disks, so no volume was made with ADM's own tools.

*0.6.0 run, from a clean system:* steps 1 (the `id -u` check only) to 10 as written, with the published 0.6.0 archive, using three images of the conformance corpus (22 DSF and WAV files). The checksum was `OK`, `--version` printed `sacdfs 0.6.0`, and all 22 files hashed identically to a reference read by a Linux host with the same release. After `reboot`, `S99sacdfs` had started sacdfs by itself when the VM was 1 minute up (one sacdfs process, one mount), with the same 22 files identical. The stop on shut-down wasn't looked at on 0.6.0.

*Earlier versions:* with 0.5.0 the same steps (by hand) gave all 22 files of two test images identical to the reference. A Samba guest share of `/volume1/sacd` (ADM's Samba 4.20, a minimal hand-made `smb.conf` on port 4450, mounted on the NAS with `mount.cifs`) read all 22 files with `--allow-other`; without it the mount failed with "Permission denied". With 0.4.1-rc.3 an NFSv3 export, mounted on the same system, was correct after `modprobe nfsd nfsv3 cifs`. The `init.d` files were checked on a 0.6.0 development build: sacdfs mounted at boot (18 files identical) and `signal 15: unmounting` was logged after a restart from ADM.

**Not checked:** ADM's web screens, `sudo -i`, a client on another machine, and whether `/usr/local/etc/init.d` survives an ADM update.

### Options and reasons

- **Why `/volume1/sacdfs` and not a system folder:** keep sacdfs on the data volume. The settings file lives there too, because `/etc/default` isn't kept across restarts on all NAS systems.
- **Why one file for both start and stop:** `S99sacdfs` and the link `K01sacdfs` run the same script; ADM passes `start` or `stop`.
- **ADM ran `S99sacdfs start` twice** at each boot in the VM, a second apart or in the same second. `sacdfs-ctl` takes a lock, so the second run says "already running" and does nothing. Without a lock, 20 of 25 pairs of starts at once left two sacdfs processes, and one kept a mount that `stop` didn't reach.
- **Every `sacdfs-ctl` command and setting:** [nas-binary.md](nas-binary.md#start-at-boot-and-stop-safely).

### Container route

ADM's Docker package (Docker CE 25.0.5, Compose v2.24.7) ran [deploy/nas-compose.yaml](../../deploy/nas-compose.yaml), the one Compose file for every NAS container manager, as a Compose project. Put it in a folder of its own (for example `/volume1/docker/sacdfs/compose.yaml`), change the one marked line, `/volume1/music`, to your images folder, and run:

```sh
docker compose up -d
docker inspect -f '{{.State.Health.Status}}' sacdfs
```

*You should see* `healthy` within a minute or so (31 to 40 s in the checks). `docker logs sacdfs` shows the number of image files, `mounted`, the SMB share and `NFS SERVER INITIALIZED`. Ports 10445 and 12049 are used because ADM's own services hold 445 and 2049; clients name the port ([linux-client.md](linux-client.md#on-other-ports)). `apparmor=unconfined` is for NAS systems with AppArmor; ADM has none and Docker accepted it. More: [docker-podman.md](docker-podman.md).

What was run (0.5.0 image, in the VM): healthy 40 s after `up`; SMB on 10445 mounted on the NAS with `mount.cifs` (`port=10445,guest,ro,vers=3.0`) read all 22 files identical; a 457-track corpus (914 files) over SMB and over NFS 4.2 on 12049 (from a Fedora 44 container with `--network host`, since ADM's `mount` has no NFSv4) matched the SHA-256 of a Linux client on another system; `docker compose restart` was back in 1 s and healthy 31 s later; `docker stop` 2 s after a start, 15 times, took over 10 s in 3 cases with the 0.5.0 image (fixed in 0.6.0: [docker-podman.md](docker-podman.md#stopping-and-restarting)).
In the VM, App Central's Docker package wouldn't install (`apkg --install` exited with 22 and no message), so Docker was started with the package's own `CONTROL/start-stop.sh start`, which needs `/usr/builtin/sbin` on the PATH ("iptables not found" otherwise) and the package's `bin` folder ("invalid userland-proxy-path" otherwise); this is not a recommendation. Whether App Central does all this on a real NAS, and the ADM Docker app's own screens, weren't checked.

### More problems

- **You see:** `./sacdfs: not found`. **Do this:** use the full path, `/volume1/sacdfs/sacdfs`.
- **You see:** `command not found` for a tool such as `curl` or `umount`. **Do this:** ADM keeps some tools in `/usr/builtin/bin` and `/usr/builtin/sbin`, which may not be on an SSH session's PATH; type `export PATH=$PATH:/usr/builtin/bin:/usr/builtin/sbin` and try again.
- **You see:** `umount: ... Device or resource busy`. **Do this:** a share or app serves the folder; stop it first, or `cd /`, then try again.
- **You see:** `... a sacdfs mount of this user's is still mounted there ...`. **Do this:** sacdfs already runs on that folder; `sacdfs-ctl stop` first.
- **You see:** `Illegal instruction`. **Do this:** you used the `x86_64-v3` build on a CPU without AVX2; use the `x86_64` archive ([binary.md](binary.md#binaries)).

Reports from real hardware are welcome on [#82](https://github.com/aclater/sacdfs/issues/82).
