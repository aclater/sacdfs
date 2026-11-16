# macOS: mount sacdfs on a Mac

This shows your SACD images as a folder of DSF and WAV files on a Mac. The Mac's own NFS client
mounts the folder (NFS is the Network File System, here used only inside the Mac), so there is
nothing else to install and no security setting to change: no macFUSE, no FUSE-T, no kernel
extension, no restart.

Checked on macOS 27.2 in a VM on Apple hardware (Apple Silicon, from the command line), and on
GitHub's macOS 26 (Apple Silicon) and macOS 15 (Intel) runners, with sacdfs built from its
source before 0.6.5. See [Details](#details).

**Build from source until the first macOS release.** Releases don't include Mac programs yet:
they will once sacdfs's NFS layer has moved to a maintained library (#420). Until then, steps 1
to 3 build sacdfs on the Mac: about 10 minutes, and about 2 GB of disk for Apple's command
line tools and Rust.

## What you need

- A Mac with macOS 11 or later, Apple Silicon or Intel, and the Terminal app.
- A folder with your SACD images (`.iso` files) on the Mac, or on a share the Mac has mounted.
  The examples use `~/Music/SACD images`.

## Steps

1. **Install Apple's command line tools** (the compiler's linker):

   ```sh
   xcode-select --install
   ```

   A window asks to install them: click Install. If it says they are installed already, go
   on.

2. **Install Rust**, for your user only:

   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   source ~/.cargo/env
   ```

   *You should see* `Rust is installed now. Great!`.

3. **Build and install sacdfs** with the Mac's mount (`--features nfs`):

   ```sh
   cargo install --locked --features nfs --git https://github.com/aclater/sacdfs sacdfs
   ~/.cargo/bin/sacdfs-nfs --version
   ```

   *You should see* `Installed package sacdfs ...` (`executables sacdfs, sacdfs-nfs`), then
   `sacdfs-nfs` and its version. The programs are in `~/.cargo/bin`.

4. **Make an empty folder to mount on, and start sacdfs.**

   ```sh
   mkdir -p ~/SACD
   ~/.cargo/bin/sacdfs-nfs ~/Music/"SACD images" ~/SACD
   ```

   *You should see* `mounted on /Users/you/SACD` and `serving ... on 127.0.0.1:20501` (the
   port differs per user). It keeps running in this Terminal window.

5. **Open the folder.** In another Terminal window, `ls ~/SACD`, or in the Finder, Go > Home,
   then SACD. Each image is a folder with `Stereo` and `Multichannel` folders of DSF and WAV
   files. The files can't be changed.

6. **Stop it** with Control-C in its window. sacdfs unmounts the folder, then stops.

## Start it at each login

Instead of step 4, write a launch agent with the same command and `--launch-agent`:

```sh
~/.cargo/bin/sacdfs-nfs --launch-agent ~/Music/"SACD images" ~/SACD
launchctl load -w ~/Library/LaunchAgents/io.github.aclater.sacdfs-nfs.plist
```

*You should see* `wrote /Users/you/Library/LaunchAgents/io.github.aclater.sacdfs-nfs.plist`,
then `~/SACD` fills within a few seconds. It now starts at each login, and again if it fails.
Its log is `~/Library/Logs/sacdfs-nfs.log`. To stop it and keep it from starting:

```sh
launchctl unload -w ~/Library/LaunchAgents/io.github.aclater.sacdfs-nfs.plist
```

## Options

`sacdfs-nfs` takes the same options as `sacdfs` on Linux ([options](../options.md)): for
example `--format dsf` for DSF files only, or `--pcm-rate 88200` for smaller WAV files. These
are its own:

- `--port PORT`: the port (default 20000 plus the last four digits of your user ID; 1007 for
  root).
- `-o LIST`: more mount options, such as `-o nobrowse`, which keeps the mount out of the
  Finder's sidebar.
- `--allow-other`: answer every user (see below). `--listen ADDR`: listen on another address
  than 127.0.0.1, which serves the files to whoever can reach it.

## Other users of the Mac

Each user runs their own `sacdfs-nfs` on their own port. Only you (and root) can read your
mount: sacdfs refuses requests made as another user. This guards against mistakes, not against
someone who wants the files: a program that speaks NFS itself can claim to be you. **Any
user of the Mac can read the files served**, as with `--allow-other` on Linux.

## If something goes wrong

- **`cannot listen on 127.0.0.1:20501: Address already in use`:** another `sacdfs-nfs` of
  yours is running (`pgrep -l sacdfs-nfs`), or another program has the port. Stop it, or add
  `--port 20600`.
- **Programs reading the folder wait:** sacdfs stopped while the folder was mounted. Start it
  again with the same command within a minute: the mount picks it up, open files too. After
  about a minute without it, reads fail with `Device not configured` and macOS unmounts the
  folder itself.
- **The folder is empty:** sacdfs isn't running; see the step 4 window or the log.

## Details

- **How it works:** `sacdfs-nfs` serves NFS version 3 on 127.0.0.1 and runs `mount_nfs` as
  you. `sacdfs SOURCE MOUNTPOINT` does the same on a Mac, and `sacdfs verify` and
  `sacdfs tags` work as on Linux. The mount is read-only, `nosuid`, `nodev` and `noexec`; names are found in both Unicode
  forms (`nfc`). See [design](../design.md#the-nfs-front-end-420).
- **Spotlight** doesn't index the mount, as for NFS mounts in general.
- **What was checked in the VM** (from the command line, as the user, with no root):
  - every one of the 910 corpus files is byte-identical to a Linux FUSE mount;
  - all 1,252 names are found in both Unicode forms;
  - the mount is read-only and gets no `._` files, and its size is the images' total;
  - sacdfs killed and started again while a file was half read: the read goes on with the same
    bytes; a read during a 20 s restart waits and completes;
  - sacdfs killed for good: reads fail after about a minute and macOS unmounts the folder;
  - two users each with their own mount at once, neither able to read the other's;
  - the launch agent, loaded and unloaded with `launchctl` (not across a real logout and
    login);
  - Control-C and SIGTERM unmount before sacdfs stops.
- **Checked elsewhere:** WAV files over 4 GiB (RF64, at `--pcm-rate 352800`) through the same
  server with Linux's NFS client: byte-identical to FUSE, also across the 4 GiB mark. No corpus
  image is that long, and real discs don't go into the VM.
- **Not checked yet:** the Finder, Quick Look, "Connect to Server" (`nfs://localhost:PORT/`),
  players (Audirvana, Roon Bridge, Swinsian and others), sleep and wake (the VM without a screen
  doesn't sleep), a real login, speed against Linux on real discs, and macOS older than 15.
  They need a Mac with a screen and real discs: a VM with graphics, or a Mac set aside for
  tests.
- **The build check:** the VM got the command line tools with `softwareupdate` instead of
  the `xcode-select --install` window, which needs a screen; steps 2 and 3 ran as written
  (from the branch of this change, before it was on main).
- **Signing:** built on your own Mac, the programs need no signature. Release archives, when
  they come, won't be signed by a developer or notarised by Apple either (see
  [design](../design.md#the-nfs-front-end-420)).
