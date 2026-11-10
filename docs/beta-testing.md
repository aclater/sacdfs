# Beta testing sacdfs on a NAS

**We want to hear from NAS owners, at any skill level.** Most people who keep Super Audio CD
images keep them on a NAS: Synology, QNAP, UGREEN, Unraid, TrueNAS, ASUSTOR, TerraMaster, OpenMediaVault
or a home-built box. So far sacdfs has been checked on those systems only in virtual machines, which
cannot tell us what real hardware does. A report from your NAS can.

## What to do

1. **Pick your NAS in the [install guides](install/README.md)** and follow its guide.
   A guide that says "Not tested" is exactly where we most need you.
2. **Try it with your player or server**, if you have one: Roon, Plex, Jellyfin, Navidrome, Lyrion, MPD
   or just a file manager. See whether the discs show up, play, and have titles and art.
3. **Tell us how it went** with the
   [NAS test report form](https://github.com/aclater/sacdfs/issues/new?template=nas-test-report.yml).
   You need a free GitHub account. Only a few fields are required.

**Failure reports are just as useful as successes.** "I got stuck at step 3 and saw this message" is
exactly what lets us fix a guide or a bug before everybody else meets it. You don't need to understand
the cause. If you can't finish, tell us where you stopped.

## sacdfs does not touch your ISOs

sacdfs only reads your disc images. It shows them as folders of DSF and WAV files, made on the fly,
and the result is read-only. Nothing is written back to the ISO files, and the guides mount your
images folder read-only (`:ro`) as well, so even a bug in sacdfs could not change them. The only things it
keeps are its own cache (a Docker volume or a folder, which you can delete) and, if you switch them on, tags
files in a folder you choose.

## Collecting the details we need

The commands below only read information. They don't change anything and don't print passwords,
serial numbers or your NAS's name. (We leave out `uname -a` on purpose, because it prints the
host name.) Look at the output before you paste it, and remove anything you'd rather keep private.

You run them in a terminal on the NAS, usually over SSH. Most NAS systems have SSH switched off until
you turn it on in their settings; your vendor's help explains how. If you'd rather not use a terminal,
fill in what you know on the form and skip the rest.

### 1. System details (all platforms)

This block works on any Linux-based NAS. It prints the CPU, RAM, kernel, whether `/dev/fuse` exists,
which `fusermount` helper is installed, the container runtime, and the sacdfs version if the binary
is on your path. Paste it into the terminal as it is:

```sh
echo "== Kernel"; uname -srm
echo "== CPU and RAM"
grep -m1 'model name' /proc/cpuinfo
grep MemTotal /proc/meminfo
echo "== FUSE"
ls -l /dev/fuse 2>&1
grep -w fuse /proc/filesystems || echo "fuse not listed in /proc/filesystems"
command -v fusermount3 fusermount || echo "no fusermount found"
echo "== Container runtime"
docker --version 2>/dev/null || echo "docker: not installed"
podman --version 2>/dev/null || echo "podman: not installed"
echo "== sacdfs"
sacdfs --version 2>/dev/null || echo "sacdfs: not on the PATH (fine for the container route)"
```

What the lines tell us: no `/dev/fuse` and no `fuse` line usually means the FUSE kernel module isn't
loaded, which is the first thing to fix on some NAS systems (the [Synology guide](install/synology.md)
shows how). On ARM NAS systems `model name` may not exist; `uname -srm` still tells us the
architecture.

### 2. The NAS operating system and version (pick your platform)

| Your NAS | Command | Checked |
|---|---|---|
| Synology DSM | `grep -E '^(productversion\|buildnumber)' /etc.defaults/VERSION` | Run on DSM 7.4.1 in a VM |
| ASUSTOR ADM | `grep -E '^(Model\|Version) ' /etc/nas.conf` | Run on ADM 5.1.4 in a VM |
| TerraMaster TOS | `cat /etc/tos/config/version` | Run on TOS 7.0 in a VM |
| QNAP QTS / QuTS hero | `getcfg System Version -f /etc/config/uLinux.conf` | Untested |
| UGREEN UGOS Pro | `head -4 /etc/os-release` | Untested on UGOS Pro (the file exists in the Debian-based VM we had) |
| Unraid | `cat /etc/unraid-version` | Untested |
| TrueNAS SCALE, OpenMediaVault | `head -4 /etc/os-release` | Untested (a plain Debian file; also visible in the web interface under System) |

In the `grep` commands the backslash before the `|` is only for the table; type a plain `|`.
Don't paste `/etc/nas.conf` as a whole: it holds your NAS's name and serial number.

Both Synology and ASUSTOR will also tell you the model in the web interface, which is the easiest
way to fill in "NAS make and model".

### 3. The sacdfs log

Pick the line that matches how you installed sacdfs, and paste the last part of its output into the
form's log box. If nothing happened at all, say so; an empty log is also a result.

| How you run sacdfs | Command | Checked |
|---|---|---|
| Podman | `podman logs --tail 200 sacdfs 2>&1` | Run with Podman 6.1.3 and the 0.4.1 image |
| Docker, Docker Compose, Container Manager, Container Station | `docker logs --tail 200 sacdfs 2>&1` (with Compose: `docker compose logs --tail 200`) | Untested; same syntax as Podman |
| The plain binary, started with `--log-file /path/sacdfs.log` | `tail -n 200 /path/sacdfs.log` | Run with the 0.4.1-rc.3 binary on a TerraMaster TOS 7.0 VM and an ASUSTOR ADM 5.1 VM |
| The plain binary, started without `--log-file` | Stop it, then start it again with `-v --log-file /path/sacdfs.log` ([options](options.md)) | Same |
| A NAS web interface (container or app logs page) | Open the container or app, find "Log" or "Logs", copy the end | Untested |

If you use the second binary row, try the thing that failed again, then copy the log. A mount that
works logs a line starting `mounted`.

Disc folder names and file names appear in the log. Read it first and replace what you don't want public.
The log never contains the audio.

## Removing sacdfs afterwards

Removing it leaves your ISOs, and anything else in your music folder, exactly as they were.

**Container route** (the same commands work for `docker`; they were run with Podman 6.1.3, the Docker
versions are untested):

```sh
podman stop sacdfs
podman rm sacdfs
podman volume rm sacdfs-cache     # sacdfs's own cache; only if you used a volume of that name
```

If you used Compose, run `docker compose down` in the folder of your compose file instead of the first
two lines. If you also want the image gone: `podman rmi ghcr.io/aclater/sacdfs:TAG`, with the tag you
pulled (untested). Remove any share or mount you added on your other computers, or they will point at
nothing. In a NAS web interface, stop and delete the container or app there instead (untested); its
volume or data folder is deleted separately.

**Binary route:**

1. Stop whatever starts it: delete the entry you added in the NAS's task scheduler or startup scripts,
   the `/etc/fstab` line, or the systemd service (`systemctl disable --now sacdfs`, then delete the
   unit file).
2. Unmount it: `fusermount3 -u /path/to/mountpoint`, `fusermount -u /path/to/mountpoint`, or as
   root `umount /path/to/mountpoint`. Unmounting this way was tried on the TerraMaster
   (`fusermount3 -u`) and ASUSTOR (`umount`) VMs; the mount was gone afterwards.
3. Delete the `sacdfs` file, its log, and the empty mountpoint folder. Any NFS or SMB share you made of
   the mount should be removed from the NAS's sharing settings.

Check with `ls /path/to/mountpoint`, which should now be empty, and with
`mount | grep sacdfs`, which should print nothing.

## Questions

Ask in the report, or in [an issue](https://github.com/aclater/sacdfs/issues). Progress on NAS
systems is collected in [#82](https://github.com/aclater/sacdfs/issues/82). Other ways to help are
in [CONTRIBUTING.md](../CONTRIBUTING.md).
