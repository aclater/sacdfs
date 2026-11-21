# Linux packages: .rpm and .deb

Each release from 0.6.5 has sacdfs as `.rpm` and `.deb` packages, for x86_64 and aarch64, and
Fedora and RHEL can also get it from a COPR repository that dnf keeps up to date
([COPR](#fedora-and-rhel-copr-updates-through-dnf)). A package installs `/usr/bin/sacdfs`, with
`sacdfs-ctl`, a systemd unit and a settings file, and needs FUSE 3 (package `fuse3`, which the
package manager installs with it). About 5 minutes.

| System | Package |
|---|---|
| Fedora 44 and 45, RHEL 9 and 10 | `sacdfs-0.7.1-1.x86_64.rpm`, `sacdfs-0.7.1-1.aarch64.rpm` |
| Debian 12 and 13, Ubuntu 22.04 and 24.04 | `sacdfs_0.7.1-1_amd64.deb`, `sacdfs_0.7.1-1_arm64.deb` |

The packages are not signed with a package key. Check each download against its `.sha256`
file, and, on a computer with the GitHub CLI, against its attestation, which shows that
GitHub's release workflow built it.

The commands below are for x86_64; on aarch64 (`uname -m` prints `aarch64`) use the aarch64
(rpm) or arm64 (deb) file instead. See [Details](#details) for what was checked where.

## Fedora and RHEL

1. **Download the package and its checksum, and check it:**

   ```sh
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.7.1/sacdfs-0.7.1-1.x86_64.rpm
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.7.1/sacdfs-0.7.1-1.x86_64.rpm.sha256
   sha256sum -c sacdfs-0.7.1-1.x86_64.rpm.sha256
   ```

   *You should see* `sacdfs-0.7.1-1.x86_64.rpm: OK`. With the GitHub CLI,
   `gh attestation verify sacdfs-0.7.1-1.x86_64.rpm -R aclater/sacdfs` also checks where it
   was built.

2. **Install it:**

   ```sh
   sudo dnf install ./sacdfs-0.7.1-1.x86_64.rpm
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

## Fedora and RHEL: COPR (updates through dnf)

The same packages, built from the release's source by Fedora's COPR service, come from a dnf
repository, so `dnf upgrade` brings each new release. The repository's packages are signed by
COPR, and dnf checks the signature.

Fedora 44 and 45:

```sh
sudo dnf copr enable aclater/sacdfs
sudo dnf install sacdfs
```

RHEL 9 (on RHEL 10, put `10` for each `9`):

```sh
sudo curl -fsSL -o /etc/yum.repos.d/aclater-sacdfs.repo https://copr.fedorainfracloud.org/coprs/aclater/sacdfs/repo/rhel-9/aclater-sacdfs-rhel-9.repo
sudo dnf install sacdfs
```

*You should see* `Complete!`; `sacdfs --version` then prints `sacdfs 0.7.1`. On RHEL,
`sudo dnf copr enable aclater/sacdfs` alone looks for an EPEL build, which this project
doesn't have; with `dnf-plugins-core` installed, `sudo dnf copr enable aclater/sacdfs
rhel-9-x86_64` works too. Then go on with step 3 above (settings, then start).

A COPR build sorts newer than the release's `.rpm` of the same version (`0.7.1-1.el9` after
`0.7.1-1`), so once the repository is enabled, `dnf upgrade` replaces a downloaded package with
it. The files are laid out the same; the program is built from the release's source by COPR
instead of being the release archive's.

## Debian and Ubuntu

1. **Download the package and its checksum, and check it:**

   ```sh
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.7.1/sacdfs_0.7.1-1_amd64.deb
   curl -fLO https://github.com/aclater/sacdfs/releases/download/v0.7.1/sacdfs_0.7.1-1_amd64.deb.sha256
   sha256sum -c sacdfs_0.7.1-1_amd64.deb.sha256
   ```

   *You should see* `sacdfs_0.7.1-1_amd64.deb: OK`. With the GitHub CLI,
   `gh attestation verify sacdfs_0.7.1-1_amd64.deb -R aclater/sacdfs` also checks where it was
   built.

2. **Install it:**

   ```sh
   sudo apt install ./sacdfs_0.7.1-1_amd64.deb
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
  `apt purge`) deletes. In the release's `.rpm` and `.deb` files the program is the release
  archive's, byte for byte.
- **The service** is not enabled or started when the package is installed. Removing the
  package stops and disables it. Options for sacdfs go in `SACDFS_OPTIONS` in the settings file
  ([options](../options.md)).
- **Next to an install.sh setup:** the installer's systemd route uses `/usr/local/bin/sacdfs`,
  `/etc/systemd/system/sacdfs.service` and `/etc/default/sacdfs`. The package warns when it
  finds them. `/etc/systemd/system/sacdfs.service` takes precedence over the package's unit:
  remove it and run `sudo systemctl daemon-reload` to use the package's.
  The installer's `--uninstall` of that route also deletes `/etc/default/sacdfs` (from reading
  install.sh; not run with the package), which on Debian and Ubuntu is the package's settings
  file: uninstall an installer setup before installing the package, not after.
- **Checked** with the 0.6.5 packages built by the release workflow from the 0.6.5 archives:
  install, verify, upgrade from a 0.6.4 package and removal in containers of UBI 9, UBI 10,
  Fedora 44 and 45, Debian 12 and 13, Ubuntu 22.04 and 24.04 (x86_64 and aarch64); and the
  steps above, the service at boot, an upgrade while it runs and removal under systemd on
  RHEL 10 (10.3 packages, SELinux enforcing) and Debian 13, in VMs. The download, check and
  install steps were run against the published 0.6.5 files on UBI 10 and Debian 13 (x86_64),
  and `gh attestation verify` on all five. The COPR steps were run (as root, x86_64) in
  containers: `dnf copr enable` on Fedora 44 and 45; the `.repo` file and the
  `rhel-N-x86_64` form on UBI 9 and UBI 10, standing in for RHEL; and a downloaded `.rpm`
  replaced by the COPR build on `dnf upgrade` (UBI 10). Not run on aarch64.
