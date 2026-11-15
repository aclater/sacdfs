// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs-nfs SOURCE [MOUNTPOINT]` (#420): serves SOURCE's disc images as DSF and WAV files
//! over NFSv3, on 127.0.0.1 by default, for the system's own NFS client: on macOS, where it
//! is how sacdfs mounts, and on Linux for NFS clients without FUSE. With MOUNTPOINT it also
//! mounts there, and unmounts when stopped. See docs/install/macos.md.

#![allow(unsafe_code)] // getuid and getgid only, as in main.rs.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use sacdfs::disc::Tuning;
use sacdfs::engine::{Engine, default_threads};
use sacdfs::nfs::serve::{self, DEFAULT_LISTEN, Mount};
use sacdfs::nfs::{Callers, NfsFront, handle};
use sacdfs::pcm::{Cutoff, PcmConfig, PcmRate};
use sacdfs::sidecar::Sidecars;
use sacdfs::tree::{Output, SourceDir, Tree};
use sacdfs::vfs::Volume;

/// Serve the audio of SACD disc images as DSF and WAV files over NFSv3, for the system's own
/// NFS client, and mount it if MOUNTPOINT is given.
///
/// It runs in the foreground until SIGINT, SIGTERM or SIGHUP, then unmounts and stops.
/// Without MOUNTPOINT, mount it yourself; on macOS:
/// `mount_nfs -o vers=3,tcp,port=PORT,mountport=PORT,rdonly,nfc,locallocks,soft,intr localhost:/ MOUNTPOINT`.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// An SACD image file, or a directory of them.
    source: PathBuf,
    /// A folder to mount on (on Linux, mounting needs root). Without it, nothing is mounted.
    mountpoint: Option<PathBuf>,
    /// Address to listen on. The default, 127.0.0.1, takes connections from this computer
    /// only; any other address serves the files to whoever can reach it.
    #[arg(long, default_value_t = DEFAULT_LISTEN, value_name = "ADDR")]
    listen: IpAddr,
    /// Port to listen on (default: 20000 plus the user ID's last four digits, so one per
    /// user; 1007 for root, below 1024 so that no other user can take it over). 0: any free
    /// port.
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,
    /// Answer every caller: the default when run as root, as for `sacdfs`. Otherwise only
    /// calls made as this user (or root) are answered: a guard against mistakes, not a
    /// security boundary (any local program can claim to be this user). Needed for clients
    /// on other computers, whose user IDs differ.
    #[arg(long)]
    allow_other: bool,
    /// Only calls made as this user (or root) are answered, also when run as root.
    #[arg(long, conflicts_with = "allow_other")]
    no_allow_other: bool,
    /// More options for the mount, comma-separated (macOS: nobrowse, for example).
    #[arg(short = 'o', long = "mount-options", value_name = "LIST", value_delimiter = ',')]
    mount_options: Vec<String>,
    /// DST decoder threads (0: one per CPU).
    #[arg(long, default_value_t = 0, value_name = "N")]
    threads: usize,
    /// Size of the cache of decoded and converted frames, in MiB.
    #[arg(long, default_value_t = 32, value_name = "N")]
    cache_mb: usize,
    /// Which files to show: dsf, wav, or both (dsf,wav).
    #[arg(long, default_value = "dsf,wav", value_name = "LIST", value_parser = parse_formats)]
    format: Formats,
    /// Pauses: dropped (each file is the track's own range) or kept (with its pause).
    #[arg(long, default_value = "dropped", value_name = "MODE", value_parser = ["dropped", "kept"])]
    pauses: String,
    /// Sample rate of the WAV files, in Hz: 88200, 176400 or 352800.
    #[arg(long, default_value_t = 176_400, value_name = "HZ", value_parser = parse_rate)]
    pcm_rate: u32,
    /// Low-pass cutoff of the PCM conversion: 40 or 30 (kHz), or wide (needs a --pcm-rate of
    /// 176400 or 352800).
    #[arg(long, default_value = "40", value_name = "KHZ", value_parser = parse_cutoff)]
    pcm_cutoff: Cutoff,
    /// Collapse each chain of folders below the top level that holds a single image into its
    /// outermost folder.
    #[arg(long)]
    flatten: bool,
    /// Files passed through from SOURCE next to the images' folders (as for `sacdfs`).
    #[arg(long, default_value = "default", value_name = "LIST", value_parser = Sidecars::parse)]
    sidecars: Sidecars,
    /// Seconds a request may wait for the source before it fails with an I/O error; 0 waits
    /// for good.
    #[arg(long, default_value_t = sacdfs::watchdog::DEFAULT_TIMEOUT.as_secs(), value_name = "SECS")]
    request_timeout: u64,
    /// Log more (repeat for more still); RUST_LOG overrides it.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

/// The formats shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Formats {
    dsf: bool,
    wav: bool,
}

fn parse_formats(s: &str) -> Result<Formats, String> {
    let mut f = Formats { dsf: false, wav: false };
    for part in s.split(',') {
        match part.trim().to_ascii_lowercase().as_str() {
            "dsf" => f.dsf = true,
            "wav" => f.wav = true,
            other => return Err(format!("unknown format {other:?}: use dsf, wav or dsf,wav")),
        }
    }
    Ok(f)
}

fn pcm_rate(hz: u32) -> Option<PcmRate> {
    match hz {
        88_200 => Some(PcmRate::R88200),
        176_400 => Some(PcmRate::R176400),
        352_800 => Some(PcmRate::R352800),
        _ => None,
    }
}

fn parse_rate(s: &str) -> Result<u32, String> {
    s.parse()
        .ok()
        .filter(|&hz| pcm_rate(hz).is_some())
        .ok_or_else(|| "use 88200, 176400 or 352800".to_owned())
}

fn parse_cutoff(s: &str) -> Result<Cutoff, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "40" => Ok(Cutoff::Khz40),
        "30" => Ok(Cutoff::Khz30),
        "wide" => Ok(Cutoff::Wide80),
        _ => Err("use 40, 30 or wide".to_owned()),
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
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

fn with_path(e: io::Error, path: &std::path::Path) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

fn run(args: Args) -> io::Result<()> {
    raise_nofile();
    let (uid, gid) = ids();
    let rate = pcm_rate(args.pcm_rate).unwrap_or(PcmRate::R176400);
    let pcm = PcmConfig { rate, cutoff: args.pcm_cutoff, gain_db: 0.0 };
    pcm.validate().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("--pcm-cutoff wide needs --pcm-rate 176400 or 352800 ({e})"),
        )
    })?;
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
            serve::check_mountpoint(&m, uid)?;
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
    let root = SourceDir::scan_with(&source, &args.sidecars, mountpoint.as_deref())
        .map_err(|e| with_path(e, &source))?;
    log::info!(
        "{}: {} image files; sidecar files: {}",
        source.display(),
        root.image_count(),
        args.sidecars.describe()
    );
    let threads = if args.threads == 0 { default_threads() } else { args.threads };
    let output = Output {
        dsf: args.format.dsf,
        wav: args.format.wav,
        samples_per_frame: rate.samples_per_frame() as u32,
        pauses_kept: args.pauses == "kept",
    };
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.min(8))
        .thread_name(|i| format!("sacdfs-toc-{i}"))
        .build_global();
    let engine = Arc::new(Engine::with_pcm(threads, args.cache_mb.saturating_mul(1 << 20), pcm)?);
    engine.start_idle_release()?;
    let mut tree =
        Tree::with_options(root, Tuning::default(), output).with_flatten(args.flatten).with_pcm(&pcm);
    if let Some(m) = &mountpoint {
        tree = tree.excluding(m);
    }
    let tree = Arc::new(if source.is_dir() { tree.watching(&args.sidecars) } else { tree });
    let volume = Volume::new(tree, engine, threads.clamp(4, 16), uid, gid)?
        .with_request_timeout(Some(Duration::from_secs(args.request_timeout)))?;
    sacdfs::handles::start_idle_closer()?;
    let callers = if args.allow_other || (uid == 0 && !args.no_allow_other) {
        log::info!("every caller is answered (--no-allow-other answers only this user)");
        Callers::Anyone
    } else {
        Callers::User(uid)
    };
    let front = NfsFront::new(Arc::new(volume), handle::volume_id(&source), callers);
    let addr = SocketAddr::new(args.listen, args.port.unwrap_or_else(|| serve::default_port(uid)));
    let mount = mountpoint.map(|at| Mount { at, extra: args.mount_options.clone() });
    let what = source.display().to_string();
    serve::run(front, addr, mount, |bound| log::info!("serving {what} on {bound}"))
}

/// Raises the soft limit on open files to the hard one: images and sidecars share it.
fn raise_nofile() {
    use nix::sys::resource::{Resource, getrlimit, setrlimit};
    if let Ok((soft, hard)) = getrlimit(Resource::RLIMIT_NOFILE) {
        if soft < hard && setrlimit(Resource::RLIMIT_NOFILE, hard, hard).is_err() {
            // macOS refuses an unlimited hard limit as the soft one.
            let _ = setrlimit(Resource::RLIMIT_NOFILE, hard.min(10_240), hard);
        }
    }
}

fn ids() -> (u32, u32) {
    // SAFETY: getuid and getgid have no preconditions and cannot fail.
    unsafe { (libc::getuid(), libc::getgid()) }
}
