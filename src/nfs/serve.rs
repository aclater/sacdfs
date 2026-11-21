// SPDX-License-Identifier: GPL-3.0-or-later

//! Running the NFS server, and mounting it with the system's NFS client (#420).
//!
//! [`run`] listens, mounts if asked ([`Mount`]), serves until SIGINT, SIGTERM, SIGHUP or
//! SIGQUIT, then unmounts and stops. On macOS the mount is the user's own: `mount_nfs` needs
//! no root for a folder the user owns. On Linux, mounting NFS needs root.
//!
//! **A mount left behind** (the server killed with SIGKILL, or an unmount that failed) points
//! at a port nothing answers on: reads fail after the `soft` timeout on Linux, and on macOS
//! (`hard,deadtimeout=60`) after about a minute, when macOS unmounts the folder itself.
//! Whoever listens on that port before then answers for the mount. So the mount is always
//! `nosuid,nodev,noexec`, whatever `-o` says, and root's default port is below 1024, where no
//! other user can listen.

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
/// server is found again by the mount of the one before. Users whose IDs differ by a multiple
/// of 10,000 share a port: the second to start is told to choose one with `--port`.
pub fn default_port(uid: u32) -> u16 {
    // `uid % 10_000` is below 10,000: it fits in a u16, and the sum stays below 30,000.
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
/// - `hard,intr,deadtimeout=60`: a read waits while the server restarts, so a restart is not
///   seen; a server gone for good fails reads after about a minute, and macOS then unmounts
///   the folder itself. (`soft` failed every read after 6 to 8 s on macOS 27.2, whatever
///   `timeo` and `retrans` said, so a restart broke open files.) `intr` lets Control-C stop a
///   waiting program;
/// - `rsize=1048576`: reads as large as sacdfs answers ([`crate::vfs::MAX_READ`]);
/// - `rdirplus`: listings with attributes (READDIRPLUS), one call per page instead of one
///   per entry.
///
/// Linux:
/// - `mountproto=tcp`: the MOUNT call goes over TCP too, to the same port;
/// - `nolock`: no lock manager is asked (the server has none); locks stay on the client;
/// - `soft,timeo=10,retrans=30`: a call that gets no answer is sent again, first after 1 s
///   (`timeo` counts tenths of a second), and fails after 30 tries (`retrans`), so a read
///   waits while the server restarts but not for good for one that is gone. How long the
///   30 tries take in all has not been measured.
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
            "hard".into(),
            "intr".into(),
            "deadtimeout=60".into(),
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

/// Why [`unmount`] did not unmount.
#[derive(Debug)]
enum UnmountError {
    /// The path no longer names the mount made: nothing was tried.
    NotOurs(io::Error),
    /// The plain and the forced unmount both failed: the mount is still there.
    Failed(io::Error),
}

/// Unmounts `m`: a plain unmount (a few tries: a file closed just now keeps the mount busy for
/// a moment), then a forced one. An error, and nothing unmounted, if the path is no longer
/// this mount.
fn unmount(m: Mounted<'_>) -> Result<(), UnmountError> {
    let dev = std::fs::metadata(m.at).map(|x| x.dev());
    if dev.as_ref().ok() != Some(&m.dev) {
        return Err(UnmountError::NotOurs(io::Error::other(format!(
            "{} is no longer this mount: not unmounting it",
            m.at.display()
        ))));
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
    run_command(c, "umount -f", UMOUNT_TIMEOUT).map_err(UnmountError::Failed)
}

/// [`unmount`], with its outcome logged. Only a mount that is still there is said to need
/// unmounting by hand.
fn unmount_and_log(m: Mounted<'_>) -> io::Result<()> {
    match unmount(m) {
        Ok(()) => {
            log::info!("unmounted {}", m.at.display());
            Ok(())
        }
        Err(UnmountError::Failed(e)) => {
            log::error!("{e}: unmount {} by hand", m.at.display());
            Err(e)
        }
        Err(UnmountError::NotOurs(e)) => {
            log::error!("{e}");
            Err(e)
        }
    }
}

/// The runtime's threads. They decode calls and frame replies; reads are done by the core's
/// own threads, and a call waiting for the core holds none of these (`ask` in the parent module).
const RUNTIME_THREADS: usize = 4;

/// How long the runtime may take to end its tasks once the server has stopped.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);

/// Runs `f` on the runtime's blocking pool: for the system commands, which wait.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(io::Error::other).and_then(|r| r)
}

/// The signals that stop the server.
struct StopSignals {
    term: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
    quit: tokio::signal::unix::Signal,
}

impl StopSignals {
    fn new() -> io::Result<StopSignals> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(StopSignals {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
            hup: signal(SignalKind::hangup())?,
            quit: signal(SignalKind::quit())?,
        })
    }

    /// Waits for the first of them, and logs which it was.
    async fn recv(&mut self) {
        let name = tokio::select! {
            _ = self.term.recv() => "SIGTERM",
            _ = self.int.recv() => "SIGINT",
            _ = self.hup.recv() => "SIGHUP",
            _ = self.quit.recv() => "SIGQUIT",
        };
        log::info!("{name}: stopping");
    }
}

/// Listens on `addr`, with a hint in the error if the port is taken.
async fn listen(addr: SocketAddr, front: NfsFront) -> io::Result<NFSTcpListener<NfsFront>> {
    NFSTcpListener::bind(addr, front).await.map_err(|e| {
        let hint = if e.kind() == io::ErrorKind::AddrInUse {
            " (another sacdfs-nfs, or another program, has it: choose another with --port)"
        } else {
            ""
        };
        io::Error::new(e.kind(), format!("cannot listen on {addr}: {e}{hint}"))
    })
}

/// Mounts the server listening on `bound` as `m` says. The mount's device number, to
/// recognise it when unmounting; `None` if it cannot be read (the mount is then left at the
/// end).
async fn mount_now(m: &Mount, bound: SocketAddr) -> io::Result<Option<u64>> {
    let c = mount_command(bound, &m.at, &m.extra)?;
    log::debug!("mounting: {c:?}");
    blocking(move || run_command(c, "mount", MOUNT_TIMEOUT))
        .await
        .map_err(|e| io::Error::new(e.kind(), format!("cannot mount on {}: {e}", m.at.display())))?;
    let dev = match std::fs::metadata(&m.at).map(|x| x.dev()) {
        Ok(dev) => Some(dev),
        Err(e) => {
            log::warn!("{}: {e}: it will not be unmounted at the end", m.at.display());
            None
        }
    };
    log::info!("mounted on {}", m.at.display());
    Ok(dev)
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
        .worker_threads(RUNTIME_THREADS)
        .thread_name("sacdfs-nfs")
        .enable_all()
        .build()?;
    let closer = front.clone();
    let result = runtime.block_on(async move {
        // Before listening: a signal from then on stops the server cleanly.
        let mut stop = StopSignals::new()?;
        let listener = listen(addr, front.clone()).await?;
        let bound = listener.local_addr()?;
        if !bound.ip().is_loopback() {
            log::warn!(
                "listening on {bound}, not on loopback only: anyone who can reach it can read the files"
            );
        }
        let idle = front.clone();
        tokio::spawn(async move {
            loop {
                // Often enough that a file is closed within a quarter over its idle time.
                tokio::time::sleep(super::IDLE / 4).await;
                idle.close_idle();
            }
        });
        let mut server = tokio::spawn(async move { listener.handle_forever().await });
        let mut mounted_dev = None;
        if let Some(m) = &mount {
            match mount_now(m, bound).await {
                Ok(dev) => mounted_dev = dev,
                Err(e) => {
                    server.abort();
                    return Err(e);
                }
            }
        }
        ready(bound);
        let served: io::Result<()> = tokio::select! {
            () = stop.recv() => Ok(()),
            r = &mut server => Err(match r {
                Ok(Err(e)) => e,
                Ok(Ok(())) => io::Error::other("the server stopped"),
                Err(e) => io::Error::other(e),
            }),
        };
        // Every way out unmounts what was mounted. After a signal the server still answers
        // while it does; if the server itself ended, nothing answers the client's last calls
        // and the unmount may need its forced step.
        let unmounted = match (&mount, mounted_dev) {
            (Some(m), Some(dev)) => {
                let at = m.at.clone();
                blocking(move || unmount_and_log(Mounted { at: &at, dev })).await
            }
            _ => Ok(()),
        };
        server.abort();
        served.and(unmounted)
    });
    closer.close_all();
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
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
