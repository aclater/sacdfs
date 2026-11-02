// SPDX-License-Identifier: GPL-3.0-or-later

//! The persistent seek index lets go of the discs of images removed from SOURCE or renamed
//! (#153), after saving what they read. Its own binary: the index cache is process-wide.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::index_cache::{self, IndexCache};
use sacdfs::sidecar::Sidecars;
use sacdfs::track::Reader;
use sacdfs::tree::{Output, ROOT, SourceDir, Tree};

#[test]
fn the_seek_index_cache_lets_go_of_removed_images() {
    let cache_dir = tempfile::tempdir().unwrap();
    assert!(index_cache::install(Arc::new(IndexCache::open(cache_dir.path()).unwrap())));
    let cache = index_cache::global().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().canonicalize().unwrap();
    let img = common::corpus().join("images/dst-stereo-5ch-2054.iso");
    std::fs::create_dir(lib.join("kept")).unwrap();
    std::os::unix::fs::symlink(&img, lib.join("kept/disc.iso")).unwrap();
    let tree = Tree::with_options(SourceDir::scan(&lib).unwrap(), Tuning::default(), Output::dsf_only())
        .watching(&Sidecars::none())
        .with_refresh(Duration::ZERO);
    let settle = || {
        std::thread::sleep(Duration::from_millis(20));
        tree.sweep();
    };
    let engine = Engine::new(1, 1 << 20).unwrap();
    let _ = tree.all_files();
    cache.flush().unwrap();
    assert_eq!(cache.live_count(), 1);
    let mut counts = Vec::new();
    for n in 0..10 {
        let album = lib.join(format!("album-{n}"));
        std::fs::create_dir(&album).unwrap();
        std::os::unix::fs::symlink(&img, album.join("disc.iso")).unwrap();
        settle();
        // Read the new album's files, so that its seek index has something to save.
        for (path, f) in tree.all_files() {
            if path.starts_with(format!("album-{n}")) {
                Reader::new(f.clone()).read(&engine, f.size / 2, 4096).unwrap();
            }
        }
        cache.flush().unwrap();
        let writes = cache.writes();
        std::fs::remove_file(album.join("disc.iso")).unwrap();
        std::fs::remove_dir(&album).unwrap();
        settle();
        let _ = tree.children(ROOT);
        // The engine's frame cache holds no disc; the reads above are done.
        cache.flush().unwrap();
        counts.push(cache.live_count());
        assert!(cache.writes() >= writes, "a removed disc's index is saved, not lost");
    }
    eprintln!("discs held by the seek index cache after each add+remove: {counts:?}");
    assert!(counts.iter().all(|&c| c == 1), "the seek index cache keeps removed discs: {counts:?}");
}
