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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::SystemTime;

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
        let scan = Scan { sidecars, exclude };
        Ok(Self::scan_dir(source, 0, &scan, false)?.unwrap_or(SourceDir {
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
                if depth + 1 >= MAX_DEPTH {
                    continue;
                }
                let folder = note && !sidecar::hidden(&name) && (everything || scan.sidecars.folder(&name));
                match Self::scan_dir(&path, depth + 1, scan, folder) {
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

fn is_iso(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso"))
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
    Flat(PathBuf, Vec<Sidecar>),
    /// A sidecar folder passed through (`Artwork/`): shows its content.
    Folder(Vec<Sidecar>),
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
    children: Mutex<Option<Arc<Vec<Entry>>>>,
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
        let node =
            Node::Dir(Dir { kind: DirKind::Source(root), mtime, parent: ROOT, children: Mutex::new(None) });
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
        }
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

    /// The entries of directory `ino`, made on first use. `None` if it is not a directory.
    pub fn children(&self, ino: u64) -> Option<Arc<Vec<Entry>>> {
        let node = self.get(ino)?;
        let Node::Dir(dir) = &*node else { return None };
        let mut guard = dir.children.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = &*guard {
            return Some(c.clone());
        }
        let entries = Arc::new(self.load(dir, ino));
        *guard = Some(entries.clone());
        Some(entries)
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
            Node::Dir(d) => d.mtime,
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

    fn load(&self, dir: &Dir, parent: u64) -> Vec<Entry> {
        let mut dedup = Deduper::with_reserved([".", ".."]);
        let mut out = Vec::new();
        match &dir.kind {
            DirKind::Source(src) => {
                for (name, sub) in &src.dirs {
                    let mtime = fs::metadata(&sub.path).and_then(|m| m.modified()).unwrap_or(dir.mtime);
                    // The top-level directories (usually artists) are never collapsed.
                    let flat = if self.flatten && parent != ROOT { sub.single_image() } else { None };
                    // A collapsed directory is the image's directory: it has its number.
                    let (kind, ino) = match flat {
                        Some(image) => {
                            (DirKind::Flat(image.to_owned(), sub.chain_sidecars()), self.ids.image(image))
                        }
                        None => (DirKind::Source(sub.clone()), self.ids.dir(&sub.path)),
                    };
                    if !self.add(ino, Node::Dir(Dir { kind, mtime, parent, children: Mutex::new(None) })) {
                        continue;
                    }
                    out.push(Entry { name: dedup.unique(name, false), ino, is_dir: true });
                }
                // Opening an image reads its master and area TOCs: a few small reads,
                // worth doing in parallel on a slow (network) source.
                let opened: Vec<(PathBuf, Option<Disc>)> = src
                    .images
                    .par_iter()
                    .map(|p| {
                        let id = self.next_disc.fetch_add(1, Ordering::Relaxed);
                        match Disc::open(p, id, self.tuning) {
                            Ok(d) => (p.clone(), Some(d)),
                            Err(e) => {
                                log::info!("{}: not shown: {e}", p.display());
                                (p.clone(), None)
                            }
                        }
                    })
                    .collect();
                for (path, disc) in opened {
                    let Some(disc) = disc else { continue };
                    let stem = path.file_stem().map_or_else(|| OsString::from("disc"), OsStr::to_owned);
                    let mtime = disc.mtime;
                    let ino = self.ids.image(&path);
                    let dir = Dir {
                        kind: DirKind::Image(Arc::new(disc)),
                        mtime,
                        parent,
                        children: Mutex::new(None),
                    };
                    if !self.add(ino, Node::Dir(dir)) {
                        continue;
                    }
                    out.push(Entry { name: dedup.unique(&stem, false), ino, is_dir: true });
                }
                self.list_sidecars(&src.sidecars, parent, &mut dedup, &mut out);
            }
            DirKind::Folder(list) => self.list_sidecars(list, parent, &mut dedup, &mut out),
            DirKind::Image(disc) => {
                for (i, area) in disc.toc.areas.iter().enumerate() {
                    let name = match area.slot {
                        Slot::TwoChannel => STEREO,
                        Slot::Multichannel => MULTICHANNEL,
                    };
                    let ino = ident::area(parent, area.slot);
                    let dir = Dir {
                        kind: DirKind::Area(disc.clone(), i),
                        mtime: disc.mtime,
                        parent,
                        children: Mutex::new(None),
                    };
                    if !self.add(ino, Node::Dir(dir)) {
                        log::info!("{}: second {} area: not shown", disc.path.display(), area.slot);
                        continue;
                    }
                    out.push(Entry { name: dedup.unique(OsStr::new(name), false), ino, is_dir: true });
                }
            }
            DirKind::Flat(path, sidecars) => {
                let id = self.next_disc.fetch_add(1, Ordering::Relaxed);
                match Disc::open(path, id, self.tuning) {
                    // Listed exactly as the image's own directory would be, then the
                    // sidecars of the chain.
                    Ok(disc) => {
                        let image = Dir {
                            kind: DirKind::Image(Arc::new(disc)),
                            mtime: dir.mtime,
                            parent: dir.parent,
                            children: Mutex::new(None),
                        };
                        out = self.load(&image, parent);
                        for e in &out {
                            dedup.unique(&e.name, false);
                        }
                    }
                    Err(e) => log::info!("{}: not shown: {e}", path.display()),
                }
                self.list_sidecars(sidecars, parent, &mut dedup, &mut out);
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
            }
        }
        out
    }

    /// Lists the sidecars `list` in directory `parent`, after its other entries: each is
    /// looked at now (in parallel, as images are opened) and shown if it is a regular file
    /// (or folder) where SOURCE allows; numbered by its source path.
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
                        Node::Dir(Dir {
                            kind: DirKind::Folder(inner.clone()),
                            mtime: m.mtime,
                            parent,
                            children: Mutex::new(None),
                        })
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
            if !self.add(ino, node) {
                continue;
            }
            out.push(Entry { name: dedup.unique(&sc.name, !is_dir), ino, is_dir });
        }
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
        // With the default options, every DSF file shows its image's mtime (issue #64), and
        // every WAV file its image's moved forward by one output version per WAV_VERSION
        // after the first (25,203 s each; WAV_VERSION 2 since #84's converter change).
        let image = fs::metadata(dir.path().join("Artist/x.iso")).unwrap().modified().unwrap();
        let wav = image + std::time::Duration::from_secs(25_203);
        assert!(base.iter().all(|f| f.3 == if f.0.ends_with(".dsf") { image } else { wav }));
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
            let mut sidecars = 0;
            for (path, ino) in &all {
                // A fresh tree, as after a restart: only the number is known.
                let t = tree_sc(dir.path(), flatten);
                let node = t.get(*ino).unwrap_or_else(|| panic!("{path} ({ino:#x}) not found"));
                assert_eq!(t.resolve(Path::new(path)), Some(*ino), "{path}");
                if matches!(&*node, Node::Sidecar(_)) {
                    sidecars += 1;
                    assert!(!path.ends_with(".dsf") && !path.ends_with(".wav"));
                }
            }
            assert_eq!(sidecars, 5, "flatten {flatten}");
            // Every number distinct.
            let mut inos: Vec<u64> = all.iter().map(|(_, i)| *i).collect();
            inos.sort_unstable();
            inos.dedup();
            assert_eq!(inos.len(), all.len());
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
}
