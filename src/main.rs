// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs SOURCE MOUNTPOINT`: mount SACD disc images as DSF and WAV files.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;
use fuser::{Config, MountOption, Session, SessionACL};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use sacdfs::disc::Tuning;
use sacdfs::engine::{Engine, default_threads};
use sacdfs::fs::{MAX_READ, SacdFs};
use sacdfs::index_cache::{self, IndexCache};
use sacdfs::pcm::{Cutoff, PcmConfig, PcmRate};
use sacdfs::tree::{Output, SourceDir, Tree};

/// Show the audio of SACD disc images as DSF and WAV files, decoded on demand.
///
/// SOURCE is an image file or a directory searched recursively for *.iso files. Each image
/// appears as MOUNTPOINT/<relative dir>/<image name>/{Stereo,Multichannel}/NN - title.dsf,
/// with NN - title.wav (24-bit PCM, converted on demand) next to it.
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
    /// Size of the cache of decoded and converted frames, in MiB.
    #[arg(long, default_value_t = 256, value_name = "N")]
    cache_mb: usize,
    /// Which files to show: dsf, wav, or both (dsf,wav).
    #[arg(long, default_value = "dsf,wav", value_name = "LIST", value_parser = parse_formats)]
    format: Formats,
    /// Sample rate of the WAV files, in Hz: 88200, 176400 or 352800.
    #[arg(long, default_value_t = 176_400, value_name = "HZ", value_parser = parse_rate)]
    pcm_rate: u32,
    /// Low-pass cutoff of the PCM conversion: 40 or 30 (kHz), or wide (about 80 kHz; needs a
    /// --pcm-rate of 176400 or 352800).
    #[arg(long, default_value = "40", value_name = "KHZ", value_parser = parse_cutoff)]
    pcm_cutoff: Cutoff,
    /// Gain of the PCM conversion in dB, -60 to +24; samples that would clip are clamped.
    #[arg(long, default_value_t = 0.0, value_name = "DB", allow_hyphen_values = true, value_parser = parse_gain)]
    pcm_gain: f32,
    /// Collapse each chain of folders below the top level that holds a single image into
    /// its outermost folder, which then holds the image's Stereo/Multichannel folders.
    #[arg(long)]
    flatten: bool,
    /// Let other users see the mount (needs `user_allow_other` in /etc/fuse.conf).
    #[arg(long)]
    allow_other: bool,
    /// Directory of the persistent seek index, which lets seeks after a remount skip most
    /// of their search (default: $XDG_CACHE_HOME/sacdfs, else ~/.cache/sacdfs).
    #[arg(long, value_name = "DIR")]
    index_cache: Option<PathBuf>,
    /// Keep no persistent seek index: every mount starts with an empty one.
    #[arg(long, conflicts_with = "index_cache")]
    no_index_cache: bool,
    /// Most image files kept open at once (1 to 1024); the least recently used is closed
    /// first, and any closes after 5 minutes unused.
    #[arg(long, default_value_t = sacdfs::handles::DEFAULT_MAX_OPEN, value_name = "N",
          value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=1024))]
    max_open_images: usize,
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

fn parse_gain(s: &str) -> Result<f32, String> {
    s.parse::<f32>()
        .ok()
        .filter(|g| g.is_finite() && (-60.0..=24.0).contains(g))
        .ok_or_else(|| "use a number of dB from -60 to 24".to_owned())
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
    // The wide filter passes up to about 80 kHz, which 88.2 kHz PCM cannot hold (its Nyquist
    // frequency is 44.1 kHz). The converter would quietly use 40 kHz instead: refuse the
    // combination before mounting.
    let rate = pcm_rate(args.pcm_rate).unwrap_or(PcmRate::R176400);
    let pcm = PcmConfig { rate, cutoff: args.pcm_cutoff, gain_db: args.pcm_gain };
    if let Err(e) = pcm.validate() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("--pcm-cutoff wide needs --pcm-rate 176400 or 352800 ({e})"),
        ));
    }
    let source = args.source.canonicalize().map_err(|e| with_path(e, &args.source))?;
    // A FUSE mount still on the mount point (an earlier sacdfs that is stopping): mounting
    // over it would put this mount inside it, where its unmount takes this one along (#51).
    // Waited for before the mount point itself is looked at: a stat of a mount that is
    // going away can fail.
    wait_for_earlier_mount(&outside_path(&args.mountpoint))?;
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

    // Before daemonizing, which changes the working directory.
    open_index_cache(&args);

    // Become a daemon before any thread exists; the parent waits for the mount.
    let notify = if args.foreground { None } else { Some(daemonize()?) };

    sacdfs::handles::global().configure(args.max_open_images, sacdfs::handles::DEFAULT_IDLE);

    let threads = if args.threads == 0 { default_threads() } else { args.threads };
    // Opening the images of a directory listing runs on rayon's global pool.
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.min(8))
        .thread_name(|i| format!("sacdfs-toc-{i}"))
        .build_global();
    let output = Output {
        dsf: args.format.dsf,
        wav: args.format.wav,
        samples_per_frame: rate.samples_per_frame() as u32,
    };
    let engine = Arc::new(Engine::with_pcm(threads, args.cache_mb.saturating_mul(1 << 20), pcm)?);
    let tree = Arc::new(
        Tree::with_options(root, Tuning::default(), output).with_flatten(args.flatten).with_pcm(&pcm),
    );
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
    let session = match Session::new(fs, &mountpoint, &config) {
        Ok(s) => s,
        Err(e) => {
            let e = io::Error::new(e.kind(), format!("cannot mount on {}: {e}", mountpoint.display()));
            if let Some(n) = notify {
                n.fail(&e);
            }
            return Err(e);
        }
    };
    // This mount's identity: its device number, unique while the mount exists. Read from
    // the mount table, which asks the file system nothing (a stat would wait for the session).
    let own = top_mount_device(&mountpoint);
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    let at = mountpoint.clone();
    let mine = own.clone();
    let detached = Arc::new(AtomicBool::new(false));
    let detached_by_signal = detached.clone();
    std::thread::Builder::new().name("sacdfs-signals".to_owned()).spawn(move || {
        if let Some(sig) = signals.forever().next() {
            // Unmounting goes by path, so only while the mount on top at the path is still
            // this one: once it was unmounted or detached, or something else was mounted
            // over it, the path names another mount, which must be left alone (#51).
            if mine.is_none() || top_mount_device(&at) != mine {
                log::warn!(
                    "signal {sig}: {} is no longer this mount (unmounted, or another is mounted \
                     over it): not unmounting it; sacdfs stops when its mount is gone",
                    at.display()
                );
                return;
            }
            log::info!("signal {sig}: unmounting");
            detached_by_signal.store(true, Ordering::Relaxed);
            if let Err(e) = unmount(&at) {
                // Busy (open files, or an NFS server exporting the mount): detach it, as
                // fusermount3 does for users other than root. sacdfs then serves the files
                // still open and exits when the last is closed.
                if top_mount_device(&at) != mine {
                    log::warn!("unmount: {e}; {} is no longer this mount: not detaching it", at.display());
                    return;
                }
                match lazy_unmount(&at) {
                    Ok(()) => log::warn!(
                        "unmount: {e}; detached instead: sacdfs stops when its open files are closed"
                    ),
                    Err(e2) => log::error!("unmount: {e}; detaching: {e2}"),
                }
            }
        }
    })?;
    sacdfs::handles::start_idle_closer()?;
    if let Some(cache) = index_cache::global() {
        let cache = cache.clone();
        std::thread::Builder::new().name("sacdfs-index".to_owned()).spawn(move || {
            let mut warned = false;
            loop {
                std::thread::sleep(FLUSH_EVERY);
                if let Err(e) = cache.flush() {
                    if !std::mem::replace(&mut warned, true) {
                        log::warn!("seek index cache {}: {e}", cache.dir().display());
                    }
                }
            }
        })?;
    }
    if let Some(n) = notify {
        n.ok();
    }
    log::info!("mounted {} on {}", source.display(), mountpoint.display());
    let result = serve(session);
    let failed = result.as_ref().err().filter(|e| e.raw_os_error() != Some(libc::ECONNABORTED));
    if let Some(e) = failed.filter(|_| own.is_some() && top_mount_device(&mountpoint) == own) {
        // The session failed while still mounted (an error, not an unmount): don't leave a
        // dead mount behind. The device number is still this mount's: the mount still
        // exists, so no other can have it.
        log::warn!("session ended: {e}; detaching {}", mountpoint.display());
        let _ = lazy_unmount(&mountpoint);
    }
    if let Some(cache) = index_cache::global() {
        match cache.flush() {
            Ok(n) => log::debug!("seek index cache: {n} entries written"),
            Err(e) => log::warn!("seek index cache {}: {e}", cache.dir().display()),
        }
    }
    let (calls, bytes) = sacdfs::disc::total_reads();
    log::info!("image reads for audio: {calls} calls, {bytes} bytes");
    match result {
        // After a detach, the kernel can end the connection as aborted rather than
        // unmounted once the last open file is closed: the stop that was asked for.
        Err(e) if e.raw_os_error() == Some(libc::ECONNABORTED) && detached.load(Ordering::Relaxed) => {
            log::debug!("connection ended after the unmount: {e}");
            Ok(())
        }
        r => r,
    }
}

/// How often the persistent seek index is saved, if it changed.
const FLUSH_EVERY: Duration = Duration::from_secs(30);

/// Installs the persistent seek index the options ask for. A cache that cannot be used is
/// only a warning: it costs speed, never the mount.
fn open_index_cache(args: &Args) {
    if args.no_index_cache {
        return;
    }
    let Some(dir) = args.index_cache.clone().or_else(index_cache::default_dir) else {
        log::warn!("no seek index cache: neither XDG_CACHE_HOME nor HOME is set");
        return;
    };
    let opened = std::path::absolute(&dir).and_then(|d| IndexCache::open(&d));
    match opened {
        Ok(cache) => {
            log::debug!("seek index cache in {}", cache.dir().display());
            index_cache::install(Arc::new(cache));
        }
        Err(e) => log::warn!("no seek index cache in {}: {e}", dir.display()),
    }
}

fn with_path(e: io::Error, path: &std::path::Path) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

/// How long a new mount waits for an earlier FUSE mount on the same mount point to go away.
const EARLIER_MOUNT_WAIT: Duration = Duration::from_secs(10);

/// Waits until no FUSE file system is mounted on `at`, for up to [`EARLIER_MOUNT_WAIT`].
///
/// A sacdfs restarted at the same path (by a service manager, a container, a script) can
/// start before the previous one has unmounted. Mounted on top, the new mount would sit
/// inside the old one: the old one's unmount (by path) would then remove the new mount, or
/// its detach (`MNT_DETACH`, which takes every mount below too) would take it along (#51).
fn wait_for_earlier_mount(at: &std::path::Path) -> io::Result<()> {
    if !fuse_mounted_on(at) {
        return Ok(());
    }
    log::info!("{} is a FUSE mount: waiting for it to be unmounted", at.display());
    let start = std::time::Instant::now();
    while fuse_mounted_on(at) {
        if start.elapsed() >= EARLIER_MOUNT_WAIT {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{}: already a FUSE mount (another sacdfs?); unmount it first", at.display()),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// `path` made absolute and canonical without looking at `path` itself (only at its parent,
/// which is outside any mount on it). The mount point's own name is kept as given.
fn outside_path(path: &std::path::Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    match (abs.parent(), abs.file_name()) {
        (Some(parent), Some(name)) => parent.canonicalize().map_or_else(|_| abs.clone(), |p| p.join(name)),
        _ => abs,
    }
}

/// Whether the mount on top at `at` is a FUSE file system (`at` is its mount point, not
/// just inside one). Read from the mount table, which asks the file system nothing.
fn fuse_mounted_on(at: &std::path::Path) -> bool {
    top_mount(at).is_some_and(|(_, fstype)| fstype == "fuse" || fstype.starts_with("fuse."))
}

/// The device number (`major:minor`) of the mount on top at `at`, if `at` is a mount point.
fn top_mount_device(at: &std::path::Path) -> Option<String> {
    top_mount(at).map(|(dev, _)| dev)
}

/// The device number and file system type of the mount on top at `at`, from
/// `/proc/self/mountinfo`: of the mounts on that path, the last listed, which is the one
/// mounted last, over the others.
fn top_mount(at: &std::path::Path) -> Option<(String, String)> {
    let info = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let want = at.as_os_str().as_encoded_bytes();
    info.lines()
        .filter_map(|l| {
            let mut f = l.split(' ');
            let (_, _, dev, _, mp) = (f.next()?, f.next()?, f.next()?, f.next()?, f.next()?);
            let fstype = l.split(" - ").nth(1)?.split(' ').next()?;
            (unescape_mountinfo(mp) == want).then(|| (dev.to_owned(), fstype.to_owned()))
        })
        .next_back()
}

/// A path from `/proc/self/mountinfo`, where space, tab, newline and backslash are written
/// as `\ooo` (octal).
fn unescape_mountinfo(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let oct =
            b.get(i + 1..i + 4).filter(|o| b[i] == b'\\' && o.iter().all(|c| (b'0'..=b'7').contains(c)));
        match oct {
            Some(o) => {
                out.push(o.iter().fold(0u8, |v, c| v.wrapping_mul(8).wrapping_add(c - b'0')));
                i += 4;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    out
}

/// Serves the session until the kernel ends it, then returns, without unmounting.
///
/// `Session::run` unmounts by path when it ends, and in fuser 0.18 its check that the
/// mount still exists always passes (`is_mounted` reads `POLLERR` the wrong way round;
/// fixed upstream after 0.18.0). After `fusermount3 -u`, a sacdfs on its way out would then
/// unmount whatever was mounted on the path since: a restarted sacdfs (#51). `spawn` moves
/// the mount out of the session, so its end unmounts nothing; the mount is then never
/// dropped (its drop unmounts by path, too). sacdfs unmounts on a signal itself, by path
/// but only after checking that the mount there is its own.
#[allow(unsafe_code)]
fn serve(session: Session<SacdFs>) -> io::Result<()> {
    let background = std::mem::ManuallyDrop::new(session.spawn()?);
    // SAFETY: the handle is read out once and `background` is never dropped or used again,
    // so the handle has a single owner.
    let guard = unsafe { std::ptr::read(&background.guard) };
    guard.join().map_err(|_| io::Error::other("the FUSE session panicked"))?
}

/// Unmounts the file system at `at`: `umount`, or `fusermount3 -u -z` if not root (as fuser
/// did; the setuid helper detaches, so a busy mount is not refused).
#[allow(unsafe_code)]
fn unmount(at: &std::path::Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(at.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mount point holds a NUL byte"))?;
    // SAFETY: a valid NUL-terminated path that outlives the call.
    if unsafe { libc::umount2(c.as_ptr(), 0) } == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() != Some(libc::EPERM) {
        return Err(e);
    }
    let st = std::process::Command::new("fusermount3").args(["-u", "-z", "--"]).arg(at).status()?;
    if st.success() { Ok(()) } else { Err(io::Error::other(format!("fusermount3 -u -z: {st}"))) }
}

/// Detaches the mount at `at` (`umount -l`).
#[allow(unsafe_code)]
fn lazy_unmount(at: &std::path::Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(at.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mount point holds a NUL byte"))?;
    // SAFETY: a valid NUL-terminated path that outlives the call.
    if unsafe { libc::umount2(c.as_ptr(), libc::MNT_DETACH) } == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    // Not root: the setuid helper, lazily.
    let st = std::process::Command::new("fusermount3").args(["-u", "-z", "--"]).arg(at).status();
    match st {
        Ok(s) if s.success() => Ok(()),
        _ => Err(e),
    }
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
