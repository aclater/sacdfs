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
//! made when the area directory is first looked into. Inode numbers are handed out as nodes
//! are made and never reused, so they are stable for the life of the mount.
//!
//! With [`Tree::with_flatten`], a chain of source directories below the top level whose only
//! content is one image is shown as its outermost directory holding the image's areas. The
//! chains are found from the names walked at mount time ([`SourceDir::single_image`]); the
//! image is still opened only when that directory is first looked into.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use rayon::prelude::*;
use scarletbook::Slot;
use scarletbook::audio::check_track;

use crate::disc::{Disc, Tuning};
use crate::names::{Deduper, track_file_name};
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
}

impl SourceDir {
    /// Scans `source`: a single image file, or a directory searched recursively for
    /// `*.iso` files (any case). Symbolic links to files are followed; symbolic links to
    /// directories are not.
    pub fn scan(source: &Path) -> io::Result<SourceDir> {
        let meta = fs::metadata(source)?;
        if meta.is_file() {
            let parent = source.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            return Ok(SourceDir { path: parent, dirs: Vec::new(), images: vec![source.to_owned()] });
        }
        Ok(Self::scan_dir(source, 0)?.unwrap_or(SourceDir {
            path: source.to_owned(),
            dirs: Vec::new(),
            images: Vec::new(),
        }))
    }

    fn scan_dir(dir: &Path, depth: usize) -> io::Result<Option<SourceDir>> {
        let mut dirs = Vec::new();
        let mut images = Vec::new();
        let mut entries: Vec<_> = fs::read_dir(dir)?.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            if ft.is_dir() {
                if depth + 1 >= MAX_DEPTH {
                    continue;
                }
                match Self::scan_dir(&path, depth + 1) {
                    Ok(Some(sub)) => dirs.push((e.file_name(), sub)),
                    Ok(None) => {}
                    Err(err) => log::warn!("{}: {err}", path.display()),
                }
            } else if is_iso(&path) && (ft.is_file() || (ft.is_symlink() && path.is_file())) {
                images.push(path);
            }
        }
        if dirs.is_empty() && images.is_empty() {
            return Ok(None);
        }
        Ok(Some(SourceDir { path: dir.to_owned(), dirs, images }))
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
    /// image's areas, like [`DirKind::Image`], once the image is opened on first use.
    Flat(PathBuf),
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
}

/// The tree: every node made so far, by inode number.
#[derive(Debug)]
pub struct Tree {
    nodes: RwLock<Vec<Arc<Node>>>,
    next_disc: AtomicU32,
    tuning: Tuning,
    output: Output,
    flatten: bool,
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
        let node =
            Node::Dir(Dir { kind: DirKind::Source(root), mtime, parent: ROOT, children: Mutex::new(None) });
        Tree {
            nodes: RwLock::new(vec![Arc::new(node)]),
            next_disc: AtomicU32::new(1),
            tuning,
            output,
            flatten: false,
        }
    }

    /// The same tree, with chains of directories below the top level that hold a single
    /// image collapsed into their outermost directory when `flatten` is set (see the
    /// module documentation). Off by default.
    #[must_use]
    pub fn with_flatten(mut self, flatten: bool) -> Tree {
        self.flatten = flatten;
        self
    }

    /// The node with inode number `ino`.
    pub fn get(&self, ino: u64) -> Option<Arc<Node>> {
        let i = usize::try_from(ino.checked_sub(1)?).ok()?;
        self.nodes.read().unwrap_or_else(|p| p.into_inner()).get(i).cloned()
    }

    /// Number of nodes made so far.
    pub fn len(&self) -> usize {
        self.nodes.read().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Whether the tree has only its root (never true: the root always exists).
    pub fn is_empty(&self) -> bool {
        false
    }

    fn add(&self, node: Node) -> u64 {
        let mut nodes = self.nodes.write().unwrap_or_else(|p| p.into_inner());
        nodes.push(Arc::new(node));
        nodes.len() as u64
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
                    let kind = match flat {
                        Some(image) => DirKind::Flat(image.to_owned()),
                        None => DirKind::Source(sub.clone()),
                    };
                    let ino = self.add(Node::Dir(Dir { kind, mtime, parent, children: Mutex::new(None) }));
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
                    let ino = self.add(Node::Dir(Dir {
                        kind: DirKind::Image(Arc::new(disc)),
                        mtime,
                        parent,
                        children: Mutex::new(None),
                    }));
                    out.push(Entry { name: dedup.unique(&stem, false), ino, is_dir: true });
                }
            }
            DirKind::Image(disc) => {
                for (i, area) in disc.toc.areas.iter().enumerate() {
                    let name = match area.slot {
                        Slot::TwoChannel => STEREO,
                        Slot::Multichannel => MULTICHANNEL,
                    };
                    let ino = self.add(Node::Dir(Dir {
                        kind: DirKind::Area(disc.clone(), i),
                        mtime: disc.mtime,
                        parent,
                        children: Mutex::new(None),
                    }));
                    out.push(Entry { name: dedup.unique(OsStr::new(name), false), ino, is_dir: true });
                }
            }
            DirKind::Flat(path) => {
                let id = self.next_disc.fetch_add(1, Ordering::Relaxed);
                match Disc::open(path, id, self.tuning) {
                    // Listed exactly as the image's own directory would be.
                    Ok(disc) => {
                        let image = Dir {
                            kind: DirKind::Image(Arc::new(disc)),
                            mtime: dir.mtime,
                            parent: dir.parent,
                            children: Mutex::new(None),
                        };
                        return self.load(&image, parent);
                    }
                    Err(e) => log::info!("{}: not shown: {e}", path.display()),
                }
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
                        let ino = self.add(Node::File(Arc::new(file)));
                        out.push(Entry { name: dedup.unique(OsStr::new(&name), true), ino, is_dir: false });
                    }
                }
            }
        }
        out
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
    /// to a corpus image, `.txt` an ordinary file, anything else a directory.
    fn source(paths: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for p in paths {
            let path = dir.path().join(p);
            if p.ends_with(".iso") {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(corpus_image(), &path).unwrap();
            } else if p.ends_with(".txt") {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, b"x").unwrap();
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
}
