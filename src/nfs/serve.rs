// SPDX-License-Identifier: GPL-3.0-or-later

//! Running the NFS server, and mounting it with the system's NFS client (#420).
//!
//! [`run`] listens, mounts if asked ([`Mount`]), serves until SIGINT, SIGTERM or SIGHUP, then
//! unmounts and stops. On macOS the mount is the user's own: `mount_nfs` needs no root for
//! a folder the user owns. On Linux, mounting NFS needs root.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::NfsFront;
use super::proto::tcp::NFSTcpListener;

/// The first port of [`default_port`]'s range: below the systems' ephemeral ranges (macOS
/// from 49152, Linux from 32768), so that a port the system handed out never takes it.
pub const PORT_BASE: u16 = 20000;

/// The default port of user `uid`: one per user, so that two users of one Mac each get their
/// own server without choosing a port, and the same one on every run, so that a restarted
/// server is found again by the mount of the one before.
pub fn default_port(uid: u32) -> u16 {
    PORT_BASE + (uid % 10_000) as u16
}

/// The default address: loopback only.
pub const DEFAULT_LISTEN: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// A mount to make once the server listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// The folder to mount on.
    pub at: PathBuf,
    /// Options added to the defaults ([`mount_options`]).
    pub extra: Vec<String>,
}

/// The NFS client's options for a mount of the server on `port`: read-only, NFSv3 over TCP,
/// both services on the one port (no port mapper or lock manager is asked).
///
/// macOS (see docs/design.md, "The NFS front end", for each choice):
/// - `nfc`: names looked up in NFD (as the Finder and most Mac software type them) find the
///   NFC names sacdfs gives;
/// - `locallocks`: locks are kept by the client, so software that locks a file it reads
///   still can (`nolocks` fails those locks);
/// - `soft,intr` with `timeo` and `retrans`: a stopped server fails a read with an error
///   after about 30 s instead of hanging it for good, and a restart within that time is not
///   seen at all;
/// - `rsize=1048576`: reads as large as sacdfs answers ([`crate::vfs::MAX_READ`]);
/// - `rdirplus`: listings with attributes (READDIRPLUS), one call per page instead of one
///   per entry.
pub fn mount_options(port: u16, extra: &[String]) -> Vec<String> {
    let mut o: Vec<String> = if cfg!(target_os = "macos") {
        vec![
            "vers=3".into(),
            "tcp".into(),
            format!("port={port}"),
            format!("mountport={port}"),
            "rdonly".into(),
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
            "ro".into(),
            "nolock".into(),
            "soft".into(),
            "timeo=10".into(),
            "retrans=30".into(),
            "rsize=1048576".into(),
        ]
    };
    o.extend(extra.iter().cloned());
    o
}

/// The command that mounts the server at `addr` on `at`.
pub fn mount_command(addr: SocketAddr, at: &Path, extra: &[String]) -> Command {
    let opts = mount_options(addr.port(), extra).join(",");
    let host = match addr.ip() {
        // The client asks the server's own address; a wildcard listener is reached on loopback.
        ip if ip.is_unspecified() => "127.0.0.1".to_owned(),
        IpAddr::V6(ip) => format!("[{ip}]"),
        ip => ip.to_string(),
    };
    let mut c = if cfg!(target_os = "macos") {
        let mut c = Command::new("/sbin/mount_nfs");
        c.arg("-o").arg(opts);
        c
    } else {
        let mut c = Command::new("mount");
        c.args(["-t", "nfs", "-o"]).arg(opts);
        c
    };
    c.arg(format!("{host}:/")).arg(at);
    c
}

/// Runs `c` and turns a failure into an error that says what failed.
fn run_command(mut c: Command, what: &str) -> io::Result<()> {
    let out =
        c.output().map_err(|e| io::Error::new(e.kind(), format!("{what}: {:?}: {e}", c.get_program())))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(io::Error::other(format!("{what} failed ({}): {}", out.status, err.trim())))
}

/// Unmounts `at`: a plain unmount, then a forced one (files still open, as `sacdfs` on Linux
/// detaches a busy mount).
pub fn unmount(at: &Path) -> io::Result<()> {
    let plain = run_command(
        {
            let mut c = Command::new(if cfg!(target_os = "macos") { "/sbin/umount" } else { "umount" });
            c.arg(at);
            c
        },
        "umount",
    );
    let Err(e) = plain else { return Ok(()) };
    log::warn!("{e}; unmounting by force");
    let mut c = Command::new(if cfg!(target_os = "macos") { "/sbin/umount" } else { "umount" });
    c.arg(if cfg!(target_os = "macos") { "-f" } else { "-l" }).arg(at);
    run_command(c, "umount (forced)")
}

/// Serves `front` on `addr` until a stop signal, mounting it first if `mount` says so;
/// `ready` is told the address listened on (the port the system chose for port 0). Returns
/// when the server has stopped, after unmounting.
pub fn run(
    front: NfsFront,
    addr: SocketAddr,
    mount: Option<Mount>,
    ready: impl FnOnce(SocketAddr),
) -> io::Result<()> {
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
        let server = tokio::spawn(async move { listener.handle_forever().await });
        if let Some(m) = &mount {
            let c = mount_command(bound, &m.at, &m.extra);
            log::debug!("mounting: {c:?}");
            let mounted = tokio::task::spawn_blocking(move || run_command(c, "mount"))
                .await
                .map_err(io::Error::other)?;
            if let Err(e) = mounted {
                server.abort();
                return Err(io::Error::new(e.kind(), format!("cannot mount on {}: {e}", m.at.display())));
            }
            log::info!("mounted on {}", m.at.display());
        }
        ready(bound);
        let sig = tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = int.recv() => "SIGINT",
            _ = hup.recv() => "SIGHUP",
            r = server => {
                // The listener never ends but by an error.
                return Err(match r {
                    Ok(Err(e)) => e,
                    Ok(Ok(())) => io::Error::other("the server stopped"),
                    Err(e) => io::Error::other(e),
                });
            }
        };
        log::info!("{sig}: stopping");
        if let Some(m) = &mount {
            // The server still answers while the client unmounts.
            let at = m.at.clone();
            match tokio::task::spawn_blocking(move || unmount(&at)).await.map_err(io::Error::other)? {
                Ok(()) => log::info!("unmounted {}", m.at.display()),
                Err(e) => log::error!("{e}: unmount {} by hand", m.at.display()),
            }
        }
        Ok(())
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
        assert_eq!(default_port(0), PORT_BASE);
        assert!(default_port(u32::MAX) < 30000);
    }

    #[test]
    fn options_name_the_port_and_add_extras() {
        let o = mount_options(20501, &["nobrowse".to_owned()]);
        assert!(o.contains(&"port=20501".to_owned()));
        assert!(o.contains(&"mountport=20501".to_owned()));
        assert_eq!(o.last().map(String::as_str), Some("nobrowse"));
        assert!(o.iter().any(|x| x == "rdonly" || x == "ro"));
    }

    #[test]
    fn mount_command_names_the_export() {
        let c = mount_command("127.0.0.1:20501".parse().unwrap(), Path::new("/tmp/m"), &[]);
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(&args[args.len() - 2..], ["127.0.0.1:/", "/tmp/m"]);
    }
}
