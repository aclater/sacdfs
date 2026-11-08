// SPDX-License-Identifier: GPL-3.0-or-later
//! Plain-language checks before mounting (#282).
//!
//! Most people who start sacdfs by hand do it on a NAS, over SSH, and are not Linux users. The
//! first things that go wrong there (FUSE not loaded, the folder to mount on missing, no mount
//! helper, a container without the right to mount) used to end in messages that point the
//! wrong way, such as `cannot mount on /volume1/sacd: No such file or directory` when it is
//! `/dev/fuse` that is missing. The checks here run before the library is scanned and before
//! anything is mounted, and each failure is one line that says what is wrong and what to type.
//!
//! The decisions are pure functions of [`Facts`], so that they are tested without root, a
//! missing `/dev/fuse` or a container; [`Facts::gather`] reads the facts from the system.

use std::path::{Path, PathBuf};

/// `CAP_SYS_ADMIN`'s bit in the capability sets: what mount(2) needs.
const CAP_SYS_ADMIN: u32 = 21;

/// What the checks look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The effective user id.
    pub euid: u32,
    /// Opening `/dev/fuse` for reading and writing: `Ok`, or the errno it failed with.
    pub dev_fuse: Result<(), i32>,
    /// Whether this process may call mount(2) itself (`CAP_SYS_ADMIN` in its effective set);
    /// `None` if `/proc/self/status` could not be read.
    pub may_mount: Option<bool>,
    /// Whether a setuid helper could gain that right (`CAP_SYS_ADMIN` in the bounding set):
    /// not in a container started without `--cap-add SYS_ADMIN`.
    pub helper_may_mount: Option<bool>,
    /// The FUSE mount helper found, by name (`fusermount3`, `fusermount`) or as
    /// `FUSERMOUNT_PATH` gives it.
    pub helper: Option<String>,
}

impl Facts {
    /// The facts of this process and system.
    pub fn gather() -> Facts {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        Facts {
            euid: euid(),
            dev_fuse: open_dev_fuse(),
            may_mount: has_cap(&status, "CapEff:", CAP_SYS_ADMIN),
            helper_may_mount: has_cap(&status, "CapBnd:", CAP_SYS_ADMIN),
            helper: find_helper(),
        }
    }
}

#[allow(unsafe_code)]
fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Opens and closes `/dev/fuse`, as fuser does first when it mounts.
fn open_dev_fuse() -> Result<(), i32> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
        .map(drop)
        .map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))
}

/// Whether bit `cap` is set in the capability set on the line starting with `key` (`CapEff:`)
/// of a `/proc/PID/status`.
pub fn has_cap(status: &str, key: &str, cap: u32) -> Option<bool> {
    let hex = status.lines().find_map(|l| l.strip_prefix(key))?.trim();
    u64::from_str_radix(hex, 16).ok().map(|set| set >> cap & 1 == 1)
}

/// Where the mount helpers are looked for besides `PATH`, as fuser does (some systems keep
/// them in /sbin, out of a login's `PATH`).
const HELPER_DIRS: &[&str] = &["/sbin", "/bin", "/usr/sbin", "/usr/bin", "/usr/local/sbin", "/usr/local/bin"];

/// The FUSE mount helper fuser would mount with: `FUSERMOUNT_PATH` if set, else the first of
/// `fusermount3` and `fusermount` found.
pub fn find_helper() -> Option<String> {
    if let Some(p) = std::env::var_os("FUSERMOUNT_PATH").filter(|p| !p.is_empty()) {
        return Some(p.to_string_lossy().into_owned());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> =
        std::env::split_paths(&path).chain(HELPER_DIRS.iter().map(PathBuf::from)).collect();
    ["fusermount3", "fusermount"]
        .into_iter()
        .find(|name| dirs.iter().any(|d| is_executable(&d.join(name))))
        .map(str::to_owned)
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The check before mounting: `Err` holds the one line to print.
pub fn check(f: &Facts) -> Result<(), String> {
    match f.dev_fuse {
        Ok(()) => {}
        // No device node, or a node without the driver behind it.
        Err(libc::ENOENT | libc::ENODEV | libc::ENXIO) => {
            return Err("FUSE is not loaded: /dev/fuse does not exist. Load it as root with \
                        `modprobe fuse`, then start sacdfs again (in a container: add \
                        `--device /dev/fuse`)."
                .to_owned());
        }
        Err(libc::EACCES | libc::EPERM) if f.euid == 0 => {
            return Err("/dev/fuse exists but may not be opened here. In a container, add \
                        `--device /dev/fuse`; elsewhere, check the device's permissions with \
                        `ls -l /dev/fuse`."
                .to_owned());
        }
        Err(libc::EACCES | libc::EPERM) => {
            return Err("this user may not use FUSE: /dev/fuse can't be opened. Run sacdfs as \
                        root, or let this user open /dev/fuse (on some systems, by adding it \
                        to the `fuse` group)."
                .to_owned());
        }
        // Anything else: the mount itself will say.
        Err(_) => {}
    }
    if f.may_mount == Some(false) {
        if f.helper_may_mount == Some(false) {
            return Err("no right to mount here: CAP_SYS_ADMIN is missing, and no mount helper can \
                        gain it. In a container, add `--cap-add SYS_ADMIN` (Compose: \
                        `cap_add: [SYS_ADMIN]`)."
                .to_owned());
        }
        if f.helper.is_none() {
            let why = if f.euid == 0 {
                "sacdfs runs as root but without the right to mount (CAP_SYS_ADMIN), and FUSE's \
                 mount helper, fusermount3, isn't installed"
            } else {
                "a user other than root needs FUSE's mount helper, fusermount3, and it isn't \
                 installed"
            };
            return Err(format!(
                "{why}. Run sacdfs as root, or install the `fuse3` package (`fuse` on older \
                 systems)."
            ));
        }
    }
    Ok(())
}

/// The command that unmounts `at` here: `umount` for root (always there, and all a NAS
/// without FUSE 3 has), else the mount helper found, else `umount` as root.
pub fn unmount_command(euid: u32, helper: Option<&str>, at: &Path) -> String {
    let at = at.display();
    match helper {
        _ if euid == 0 => format!("umount {at}"),
        Some(h) => format!("{h} -u {at}"),
        None => format!("umount {at}, as root"),
    }
}

/// [`unmount_command`] for this process and system.
pub fn unmount_hint(at: &Path) -> String {
    unmount_command(euid(), find_helper().as_deref(), at)
}

/// Whether other users may read the mount (`allow_other`): asked for with `--allow-other`,
/// and by default when sacdfs runs as root, unless `--no-allow-other` says not to (#282).
/// For other users nothing changes: off unless asked for, as FUSE allows it to them only
/// with `user_allow_other` in /etc/fuse.conf.
pub fn allow_other(flag: bool, no_flag: bool, euid: u32) -> bool {
    !no_flag && (flag || euid == 0)
}

/// The message for a mount that missing folder `path` can't be made on.
pub fn missing_mountpoint(path: &Path) -> String {
    format!(
        "the folder to mount on, {p}, does not exist. Create it with `mkdir -p {p}`, then start \
         sacdfs again.",
        p = path.display()
    )
}

/// Which mandatory access control confines this process, from its security label
/// (`/proc/self/attr/current`, or `/proc/self/attr/apparmor/current` on newer kernels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confinement {
    /// An AppArmor profile in enforce mode (`docker-default (enforce)`).
    AppArmor,
    /// An SELinux domain (`system_u:system_r:container_t:s0:c1,c2`).
    SeLinux,
    /// Unconfined, or not known.
    None,
}

/// Reads [`Confinement`] from a label.
pub fn confinement(label: &str) -> Confinement {
    let label = label.trim_end_matches(['\0', '\n']).trim();
    if label.ends_with("(enforce)") {
        Confinement::AppArmor
    } else if label.split(':').count() >= 3 && !label.contains("unconfined_t") {
        Confinement::SeLinux
    } else {
        Confinement::None
    }
}

/// This process's [`Confinement`].
pub fn own_confinement() -> Confinement {
    ["/proc/self/attr/apparmor/current", "/proc/self/attr/current"]
        .iter()
        .find_map(|p| {
            std::fs::read_to_string(p).ok().filter(|s| !s.trim_end_matches(['\0', '\n']).is_empty())
        })
        .map_or(Confinement::None, |l| confinement(&l))
}

/// A plain line for a mount that failed with `error` (fuser's message), if it is one of the
/// failures met on NAS systems; `None` keeps the error as it is.
pub fn explain_mount_error(error: &str, euid: u32, conf: Confinement) -> Option<String> {
    if error.contains("user_allow_other") {
        return Some(
            "--allow-other, for a user other than root, needs the line `user_allow_other` in \
             /etc/fuse.conf. Add it as root (`echo user_allow_other >> /etc/fuse.conf`), or \
             start sacdfs without --allow-other."
                .to_owned(),
        );
    }
    // mount(2) refused with EACCES, which root with CAP_SYS_ADMIN gets only from a security
    // module: Docker's default AppArmor profile, for one.
    if euid == 0 && error.contains("EACCES") {
        let fix = match conf {
            Confinement::AppArmor => "AppArmor. In a container, add `--security-opt apparmor=unconfined`",
            Confinement::SeLinux => "SELinux. In a container, add `--security-opt label=disable`",
            Confinement::None => {
                "AppArmor or SELinux. In a container, add `--security-opt apparmor=unconfined` \
                 (AppArmor) or `--security-opt label=disable` (SELinux)"
            }
        };
        return Some(format!("the system's security policy refused the mount: {fix}."));
    }
    // A mount helper that exited without a word.
    if error.trim().is_empty() {
        return Some("FUSE's mount helper failed without saying why; try as root.".to_owned());
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn root() -> Facts {
        Facts { euid: 0, dev_fuse: Ok(()), may_mount: Some(true), helper_may_mount: Some(true), helper: None }
    }

    fn user() -> Facts {
        Facts { euid: 1000, may_mount: Some(false), helper: Some("fusermount3".into()), ..root() }
    }

    #[test]
    fn a_working_system_passes() {
        assert_eq!(check(&root()), Ok(()));
        assert_eq!(check(&user()), Ok(()));
        // FUSE 2's helper is enough (Synology DSM).
        assert_eq!(check(&Facts { helper: Some("fusermount".into()), ..user() }), Ok(()));
        // Facts that could not be read pass: the mount itself will say.
        assert_eq!(check(&Facts { may_mount: None, helper_may_mount: None, ..user() }), Ok(()));
        assert_eq!(check(&Facts { dev_fuse: Err(libc::EIO), ..root() }), Ok(()));
    }

    #[test]
    fn no_dev_fuse_says_modprobe() {
        for errno in [libc::ENOENT, libc::ENODEV, libc::ENXIO] {
            for f in [root(), user()] {
                let e = check(&Facts { dev_fuse: Err(errno), ..f }).unwrap_err();
                assert!(e.contains("`modprobe fuse`") && e.contains("/dev/fuse does not exist"), "{e}");
                assert!(!e.contains("No such file or directory"));
            }
        }
    }

    #[test]
    fn dev_fuse_not_allowed() {
        let e = check(&Facts { dev_fuse: Err(libc::EPERM), ..root() }).unwrap_err();
        assert!(e.contains("--device /dev/fuse"), "{e}");
        let e = check(&Facts { dev_fuse: Err(libc::EACCES), ..user() }).unwrap_err();
        assert!(e.contains("Run sacdfs as root"), "{e}");
    }

    #[test]
    fn no_helper_for_a_user() {
        let e = check(&Facts { helper: None, ..user() }).unwrap_err();
        assert!(e.contains("other than root") && e.contains("fuse3"), "{e}");
        // Root needs none.
        assert_eq!(check(&root()), Ok(()));
        // Root without the right to mount does (`capsh --drop`).
        let e = check(&Facts { may_mount: Some(false), ..root() }).unwrap_err();
        assert!(e.contains("runs as root but without the right to mount"), "{e}");
    }

    #[test]
    fn a_container_without_sys_admin() {
        for f in [root(), user()] {
            let e = check(&Facts { may_mount: Some(false), helper_may_mount: Some(false), ..f }).unwrap_err();
            assert!(e.contains("--cap-add SYS_ADMIN"), "{e}");
        }
    }

    #[test]
    fn capabilities_from_proc_status() {
        let s = "Name:\tx\nCapInh:\t0000000000000000\nCapEff:\t00000000a80425fb\nCapBnd:\t000001ffffffffff\n";
        // Docker's default set: no CAP_SYS_ADMIN (bit 21), which --cap-add adds (0x200000).
        assert_eq!(has_cap(s, "CapEff:", CAP_SYS_ADMIN), Some(false));
        assert_eq!(has_cap(s, "CapBnd:", CAP_SYS_ADMIN), Some(true));
        assert_eq!(has_cap("CapEff:\t00000000a82425fb\n", "CapEff:", CAP_SYS_ADMIN), Some(true));
        assert_eq!(has_cap("", "CapEff:", CAP_SYS_ADMIN), None);
        assert_eq!(has_cap("CapEff:\tzz\n", "CapEff:", CAP_SYS_ADMIN), None);
    }

    #[test]
    fn unmount_names_a_command_that_exists() {
        let at = Path::new("/volume1/sacd");
        // Root: umount, whatever helper there is (ADM, DSM and QuTScloud have no fusermount3).
        assert_eq!(unmount_command(0, None, at), "umount /volume1/sacd");
        assert_eq!(unmount_command(0, Some("fusermount3"), at), "umount /volume1/sacd");
        assert_eq!(unmount_command(1000, Some("fusermount3"), at), "fusermount3 -u /volume1/sacd");
        assert_eq!(unmount_command(1000, Some("fusermount"), at), "fusermount -u /volume1/sacd");
        assert_eq!(unmount_command(1000, None, at), "umount /volume1/sacd, as root");
    }

    #[test]
    fn allow_other_defaults_on_for_root_only() {
        // Root: on, unless turned off.
        assert!(allow_other(false, false, 0));
        assert!(allow_other(true, false, 0));
        assert!(!allow_other(false, true, 0));
        // Other users: unchanged, off unless asked for.
        assert!(!allow_other(false, false, 1000));
        assert!(allow_other(true, false, 1000));
        assert!(!allow_other(false, true, 1000));
    }

    #[test]
    fn missing_mountpoint_says_mkdir() {
        let m = missing_mountpoint(Path::new("/volume1/sacd"));
        assert!(m.contains("`mkdir -p /volume1/sacd`"), "{m}");
    }

    #[test]
    fn labels() {
        assert_eq!(confinement("docker-default (enforce)\n"), Confinement::AppArmor);
        assert_eq!(confinement("unconfined\n"), Confinement::None);
        assert_eq!(confinement("foo (complain)"), Confinement::None);
        assert_eq!(confinement("system_u:system_r:container_t:s0:c1,c2\0"), Confinement::SeLinux);
        assert_eq!(confinement("unconfined_u:unconfined_r:unconfined_t:s0-s0:c0.c1023"), Confinement::None);
        assert_eq!(confinement(""), Confinement::None);
    }

    #[test]
    fn mount_errors_explained() {
        let fusermount =
            "fusermount3: option allow_other only allowed if 'user_allow_other' is set in /etc/fuse.conf\n";
        let e = explain_mount_error(fusermount, 1000, Confinement::None).unwrap();
        assert!(e.contains("user_allow_other") && e.contains("/etc/fuse.conf"), "{e}");
        let apparmor = "Error calling mount() at \"/srv/sacd\" with \"fd=3,rootmode=40755,user_id=0,group_id=0\" \
                        and flags=MS_NOSUID | MS_NODEV: EACCES: Permission denied";
        let e = explain_mount_error(apparmor, 0, Confinement::AppArmor).unwrap();
        assert!(e.contains("apparmor=unconfined") && !e.contains("label=disable"), "{e}");
        let e = explain_mount_error(apparmor, 0, Confinement::SeLinux).unwrap();
        assert!(e.contains("label=disable") && !e.contains("apparmor"), "{e}");
        let e = explain_mount_error(apparmor, 0, Confinement::None).unwrap();
        assert!(e.contains("label=disable") && e.contains("apparmor=unconfined"), "{e}");
        assert_eq!(explain_mount_error("something else", 0, Confinement::AppArmor), None);
        assert!(explain_mount_error("\n", 1000, Confinement::None).unwrap().contains("without saying why"));
    }
}
