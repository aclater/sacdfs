# Linux packages: .rpm and .deb

Each release from 0.6.5 has sacdfs as `.rpm` and `.deb` packages, for x86_64 and aarch64. A
package installs the release's own program as `/usr/bin/sacdfs`, with `sacdfs-ctl`, a systemd
unit and a settings file, and needs FUSE 3 (package `fuse3`, which the package manager installs
with it). About 5 minutes.

| System | Package |
|---|---|
| Fedora 44 and 45, RHEL 9 and 10 | `sacdfs-0.6.5-1.x86_64.rpm`, `sacdfs-0.6.5-1.aarch64.rpm` |
| Debian 12 and 13, Ubuntu 22.04 and 24.04 | `sacdfs_0.6.5-1_amd64.deb`, `sacdfs_0.6.5-1_arm64.deb` |

The packages are not signed with a package key. Check each download against its `.sha256`
file, and, on a computer with the GitHub CLI, against its attestation, which shows that
GitHub's release workflow built it.

The commands below are for x86_64; on aarch64 (`uname -m` prints `aarch64`) use the aarch64
(rpm) or arm64 (deb) file instead. See [Details](#details) for what was checked where.

## Fedora and RHEL

1. **Download the package and its checksum, and check it:**

   ```sh
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.5/sacdfs-0.6.5-1.x86_64.rpm
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.5/sacdfs-0.6.5-1.x86_64.rpm.sha256
   sha256sum -c sacdfs-0.6.5-1.x86_64.rpm.sha256
   ```

   *You should see* `sacdfs-0.6.5-1.x86_64.rpm: OK`. With the GitHub CLI,
   `gh attestation verify sacdfs-0.6.5-1.x86_64.rpm -R aclater/sacdfs` also checks where it
   was built.

2. **Install it:**

   ```sh
   sudo dnf install ./sacdfs-0.6.5-1.x86_64.rpm
   ```

   *You should see* `Complete!`, with `fuse3` installed too if it wasn't there.

3. **Name your folders** in `/etc/sysconfig/sacdfs` (`sudoedit /etc/sysconfig/sacdfs`): the
   folder that holds your SACD images, and an empty folder to show the tracks in. For example:

   ```sh
   SACDFS_IMAGES=/srv/sacd-images
   SACDFS_MOUNT=/srv/sacd
   ```

4. **Start sacdfs, and start it at each boot:**

   ```sh
   sudo systemctl enable --now sacdfs
   ls /srv/sacd
   ```

   *You should see* one folder per disc. `sudo journalctl -u sacdfs` shows its log.

To remove it: `sudo dnf remove sacdfs`. A settings file you changed is kept as
`/etc/sysconfig/sacdfs.rpmsave`.

## Debian and Ubuntu

1. **Download the package and its checksum, and check it:**

   ```sh
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.5/sacdfs_0.6.5-1_amd64.deb
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.6.5/sacdfs_0.6.5-1_amd64.deb.sha256
   sha256sum -c sacdfs_0.6.5-1_amd64.deb.sha256
   ```

   *You should see* `sacdfs_0.6.5-1_amd64.deb: OK`. With the GitHub CLI,
   `gh attestation verify sacdfs_0.6.5-1_amd64.deb -R aclater/sacdfs` also checks where it was
   built.

2. **Install it:**

   ```sh
   sudo apt install ./sacdfs_0.6.5-1_amd64.deb
   ```

3. **Name your folders** in `/etc/default/sacdfs` (`sudoedit /etc/default/sacdfs`), as in
   step 3 for Fedora above.

4. **Start sacdfs, and start it at each boot:**

   ```sh
   sudo systemctl enable --now sacdfs
   ls /srv/sacd
   ```

To remove it: `sudo apt remove sacdfs` keeps `/etc/default/sacdfs`; `sudo apt purge sacdfs`
removes it too.

## Updating

Download the newer release's package and install it the same way (`sudo dnf install ./FILE` or
`sudo apt install ./FILE`, with the new file name). Your settings file is kept, and a running
sacdfs restarts on the new program.

## Details

- **Files:** `/usr/bin/sacdfs`, `/usr/bin/sacdfs-ctl`, `/usr/lib/systemd/system/sacdfs.service`,
  the settings (`/etc/sysconfig/sacdfs` on Fedora and RHEL, `/etc/default/sacdfs` on Debian and
  Ubuntu; the unit and `sacdfs-ctl` both read it), and the README, changelog and licence. The
  service keeps its seek index cache in `/var/cache/sacdfs`, which removal (`dnf remove`,
  `apt purge`) deletes. The program is the release archive's, byte for byte.
- **The service** is not enabled or started when the package is installed. Removing the
  package stops and disables it. Options for sacdfs go in `SACDFS_OPTIONS` in the settings file
  ([options](../options.md)).
- **Next to an install.sh setup:** the installer's systemd route uses `/usr/local/bin/sacdfs`,
  `/etc/systemd/system/sacdfs.service` and `/etc/default/sacdfs`. The package warns when it
  finds them. `/etc/systemd/system/sacdfs.service` takes precedence over the package's unit:
  remove it and run `sudo systemctl daemon-reload` to use the package's.
- **Checked** with the 0.6.5 packages built by the release workflow from the 0.6.5 archives:
  install, verify, upgrade from a 0.6.4 package and removal in containers of UBI 9, UBI 10,
  Fedora 44 and 45, Debian 12 and 13, Ubuntu 22.04 and 24.04 (x86_64 and aarch64); and the
  steps above, the service at boot, an upgrade while it runs and removal under systemd on
  RHEL 10 (10.3 packages, SELinux enforcing) and Debian 13, in VMs. The download and
  `gh attestation verify` commands were not run against the release before it carried the
  packages.
