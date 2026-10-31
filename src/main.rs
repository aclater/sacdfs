// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs SOURCE MOUNTPOINT`: mount SACD disc images as DSF files.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use fuser::{Config, MountOption, Session, SessionACL};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use sacdfs::engine::{Engine, default_threads};
use sacdfs::fs::{MAX_READ, SacdFs};
use sacdfs::tree::{SourceDir, Tree};

/// Show the audio of SACD disc images as DSF files, decoded on demand.
///
/// SOURCE is an image file or a directory searched recursively for *.iso files. Each image
/// appears as MOUNTPOINT/<relative dir>/<image name>/{Stereo,Multichannel}/NN - title.dsf.
/// Unmount with `fusermount3 -u MOUNTPOINT`, or stop sacdfs with SIGINT or SIGTERM.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// An SACD image file, or a directory of them.
    source: PathBuf,
    /// An empty directory to mount on.
    mountpoint: PathBuf,
    /// Stay in the foreground and log to standard error.
    #[arg(short, long)]
    foreground: bool,
    /// DST decoder threads (0: one per CPU).
    #[arg(long, default_value_t = 0, value_name = "N")]
    threads: usize,
    /// Size of the cache of decoded frames, in MiB.
    #[arg(long, default_value_t = 256, value_name = "N")]
    cache_mb: usize,
    /// Let other users see the mount (needs `user_allow_other` in /etc/fuse.conf).
    #[arg(long)]
    allow_other: bool,
    /// Log more (repeat for more still); RUST_LOG overrides it.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let level = match args.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).init();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sacdfs: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> io::Result<()> {
    let source = args.source.canonicalize().map_err(|e| with_path(e, &args.source))?;
    let mountpoint = args.mountpoint.canonicalize().map_err(|e| with_path(e, &args.mountpoint))?;
    if !mountpoint.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{}: not a directory", mountpoint.display()),
        ));
    }
    // Only names are read here; images are opened when their directory is first listed.
    let root = SourceDir::scan(&source).map_err(|e| with_path(e, &source))?;
    log::info!("{}: {} image files", source.display(), root.image_count());

    // Become a daemon before any thread exists; the parent waits for the mount.
    let notify = if args.foreground { None } else { Some(daemonize()?) };

    let threads = if args.threads == 0 { default_threads() } else { args.threads };
    // Opening the images of a directory listing runs on rayon's global pool.
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.min(8))
        .thread_name(|i| format!("sacdfs-toc-{i}"))
        .build_global();
    let engine = Arc::new(Engine::new(threads, args.cache_mb.saturating_mul(1 << 20))?);
    let tree = Arc::new(Tree::new(root));
    // SAFETY-free: getuid/getgid cannot fail.
    let (uid, gid) = ids();
    let fs = SacdFs::new(tree, engine, threads.clamp(4, 16), uid, gid)?;

    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::RO,
        MountOption::FSName(source.display().to_string()),
        MountOption::Subtype("sacdfs".to_owned()),
        MountOption::NoDev,
        MountOption::NoSuid,
        MountOption::NoExec,
        MountOption::CUSTOM(format!("max_read={MAX_READ}")),
    ];
    if args.allow_other {
        config.acl = SessionACL::All;
    }
    config.n_threads = Some(4);
    let mut session = match Session::new(fs, &mountpoint, &config) {
        Ok(s) => s,
        Err(e) => {
            let e = io::Error::new(e.kind(), format!("cannot mount on {}: {e}", mountpoint.display()));
            if let Some(n) = notify {
                n.fail(&e);
            }
            return Err(e);
        }
    };
    let mut unmounter = session.unmount_callable();
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    std::thread::Builder::new().name("sacdfs-signals".to_owned()).spawn(move || {
        if let Some(sig) = signals.forever().next() {
            log::info!("signal {sig}: unmounting");
            if let Err(e) = unmounter.unmount() {
                log::error!("unmount: {e}");
            }
        }
    })?;
    if let Some(n) = notify {
        n.ok();
    }
    log::info!("mounted {} on {}", source.display(), mountpoint.display());
    session.run()?;
    Ok(())
}

fn with_path(e: io::Error, path: &std::path::Path) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

#[allow(unsafe_code)]
fn ids() -> (u32, u32) {
    // SAFETY: getuid and getgid have no preconditions and cannot fail.
    unsafe { (libc::getuid(), libc::getgid()) }
}

/// The daemon side of the pipe to the waiting parent.
struct Notify(File);

impl Notify {
    fn ok(mut self) {
        let _ = self.0.write_all(b"\0");
        redirect_stdio();
    }

    fn fail(mut self, e: &io::Error) {
        let _ = write!(self.0, "{e}");
    }
}

/// Forks. The parent waits until the child reports the mount, then exits; the child
/// continues in a new session.
#[allow(unsafe_code)]
fn daemonize() -> io::Result<Notify> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: fds is a valid array of two ints for pipe to fill.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 returned two new descriptors that nothing else owns.
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // SAFETY: the process has a single thread here (no pool or session exists yet), so the
    // child may run any code after fork.
    match unsafe { libc::fork() } {
        -1 => Err(io::Error::last_os_error()),
        0 => {
            drop(r);
            // SAFETY: setsid has no preconditions; it fails only for a group leader, which
            // a fresh child is not.
            unsafe { libc::setsid() };
            let _ = std::env::set_current_dir("/");
            Ok(Notify(File::from(w)))
        }
        _ => {
            drop(w);
            let mut msg = Vec::new();
            let _ = File::from(r).read_to_end(&mut msg);
            match msg.as_slice() {
                [0] => std::process::exit(0),
                [] => {
                    eprintln!("sacdfs: the daemon exited before mounting");
                    std::process::exit(1)
                }
                m => {
                    eprintln!("sacdfs: {}", String::from_utf8_lossy(m));
                    std::process::exit(1)
                }
            }
        }
    }
}

/// Points standard input, output and error at /dev/null.
#[allow(unsafe_code)]
fn redirect_stdio() {
    let Ok(null) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/null") else { return };
    for fd in 0..3 {
        // SAFETY: dup2 onto the standard descriptors; `null` stays open for the call.
        unsafe { libc::dup2(null.as_raw_fd(), fd) };
    }
}
