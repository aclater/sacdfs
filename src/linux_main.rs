// SPDX-License-Identifier: GPL-3.0-or-later

//! The Linux FUSE mount, `sacdfs SOURCE MOUNTPOINT`: the options, the checks before mounting,
//! the daemon, signals and unmounting. `main.rs` calls [`main`] on Linux.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;

use crate::{mount_opts, startup, stop_signals};

use fuser::{Config, MountOption, Session, SessionACL};

use sacdfs::fuse::{MAX_READ, SacdFs};
use sacdfs::index_cache;
use sacdfs::serve::{self, with_path};
use sacdfs::tree::SourceDir;

/// Show the audio of SACD disc images as DSF and WAV files, decoded on demand.
///
/// SOURCE is an image file or a directory searched recursively for *.iso files. Each image
/// appears as MOUNTPOINT/<relative dir>/<image name>/{Stereo,Multichannel}/NN - title.dsf,
/// with NN - title.wav (24-bit PCM, converted on demand) next to it.
/// Unmount with `umount MOUNTPOINT` as root, else `fusermount3 -u MOUNTPOINT` (`fusermount -u`
/// with FUSE 2), or stop sacdfs with SIGINT, SIGTERM or SIGHUP (it exits after --stop-grace seconds even if
/// files are still open).
///
/// `sacdfs verify SOURCE [EXTRACTS]...` checks extracted DSF and DFF files against what
/// sacdfs serves, before they are deleted: see `sacdfs verify --help`. `sacdfs tags` keeps
/// user tags for the files (`--tags`): see `sacdfs tags --help`.
#[derive(Debug, Parser)]
#[command(version, about, after_help = serve::TUNING_HELP)]
struct Args {
    /// An SACD image file, or a directory of them.
    source: PathBuf,
    /// A directory to mount on, normally empty (its files are hidden while mounted); not
    /// SOURCE or a directory above it.
    mountpoint: PathBuf,
    /// Stay in the foreground and log to standard error.
    #[arg(short, long)]
    foreground: bool,
    #[command(flatten)]
    serve: serve::Options,
    /// Let other users read the mount: the default when run as root (the mount is read-only).
    /// A user other than root needs `user_allow_other` in /etc/fuse.conf for it.
    #[arg(long, value_name = "yes|no", num_args = 0..=1, require_equals = true,
          default_missing_value = "yes", value_parser = serve::parse_yes_no)]
    allow_other: Option<bool>,
    /// Same as `--allow-other=no`.
    #[arg(long, hide = true, conflicts_with = "allow_other")]
    no_allow_other: bool,
    #[command(flatten)]
    limits: serve::Limits,
    /// Seconds after SIGINT, SIGTERM or SIGHUP that sacdfs keeps serving files clients still hold
    /// open before it exits anyway (the mount is detached at once, so nothing new opens); those
    /// clients then get I/O errors. 0 waits for the last file to close, however long.
    #[arg(long, hide = true, default_value_t = DEFAULT_STOP_GRACE, value_name = "SECS")]
    stop_grace: u64,
    /// Append the log to FILE instead of standard error, at info level unless -v says more.
    /// Without it a background mount logs nowhere, except to the journal under systemd.
    #[arg(long, value_name = "FILE")]
    log_file: Option<PathBuf>,
    /// Log more (repeat for more still); RUST_LOG overrides it.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

/// The command line, for the test that keeps the `-o` option lists in step with it.
#[cfg(test)]
pub(crate) fn command() -> clap::Command {
    <Args as clap::CommandFactory>::command()
}

pub fn main() -> ExitCode {
    // `-o` options, as `mount -t fuse.sacdfs` and fstab pass them (#60).
    let argv = match mount_opts::expand(std::env::args_os().collect()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sacdfs: {}", sacdfs::safe_file::printable(&e.to_string()));
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
            Ok(mut f) => {
                // The file is appended to: a line marks where this session starts (#481). A
                // failed write is ignored, as the log's own writes are.
                let _ = sacdfs::errlog::write_session_start(&mut f, "sacdfs");
                logger.target(env_logger::Target::Pipe(Box::new(f)));
            }
            Err(e) => {
                eprintln!(
                    "sacdfs: --log-file {}: {}",
                    sacdfs::safe_file::shown(path),
                    sacdfs::safe_file::printable(&e.to_string())
                );
                return ExitCode::FAILURE;
            }
        }
    }
    // Names from SOURCE in the log are escaped where lines are written (#391).
    if let Err(e) = sacdfs::printable::install(logger.build()) {
        eprintln!("sacdfs: {}", sacdfs::safe_file::printable(&e.to_string()));
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        // A daemon that told the waiting parent, which printed it: once is enough (#282).
        Err(_) if REPORTED.load(Ordering::Relaxed) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("sacdfs: {}", sacdfs::safe_file::printable(&e.to_string()));
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
    // Before anything is opened: images and sidecars share this limit.
    let nofile = startup::raise_nofile();
    let pcm = args.serve.check()?;
    let source = args.source.canonicalize().map_err(|e| with_path(e, &args.source))?;
    // A FUSE mount still on the mount point (an earlier sacdfs that is stopping): mounting
    // over it would put this mount inside it, where its unmount takes this one along (#51).
    // Waited for before the mount point itself is looked at: a stat of a mount that is
    // going away can fail.
    wait_for_earlier_mount(&outside_path(&args.mountpoint))?;
    let mountpoint = args.mountpoint.canonicalize().map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => io::Error::new(e.kind(), startup::missing_mountpoint(&args.mountpoint)),
        _ => with_path(e, &args.mountpoint),
    })?;
    if !mountpoint.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{}: not a directory", mountpoint.display()),
        ));
    }
    // FUSE, the right to mount and the mount helper (#282): before the scan, which can take
    // minutes on a large library, and with a plain line for each thing that is missing.
    startup::check(&startup::Facts::gather()).map_err(io::Error::other)?;
    // Nothing sacdfs reads may lie under its own mount (#56): before the scan, which can
    // take a while on a large library.
    refuse_source_under_mountpoint(&source, &mountpoint)?;
    let cache_dir = args.limits.index_cache_dir(Some(&mountpoint))?;
    warn_if_not_empty(&mountpoint);
    // Before any thread allocates (#73), and so before the walk below, which reads the top-level
    // folders on several threads (#175): glibc gives each thread that allocates an arena of its
    // own, and what the walk allocates and frees stays in them.
    sacdfs::engine::limit_malloc_arenas();
    // Only names are read here; images are opened when their directory is first listed.
    let mut root = args.serve.scan(&source, Some(&mountpoint))?;
    refuse_images_under_mountpoint(&root, &mountpoint)?;
    skip_linked_images_under_mountpoint(&mut root, &mountpoint);
    args.serve.log_scan(&source, &root);
    // What the memory limit asks of the defaults (#231, #237). Said here, before the fork, so
    // that whoever started it sees it without a log file.
    let sizing = args.serve.sizing(&root);

    // Before daemonizing, which changes the working directory.
    if let Some(dir) = &cache_dir {
        serve::open_index_cache(dir);
    }

    // Become a daemon before any thread exists; the parent waits for the mount.
    let notify = if args.foreground {
        None
    } else {
        Some(daemonize(keeps_stderr(args.log_file.is_some(), std::env::var_os("JOURNAL_STREAM").is_some()))?)
    };
    // The signal handlers go in before mounting (#57), and before any thread exists, which
    // keeps a signal that arrives as they go in from being lost (#334): see stop_signals.rs.
    // A SIGINT, SIGTERM or SIGHUP from here on is held until the check below, before
    // mounting, or the signal thread, after: either way no dead mount is left behind.
    let mut signals = stop_signals::catch()?;
    let open_files = startup::open_files_budget(nofile, args.limits.max_open_images);
    let (tree, engine) = serve::build(
        &args.serve,
        serve::Parts {
            source: &source,
            root,
            pcm,
            sizing: &sizing,
            exclude: Some(&mountpoint),
            cache_dir: cache_dir.as_deref(),
            open_files,
        },
    )?;
    let (uid, gid) = ids();
    let tags_tree = tree.clone();
    let shedding = engine.clone();
    let fs = SacdFs::new(tree, engine, serve::request_threads(sizing.threads), uid, gid)?
        .with_request_timeout(args.limits.request_timeout())?;

    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::RO,
        // A fixed name, not the source path (#55): see FSNAME.
        MountOption::FSName(FSNAME.to_owned()),
        MountOption::Subtype("sacdfs".to_owned()),
        MountOption::NoDev,
        MountOption::NoSuid,
        MountOption::NoExec,
        // The kernel checks access from the modes itself (0555 folders, 0444 files, a
        // read-only mount: the same answers `access` gave), instead of asking sacdfs for
        // every access(2) and chdir(2). Samba changes directory for nearly every SMB request:
        // about one ACCESS per entry listed (#205).
        MountOption::DefaultPermissions,
        MountOption::CUSTOM(format!("max_read={MAX_READ}")),
    ];
    // Root lets other users read by default (#282): see docs/design.md, "Other users".
    if args.no_allow_other {
        log::warn!("--no-allow-other is deprecated: use --allow-other=no");
    }
    let refused = args.no_allow_other || args.allow_other == Some(false);
    if startup::allow_other(args.allow_other == Some(true), refused, uid) {
        log::info!("other users may read the mount (--allow-other=no turns it off)");
        config.acl = SessionACL::All;
    }
    config.n_threads = Some(4);
    // A signal since the handlers went in: not mounting. One from here on waits for the
    // signal thread below, which unmounts. With the handlers set up after mounting, as
    // before #57, a signal in between killed sacdfs and left a dead mount.
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
            log::debug!("mount: {e}");
            let why = startup::explain_mount_error(&e.to_string(), uid, startup::own_confinement())
                .unwrap_or_else(|| e.to_string());
            let e = io::Error::new(e.kind(), format!("cannot mount on {}: {why}", mountpoint.display()));
            if let Some(n) = notify {
                n.fail(&e);
            }
            return Err(e);
        }
    };
    // This mount's identity: its device number, read from the mount table (which asks the
    // file system nothing; a stat would wait for the session), plus its FUSE connection,
    // which tells whether the number is still this mount's (#54).
    let own = Arc::new(OwnMount {
        dev: top_mount_device(&mountpoint),
        conn: session.as_fd().try_clone_to_owned()?,
    });
    let at = mountpoint.clone();
    let mine = own.clone();
    let stop_tree = tags_tree.clone();
    let stop_grace = args.stop_grace;
    std::thread::Builder::new().name("sacdfs-signals".to_owned()).spawn(move || {
        if let Some(sig) = signals.forever().next() {
            // Unmounting goes by path, so only while the mount on top at the path is still
            // this one: once it was unmounted or detached, or something else was mounted
            // over it, the path names another mount, which must be left alone (#51).
            if !mine.is_on_top(&at) {
                log::warn!(
                    "signal {sig}: {} is no longer this mount (unmounted, or another is mounted \
                     over it): not unmounting it; sacdfs stops when its mount is gone",
                    at.display()
                );
            } else {
                log::info!("signal {sig}: unmounting");
                if let Err(e) = unmount(&at) {
                    // Busy (open files, or an NFS server exporting the mount): detach it, as
                    // fusermount3 does for users other than root. sacdfs then serves the files
                    // still open, for the grace period at most (#276).
                    if !mine.is_on_top(&at) {
                        log::warn!("unmount: {e}; {} is no longer this mount: not detaching", at.display());
                    } else {
                        match lazy_unmount(&at) {
                            Ok(()) => log::warn!("unmount: {e}; detached instead: files still open"),
                            Err(e2) => log::error!("unmount: {e}; detaching: {e2}"),
                        }
                    }
                }
            }
            if stop_grace == 0 {
                return;
            }
            // Past the grace period, exit even if clients still hold files open (#276): the
            // kernel then fails their reads. Only once the mount is gone from every place
            // this process can see it: exiting with it still mounted would leave it dead
            // ("Transport endpoint is not connected").
            std::thread::sleep(Duration::from_secs(stop_grace));
            if mine.mounted_anywhere() == Some(true) && mine.is_on_top(&at) {
                let _ = lazy_unmount(&at);
            }
            match mine.mounted_anywhere() {
                Some(false) => {
                    log::warn!(
                        "{stop_grace} s after the signal files are still open: exiting; \
                         their clients get I/O errors"
                    );
                    stop_tree.save_tag_watch();
                    if let Some(cache) = index_cache::global() {
                        let _ = cache.flush();
                    }
                    std::process::exit(0);
                }
                Some(true) => log::error!(
                    "{stop_grace} s after the signal the mount is still there and cannot be detached: \
                     not exiting, as that would leave a dead mount; unmount it by hand"
                ),
                None => log::error!("cannot read the mount table: not exiting after the grace period"),
            }
        }
    })?;
    let mut background =
        serve::start_background(&args.serve, sizing, &tags_tree, &shedding, cache_dir.as_deref())?;
    if let Some(n) = notify {
        n.ok();
    }
    sd_notify_ready();
    log::info!("mounted {} on {}", source.display(), mountpoint.display());
    let result = serve(session);
    let failed = result.as_ref().err().filter(|e| e.raw_os_error() != Some(libc::ECONNABORTED));
    if let Some(e) = failed.filter(|_| own.is_on_top(&mountpoint)) {
        // The session failed while still mounted (an error, not an unmount): don't leave a
        // dead mount behind.
        log::warn!("session ended: {e}; detaching {}", mountpoint.display());
        let _ = lazy_unmount(&mountpoint);
    }
    background.stop();
    match result {
        // The connection ended: the same end as the ENODEV that fuser takes for one (#322).
        // When a connection ends (an unmount, ours after a signal or one from outside such as
        // `fusermount3 -u` or `umount -l`), the kernel shuts its processing queue before its
        // input queue, and a read of /dev/fuse that takes a request still queued in between
        // (the RELEASE of a file just closed, often) fails with ECONNABORTED instead. Nothing
        // else returns it here: that is only for FUSE_ABORT_ERROR, which is not asked for.
        Err(e) if e.raw_os_error() == Some(libc::ECONNABORTED) => {
            log::debug!("connection ended after the unmount: {e}");
            Ok(())
        }
        r => r,
    }
}

/// The default of `--stop-grace`, in seconds. Shorter than every stop timeout shipped in
/// `deploy/` (systemd 30 s, `sacdfs-ctl` 30 s, the container's 30 s less its servers' 10 s).
const DEFAULT_STOP_GRACE: u64 = 10;

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
                     unmount it first: {}",
                    at.display(),
                    m.describe(uid),
                    EARLIER_MOUNT_WAIT.as_secs(),
                    startup::unmount_hint(at)
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
        return Err(dead(&format!("unmount it first: {}", startup::unmount_hint(at))));
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

/// The identity of sacdfs's own mount, to unmount by path only while the path is still it.
///
/// The device number alone is not enough (#54): the kernel hands a freed number to the next
/// mount at once, so after an unmount from outside (`fusermount3 -u`) a new mount on the
/// same path usually has the old one's number, and an old sacdfs that then got a signal
/// unmounted the new mount. The FUSE connection closes the gap: the kernel aborts it when
/// the mount's superblock goes, before the number is freed, so while the connection is
/// still up the number is this mount's and no other's.
struct OwnMount {
    /// The device number read from the mount table right after mounting.
    dev: Option<String>,
    /// A duplicate of the session's `/dev/fuse` descriptor.
    conn: OwnedFd,
}

impl OwnMount {
    /// Whether the mount on top at `at` is this one. The mount table is read first and the
    /// connection checked after: up then, the number read was this mount's.
    fn is_on_top(&self, at: &Path) -> bool {
        self.dev.is_some() && top_mount_device(at) == self.dev && fuse_connected(&self.conn)
    }
}

impl OwnMount {
    /// Whether this mount is still in the mount table, under any path (one mounted over it
    /// hides it, and it is still there). `None` if the table cannot be read or the device
    /// number is not known. A mount detached from the namespace is gone from it.
    fn mounted_anywhere(&self) -> Option<bool> {
        let dev = self.dev.as_deref()?;
        let info = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
        Some(info.lines().any(|l| l.split(' ').nth(2) == Some(dev)))
    }
}

/// Whether the FUSE connection of `/dev/fuse` descriptor `fd` is still up: the device
/// reports `POLLERR` once the connection is aborted (unmounted, or the mount went away).
/// `poll` with no wait asks the file system nothing.
#[allow(unsafe_code)]
fn fuse_connected(fd: &OwnedFd) -> bool {
    let mut p = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd, an open descriptor, no wait.
    let n = unsafe { libc::poll(&mut p, 1, 0) };
    n >= 0 && p.revents & (libc::POLLERR | libc::POLLNVAL) == 0
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

/// Unmounts the file system at `at`: `umount`, or `fusermount3 -u -z` (`fusermount -u -z` where
/// only FUSE 2's helper is installed) if not root (as fuser did; the setuid helper detaches, so a
/// busy mount is not refused).
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
    helper_detach(at)
}

/// The setuid helpers that unmount for users other than root, in the order fuser mounts with
/// them: FUSE 3's `fusermount3`, then FUSE 2's `fusermount`, which is all some systems have
/// (Synology DSM, for one). `FUSERMOUNT_PATH`, which fuser mounts with when it is set, comes
/// first.
fn fusermount_helpers() -> Vec<std::ffi::OsString> {
    let mut helpers: Vec<std::ffi::OsString> = std::env::var_os("FUSERMOUNT_PATH").into_iter().collect();
    helpers.extend(["fusermount3", "fusermount"].map(Into::into));
    helpers
}

/// Detaches the mount at `at` through the first setuid helper that exists (`-u -z`).
fn helper_detach(at: &std::path::Path) -> io::Result<()> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "neither fusermount3 nor fusermount found");
    for helper in fusermount_helpers() {
        match std::process::Command::new(&helper).args(["-u", "-z", "--"]).arg(at).status() {
            Ok(st) if st.success() => return Ok(()),
            Ok(st) => return Err(io::Error::other(format!("{} -u -z: {st}", helper.to_string_lossy()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
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
    helper_detach(at).map_err(|_| e)
}

#[allow(unsafe_code)]
fn ids() -> (u32, u32) {
    // SAFETY: getuid and getgid have no preconditions and cannot fail.
    unsafe { (libc::getuid(), libc::getgid()) }
}

/// Set once the daemon has handed its error to the waiting parent, which prints it.
static REPORTED: AtomicBool = AtomicBool::new(false);

/// The daemon side of the pipe to the waiting parent.
struct Notify(File, bool);

impl Notify {
    fn ok(mut self) {
        let _ = self.0.write_all(b"\0");
        redirect_stdio(self.1);
    }

    fn fail(mut self, e: &io::Error) {
        if write!(self.0, "{e}").is_ok() {
            REPORTED.store(true, Ordering::Relaxed);
        }
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
                    eprintln!("sacdfs: {}", sacdfs::safe_file::printable(&String::from_utf8_lossy(m)));
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
            rejected: None,
            stamp: Default::default(),
        };
        let root = SourceDir {
            path: "/s".into(),
            dirs: vec![("m".into(), leaf)],
            images: vec!["/s/a.iso".into()],
            sidecars: vec![],
            rejected: None,
            stamp: Default::default(),
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
        // Images are listed only if they hold the SACD master TOC signature (#175).
        let mut sacd = vec![0u8; 511 * 2048];
        sacd[510 * 2048..510 * 2048 + 8].copy_from_slice(b"SACDMTOC");
        std::fs::write(mnt.join("real.iso"), &sacd).unwrap();
        std::fs::write(src.join("ok.iso"), &sacd).unwrap();
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
    fn probe_of_a_plain_directory_answers() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(probe(d.path(), PROBE_TIMEOUT), Probe::Answers);
        assert_eq!(probe(&d.path().join("missing"), PROBE_TIMEOUT), Probe::Answers);
    }
}
