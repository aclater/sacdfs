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

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

use rayon::prelude::*;
use scarletbook::Slot;
use scarletbook::audio::check_track;

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
}

impl Default for Output {
    /// DSF and WAV side by side, WAV at 176.4 kHz.
    fn default() -> Self {
        Output { dsf: true, wav: true, samples_per_frame: 2352 }
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

/// Most nested source directories followed.
const MAX_DEPTH: usize = 64;

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
            });
        }
        let scan = Scan { sidecars, exclude, skip_exclude: false };
        Ok(Self::scan_dir(source, 0, &scan, false, None)?.unwrap_or(SourceDir {
            path: source.to_owned(),
            dirs: Vec::new(),
            images: Vec::new(),
            sidecars: Vec::new(),
        }))
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
                        dirs.push((name, sub.clone()));
                        continue;
                    }
                    if let Some(f) =
                        k.sidecars.iter().find(|s| s.name == name && matches!(s.kind, SidecarKind::Folder(_)))
                    {
                        sidecars.push(f.clone());
                        continue;
                    }
                }
                let folder = note && !sidecar::hidden(&name) && (everything || scan.sidecars.folder(&name));
                match Self::scan_dir(&path, depth + 1, scan, folder, None) {
                    Ok(Some(sub)) if sub.holds_images() => dirs.push((name, sub)),
                    Ok(Some(sub)) if folder => {
                        sidecars.push(Sidecar { name, path, kind: SidecarKind::Folder(sub.sidecars) });
                    }
                    Ok(_) => {}
                    Err(err) => log::warn!("{}: {err}", path.display()),
                }
            } else if is_iso(&path) && (ft.is_file() || (ft.is_symlink() && path.is_file())) {
                images.push(path);
            } else if note
                && (ft.is_file() || ft.is_symlink())
                && !sidecar::hidden(&name)
                && (everything || scan.sidecars.file(&name))
            {
                let kind = if ft.is_symlink() { SidecarKind::Link } else { SidecarKind::File };
                sidecars.push(Sidecar { name, path, kind });
            }
        }
        if dirs.is_empty() && images.is_empty() && sidecars.is_empty() {
            return Ok(None);
        }
        Ok(Some(SourceDir { path: dir.to_owned(), dirs, images, sidecars }))
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
        });
        out.path.clone_from(&self.path);
        Ok(out)
    }

    /// What to watch for this directory's listing to change: its own modification time, and
    /// those of its subdirectories that are not shown (they hold no image yet: one that
    /// gains one changes nothing in this directory).
    fn watch(&self, exclude: Option<&Path>) -> Vec<(PathBuf, Option<SystemTime>)> {
        let mut out = vec![(self.path.clone(), mtime_of(&self.path))];
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

    /// The directories of a collapsed chain, outermost first.
    fn chain_dirs(&self) -> Vec<PathBuf> {
        let mut out = vec![self.path.clone()];
        if let ([(_, sub)], []) = (self.dirs.as_slice(), self.images.as_slice()) {
            out.extend(sub.chain_dirs());
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

/// The modification time of the directory `path`, if it can be read.
fn mtime_of(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn is_iso(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso"))
}

/// Whether a directory is re-read when its source changes (on a watching tree).
fn live(d: &Dir) -> bool {
    match d.kind {
        DirKind::Source(_) | DirKind::Flat(..) | DirKind::Folder(..) => true,
        DirKind::Area(..) => d.art.is_some(),
        DirKind::Image(_) => false,
    }
}

/// What a directory shows.
#[derive(Debug)]
pub enum DirKind {
    /// A directory of the source tree.
    Source(SourceDir),
    /// The directory of one image: one subdirectory per area.
    Image(Arc<Disc>),
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
    /// The modification time of the source directory when it was last re-read, in
    /// nanoseconds since the epoch; 0 until then (`mtime` is shown).
    shown: AtomicU64,
    children: Mutex<Option<Box<Listing>>>,
}

impl Dir {
    fn new(kind: DirKind, mtime: SystemTime, parent: u64) -> Dir {
        Dir { kind, mtime, parent, art: None, shown: AtomicU64::new(0), children: Mutex::new(None) }
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
}

#[derive(Debug)]
enum Snapshot {
    /// A source directory, or the top of a collapsed chain.
    Dir(SourceDir),
    /// The content of a sidecar folder.
    Folder(Vec<Sidecar>),
}

/// What a listing being made depends on.
#[derive(Default)]
struct Cx {
    watch: Vec<(PathBuf, Option<SystemTime>)>,
    snapshot: Option<Snapshot>,
    art_version: u64,
    head: usize,
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
    /// A directory.
    Dir(Dir),
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
        // Hashes every name walked once (no file is opened), so that numbers made later can
        // be found again from their key.
        let ids = Ids::new(&root);
        let node = Node::Dir(Dir::new(DirKind::Source(root), mtime, ROOT));
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
        }
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
            return Some(n.clone());
        }
        self.find(ino)
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
        let mut prev = guard.take();
        if let Some(l) = prev.as_mut() {
            if !self.stale(dir, l) {
                let entries = l.entries.clone();
                *guard = prev;
                return Some(entries);
            }
        }
        let mut cx = Cx::default();
        let entries = Arc::new(self.load(dir, ino, prev.as_deref(), &mut cx));
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
        }));
        Some(entries)
    }

    /// Whether `l`, the listing of `dir`, is out of date: its source changed. Looks at the
    /// source at most once per refresh interval.
    fn stale(&self, dir: &Dir, l: &mut Listing) -> bool {
        if !self.watching || !live(dir) {
            return false;
        }
        if let (DirKind::Area(..), Some(art)) = (&dir.kind, &dir.art) {
            return art.current(self).1 != l.art_version;
        }
        if l.checked.elapsed() < self.refresh {
            return false;
        }
        l.checked = Instant::now();
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
            Node::Dir(d) => match d.shown.load(Ordering::Relaxed) {
                0 => d.mtime,
                n => SystemTime::UNIX_EPOCH + Duration::from_nanos(n),
            },
            Node::File(f) => ident::mtime(f, self.wav_mtime_offset),
            Node::Sidecar(s) => s.meta.mtime,
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
        let reread = self.watching && prev.is_some();
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
                        if !self.add(ino, Node::Dir(Dir::new(kind, mtime, parent))) {
                            continue;
                        }
                    }
                    out.push(Entry { name: dedup.unique(name, false), ino, is_dir: true });
                }
                // Opening an image reads its master and area TOCs: a few small reads,
                // worth doing in parallel on a slow (network) source.
                let new: Vec<&PathBuf> =
                    src.images.iter().filter(|p| !self.exists(self.ids.image(p))).collect();
                let mut opened: HashMap<&PathBuf, Disc> = new
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
                for path in &src.images {
                    let ino = self.ids.image(path);
                    if !self.exists(ino) {
                        let Some(disc) = opened.remove(path) else { continue };
                        let mtime = disc.mtime;
                        let image =
                            Dir::new(DirKind::Image(Arc::new(disc)), mtime, parent).with_art(art.clone());
                        if !self.add(ino, Node::Dir(image)) {
                            continue;
                        }
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
            DirKind::Image(disc) => {
                for (i, area) in disc.toc.areas.iter().enumerate() {
                    let name = match area.slot {
                        Slot::TwoChannel => STEREO,
                        Slot::Multichannel => MULTICHANNEL,
                    };
                    let ino = ident::area(parent, area.slot);
                    let area_dir = Dir::new(DirKind::Area(disc.clone(), i), disc.mtime, parent)
                        .with_art(dir.art.clone());
                    if !self.add(ino, Node::Dir(area_dir)) {
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
                                let mtime = dir.mtime;
                                let art = self.art_of(chain.chain_dirs(), &sidecars);
                                let img = Dir::new(DirKind::Image(Arc::new(disc)), mtime, dir.parent)
                                    .with_art(Some(art));
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
                let Some(area) = disc.toc.areas.get(*i) else { return out };
                for track in &area.tracks {
                    if let Err(e) = check_track(area, track, disc.toc.sectors) {
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
                        let Some(file) = TrackFile::with_format(disc.clone(), *i, track, format, spf) else {
                            continue;
                        };
                        let name =
                            track_file_name(track.number, track.text.title.as_deref(), format.extension());
                        let ino = ident::file(parent, area.slot, track.number, format, self.wav_flavour);
                        if !self.add(ino, Node::File(Arc::new(file))) {
                            continue;
                        }
                        out.push(Entry { name: dedup.unique(OsStr::new(&name), true), ino, is_dir: false });
                    }
                }
                // The folder's art, as the same files as in the folder (#95), after the tracks.
                if let Some(art) = &dir.art {
                    let (items, version) = art.current(self);
                    cx.art_version = version;
                    self.list_sidecars(&items, parent, &mut dedup, &mut out);
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
                        Node::Dir(Dir::new(DirKind::Folder(sc.path.clone(), inner.clone()), m.mtime, parent))
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
        // Images that are not SACD images still count (decided from names); the collapsed
        // folder shows up, empty, as a folder of such images does without the option.
        let dir = tempfile::tempdir().unwrap();
        for p in ["Artist/A/Rip/x.iso", "Artist/B/y.iso"] {
            let path = dir.path().join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![0u8; 4096]).unwrap();
        }
        let t = tree(dir.path(), true);
        assert_eq!(names(&t, "Artist"), ["A", "B"]);
        assert_eq!(t.next_disc.load(Ordering::Relaxed), 1, "listing opened an image");
        assert!(names(&t, "Artist/A").is_empty());
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
        // A collapsed image that is not an SACD image still shows its sidecars.
        let bad = tempfile::tempdir().unwrap();
        fs::create_dir_all(bad.path().join("Artist/Album/Rip")).unwrap();
        fs::write(bad.path().join("Artist/Album/Rip/x.iso"), vec![0u8; 4096]).unwrap();
        fs::write(bad.path().join("Artist/Album/folder.jpg"), b"art").unwrap();
        assert_eq!(names(&tree_sc(bad.path(), true), "Artist/Album"), ["folder.jpg"]);
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
        fs::write(root.join("mnt/x.iso"), b"").unwrap();
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
        assert!(
            stereo.ends_with(&["back.png".to_owned(), "folder.jpg".to_owned()])
                || stereo.ends_with(&["folder.jpg".to_owned(), "back.png".to_owned()]),
            "{stereo:?}"
        );
        // Found again from the number alone, as after a restart.
        let ino = ino_of(&t, "Artist/Album/Stereo/folder.jpg");
        assert_eq!(ino, ino_of(&t, "Artist/Album/folder.jpg"));
        assert!(tree_sc(dir.path(), true).get(ino).is_some());
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
}
