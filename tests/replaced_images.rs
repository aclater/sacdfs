// SPDX-License-Identifier: GPL-3.0-or-later

//! Image files replaced while mounted. A copy of the same disc put in place by a rename (rsync
//! without `--inplace`, sync tools) or rewritten in place reads on from the new file. Another
//! disc, or a file whose TOC does not read, at the path is not read through the layout of the
//! disc listed before: reads of the files made from that disc fail, and the folder's next
//! listing opens the image as a new one.

// Windows refuses to rename over an image file that is open, as these tests do.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use sacdfs::disc::{Disc, Tuning};
use sacdfs::engine::Engine;
use sacdfs::sidecar::Sidecars;
use sacdfs::track::Reader;
use sacdfs::tree::{Output, SourceDir, Tree};

/// A disc, and another disc whose image has exactly the same length.
const IMAGE: &str = "images/no-igl.iso";
const OTHER: &str = "images/no-text.iso";

/// A watching tree of `lib` that looks at its source on every call.
fn watching(lib: &Path, flatten: bool) -> Tree {
    Tree::with_options(SourceDir::scan(lib).unwrap(), Tuning::default(), Output::dsf_only())
        .with_flatten(flatten)
        .watching(&Sidecars::none())
        .with_refresh(Duration::ZERO)
}

/// A library with one image, a copy of corpus image `image`, at `artist/album/disc.iso`.
fn library(image: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(lib.join("artist/album")).unwrap();
    std::fs::copy(common::corpus().join(image), lib.join("artist/album/disc.iso")).unwrap();
    (dir, lib)
}

/// Puts `content` at `path` as rsync does without `--inplace`: written to a temporary file in
/// the same folder, given the modification time of `like`, then renamed over `path`.
fn resync(path: &Path, content: &[u8], like: SystemTime) {
    let tmp = path.with_file_name(".disc.iso.Xa9b8c");
    std::fs::write(&tmp, content).unwrap();
    std::fs::File::options().write(true).open(&tmp).unwrap().set_modified(like).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

/// Waits, sweeping and listing as a scanner does, until `shown` holds for the tree.
fn until(tree: &Tree, what: &str, shown: impl Fn(&Tree) -> bool) {
    let start = std::time::Instant::now();
    loop {
        tree.sweep();
        if shown(tree) {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "the tree never showed {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The first track file of the tree, with the bytes it reads.
fn first_file(tree: &Tree) -> (Arc<sacdfs::track::TrackFile>, Vec<u8>) {
    let (_, file) = tree.all_files().into_iter().next().expect("a track file");
    let bytes = Reader::new(file.clone()).read(&Engine::new(1, 1 << 20).unwrap(), 0, file.size).unwrap().0;
    (file, bytes)
}

/// The disc listed for the tree's first track file.
fn listed_disc(tree: &Tree) -> Arc<Disc> {
    tree.all_files().into_iter().next().expect("a track file").1.disc.clone()
}

/// The first track file of `image` as a mount of it alone shows it.
fn reference(image: &str) -> Vec<u8> {
    let (_dir, lib) = library(image);
    first_file(&watching(&lib, false)).1
}

#[test]
fn an_image_resynced_through_a_rename_reads_on() {
    sacdfs::handles::global().set_revalidate_every(Duration::ZERO);
    for flatten in [false, true] {
        let (_dir, lib) = library(IMAGE);
        let path = lib.join("artist/album/disc.iso");
        let tree = watching(&lib, flatten);
        let (file, whole) = first_file(&tree);
        let disc = Arc::downgrade(&file.disc);
        let open = Reader::new(file.clone());
        let half = whole.len() as u64 / 2;
        let (first, _) = open.read(&Engine::new(1, 1 << 20).unwrap(), 0, half).unwrap();
        let reads = file.disc.sector_reads();
        drop(file);

        // The same disc, copied over it again: another file at the path, same time.
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let content = std::fs::read(&path).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        resync(&path, &content, mtime);

        // The open file reads the rest from the new file, through a new engine (nothing cached).
        let (rest, _) =
            open.read(&Engine::new(1, 1 << 20).unwrap(), half, whole.len() as u64 - half).unwrap();
        assert_eq!([first, rest].concat(), whole, "flatten {flatten}");
        let now = disc.upgrade().unwrap();
        assert!(now.sector_reads() > reads, "the rest was read from the image");
        drop(now);

        // A later listing of the folder keeps the disc listed: nothing to open again.
        until(&tree, "the re-synced folder", |t| t.all_files().len() == tree.all_files().len());
        let listed = listed_disc(&tree);
        assert!(
            std::ptr::eq(Arc::as_ptr(&listed), disc.as_ptr()),
            "flatten {flatten}: the disc was opened again"
        );
        assert_eq!(first_file(&tree).1, whole);

        // Rewritten in place with the same content and a new time: reads on as well.
        std::fs::write(&path, &content).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime + Duration::from_secs(60))
            .unwrap();
        let (again, _) = open.read(&Engine::new(1, 1 << 20).unwrap(), 0, whole.len() as u64).unwrap();
        assert_eq!(again, whole, "flatten {flatten}");
    }
}

/// With rsync itself, where it is installed: `rsync -a --ignore-times` copies the image again
/// into a temporary file and renames it over the one listed.
#[test]
fn an_image_resynced_by_rsync_reads_on() {
    if std::process::Command::new("rsync").arg("--version").output().is_err() {
        common::report_skip("an_image_resynced_by_rsync_reads_on", "no rsync");
        return;
    }
    sacdfs::handles::global().set_revalidate_every(Duration::ZERO);
    let (_dir, lib) = library(IMAGE);
    let path = lib.join("artist/album/disc.iso");
    let (_src, from) = library(IMAGE);
    let tree = watching(&lib, false);
    let (file, whole) = first_file(&tree);
    let open = Reader::new(file.clone());
    let before = sacdfs::platform::file_id_at(&path).unwrap().1;
    let status = std::process::Command::new("rsync")
        .args(["-a", "--ignore-times"])
        .arg(from.join("artist/album/disc.iso"))
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    assert_ne!(sacdfs::platform::file_id_at(&path).unwrap().1, before, "rsync put another file in place");
    let (again, _) = open.read(&Engine::new(1, 1 << 20).unwrap(), 0, whole.len() as u64).unwrap();
    assert_eq!(again, whole);
    until(&tree, "the re-synced folder", |t| !t.all_files().is_empty());
    assert!(Arc::ptr_eq(&listed_disc(&tree), &file.disc), "the disc was opened again");
}

#[test]
fn another_disc_at_the_path_is_refused_then_listed_anew() {
    sacdfs::handles::global().set_revalidate_every(Duration::ZERO);
    let other = std::fs::read(common::corpus().join(OTHER)).unwrap();
    let expected = reference(OTHER);
    for flatten in [false, true] {
        let (_dir, lib) = library(IMAGE);
        let path = lib.join("artist/album/disc.iso");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), other.len() as u64, "the same length");
        let tree = watching(&lib, flatten);
        let (file, whole) = first_file(&tree);
        assert_ne!(whole, expected, "two different discs");
        let open = Reader::new(file.clone());
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        resync(&path, &other, mtime);

        // Nothing is read from the other disc through the layout of the one listed.
        let e = open.read(&Engine::new(1, 1 << 20).unwrap(), 0, whole.len() as u64).unwrap_err();
        assert!(e.to_string().contains("another disc"), "flatten {flatten}: {e}");
        assert_eq!(Reader::new(file.clone()).read(&Engine::new(1, 1 << 20).unwrap(), 0, 4096).ok(), None);

        // The next listing opens the image anew: its files are the other disc's.
        until(&tree, "the new disc", |t| {
            !t.all_files().is_empty() && !Arc::ptr_eq(&listed_disc(t), &file.disc)
        });
        assert_eq!(first_file(&tree).1, expected, "flatten {flatten}");
    }
}

#[test]
fn a_file_whose_toc_does_not_read_is_refused() {
    sacdfs::handles::global().set_revalidate_every(Duration::ZERO);
    let (_dir, lib) = library(IMAGE);
    let path = lib.join("artist/album/disc.iso");
    let tree = watching(&lib, false);
    let (file, whole) = first_file(&tree);
    let open = Reader::new(file.clone());
    // The same length, the master TOC and its copies zeros.
    let mut damaged = std::fs::read(&path).unwrap();
    damaged[510 * 2048..540 * 2048].fill(0);
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    resync(&path, &damaged, mtime);
    assert!(open.read(&Engine::new(1, 1 << 20).unwrap(), 0, whole.len() as u64).is_err());
    // Listed again, the image is left out like any file that is not a disc.
    until(&tree, "the damaged image left out", |t| t.all_files().is_empty());
    // Put back, it is listed again.
    std::thread::sleep(Duration::from_millis(20));
    resync(&path, &std::fs::read(common::corpus().join(IMAGE)).unwrap(), mtime);
    until(&tree, "the image put back", |t| !t.all_files().is_empty());
    assert_eq!(first_file(&tree).1, whole);
}

/// A link at the image's path pointed at another file: its content is checked like any other.
#[test]
fn a_repointed_link_is_checked() {
    sacdfs::handles::global().set_revalidate_every(Duration::ZERO);
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().canonicalize().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(lib.join("artist/album")).unwrap();
    let image = outside.path().join("disc.bin");
    let other = outside.path().join("other.bin");
    std::fs::copy(common::corpus().join(IMAGE), &image).unwrap();
    std::fs::write(&other, vec![0x5a; std::fs::metadata(&image).unwrap().len() as usize]).unwrap();
    let path = lib.join("artist/album/disc.iso");
    std::os::unix::fs::symlink(&image, &path).unwrap();
    let tree = watching(&lib, false);
    let (file, whole) = first_file(&tree);
    let open = Reader::new(file.clone());
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&other, &path).unwrap();
    let e = open.read(&Engine::new(1, 1 << 20).unwrap(), 0, whole.len() as u64).unwrap_err();
    assert!(e.to_string().contains("another disc"), "{e}");
}

/// Checking a re-synced file reads only its TOC again, and is not counted as audio reads: a
/// whole track reads the image as often as without the re-sync.
#[test]
fn a_resync_adds_no_audio_reads() {
    sacdfs::handles::global().set_revalidate_every(Duration::ZERO);
    let reads = |resynced: bool| {
        let (_dir, lib) = library(IMAGE);
        let path = lib.join("artist/album/disc.iso");
        let tree = watching(&lib, false);
        let (_, file) = tree.all_files().into_iter().next().unwrap();
        if resynced {
            let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
            resync(&path, &std::fs::read(&path).unwrap(), mtime);
        }
        let before = file.disc.sector_reads();
        let bytes =
            Reader::new(file.clone()).read(&Engine::new(1, 1 << 20).unwrap(), 0, file.size).unwrap().0;
        (file.disc.sector_reads() - before, bytes)
    };
    let (plain, a) = reads(false);
    let (resynced, b) = reads(true);
    assert_eq!(a, b);
    assert!(plain > 0);
    assert_eq!(resynced, plain, "audio reads of the image");
}

/// An image file of another size put in place: the files of the old disc give errors, and the
/// folder's next listing shows the new one.
#[test]
fn an_image_of_another_size_is_listed_anew() {
    sacdfs::handles::global().set_revalidate_every(Duration::ZERO);
    const LONGER: &str = "images/full-text.iso";
    let expected = reference(LONGER);
    let (_dir, lib) = library(IMAGE);
    let path = lib.join("artist/album/disc.iso");
    let tree = watching(&lib, false);
    let (file, whole) = first_file(&tree);
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    resync(&path, &std::fs::read(common::corpus().join(LONGER)).unwrap(), mtime);
    assert!(
        Reader::new(file.clone()).read(&Engine::new(1, 1 << 20).unwrap(), 0, whole.len() as u64).is_err()
    );
    until(&tree, "the new disc", |t| !t.all_files().is_empty() && !Arc::ptr_eq(&listed_disc(t), &file.disc));
    assert_eq!(first_file(&tree).1, expected);
}
