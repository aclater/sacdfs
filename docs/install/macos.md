[sacdfs](../../README.md) › [Documentation](../README.md) › [Install](README.md) › macOS: mount sacdfs on a Mac

# macOS: mount sacdfs on a Mac

This shows your SACD images as a folder of DSF and WAV files on a Mac. The Mac's own NFS client
mounts the folder (NFS is the Network File System, here used only inside the Mac), so there is
nothing else to install and no security setting to change: no macFUSE, no FUSE-T, no kernel
extension, no restart.

Checked with the 0.7.0-rc.1 release archive for Apple Silicon on macOS 27.2 in a VM on Apple
hardware, from the command line. The Intel archive was checked only by the release's own test on
GitHub's macOS 15 (Intel) runner. See [Details](#details).

The programs are not signed by a developer or notarised by Apple. Downloaded with `curl` as
below, macOS starts them; downloaded with a browser, macOS refuses them until you take one step
([Downloaded with a browser](#downloaded-with-a-browser)).

## What you need

- A Mac with macOS 11 or later, Apple Silicon or Intel, and the Terminal app.
- A folder with your SACD images (`.iso` files) on the Mac, or on a share the Mac has mounted.
  The examples use `~/Music/SACD images`.

## Steps

1. **Download the archive and its checksum** in Terminal. On Apple Silicon (M1 and later):

   ```sh
   mkdir -p ~/sacdfs
   cd ~/sacdfs
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.7.2/sacdfs-0.7.2-aarch64-macos.tar.gz
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.7.2/sacdfs-0.7.2-aarch64-macos.tar.gz.sha256
   ```

   On an Intel Mac, the same with `x86_64` (not run in the VM, which is Apple Silicon):

   ```sh
   mkdir -p ~/sacdfs
   cd ~/sacdfs
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.7.2/sacdfs-0.7.2-x86_64-macos.tar.gz
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.7.2/sacdfs-0.7.2-x86_64-macos.tar.gz.sha256
   ```

   The rest of this guide shows the Apple Silicon names; on Intel, put `x86_64` for `aarch64`.

2. **Check the download:**

   ```sh
   shasum -a 256 -c sacdfs-0.7.2-aarch64-macos.tar.gz.sha256
   ```

   *You should see* `sacdfs-0.7.2-aarch64-macos.tar.gz: OK`. On a computer with the GitHub CLI,
   `gh attestation verify sacdfs-0.7.2-aarch64-macos.tar.gz -R aclater/sacdfs` also checks that
   GitHub's release workflow built it.

3. **Unpack it:**

   ```sh
   tar -xzf sacdfs-0.7.2-aarch64-macos.tar.gz
   ~/sacdfs/sacdfs-0.7.2-aarch64-macos/sacdfs-nfs --version
   ```

   *You should see* `sacdfs-nfs 0.7.2`. The folder holds `sacdfs-nfs` (the mount), `sacdfs`
   (`verify` and `tags`), and the licences.

4. **Make an empty folder to mount on, and start sacdfs.**

   ```sh
   mkdir -p ~/SACD
   ~/sacdfs/sacdfs-0.7.2-aarch64-macos/sacdfs-nfs ~/Music/"SACD images" ~/SACD
   ```

   *You should see* `mounted on /Users/you/SACD` and `serving ... on 127.0.0.1:20501` (the
   port differs per user). It keeps running in this Terminal window.

5. **Open the folder.** In another Terminal window, `ls ~/SACD`, or in the Finder, Go > Home,
   then SACD. Each image is a folder with `Stereo` and `Multichannel` folders of DSF and WAV
   files. The files can't be changed.

6. **Stop it** with Control-C in its window. sacdfs unmounts the folder, then stops.

## Downloaded with a browser

A browser marks what it downloads as quarantined, and macOS refuses to start an unsigned program
from such a file. With both files in Downloads, check the archive and remove the mark **before
you unpack it**:

```sh
cd ~/Downloads
shasum -a 256 -c sacdfs-0.7.2-aarch64-macos.tar.gz.sha256
xattr -d com.apple.quarantine sacdfs-0.7.2-aarch64-macos.tar.gz
mkdir -p ~/sacdfs
tar -xzf sacdfs-0.7.2-aarch64-macos.tar.gz -C ~/sacdfs
~/sacdfs/sacdfs-0.7.2-aarch64-macos/sacdfs-nfs --version
```

Then go on with step 4.

If you started the program before removing the mark, macOS refused it. In the VM (no screen),
the program waited instead of starting, and macOS kept refusing that copy even after the mark
was removed; a fresh unpack into another folder started. What macOS shows on a screen, and its
"Open Anyway" button in System Settings, were not checked.

## Alternative: build from source

About 10 minutes and about 2 GB of disk for Apple's command line tools and Rust. Checked on
macOS 27.2 in a VM on Apple hardware with sacdfs before 0.6.5, not with 0.6.5; the VM got the
command line tools with `softwareupdate`, because the `xcode-select --install` window needs a
screen.

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
   `sacdfs-nfs` and its version. The programs are in `~/.cargo/bin`: use
   `~/.cargo/bin/sacdfs-nfs` where this guide says `~/sacdfs/sacdfs-0.7.2-aarch64-macos/sacdfs-nfs`.

## Start it at each login

Instead of step 4, write a launch agent with the same command and `--launch-agent`:

```sh
~/sacdfs/sacdfs-0.7.2-aarch64-macos/sacdfs-nfs --launch-agent ~/Music/"SACD images" ~/SACD
launchctl load -w ~/Library/LaunchAgents/io.github.aclater.sacdfs-nfs.plist
```

*You should see* `wrote /Users/you/Library/LaunchAgents/io.github.aclater.sacdfs-nfs.plist`,
then `~/SACD` fills within a few seconds. It now starts at each login, and again if it fails.
Its log is `~/Library/Logs/sacdfs-nfs.log`. The launch agent names the program by its path, so
keep that folder; after unpacking a newer release elsewhere, write the launch agent again. To
stop it and keep it from starting:

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
user of the Mac can read the files served**, as with `--allow-other` on Linux. If your
`sacdfs-nfs` stops while the folder is mounted, another user could start a server on your port
in the minute before macOS unmounts it, and show you other files there (they can't be run:
the mount is `noexec`).

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
- **What was checked with 0.7.0-rc.1** (macOS 27.2 in a VM on Apple hardware, Apple Silicon,
  from the command line, as the user): all 910 corpus files through the `sacdfs-nfs` mount are
  byte-identical to the Linux read of the same release, names look up in NFC and NFD, writes are
  refused, a read during a 20 s restart waits and completes with the same bytes, and the launch
  agent loads and unloads with `launchctl` (not across a real logout). 0.7.0-rc.2 changed only
  the read path and was not checked on macOS; its Linux bytes are rc.1's.
- **What was checked with 0.6.5** (macOS 27.2 in a VM on Apple hardware, Apple Silicon, from
  the command line, as the user, with no root): the archive from the release run, checked with
  its `.sha256`, unpacked and started as in steps 2 to 6:
  - every one of the 910 corpus files is byte-identical to the Linux hashes of the same release
    run;
  - the mount is read-only (`touch` fails with "Read-only file system") and its size is the
    images' total;
  - Control-C unmounts before sacdfs stops;
  - the launch agent, loaded and unloaded with `launchctl` (not across a real logout and
    login);
  - Gatekeeper turned on: a download without the quarantine mark (`curl`) starts; one with it
    (as a browser sets it) doesn't, until the mark is removed before unpacking.
- **The release's own test** ran `tests/macos-nfs.sh` on each archive, on GitHub's macOS 26
  (Apple Silicon) and macOS 15 (Intel) runners: 910 files hashed, all 1,252 names found in both
  Unicode forms, another user refused, the server killed and started again mid-read with the
  same bytes, and SIGTERM unmounting. That is the only check of the Intel archive.
- **Checked before 0.6.5, built from source** (same VM): a read during a 20 s restart waits and
  completes; sacdfs killed for good: reads fail after about a minute and macOS unmounts the
  folder; two users each with their own mount at once, neither able to read the other's; no
  `._` files.
- **Checked elsewhere:** WAV files over 4 GiB (RF64, at `--pcm-rate 352800`) through the same
  server with Linux's NFS client: byte-identical to FUSE, also across the 4 GiB mark. No corpus
  image is that long, and real discs don't go into the VM.
- **Not checked yet:** the Intel archive on a Mac, the Finder, Quick Look, Gatekeeper's dialog
  on a screen, "Connect to Server" (`nfs://localhost:PORT/`), players (Audirvana, Roon Bridge,
  Swinsian and others), sleep and wake (the VM without a screen doesn't sleep), a real login,
  speed against Linux on real discs, and macOS older than 15. They need a Mac with a screen and
  real discs: a VM with graphics, or a Mac set aside for tests.
- **Signing:** the release programs are ad hoc signed by the linker only: not signed by a
  developer and not notarised by Apple (see [design](../design.md#the-nfs-front-end-420)).
  Built on your own Mac, the programs need no signature.

## See also

- [Options](../options.md)
- [Windows](windows.md)
- [Platforms](../../README.md#platforms)
- [Design](../design.md)

Part of the [DSD Toolbox](https://github.com/aclater/dsd-toolbox).
