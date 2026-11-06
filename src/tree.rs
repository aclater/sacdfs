// SPDX-License-Identifier: GPL-3.0-or-later

//! The directory tree of the mount and its inode numbers.
//!
//! ```text
//! MOUNT/<relative dir of the image>/<image file stem>/{Stereo,Multichannel}/NN - title.{dsf,wav}
//! ```
//!
//! At mount time only the names of the source tree are walked, to find the directories
//! that hold `*.iso` files. A source directory's images are opened (their master and area
//! TOCs read) when the directory is first looked into; an area's files and their tags are
//! made when the area directory is first looked into.
//!
//! Inode numbers say what an entry is (see [`crate::ident`]): the same source tree gives the
//! same numbers on every mount. A number the tree has not made yet, such as one from an NFS
//! file handle kept across a restart, is found again from the index of names built at mount
//! ([`Ids`]): the directories on its way are loaded as a lookup of its path would load them.
//!
//! With [`Tree::with_flatten`], a chain of source directories below the top level whose only
//! content is one image is shown as its outermost directory holding the image's areas. The
//! chains are found from the names walked at mount time ([`SourceDir::single_image`]); the
//! image is still opened only when that directory is first looked into.
//!
//! Sidecar files ([`crate::sidecar`]: folder art, scans, playlists) in a directory that holds
//! images are listed after its images, at their relative path; with `--flatten`, those of
//! every directory of a collapsed chain are listed in the collapsed directory, outermost
//! first. Their names are found by the walk at mount time; they are looked at (`fstatat`)
//! when their directory is first listed, and opened when they are.

use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock, TryLockError, Weak};
use std::time::{Duration, Instant, SystemTime};

use rayon::prelude::*;
use scarletbook::audio::check_track;
use scarletbook::{MASTER_TOC_LSN, SectorFormat, Slot};

use crate::disc::{Disc, Tuning};
use crate::ident::{self, Ids};
use crate::names::{Deduper, track_file_name};
use crate::pcm::PcmConfig;
use crate::sidecar::{self, Meta, Sidecar, SidecarKind, Sidecars};
use crate::track::{Format, TrackFile};

/// Which files an area directory shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output {
    /// Show DSF files.
    pub dsf: bool,
    /// Show WAV files.
    pub wav: bool,
    /// Samples per channel per frame of the WAV files (from the PCM rate).
    pub samples_per_frame: u32,
    /// Serve each track with its pause (`--pauses kept`, #112), as extractors that keep the
    /// pauses write it. Off: the track's own range, the pause dropped.
    pub pauses_kept: bool,
}

impl Default for Output {
    /// DSF and WAV side by side, WAV at 176.4 kHz.
    fn default() -> Self {
        Output { dsf: true, wav: true, samples_per_frame: 2352, pauses_kept: false }
    }
}

impl Output {
    /// DSF files only.
    pub fn dsf_only() -> Self {
        Output { dsf: true, wav: false, ..Output::default() }
    }

    /// The formats shown, in the order a track's files are listed.
    pub fn formats(&self) -> Vec<Format> {
        let mut f = Vec::new();
        if self.dsf {
            f.push(Format::Dsf);
        }
        if self.wav {
            f.push(Format::Wav);
        }
        f
    }
}

/// Inode number of the root directory.
pub const ROOT: u64 = 1;

/// Name of the directory of the 2-channel area.
pub const STEREO: &str = "Stereo";

/// Name of the directory of the multichannel area.
pub const MULTICHANNEL: &str = "Multichannel";

/// How often a directory's source is looked at for changes, and so how long the kernel
/// remembers the names of such a directory (#101).
pub const REFRESH: Duration = Duration::from_secs(2);

/// Threads that read the top-level directories of the source at mount (#175).
const SCAN_THREADS: usize = 16;

/// What an image holds with its TOC open and none of its files made, in bytes (#237): the TOC
/// and text, the disc, the nodes of its areas.
const IMAGE_HELD: usize = 14 << 10;

/// Images opened at once when listing a source directory, under a bound (#237): one directory
/// can hold more images than the bound allows open.
const OPEN_BATCH: usize = 64;

/// Images kept listed whatever the bound: a client may be working through a few at a time.
const MIN_HELD: usize = 8;

/// Most nested source directories followed.
const MAX_DEPTH: usize = 64;

/// A file and its modification time when looked at.
pub type Watched = (PathBuf, Option<SystemTime>);

/// A directory of the source tree that holds images, directly or below.
#[derive(Debug, Clone)]
pub struct SourceDir {
    /// Its path.
    pub path: PathBuf,
    /// Subdirectories that hold images, sorted by name.
    pub dirs: Vec<(OsString, SourceDir)>,
    /// Image files directly in it, sorted by name.
    pub images: Vec<PathBuf>,
    /// Sidecar files and folders directly in it, sorted by name (none unless scanned with
    /// [`SourceDir::scan_with`]).
    pub sidecars: Vec<Sidecar>,
    /// `.iso` files in it, and in the subdirectories left out for holding no image, that are
    /// not SACD images, with their modification times when looked at (#175): watched, so
    /// that one written into place later brings its folder back.
    pub rejected: Option<Arc<Vec<Watched>>>,
    /// The modification times found in it and below when it was read (#123). Boxed: a
    /// directory node stays small.
    pub stamp: Box<Stamp>,
}

/// The modification times found in a source directory and below when it was read: what its
/// served directory's mtime is made of (#123). Scanners such as Roon skip a directory whose
/// mtime did not change, and everything below it, so a directory's mtime must move when
/// anything shown below it changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stamp {
    /// The directory's own.
    pub own: Option<SystemTime>,
    /// The newest of it, its sidecar files and folders, and its subdirectories, images aside.
    pub other: Option<SystemTime>,
    /// The newest of its images and those below (their files' mtimes are later, see
    /// [`ident::mtime`]).
    pub image: Option<SystemTime>,
}

impl Stamp {
    fn merge(&mut self, o: &Stamp) {
        self.other = self.other.max(o.other);
        self.image = self.image.max(o.image);
    }
}

/// What a scan looks for besides images.
struct Scan<'a> {
    sidecars: &'a Sidecars,
    /// MOUNTPOINT, when it is inside SOURCE: nothing in it is a sidecar.
    exclude: Option<&'a Path>,
    /// Re-reading a directory of a live mount (#101): never go into `exclude`, which would
    /// read through sacdfs's own mount.
    skip_exclude: bool,
}

impl SourceDir {
    /// Scans `source`: a single image file, or a directory searched recursively for
    /// `*.iso` files (any case). Symbolic links to files are followed; symbolic links to
    /// directories are not. Finds no sidecar files.
    pub fn scan(source: &Path) -> io::Result<SourceDir> {
        Self::scan_with(source, &Sidecars::none(), None)
    }

    /// [`SourceDir::scan`], also noting the `sidecars` of the directories that hold images
    /// (from their names: nothing more is read), except in `exclude` (the mount point). A
    /// single image file has none.
    pub fn scan_with(source: &Path, sidecars: &Sidecars, exclude: Option<&Path>) -> io::Result<SourceDir> {
        let meta = fs::metadata(source)?;
        if meta.is_file() {
            let parent = source.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            return Ok(SourceDir {
                path: parent,
                dirs: Vec::new(),
                images: vec![source.to_owned()],
                sidecars: Vec::new(),
                rejected: None,
                stamp: Box::default(),
            });
        }
        let scan = Scan { sidecars, exclude, skip_exclude: false };
        Ok(Self::scan_dir(source, 0, &scan, false, None)?.unwrap_or(SourceDir {
            path: source.to_owned(),
            dirs: Vec::new(),
            images: Vec::new(),
            sidecars: Vec::new(),
            rejected: None,
            stamp: Box::new(Stamp { own: mtime_of(source), other: mtime_of(source), image: None }),
        }))
    }

    /// [`SourceDir::scan_dir`] of each of `dirs` (name, path, whether a sidecar folder), in
    /// order; on up to [`SCAN_THREADS`] scoped threads if `parallel`.
    fn scan_subdirs(
        dirs: &[(OsString, PathBuf, bool)],
        depth: usize,
        scan: &Scan<'_>,
        parallel: bool,
    ) -> Vec<io::Result<Option<SourceDir>>> {
        let one =
            |(_, path, folder): &(OsString, PathBuf, bool)| Self::scan_dir(path, depth, scan, *folder, None);
        if !parallel || dirs.len() < 2 {
            return dirs.iter().map(one).collect();
        }
        let next = AtomicUsize::new(0);
        let slots: Vec<Mutex<Option<io::Result<Option<SourceDir>>>>> =
            dirs.iter().map(|_| Mutex::new(None)).collect();
        std::thread::scope(|s| {
            for _ in 0..SCAN_THREADS.min(dirs.len()) {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(d) = dirs.get(i) else { break };
                        *slots[i].lock().unwrap_or_else(|p| p.into_inner()) = Some(one(d));
                    }
                });
            }
        });
        slots
            .into_iter()
            .map(|m| m.into_inner().unwrap_or_else(|p| p.into_inner()).unwrap_or(Ok(None)))
            .collect()
    }

    /// Scans `dir`; `everything` inside a sidecar folder, whose every file and folder is a
    /// sidecar. `None` if it holds neither image nor sidecar.
    fn scan_dir(
        dir: &Path,
        depth: usize,
        scan: &Scan<'_>,
        everything: bool,
        known: Option<&SourceDir>,
    ) -> io::Result<Option<SourceDir>> {
        let mut dirs = Vec::new();
        let mut images = Vec::new();
        let mut sidecars = Vec::new();
        let mut rejected: Vec<Watched> = Vec::new();
        let mut todo = Vec::new();
        let own = mtime_of(dir);
        let mut stamp = Stamp { own, other: own, image: None };
        let note = !scan.sidecars.is_empty() && !scan.exclude.is_some_and(|m| dir.starts_with(m));
        let mut entries: Vec<_> = fs::read_dir(dir)?.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            let name = e.file_name();
            if ft.is_dir() {
                if depth + 1 >= MAX_DEPTH
                    || (scan.skip_exclude && scan.exclude.is_some_and(|m| path.starts_with(m)))
                {
                    continue;
                }
                // A directory already known keeps what was read of it (it re-reads itself
                // when it is listed).
                if let Some(k) = known {
                    if let Some((_, sub)) = k.dirs.iter().find(|(n, _)| *n == name) {
                        stamp.merge(&sub.stamp);
                        dirs.push((name, sub.clone()));
                        continue;
                    }
                    if let Some(f) =
                        k.sidecars.iter().find(|s| s.name == name && matches!(s.kind, SidecarKind::Folder(_)))
                    {
                        stamp.other = stamp.other.max(f.newest());
                        sidecars.push(f.clone());
                        continue;
                    }
                }
                let folder = note && !sidecar::hidden(&name) && (everything || scan.sidecars.folder(&name));
                todo.push((name, path, folder));
            } else if is_iso(&path) && (ft.is_file() || (ft.is_symlink() && path.is_file())) {
                // An image that is not an SACD (a DVD-Video or Blu-ray image, say) is not
                // a candidate: a folder with none is hidden (#175).
                if !may_be_sacd(&path) {
                    log::info!("{}: not an SACD image: not listed", path.display());
                    rejected.push((path.clone(), mtime_of(&path)));
                    continue;
                }
                stamp.image = stamp.image.max(mtime_of(&path));
                images.push(path);
            } else if note
                && (ft.is_file() || ft.is_symlink())
                && !sidecar::hidden(&name)
                && (everything || scan.sidecars.file(&name))
            {
                let kind = if ft.is_symlink() { SidecarKind::Link } else { SidecarKind::File };
                let mtime = mtime_of(&path);
                stamp.other = stamp.other.max(mtime);
                sidecars.push(Sidecar { name, path, kind, mtime });
            }
        }
        // The subdirectories left to read: those of the source's top level in parallel, each
        // image of them being a read of a network share that waits (#175). Scoped threads, not
        // rayon: the mount scans before it forks into the background, which needs a process
        // with one thread, and these are all joined before this returns.
        let found = Self::scan_subdirs(&todo, depth + 1, scan, depth == 0);
        for ((name, path, folder), result) in todo.into_iter().zip(found) {
            match result {
                Ok(Some(sub)) if sub.holds_images() => {
                    stamp.merge(&sub.stamp);
                    dirs.push((name, sub));
                }
                Ok(Some(sub)) if folder => {
                    stamp.other = stamp.other.max(sub.stamp.other);
                    rejected.extend(sub.rejected.iter().flat_map(|r| r.iter().cloned()));
                    let kind = SidecarKind::Folder(sub.sidecars);
                    sidecars.push(Sidecar { name, path, kind, mtime: sub.stamp.own });
                }
                // Left out, but its non-SACD images are watched (#175).
                Ok(Some(sub)) => rejected.extend(sub.rejected.iter().flat_map(|r| r.iter().cloned())),
                Ok(None) => {}
                Err(err) => log::warn!("{}: {err}", path.display()),
            }
        }
        // Added after the known ones and the files: sorted by name again.
        sidecars.sort_by(|a, b| a.name.cmp(&b.name));
        dirs.sort_by(|a, b| a.0.cmp(&b.0));
        if dirs.is_empty() && images.is_empty() && sidecars.is_empty() && rejected.is_empty() {
            return Ok(None);
        }
        Ok(Some(SourceDir {
            path: dir.to_owned(),
            dirs,
            images,
            sidecars,
            rejected: (!rejected.is_empty()).then(|| Arc::new(rejected)),
            stamp: Box::new(stamp),
        }))
    }

    /// This directory read again (#101): its images and sidecar files as they are now; the
    /// subdirectories already known are kept as they were read, new ones are scanned.
    fn reread(&self, sidecars: &Sidecars, exclude: Option<&Path>) -> io::Result<SourceDir> {
        let scan = Scan { sidecars, exclude, skip_exclude: true };
        let depth = 1;
        let mut out = Self::scan_dir(&self.path, depth, &scan, false, Some(self))?.unwrap_or(SourceDir {
            path: self.path.clone(),
            dirs: Vec::new(),
            images: Vec::new(),
            sidecars: Vec::new(),
            rejected: None,
            stamp: Box::new(Stamp { own: mtime_of(&self.path), other: mtime_of(&self.path), image: None }),
        });
        out.path.clone_from(&self.path);
        Ok(out)
    }

    /// What to watch for this directory's listing to change: its own modification time, and
    /// those of its subdirectories that are not shown (they hold no image yet: one that
    /// gains one changes nothing in this directory).
    fn watch(&self, exclude: Option<&Path>) -> Vec<(PathBuf, Option<SystemTime>)> {
        let mut out = vec![(self.path.clone(), mtime_of(&self.path))];
        // A non-SACD image written into place changes it, not its directory (#175).
        out.extend(self.rejected.iter().flat_map(|r| r.iter().cloned()));
        let Ok(rd) = fs::read_dir(&self.path) else { return out };
        for e in rd.filter_map(Result::ok) {
            let path = e.path();
            if !e.file_type().is_ok_and(|t| t.is_dir()) || exclude.is_some_and(|m| path.starts_with(m)) {
                continue;
            }
            let name = e.file_name();
            let shown = self.dirs.iter().any(|(n, _)| *n == name)
                || self.sidecars.iter().any(|s| s.name == name && matches!(s.kind, SidecarKind::Folder(_)));
            if !shown {
                let m = mtime_of(&path);
                out.push((path, m));
            }
        }
        out
    }

    /// [`SourceDir::reread`], also for each directory of a collapsed chain.
    fn reread_chain(&self, sidecars: &Sidecars, exclude: Option<&Path>) -> io::Result<SourceDir> {
        let mut out = self.reread(sidecars, exclude)?;
        if let ([_], []) = (out.dirs.as_slice(), out.images.as_slice()) {
            if let Some((name, sub)) = out.dirs.pop() {
                let sub = sub.reread_chain(sidecars, exclude).unwrap_or(sub);
                out.dirs.push((name, sub));
            }
        }
        Ok(out)
    }

    /// Whether a directory of this chain (`--flatten`), or this directory alone, changed
    /// since it was read.
    fn chain_changed(&self) -> bool {
        mtime_of(&self.path) != self.stamp.own
            || matches!((self.dirs.as_slice(), self.images.as_slice()), ([(_, sub)], []) if sub.chain_changed())
    }

    /// The directories of a collapsed chain, outermost first.
    fn chain_dirs(&self) -> Vec<PathBuf> {
        let mut out = vec![self.path.clone()];
        if let ([(_, sub)], []) = (self.dirs.as_slice(), self.images.as_slice()) {
            out.extend(sub.chain_dirs());
        }
        out
    }

    /// The rejected images of a collapsed chain ([`SourceDir::chain_dirs`]).
    fn chain_rejected(&self) -> Vec<Watched> {
        let mut out: Vec<Watched> = self.rejected.iter().flat_map(|r| r.iter().cloned()).collect();
        if let ([(_, sub)], []) = (self.dirs.as_slice(), self.images.as_slice()) {
            out.extend(sub.chain_rejected());
        }
        out
    }

    /// Whether it holds an image, directly or below.
    fn holds_images(&self) -> bool {
        !self.dirs.is_empty() || !self.images.is_empty()
    }

    /// The sidecars of a chain collapsed by `--flatten` ([`SourceDir::single_image`]): this
    /// directory's, then those of each directory down the chain.
    fn chain_sidecars(&self) -> Vec<Sidecar> {
        let mut out = self.sidecars.clone();
        if let ([(_, sub)], []) = (self.dirs.as_slice(), self.images.as_slice()) {
            out.extend(sub.chain_sidecars());
        }
        out
    }

    /// The one image of this directory and its subdirectories, if that is all they hold:
    /// either the directory holds one image and no subdirectory, or no image and one
    /// subdirectory of which this is true. Decided from the names alone (the image may
    /// still turn out not to be an SACD image).
    pub fn single_image(&self) -> Option<&Path> {
        match (self.dirs.as_slice(), self.images.as_slice()) {
            ([], [image]) => Some(image),
            ([(_, sub)], []) => sub.single_image(),
            _ => None,
        }
    }

    /// Number of images in this directory and below.
    pub fn image_count(&self) -> usize {
        self.images.len() + self.dirs.iter().map(|(_, d)| d.image_count()).sum::<usize>()
    }
}

/// The modification time of `path` (a link's target's), if it can be read.
fn mtime_of(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// What a look at each image found (SACD or not), with its size and time then: a rescan of
/// its folder reads nothing of it until one changes (#175). Without it every periodic rescan
/// would read each image again and keep a sleeping disk awake. One small entry per image
/// seen; a removed image's stays until the process ends.
/// Size, time and answer of the look at each image path.
type Looks = HashMap<PathBuf, (u64, Option<SystemTime>, bool)>;

static LOOKED_AT: Mutex<Option<Looks>> = Mutex::new(None);

#[cfg(test)]
thread_local! {
    /// Images read by [`may_be_sacd`] on this thread.
    static PROBES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Whether the image at `path` can be an SACD: it holds the master TOC signature where any
/// sector format puts it (what opening the image checks first, with one small read per
/// format tried). *Why here and not when the folder is listed:* the parents of a folder
/// are listed before it, and must not show it if nothing below it is an SACD (#175). An image
/// that cannot be read now counts as a candidate: opening it later reports why. An answer is kept
/// while the image's size and time stay the same ([`LOOKED_AT`]).
fn may_be_sacd(path: &Path) -> bool {
    use std::os::unix::fs::FileExt;
    let Ok(meta) = fs::metadata(path) else { return true };
    let (len, mtime) = (meta.len(), meta.modified().ok());
    let with_map = |f: &mut dyn FnMut(&mut Looks)| {
        f(LOOKED_AT.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(HashMap::new));
    };
    let mut known = None;
    with_map(&mut |m| known = m.get(path).copied());
    if let Some((l, t, answer)) = known {
        if (l, t) == (len, mtime) {
            return answer;
        }
    }
    #[cfg(test)]
    PROBES.with(|c| c.set(c.get() + 1));
    let Ok(file) = fs::File::open(path) else { return true };
    let mut sig = [0u8; 8];
    let mut answer = false;
    for format in SectorFormat::DETECTION_ORDER {
        let at = u64::from(MASTER_TOC_LSN) * format.stored_size() + format.data_offset();
        if at + 8 > len {
            continue;
        }
        match file.read_exact_at(&mut sig, at) {
            Ok(()) if &sig == b"SACDMTOC" => {
                answer = true;
                break;
            }
            Ok(()) => {}
            Err(_) => return true,
        }
    }
    with_map(&mut |m| {
        m.insert(path.to_owned(), (len, mtime, answer));
    });
    answer
}

fn is_iso(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso"))
}

/// What an image's directory and its areas show below them (#123): its track files and
/// the folder art of its directory.
fn image_stamp(image: SystemTime, art: Option<&Art>) -> Stamp {
    Stamp { own: None, other: art.and_then(Art::newest), image: Some(image) }
}

/// Whether the source of directory `d`, not listed yet, changed since what it would be
/// listed from was read: one `stat` (per directory of a `--flatten` chain).
fn changed_since_read(d: &Dir) -> bool {
    match &d.kind {
        DirKind::Source(s) => mtime_of(&s.path) != s.stamp.own,
        DirKind::Flat(_, top) => top.chain_changed(),
        DirKind::Folder(p, _) => mtime_of(p) != d.stamp.own,
        DirKind::Image(_) | DirKind::Closed(_) | DirKind::Area(..) => false,
    }
}

/// Whether a directory is re-read when its source changes (on a watching tree).
fn live(d: &Dir) -> bool {
    match d.kind {
        DirKind::Source(_) | DirKind::Flat(..) | DirKind::Folder(..) => true,
        // An area's files follow its folder art and its disc's tags file (#81).
        DirKind::Area(..) => d.art.is_some() || crate::tags::installed(),
        // Never re-read, but its mtime moves with the tags file: kept by the kernel no
        // longer than a re-read directory's.
        DirKind::Image(_) | DirKind::Closed(_) => crate::tags::installed(),
    }
}

/// What a directory shows.
#[derive(Debug)]
pub enum DirKind {
    /// A directory of the source tree.
    Source(SourceDir),
    /// The directory of one image: one subdirectory per area.
    Image(Arc<Disc>),
    /// The directory of an image whose disc was let go of to keep within the bound on listed
    /// images (#237): it is opened again when the directory is next looked into, and then held
    /// by its areas, as [`DirKind::Flat`]'s is.
    Closed(PathBuf),
    /// A chain of source directories collapsed onto its one image (`--flatten`): shows the
    /// image's areas, like [`DirKind::Image`], once the image is opened on first use, and the
    /// sidecars of the chain's directories.
    Flat(PathBuf, SourceDir),
    /// A sidecar folder passed through (`Artwork/`): shows its content.
    Folder(PathBuf, Vec<Sidecar>),
    /// The directory of one area: one file per track.
    Area(Arc<Disc>, usize),
}

/// A directory entry.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The name.
    pub name: OsString,
    /// The inode number.
    pub ino: u64,
    /// Whether it is a directory.
    pub is_dir: bool,
}

/// A directory node.
#[derive(Debug)]
pub struct Dir {
    /// What it shows.
    pub kind: DirKind,
    /// Modification time shown.
    pub mtime: SystemTime,
    /// Inode number of the parent directory (the root's is itself).
    pub parent: u64,
    /// Folder art shown in an area (#95).
    art: Option<Arc<Art>>,
    /// What is shown below it, for its mtime (#123). Boxed: a node stays small.
    stamp: Box<Stamp>,
    /// The modification time of the source directory when it was last re-read, in
    /// nanoseconds since the epoch; 0 until then (`mtime` is shown).
    shown: AtomicU64,
    /// Set when the directory is no longer listed anywhere and its nodes are being freed
    /// (#153): it lists nothing from then on.
    gone: AtomicBool,
    /// Set when the image is let go of (#237): this node is out of the tree, and a request
    /// holding it asks the tree again, which makes the directory anew.
    closed: AtomicBool,
    children: Mutex<Option<Box<Listing>>>,
}

impl Dir {
    fn new(kind: DirKind, mtime: SystemTime, parent: u64) -> Dir {
        let stamp = Box::default();
        Dir {
            kind,
            mtime,
            parent,
            art: None,
            stamp,
            shown: AtomicU64::new(0),
            gone: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            children: Mutex::new(None),
        }
    }

    fn with_stamp(mut self, stamp: Stamp) -> Dir {
        *self.stamp = stamp;
        self
    }

    /// The source directory whose changes, and those below it, move its mtime (#123).
    fn key(&self) -> Option<&Path> {
        match &self.kind {
            DirKind::Source(d) | DirKind::Flat(_, d) => Some(&d.path),
            DirKind::Folder(p, _) | DirKind::Closed(p) => Some(p),
            // An image's own directories move with its tags file (#81), noted under the
            // image's path (`Tree::bump`).
            DirKind::Image(disc) | DirKind::Area(disc, _) => Some(&disc.path),
        }
    }

    fn with_art(mut self, art: Option<Arc<Art>>) -> Dir {
        self.art = art;
        self
    }
}

/// What a directory listing was made from, to tell when it is out of date (#101).
#[derive(Debug)]
struct Listing {
    entries: Arc<Vec<Entry>>,
    /// Source directories the listing depends on, with their modification time then.
    watch: Vec<(PathBuf, Option<SystemTime>)>,
    /// When `watch` was last compared.
    checked: Instant,
    /// What was read of the source directory then, for the next re-read.
    snapshot: Option<Snapshot>,
    /// The version of the folder art listed in an area.
    art_version: u64,
    /// With `--flatten`: how many leading entries are the image's own (kept on a re-read).
    head: usize,
    /// The directory's mtime as shown when the listing was made or last compared (#123).
    served: SystemTime,
    /// An area's disc's tags file key and what that file was like when listed (#81).
    tags: Option<(String, crate::tags::FileState)>,
}

#[derive(Debug)]
enum Snapshot {
    /// A source directory, or the top of a collapsed chain.
    Dir(SourceDir),
    /// The content of a sidecar folder.
    Folder(Vec<Sidecar>),
}

/// The art file of `art` (listed, by `listed`) to show also as `cover.<ext>`, and that name,
/// for the servers that look only for files so named (MPD, #148); `None` if the art has a
/// `cover.*` already or no listed file. The choice is `folder.*`, then `front.*`, then the
/// first by name; the extension is in lower case, `jpeg` as `jpg` (what MPD looks for).
fn cover_alias(art: &[Sidecar], listed: impl Fn(&Sidecar) -> bool) -> Option<(&Sidecar, String)> {
    let stem = |sc: &Sidecar| {
        let n = sc.name.to_string_lossy().to_ascii_lowercase();
        n.rsplit_once('.').map(|(s, e)| (s.to_owned(), e.to_owned()))
    };
    if art.iter().any(|sc| stem(sc).is_some_and(|(s, _)| s == "cover")) {
        return None;
    }
    let rank = |sc: &Sidecar| match stem(sc).map(|(s, _)| s).as_deref() {
        Some("folder") => 0,
        Some("front") => 1,
        _ => 2,
    };
    let pick = art.iter().filter(|sc| listed(sc)).min_by_key(|sc| rank(sc))?;
    let (_, ext) = stem(pick)?;
    let ext = if ext == "jpeg" { "jpg".to_owned() } else { ext };
    Some((pick, format!("cover.{ext}")))
}

/// What a listing being made depends on.
#[derive(Default)]
struct Cx {
    watch: Vec<(PathBuf, Option<SystemTime>)>,
    snapshot: Option<Snapshot>,
    art_version: u64,
    head: usize,
    tags: Option<(String, crate::tags::FileState)>,
    /// A closed image could not be opened again: the listing is not kept (#237).
    unreadable: bool,
}

/// The folder art of the directory (or chain of directories) of an image: the image files
/// among its sidecars, shown inside each area of the image as well (#95). The directory is
/// re-read when its modification time changes, like a listing (#101); `version` counts
/// the changes, so that an area knows when its listing is out of date.
#[derive(Debug)]
pub struct Art {
    dirs: Vec<PathBuf>,
    state: Mutex<ArtState>,
}

#[derive(Debug)]
struct ArtState {
    items: Arc<Vec<Sidecar>>,
    watch: Vec<(PathBuf, Option<SystemTime>)>,
    checked: Instant,
    version: u64,
}

impl Art {
    /// The newest modification time of the art as last read.
    fn newest(&self) -> Option<SystemTime> {
        let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.items.iter().filter_map(|s| s.mtime).max()
    }

    fn new(dirs: Vec<PathBuf>, items: Vec<Sidecar>) -> Arc<Art> {
        let watch = dirs.iter().map(|d| (d.clone(), mtime_of(d))).collect();
        Arc::new(Art {
            dirs,
            state: Mutex::new(ArtState {
                items: Arc::new(items),
                watch,
                checked: Instant::now(),
                version: 0,
            }),
        })
    }

    /// The art now, and its version. Compares modification times at most once per
    /// `refresh`; `watching` is false for a mount that never re-reads.
    fn current(&self, tree: &Tree) -> (Arc<Vec<Sidecar>>, u64) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if tree.watching && st.checked.elapsed() >= tree.refresh {
            st.checked = Instant::now();
            if st.watch.iter().any(|(p, m)| mtime_of(p) != *m) {
                let watch: Vec<_> = self.dirs.iter().map(|d| (d.clone(), mtime_of(d))).collect();
                let exclude = tree.exclude_abs();
                let mut items = Vec::new();
                for d in &self.dirs {
                    if exclude.as_deref().is_some_and(|m| d.starts_with(m)) {
                        continue;
                    }
                    let Ok(rd) = fs::read_dir(d) else { continue };
                    let mut found: Vec<Sidecar> = rd
                        .filter_map(Result::ok)
                        .filter_map(|e| {
                            let ft = e.file_type().ok()?;
                            let name = e.file_name();
                            let kind = if ft.is_file() {
                                SidecarKind::File
                            } else if ft.is_symlink() {
                                SidecarKind::Link
                            } else {
                                return None;
                            };
                            (!sidecar::hidden(&name) && tree.sel.file(&name)).then(|| Sidecar {
                                mtime: mtime_of(&e.path()),
                                path: e.path(),
                                name,
                                kind,
                            })
                        })
                        .filter(Sidecar::is_art)
                        .collect();
                    found.sort_by(|a, b| a.name.cmp(&b.name));
                    items.extend(found);
                }
                st.items = Arc::new(items);
                st.watch = watch;
                st.version += 1;
            }
        }
        (st.items.clone(), st.version)
    }
}

/// A node of the tree.
#[derive(Debug)]
pub enum Node {
    /// A directory. Boxed: most nodes are files, and each would be as big as a directory (#237).
    Dir(Box<Dir>),
    /// A track file.
    File(Arc<TrackFile>),
    /// A sidecar file passed through from SOURCE.
    Sidecar(Arc<SidecarFile>),
}

/// A sidecar file passed through: its bytes are its source file's.
#[derive(Debug)]
pub struct SidecarFile {
    /// The file whose bytes it shows, relative to SOURCE: its own path, or for a link the
    /// path it names.
    pub target: PathBuf,
    /// Its attributes when its directory was listed: shown as they are.
    pub meta: Meta,
    /// Its generation ([`ident::sidecar_generation`]).
    pub generation: u64,
}

/// The tree: every node made so far, by inode number.
#[derive(Debug)]
pub struct Tree {
    nodes: RwLock<HashMap<u64, Arc<Node>>>,
    ids: Ids,
    next_disc: AtomicU32,
    tuning: Tuning,
    output: Output,
    flatten: bool,
    /// The flavour of the WAV files (PCM options and rate), part of their numbers.
    wav_flavour: u64,
    /// Seconds added to the WAV files' mtime for the PCM options (issue #64).
    wav_mtime_offset: u64,
    /// SOURCE, opened when a sidecar is first looked at.
    root_fd: OnceLock<Option<OwnedFd>>,
    /// MOUNTPOINT relative to SOURCE, when it is inside it: no sidecar is served from there.
    exclude: Option<PathBuf>,
    /// Whether directories are re-read when their source changes (#101).
    watching: bool,
    /// The sidecars selection, for re-reading.
    sel: Sidecars,
    /// How often a directory's source is looked at for changes.
    refresh: Duration,
    /// What changed below each source directory since it was read (#123), by path.
    bumps: RwLock<HashMap<PathBuf, Stamp>>,
    /// Every source directory and sidecar folder, looked at for changes (#123).
    sweep: Mutex<Option<Sweep>>,
    /// Each opened image with its tags file key and what that file was like when last
    /// looked at (#81), for the sweep.
    tagged: Mutex<crate::tags::Watch>,
    /// The bound on what listed images hold, in bytes (#237); `usize::MAX` for none.
    budget: usize,
    /// The bound in force now: less under memory pressure.
    budget_now: AtomicUsize,
    /// Short of memory: nothing new is listed or opened (#237).
    refusing: AtomicBool,
    /// The images held, with no bound: nothing.
    held: Mutex<Held>,
    /// Images let go of since the persistent seek index last let go of its own.
    released: AtomicUsize,
}

/// The images that hold their discs and files in memory, under a bound (#237), each looked at in
/// turn when memory is needed and let go of unless used since the last look (the clock
/// algorithm). *Why not strict least-recently-used:* a request that is answered from memory, on a
/// fuser thread, then only sets a flag.
#[derive(Debug, Default)]
struct Held {
    images: HashMap<u64, HeldImage>,
    queue: VecDeque<u64>,
    bytes: usize,
}

#[derive(Debug)]
struct HeldImage {
    disc: Weak<Disc>,
    bytes: usize,
}

/// `m`'s content, if it is free now (poisoned or not).
fn try_lock<T>(m: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match m.try_lock() {
        Ok(g) => Some(g),
        Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

/// The directories [`Tree::sweep`] looks at: each with its modification time when last read
/// and whether it is a sidecar folder.
#[derive(Debug)]
struct Sweep {
    checked: Option<Instant>,
    dirs: HashMap<PathBuf, (Option<SystemTime>, bool)>,
}

impl Sweep {
    /// Adds `d` and everything below it that is not there yet.
    fn add(&mut self, d: &SourceDir) {
        self.dirs.entry(d.path.clone()).or_insert((d.stamp.own, false));
        for (_, sub) in &d.dirs {
            self.add(sub);
        }
        self.add_folders(&d.sidecars);
    }

    fn add_folders(&mut self, list: &[Sidecar]) {
        for sc in list {
            if let SidecarKind::Folder(inner) = &sc.kind {
                self.dirs.entry(sc.path.clone()).or_insert((sc.mtime, true));
                self.add_folders(inner);
            }
        }
    }
}

impl Tree {
    /// A tree with `root` at its root, showing DSF and WAV files.
    pub fn new(root: SourceDir) -> Tree {
        Self::with_options(root, Tuning::default(), Output::default())
    }

    /// A tree showing DSF files only, whose discs search for frames with `tuning`.
    pub fn with_tuning(root: SourceDir, tuning: Tuning) -> Tree {
        Self::with_options(root, tuning, Output::dsf_only())
    }

    /// A tree whose discs search for frames with `tuning`, showing the files of `output`.
    pub fn with_options(root: SourceDir, tuning: Tuning, output: Output) -> Tree {
        let mtime = fs::metadata(&root.path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
        let stamp = *root.stamp;
        // Hashes every name walked once (no file is opened), so that numbers made later can
        // be found again from their key.
        let ids = Ids::new(&root);
        let node = Node::Dir(Box::new(Dir::new(DirKind::Source(root), mtime, ROOT).with_stamp(stamp)));
        let wav_flavour =
            ident::wav_flavour(ident::pcm_flavour(&PcmConfig::default()), output.samples_per_frame);
        Tree {
            nodes: RwLock::new(HashMap::from([(ROOT, Arc::new(node))])),
            ids,
            next_disc: AtomicU32::new(1),
            tuning,
            output,
            flatten: false,
            wav_flavour,
            wav_mtime_offset: 0,
            root_fd: OnceLock::new(),
            exclude: None,
            watching: false,
            sel: Sidecars::none(),
            refresh: REFRESH,
            bumps: RwLock::new(HashMap::new()),
            sweep: Mutex::new(None),
            tagged: Mutex::new(crate::tags::Watch::default()),
            budget: usize::MAX,
            budget_now: AtomicUsize::new(usize::MAX),
            refusing: AtomicBool::new(false),
            held: Mutex::new(Held::default()),
            released: AtomicUsize::new(0),
        }
    }

    /// The same tree, keeping what listed images hold to about `budget` bytes (#237; `None`: no
    /// bound). Past it, the images not used lately are let go of: their discs, areas and files.
    /// Numbers come from paths, so what was shown is made again as it was the next time
    /// anything of them is asked for, at the cost of reading the image's TOC again. An image
    /// with a file open is kept.
    #[must_use]
    pub fn with_listing_budget(mut self, budget: Option<usize>) -> Tree {
        self.budget = budget.unwrap_or(usize::MAX);
        self.budget_now.store(self.budget, Ordering::Relaxed);
        self
    }

    /// Acts on memory pressure ([`crate::memory::Pressure`]) as far as the tree goes: under any,
    /// listings are kept to a quarter of their budget, now and until it ends, and at the last
    /// level nothing new is listed.
    pub fn set_pressure(&self, pressure: crate::memory::Pressure) {
        use crate::memory::Pressure;
        self.refusing.store(pressure == Pressure::Refuse, Ordering::Relaxed);
        let budget = if pressure == Pressure::None { self.budget } else { self.budget / 4 };
        self.budget_now.store(budget, Ordering::Relaxed);
        self.keep_to_budget();
    }

    /// Whether requests for anything not in memory are being refused for lack of it.
    pub fn refusing(&self) -> bool {
        self.refusing.load(Ordering::Relaxed)
    }

    fn bounded(&self) -> bool {
        self.budget != usize::MAX
    }

    /// Notes that `node` was asked for, under a bound.
    fn touch(&self, node: &Node) {
        if !self.bounded() {
            return;
        }
        let disc = match node {
            Node::File(f) => &f.disc,
            Node::Dir(d) => match &d.kind {
                DirKind::Image(disc) | DirKind::Area(disc, _) => disc,
                _ => return,
            },
            Node::Sidecar(_) => return,
        };
        disc.referenced.store(true, Ordering::Relaxed);
    }

    /// Notes under a bound that `disc`, the image `ino`'s, is now held in memory.
    fn hold(&self, ino: u64, disc: &Arc<Disc>) {
        if !self.bounded() {
            return;
        }
        let mut h = self.held.lock().unwrap_or_else(|p| p.into_inner());
        h.images.insert(ino, HeldImage { disc: Arc::downgrade(disc), bytes: IMAGE_HELD });
        h.queue.push_back(ino);
        h.bytes += IMAGE_HELD;
    }

    /// Notes that the image `ino` holds `bytes` more.
    fn charge(&self, ino: u64, bytes: usize) {
        if !self.bounded() {
            return;
        }
        let mut h = self.held.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(i) = h.images.get_mut(&ino) {
            i.bytes += bytes;
            h.bytes += bytes;
        }
    }

    /// Bytes counted as held by listed images (for tests and diagnostics).
    pub fn held_bytes(&self) -> usize {
        self.held.lock().unwrap_or_else(|p| p.into_inner()).bytes
    }

    /// Images held in memory, under a bound (for tests and diagnostics).
    pub fn held_images(&self) -> usize {
        self.held.lock().unwrap_or_else(|p| p.into_inner()).images.len()
    }

    /// Lets go of images until what they hold is within the bound.
    fn keep_to_budget(&self) {
        if !self.bounded() {
            return;
        }
        // Another thread at it is enough.
        let Some(mut h) = try_lock(&self.held) else { return };
        // Twice round at most: the first look only clears the flags.
        let mut looks = h.queue.len() * 2;
        let mut released = 0;
        let budget = self.budget_now.load(Ordering::Relaxed);
        while h.bytes > budget && h.images.len() > MIN_HELD && looks > 0 {
            looks -= 1;
            let Some(ino) = h.queue.pop_front() else { break };
            let Some(image) = h.images.get(&ino) else { continue };
            let disc = image.disc.upgrade();
            if disc.as_ref().is_some_and(|d| d.referenced.swap(false, Ordering::Relaxed)) {
                h.queue.push_back(ino);
                continue;
            }
            drop(disc);
            if self.let_go(ino) {
                if let Some(i) = h.images.remove(&ino) {
                    h.bytes -= i.bytes;
                }
                released += 1;
            } else {
                h.queue.push_back(ino);
            }
        }
        drop(h);
        if released > 0 && self.released.fetch_add(released, Ordering::Relaxed) + released >= OPEN_BATCH {
            self.released.store(0, Ordering::Relaxed);
            if let Some(cache) = crate::index_cache::global() {
                cache.release_retired();
            }
        }
    }

    /// Lets go of what image `ino` holds: its disc, its areas and files. `false` if a file of
    /// it is open, or one of its directories is being listed, which keep it for now.
    ///
    /// *Why this is safe for the kernel:* as for a freed node ([`Tree::forget`]), numbers come
    /// from paths, a number no longer in the tree is looked for along its path, and that makes
    /// the directories and files again with the same names, numbers and generations while the
    /// image is unchanged. Open files hold their own nodes. Nothing waits for a lock here
    /// but the tree's own, so that a stalled listing holds nothing up.
    fn let_go(&self, ino: u64) -> bool {
        let Some(node) = self.loaded(ino) else { return true };
        let Node::Dir(top) = &*node else { return true };
        let path = match &top.kind {
            DirKind::Image(disc) => Some(disc.path.clone()),
            DirKind::Closed(_) | DirKind::Flat(..) => None,
            _ => return true,
        };
        let Some(mut top_guard) = try_lock(&top.children) else { return false };
        let areas: Vec<(u64, Arc<Node>)> = top_guard
            .iter()
            .flat_map(|l| l.entries.iter())
            .filter(|e| e.is_dir)
            .filter_map(|e| Some((e.ino, self.loaded(e.ino)?)))
            .filter(|(_, n)| matches!(&**n, Node::Dir(d) if matches!(d.kind, DirKind::Area(..))))
            .collect();
        let mut guards = Vec::with_capacity(areas.len());
        for (_, area) in &areas {
            let Node::Dir(d) = &**area else { continue };
            let Some(g) = try_lock(&d.children) else { return false };
            guards.push((d, g));
        }
        let mut files = Vec::new();
        for (_, g) in &guards {
            for e in g.iter().flat_map(|l| l.entries.iter()) {
                let Some(file) = self.loaded(e.ino) else { continue };
                if let Node::File(f) = &*file {
                    // A reader holds it (the node's is the first).
                    if Arc::strong_count(f) > 1 {
                        return false;
                    }
                    files.push(e.ino);
                }
            }
        }
        let closed = path.map(|p| {
            let mut d = Dir::new(DirKind::Closed(p), top.mtime, top.parent)
                .with_art(top.art.clone())
                .with_stamp(*top.stamp);
            d.shown = AtomicU64::new(top.shown.load(Ordering::Relaxed));
            Arc::new(Node::Dir(Box::new(d)))
        });
        let mut freed = Vec::with_capacity(files.len() + areas.len());
        {
            let mut nodes = self.nodes.write().unwrap_or_else(|p| p.into_inner());
            for f in &files {
                freed.extend(nodes.remove(f));
            }
            for (i, _) in &areas {
                freed.extend(nodes.remove(i));
            }
            if let Some(c) = closed {
                top.closed.store(true, Ordering::Release);
                freed.extend(nodes.insert(ino, c));
            }
        }
        for (d, g) in &mut guards {
            d.closed.store(true, Ordering::Release);
            drop(g.take());
        }
        let listing = top_guard.take();
        drop(guards);
        drop(top_guard);
        drop(listing);
        drop(freed);
        true
    }

    /// The same tree, re-reading a directory when its source directory changed since it
    /// was last read (#101): new and removed images and sidecar files (those `sel` passes)
    /// show up without a remount. Not for a single image file as SOURCE.
    #[must_use]
    pub fn watching(mut self, sel: &Sidecars) -> Tree {
        self.watching = true;
        self.sel = sel.clone();
        self
    }

    /// The same tree, looking at a source directory for changes at most once per `every`
    /// (default [`REFRESH`]).
    #[must_use]
    pub fn with_refresh(mut self, every: Duration) -> Tree {
        self.refresh = every;
        self
    }

    /// How long the kernel may keep what it was told about a directory that is re-read
    /// (a name that is there or not): [`REFRESH`], else `None`.
    pub fn live_ttl(&self, parent: u64) -> Option<Duration> {
        self.is_live(parent).then_some(self.refresh.max(Duration::from_secs(1)))
    }

    /// Whether directory `ino` is re-read when its source changes.
    pub fn is_live(&self, ino: u64) -> bool {
        self.watching
            && matches!(self.nodes.read().unwrap_or_else(|p| p.into_inner()).get(&ino).map(|n| &**n),
            Some(Node::Dir(d)) if live(d))
    }

    /// MOUNTPOINT as a path, when it is inside SOURCE.
    fn exclude_abs(&self) -> Option<PathBuf> {
        self.exclude.as_ref().map(|r| self.source().join(r))
    }

    /// The same tree, serving no sidecar from `mountpoint` if it is inside SOURCE: a link
    /// into it would make sacdfs read through its own mount (#67).
    #[must_use]
    pub fn excluding(mut self, mountpoint: &Path) -> Tree {
        self.exclude = mountpoint.strip_prefix(self.ids.source()).ok().map(Path::to_path_buf);
        self
    }

    /// SOURCE's directory, opened on first use.
    fn root_fd(&self) -> Option<&OwnedFd> {
        self.root_fd
            .get_or_init(|| match sidecar::open_root(self.source()) {
                Ok(fd) => Some(fd),
                Err(e) => {
                    log::warn!("{}: {e}: no sidecar files", self.source().display());
                    None
                }
            })
            .as_ref()
    }

    /// Opens a sidecar file for reading. `ESTALE` if its source file changed (size or mtime)
    /// since it was listed: its attributes, already shown, would no longer match its bytes.
    pub fn open_sidecar(&self, f: &SidecarFile) -> io::Result<fs::File> {
        let root = self.root_fd().ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
        let (file, now) = sidecar::open_file(root, &f.target)?;
        if now.size != f.meta.size || now.mtime != f.meta.mtime {
            return Err(io::Error::from_raw_os_error(libc::ESTALE));
        }
        Ok(file)
    }

    /// The same tree, its WAV files converted with `pcm` (whose rate must be the one of the
    /// output's samples per frame): the PCM options are part of the WAV files' inode and
    /// generation numbers, and of their mtime. Without it, the default options are assumed.
    #[must_use]
    pub fn with_pcm(mut self, pcm: &PcmConfig) -> Tree {
        self.wav_flavour = ident::wav_flavour(ident::pcm_flavour(pcm), self.output.samples_per_frame);
        self.wav_mtime_offset = ident::pcm_mtime_offset(pcm);
        self
    }

    /// The same tree, with chains of directories below the top level that hold a single
    /// image collapsed into their outermost directory when `flatten` is set (see the
    /// module documentation). Off by default.
    #[must_use]
    pub fn with_flatten(mut self, flatten: bool) -> Tree {
        self.flatten = flatten;
        self
    }

    /// The node with inode number `ino`. A number not made yet in this mount (one from an
    /// NFS file handle kept across a restart) is looked for along its path, which loads the
    /// directories on the way as looking up that path would.
    pub fn get(&self, ino: u64) -> Option<Arc<Node>> {
        if let Some(n) = self.nodes.read().unwrap_or_else(|p| p.into_inner()).get(&ino) {
            self.touch(n);
            return Some(n.clone());
        }
        self.find(ino)
    }

    /// The node with inode number `ino`, if it is made already: never reads the source
    /// (unlike [`Tree::get`]), so it can answer on fuser's own threads (#63).
    pub fn loaded(&self, ino: u64) -> Option<Arc<Node>> {
        let nodes = self.nodes.read().unwrap_or_else(|p| p.into_inner());
        let node = nodes.get(&ino)?;
        self.touch(node);
        Some(node.clone())
    }

    /// Finds `ino` from the index: walks from the root through the directories its key's
    /// path goes through (matching inode numbers, not names, so that `--flatten` and
    /// de-duplicated names do not matter), then into the image's area and file.
    fn find(&self, ino: u64) -> Option<Arc<Node>> {
        let mut chain = self.ids.chain(ino)?;
        // A sidecar file is found in its directory's listing.
        let sidecar = self.ids.is_sidecar(ino);
        if sidecar {
            chain.pop();
        }
        let mut cur = ROOT;
        let mut rest = chain.as_slice();
        while !rest.is_empty() {
            let children = self.children(cur)?;
            // The deepest directory of the chain listed here: with --flatten, a collapsed
            // directory carries its image's number, skipping the directories in between.
            let (at, e) = children.iter().filter(|e| e.is_dir).find_map(|e| {
                rest.iter()
                    .position(|&c| c == e.ino)
                    .or_else(|| self.collapsed_over(e.ino, rest))
                    .map(|j| (j, e.ino))
            })?;
            cur = e;
            rest = &rest[at + 1..];
        }
        if sidecar {
            if !self.children(cur)?.iter().any(|e| e.ino == ino) {
                return None;
            }
        } else if ident::entry_of(ino) != 0 {
            // An area, or a file of the area with the same key and slot.
            let area = ident::area_of(ino)?;
            if !self.children(cur)?.iter().any(|e| e.ino == area) {
                return None;
            }
            if area != ino && !self.children(area)?.iter().any(|e| e.ino == ino) {
                return None;
            }
        } else if cur != ino {
            return None;
        }
        self.nodes.read().unwrap_or_else(|p| p.into_inner()).get(&ino).cloned()
    }

    /// With `--flatten`, a collapsed directory (numbered as its image) stands for the source
    /// directories of its chain, whose sidecars it shows: the index in `rest` of the last of
    /// them that `rest` starts with, if it starts with any.
    fn collapsed_over(&self, ino: u64, rest: &[u64]) -> Option<usize> {
        if !self.flatten || ident::entry_of(ino) != 0 {
            return None;
        }
        let chain = self.ids.chain(ino)?;
        rest.iter().take_while(|r| chain.contains(r)).count().checked_sub(1)
    }

    /// Number of nodes made so far.
    pub fn len(&self) -> usize {
        self.nodes.read().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Whether the tree has only its root (never true: the root always exists).
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Adds `node` as `ino`; `false` (and the node is dropped) if that number is taken,
    /// which only a hostile image (two areas in one slot) can cause.
    fn add(&self, ino: u64, node: Node) -> bool {
        let mut nodes = self.nodes.write().unwrap_or_else(|p| p.into_inner());
        match nodes.entry(ino) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(Arc::new(node));
                true
            }
        }
    }

    /// The entries of directory `ino`, made on first use, and again when its source changed
    /// since (on a [watching](Tree::watching) tree). `None` if it is not a directory.
    pub fn children(&self, ino: u64) -> Option<Arc<Vec<Entry>>> {
        let node = self.get(ino)?;
        let Node::Dir(dir) = &*node else { return None };
        let mut guard = dir.children.lock().unwrap_or_else(|p| p.into_inner());
        if dir.gone.load(Ordering::Acquire) {
            return None;
        }
        if dir.closed.load(Ordering::Acquire) {
            // Its image was let go of while this waited (#237): this node is out of the tree.
            drop(guard);
            return self.children(ino);
        }
        let mut prev = guard.take();
        if let Some(l) = prev.as_mut() {
            if !self.stale(dir, l) {
                let entries = l.entries.clone();
                *guard = prev;
                return Some(entries);
            }
        }
        if self.refusing() {
            // No memory for a new listing: what was listed is still good enough.
            let old = prev.as_ref().map(|l| l.entries.clone());
            *guard = prev;
            return old;
        }
        let mut cx = Cx::default();
        let entries = Arc::new(self.load(dir, ino, prev.as_deref(), &mut cx));
        if dir.gone.load(Ordering::Acquire) {
            // Removed while it was being listed: what the listing made is freed here, as
            // the removal could not take it (#153).
            let old = prev.iter().flat_map(|p| p.entries.iter());
            self.forget(dir, entries.iter().chain(old).map(|e| e.ino));
            return None;
        }
        if cx.unreadable {
            return None;
        }
        if let Some(p) = &prev {
            // What the source no longer has is freed (#153): numbers stay what they were,
            // since they come from paths, and open files hold what they read.
            let kept: std::collections::HashSet<u64> = entries.iter().map(|e| e.ino).collect();
            self.forget(dir, p.entries.iter().map(|e| e.ino).filter(|i| !kept.contains(i)));
        }
        if prev.is_some() {
            // What the directory shows as its modification time follows its source.
            let newest = cx.watch.iter().filter_map(|(_, m)| *m).max();
            if let Some(n) = newest.and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok()) {
                dir.shown.store(u64::try_from(n.as_nanos()).unwrap_or(0).max(1), Ordering::Relaxed);
            }
        }
        *guard = Some(Box::new(Listing {
            entries: entries.clone(),
            watch: cx.watch,
            checked: Instant::now(),
            snapshot: cx.snapshot.or_else(|| prev.and_then(|p| p.snapshot)),
            art_version: cx.art_version,
            head: cx.head,
            served: self.dir_mtime(dir),
            tags: cx.tags,
        }));
        drop(guard);
        self.keep_to_budget();
        Some(entries)
    }

    /// Frees the nodes `inos`, entries that directory `dir` no longer lists, and every node
    /// below them (#153): an image removed from SOURCE or renamed, a sidecar removed. Their
    /// discs go with the last node or open file that holds them.
    ///
    /// *Why this is safe for the kernel:* inode numbers are not handed out but made from
    /// paths ([`crate::ident`]), so a freed number is never given to anything else; the
    /// same path coming back gets the same number, with its generation. A number the
    /// kernel still holds is answered as an unknown one (looked for along its path, so
    /// `ENOENT` once the path is gone), and an open file keeps reading from its handle,
    /// which holds its own node. So no lookup count is kept, and nothing waits for `forget`.
    ///
    /// The folder art an area lists is listed by its folder too, under the same numbers, and
    /// is freed from there. A directory being listed while it is removed (its lock is held)
    /// is marked gone, and frees what that listing made itself. Called with `dir`'s lock
    /// held: nothing lists it again meanwhile, and so nothing remakes a node being freed.
    fn forget(&self, dir: &Dir, inos: impl Iterator<Item = u64>) {
        let area = matches!(dir.kind, DirKind::Area(..));
        let mut stack: Vec<(u64, bool)> = inos.map(|i| (i, area)).collect();
        if stack.is_empty() {
            return;
        }
        let mut freed: Vec<(u64, Arc<Node>)> = Vec::new();
        while let Some((ino, in_area)) = stack.pop() {
            let Some(node) = self.loaded(ino) else { continue };
            match &*node {
                Node::Sidecar(_) if in_area => continue,
                Node::Dir(d) => {
                    d.gone.store(true, Ordering::Release);
                    let listing = match d.children.try_lock() {
                        Ok(mut g) => g.take(),
                        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().take(),
                        // Being listed (maybe on a stalled source): that listing frees its
                        // own entries when done, so none is waited for here.
                        Err(std::sync::TryLockError::WouldBlock) => None,
                    };
                    if let Some(l) = listing {
                        let sub = matches!(d.kind, DirKind::Area(..));
                        stack.extend(l.entries.iter().map(|e| (e.ino, sub)));
                    }
                }
                _ => {}
            }
            freed.push((ino, node));
        }
        {
            let mut nodes = self.nodes.write().unwrap_or_else(|p| p.into_inner());
            for (ino, node) in &freed {
                if nodes.get(ino).is_some_and(|n| Arc::ptr_eq(n, node)) {
                    nodes.remove(ino);
                }
            }
        }
        if self.bounded() {
            let mut h = self.held.lock().unwrap_or_else(|p| p.into_inner());
            for (ino, _) in &freed {
                if let Some(i) = h.images.remove(ino) {
                    h.bytes -= i.bytes;
                }
            }
        }
        // What the tree notes by source path for them goes too, for the paths that are gone:
        // the mtimes passed up (#123) and the tags files watched (#81).
        let mut paths: Vec<&Path> = Vec::new();
        let mut images: Vec<&Path> = Vec::new();
        for (_, node) in &freed {
            let Node::Dir(d) = &**node else { continue };
            match &d.kind {
                DirKind::Source(s) => paths.push(&s.path),
                DirKind::Folder(p, _) => paths.push(p),
                DirKind::Closed(image) => {
                    paths.push(image);
                    images.push(image);
                }
                DirKind::Image(disc) => {
                    paths.push(&disc.path);
                    images.push(&disc.path);
                }
                DirKind::Flat(image, s) => {
                    paths.push(&s.path);
                    paths.push(image);
                    images.push(image);
                }
                DirKind::Area(..) => {}
            }
        }
        let gone = |p: &&Path| fs::symlink_metadata(p).is_err();
        paths.retain(gone);
        images.retain(gone);
        if !paths.is_empty() {
            let mut b = self.bumps.write().unwrap_or_else(|p| p.into_inner());
            for p in &paths {
                b.remove(*p);
            }
        }
        if !images.is_empty() && crate::tags::installed() {
            let mut w = self.tagged.lock().unwrap_or_else(|p| p.into_inner());
            for i in &images {
                w.forget(i);
            }
        }
        // The discs are dropped here, outside the lock (each closes its image handle).
        drop(paths);
        drop(images);
        drop(freed);
    }

    /// Whether `l`, the listing of `dir`, is out of date: its source changed. Looks at the
    /// source at most once per refresh interval.
    fn stale(&self, dir: &Dir, l: &mut Listing) -> bool {
        if !self.watching || !live(dir) {
            return false;
        }
        if let DirKind::Area(..) = &dir.kind {
            let art = dir.art.as_ref().is_some_and(|a| a.current(self).1 != l.art_version);
            // The tags file (#81): one `stat` per refresh interval, or now if the area's
            // mtime moved (the sweep found the edit).
            let served = self.dir_mtime(dir);
            let tags = match &l.tags {
                Some((key, was)) if l.checked.elapsed() >= self.refresh || served != l.served => {
                    l.checked = Instant::now();
                    l.served = served;
                    crate::tags::state_of(key) != *was
                }
                _ => false,
            };
            return art || tags;
        }
        // A directory whose mtime moved (the sweep found a change below it, #123) is compared
        // now, whatever the interval: a client that keeps a listing until its directory's
        // mtime moves (SMB clients do) would otherwise keep the old listing under the new
        // mtime for good.
        let served = self.dir_mtime(dir);
        if l.checked.elapsed() < self.refresh && served == l.served {
            return false;
        }
        l.checked = Instant::now();
        l.served = served;
        l.watch.iter().any(|(p, m)| mtime_of(p) != *m)
    }

    /// Whether node `ino` exists already.
    fn exists(&self, ino: u64) -> bool {
        self.nodes.read().unwrap_or_else(|p| p.into_inner()).contains_key(&ino)
    }

    /// The parent directory of `ino` (the root for files and unknown nodes; files are
    /// never asked for their parent).
    pub fn parent_of(&self, ino: u64) -> u64 {
        match self.get(ino).as_deref() {
            Some(Node::Dir(d)) => d.parent,
            _ => ROOT,
        }
    }

    /// The generation number of `node`: 0 for a directory; for a track file, a hash of what
    /// its bytes depend on (see [`crate::ident`]); for a sidecar file, of its source file's
    /// size and mtime.
    pub fn generation(&self, node: &Node) -> u64 {
        match node {
            Node::Dir(_) => 0,
            Node::File(f) => ident::generation(f, self.wav_flavour),
            Node::Sidecar(s) => s.generation,
        }
    }

    /// The modification time shown for `node`: a directory's own; for a track file, its
    /// image's, moved by the output version and PCM options (see [`ident::mtime`]); for a
    /// sidecar file, its source file's.
    pub fn mtime(&self, node: &Node) -> SystemTime {
        match node {
            Node::Dir(d) => self.dir_mtime(d),
            Node::File(f) => ident::mtime(f, self.wav_mtime_offset),
            Node::Sidecar(s) => s.meta.mtime,
        }
    }

    /// The modification time shown for directory `d` (see [`Tree::mtime`]).
    fn dir_mtime(&self, d: &Dir) -> SystemTime {
        let own = match d.shown.load(Ordering::Relaxed) {
            0 => d.mtime,
            n => SystemTime::UNIX_EPOCH + Duration::from_nanos(n),
        };
        let mut stamp = *d.stamp;
        let bump = d.key().and_then(|k| self.bumps.read().unwrap_or_else(|p| p.into_inner()).get(k).copied());
        if let Some(b) = bump {
            stamp.merge(&b);
        }
        own.max(self.served(&stamp))
    }

    /// The newest mtime shown below a directory whose content is `stamp` (#123): of its
    /// sidecars and source directories, and of its images' files, whose mtimes are their
    /// images' moved by the output version and PCM options ([`ident::newest_offset`]).
    ///
    /// With an image below, the sidecars' and source directories' times move one step too
    /// ([`ident::FILLED_STEP`], #185). *Why:* a folder newer than its images' files would
    /// otherwise keep its time when a file below it moves, and a scanner that prunes unchanged
    /// folders would never reach that file.
    fn served(&self, stamp: &Stamp) -> SystemTime {
        let off = ident::newest_offset(self.output.dsf, self.output.wav, self.wav_mtime_offset);
        match stamp.image {
            Some(i) => stamp.other.map(|o| o + ident::FILLED_STEP).max(Some(i + off)),
            None => stamp.other,
        }
        .unwrap_or(SystemTime::UNIX_EPOCH)
    }

    /// Notes that `stamp` was found in source directory `path` (#123): it and each of its
    /// ancestors up to SOURCE show an mtime at least as new from now on. Only ever moves
    /// forward, so what was shown stays shown until a remount reads everything again.
    fn bump(&self, path: &Path, stamp: Stamp) {
        let src = self.source();
        let mut b = self.bumps.write().unwrap_or_else(|p| p.into_inner());
        for a in path.ancestors().take_while(|a| a.starts_with(src)) {
            b.entry(a.to_owned()).or_default().merge(&stamp);
        }
    }

    /// The same tree, watching the tags files of its images (#81, see
    /// [`crate::tags::Watch`]), kept across mounts in `file` (`None`: not kept). Every
    /// directory above an image whose disc has a tags file shows an mtime no older than it
    /// from the start. Nothing without a tags directory.
    #[must_use]
    pub fn with_tag_watch(self, file: Option<PathBuf>) -> Tree {
        if !crate::tags::installed() {
            return self;
        }
        let mut images = Vec::new();
        let root = self.get(ROOT);
        if let Some(DirKind::Source(top)) = root.as_deref().and_then(|n| match n {
            Node::Dir(d) => Some(&d.kind),
            _ => None,
        }) {
            let mut stack = vec![top];
            while let Some(d) = stack.pop() {
                images.extend(d.images.iter().cloned());
                stack.extend(d.dirs.iter().map(|(_, s)| s));
            }
        }
        let mut w = crate::tags::Watch::load(file, images.iter().map(PathBuf::as_path));
        for (image, t) in w.check(true) {
            self.bump(&image, Stamp { own: None, other: Some(t), image: None });
        }
        w.save();
        *self.tagged.lock().unwrap_or_else(|p| p.into_inner()) = w;
        self
    }

    /// Writes what is known of the tags files, if it changed (at unmount).
    pub fn save_tag_watch(&self) {
        self.tagged.lock().unwrap_or_else(|p| p.into_inner()).save();
    }

    /// Notes `disc`, just opened, for the tag layer (#81): its tags file is looked at by
    /// [`Tree::sweep`], and if it has one, the image's directories and every directory above
    /// show an mtime no older than it (its files' are, see [`ident::mtime`]).
    fn opened(&self, disc: &Disc) {
        if !crate::tags::installed() {
            return;
        }
        let t = self.tagged.lock().unwrap_or_else(|p| p.into_inner()).opened(&disc.path, disc.toc());
        if let Some(t) = t {
            self.bump(&disc.path, Stamp { own: None, other: Some(t), image: None });
        }
    }

    /// Looks at the tags files of the images (#81, [`crate::tags::Watch::check`]): one
    /// added, edited or removed moves the mtime of its image's directories and of every
    /// directory above, as a new sidecar does (#123), so that a scanner that skips unchanged
    /// folders finds the files whose tags changed. Their areas list them again
    /// (`Tree::stale`).
    fn sweep_tags(&self) {
        if !crate::tags::installed() {
            return;
        }
        let mut w = self.tagged.lock().unwrap_or_else(|p| p.into_inner());
        for (image, t) in w.check(false) {
            self.bump(&image, Stamp { own: None, other: Some(t), image: None });
        }
        w.save();
    }

    /// Looks, at most once per refresh interval, at the modification time of every source
    /// directory and sidecar folder (one `stat` each), so that a change deep in SOURCE
    /// moves the mtime of every directory above it (#123) without any of them being listed:
    /// a scanner that skips unchanged directories from the top then finds it. A changed
    /// directory is read again (its own entries, and any new subdirectory in full) and its
    /// stamp passed up (`Tree::bump`). Called when the root's attributes are asked for,
    /// which a scan does first. Nothing on a mount that does not re-read (#101).
    pub fn sweep(&self) {
        if !self.watching {
            return;
        }
        let Some(root) = self.get(ROOT) else { return };
        let Node::Dir(top) = &*root else { return };
        let DirKind::Source(top) = &top.kind else { return };
        // A sweep running already (maybe held up by a stalled source) is not waited for: the
        // root's attributes are answered as they are, and the sweep moves them when done
        // (#63).
        let mut guard = match self.sweep.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        let st = guard.get_or_insert_with(|| {
            let mut s = Sweep { checked: None, dirs: HashMap::new() };
            s.add(top);
            s
        });
        if st.checked.is_some_and(|c| c.elapsed() < self.refresh) {
            return;
        }
        st.checked = Some(Instant::now());
        self.sweep_tags();
        let changed: Vec<(PathBuf, bool)> = st
            .dirs
            .iter()
            .filter(|(p, (m, _))| mtime_of(p) != *m)
            .map(|(p, (_, folder))| (p.clone(), *folder))
            .collect();
        if changed.is_empty() {
            return;
        }
        let exclude = self.exclude_abs();
        let src = self.source().to_owned();
        for (path, folder) in changed {
            let Some(now) = mtime_of(&path) else {
                st.dirs.retain(|p, _| !p.starts_with(&path));
                continue;
            };
            // Its subdirectories already looked at are not read again: their own changes
            // are found on their own.
            let mut known = SourceDir {
                path: path.clone(),
                dirs: Vec::new(),
                images: Vec::new(),
                sidecars: Vec::new(),
                rejected: None,
                stamp: Box::default(),
            };
            for (p, (_, f)) in &st.dirs {
                let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else { continue };
                if parent != path {
                    continue;
                }
                let stub = SourceDir {
                    path: p.clone(),
                    dirs: Vec::new(),
                    images: Vec::new(),
                    sidecars: Vec::new(),
                    rejected: None,
                    stamp: Box::default(),
                };
                if *f {
                    let kind = SidecarKind::Folder(Vec::new());
                    known.sidecars.push(Sidecar {
                        name: name.to_owned(),
                        path: p.clone(),
                        kind,
                        mtime: None,
                    });
                } else {
                    known.dirs.push((name.to_owned(), stub));
                }
            }
            let depth = path.strip_prefix(&src).map_or(0, |r| r.components().count());
            let scan = Scan { sidecars: &self.sel, exclude: exclude.as_deref(), skip_exclude: true };
            match SourceDir::scan_dir(&path, depth, &scan, folder, Some(&known)) {
                Ok(found) => {
                    let stamp = match &found {
                        Some(d) => {
                            st.add(d);
                            *d.stamp
                        }
                        None => Stamp { own: Some(now), other: Some(now), image: None },
                    };
                    st.dirs.insert(path.clone(), (Some(now), folder));
                    self.bump(&path, stamp);
                }
                Err(e) => log::warn!("{}: {e}: not looked at", path.display()),
            }
        }
    }

    /// The source path: the SOURCE directory, or the directory of a single image.
    pub fn source(&self) -> &Path {
        self.ids.source()
    }

    /// Looks up `name` in directory `parent`.
    pub fn lookup(&self, parent: u64, name: &OsStr) -> Option<u64> {
        self.children(parent)?.iter().find(|e| e.name == name).map(|e| e.ino)
    }

    /// Resolves a relative path of names from the root.
    pub fn resolve(&self, path: &Path) -> Option<u64> {
        let mut ino = ROOT;
        for part in path.components() {
            ino = self.lookup(ino, part.as_os_str())?;
        }
        Some(ino)
    }

    /// Makes the entries of `dir` (numbered `parent`), recording in `cx` what they depend on.
    /// `prev` is the listing it replaces, if any: what is already there is kept, so that
    /// unchanged entries keep their nodes and numbers.
    fn load(&self, dir: &Dir, parent: u64, prev: Option<&Listing>, cx: &mut Cx) -> Vec<Entry> {
        let mut dedup = Deduper::with_reserved([".", ".."]);
        let mut out = Vec::new();
        let exclude = self.exclude_abs();
        // Only a watching tree re-reads, and only on a later listing.
        // A directory listed for the first time is read again too if its source changed
        // since what it would be listed from was read (the mount's scan, or its parent's
        // last re-read): else it would show that until a later change.
        let reread = self.watching && (prev.is_some() || changed_since_read(dir));
        match &dir.kind {
            DirKind::Source(first) => {
                let known = match prev.and_then(|p| p.snapshot.as_ref()) {
                    Some(Snapshot::Dir(d)) => d,
                    _ => first,
                };
                let fresh;
                let src = if reread {
                    match known.reread(&self.sel, exclude.as_deref()) {
                        Ok(n) => {
                            self.bump(&n.path, *n.stamp);
                            fresh = n;
                            &fresh
                        }
                        Err(e) => {
                            log::warn!("{}: {e}: not re-read", known.path.display());
                            known
                        }
                    }
                } else {
                    known
                };
                if self.watching {
                    cx.watch = src.watch(exclude.as_deref());
                    if reread {
                        cx.snapshot = Some(Snapshot::Dir(src.clone()));
                    }
                }
                let art =
                    (!src.images.is_empty()).then(|| self.art_of(vec![src.path.clone()], &src.sidecars));
                for (name, sub) in &src.dirs {
                    // The top-level directories (usually artists) are never collapsed.
                    let flat = if self.flatten && parent != ROOT { sub.single_image() } else { None };
                    // A collapsed directory is the image's directory: it has its number.
                    let ino = match flat {
                        Some(image) => self.ids.image(image),
                        None => self.ids.dir(&sub.path),
                    };
                    if !self.exists(ino) {
                        let mtime = mtime_of(&sub.path).unwrap_or(dir.mtime);
                        let kind = match flat {
                            Some(image) => DirKind::Flat(image.to_owned(), sub.clone()),
                            None => DirKind::Source(sub.clone()),
                        };
                        let sub_dir = Dir::new(kind, mtime, parent).with_stamp(*sub.stamp);
                        if !self.add(ino, Node::Dir(Box::new(sub_dir))) {
                            continue;
                        }
                    }
                    out.push(Entry { name: dedup.unique(name, false), ino, is_dir: true });
                }
                // Opening an image reads its master and area TOCs: a few small reads,
                // worth doing in parallel on a slow (network) source.
                let new: Vec<&PathBuf> =
                    src.images.iter().filter(|p| !self.exists(self.ids.image(p))).collect();
                // Under a bound, a few at a time: one directory can hold more images than the
                // bound allows open (#237).
                let batch = if self.bounded() { OPEN_BATCH } else { new.len().max(1) };
                for images in new.chunks(batch) {
                    let opened: Vec<(&PathBuf, Disc)> = images
                        .par_iter()
                        .filter_map(|&p| {
                            let id = self.next_disc.fetch_add(1, Ordering::Relaxed);
                            match Disc::open(p, id, self.tuning) {
                                Ok(d) => Some((p, d)),
                                Err(e) => {
                                    log::info!("{}: not shown: {e}", p.display());
                                    None
                                }
                            }
                        })
                        .collect();
                    for (path, disc) in opened {
                        let ino = self.ids.image(path);
                        self.opened(&disc);
                        let mtime = disc.mtime;
                        let stamp = image_stamp(mtime, art.as_deref());
                        let disc = Arc::new(disc);
                        let image = Dir::new(DirKind::Image(disc.clone()), mtime, parent)
                            .with_art(art.clone())
                            .with_stamp(stamp);
                        if self.add(ino, Node::Dir(Box::new(image))) {
                            self.hold(ino, &disc);
                        }
                    }
                    self.keep_to_budget();
                }
                for path in &src.images {
                    let ino = self.ids.image(path);
                    if !self.exists(ino) {
                        continue;
                    }
                    let stem = path.file_stem().map_or_else(|| OsString::from("disc"), OsStr::to_owned);
                    out.push(Entry { name: dedup.unique(&stem, false), ino, is_dir: true });
                }
                self.list_sidecars(&src.sidecars, parent, &mut dedup, &mut out);
            }
            DirKind::Folder(path, first) => {
                let known = match prev.and_then(|p| p.snapshot.as_ref()) {
                    Some(Snapshot::Folder(l)) => l,
                    _ => first,
                };
                let fresh;
                let list = if reread {
                    let scan = Scan { sidecars: &self.sel, exclude: exclude.as_deref(), skip_exclude: true };
                    match SourceDir::scan_dir(path, 1, &scan, true, None) {
                        Ok(n) => {
                            let own = mtime_of(path);
                            self.bump(
                                path,
                                n.as_ref().map_or(Stamp { own, other: own, image: None }, |d| *d.stamp),
                            );
                            fresh = n.map(|d| d.sidecars).unwrap_or_default();
                            cx.snapshot = Some(Snapshot::Folder(fresh.clone()));
                            &fresh
                        }
                        Err(e) => {
                            log::warn!("{}: {e}: not re-read", path.display());
                            known
                        }
                    }
                } else {
                    known
                };
                if self.watching {
                    cx.watch = vec![(path.clone(), mtime_of(path))];
                }
                self.list_sidecars(list, parent, &mut dedup, &mut out);
            }
            DirKind::Closed(image) => {
                let id = self.next_disc.fetch_add(1, Ordering::Relaxed);
                match Disc::open(image, id, self.tuning) {
                    // Listed as when it was first opened; its areas hold the disc from here on.
                    Ok(disc) => {
                        self.opened(&disc);
                        let disc = Arc::new(disc);
                        let img = Dir::new(DirKind::Image(disc.clone()), dir.mtime, dir.parent)
                            .with_art(dir.art.clone())
                            .with_stamp(*dir.stamp);
                        self.hold(parent, &disc);
                        out = self.load(&img, parent, None, &mut Cx::default());
                    }
                    Err(e) => {
                        log::warn!("{}: not opened again: {e}", image.display());
                        cx.unreadable = true;
                    }
                }
            }
            DirKind::Image(disc) => {
                for (i, area) in disc.toc().areas.iter().enumerate() {
                    let name = match area.slot {
                        Slot::TwoChannel => STEREO,
                        Slot::Multichannel => MULTICHANNEL,
                    };
                    let ino = ident::area(parent, area.slot);
                    let area_dir = Dir::new(DirKind::Area(disc.clone(), i), disc.mtime, parent)
                        .with_art(dir.art.clone())
                        .with_stamp(*dir.stamp);
                    if !self.add(ino, Node::Dir(Box::new(area_dir))) {
                        log::info!("{}: second {} area: not shown", disc.path.display(), area.slot);
                        continue;
                    }
                    out.push(Entry { name: dedup.unique(OsStr::new(name), false), ino, is_dir: true });
                }
            }
            DirKind::Flat(image, top) => {
                let known = match prev.and_then(|p| p.snapshot.as_ref()) {
                    Some(Snapshot::Dir(d)) => d,
                    _ => top,
                };
                let fresh;
                let chain = if reread {
                    match known.reread_chain(&self.sel, exclude.as_deref()) {
                        // Only the sidecars follow: what the folder is (one image) was
                        // decided when its parent was listed.
                        Ok(n) if n.single_image() == Some(image.as_path()) => {
                            self.bump(&n.path, *n.stamp);
                            fresh = n;
                            cx.snapshot = Some(Snapshot::Dir(fresh.clone()));
                            &fresh
                        }
                        Ok(_) => {
                            log::info!("{}: changed in a way that needs a remount", image.display());
                            known
                        }
                        Err(e) => {
                            log::warn!("{}: {e}: not re-read", image.display());
                            known
                        }
                    }
                } else {
                    known
                };
                if self.watching {
                    cx.watch = chain.chain_dirs().into_iter().map(|d| (d.clone(), mtime_of(&d))).collect();
                    cx.watch.extend(chain.chain_rejected());
                }
                let sidecars = chain.chain_sidecars();
                match prev {
                    // The image's own entries are kept: its areas were made once.
                    Some(p) if p.head > 0 => {
                        out = p.entries[..p.head].to_vec();
                        for e in &out {
                            dedup.unique(&e.name, false);
                        }
                        cx.head = p.head;
                    }
                    _ => {
                        let id = self.next_disc.fetch_add(1, Ordering::Relaxed);
                        match Disc::open(image, id, self.tuning) {
                            // Listed exactly as the image's own directory would be, then the
                            // sidecars of the chain.
                            Ok(disc) => {
                                self.opened(&disc);
                                let mtime = dir.mtime;
                                let art = self.art_of(chain.chain_dirs(), &sidecars);
                                let stamp = image_stamp(disc.mtime, Some(&art));
                                let disc = Arc::new(disc);
                                let img = Dir::new(DirKind::Image(disc.clone()), mtime, dir.parent)
                                    .with_art(Some(art))
                                    .with_stamp(stamp);
                                self.hold(parent, &disc);
                                out = self.load(&img, parent, None, &mut Cx::default());
                                for e in &out {
                                    dedup.unique(&e.name, false);
                                }
                                cx.head = out.len();
                            }
                            Err(e) => log::info!("{}: not shown: {e}", image.display()),
                        }
                    }
                }
                self.list_sidecars(&sidecars, parent, &mut dedup, &mut out);
            }
            DirKind::Area(disc, i) => {
                let Some(area) = disc.toc().areas.get(*i) else { return out };
                cx.tags = crate::tags::key_of(disc.toc()).map(|k| {
                    let state = crate::tags::state_of(&k);
                    (k, state)
                });
                let mut seen = std::collections::HashSet::new();
                let mut held = 0;
                for track in &area.tracks {
                    if let Err(e) = check_track(area, track, disc.toc().sectors) {
                        log::info!(
                            "{}: {} track {}: not shown: {e}",
                            disc.path.display(),
                            area.slot,
                            track.number
                        );
                        continue;
                    }
                    // The files of one track are listed together and get the same name
                    // but for the extension (and the same de-duplication suffix, since
                    // they are made unique in the same order).
                    for format in self.output.formats() {
                        let spf = self.output.samples_per_frame;
                        let Some(file) = TrackFile::with_pauses(
                            disc.clone(),
                            *i,
                            track,
                            format,
                            spf,
                            self.output.pauses_kept,
                        ) else {
                            continue;
                        };
                        let name =
                            track_file_name(track.number, track.text.title.as_deref(), format.extension());
                        let ino = ident::file(parent, area.slot, track.number, format, self.wav_flavour);
                        held += file.held_bytes();
                        // Listed again (art or tags file changed): a file whose tags changed
                        // gets a new node under the same number, with a new generation (#81).
                        let added = if prev.is_some() {
                            seen.insert(ino) && self.renew(ino, file)
                        } else {
                            self.add(ino, Node::File(Arc::new(file)))
                        };
                        if !added {
                            continue;
                        }
                        out.push(Entry { name: dedup.unique(OsStr::new(&name), true), ino, is_dir: false });
                    }
                }
                if prev.is_none() {
                    self.charge(dir.parent, held);
                }
                // The folder's art, as the same files as in the folder (#95), after the tracks.
                if let Some(art) = &dir.art {
                    let (items, version) = art.current(self);
                    cx.art_version = version;
                    let first = out.len();
                    self.list_sidecars(&items, parent, &mut dedup, &mut out);
                    // MPD looks only for `cover.*` (#148): when there is none, one of the
                    // art files is also there under that name, as the same node.
                    if let Some((sc, name)) = cover_alias(&items, |sc| {
                        let ino = self.ids.sidecar(&sc.path);
                        out[first..].iter().any(|e| e.ino == ino)
                    }) {
                        let ino = self.ids.sidecar(&sc.path);
                        out.push(Entry { name: dedup.unique(OsStr::new(&name), true), ino, is_dir: false });
                    }
                }
            }
        }
        out
    }

    /// The art of the directories `dirs`, from their sidecars `list`.
    fn art_of(&self, dirs: Vec<PathBuf>, list: &[Sidecar]) -> Arc<Art> {
        Art::new(dirs, list.iter().filter(|s| s.is_art()).cloned().collect())
    }

    /// Lists the sidecars `list` in directory `parent`, after its other entries: each is
    /// looked at now (in parallel, as images are opened) and shown if it is a regular file
    /// (or folder) where SOURCE allows; numbered by its source path. A file that is there
    /// already keeps its node if its size and time are the same, else gets a new one under
    /// the same number (a new generation).
    fn list_sidecars(&self, list: &[Sidecar], parent: u64, dedup: &mut Deduper, out: &mut Vec<Entry>) {
        if list.is_empty() {
            return;
        }
        let Some(root) = self.root_fd() else { return };
        let src = self.source();
        let found: Vec<Option<Node>> = list
            .par_iter()
            .map(|sc| {
                let rel = sc.path.strip_prefix(src).ok()?;
                let node = match &sc.kind {
                    SidecarKind::Folder(inner) => sidecar::stat_dir(root, rel).map(|m| {
                        // `own` is the folder's mtime when its content was read, to tell
                        // whether its first listing must read it again.
                        let stamp =
                            Stamp { own: sc.mtime, other: sc.newest().max(Some(m.mtime)), image: None };
                        Node::Dir(Box::new(
                            Dir::new(DirKind::Folder(sc.path.clone(), inner.clone()), m.mtime, parent)
                                .with_stamp(stamp),
                        ))
                    }),
                    SidecarKind::File | SidecarKind::Link => {
                        let link = matches!(sc.kind, SidecarKind::Link);
                        sidecar::target(root, src, rel, link, self.exclude.as_deref()).and_then(|target| {
                            let meta = sidecar::stat_file(root, &target)?;
                            let generation = ident::sidecar_generation(meta.size, meta.mtime);
                            Ok(Node::Sidecar(Arc::new(SidecarFile { target, meta, generation })))
                        })
                    }
                };
                match node {
                    Ok(n) => Some(n),
                    Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                        log::warn!(
                            "{}: link not followed: it points outside SOURCE or into the mount",
                            sc.path.display()
                        );
                        None
                    }
                    Err(e) => {
                        log::info!("{}: not passed through: {e}", sc.path.display());
                        None
                    }
                }
            })
            .collect();
        for (sc, node) in list.iter().zip(found) {
            let Some(node) = node else { continue };
            let is_dir = matches!(node, Node::Dir(_));
            let ino = if is_dir { self.ids.dir(&sc.path) } else { self.ids.sidecar(&sc.path) };
            if !self.publish(ino, node) {
                continue;
            }
            out.push(Entry { name: dedup.unique(&sc.name, !is_dir), ino, is_dir });
        }
    }

    /// Makes `file` the track file `ino` when listing its area again: the node there is kept
    /// if its tag is the same (open files and caches stay as they are), else replaced;
    /// `false` if that number is taken by something else.
    fn renew(&self, ino: u64, file: TrackFile) -> bool {
        let mut nodes = self.nodes.write().unwrap_or_else(|p| p.into_inner());
        match nodes.get(&ino).map(|n| &**n) {
            None => {}
            Some(Node::File(old)) => {
                if old.tags == file.tags && old.size == file.size {
                    return true;
                }
            }
            Some(_) => return false,
        }
        nodes.insert(ino, Arc::new(Node::File(Arc::new(file))));
        true
    }

    /// Makes `node`, a sidecar file or folder, the node `ino` (see [`Tree::list_sidecars`]);
    /// `false` if that number is taken by something else.
    fn publish(&self, ino: u64, node: Node) -> bool {
        let mut nodes = self.nodes.write().unwrap_or_else(|p| p.into_inner());
        match (nodes.get(&ino).map(|n| &**n), &node) {
            (None, _) => {}
            // A folder lists itself.
            (Some(Node::Dir(_)), Node::Dir(_)) => return true,
            (Some(Node::Sidecar(a)), Node::Sidecar(b)) => {
                if a.target == b.target && a.meta.size == b.meta.size && a.meta.mtime == b.meta.mtime {
                    return true;
                }
            }
            _ => return false,
        }
        nodes.insert(ino, Arc::new(node));
        true
    }

    /// Every track file in the tree, with its path, loading every directory (for tests and
    /// tools; a mount loads lazily).
    pub fn all_files(&self) -> Vec<(PathBuf, Arc<TrackFile>)> {
        let mut out = Vec::new();
        let mut stack = vec![(PathBuf::new(), ROOT)];
        while let Some((path, ino)) = stack.pop() {
            let Some(children) = self.children(ino) else { continue };
            for e in children.iter() {
                let p = path.join(&e.name);
                if e.is_dir {
                    stack.push((p, e.ino));
                } else if let Some(node) = self.get(e.ino) {
                    if let Node::File(f) = &*node {
                        out.push((p, f.clone()));
                    }
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A corpus image with a stereo and a multichannel area.
    fn corpus_image() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/images/dsd-stereo-mch-2048.iso")
    }

    /// Builds a source tree in a temporary directory: each path ending in `.iso` is a link
    /// to a corpus image, one with another extension an ordinary file holding its path,
    /// anything else a directory.
    fn source(paths: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for p in paths {
            let path = dir.path().join(p);
            if p.ends_with(".iso") {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(corpus_image(), &path).unwrap();
            } else if Path::new(p).extension().is_some() {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, p.as_bytes()).unwrap();
            } else {
                fs::create_dir_all(&path).unwrap();
            }
        }
        dir
    }

    fn tree(dir: &Path, flatten: bool) -> Tree {
        Tree::new(SourceDir::scan(dir).unwrap()).with_flatten(flatten)
    }

    fn names(tree: &Tree, path: &str) -> Vec<String> {
        let ino = tree.resolve(Path::new(path)).unwrap_or_else(|| panic!("{path} not found"));
        tree.children(ino).unwrap().iter().map(|e| e.name.to_string_lossy().into_owned()).collect()
    }

    /// Every directory and file path of the tree, loading every directory.
    fn paths(tree: &Tree) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![(PathBuf::new(), ROOT)];
        while let Some((path, ino)) = stack.pop() {
            for e in tree.children(ino).unwrap().iter() {
                let p = path.join(&e.name);
                out.push(p.display().to_string());
                if e.is_dir {
                    stack.push((p, e.ino));
                }
            }
        }
        out.sort();
        out
    }

    /// What a client sees of the tree: each path with its number, kind, size, mtime and
    /// generation, and each track file's header length and tag.
    fn seen(tree: &Tree) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![(PathBuf::new(), ROOT)];
        while let Some((path, ino)) = stack.pop() {
            for e in tree.children(ino).unwrap().iter() {
                let p = path.join(&e.name);
                let node = tree.get(e.ino).unwrap();
                let (size, extra) = match &*node {
                    Node::File(f) => (f.size, format!("{} {:?}", f.header_len(), f.tag())),
                    Node::Sidecar(s) => (s.meta.size, String::new()),
                    Node::Dir(_) => (0, String::new()),
                };
                let mtime = tree.mtime(&node);
                let generation = tree.generation(&node);
                out.push(format!(
                    "{} {} {} {size} {mtime:?} {generation} {extra}",
                    p.display(),
                    e.ino,
                    e.is_dir
                ));
                if e.is_dir {
                    stack.push((p, e.ino));
                }
            }
        }
        out.sort();
        out
    }

    /// A library of `n` images, two to a folder, with a cover in each folder.
    fn library(n: usize) -> tempfile::TempDir {
        let mut paths = Vec::new();
        for i in 0..n {
            paths.push(format!("A{:02}/D{i:03}.iso", i / 2));
            paths.push(format!("A{:02}/cover.jpg", i / 2));
        }
        source(&paths.iter().map(String::as_str).collect::<Vec<_>>())
    }

    /// The bound that keeps only [`MIN_HELD`] images.
    const TINY: Option<usize> = Some(1);

    fn bounded(dir: &Path, budget: Option<usize>, flatten: bool) -> Tree {
        Tree::with_options(
            SourceDir::scan_with(dir, &Sidecars::default(), None).unwrap(),
            Tuning::default(),
            Output::default(),
        )
        .with_flatten(flatten)
        .with_listing_budget(budget)
    }

    #[test]
    fn a_bound_changes_nothing_a_client_sees() {
        for flatten in [false, true] {
            let dir = library(24);
            let free = seen(&bounded(dir.path(), None, flatten));
            assert!(free.len() > 24 * 5, "{}", free.len());
            let tree = bounded(dir.path(), TINY, flatten);
            assert_eq!(seen(&tree), free, "flatten {flatten}");
            // Images were let go of, and everything comes back the same, more than once.
            assert!(tree.held_images() <= MIN_HELD + 1, "{}", tree.held_images());
            assert_eq!(seen(&tree), free, "flatten {flatten}, again");
            assert_eq!(seen(&tree), free, "flatten {flatten}, a third time");
            assert!(tree.held_images() <= MIN_HELD + 1, "{}", tree.held_images());
        }
    }

    #[test]
    fn with_a_flattened_chain_each_image_comes_back() {
        let paths: Vec<String> = (0..24).map(|i| format!("A/B{i:02}/C/x.iso")).collect();
        let dir = source(&paths.iter().map(String::as_str).collect::<Vec<_>>());
        let free = seen(&bounded(dir.path(), None, true));
        let tree = bounded(dir.path(), TINY, true);
        assert_eq!(seen(&tree), free);
        assert!(tree.held_images() <= MIN_HELD + 1, "{}", tree.held_images());
        assert_eq!(seen(&tree), free);
    }

    #[test]
    fn under_pressure_listings_shrink_and_then_new_ones_fail() {
        use crate::memory::Pressure;
        let dir = library(24);
        let free = seen(&bounded(dir.path(), None, false));
        let tree = bounded(dir.path(), Some(1 << 30), false);
        assert_eq!(seen(&tree), free);
        assert_eq!(tree.held_images(), 24);
        let one = tree.held_bytes() / 24;
        // A budget of 8 images' worth, at a quarter: 2, and at least the few always kept.
        let tree = bounded(dir.path(), Some(one * 8), false);
        assert_eq!(seen(&tree), free);
        tree.set_pressure(Pressure::Listings);
        assert_eq!(tree.held_images(), MIN_HELD);
        tree.set_pressure(Pressure::Refuse);
        assert!(tree.refusing());
        // What is listed stays listed; a new listing fails.
        let held =
            (0..24).map(|i| format!("A{:02}/D{i:03}", i / 2)).map(|p| tree.resolve(Path::new(&p)).unwrap());
        let (closed, open): (Vec<_>, Vec<_>) = held.partition(|i| {
            matches!(tree.loaded(*i).as_deref(), Some(Node::Dir(d)) if matches!(d.kind, DirKind::Closed(_)))
        });
        assert!(!open.is_empty() && !closed.is_empty());
        assert!(tree.children(closed[0]).is_none());
        // Over: back as they were, nothing lost.
        tree.set_pressure(Pressure::None);
        assert!(!tree.refusing());
        assert_eq!(seen(&tree), free);
    }

    #[test]
    fn no_bound_holds_no_account() {
        let dir = library(12);
        let tree = bounded(dir.path(), None, false);
        seen(&tree);
        assert_eq!((tree.held_images(), tree.held_bytes()), (0, 0));
    }

    #[test]
    fn a_big_budget_lets_nothing_go() {
        let dir = library(12);
        let tree = bounded(dir.path(), Some(1 << 30), false);
        seen(&tree);
        assert_eq!(tree.held_images(), 12);
        assert!(tree.held_bytes() > 12 * IMAGE_HELD);
    }

    #[test]
    fn numbers_not_in_the_tree_are_found_again() {
        let dir = library(24);
        let tree = bounded(dir.path(), TINY, false);
        let files: Vec<u64> = {
            let free = bounded(dir.path(), None, false);
            let mut stack = vec![ROOT];
            let mut out = Vec::new();
            while let Some(i) = stack.pop() {
                for e in free.children(i).unwrap().iter() {
                    if e.is_dir {
                        stack.push(e.ino);
                    } else if matches!(free.get(e.ino).as_deref(), Some(Node::File(_))) {
                        out.push(e.ino);
                    }
                }
            }
            out
        };
        seen(&tree);
        assert!(files.iter().any(|i| tree.loaded(*i).is_none()), "something must have been let go of");
        for ino in files {
            assert!(matches!(tree.get(ino).as_deref(), Some(Node::File(_))), "{ino}");
            assert_eq!(tree.parent_of(ident::area_of(ino).unwrap()) & ident::key_of(ino), ident::key_of(ino));
        }
    }

    #[test]
    fn an_open_file_keeps_its_image() {
        let dir = library(24);
        let tree = bounded(dir.path(), TINY, false);
        let image = tree.resolve(Path::new("A00/D000")).unwrap();
        let area = tree.children(image).unwrap()[0].ino;
        let entries = tree.children(area).unwrap();
        let first = entries.iter().find(|e| !e.name.to_string_lossy().contains("cover")).unwrap();
        let Some(Node::File(file)) = tree.get(first.ino).as_deref().map(|n| match n {
            Node::File(f) => Node::File(f.clone()),
            _ => panic!("a file"),
        }) else {
            panic!("a file")
        };
        for _ in 0..3 {
            seen(&tree);
        }
        // Same node, so the same disc: it was never let go of.
        assert!(matches!(tree.loaded(first.ino).as_deref(), Some(Node::File(f)) if Arc::ptr_eq(f, &file)));
        assert!(
            matches!(tree.loaded(image).as_deref(), Some(Node::Dir(d)) if matches!(d.kind, DirKind::Image(_)))
        );
        drop(file);
        seen(&tree);
        seen(&tree);
        assert!(
            tree.loaded(first.ino).is_none_or(|n| matches!(&*n, Node::File(f) if Arc::strong_count(f) == 1))
        );
    }

    #[test]
    fn a_directory_of_many_images_stays_within_the_bound() {
        let paths: Vec<String> = (0..300).map(|i| format!("A/D{i:03}.iso")).collect();
        let dir = source(&paths.iter().map(String::as_str).collect::<Vec<_>>());
        let tree = bounded(dir.path(), TINY, false);
        let a = tree.resolve(Path::new("A")).unwrap();
        assert_eq!(tree.children(a).unwrap().len(), 300);
        assert!(tree.held_images() <= MIN_HELD + 1, "{}", tree.held_images());
    }

    #[test]
    fn an_image_gone_while_closed_is_gone() {
        let dir = library(24);
        let tree =
            bounded(dir.path(), TINY, false).watching(&Sidecars::default()).with_refresh(Duration::ZERO);
        seen(&tree);
        let closed = (0..24)
            .find_map(|i| {
                let p = format!("A{:02}/D{i:03}", i / 2);
                let ino = tree.resolve(Path::new(&p))?;
                matches!(tree.loaded(ino).as_deref(), Some(Node::Dir(d)) if matches!(d.kind, DirKind::Closed(_)))
                    .then_some((p, ino))
            })
            .expect("a closed image");
        let (rel, ino) = closed;
        let iso = dir.path().join(format!("{rel}.iso"));
        let target = fs::read_link(&iso).unwrap();
        fs::remove_file(&iso).unwrap();
        // Its folder not yet read again: the image can't be opened, and that is not kept.
        assert!(tree.children(ino).is_none());
        // Written back: the same directory, listed.
        std::os::unix::fs::symlink(&target, &iso).unwrap();
        assert!(!tree.children(ino).unwrap().is_empty());
        // Removed for good: its folder, read again, no longer shows it.
        fs::remove_file(&iso).unwrap();
        let parent = rel.split('/').next().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let names = names(&tree, parent);
        assert!(!names.contains(&rel.split('/').nth(1).unwrap().to_owned()), "{names:?}");
        assert!(tree.get(ino).is_none());
    }

    #[test]
    fn readers_racing_a_tiny_bound_all_see_the_same() {
        let dir = library(32);
        let free = seen(&bounded(dir.path(), None, false));
        let tree = Arc::new(bounded(dir.path(), TINY, false));
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for _ in 0..4 {
                        assert_eq!(seen(&tree), free);
                    }
                });
            }
        });
    }

    #[test]
    fn single_image_follows_chains() {
        let dir = source(&[
            "A/B/C/x.iso",
            "A/B/notes.txt",
            "A/empty/deeper",
            "M/x.iso",
            "M/y.iso",
            "N/x.iso",
            "N/S/y.iso",
        ]);
        let root = SourceDir::scan(dir.path()).unwrap();
        let sub = |name: &str| &root.dirs.iter().find(|(n, _)| n == name).unwrap().1;
        assert_eq!(sub("A").single_image(), Some(dir.path().join("A/B/C/x.iso").as_path()));
        assert_eq!(sub("M").single_image(), None);
        assert_eq!(sub("N").single_image(), None);
        assert_eq!(root.single_image(), None);
    }

    #[test]
    fn chains_collapse_into_their_outermost_directory() {
        let dir =
            source(&["Artist/Album/Rip/Disc/x.iso", "Artist/Album/Rip/cover.txt", "Artist/Other/y.iso"]);
        let t = tree(dir.path(), true);
        assert_eq!(names(&t, "Artist"), ["Album", "Other"]);
        assert_eq!(names(&t, "Artist/Album"), ["Stereo", "Multichannel"]);
        assert_eq!(names(&t, "Artist/Other"), ["Stereo", "Multichannel"]);
        assert!(names(&t, "Artist/Album/Stereo")[0].starts_with("01"));
        // `..` of an area is the collapsed directory, and of that its source parent.
        let album = t.resolve(Path::new("Artist/Album")).unwrap();
        let stereo = t.resolve(Path::new("Artist/Album/Stereo")).unwrap();
        assert_eq!(t.parent_of(stereo), album);
        assert_eq!(t.parent_of(album), t.resolve(Path::new("Artist")).unwrap());
        // Without the option the tree is as before.
        let t = tree(dir.path(), false);
        assert_eq!(names(&t, "Artist/Album/Rip/Disc/x"), ["Stereo", "Multichannel"]);
    }

    #[test]
    fn top_level_is_not_collapsed() {
        let dir = source(&["Artist/Album/x.iso", "Solo/y.iso", "loose.iso"]);
        let t = tree(dir.path(), true);
        assert_eq!(names(&t, ""), ["Artist", "Solo", "loose"]);
        assert_eq!(names(&t, "Artist"), ["Album"]);
        assert_eq!(names(&t, "Artist/Album"), ["Stereo", "Multichannel"]);
        assert_eq!(names(&t, "Solo"), ["y"]);
        assert_eq!(names(&t, "loose"), ["Stereo", "Multichannel"]);
    }

    #[test]
    fn mixed_directories_are_not_collapsed() {
        let dir =
            source(&["Artist/Box/a.iso", "Artist/Box/Bonus/b.iso", "Artist/Pair/c.iso", "Artist/Pair/d.iso"]);
        let t = tree(dir.path(), true);
        // An image next to a folder with an image: kept; the folder alone collapses.
        assert_eq!(names(&t, "Artist/Box"), ["Bonus", "a"]);
        assert_eq!(names(&t, "Artist/Box/Bonus"), ["Stereo", "Multichannel"]);
        assert_eq!(names(&t, "Artist/Box/a"), ["Stereo", "Multichannel"]);
        // Two images: kept.
        assert_eq!(names(&t, "Artist/Pair"), ["c", "d"]);
    }

    #[test]
    fn collisions_get_a_number() {
        // The collapsed folder `Disc` and the image `Disc.iso` next to it.
        let dir = source(&["Artist/Album/Disc/Rip/x.iso", "Artist/Album/Disc.iso"]);
        let t = tree(dir.path(), true);
        assert_eq!(names(&t, "Artist/Album"), ["Disc", "Disc (2)"]);
        assert_eq!(names(&t, "Artist/Album/Disc"), ["Stereo", "Multichannel"]);
        assert_eq!(names(&t, "Artist/Album/Disc (2)"), ["Stereo", "Multichannel"]);
    }

    #[test]
    fn single_file_source_is_unaffected() {
        let t = Tree::new(SourceDir::scan(&corpus_image()).unwrap()).with_flatten(true);
        assert_eq!(names(&t, ""), ["dsd-stereo-mch-2048"]);
        assert_eq!(names(&t, "dsd-stereo-mch-2048"), ["Stereo", "Multichannel"]);
    }

    #[test]
    fn inodes_are_stable() {
        let dir = source(&["Artist/Album/Rip/x.iso", "Artist/Other/Rip/y.iso"]);
        let t = tree(dir.path(), true);
        let first: Vec<_> =
            ["Artist/Album/Stereo", "Artist/Album"].map(|p| t.resolve(Path::new(p)).unwrap()).into();
        let all = paths(&t);
        let again: Vec<_> =
            ["Artist/Album/Stereo", "Artist/Album"].map(|p| t.resolve(Path::new(p)).unwrap()).into();
        assert_eq!(first, again);
        let n = t.len();
        assert_eq!(paths(&t), all);
        assert_eq!(t.len(), n, "listing again makes no nodes");
        // Every inode is distinct.
        let mut inos: Vec<u64> = all.iter().map(|p| t.resolve(Path::new(p)).unwrap()).collect();
        inos.sort_unstable();
        inos.dedup();
        assert_eq!(inos.len(), all.len());
    }

    #[test]
    fn flattening_opens_no_image_until_looked_into() {
        // The image is probed for the SACD signature (one small read) when the mount scans,
        // but not opened (its TOCs parsed) until the folder is looked into.
        let dir = source(&["Artist/A/Rip/x.iso", "Artist/B/y.iso"]);
        let t = tree(dir.path(), true);
        assert_eq!(names(&t, "Artist"), ["A", "B"]);
        assert_eq!(t.next_disc.load(Ordering::Relaxed), 1, "listing opened an image");
        assert_eq!(names(&t, "Artist/A"), ["Stereo", "Multichannel"]);
        assert_eq!(t.next_disc.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn same_files_with_and_without_flatten() {
        let dir =
            source(&["Artist/Album/Rip/x.iso", "Artist/Box/a.iso", "Artist/Box/Bonus/b.iso", "Solo/y.iso"]);
        let count = |flatten| tree(dir.path(), flatten).all_files().len();
        assert_eq!(count(true), count(false));
        assert!(count(true) > 0);
    }

    /// Every path of the tree with its inode number, loading every directory.
    fn numbered(tree: &Tree) -> Vec<(String, u64)> {
        let mut out: Vec<_> = paths(tree)
            .into_iter()
            .map(|p| {
                let ino = tree.resolve(Path::new(&p)).unwrap();
                (p, ino)
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn numbers_are_the_same_on_every_mount() {
        let dir = source(&["Artist/Album/Rip/x.iso", "Artist/Box/a.iso", "Artist/Box/Bonus/b.iso", "y.iso"]);
        let a = numbered(&tree(dir.path(), false));
        // Another tree (a remount), listed in another order.
        let t = tree(dir.path(), false);
        let _ = names(&t, "y/Multichannel");
        assert_eq!(numbered(&t), a);
        // Every number distinct, none the root's.
        let mut inos: Vec<u64> = a.iter().map(|(_, i)| *i).collect();
        inos.sort_unstable();
        inos.dedup();
        assert_eq!(inos.len(), a.len());
        assert!(!inos.contains(&ROOT));
    }

    #[test]
    fn flatten_keeps_the_numbers_of_images_areas_and_files() {
        let dir = source(&["Artist/Album/Rip/x.iso", "Artist/Box/a.iso", "Artist/Box/Bonus/b.iso"]);
        let plain = tree(dir.path(), false);
        let flat = tree(dir.path(), true);
        let same = [
            ("Artist/Album/Rip/x", "Artist/Album"),
            ("Artist/Album/Rip/x/Stereo", "Artist/Album/Stereo"),
            ("Artist/Box/Bonus/b/Multichannel", "Artist/Box/Bonus/Multichannel"),
            ("Artist/Box/a/Stereo", "Artist/Box/a/Stereo"),
            ("Artist", "Artist"),
        ];
        for (p, f) in same {
            assert_eq!(plain.resolve(Path::new(p)), flat.resolve(Path::new(f)), "{p} / {f}");
        }
        let files = |t: &Tree, area: &str| -> Vec<u64> {
            let ino = t.resolve(Path::new(area)).unwrap();
            t.children(ino).unwrap().iter().map(|e| e.ino).collect()
        };
        assert_eq!(files(&plain, "Artist/Album/Rip/x/Stereo"), files(&flat, "Artist/Album/Stereo"));
        // The collapsed source directories have no node with --flatten.
        let rip = plain.resolve(Path::new("Artist/Album/Rip")).unwrap();
        assert!(flat.get(rip).is_none());
    }

    #[test]
    fn numbers_are_found_on_a_fresh_mount() {
        let dir = source(&[
            "Artist/Album/Rip/x.iso",
            "Artist/Box/a.iso",
            "Artist/Box/Bonus/b.iso",
            "Other/Deep/z.iso",
            "y.iso",
        ]);
        for flatten in [false, true] {
            let all = numbered(&tree(dir.path(), flatten));
            for (path, ino) in &all {
                // A fresh tree, as after a restart: only the number is known.
                let t = tree(dir.path(), flatten);
                let node = t.get(*ino).unwrap_or_else(|| panic!("{path} ({ino:#x}) not found"));
                // Only the images of the directories on the way are opened (as a lookup of
                // the path would): never all five, at most y, a and b (next_disc starts at 1).
                if !path.starts_with("Other") {
                    assert!(t.next_disc.load(Ordering::Relaxed) <= 4, "{path}");
                }
                assert_eq!(t.resolve(Path::new(path)), Some(*ino));
                let is_dir = matches!(&*node, Node::Dir(_));
                assert_eq!(is_dir, !path.ends_with(".dsf") && !path.ends_with(".wav"), "{path}");
            }
            // Numbers that name nothing.
            let t = tree(dir.path(), flatten);
            let (_, area) = all.iter().find(|(p, _)| p.ends_with("Stereo")).unwrap();
            for bad in [0, 2, 1 << 16, area | (3 << 14), area | 0x1fff, u64::MAX] {
                assert!(t.get(bad).is_none(), "{bad:#x}");
            }
        }
        // A source directory hidden by --flatten is not found in a flattened mount.
        let rip = tree(dir.path(), false).resolve(Path::new("Artist/Album/Rip")).unwrap();
        assert!(tree(dir.path(), true).get(rip).is_none());
    }

    #[test]
    fn pcm_options_change_wav_numbers_and_generations_only() {
        let dir = source(&["Artist/x.iso"]);
        let files = |pcm: PcmConfig, spf: u32| {
            let out = Output { samples_per_frame: spf, ..Output::default() };
            let t = Tree::with_options(SourceDir::scan(dir.path()).unwrap(), Tuning::default(), out)
                .with_pcm(&pcm);
            let mut v: Vec<(String, u64, u64, SystemTime)> = t
                .all_files()
                .into_iter()
                .map(|(p, f)| {
                    let ino = t.resolve(&p).unwrap();
                    let mtime = t.mtime(&Node::File(f.clone()));
                    (p.display().to_string(), ino, ident::generation(&f, t.wav_flavour), mtime)
                })
                .collect();
            v.sort();
            v
        };
        let base = files(PcmConfig::default(), 2352);
        // The same options on another mount: the same numbers, generations and mtimes.
        assert_eq!(files(PcmConfig::default(), 2352), base);
        // With the default options, every file shows its image's mtime moved by its format's
        // output version (issue #64).
        let image = fs::metadata(dir.path().join("Artist/x.iso")).unwrap().modified().unwrap();
        let shown = |p: &str| {
            let format =
                if p.ends_with(".dsf") { crate::track::Format::Dsf } else { crate::track::Format::Wav };
            image + ident::version_offset(format)
        };
        assert!(base.iter().all(|f| f.3 == shown(&f.0)));
        // Options that keep the size move the WAV files' mtime; the rate changes the size.
        for (other, mtime_moves) in [
            (files(PcmConfig { gain_db: -3.0, ..PcmConfig::default() }, 2352), true),
            (files(PcmConfig { cutoff: crate::pcm::Cutoff::Khz30, ..PcmConfig::default() }, 2352), true),
            (files(PcmConfig::default(), 4704), false),
        ] {
            for (a, b) in base.iter().zip(&other) {
                assert_eq!(a.0, b.0);
                if a.0.ends_with(".dsf") {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a.2, b.2, "{}: same generation", a.0);
                    assert_eq!(a.3 != b.3, mtime_moves, "{}: mtime", a.0);
                    assert!(b.3 >= a.3, "{}: older mtime", a.0);
                }
            }
        }
    }

    #[test]
    fn a_changed_image_gets_new_generations() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("x.iso");
        fs::copy(corpus_image(), &image).unwrap();
        let gens = || -> Vec<(u64, u64)> {
            let t = tree(dir.path(), false);
            t.all_files()
                .into_iter()
                .map(|(p, f)| (t.resolve(&p).unwrap(), t.generation(&Node::File(f))))
                .collect()
        };
        let before = gens();
        assert_eq!(gens(), before);
        let f = fs::File::options().write(true).open(&image).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000)).unwrap();
        let after = gens();
        for (a, b) in before.iter().zip(&after) {
            assert_eq!(a.0, b.0, "same numbers");
            assert_ne!(a.1, b.1, "new generation");
            assert_ne!(b.1, 0);
        }
        // Directories keep generation 0.
        let t = tree(dir.path(), false);
        assert_eq!(t.generation(&t.get(ROOT).unwrap()), 0);
    }

    /// A tree passing the default sidecars through.
    fn tree_sc(dir: &Path, flatten: bool) -> Tree {
        Tree::new(SourceDir::scan_with(dir, &Sidecars::standard(), None).unwrap()).with_flatten(flatten)
    }

    fn sidecar_of(t: &Tree, path: &str) -> Arc<SidecarFile> {
        let node = t.get(t.resolve(Path::new(path)).unwrap_or_else(|| panic!("{path} not found"))).unwrap();
        let Node::Sidecar(s) = &*node else { panic!("{path}: not a sidecar") };
        s.clone()
    }

    fn read_sidecar(t: &Tree, path: &str) -> Vec<u8> {
        use std::io::Read;
        let mut v = Vec::new();
        t.open_sidecar(&sidecar_of(t, path)).unwrap().read_to_end(&mut v).unwrap();
        v
    }

    const ALBUM: &[&str] = &[
        "Artist/Album/x.iso",
        "Artist/Album/folder.jpg",
        "Artist/Album/cover.PNG",
        "Artist/Album/notes.txt",
        "Artist/Album/._folder.jpg",
        "Artist/Album/.hidden/a.jpg",
        "Artist/Album/Artwork/front.jpg",
        "Artist/Album/Artwork/info.pdf",
        "Artist/Album/Artwork/Booklet/p1.tif",
        "Artist/Album/Lyrics/x.jpg",
        "Artist/list.m3u8",
        "Artist/NoImages/cover.jpg",
    ];

    /// Writes a file named `.iso` that is no SACD (a DVD-Video image, say), at `rel`.
    fn other_iso(dir: &Path, rel: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Long enough for every sector format's signature position, holding no signature.
        fs::write(&path, vec![0u8; 600 * 2064]).unwrap();
    }

    #[test]
    fn folders_without_an_sacd_image_are_hidden_with_their_sidecars() {
        // 175: a folder whose only image is not an SACD showed its art, and its parents showed.
        let dir = source(&["Good/Album/x.iso", "Good/Album/cover.jpg", "Good/Album/Artwork/front.jpg"]);
        other_iso(dir.path(), "Dvd/Album/movie.iso");
        fs::write(dir.path().join("Dvd/Album/cover.jpg"), b"art").unwrap();
        fs::create_dir_all(dir.path().join("Dvd/Album/Artwork")).unwrap();
        fs::write(dir.path().join("Dvd/Album/Artwork/front.jpg"), b"art").unwrap();
        for flatten in [false, true] {
            let t = tree_sc(dir.path(), flatten);
            assert_eq!(names(&t, ""), ["Good"], "flatten {flatten}");
            assert!(t.resolve(Path::new("Dvd")).is_none());
            assert!(names(&t, "Good/Album").contains(&"cover.jpg".to_owned()), "flatten {flatten}");
        }
    }

    #[test]
    fn a_box_set_drops_its_non_sacd_discs() {
        let dir = source(&["Set/Disc 1/x.iso", "Set/cover.jpg"]);
        other_iso(dir.path(), "Set/Disc 2/movie.iso");
        other_iso(dir.path(), "Set/Disc 3/BDMV/disc.iso");
        fs::write(dir.path().join("Set/Disc 2/cover.jpg"), b"art").unwrap();
        let t = tree_sc(dir.path(), false);
        let set = names(&t, "Set");
        assert!(set.contains(&"Disc 1".to_owned()) && set.contains(&"cover.jpg".to_owned()), "{set:?}");
        assert!(!set.iter().any(|n| n.starts_with("Disc 2") || n.starts_with("Disc 3")), "{set:?}");
    }

    #[test]
    fn an_image_that_becomes_an_sacd_later_appears() {
        use std::io::{Seek, SeekFrom, Write};
        // 175: a folder hidden for its non-SACD image must come back when the image is
        // written in place (a torrent filling a preallocated file, a copy in progress), which
        // changes the file and not its directory. Both a file of full size with the TOC
        // sector still zeros, and one still too short to hold it.
        let dir = source(&["Artist/Other/y.iso"]);
        other_iso(dir.path(), "Artist/Zeros/z.iso");
        other_iso(dir.path(), "Artist/Deeper/Sub/w.iso");
        fs::create_dir_all(dir.path().join("Artist/Short")).unwrap();
        fs::write(dir.path().join("Artist/Short/s.iso"), b"partial").unwrap();
        let t = watching(dir.path(), "default", false);
        assert_eq!(names(&t, "Artist"), ["Other"]);
        let later = std::time::SystemTime::now() + Duration::from_secs(10);
        // The TOC sector arrives.
        let mut f = fs::OpenOptions::new().write(true).open(dir.path().join("Artist/Zeros/z.iso")).unwrap();
        f.seek(SeekFrom::Start(510 * 2048)).unwrap();
        f.write_all(b"SACDMTOC").unwrap();
        f.set_modified(later).unwrap();
        // The short file grows to a whole image.
        let full = fs::read(corpus_image()).unwrap();
        let mut f = fs::OpenOptions::new().write(true).open(dir.path().join("Artist/Short/s.iso")).unwrap();
        f.write_all(&full).unwrap();
        f.set_modified(later).unwrap();
        // One two levels below a hidden folder.
        let mut f =
            fs::OpenOptions::new().write(true).open(dir.path().join("Artist/Deeper/Sub/w.iso")).unwrap();
        f.seek(SeekFrom::Start(510 * 2048)).unwrap();
        f.write_all(b"SACDMTOC").unwrap();
        f.set_modified(later).unwrap();
        assert_eq!(names(&t, "Artist"), ["Deeper", "Other", "Short", "Zeros"]);
        assert_eq!(names(&t, "Artist/Short/s"), ["Stereo", "Multichannel"]);
        assert_eq!(names(&t, "Artist/Deeper"), ["Sub"]);
    }

    #[test]
    fn a_rejected_image_is_probed_again_only_when_it_changes() {
        // 175: a rescan reads nothing of an image it rejected, until its size or time moves.
        let dir = source(&["Artist/Other/y.iso"]);
        other_iso(dir.path(), "Artist/Zeros/z.iso");
        let path = dir.path().join("Artist/Zeros/z.iso");
        let before = PROBES.with(std::cell::Cell::get);
        let t = watching(dir.path(), "default", false);
        assert_eq!(names(&t, "Artist"), ["Other"]);
        let scanned = PROBES.with(std::cell::Cell::get);
        assert!(scanned > before);
        // A change elsewhere in the folder makes the folder be read again.
        let artist = dir.path().join("Artist");
        fs::write(artist.join("Other/cover.jpg"), b"art").unwrap();
        fs::File::open(artist.join("Other"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(20))
            .unwrap();
        fs::File::open(&artist)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(30))
            .unwrap();
        assert_eq!(names(&t, "Artist"), ["Other"]);
        assert_eq!(names(&t, "Artist/Other"), ["y", "cover.jpg"]);
        assert_eq!(PROBES.with(std::cell::Cell::get), scanned, "the rejected image was read again");
        drop(path);
    }

    #[test]
    fn a_non_sacd_image_beside_an_sacd_is_not_listed() {
        let dir = source(&["Album/x.iso", "Album/cover.jpg"]);
        other_iso(dir.path(), "Album/movie.iso");
        let t = tree_sc(dir.path(), false);
        assert_eq!(names(&t, "Album"), ["x", "cover.jpg"]);
    }

    #[test]
    fn sidecars_are_listed_after_the_images() {
        let dir = source(ALBUM);
        let t = tree_sc(dir.path(), false);
        assert_eq!(names(&t, ""), ["Artist"]);
        assert_eq!(names(&t, "Artist"), ["Album", "list.m3u8"]);
        // Folders without images are not shown unless allowed by name (Artwork, whose whole
        // content passes); hidden names and other extensions never are.
        assert_eq!(names(&t, "Artist/Album"), ["x", "Artwork", "cover.PNG", "folder.jpg"]);
        assert_eq!(names(&t, "Artist/Album/Artwork"), ["Booklet", "front.jpg", "info.pdf"]);
        assert_eq!(names(&t, "Artist/Album/Artwork/Booklet"), ["p1.tif"]);
        for p in ["Artist/Album/folder.jpg", "Artist/Album/Artwork/Booklet/p1.tif", "Artist/list.m3u8"] {
            assert_eq!(read_sidecar(&t, p), p.as_bytes(), "{p}");
            let s = sidecar_of(&t, p);
            let src = fs::metadata(dir.path().join(p)).unwrap();
            assert_eq!(s.meta.size, src.len());
            assert_eq!(s.meta.mtime, src.modified().unwrap());
            assert_eq!(t.mtime(&Node::Sidecar(s.clone())), src.modified().unwrap());
            assert_ne!(t.generation(&Node::Sidecar(s)), 0);
        }
        // A sidecar folder is a directory, generation 0, its parent the album.
        let art = t.resolve(Path::new("Artist/Album/Artwork")).unwrap();
        assert_eq!(t.parent_of(art), t.resolve(Path::new("Artist/Album")).unwrap());
        assert_eq!(t.generation(&t.get(art).unwrap()), 0);
        // Nothing is passed through without the option.
        let plain = tree(dir.path(), false);
        assert_eq!(names(&plain, "Artist"), ["Album"]);
        assert_eq!(names(&plain, "Artist/Album"), ["x"]);
    }

    #[test]
    fn nothing_changes_for_images_and_their_files() {
        // With sidecars present: every entry of the tree without them keeps its name and
        // number; and with none present, the trees are the same.
        let dir = source(ALBUM);
        for flatten in [false, true] {
            let plain = numbered(&tree(dir.path(), flatten));
            let t = tree_sc(dir.path(), flatten);
            for (p, ino) in &plain {
                assert_eq!(t.resolve(Path::new(p)), Some(*ino), "{p}");
            }
            assert!(numbered(&t).len() > plain.len());
        }
        let dir = source(&["Artist/Album/Rip/x.iso", "Artist/Box/a.iso", "Artist/Box/notes.txt", "y.iso"]);
        for flatten in [false, true] {
            assert_eq!(numbered(&tree_sc(dir.path(), flatten)), numbered(&tree(dir.path(), flatten)));
        }
    }

    #[test]
    fn flatten_shows_the_chains_sidecars_in_the_collapsed_folder() {
        let dir = source(&[
            "Artist/Album/Rip/Disc/x.iso",
            "Artist/Album/folder.jpg",
            "Artist/Album/Rip/folder.jpg",
            "Artist/Album/Rip/Disc/Scans/back.jpg",
            "Artist/Album/Rip/Disc/list.m3u",
            "Artist/Album/Rip/Disc/Stereo.png",
        ]);
        let flat = tree_sc(dir.path(), true);
        // The areas first, then the chain's sidecars from the outermost directory in; a
        // name taken gets a number.
        assert_eq!(
            names(&flat, "Artist/Album"),
            ["Stereo", "Multichannel", "folder.jpg", "folder (2).jpg", "Scans", "Stereo.png", "list.m3u"]
        );
        assert_eq!(read_sidecar(&flat, "Artist/Album/folder (2).jpg"), b"Artist/Album/Rip/folder.jpg");
        assert_eq!(names(&flat, "Artist/Album/Scans"), ["back.jpg"]);
        // Without --flatten each sits in its own directory, with the same number.
        let plain = tree_sc(dir.path(), false);
        assert_eq!(names(&plain, "Artist/Album"), ["Rip", "folder.jpg"]);
        assert_eq!(names(&plain, "Artist/Album/Rip/Disc"), ["x", "Scans", "Stereo.png", "list.m3u"]);
        for (p, f) in [
            ("Artist/Album/folder.jpg", "Artist/Album/folder.jpg"),
            ("Artist/Album/Rip/folder.jpg", "Artist/Album/folder (2).jpg"),
            ("Artist/Album/Rip/Disc/Scans", "Artist/Album/Scans"),
            ("Artist/Album/Rip/Disc/Scans/back.jpg", "Artist/Album/Scans/back.jpg"),
            ("Artist/Album/Rip/Disc/list.m3u", "Artist/Album/list.m3u"),
        ] {
            assert_eq!(plain.resolve(Path::new(p)), flat.resolve(Path::new(f)), "{p} / {f}");
        }
        // `..` of a sidecar folder in the collapsed directory is that directory.
        let scans = flat.resolve(Path::new("Artist/Album/Scans")).unwrap();
        assert_eq!(flat.parent_of(scans), flat.resolve(Path::new("Artist/Album")).unwrap());
        // A chain whose image is not an SACD image is hidden, sidecars included (#175).
        let bad = tempfile::tempdir().unwrap();
        other_iso(bad.path(), "Artist/Album/Rip/x.iso");
        fs::write(bad.path().join("Artist/Album/folder.jpg"), b"art").unwrap();
        assert!(names(&tree_sc(bad.path(), true), "").is_empty());
    }

    #[test]
    fn sidecar_numbers_are_found_on_a_fresh_mount() {
        let dir = source(&[
            "Artist/Album/Rip/Disc/x.iso",
            "Artist/Album/folder.jpg",
            "Artist/Album/Rip/Disc/Scans/Inner/back.jpg",
            "Artist/Box/a.iso",
            "Artist/Box/b.iso",
            "Artist/Box/cover.webp",
            "Artist/Box/Artwork/a.jpg",
            "top.m3u",
        ]);
        for flatten in [false, true] {
            let all = numbered(&tree_sc(dir.path(), flatten));
            let mut sidecars = std::collections::HashSet::new();
            for (path, ino) in &all {
                // A fresh tree, as after a restart: only the number is known.
                let t = tree_sc(dir.path(), flatten);
                let node = t.get(*ino).unwrap_or_else(|| panic!("{path} ({ino:#x}) not found"));
                assert_eq!(t.resolve(Path::new(path)), Some(*ino), "{path}");
                if matches!(&*node, Node::Sidecar(_)) {
                    sidecars.insert(*ino);
                    assert!(!path.ends_with(".dsf") && !path.ends_with(".wav"));
                }
            }
            assert_eq!(sidecars.len(), 5, "flatten {flatten}");
            // Every number distinct, but for folder art, which is also in the areas (#95).
            let mut by_ino: HashMap<u64, Vec<&String>> = HashMap::new();
            for (p, i) in &all {
                by_ino.entry(*i).or_default().push(p);
            }
            for (i, ps) in by_ino {
                assert!(
                    ps.len() == 1 || ps.iter().all(|p| p.ends_with(".jpg") || p.ends_with(".webp")),
                    "{i:#x} {ps:?}"
                );
            }
        }
    }

    #[test]
    fn a_changed_sidecar_is_stale() {
        let dir = source(&["Artist/x.iso", "Artist/folder.jpg"]);
        let t = tree_sc(dir.path(), false);
        let before = sidecar_of(&t, "Artist/folder.jpg");
        let ino = t.resolve(Path::new("Artist/folder.jpg")).unwrap();
        fs::write(dir.path().join("Artist/folder.jpg"), b"a new and longer cover").unwrap();
        let err = t.open_sidecar(&before).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ESTALE));
        // The next mount shows the new file under the same number, in a new generation.
        let t2 = tree_sc(dir.path(), false);
        assert_eq!(t2.resolve(Path::new("Artist/folder.jpg")), Some(ino));
        let after = sidecar_of(&t2, "Artist/folder.jpg");
        assert_ne!(after.generation, before.generation);
        assert_eq!(read_sidecar(&t2, "Artist/folder.jpg"), b"a new and longer cover");
    }

    #[test]
    fn links_are_followed_only_inside_source() {
        use std::os::unix::fs::symlink;
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().canonicalize().unwrap().join("src");
        fs::create_dir_all(root.join("Artist/Album/Artwork")).unwrap();
        fs::create_dir_all(root.join("Shared")).unwrap();
        fs::create_dir_all(root.join("mnt")).unwrap();
        symlink(corpus_image(), root.join("Artist/Album/x.iso")).unwrap();
        fs::write(root.join("Shared/art.jpg"), b"shared art").unwrap();
        fs::write(root.join("mnt/m.jpg"), b"in the mount").unwrap();
        fs::write(outer.path().join("secret.jpg"), b"secret").unwrap();
        let link = |to: &Path, name: &str| symlink(to, root.join("Artist/Album").join(name)).unwrap();
        link(Path::new("../../Shared/art.jpg"), "inside.jpg");
        link(&root.join("Shared/art.jpg"), "absolute.jpg");
        link(Path::new("../../../secret.jpg"), "outside.jpg");
        link(&outer.path().join("secret.jpg"), "outside-abs.jpg");
        link(Path::new("../../mnt/m.jpg"), "mount.jpg");
        link(Path::new("/etc"), "etc.jpg");
        // A folder link named Artwork is not followed either (directory links never are).
        symlink(root.join("Shared"), root.join("Artist/Scans")).unwrap();
        let t = Tree::new(SourceDir::scan_with(&root, &Sidecars::standard(), None).unwrap())
            .excluding(&root.join("mnt"));
        assert_eq!(names(&t, "Artist"), ["Album"]);
        // (An empty sidecar folder is not shown.)
        assert_eq!(names(&t, "Artist/Album"), ["x", "absolute.jpg", "inside.jpg"]);
        assert_eq!(read_sidecar(&t, "Artist/Album/inside.jpg"), b"shared art");
        assert_eq!(read_sidecar(&t, "Artist/Album/absolute.jpg"), b"shared art");
        // Nothing in the mount point is a sidecar.
        symlink(corpus_image(), root.join("mnt/x.iso")).unwrap();
        fs::write(root.join("mnt/folder.jpg"), b"").unwrap();
        let s = SourceDir::scan_with(&root, &Sidecars::standard(), Some(&root.join("mnt"))).unwrap();
        let mnt = &s.dirs.iter().find(|(n, _)| n == "mnt").unwrap().1;
        assert!(mnt.sidecars.is_empty());
    }

    fn ino_of(t: &Tree, path: &str) -> u64 {
        t.resolve(Path::new(path)).unwrap_or_else(|| panic!("{path} not found"))
    }

    fn watching(dir: &Path, sel: &str, flatten: bool) -> Tree {
        let sel = Sidecars::parse(sel).unwrap();
        let root = SourceDir::scan_with(dir, &sel, None).unwrap();
        Tree::new(root).with_flatten(flatten).watching(&sel).with_refresh(Duration::ZERO)
    }

    #[test]
    fn folder_art_is_also_in_the_areas() {
        let dir = source(ALBUM);
        let t = tree_sc(dir.path(), false);
        for area in ["Stereo", "Multichannel"] {
            let names = names(&t, &format!("Artist/Album/x/{area}"));
            // The images among the sidecars only (not playlists, folders or other files),
            // after the tracks.
            let art: Vec<_> = names.iter().filter(|n| !n.ends_with(".dsf") && !n.ends_with(".wav")).collect();
            assert_eq!(art, ["cover.PNG", "folder.jpg"], "{area}");
            assert!(names.iter().take(art.len()).all(|n| n.ends_with(".dsf") || n.ends_with(".wav")));
            // The same file under a second name: the same inode number.
            for n in ["cover.PNG", "folder.jpg"] {
                assert_eq!(
                    ino_of(&t, &format!("Artist/Album/x/{area}/{n}")),
                    ino_of(&t, &format!("Artist/Album/{n}"))
                );
            }
            assert_eq!(
                read_sidecar(&t, &format!("Artist/Album/x/{area}/folder.jpg")),
                b"Artist/Album/folder.jpg"
            );
        }
        // A folder without an image of its own gets nothing.
        assert!(!names(&t, "Artist/Album/Artwork").iter().any(|n| n.ends_with(".dsf")));
        // --sidecars decides: no image extension, no art in the areas; none at all, no change.
        for (sel, want) in [("default,-jpg,-jpeg", vec!["cover.PNG"]), ("m3u8", vec![]), ("none", vec![])] {
            let r = SourceDir::scan_with(dir.path(), &Sidecars::parse(sel).unwrap(), None).unwrap();
            let t = Tree::new(r);
            let got: Vec<_> = names(&t, "Artist/Album/x/Stereo")
                .into_iter()
                .filter(|n| !n.ends_with(".dsf") && !n.ends_with(".wav"))
                .collect();
            assert_eq!(got, want, "{sel}");
        }
        // The audio files' numbers do not depend on the art.
        let none = tree(dir.path(), false);
        for (p, ino) in numbered(&none) {
            assert_eq!(t.resolve(Path::new(&p)), Some(ino), "{p}");
        }
    }

    #[test]
    fn folder_art_in_the_areas_with_flatten_and_a_fresh_mount() {
        let dir = source(&["Artist/Album/Rip/x.iso", "Artist/Album/folder.jpg", "Artist/Album/Rip/back.png"]);
        let t = tree_sc(dir.path(), true);
        let stereo = names(&t, "Artist/Album/Stereo");
        let art = &stereo[stereo.len() - 3..stereo.len() - 1];
        assert!(art == ["back.png", "folder.jpg"] || art == ["folder.jpg", "back.png"], "{stereo:?}");
        // Found again from the number alone, as after a restart.
        let ino = ino_of(&t, "Artist/Album/Stereo/folder.jpg");
        assert_eq!(ino, ino_of(&t, "Artist/Album/folder.jpg"));
        assert!(tree_sc(dir.path(), true).get(ino).is_some());
        // With no `cover.*` among them, the first of the art is also `cover.jpg` (#148).
        assert_eq!(stereo.last().map(String::as_str), Some("cover.jpg"), "{stereo:?}");
        assert_eq!(ino_of(&t, "Artist/Album/Stereo/cover.jpg"), ino);
    }

    /// The names in `Stereo` that are not audio.
    fn art_names(dir: &Path, sel: &str) -> Vec<String> {
        let t = Tree::new(SourceDir::scan_with(dir, &Sidecars::parse(sel).unwrap(), None).unwrap());
        names(&t, "Artist/Album/x/Stereo")
            .into_iter()
            .filter(|n| !n.ends_with(".dsf") && !n.ends_with(".wav"))
            .collect()
    }

    #[test]
    fn art_not_named_cover_is_also_cover_for_mpd() {
        // 148: MPD finds folder art only in a file named `cover.*`.
        let dir = source(&["Artist/Album/x.iso", "Artist/Album/front.png", "Artist/Album/folder.JPEG"]);
        // `folder` is chosen before `front`; `jpeg` is shown as `jpg`; it is the same node.
        assert_eq!(art_names(dir.path(), "default"), ["folder.JPEG", "front.png", "cover.jpg"]);
        let t = tree_sc(dir.path(), false);
        assert_eq!(ino_of(&t, "Artist/Album/x/Stereo/cover.jpg"), ino_of(&t, "Artist/Album/folder.JPEG"));
        assert_eq!(read_sidecar(&t, "Artist/Album/x/Multichannel/cover.jpg"), b"Artist/Album/folder.JPEG");
        // The folder itself is unchanged, and so are the audio files' numbers.
        assert_eq!(names(&t, "Artist/Album"), ["x", "folder.JPEG", "front.png"]);
        // Another name when only one kind is passed through, and no alias with no art.
        assert_eq!(art_names(dir.path(), "default,-jpeg"), ["front.png", "cover.png"]);
        assert!(art_names(dir.path(), "none").is_empty());
        let plain = source(&["Artist/Album/x.iso", "Artist/Album/b.webp", "Artist/Album/a.jpg"]);
        assert_eq!(art_names(plain.path(), "default"), ["a.jpg", "b.webp", "cover.jpg"]);
    }

    #[test]
    fn art_already_named_cover_gets_no_alias() {
        for existing in ["cover.jpg", "Cover.PNG", "cover.webp"] {
            let dir = source(&[
                "Artist/Album/x.iso",
                "Artist/Album/folder.jpg",
                &format!("Artist/Album/{existing}"),
            ]);
            let got = art_names(dir.path(), "default");
            assert_eq!(got.len(), 2, "{existing}: {got:?}");
            assert!(!got.iter().any(|n| n == "cover (2).jpg"), "{got:?}");
        }
    }

    #[test]
    fn new_and_removed_files_show_without_a_remount() {
        let dir = source(ALBUM);
        let t = watching(dir.path(), "default", false);
        let before = numbered(&t);
        assert_eq!(names(&t, "Artist/Album"), ["x", "Artwork", "cover.PNG", "folder.jpg"]);
        let folder = ino_of(&t, "Artist/Album/folder.jpg");
        // Keep a file open across its removal.
        let kept = sidecar_of(&t, "Artist/Album/cover.PNG");
        let mut open = t.open_sidecar(&kept).unwrap();
        let album = dir.path().join("Artist/Album");
        fs::write(album.join("back.jpg"), b"back").unwrap();
        fs::remove_file(album.join("cover.PNG")).unwrap();
        fs::create_dir(album.join("Scans")).unwrap();
        fs::write(album.join("Scans/s1.png"), b"s1").unwrap();
        // The directory's modification time may not have moved on a coarse clock.
        let now = std::time::SystemTime::now() + Duration::from_secs(5);
        fs::File::open(&album).unwrap().set_modified(now).unwrap();
        assert_eq!(names(&t, "Artist/Album"), ["x", "Artwork", "Scans", "back.jpg", "folder.jpg"]);
        assert_eq!(read_sidecar(&t, "Artist/Album/back.jpg"), b"back");
        assert_eq!(read_sidecar(&t, "Artist/Album/Scans/s1.png"), b"s1");
        // Unchanged entries keep their numbers; the removed one is gone from lookups but the
        // open file still reads.
        assert_eq!(ino_of(&t, "Artist/Album/folder.jpg"), folder);
        for (p, ino) in &before {
            if p != "Artist/Album/cover.PNG" && !p.starts_with("Artist/Album/x/") {
                assert_eq!(t.resolve(Path::new(p)), Some(*ino), "{p}");
            }
        }
        assert_eq!(t.resolve(Path::new("Artist/Album/cover.PNG")), None);
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut open, &mut bytes).unwrap();
        assert_eq!(bytes, b"Artist/Album/cover.PNG");
        // The art in the areas follows.
        let stereo = names(&t, "Artist/Album/x/Stereo");
        assert!(
            stereo.contains(&"back.jpg".to_owned()) && !stereo.contains(&"cover.PNG".to_owned()),
            "{stereo:?}"
        );
        // A sidecar folder is re-read too.
        fs::write(album.join("Artwork/extra.jpg"), b"extra").unwrap();
        fs::File::open(album.join("Artwork")).unwrap().set_modified(now).unwrap();
        assert_eq!(names(&t, "Artist/Album/Artwork"), ["Booklet", "extra.jpg", "front.jpg", "info.pdf"]);
        // A replaced file gets new attributes under the same number.
        let ino = ino_of(&t, "Artist/Album/folder.jpg");
        fs::write(album.join("folder.jpg"), b"a longer picture").unwrap();
        fs::File::open(&album).unwrap().set_modified(now + Duration::from_secs(5)).unwrap();
        names(&t, "Artist/Album");
        assert_eq!(ino_of(&t, "Artist/Album/folder.jpg"), ino);
        assert_eq!(read_sidecar(&t, "Artist/Album/folder.jpg"), b"a longer picture");
    }

    #[test]
    fn new_images_and_folders_show_without_a_remount() {
        let dir = source(&["Artist/A/x.iso", "Artist/Empty/sub"]);
        let t = watching(dir.path(), "default", false);
        assert_eq!(names(&t, "Artist"), ["A"]);
        let a = ino_of(&t, "Artist/A/x");
        // A new album, a second image in a known one, and an image in a folder that was
        // there (empty) at the first listing.
        fs::create_dir_all(dir.path().join("Artist/B")).unwrap();
        std::os::unix::fs::symlink(corpus_image(), dir.path().join("Artist/B/y.iso")).unwrap();
        std::os::unix::fs::symlink(corpus_image(), dir.path().join("Artist/A/z.iso")).unwrap();
        std::os::unix::fs::symlink(corpus_image(), dir.path().join("Artist/Empty/sub/w.iso")).unwrap();
        let now = std::time::SystemTime::now() + Duration::from_secs(5);
        for d in ["Artist", "Artist/A", "Artist/Empty"] {
            fs::File::open(dir.path().join(d)).unwrap().set_modified(now).unwrap();
        }
        assert_eq!(names(&t, "Artist"), ["A", "B", "Empty"]);
        assert_eq!(names(&t, "Artist/A"), ["x", "z"]);
        assert_eq!(ino_of(&t, "Artist/A/x"), a);
        assert_eq!(names(&t, "Artist/B"), ["y"]);
        assert_eq!(names(&t, "Artist/Empty/sub"), ["w"]);
        // Removed: not found any more, though the number still resolves.
        fs::remove_file(dir.path().join("Artist/A/z.iso")).unwrap();
        fs::File::open(dir.path().join("Artist/A"))
            .unwrap()
            .set_modified(now + Duration::from_secs(5))
            .unwrap();
        assert_eq!(names(&t, "Artist/A"), ["x"]);
        assert!(t.get(a).is_some());
    }

    #[test]
    fn a_tree_that_does_not_watch_keeps_its_listings() {
        let dir = source(ALBUM);
        let t = tree_sc(dir.path(), false);
        let first = names(&t, "Artist/Album");
        fs::write(dir.path().join("Artist/Album/new.jpg"), b"n").unwrap();
        fs::File::open(dir.path().join("Artist/Album"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(9))
            .unwrap();
        assert_eq!(names(&t, "Artist/Album"), first);
        assert!(!t.is_live(ino_of(&t, "Artist/Album")));
    }

    #[test]
    fn flatten_follows_the_chains_sidecars() {
        let dir = source(&["Artist/Album/Rip/x.iso", "Artist/Album/folder.jpg"]);
        let t = watching(dir.path(), "default", true);
        assert_eq!(names(&t, "Artist/Album"), ["Stereo", "Multichannel", "folder.jpg"]);
        fs::write(dir.path().join("Artist/Album/list.m3u8"), b"#EXTM3U").unwrap();
        fs::write(dir.path().join("Artist/Album/Rip/back.png"), b"b").unwrap();
        let now = std::time::SystemTime::now() + Duration::from_secs(5);
        for d in ["Artist/Album", "Artist/Album/Rip"] {
            fs::File::open(dir.path().join(d)).unwrap().set_modified(now).unwrap();
        }
        assert_eq!(
            names(&t, "Artist/Album"),
            ["Stereo", "Multichannel", "folder.jpg", "list.m3u8", "back.png"]
        );
        let stereo = names(&t, "Artist/Album/Stereo");
        assert!(
            stereo.contains(&"back.png".to_owned()) && !stereo.contains(&"list.m3u8".to_owned()),
            "{stereo:?}"
        );
    }

    /// Sets the modification time of `path` (a file or directory).
    fn set_mtime(path: &Path, t: SystemTime) {
        fs::File::open(path).unwrap().set_modified(t).unwrap();
    }

    /// A source tree `Artist/Album/disc.iso` (a copy of a corpus image) with every
    /// modification time `old`.
    fn old_tree(old: SystemTime) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Artist/Album");
        fs::create_dir_all(&album).unwrap();
        fs::copy(corpus_image(), album.join("disc.iso")).unwrap();
        fs::write(album.join("folder.jpg"), b"art").unwrap();
        for p in ["Artist/Album/disc.iso", "Artist/Album/folder.jpg", "Artist/Album", "Artist", ""] {
            set_mtime(&dir.path().join(p), old);
        }
        dir
    }

    fn dir_mtimes(t: &Tree) -> Vec<SystemTime> {
        ["", "Artist", "Artist/Album", "Artist/Album/disc", "Artist/Album/disc/Stereo"]
            .iter()
            .map(|p| t.mtime(&t.get(t.resolve(Path::new(p)).unwrap()).unwrap()))
            .collect()
    }

    #[test]
    fn directory_mtimes_show_the_newest_below() {
        // #123: every directory up to the root shows the newest mtime a track file of its
        // images could have (its image's moved by the output version, and a step for a file
        // with text its disc lacks, #185), not its source directory's own.
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        for flatten in [false, true] {
            let dir = old_tree(old);
            let t = watching(dir.path(), "default", flatten);
            let newest = old + ident::newest_offset(true, true, 0);
            assert!(newest > old);
            let paths = if flatten {
                vec!["", "Artist", "Artist/Album", "Artist/Album/Stereo"]
            } else {
                vec!["", "Artist", "Artist/Album", "Artist/Album/disc", "Artist/Album/disc/Stereo"]
            };
            let ino = |p: &str| t.resolve(Path::new(p)).unwrap();
            for p in &paths {
                assert_eq!(t.mtime(&t.get(ino(p)).unwrap()), newest, "{p} (flatten {flatten})");
            }
            let file = t.children(ino(paths[paths.len() - 1])).unwrap();
            let newest_file = file.iter().map(|e| t.mtime(&t.get(e.ino).unwrap())).max().unwrap();
            assert!(newest_file > old && newest_file <= newest);
            // DSF only: the DSF output version's offset.
            let dsf = Tree::with_tuning(SourceDir::scan(dir.path()).unwrap(), Tuning::default());
            assert_eq!(dsf.mtime(&dsf.get(ROOT).unwrap()), old + ident::newest_offset(true, false, 0));
            // Directories keep generation 0 and their numbers: only the time moves.
            assert_eq!(t.generation(&t.get(ROOT).unwrap()), 0);
        }
    }

    /// #185: a folder newer than its images' files moves too, or a scanner that prunes unchanged
    /// folders never reaches a file below it whose time moved.
    #[test]
    fn a_folder_newer_than_its_images_still_moves_one_step() {
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let dir = old_tree(old);
        let newer = old + Duration::from_secs(10_000_000);
        let album = dir.path().join("Artist/Album");
        fs::File::open(&album).unwrap().set_modified(newer).unwrap();
        let t = watching(dir.path(), "default", false);
        for p in ["", "Artist", "Artist/Album"] {
            let shown = t.mtime(&t.get(t.resolve(Path::new(p)).unwrap()).unwrap());
            assert_eq!(shown, newer + ident::FILLED_STEP, "{p:?}");
        }
    }

    #[test]
    fn a_change_deep_below_moves_every_ancestor_and_matches_a_remount() {
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let dir = old_tree(old);
        let t = watching(dir.path(), "default", false);
        let before = dir_mtimes(&t);
        // A new playlist deep in the tree, newer than anything shown: nothing is listed
        // again, the root's attributes are asked for (as a scan does first).
        let later = old + Duration::from_secs(10 * 86_400);
        let album = dir.path().join("Artist/Album");
        fs::write(album.join("new.m3u8"), b"#EXTM3U\n").unwrap();
        set_mtime(&album.join("new.m3u8"), later);
        set_mtime(&album, later - Duration::from_secs(1));
        t.sweep();
        let after = dir_mtimes(&t);
        // The source directories up to the root move; the image's own folders do not (the
        // playlist is not shown in them).
        // One step on (#185), as for any folder with an image below.
        let step = ident::FILLED_STEP;
        assert!(after[..3].iter().all(|m| *m == later + step), "{after:?}");
        assert_eq!(after[3..], before[3..]);
        // A fresh mount shows the same times (#17: stable across remounts).
        let again = watching(dir.path(), "default", false);
        assert_eq!(dir_mtimes(&again), after);
        // Nothing changed: the next sweep moves nothing.
        t.sweep();
        assert_eq!(dir_mtimes(&t), after);
        // A re-read of a listing moves its ancestors too: new art (in the area's folder art).
        let art = later + Duration::from_secs(5);
        fs::write(album.join("back.png"), b"b").unwrap();
        set_mtime(&album.join("back.png"), art);
        set_mtime(&album, later);
        t.children(t.resolve(Path::new("Artist/Album")).unwrap());
        assert!(dir_mtimes(&t)[..3].iter().all(|m| *m == art + step));
        // A new artist folder at the top with a new image.
        let second = dir.path().join("Second");
        fs::create_dir(&second).unwrap();
        fs::copy(corpus_image(), second.join("d.iso")).unwrap();
        let img = mtime_of(&second.join("d.iso")).unwrap();
        t.sweep();
        assert_eq!(dir_mtimes(&t)[0], img + ident::newest_offset(true, true, 0));
    }

    #[test]
    fn a_folder_never_shows_a_new_mtime_with_an_old_listing() {
        // rc.2 on a NAS over SMB: the sweep moved a folder's mtime before the folder's
        // throttled re-read was due, so a client that caches a listing until the mtime moves
        // kept the old listing for good (#123, #101).
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        for flatten in [false, true] {
            let dir = old_tree(old);
            let sel = Sidecars::parse("default").unwrap();
            let root = SourceDir::scan_with(dir.path(), &sel, None).unwrap();
            let t =
                Tree::new(root).with_flatten(flatten).watching(&sel).with_refresh(Duration::from_secs(3600));
            let album = t.resolve(Path::new("Artist/Album")).unwrap();
            let names =
                || -> Vec<OsString> { t.children(album).unwrap().iter().map(|e| e.name.clone()).collect() };
            let before = t.mtime(&t.get(album).unwrap());
            assert!(!names().iter().any(|n| n == "new.m3u8"));
            fs::write(dir.path().join("Artist/Album/new.m3u8"), b"#EXTM3U\n").unwrap();
            t.sweep();
            let after = t.mtime(&t.get(album).unwrap());
            assert!(after > before, "flatten {flatten}");
            assert!(names().iter().any(|n| n == "new.m3u8"), "flatten {flatten}: {:?}", names());
        }
    }

    #[test]
    fn a_folder_listed_first_after_a_change_shows_it() {
        // rc.3: a folder not listed since the mount was listed from the mount's scan, so a
        // sidecar added before its first listing never showed (#101, #123).
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        for flatten in [false, true] {
            let dir = old_tree(old);
            fs::create_dir(dir.path().join("Artist/Album/Artwork")).unwrap();
            fs::write(dir.path().join("Artist/Album/Artwork/back.png"), b"b").unwrap();
            let t = watching(dir.path(), "default", flatten);
            assert!(t.resolve(Path::new("Artist/Album")).is_some());
            fs::write(dir.path().join("Artist/Album/new.m3u8"), b"#EXTM3U\n").unwrap();
            fs::write(dir.path().join("Artist/Album/Artwork/front.png"), b"f").unwrap();
            let names = |p: &str| -> Vec<OsString> {
                t.children(t.resolve(Path::new(p)).unwrap()).unwrap().iter().map(|e| e.name.clone()).collect()
            };
            assert!(names("Artist/Album").iter().any(|n| n == "new.m3u8"), "flatten {flatten}");
            assert!(names("Artist/Album/Artwork").iter().any(|n| n == "front.png"), "flatten {flatten}");
        }
    }
}
