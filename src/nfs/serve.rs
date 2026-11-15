// SPDX-License-Identifier: GPL-3.0-or-later

//! Running the NFS server, and mounting it with the system's NFS client (#420).
//!
//! [`run`] listens, mounts if asked ([`Mount`]), serves until SIGINT, SIGTERM, SIGHUP or
//! SIGQUIT, then unmounts and stops. On macOS the mount is the user's own: `mount_nfs` needs
//! no root for a folder the user owns. On Linux, mounting NFS needs root.
//!
//! **A mount left behind** (the server killed with SIGKILL, or an unmount that failed) points
//! at a port nothing answers on: reads fail after the `soft` timeout. Whoever then listens on
//! that port answers for the mount. So the mount is always `nosuid,nodev,noexec`, whatever
//! `-o` says, and root's default port is below 1024, where no other user can listen.

use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::NfsFront;
use super::proto::tcp::NFSTcpListener;

/// The first port of [`default_port`]'s range for users other than root: below the systems'
/// ephemeral ranges (macOS from 49152, Linux from 32768), so that a port the system handed
/// out never takes it.
pub const PORT_BASE: u16 = 20000;

/// Root's default port: below 1024, so that no other user can listen on it if the server
/// stops with the mount still there.
pub const ROOT_PORT: u16 = 1007;

/// The default port of user `uid`: one per user, so that two users of one Mac each get their
/// own server without choosing a port, and the same one on every run, so that a restarted
/// server is found again by the mount of the one before.
pub fn default_port(uid: u32) -> u16 {
    if uid == 0 { ROOT_PORT } else { PORT_BASE + (uid % 10_000) as u16 }
}

/// The default address: loopback only.
pub const DEFAULT_LISTEN: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// How long `mount` and each `umount` may take before they are stopped.
const MOUNT_TIMEOUT: Duration = Duration::from_secs(60);
const UMOUNT_TIMEOUT: Duration = Duration::from_secs(10);

/// A mount to make once the server listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// The folder to mount on, canonical.
    pub at: PathBuf,
    /// Options added to the defaults ([`mount_options`]).
    pub extra: Vec<String>,
}

/// Options `-o` may not give: they would undo the read-only, `nosuid`, `nodev` or `noexec`
/// mount.
const REFUSED: [&str; 4] = ["rw", "suid", "dev", "exec"];

/// The NFS client's options for a mount of the server on `port`, with the user's `extra`:
/// read-only, NFSv3 over TCP, both services on the one port (no port mapper or lock manager
/// is asked), and `nosuid,nodev,noexec` last, so that they win.
///
/// macOS (see docs/design.md, "The NFS front end", for each choice):
/// - `nfc`: names looked up in NFD (as the Finder and most Mac software type them) find the
///   NFC names sacdfs gives;
/// - `locallocks`: locks are kept by the client, so software that locks a file it reads
///   still can (`nolocks` fails those locks);
/// - `soft,intr` with `timeo` and `retrans`: a stopped server fails a read with an error
///   instead of hanging it for good, and a short restart is not seen at all;
/// - `rsize=1048576`: reads as large as sacdfs answers ([`crate::vfs::MAX_READ`]);
/// - `rdirplus`: listings with attributes (READDIRPLUS), one call per page instead of one
///   per entry.
pub fn mount_options(port: u16, extra: &[String]) -> io::Result<Vec<String>> {
    if let Some(bad) = extra.iter().find(|e| REFUSED.contains(&e.trim().to_ascii_lowercase().as_str())) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("-o {bad}: the mount is always read-only, nosuid, nodev and noexec"),
        ));
    }
    let mut o: Vec<String> = if cfg!(target_os = "macos") {
        vec![
            "vers=3".into(),
            "tcp".into(),
            format!("port={port}"),
            format!("mountport={port}"),
            "nfc".into(),
            "locallocks".into(),
            "soft".into(),
            "intr".into(),
            "timeo=10".into(),
            "retrans=30".into(),
            "rsize=1048576".into(),
            "rdirplus".into(),
        ]
    } else {
        vec![
            "vers=3".into(),
            "proto=tcp".into(),
            format!("port={port}"),
            format!("mountport={port}"),
            "mountproto=tcp".into(),
            "nolock".into(),
            "soft".into(),
            "timeo=10".into(),
            "retrans=30".into(),
            "rsize=1048576".into(),
        ]
    };
    o.extend(extra.iter().cloned());
    o.push(if cfg!(target_os = "macos") { "rdonly" } else { "ro" }.into());
    o.extend(["nosuid", "nodev", "noexec"].map(String::from));
    Ok(o)
}

/// A system command by absolute path, with a fixed environment: no PATH or locale of the
/// caller's decides what runs or how it answers.
fn system_command(names: &[&str]) -> Command {
    let path = names.iter().find(|p| Path::new(p).exists()).unwrap_or(&names[0]);
    let mut c = Command::new(path);
    c.env_clear().env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").env("LC_ALL", "C");
    c
}

fn mount_program() -> Command {
    if cfg!(target_os = "macos") {
        system_command(&["/sbin/mount_nfs"])
    } else {
        system_command(&["/usr/bin/mount", "/bin/mount"])
    }
}

fn umount_program() -> Command {
    if cfg!(target_os = "macos") {
        system_command(&["/sbin/umount"])
    } else {
        system_command(&["/usr/bin/umount", "/bin/umount"])
    }
}

/// The command that mounts the server at `addr` on `at`.
pub fn mount_command(addr: SocketAddr, at: &Path, extra: &[String]) -> io::Result<Command> {
    let opts = mount_options(addr.port(), extra)?.join(",");
    let host = match addr.ip() {
        // The client asks the server's own address; a wildcard listener is reached on loopback.
        ip if ip.is_unspecified() => "127.0.0.1".to_owned(),
        IpAddr::V6(ip) => format!("[{ip}]"),
        ip => ip.to_string(),
    };
    let mut c = mount_program();
    if !cfg!(target_os = "macos") {
        c.args(["-t", "nfs"]);
    }
    c.arg("-o").arg(opts).arg(format!("{host}:/")).arg(at);
    Ok(c)
}

/// Runs `c`, stopping it after `timeout`, and turns a failure into an error that says what
/// failed.
fn run_command(mut c: Command, what: &str, timeout: Duration) -> io::Result<()> {
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child =
        c.spawn().map_err(|e| io::Error::new(e.kind(), format!("{what}: {:?}: {e}", c.get_program())))?;
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{what} did not finish in {} s", timeout.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if status.success() {
        return Ok(());
    }
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    Err(io::Error::other(format!("{what} failed ({status}): {}", err.trim())))
}

/// Refuses, for root, a mount point that another user could swap for another folder: one with
/// a folder above it that a user other than root can write, unless that folder is sticky
/// (`/tmp`) and the entry in it is root's.
pub fn check_mountpoint(at: &Path, uid: u32) -> io::Result<()> {
    if uid != 0 {
        return Ok(());
    }
    let mut child = at;
    while let Some(dir) = child.parent() {
        let m = std::fs::metadata(dir)?;
        let others_write = m.uid() != 0 || m.mode() & 0o022 != 0;
        if others_write {
            let sticky = m.mode() & 0o1000 != 0;
            let entry_roots = std::fs::symlink_metadata(child)?.uid() == 0;
            if !(sticky && entry_roots) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{}: another user can write {}, and so swap the mount point: as root, mount \
                         under folders only root can write",
                        at.display(),
                        dir.display()
                    ),
                ));
            }
        }
        child = dir;
    }
    Ok(())
}

/// A mount made: its folder and its device number, to tell it from whatever the path names
/// later.
#[derive(Debug, Clone, Copy)]
struct Mounted<'a> {
    at: &'a Path,
    dev: u64,
}

/// Unmounts `m`: a plain unmount (a few tries: a file closed just now keeps the mount busy for
/// a moment), then a forced one. Nothing if the path is no longer this mount.
fn unmount(m: Mounted<'_>) -> io::Result<()> {
    let dev = std::fs::metadata(m.at).map(|x| x.dev());
    if dev.as_ref().ok() != Some(&m.dev) {
        return Err(io::Error::other(format!(
            "{} is no longer this mount: not unmounting it",
            m.at.display()
        )));
    }
    let mut last = None;
    for i in 0..3 {
        if i > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        let mut c = umount_program();
        c.arg(m.at);
        match run_command(c, "umount", UMOUNT_TIMEOUT) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    if let Some(e) = last {
        log::warn!("{e}; unmounting by force");
    }
    let mut c = umount_program();
    c.arg("-f").arg(m.at);
    run_command(c, "umount -f", UMOUNT_TIMEOUT)
}

/// Serves `front` on `addr` until a stop signal, mounting it first if `mount` says so;
/// `ready` is told the address listened on (the port the system chose for port 0). Returns
/// when the server has stopped, after unmounting; an error if the unmount failed.
pub fn run(
    front: NfsFront,
    addr: SocketAddr,
    mount: Option<Mount>,
    ready: impl FnOnce(SocketAddr),
) -> io::Result<()> {
    if let Some(m) = &mount {
        // Before listening: a bad -o fails here.
        mount_options(addr.port(), &m.extra)?;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("sacdfs-nfs")
        .enable_all()
        .build()?;
    let closer = front.clone();
    let result = runtime.block_on(async move {
        use tokio::signal::unix::{SignalKind, signal};
        // Before listening: a signal from then on stops the server cleanly.
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        let mut hup = signal(SignalKind::hangup())?;
        let mut quit = signal(SignalKind::quit())?;
        let listener = NFSTcpListener::bind(addr, front.clone()).await.map_err(|e| {
            let hint = if e.kind() == io::ErrorKind::AddrInUse {
                " (another sacdfs-nfs, or another program, has it: choose another with --port)"
            } else {
                ""
            };
            io::Error::new(e.kind(), format!("cannot listen on {addr}: {e}{hint}"))
        })?;
        let bound = listener.local_addr()?;
        if !bound.ip().is_loopback() {
            log::warn!(
                "listening on {bound}, not on loopback only: anyone who can reach it can read the files"
            );
        }
        let idle = front.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(super::IDLE / 4).await;
                idle.close_idle();
            }
        });
        let mut server = tokio::spawn(async move { listener.handle_forever().await });
        let mut mounted = None;
        if let Some(m) = &mount {
            let c = mount_command(bound, &m.at, &m.extra)?;
            log::debug!("mounting: {c:?}");
            let made = tokio::task::spawn_blocking(move || run_command(c, "mount", MOUNT_TIMEOUT))
                .await
                .map_err(io::Error::other)
                .and_then(|r| r);
            if let Err(e) = made {
                server.abort();
                return Err(io::Error::new(e.kind(), format!("cannot mount on {}: {e}", m.at.display())));
            }
            let dev = std::fs::metadata(&m.at).map(|x| x.dev());
            match dev {
                Ok(dev) => mounted = Some(dev),
                Err(e) => log::warn!("{}: {e}: it will not be unmounted at the end", m.at.display()),
            }
            log::info!("mounted on {}", m.at.display());
        }
        ready(bound);
        let served: io::Result<()> = tokio::select! {
            _ = term.recv() => { log::info!("SIGTERM: stopping"); Ok(()) }
            _ = int.recv() => { log::info!("SIGINT: stopping"); Ok(()) }
            _ = hup.recv() => { log::info!("SIGHUP: stopping"); Ok(()) }
            _ = quit.recv() => { log::info!("SIGQUIT: stopping"); Ok(()) }
            r = &mut server => Err(match r {
                Ok(Err(e)) => e,
                Ok(Ok(())) => io::Error::other("the server stopped"),
                Err(e) => io::Error::other(e),
            }),
        };
        // Every way out unmounts what was mounted, while the server still answers.
        let unmounted = match (&mount, mounted) {
            (Some(m), Some(dev)) => {
                let at = m.at.clone();
                let r = tokio::task::spawn_blocking(move || unmount(Mounted { at: &at, dev }))
                    .await
                    .map_err(io::Error::other)
                    .and_then(|r| r);
                match &r {
                    Ok(()) => log::info!("unmounted {}", m.at.display()),
                    Err(e) => log::error!("{e}: unmount {} by hand", m.at.display()),
                }
                r
            }
            _ => Ok(()),
        };
        server.abort();
        served.and(unmounted)
    });
    closer.close_all();
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_per_user() {
        assert_eq!(default_port(501), 20501);
        assert_eq!(default_port(1000), 21000);
        assert_eq!(default_port(0), ROOT_PORT);
        const { assert!(ROOT_PORT < 1024) };
        assert!(default_port(u32::MAX) < 30000);
    }

    #[test]
    fn options_name_the_port_and_end_safe() {
        let o = mount_options(20501, &["nobrowse".to_owned()]).unwrap();
        assert!(o.contains(&"port=20501".to_owned()));
        assert!(o.contains(&"mountport=20501".to_owned()));
        assert!(o.contains(&"nobrowse".to_owned()));
        let n = o.len();
        assert_eq!(&o[n - 3..], ["nosuid", "nodev", "noexec"]);
        assert!(o[n - 4] == "rdonly" || o[n - 4] == "ro");
    }

    #[test]
    fn options_that_undo_the_safe_ones_are_refused() {
        for bad in ["rw", "suid", "dev", "exec", " SUID "] {
            assert!(mount_options(20501, &[bad.to_owned()]).is_err(), "{bad}");
        }
    }

    #[test]
    fn mount_command_names_the_export() {
        let c = mount_command("127.0.0.1:20501".parse().unwrap(), Path::new("/tmp/m"), &[]).unwrap();
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(&args[args.len() - 2..], ["127.0.0.1:/", "/tmp/m"]);
        assert!(Path::new(c.get_program()).is_absolute());
    }

    #[test]
    fn root_mount_points_under_others_folders_are_refused() {
        let d = tempfile::tempdir().unwrap();
        // Not root: no check.
        check_mountpoint(&d.path().join("m"), 1000).unwrap();
        // A folder that isn't root's, under /tmp (sticky): refused for root, unless the test
        // runs as root and owns it.
        let m = d.path().join("m");
        std::fs::create_dir(&m).unwrap();
        let mine = std::fs::metadata(d.path()).unwrap().uid();
        assert_eq!(check_mountpoint(&m, 0).is_err(), mine != 0);
    }
}
