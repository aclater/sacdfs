// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs SOURCE MOUNTPOINT`: mount SACD disc images as DSF and WAV files.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
mod mount_opts;

use fuser::{Config, MountOption, Session, SessionACL};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use sacdfs::disc::Tuning;
use sacdfs::engine::{Engine, default_threads};
use sacdfs::fs::{MAX_READ, SacdFs};
use sacdfs::index_cache::{self, IndexCache};
use sacdfs::pcm::{Cutoff, PcmConfig, PcmRate};
use sacdfs::sidecar::Sidecars;
use sacdfs::tree::{Output, SourceDir, Tree};

/// Show the audio of SACD disc images as DSF and WAV files, decoded on demand.
///
/// SOURCE is an image file or a directory searched recursively for *.iso files. Each image
/// appears as MOUNTPOINT/<relative dir>/<image name>/{Stereo,Multichannel}/NN - title.dsf,
/// with NN - title.wav (24-bit PCM, converted on demand) next to it.
/// Unmount with `fusermount3 -u MOUNTPOINT`, or stop sacdfs with SIGINT, SIGTERM or SIGHUP.
///
/// `sacdfs verify SOURCE [EXTRACTS]...` checks extracted DSF and DFF files against what
/// sacdfs serves, before they are deleted: see `sacdfs verify --help`. `sacdfs tags` keeps
/// user tags for the files (`--tags`): see `sacdfs tags --help`.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// An SACD image file, or a directory of them.
    source: PathBuf,
    /// A directory to mount on, normally empty (its files are hidden while mounted); not
    /// SOURCE or a directory above it.
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
    /// Do not add " (Multichannel)" to the album tag of the files of the multichannel area
    /// (by default they get it, so players and servers show them as an album of their own).
    #[arg(long)]
    no_multichannel_album_suffix: bool,
    /// Directory of tags files (#81), one per disc, named by its disc key
    /// (`sacdfs tags key IMAGE`): their tags override and add to the TOC's. Without it, or
    /// for a disc without a file, the files are as the TOC gives them.
    #[arg(long, value_name = "DIR")]
    tags: Option<PathBuf>,
    /// Collapse each chain of folders below the top level that holds a single image into
    /// its outermost folder, which then holds the image's Stereo/Multichannel folders.
    #[arg(long)]
    flatten: bool,
    /// Files passed through from SOURCE, byte for byte, next to the images' folders: a
    /// comma-separated list of extensions and folder names ending in / (whose whole content
    /// passes). `default` is jpg,jpeg,png,webp,m3u,m3u8,Artwork/,Scans/; `-item` removes an
    /// item (default,-webp), `none` passes nothing.
    #[arg(long, default_value = "default", value_name = "LIST", value_parser = Sidecars::parse)]
    sidecars: Sidecars,
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
    /// Append the log to FILE instead of standard error, at info level unless -v says more.
    /// Without it a background mount logs nowhere, except to the journal under systemd.
    #[arg(long, value_name = "FILE")]
    log_file: Option<PathBuf>,
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
    // `sacdfs verify ...` (#80) has options of its own: see `sacdfs verify --help`.
    if std::env::args_os().nth(1).is_some_and(|a| a == "verify") {
        return sacdfs::verify::main(std::env::args_os().skip(1));
    }
    // `sacdfs tags ...` (#81): see `sacdfs tags --help`.
    if std::env::args_os().nth(1).is_some_and(|a| a == "tags") {
        return sacdfs::tags_cli::main(std::env::args_os().skip(1));
    }
    // `-o` options, as `mount -t fuse.sacdfs` and fstab pass them (#60).
    let argv = match mount_opts::expand(std::env::args_os().collect()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sacdfs: {e}");
            return ExitCode::FAILURE;
        }
    };
    let args = Args::parse_from(argv);
    let level = match args.verbose {
        // With a log file the `mounted` line and the like are the point of it.
        0 if args.log_file.is_some() => "info",
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let mut logger = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level));
    if let Some(path) = &args.log_file {
        // Opened here, before the fork: a bad path fails the command, and the daemon's
        // change to / does not matter.
        match open_log_file(path) {
            Ok(f) => {
                logger.target(env_logger::Target::Pipe(Box::new(f)));
            }
            Err(e) => {
                eprintln!("sacdfs: --log-file {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        }
    }
    logger.init();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sacdfs: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Tells systemd the mount is ready (`READY=1` to `$NOTIFY_SOCKET`, #60), for `Type=notify`.
/// Nothing happens without the variable. Failure is only logged: the mount works anyway.
fn sd_notify_ready() {
    let Some(path) = std::env::var_os("NOTIFY_SOCKET").filter(|p| !p.is_empty()) else { return };
    if let Err(e) = send_ready(&path) {
        log::warn!("NOTIFY_SOCKET {}: {e}", path.to_string_lossy());
    }
}

/// Sends `READY=1` to the socket at `path` (an `@` first makes it an abstract address).
fn send_ready(path: &std::ffi::OsStr) -> io::Result<()> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    let addr = match path.as_bytes().strip_prefix(b"@") {
        Some(name) => SocketAddr::from_abstract_name(name)?,
        None => SocketAddr::from_pathname(Path::new(path))?,
    };
    UnixDatagram::unbound()?.send_to_addr(b"READY=1", &addr).map(drop)
}

/// Opens `path` for appending, creating it with mode 0600 (it can hold file names).
fn open_log_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)
}

/// Whether the daemon keeps standard error: under systemd, with a journal stream and no log
/// file, it is the journal (#59). `journal_stream` is `$JOURNAL_STREAM`.
fn keeps_stderr(log_file: bool, journal_stream: bool) -> bool {
    journal_stream && !log_file
}

fn run(args: Args) -> io::Result<()> {
    // The wide filter passes up to about 80 kHz, which 88.2 kHz PCM cannot hold (its Nyquist
    // frequency is 44.1 kHz). The converter would quietly use 40 kHz instead: refuse the
    // combination before mounting.
    let rate = pcm_rate(args.pcm_rate).unwrap_or(PcmRate::R176400);
    sacdfs::dsf::set_mc_album_suffix(!args.no_multichannel_album_suffix);
    if let Some(dir) = &args.tags {
        let dir = dir.canonicalize().map_err(|e| with_path(e, dir))?;
        if !dir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("--tags {}: not a directory", dir.display()),
            ));
        }
        sacdfs::tags::install(Some(sacdfs::tags::Store::new(&dir)));
    }
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
    // Nothing sacdfs reads may lie under its own mount (#56): before the scan, which can
    // take a while on a large library.
    refuse_source_under_mountpoint(&source, &mountpoint)?;
    let cache_dir = index_cache_dir(&args, &mountpoint)?;
    warn_if_not_empty(&mountpoint);
    // Only names are read here; images are opened when their directory is first listed.
    let mut root = SourceDir::scan_with(&source, &args.sidecars, Some(&mountpoint))
        .map_err(|e| with_path(e, &source))?;
    refuse_images_under_mountpoint(&root, &mountpoint)?;
    skip_linked_images_under_mountpoint(&mut root, &mountpoint);
    log::info!(
        "{}: {} image files; sidecar files: {}",
        source.display(),
        root.image_count(),
        args.sidecars.describe()
    );

    // Before daemonizing, which changes the working directory.
    if let Some(dir) = cache_dir {
        open_index_cache(&dir);
    }

    // Become a daemon before any thread exists; the parent waits for the mount.
    let notify = if args.foreground {
        None
    } else {
        Some(daemonize(keeps_stderr(args.log_file.is_some(), std::env::var_os("JOURNAL_STREAM").is_some()))?)
    };
    // Before any thread allocates (#73).
    sacdfs::engine::limit_malloc_arenas();

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
    let tree = Tree::with_options(root, Tuning::default(), output)
        .with_flatten(args.flatten)
        .with_pcm(&pcm)
        .excluding(&mountpoint);
    // A directory SOURCE is re-read when it changes (#101); a single image has no directory.
    let tree = Arc::new(if source.is_dir() { tree.watching(&args.sidecars) } else { tree });
    // SAFETY-free: getuid/getgid cannot fail.
    let (uid, gid) = ids();
    let fs = SacdFs::new(tree, engine, threads.clamp(4, 16), uid, gid)?;

    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::RO,
        // A fixed name, not the source path (#55): see FSNAME.
        MountOption::FSName(FSNAME.to_owned()),
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
    // The signal handlers go in before mounting (#57): a SIGINT, SIGTERM or SIGHUP from
    // here on is held until the signal thread below handles it, which unmounts. Registered
    // after mounting, as before, a signal in between killed sacdfs and left a dead mount.
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    if let Some(sig) = signals.pending().next() {
        log::info!("signal {sig} before mounting: not mounting");
        if let Some(n) = notify {
            n.fail(&io::Error::other(format!("stopped by signal {sig} before mounting")));
        }
        return Ok(());
    }
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
    sd_notify_ready();
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

/// The directory of the persistent seek index the options ask for, if any.
///
/// One under the mount point would be written through sacdfs's own mount (#56): an
/// `--index-cache` there is refused; the default directory there (a mount point at or above
/// the cache directory, such as the home directory) only turns the cache off, since a cache
/// that cannot be used is only a warning elsewhere too.
fn index_cache_dir(args: &Args, mountpoint: &Path) -> io::Result<Option<PathBuf>> {
    if args.no_index_cache {
        return Ok(None);
    }
    let Some(dir) = args.index_cache.clone().or_else(index_cache::default_dir) else {
        log::warn!("no seek index cache: neither XDG_CACHE_HOME nor HOME is set");
        return Ok(None);
    };
    let dir = std::path::absolute(&dir).map_err(|e| with_path(e, &dir))?;
    if !canonical_prefix(&dir).starts_with(mountpoint) {
        return Ok(Some(dir));
    }
    let why = format!(
        "seek index cache {} is inside the mount point {}: sacdfs would write it through its own mount",
        dir.display(),
        mountpoint.display()
    );
    if args.index_cache.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{why}; choose another --index-cache"),
        ));
    }
    log::warn!("{why}; no seek index cache (use --index-cache DIR)");
    Ok(None)
}

/// Installs the persistent seek index in `dir`. A cache that cannot be used is only a
/// warning: it costs speed, never the mount.
fn open_index_cache(dir: &Path) {
    match IndexCache::open(dir) {
        Ok(cache) => {
            log::debug!("seek index cache in {}", cache.dir().display());
            index_cache::install(Arc::new(cache));
        }
        Err(e) => log::warn!("no seek index cache in {}: {e}", dir.display()),
    }
}

/// The name the mount is listed under (the source field of `/proc/self/mountinfo`, and
/// what `df` and `mount` show), for every mount.
///
/// Not the source path (#55). fuser 0.18 passes the name to `fusermount3` unescaped in its
/// `-o` list, so a comma in the path ended the name and the rest became mount options
/// (`fsname=/music/Bach, J.S.` failed; `.../x,allow_other` would have applied it). Escaping
/// it as libfuse's `fuse_opt_add_opt_escaped` does is right only for that path: as root,
/// fuser calls mount(2) itself with the name as the source argument, unescaped, so the
/// same escaped name would show its backslashes; and fuser chooses between the two inside
/// `Session::new`, so sacdfs cannot know which to escape for. A fixed name is safe on both
/// paths, needs no escaping, keeps the library's path out of the mount table that every
/// user can read, and lets a dead mount be recognised as sacdfs's as root too, where the
/// type is plain `fuse` (the `sacdfs` subtype is a fusermount3 option): see
/// [`MountEntry::is_sacdfs`]. The source is logged at mount instead. Revisit when a fuser
/// release escapes the name (fuser #424, unreleased in 0.18).
const FSNAME: &str = "sacdfs";

/// Refuses a source at or below the mount point (#56).
///
/// sacdfs reads the images through their source paths. Under the mount point, after
/// mounting, those paths lead into sacdfs's own mount: each read becomes a FUSE request to
/// sacdfs itself, which waits for itself. The first `ls` hung in D state, and not even
/// SIGKILL freed it (only an abort through /sys/fs/fuse/connections did). Both paths are
/// canonical, so symbolic links cannot hide it.
fn refuse_source_under_mountpoint(source: &Path, mountpoint: &Path) -> io::Result<()> {
    if !source.starts_with(mountpoint) {
        return Ok(());
    }
    let place = if source == mountpoint { "is the mount point" } else { "is inside the mount point" };
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "SOURCE {} {place} {}: sacdfs would read its images through its own mount and hang; \
             mount somewhere else",
            source.display(),
            mountpoint.display()
        ),
    ))
}

/// Refuses images found inside the mount point, which can happen when the mount point lies
/// inside the source (#56): the mount would hide them, and reading them would go through
/// sacdfs's own mount. An empty mount point inside the source is fine.
fn refuse_images_under_mountpoint(root: &SourceDir, mountpoint: &Path) -> io::Result<()> {
    fn find<'a>(d: &'a SourceDir, at: &Path) -> Option<&'a Path> {
        d.images
            .iter()
            .map(PathBuf::as_path)
            .find(|p| p.starts_with(at))
            .or_else(|| d.dirs.iter().find_map(|(_, sub)| find(sub, at)))
    }
    let Some(image) = find(root, mountpoint) else { return Ok(()) };
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "the mount point {} is inside SOURCE and holds images ({}): the mount would hide \
             them and sacdfs would read them through its own mount; mount on an empty directory",
            mountpoint.display(),
            image.display()
        ),
    ))
}

/// Drops, with a warning, every image whose symbolic link leads below the mount point (#67).
///
/// The scan lists a link by its own path, which is outside the mount point, so the checks
/// above pass; but reading the image would go through sacdfs's own mount and hang. The
/// link's target is resolved here; a link that cannot be resolved is kept (opening it fails
/// on its own).
fn skip_linked_images_under_mountpoint(dir: &mut SourceDir, mountpoint: &Path) {
    dir.images.retain(|p| match p.canonicalize() {
        Ok(real) if real.starts_with(mountpoint) => {
            log::warn!(
                "{}: skipped, it leads to {} inside the mount point {}",
                p.display(),
                real.display(),
                mountpoint.display()
            );
            false
        }
        _ => true,
    });
    for (_, sub) in &mut dir.dirs {
        skip_linked_images_under_mountpoint(sub, mountpoint);
    }
    dir.dirs.retain(|(_, sub)| sub.image_count() > 0);
}

/// Warns when the mount point is not empty: its files are hidden while mounted. Not
/// refused, as libfuse 3 and fusermount3 allow it (libfuse 3.0 dropped the `nonempty`
/// check), and a service's mount point can hold a placeholder; the cases that can hang are
/// refused above, precisely.
fn warn_if_not_empty(mountpoint: &Path) {
    if std::fs::read_dir(mountpoint).is_ok_and(|mut d| d.next().is_some()) {
        log::warn!("{} is not empty: its files are hidden while sacdfs is mounted", mountpoint.display());
    }
}

/// `path` (absolute) with its longest existing ancestor made canonical and the rest kept as
/// given: what `canonicalize` would give for a path that may not exist yet.
fn canonical_prefix(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(c) = at.canonicalize() {
            return rest.iter().rev().fold(c, |p, name| p.join(name));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                at = parent;
            }
            _ => return path.to_owned(),
        }
    }
}

fn with_path(e: io::Error, path: &std::path::Path) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

/// How long a new mount waits for an earlier FUSE mount on the same mount point to go away.
const EARLIER_MOUNT_WAIT: Duration = Duration::from_secs(10);

/// How long a probe of the mount point may take before its server counts as not answering.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// How often a mount being waited for is probed again (its server may die meanwhile).
const PROBE_EVERY: Duration = Duration::from_millis(250);

/// Waits until no FUSE file system is mounted on `at`, for up to [`EARLIER_MOUNT_WAIT`], and
/// clears a dead sacdfs mount of this user's at once.
///
/// A sacdfs restarted at the same path (by a service manager, a container, a script) can
/// start before the previous one has unmounted. Mounted on top, the new mount would sit
/// inside the old one: the old one's unmount (by path) would then remove the new mount, or
/// its detach (`MNT_DETACH`, which takes every mount below too) would take it along (#51).
///
/// A mount whose server has died (killed, crashed) never goes by itself, and every access
/// to it fails with ENOTCONN (#57). A bounded probe tells it apart from a live one: a dead
/// sacdfs mount of the same user is detached, as nothing can be using it; any other dead
/// mount, and another user's FUSE mount, fail at once with what is there, since waiting
/// cannot help. A live mount of this user's is waited for, as before.
fn wait_for_earlier_mount(at: &Path) -> io::Result<()> {
    let uid = ids().0;
    let start = Instant::now();
    let mut next_probe = start;
    let mut answers = true;
    let mut logged = false;
    loop {
        let Some(m) = top_mount(at).filter(MountEntry::is_fuse) else { return Ok(()) };
        if m.owner.is_some_and(|o| o != uid) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{}: {} is mounted there; unmount it first", at.display(), m.describe(uid)),
            ));
        }
        if Instant::now() >= next_probe {
            let found = probe(at, PROBE_TIMEOUT);
            log::debug!("{}: {} is mounted there; probe: {found:?}", at.display(), m.describe(uid));
            match found {
                Probe::Dead => {
                    detach_dead_mount(at, &m, uid)?;
                    continue;
                }
                Probe::Answers => answers = true,
                Probe::NoAnswer => answers = false,
            }
            next_probe = Instant::now() + PROBE_EVERY;
        }
        if !std::mem::replace(&mut logged, true) {
            log::info!(
                "{}: {} is mounted there: waiting for it to be unmounted",
                at.display(),
                m.describe(uid)
            );
        }
        if start.elapsed() >= EARLIER_MOUNT_WAIT {
            let how = if answers { "" } else { ", not answering," };
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{}: {}{how} is still mounted there after {} s (another sacdfs?); stop it or \
                     unmount it first (fusermount3 -u {})",
                    at.display(),
                    m.describe(uid),
                    EARLIER_MOUNT_WAIT.as_secs(),
                    at.display()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Detaches the dead mount `m` on `at` if it is a sacdfs mount of user `uid`'s; else fails
/// with what is there. Detached only while it is still the mount on top at `at`, as the
/// unmount goes by path: a mount put over it meanwhile is left alone (#51).
fn detach_dead_mount(at: &Path, m: &MountEntry, uid: u32) -> io::Result<()> {
    let dead = |hint: &str| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{}: {} is mounted there and its server has exited (\"Transport endpoint is not \
                 connected\"); {hint}",
                at.display(),
                m.describe(uid)
            ),
        )
    };
    if !m.is_sacdfs() || m.owner != Some(uid) {
        return Err(dead(&format!("unmount it first (fusermount3 -u {})", at.display())));
    }
    if top_mount(at).as_ref().map(|t| &t.dev) != Some(&m.dev) {
        // Gone or covered meanwhile: look again.
        return Ok(());
    }
    lazy_unmount(at).map_err(|e| dead(&format!("detaching it: {e}")))?;
    log::warn!("{}: detached a dead sacdfs mount (its sacdfs had exited without unmounting)", at.display());
    Ok(())
}

/// What asking a mount point's file system found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// It returned (with or without an error other than ENOTCONN): a live mount.
    Answers,
    /// ENOTCONN: the FUSE connection is gone, its server has exited.
    Dead,
    /// No answer within the timeout (a stopped or busy server), or the probe failed.
    NoAnswer,
}

/// Asks the file system on `at` something, with `statfs`, waiting at most `timeout`.
///
/// `statfs` rather than `stat`: the kernel answers a `stat` from its attribute cache
/// (sacdfs's attributes are valid for a day) without asking the server, so a dead mount
/// can still `stat` fine; a FUSE `statfs` always goes to the server. On a dead connection
/// it fails with ENOTCONN at once, but on a live one it waits for the server, which may
/// never answer (stopped, hung), and that wait cannot be interrupted. So it runs in a
/// child process, which is left behind if it does not finish in time (it is killed, and
/// exits once the server answers or dies). A child rather than a thread: this runs before
/// [`daemonize`] forks, which needs a process with a single thread.
#[allow(unsafe_code)]
fn probe(at: &Path, timeout: Duration) -> Probe {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(at.as_os_str().as_bytes()) else { return Probe::NoAnswer };
    // SAFETY: the process has a single thread here, and the child only calls stat and
    // _exit, both async-signal-safe.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        let mut st = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: a valid NUL-terminated path and a buffer for one `struct statfs`.
        let r = unsafe { libc::statfs(c.as_ptr(), st.as_mut_ptr()) };
        let code =
            if r == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ENOTCONN) { 0 } else { 1 };
        // SAFETY: ends the child at once, running nothing of the parent's.
        unsafe { libc::_exit(code) }
    }
    if pid < 0 {
        return Probe::NoAnswer;
    }
    let start = Instant::now();
    loop {
        let mut status = 0;
        // SAFETY: waits for the child just forked, without blocking.
        match unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } {
            0 if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(1)),
            p if p == pid && libc::WIFEXITED(status) => {
                return if libc::WEXITSTATUS(status) == 1 { Probe::Dead } else { Probe::Answers };
            }
            0 => {
                // SAFETY: signals the child, which has not been waited for, so its pid is
                // still its own.
                unsafe { libc::kill(pid, libc::SIGKILL) };
                return Probe::NoAnswer;
            }
            _ => return Probe::NoAnswer,
        }
    }
}

/// `path` made absolute and canonical without looking at `path` itself (only at its parent,
/// which is outside any mount on it). The mount point's own name is kept as given.
fn outside_path(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    match (abs.parent(), abs.file_name()) {
        (Some(parent), Some(name)) => parent.canonicalize().map_or_else(|_| abs.clone(), |p| p.join(name)),
        _ => abs,
    }
}

/// The device number (`major:minor`) of the mount on top at `at`, if `at` is a mount point.
fn top_mount_device(at: &Path) -> Option<String> {
    top_mount(at).map(|m| m.dev)
}

/// A mount, as `/proc/self/mountinfo` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MountEntry {
    /// The device number, `major:minor`: unique while the mount exists.
    dev: String,
    /// The file system type: `fuse.sacdfs` when mounted through fusermount3, `fuse` when
    /// mounted by root with mount(2).
    fstype: String,
    /// The source field (a FUSE mount's fsname).
    source: Vec<u8>,
    /// The user who mounted it, for a FUSE mount (its `user_id=` option).
    owner: Option<u32>,
}

impl MountEntry {
    /// Parses one line: its mount point and the mount.
    fn parse(line: &str) -> Option<(Vec<u8>, MountEntry)> {
        let mut f = line.split(' ');
        let (_, _, dev, _, mp) = (f.next()?, f.next()?, f.next()?, f.next()?, f.next()?);
        let mut tail = line.split(" - ").nth(1)?.split(' ');
        let (fstype, source, options) = (tail.next()?, tail.next()?, tail.next().unwrap_or(""));
        let owner = options.split(',').find_map(|o| o.strip_prefix("user_id=")).and_then(|u| u.parse().ok());
        let m = MountEntry {
            dev: dev.to_owned(),
            fstype: fstype.to_owned(),
            source: unescape_mountinfo(source),
            owner,
        };
        Some((unescape_mountinfo(mp), m))
    }

    fn is_fuse(&self) -> bool {
        self.fstype == "fuse" || self.fstype.starts_with("fuse.")
    }

    /// A sacdfs mount: of type `fuse.sacdfs`, or (mounted by root, without the subtype)
    /// `fuse` named [`FSNAME`].
    fn is_sacdfs(&self) -> bool {
        self.fstype == "fuse.sacdfs" || (self.fstype == "fuse" && self.source == FSNAME.as_bytes())
    }

    /// For messages, as seen by user `uid`: "a sacdfs mount of this user's", "a FUSE mount
    /// (fuse.sshfs) of another user's". User ids are kept out of messages and logs.
    fn describe(&self, uid: u32) -> String {
        let what = if self.is_sacdfs() {
            "a sacdfs mount".to_owned()
        } else {
            format!("a FUSE mount ({})", self.fstype)
        };
        match self.owner {
            Some(o) if o == uid => format!("{what} of this user's"),
            Some(_) => format!("{what} of another user's"),
            None => what,
        }
    }
}

/// The mount on top at `at`, from `/proc/self/mountinfo`: of the mounts on that path, the
/// last listed, which is the one mounted last, over the others. Reading the mount table
/// asks no file system anything, so it cannot block on a stopping or dead one.
fn top_mount(at: &Path) -> Option<MountEntry> {
    let info = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let want = at.as_os_str().as_encoded_bytes();
    info.lines().filter_map(MountEntry::parse).filter(|(mp, _)| mp == want).map(|(_, m)| m).next_back()
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
struct Notify(File, bool);

impl Notify {
    fn ok(mut self) {
        let _ = self.0.write_all(b"\0");
        redirect_stdio(self.1);
    }

    fn fail(mut self, e: &io::Error) {
        let _ = write!(self.0, "{e}");
    }
}

/// Forks. The parent waits until the child reports the mount, then exits; the child
/// continues in a new session.
#[allow(unsafe_code)]
fn daemonize(keep_stderr: bool) -> io::Result<Notify> {
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
            Ok(Notify(File::from(w), keep_stderr))
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

/// Points standard input, output and (unless `keep_stderr`) error at /dev/null.
#[allow(unsafe_code)]
fn redirect_stdio(keep_stderr: bool) {
    let Ok(null) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/null") else { return };
    for fd in 0..if keep_stderr { 2 } else { 3 } {
        // SAFETY: dup2 onto the standard descriptors; `null` stays open for the call.
        unsafe { libc::dup2(null.as_raw_fd(), fd) };
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const USER: &str = "123 45 0:61 / /home/u/mnt\\040x rw,nosuid,nodev,relatime shared:1 - fuse.sacdfs \
                        sacdfs ro,user_id=1000,group_id=1000,max_read=1048576";
    const ROOT: &str = "123 45 0:62 / /mnt rw,nosuid,nodev,noexec - fuse sacdfs ro,user_id=0,group_id=0";
    const OTHER: &str = "9 8 0:63 / /mnt rw - fuse.sshfs host:/x rw,user_id=1000,group_id=1000";
    const OLD_ROOT: &str = "9 8 0:64 / /mnt rw - fuse /music/a\\054b rw,user_id=0,group_id=0";

    #[test]
    fn mount_entries() {
        let (mp, m) = MountEntry::parse(USER).unwrap();
        assert_eq!(mp, b"/home/u/mnt x");
        assert_eq!(m.dev, "0:61");
        assert_eq!(m.owner, Some(1000));
        assert!(m.is_fuse() && m.is_sacdfs());
        assert_eq!(m.describe(1000), "a sacdfs mount of this user's");
        let (_, m) = MountEntry::parse(ROOT).unwrap();
        assert!(m.is_sacdfs());
        assert_eq!(m.owner, Some(0));
        let (_, m) = MountEntry::parse(OTHER).unwrap();
        assert!(m.is_fuse() && !m.is_sacdfs());
        assert_eq!(m.describe(0), "a FUSE mount (fuse.sshfs) of another user's");
        // A plain `fuse` mount with another name is not taken for sacdfs's.
        let (_, m) = MountEntry::parse(OLD_ROOT).unwrap();
        assert!(!m.is_sacdfs());
        let (_, m) = MountEntry::parse("1 0 8:1 / / rw - ext4 /dev/sda1 rw").unwrap();
        assert!(!m.is_fuse() && m.owner.is_none());
    }

    #[test]
    fn source_under_mountpoint() {
        let r = |s: &str, m: &str| refuse_source_under_mountpoint(Path::new(s), Path::new(m));
        assert!(r("/a/b", "/a/b").is_err());
        assert!(r("/a/b/c", "/a/b").is_err());
        assert!(r("/a/b/c.iso", "/a").is_err());
        assert!(r("/a/bc", "/a/b").is_ok());
        assert!(r("/a", "/a/b").is_ok());
        assert!(r("/x", "/a").is_ok());
        assert!(r("/a/b", "/a/b").unwrap_err().to_string().contains("is the mount point"));
    }

    #[test]
    fn images_under_mountpoint() {
        let leaf = SourceDir {
            path: "/s/m".into(),
            dirs: vec![],
            images: vec!["/s/m/x.iso".into()],
            sidecars: vec![],
        };
        let root = SourceDir {
            path: "/s".into(),
            dirs: vec![("m".into(), leaf)],
            images: vec!["/s/a.iso".into()],
            sidecars: vec![],
        };
        assert!(refuse_images_under_mountpoint(&root, Path::new("/s/m")).is_err());
        assert!(refuse_images_under_mountpoint(&root, Path::new("/s/n")).is_ok());
    }

    #[test]
    fn ready_reaches_the_notify_socket() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("notify");
        let sock = std::os::unix::net::UnixDatagram::bind(&p).unwrap();
        send_ready(p.as_os_str()).unwrap();
        let mut buf = [0u8; 16];
        let n = sock.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"READY=1");
        assert!(send_ready(d.path().join("none").as_os_str()).is_err());
    }

    #[test]
    fn stderr_kept_only_for_the_journal() {
        assert!(keeps_stderr(false, true));
        assert!(!keeps_stderr(true, true));
        assert!(!keeps_stderr(false, false));
    }

    #[test]
    fn log_file_is_appended_privately() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("log");
        writeln!(open_log_file(&p).unwrap(), "one").unwrap();
        writeln!(open_log_file(&p).unwrap(), "two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\ntwo\n");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(open_log_file(&d.path().join("no/such/log")).is_err());
    }

    #[test]
    fn symlinked_image_into_mountpoint_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().canonicalize().unwrap();
        let mnt = base.join("mnt");
        let src = base.join("src");
        std::fs::create_dir_all(src.join("A")).unwrap();
        std::fs::create_dir_all(&mnt).unwrap();
        std::fs::write(mnt.join("real.iso"), b"x").unwrap();
        std::fs::write(src.join("ok.iso"), b"x").unwrap();
        std::os::unix::fs::symlink(mnt.join("real.iso"), src.join("A/link.iso")).unwrap();
        let mut root = SourceDir::scan_with(&src, &Default::default(), Some(&mnt)).unwrap();
        assert_eq!(root.image_count(), 2);
        // The lexical check cannot see it ...
        assert!(refuse_images_under_mountpoint(&root, &mnt).is_ok());
        // ... the link is dropped, and so is the directory it emptied.
        skip_linked_images_under_mountpoint(&mut root, &mnt);
        assert_eq!(root.image_count(), 1);
        assert!(root.dirs.is_empty());
    }

    #[test]
    fn canonical_prefix_of_a_missing_path() {
        let d = tempfile::tempdir().unwrap();
        let c = d.path().canonicalize().unwrap();
        assert_eq!(canonical_prefix(&d.path().join("x/y")), c.join("x/y"));
        assert_eq!(canonical_prefix(d.path()), c);
    }

    #[test]
    fn probe_of_a_plain_directory_answers() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(probe(d.path(), PROBE_TIMEOUT), Probe::Answers);
        assert_eq!(probe(&d.path().join("missing"), PROBE_TIMEOUT), Probe::Answers);
    }
}
