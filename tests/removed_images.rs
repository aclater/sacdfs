// SPDX-License-Identifier: GPL-3.0-or-later

//! Images removed from SOURCE or renamed are freed from the tree (#153): their nodes and
//! their discs, once no open file holds them. Files open on them read on until released.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

use sacdfs::disc::{Disc, Tuning};
use sacdfs::engine::Engine;
use sacdfs::sidecar::Sidecars;
use sacdfs::track::Reader;
use sacdfs::tree::{Output, ROOT, SourceDir, Tree};

const IMAGE: &str = "images/dsd-stereo-mch-2048.iso";

/// A watching tree of `lib` that looks at its source on every call.
fn watching(lib: &Path) -> Tree {
    Tree::with_options(SourceDir::scan(lib).unwrap(), Tuning::default(), Output::dsf_only())
        .watching(&Sidecars::none())
        .with_refresh(Duration::ZERO)
}

/// Lets the source's mtimes move, then lets the tree see the change, as a scan does.
fn settle(tree: &Tree) {
    std::thread::sleep(Duration::from_millis(20));
    tree.sweep();
}

/// Lists everything, as a scanner does (opens the images and makes their files), and
/// returns the discs listed.
fn scan(tree: &Tree) -> Vec<Weak<Disc>> {
    let mut discs: Vec<Weak<Disc>> = Vec::new();
    for (_, f) in tree.all_files() {
        let d = Arc::downgrade(&f.disc);
        if !discs.iter().any(|x| Weak::ptr_eq(x, &d)) {
            discs.push(d);
        }
    }
    discs
}

fn alive(discs: &[Weak<Disc>]) -> usize {
    discs.iter().filter(|d| d.strong_count() > 0).count()
}

#[test]
fn removed_and_renamed_images_are_freed() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().canonicalize().unwrap();
    let img = common::corpus().join(IMAGE);
    // One album that stays, so that the tree is not empty.
    std::fs::create_dir(lib.join("kept")).unwrap();
    std::os::unix::fs::symlink(&img, lib.join("kept/disc.iso")).unwrap();
    let tree = watching(&lib);
    let kept = scan(&tree);
    let _ = tree.children(ROOT);
    let base = tree.len();
    let mut sizes = Vec::new();
    let mut discs = Vec::new();
    for n in 0..20 {
        // Added, listed, then removed (the reproducer of #153).
        let album = lib.join(format!("album-{n}"));
        std::fs::create_dir(&album).unwrap();
        std::os::unix::fs::symlink(&img, album.join("disc.iso")).unwrap();
        settle(&tree);
        discs.extend(scan(&tree));
        // Renamed as a tagger does: the folder, then the image in it.
        let renamed = lib.join(format!("album-{n} (renamed)"));
        std::fs::rename(&album, &renamed).unwrap();
        settle(&tree);
        discs.extend(scan(&tree));
        std::fs::rename(renamed.join("disc.iso"), renamed.join("other.iso")).unwrap();
        settle(&tree);
        discs.extend(scan(&tree));
        std::fs::remove_file(renamed.join("other.iso")).unwrap();
        std::fs::remove_dir(&renamed).unwrap();
        settle(&tree);
        let _ = tree.children(ROOT);
        sizes.push(tree.len());
    }
    eprintln!("nodes after each add, renames and remove (baseline {base}): {sizes:?}");
    assert!(sizes.iter().all(|&s| s == base), "nodes of removed images are kept: {sizes:?}, baseline {base}");
    assert_eq!(discs.len(), 20 * 3 * 2, "every listing saw the kept disc and the new one");
    // Only the kept album's disc is alive, however often it was listed.
    let removed: Vec<_> =
        discs.iter().filter(|d| !kept.iter().any(|k| Weak::ptr_eq(k, d))).cloned().collect();
    assert_eq!(removed.len(), 20 * 3, "each add and rename opened its image");
    assert_eq!(alive(&removed), 0, "discs of removed images are kept");
    assert_eq!(alive(&kept), 1);
    // The album that stayed is still there.
    assert!(tree.resolve(Path::new("kept/disc/Stereo")).is_some());
}

#[test]
fn an_open_file_reads_on_after_its_image_is_removed() {
    // Every read checks the image's path, as one more than a second after the removal does.
    sacdfs::handles::global().set_revalidate_every(Duration::ZERO);
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().canonicalize().unwrap();
    std::fs::create_dir(lib.join("album")).unwrap();
    std::os::unix::fs::symlink(common::corpus().join(IMAGE), lib.join("album/disc.iso")).unwrap();
    let tree = watching(&lib);
    let (path, file) = tree.all_files().into_iter().next().unwrap();
    let ino = tree.resolve(&path).unwrap();
    let whole = Reader::new(file.clone()).read(&Engine::new(1, 1 << 20).unwrap(), 0, file.size).unwrap().0;
    // Opened, as `open` makes a handle, and read in part.
    let open = Reader::new(file.clone());
    let disc = Arc::downgrade(&file.disc);
    drop(file);
    let engine = Engine::new(1, 1 << 20).unwrap();
    let half = whole.len() as u64 / 2;
    let (first, _) = open.read(&engine, 0, half).unwrap();

    std::fs::remove_file(lib.join("album/disc.iso")).unwrap();
    std::fs::remove_dir(lib.join("album")).unwrap();
    settle(&tree);
    assert!(tree.children(ROOT).unwrap().is_empty());
    assert!(tree.loaded(ino).is_none(), "the removed file's node is freed");
    assert_eq!(tree.len(), 1, "only the root is left");
    assert!(tree.get(ino).is_none(), "the removed file is unknown");

    // The open file reads the rest, from a new engine (nothing cached), and the same bytes.
    let engine = Engine::new(1, 1 << 20).unwrap();
    let (rest, _) = open.read(&engine, half, whole.len() as u64 - half).unwrap();
    assert_eq!([first, rest].concat(), whole);
    assert!(disc.strong_count() > 0, "the open file holds its disc");
    // Released: the disc goes.
    drop(open);
    drop(engine);
    assert_eq!(disc.strong_count(), 0, "the disc outlives its last open file");
}
