// SPDX-License-Identifier: GPL-3.0-or-later

//! A listing made while the process has no descriptor left: the images that could not be
//! opened are left out of it, but a listing after the retry wait (1 s at first), once
//! descriptors are free again, shows them, with no change to SOURCE. Before, the listing was kept as it was until the source
//! folder changed.
//!
//! A test binary of its own, with one test: it lowers the process's open files limit and
//! uses up every descriptor under it, which would fail any other test running beside it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use common::*;
use nix::sys::resource::{Resource, getrlimit, setrlimit};
use sacdfs::disc::Tuning;
use sacdfs::sidecar::Sidecars;
use sacdfs::tree::{Output, SourceDir, Tree};

/// Descriptors open in this process now.
fn open_now() -> u64 {
    std::fs::read_dir("/proc/self/fd").unwrap().count() as u64
}

#[test]
fn a_listing_short_of_descriptors_is_complete_at_the_next_listing() {
    let base = tempfile::tempdir().unwrap();
    let album = base.path().join("Album");
    std::fs::create_dir_all(&album).unwrap();
    let ids = ["dsd-stereo-mch-2048", "dsd-1ch", "dst-features-2ch-6ch"];
    for id in ids {
        std::os::unix::fs::symlink(
            corpus().join(format!("images/{id}.iso")),
            album.join(format!("{id}.iso")),
        )
        .unwrap();
    }
    let tree = Tree::with_options(
        SourceDir::scan_with(base.path(), &Sidecars::standard(), None).unwrap(),
        Tuning::default(),
        Output::default(),
    );
    let ino = tree.resolve(Path::new("Album")).unwrap();
    let listed = || {
        let mut v: Vec<String> =
            tree.children(ino).unwrap().iter().map(|e| e.name.to_string_lossy().into_owned()).collect();
        v.sort();
        v
    };

    // No descriptor left: every one under a lowered soft limit is taken.
    let (soft, hard) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
    setrlimit(Resource::RLIMIT_NOFILE, open_now() + 16, hard).unwrap();
    let mut fill = Vec::new();
    while let Ok(f) = std::fs::File::open("/dev/null") {
        fill.push(f);
    }
    let short = listed();
    drop(fill);
    setrlimit(Resource::RLIMIT_NOFILE, soft, hard).unwrap();
    assert!(short.len() < ids.len(), "every image opened with no descriptor left: {short:?}");

    let mut want: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    want.sort();
    // Listed again once the first wait (1 s) is over.
    let start = std::time::Instant::now();
    while listed() != want {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "the images left out stay out once descriptors are free: {:?}",
            listed()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
