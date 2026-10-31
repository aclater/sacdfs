// SPDX-License-Identifier: GPL-3.0-or-later

//! The directory tree of the mount and its inode numbers.
//!
//! ```text
//! MOUNT/<relative dir of the image>/<image file stem>/{Stereo,Multichannel}/NN - title.dsf
//! ```
//!
//! At mount time only the names of the source tree are walked, to find the directories
//! that hold `*.iso` files. A source directory's images are opened (their master and area
//! TOCs read) when the directory is first looked into; an area's files and their tags are
//! made when the area directory is first looked into. Inode numbers are handed out as nodes
//! are made and never reused, so they are stable for the life of the mount.

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
use crate::track::TrackFile;

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
}

impl Tree {
    /// A tree with `root` at its root.
    pub fn new(root: SourceDir) -> Tree {
        Self::with_tuning(root, Tuning::default())
    }

    /// A tree whose discs search for frames with `tuning`.
    pub fn with_tuning(root: SourceDir, tuning: Tuning) -> Tree {
        let mtime = fs::metadata(&root.path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
        let node =
            Node::Dir(Dir { kind: DirKind::Source(root), mtime, parent: ROOT, children: Mutex::new(None) });
        Tree { nodes: RwLock::new(vec![Arc::new(node)]), next_disc: AtomicU32::new(1), tuning }
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
                    let ino = self.add(Node::Dir(Dir {
                        kind: DirKind::Source(sub.clone()),
                        mtime,
                        parent,
                        children: Mutex::new(None),
                    }));
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
                    let Some(file) = TrackFile::new(disc.clone(), *i, track) else { continue };
                    let name = track_file_name(track.number, track.text.title.as_deref());
                    let ino = self.add(Node::File(Arc::new(file)));
                    out.push(Entry { name: dedup.unique(OsStr::new(&name), true), ino, is_dir: false });
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
