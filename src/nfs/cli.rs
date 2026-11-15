// SPDX-License-Identifier: GPL-3.0-or-later

//! The command line of `sacdfs-nfs SOURCE [MOUNTPOINT]` (#420), in the library so that the
//! `sacdfs` binary can offer the same mount where it has no FUSE front end. The options that
//! shape the files are `sacdfs`'s own ([`crate::serve::Options`], [`crate::serve::Limits`]).

#![allow(unsafe_code)] // getuid and getgid only, as in the Linux front end.

use std::ffi::OsString;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;

use super::serve::{self, DEFAULT_LISTEN, Mount};
use super::{Callers, NfsFront, handle};
use crate::serve::{self as core, with_path};
use crate::vfs::Volume;

/// Serve the audio of SACD disc images as DSF and WAV files over NFSv3 (version 3 of the
/// Network File System), for this computer's own NFS client, and mount it on MOUNTPOINT.
///
/// SOURCE is an image file or a folder searched recursively for *.iso files. It runs in the
/// foreground until SIGINT, SIGTERM or SIGHUP, then unmounts and stops. Without MOUNTPOINT it
/// only serves; on macOS, mount it with
/// `mount_nfs -o vers=3,tcp,port=PORT,mountport=PORT,rdonly,nfc,locallocks,soft,intr localhost:/ MOUNTPOINT`.
#[derive(Debug, Parser)]
#[command(name = "sacdfs-nfs", version, about)]
pub struct Args {
    /// An SACD image file, or a folder of them.
    pub source: PathBuf,
    /// A folder to mount on (on Linux, mounting needs root). Without it, nothing is mounted.
    pub mountpoint: Option<PathBuf>,
    #[command(flatten)]
    pub serve: core::Options,
    /// Address to listen on. The default, 127.0.0.1, takes connections from this computer
    /// only; any other address serves the files to whoever can reach it.
    #[arg(long, default_value_t = DEFAULT_LISTEN, value_name = "ADDR")]
    pub listen: IpAddr,
    /// Port to listen on (default: 20000 plus the user ID's last four digits, so one per
    /// user). 0: any free port.
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
    /// Answer every caller: the default when run as root, as for `sacdfs`. Otherwise only
    /// calls made as this user (or root) are answered: a guard against mistakes, not a
    /// security boundary (any program on this computer can claim to be this user). Needed
    /// for clients on other computers, whose user IDs differ.
    #[arg(long)]
    pub allow_other: bool,
    /// Only calls made as this user (or root) are answered, also when run as root.
    #[arg(long, conflicts_with = "allow_other")]
    pub no_allow_other: bool,
    /// More options for the mount, comma-separated (macOS: nobrowse hides it from the Finder).
    #[arg(short = 'o', long = "mount-options", value_name = "LIST", value_delimiter = ',')]
    pub mount_options: Vec<String>,
    #[command(flatten)]
    pub limits: core::Limits,
    /// macOS: instead of serving, write a launchd agent that runs this same command (without
    /// this option) at each login, to ~/Library/LaunchAgents/io.github.aclater.sacdfs-nfs.plist,
    /// and say how to start and stop it.
    #[arg(long, requires = "mountpoint")]
    pub launch_agent: bool,
    /// Log more (repeat for more still); RUST_LOG overrides it.
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

/// Runs `sacdfs-nfs` with `args` (the program name first).
pub fn main(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let argv: Vec<OsString> = args.into_iter().collect();
    let args = Args::parse_from(&argv);
    if args.launch_agent {
        return match super::agent::write(&argv) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("sacdfs-nfs: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let level = match args.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).init();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sacdfs-nfs: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Serves, and mounts if asked, until stopped.
pub fn run(args: Args) -> io::Result<()> {
    let nofile = raise_nofile();
    let (uid, gid) = ids();
    let pcm = args.serve.check()?;
    let source = args.source.canonicalize().map_err(|e| with_path(e, &args.source))?;
    let mountpoint = match &args.mountpoint {
        Some(m) => {
            let m = m.canonicalize().map_err(|e| with_path(e, m))?;
            if !m.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!("{}: not a directory", m.display()),
                ));
            }
            // Nothing sacdfs reads may lie under its own mount (#56).
            if source.starts_with(&m) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is under the mount point {}", source.display(), m.display()),
                ));
            }
            Some(m)
        }
        None => None,
    };
    let cache_dir = args.limits.index_cache_dir(mountpoint.as_deref())?;
    crate::engine::limit_malloc_arenas();
    let root = args.serve.scan(&source, mountpoint.as_deref())?;
    args.serve.log_scan(&source, &root);
    let sizing = args.serve.sizing(&root);
    if let Some(dir) = &cache_dir {
        core::open_index_cache(dir);
    }
    let threads = sizing.threads;
    let (tree, engine) = core::build(
        &args.serve,
        core::Parts {
            source: &source,
            root,
            pcm,
            sizing: &sizing,
            exclude: mountpoint.as_deref(),
            cache_dir: cache_dir.as_deref(),
            open_files: open_files_budget(nofile, args.limits.max_open_images),
        },
    )?;
    let volume = Volume::new(tree.clone(), engine.clone(), core::request_threads(threads), uid, gid)?
        .with_request_timeout(args.limits.request_timeout())?;
    let callers = if args.allow_other || (uid == 0 && !args.no_allow_other) {
        log::info!("every caller is answered (--no-allow-other answers only this user)");
        Callers::Anyone
    } else {
        Callers::User(uid)
    };
    let front = NfsFront::new(Arc::new(volume), handle::volume_id(&source), callers);
    let addr = SocketAddr::new(args.listen, args.port.unwrap_or_else(|| serve::default_port(uid)));
    let mount = mountpoint.map(|at| Mount { at, extra: args.mount_options.clone() });
    let mut background = core::start_background(&args.serve, sizing, &tree, &engine, cache_dir.as_deref())?;
    let what = source.display().to_string();
    let result = serve::run(front, addr, mount, |bound| log::info!("serving {what} on {bound}"));
    background.stop();
    result
}

/// The image files to keep open and the bound on open sidecar files, for the open files limit
/// `nofile`: images take at most half of what the margin leaves, as on Linux.
fn open_files_budget(nofile: Option<u64>, max_open_images: usize) -> (usize, usize) {
    use crate::vfs::sidecar_slots::{DEFAULT_CAP, MARGIN, cap_for};
    let Some(limit) = nofile else { return (max_open_images, DEFAULT_CAP) };
    let most = usize::try_from(limit.saturating_sub(MARGIN) / 2).unwrap_or(usize::MAX).max(1);
    let images = max_open_images.min(most);
    if images < max_open_images {
        log::warn!(
            "--max-open-images {max_open_images} is too close to the open files limit ({limit}): \
             keeping at most {images} image files open"
        );
    }
    (images, cap_for(limit, images))
}

/// Raises the soft limit on open files as far as allowed: images and sidecars share it, and
/// macOS starts processes at 256. The limit then, if it can be read.
fn raise_nofile() -> Option<u64> {
    use nix::sys::resource::{Resource, getrlimit, setrlimit};
    let (soft, hard) = getrlimit(Resource::RLIMIT_NOFILE).ok()?;
    if soft >= hard {
        return Some(soft);
    }
    if setrlimit(Resource::RLIMIT_NOFILE, hard, hard).is_ok() {
        return Some(hard);
    }
    // macOS refuses an unlimited soft limit (above kern.maxfilesperproc); 10240 is what
    // OPEN_MAX allows everywhere.
    let some = hard.min(10_240);
    if setrlimit(Resource::RLIMIT_NOFILE, some, hard).is_ok() { Some(some) } else { Some(soft) }
}

fn ids() -> (u32, u32) {
    // SAFETY: getuid and getgid have no preconditions and cannot fail.
    unsafe { (libc::getuid(), libc::getgid()) }
}
