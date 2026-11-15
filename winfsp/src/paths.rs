// SPDX-License-Identifier: GPL-3.0-or-later

//! Paths to inodes (#419). WinFsp asks by path; the core answers by inode number.
//!
//! Each folder's listing is kept here with every name in its Windows form ([`crate::names`])
//! and its case-folded key, so that a path is walked one name at a time without asking the
//! core, and a lookup in any case finds the stored name. A folder that the core re-reads when
//! its source changes (#101) is kept for as long as the core lets a client keep it, then
//! listed again; what changed in it is queued for Windows's change notification.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sacdfs::tree::ROOT;
use sacdfs::vfs::{self, Attr, EntryAttr, Kind};

use crate::names;

/// What the core is asked for: a folder's entries and how long they may be kept.
pub trait Lister {
    /// The entries of folder `ino`, with their names as the core has them.
    fn list(&self, ino: u64) -> Result<Vec<(String, EntryAttr)>, vfs::Error>;
    /// The attributes of `ino` (the root's, which no listing holds).
    fn attr(&self, ino: u64) -> Result<Attr, vfs::Error>;
    /// How long folder `ino`'s listing may be kept: `None` for good.
    fn keep_for(&self, ino: u64) -> Option<Duration>;
}

/// One entry of a folder, as Windows sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    /// Its name on Windows.
    pub name: String,
    pub attr: Attr,
}

/// A folder's entries, and its names by case-folded key.
#[derive(Debug)]
pub struct Folder {
    /// The folder's own path on Windows, `\` for the root, in stored case.
    pub path: String,
    pub entries: Vec<Child>,
    by_key: HashMap<String, usize>,
    listed: Instant,
    keep_for: Option<Duration>,
    /// Names left out, each with why: logged once by [`Paths`].
    hidden: Vec<(String, &'static str)>,
}

impl Folder {
    /// The folder at `path` with the core's `listing` of it: names in their Windows form, and
    /// of names that fold alike or are too long for Windows, the later ones left out.
    pub fn new(path: String, listing: Vec<(String, EntryAttr)>, keep_for: Option<Duration>) -> Folder {
        let mut folder = Folder {
            path,
            entries: Vec::with_capacity(listing.len()),
            by_key: HashMap::with_capacity(listing.len()),
            listed: Instant::now(),
            keep_for,
            hidden: Vec::new(),
        };
        for (name, entry) in listing {
            if name == "." || name == ".." {
                continue;
            }
            let name = names::to_windows(&name);
            if names::too_long(&name) {
                folder.hidden.push((name, "a name of more than 255 UTF-16 units"));
                continue;
            }
            let key = names::fold(&name);
            if folder.by_key.contains_key(&key) {
                folder.hidden.push((name, "differs from another name only in case"));
                continue;
            }
            folder.by_key.insert(key, folder.entries.len());
            folder.entries.push(Child { name, attr: entry.attr });
        }
        folder
    }

    /// The entry named `name`, in any case.
    pub fn get(&self, name: &str) -> Option<&Child> {
        self.by_key.get(&names::fold(name)).map(|&i| &self.entries[i])
    }

    fn fresh(&self, now: Instant) -> bool {
        self.keep_for.is_none_or(|t| now.duration_since(self.listed) < t)
    }

    /// The path of entry `name` of this folder.
    pub fn child_path(&self, name: &str) -> String {
        if self.path == "\\" { format!("\\{name}") } else { format!("{}\\{name}", self.path) }
    }
}

/// What a path names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub attr: Attr,
    /// The path in stored case.
    pub path: String,
}

/// Why a path names nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// The last name is missing from its folder.
    Name,
    /// A folder on the way is missing, or is a file.
    Path,
    /// The core failed.
    Core(vfs::Error),
}

/// A change in a folder Windows is told of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The changed entry's path, in stored case.
    pub path: String,
    pub kind: Kind,
    pub what: ChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}

/// The folders listed so far.
#[derive(Debug)]
pub struct Paths<L> {
    lister: L,
    folders: Mutex<HashMap<u64, Entry>>,
    changes: Mutex<Vec<Change>>,
    /// Hidden names already logged, so that a folder listed every second logs each once.
    warned: Mutex<HashSet<String>>,
}

#[derive(Debug)]
struct Entry {
    folder: Arc<Folder>,
    used: Instant,
}

/// Folders not used for this long are let go of (and so are no longer watched).
const UNUSED: Duration = Duration::from_secs(600);

impl<L: Lister> Paths<L> {
    pub fn new(lister: L) -> Paths<L> {
        Paths { lister, folders: Mutex::default(), changes: Mutex::default(), warned: Mutex::default() }
    }

    pub fn lister(&self) -> &L {
        &self.lister
    }

    /// Folder `ino`, at `path`: the listing kept if still fresh (or `fresh` is false), else
    /// listed again, with what changed since queued (see [`Paths::take_changes`]).
    pub fn folder(&self, ino: u64, path: &str, fresh: bool) -> Result<Arc<Folder>, vfs::Error> {
        let now = Instant::now();
        {
            let mut folders = self.folders.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = folders.get_mut(&ino)
                && (!fresh || e.folder.fresh(now))
            {
                e.used = now;
                return Ok(e.folder.clone());
            }
        }
        self.relist(ino, path)
    }

    /// Folder `ino` listed again now; what changed since the listing kept, if any, is queued.
    fn relist(&self, ino: u64, path: &str) -> Result<Arc<Folder>, vfs::Error> {
        let old = self.folders.lock().unwrap_or_else(|p| p.into_inner()).get(&ino).map(|e| e.folder.clone());
        let path = old.as_ref().map_or(path, |f| f.path.as_str()).to_owned();
        let folder = Arc::new(Folder::new(path, self.lister.list(ino)?, self.lister.keep_for(ino)));
        if !folder.hidden.is_empty() {
            let mut warned = self.warned.lock().unwrap_or_else(|p| p.into_inner());
            for (name, why) in &folder.hidden {
                if warned.insert(folder.child_path(name)) {
                    log::warn!("{}: {why}: not shown", folder.child_path(name));
                }
            }
        }
        if let Some(old) = old {
            let changes = diff(&old, &folder);
            if !changes.is_empty() {
                self.changes.lock().unwrap_or_else(|p| p.into_inner()).extend(changes);
            }
        }
        self.folders
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(ino, Entry { folder: folder.clone(), used: Instant::now() });
        Ok(folder)
    }

    /// The entry `path` names (a Windows path from the volume's root, `\`-separated, any case).
    pub fn resolve(&self, path: &str) -> Result<Found, Missing> {
        let parts: Vec<&str> = path.split('\\').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            let attr = self.lister.attr(ROOT).map_err(Missing::Core)?;
            return Ok(Found { attr, path: "\\".to_owned() });
        }
        let (mut ino, mut at) = (ROOT, "\\".to_owned());
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let folder = self.folder(ino, &at, true).map_err(|e| match e {
                vfs::Error::NotDir | vfs::Error::NotFound => Missing::Path,
                e => Missing::Core(e),
            })?;
            // A name not in a listing kept may have appeared since: list again, once.
            let child = match folder.get(part) {
                Some(c) => c.clone(),
                None if folder.keep_for.is_some() => {
                    let again = self.folder_again(ino, &folder)?;
                    again.get(part).cloned().ok_or(if last { Missing::Name } else { Missing::Path })?
                }
                None => return Err(if last { Missing::Name } else { Missing::Path }),
            };
            let path = folder.child_path(&child.name);
            if last {
                return Ok(Found { attr: child.attr, path });
            }
            if child.attr.kind != Kind::Dir {
                return Err(Missing::Path);
            }
            ino = child.attr.ino;
            at = path;
        }
        unreachable!("the loop returns at the last name")
    }

    /// `folder` listed again now, unless it was listed under a second ago.
    fn folder_again(&self, ino: u64, folder: &Folder) -> Result<Arc<Folder>, Missing> {
        if folder.listed.elapsed() < Duration::from_secs(1) {
            return self.folder(ino, &folder.path, false).map_err(Missing::Core);
        }
        // Listed again with the old listing kept to compare: what changed is queued.
        self.relist(ino, &folder.path).map_err(Missing::Core)
    }

    /// Lists again every folder kept that the core re-reads and that is due, and lets go of
    /// folders not used for a while. What changed is queued.
    pub fn refresh(&self) {
        let now = Instant::now();
        let due: Vec<(u64, String)> = {
            let mut folders = self.folders.lock().unwrap_or_else(|p| p.into_inner());
            folders.retain(|_, e| now.duration_since(e.used) < UNUSED);
            folders
                .iter()
                .filter(|(_, e)| !e.folder.fresh(now))
                .map(|(&ino, e)| (ino, e.folder.path.clone()))
                .collect()
        };
        for (ino, path) in due {
            match self.folder(ino, &path, true) {
                Ok(_) => {}
                // Gone (its source removed): its parent's listing says so.
                Err(_) => {
                    self.folders.lock().unwrap_or_else(|p| p.into_inner()).remove(&ino);
                }
            }
        }
    }

    /// The changes queued since the last call.
    pub fn take_changes(&self) -> Vec<Change> {
        std::mem::take(&mut *self.changes.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// What changed between two listings of a folder.
fn diff(old: &Folder, new: &Folder) -> Vec<Change> {
    let mut out = Vec::new();
    for c in &old.entries {
        match new.get(&c.name) {
            None => out.push(Change {
                path: old.child_path(&c.name),
                kind: c.attr.kind,
                what: ChangeKind::Removed,
            }),
            // Renamed to another case: Windows sees the old name go and the new one come.
            Some(n) if n.name != c.name => {
                out.push(Change {
                    path: old.child_path(&c.name),
                    kind: c.attr.kind,
                    what: ChangeKind::Removed,
                });
                out.push(Change {
                    path: new.child_path(&n.name),
                    kind: n.attr.kind,
                    what: ChangeKind::Added,
                });
            }
            Some(n) if n.attr.ino != c.attr.ino || n.attr.kind != c.attr.kind => {
                out.push(Change {
                    path: old.child_path(&c.name),
                    kind: c.attr.kind,
                    what: ChangeKind::Removed,
                });
                out.push(Change {
                    path: new.child_path(&n.name),
                    kind: n.attr.kind,
                    what: ChangeKind::Added,
                });
            }
            Some(n) if n.attr.size != c.attr.size || n.attr.mtime != c.attr.mtime => {
                out.push(Change {
                    path: new.child_path(&n.name),
                    kind: n.attr.kind,
                    what: ChangeKind::Modified,
                });
            }
            Some(_) => {}
        }
    }
    for n in &new.entries {
        if old.get(&n.name).is_none() {
            out.push(Change { path: new.child_path(&n.name), kind: n.attr.kind, what: ChangeKind::Added });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::SystemTime;

    fn attr(ino: u64, kind: Kind, size: u64) -> Attr {
        let t = SystemTime::UNIX_EPOCH;
        Attr {
            ino,
            size,
            blocks: 0,
            atime: t,
            mtime: t,
            ctime: t,
            crtime: t,
            kind,
            perm: 0o444,
            nlink: 1,
            uid: 0,
            gid: 0,
            blksize: 4096,
        }
    }

    fn entry(ino: u64, kind: Kind, size: u64) -> EntryAttr {
        EntryAttr { ttl: Duration::ZERO, attr: attr(ino, kind, size), generation: 0 }
    }

    /// A folder's entries: name, inode, kind, size.
    type Entries = Vec<(String, u64, Kind, u64)>;

    /// A tree: folders by inode.
    struct Fake {
        dirs: Mutex<HashMap<u64, Entries>>,
        lists: AtomicUsize,
        keep: Option<Duration>,
    }

    impl Lister for &Fake {
        fn list(&self, ino: u64) -> Result<Vec<(String, EntryAttr)>, vfs::Error> {
            self.lists.fetch_add(1, Ordering::Relaxed);
            let dirs = self.dirs.lock().unwrap();
            let d = dirs.get(&ino).ok_or(vfs::Error::NotFound)?;
            Ok(d.iter().map(|(n, i, k, s)| (n.clone(), entry(*i, *k, *s))).collect())
        }
        fn attr(&self, ino: u64) -> Result<Attr, vfs::Error> {
            Ok(attr(ino, Kind::Dir, 0))
        }
        fn keep_for(&self, _ino: u64) -> Option<Duration> {
            self.keep
        }
    }

    fn fake(keep: Option<Duration>) -> Fake {
        let mut dirs = HashMap::new();
        dirs.insert(
            ROOT,
            vec![
                ("Bach: Suites".to_owned(), 2, Kind::Dir, 0),
                ("readme.txt".to_owned(), 3, Kind::File, 10),
                ("README.TXT".to_owned(), 4, Kind::File, 11),
            ],
        );
        dirs.insert(2, vec![("01 - Prelude.dsf".to_owned(), 5, Kind::File, 1000)]);
        Fake { dirs: Mutex::new(dirs), lists: AtomicUsize::new(0), keep }
    }

    #[test]
    fn walks_in_any_case_and_returns_the_stored_case() {
        let f = fake(None);
        let p = Paths::new(&f);
        let found = p.resolve("\\BACH\u{f022} SUITES\\01 - prelude.DSF").unwrap();
        assert_eq!(found.attr.ino, 5);
        assert_eq!(found.path, "\\Bach\u{f022} Suites\\01 - Prelude.dsf");
        assert_eq!(p.resolve("\\").unwrap().path, "\\");
        assert_eq!(p.resolve("").unwrap().attr.ino, ROOT);
    }

    #[test]
    fn missing_names_and_paths() {
        let f = fake(None);
        let p = Paths::new(&f);
        assert_eq!(p.resolve("\\nothing"), Err(Missing::Name));
        assert_eq!(p.resolve("\\nothing\\more"), Err(Missing::Path));
        assert_eq!(p.resolve("\\readme.txt\\more"), Err(Missing::Path));
    }

    #[test]
    fn of_names_that_fold_alike_the_first_is_shown() {
        let f = fake(None);
        let p = Paths::new(&f);
        assert_eq!(p.resolve("\\README.txt").unwrap().attr.ino, 3);
        let root = p.folder(ROOT, "\\", true).unwrap();
        assert_eq!(root.entries.len(), 2);
    }

    #[test]
    fn a_folder_kept_for_good_is_listed_once() {
        let f = fake(None);
        let p = Paths::new(&f);
        for _ in 0..5 {
            p.resolve("\\Bach\u{f022} Suites\\01 - Prelude.dsf").unwrap();
        }
        assert_eq!(f.lists.load(Ordering::Relaxed), 2);
        // A name missing from a folder kept for good is missing.
        assert_eq!(p.resolve("\\new"), Err(Missing::Name));
        assert_eq!(f.lists.load(Ordering::Relaxed), 2);
    }

    /// A name missing from a listing kept for a while lists the folder again, and the changes
    /// found then are queued as well.
    #[test]
    fn a_miss_lists_again_and_keeps_the_changes() {
        let f = fake(Some(Duration::from_secs(3600)));
        let p = Paths::new(&f);
        p.resolve("\\readme.txt").unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        f.dirs.lock().unwrap().get_mut(&ROOT).unwrap().push(("New.iso".to_owned(), 9, Kind::Dir, 0));
        assert_eq!(p.resolve("\\NEW.iso").unwrap().path, "\\New.iso");
        assert_eq!(
            p.take_changes(),
            vec![Change { path: "\\New.iso".into(), kind: Kind::Dir, what: ChangeKind::Added }]
        );
    }

    #[test]
    fn changes_in_a_live_folder_are_queued() {
        let f = fake(Some(Duration::ZERO));
        let p = Paths::new(&f);
        p.resolve("\\Bach\u{f022} Suites").unwrap();
        {
            let mut dirs = f.dirs.lock().unwrap();
            let root = dirs.get_mut(&ROOT).unwrap();
            root.retain(|e| e.0 != "README.TXT");
            root[1].3 = 20; // readme.txt grew
            root.push(("New.iso".to_owned(), 9, Kind::Dir, 0));
        }
        // A new name is found at once, listing again.
        assert_eq!(p.resolve("\\new.ISO").unwrap().path, "\\New.iso");
        p.refresh();
        let mut changes = p.take_changes();
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            changes,
            vec![
                Change { path: "\\New.iso".into(), kind: Kind::Dir, what: ChangeKind::Added },
                Change { path: "\\readme.txt".into(), kind: Kind::File, what: ChangeKind::Modified },
            ]
        );
        assert!(p.take_changes().is_empty());
    }
}
