// SPDX-License-Identifier: GPL-3.0-or-later

//! The directory tree of a SOURCE with hostile images under hostile file names: every entry
//! is a name the kernel accepts, unique in its directory, that finds its own entry again, and
//! every file's tag and the bytes around its audio can be read.
//!
//! Input, separated by [`SEP`](sacdfs_fuzz::SEP): the file name of a first image; of a second;
//! the bytes from the master TOC on, as in `disc_text`, of both. [`SEP`](sacdfs_fuzz::SEP) can
//! be part of the image bytes: only the first two split.

#![no_main]

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use sacdfs::names::NAME_MAX;
use sacdfs::tree::{Node, ROOT, SourceDir, Tree};
use sacdfs::verify::id3;
use sacdfs_fuzz::{LEAD_IN, check_name, fields, scratch_dir};

fn dir() -> &'static PathBuf {
    static D: OnceLock<PathBuf> = OnceLock::new();
    D.get_or_init(|| scratch_dir("tree"))
}

fn write_image(path: &Path, tail: &[u8]) -> bool {
    let write = || -> std::io::Result<()> {
        let f = std::fs::File::create(path)?;
        f.set_len(LEAD_IN + tail.len() as u64)?;
        f.write_all_at(tail, LEAD_IN)
    };
    write().is_ok()
}

/// An image's file name from the input: within NAME_MAX, ending in `.iso`.
fn image_name(raw: &[u8]) -> Option<Vec<u8>> {
    let mut name: Vec<u8> = raw.iter().copied().filter(|&b| b != b'/' && b != 0).collect();
    name.truncate(NAME_MAX - 4);
    name.extend_from_slice(b".iso");
    Some(name)
}

fn walk(tree: &Tree, ino: u64, depth: usize) {
    assert!(depth < 8, "a tree deeper than its source can be");
    let Some(entries) = tree.children(ino) else { return };
    let mut names = HashSet::new();
    for e in entries.iter() {
        check_name(e.name.as_bytes());
        assert!(names.insert(&e.name), "{:?} twice in one directory", e.name);
        assert_eq!(tree.lookup(ino, &e.name), Some(e.ino), "{:?}", e.name);
        if e.is_dir {
            walk(tree, e.ino, depth + 1);
        } else if let Some(node) = tree.get(e.ino) {
            if let Node::File(f) = &*node {
                let _ = id3::frames(f.tag());
                let head = f.assemble(0, f.header_len(), 0, &[]);
                assert_eq!(head.len() as u64, f.header_len().min(f.size));
                let tail = f.assemble(f.data_end(), f.size - f.data_end(), 0, &[]);
                assert_eq!(tail.len() as u64, f.size - f.data_end());
                let _ = tree.generation(&node);
            }
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let [first, second, tail] = fields::<3>(data);
    let dir = dir();
    let (Some(a), Some(b)) = (image_name(first), image_name(second)) else { return };
    let (a, b) = (dir.join(OsStr::from_bytes(&a)), dir.join(OsStr::from_bytes(&b)));
    let written = write_image(&a, tail) && write_image(&b, tail);
    if written {
        if let Ok(root) = SourceDir::scan(dir) {
            walk(&Tree::new(root), ROOT, 0);
        }
    }
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
});
