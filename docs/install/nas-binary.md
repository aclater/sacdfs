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
[TerraMaster](terramaster.md), [UGREEN](ugreen-ugos.md).

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
   it. NFS needs it too ([user guide](../user-guide.md#sharing-the-mount-over-nfs)).
8. **Check it works.**

   ```sh
   ls /volume1/sacd
   ```

   *You should see* one folder per image. Each holds `Stereo` and, on multichannel discs,
   `Multichannel`, with files such as `01 - Title.dsf` and `01 - Title.wav`.
9. **To stop it:**

   ```sh
   umount /volume1/sacd
   ```

**After the NAS restarts,** sacdfs isn't running, and Synology also needs `modprobe fuse`
again. Run step 7 again. A way to start it at boot was checked only on TerraMaster (a
systemd service; see [terramaster.md](terramaster.md)). The vendors' own schedulers and
startup scripts weren't checked.

**Sharing the folder.** Clients reach the files through the NAS's own SMB or NFS sharing
of `/volume1/sacd`, or of a folder above it. The vendors' sharing settings weren't checked.
What was checked on each system (hand-made NFS exports and a test Samba share) is in its
guide.

## If something goes wrong

The messages are as they appeared in these checks, except for the last two rows: the glibc
message is from an earlier check of 0.4.1 on QNAP, and the last row is from [binary.md](binary.md#binaries).

| You see | Why | What to do |
|---|---|---|
| `./sacdfs: No such file or directory` or `./sacdfs: not found` | `./` means "in this folder", and sacdfs is in another one | Use the full path, as in step 7: `/volume1/sacdfs/sacdfs` |
| `sacdfs: /volume1/sacd: No such file or directory (os error 2)` | The folder to mount on doesn't exist | Step 6: `mkdir -p /volume1/sacd` |
| `/dev/fuse not found. Try 'modprobe fuse'`, then `sacdfs: cannot mount on /volume1/sacd: No such file or directory` | FUSE isn't loaded. The second line names the mount folder, but the missing thing is `/dev/fuse` | `modprobe fuse`, then step 7 again |
| `Permission denied` when a share, an app or another user opens the folder | sacdfs was started without `--allow-other` | `umount /volume1/sacd`, then step 7 as written |
| `sha256sum: ... no properly formatted SHA256 checksum lines found`, or `gzip: stdin: not in gzip format` | The download saved an error page instead of the file, for example while GitHub was down | Delete both files and run step 3 again later. `curl -f` (step 3) reports the error instead of saving it |
| `curl: (28) Operation timed out` | GitHub didn't answer | Try again later |
| `umount: ... Device or resource busy` | Something is using the folder: a share, an app, or a login whose current folder is inside it | Stop that, or `cd /` in that login, then `umount` again |
| `... a sacdfs mount of this user's is still mounted there ...` | sacdfs is already running on that folder | `umount /volume1/sacd` first. The message suggests `fusermount3 -u`, which some NAS systems don't have; as root, `umount` does the same |
| `flag provided but not defined: -m` (TerraMaster) | On TOS, `install` is TerraMaster's own tool, not the usual one | Use `cp`, as in step 5 |
| `` weak version `GLIBC_2.28' not found `` (QNAP) | sacdfs 0.4.1 or earlier, which needs a newer system library | Use 0.5.0 or later |
| `Illegal instruction` | The `x86_64-v3` build on a CPU without AVX2 | Use the `x86_64` build ([binary.md](binary.md#binaries)) |
