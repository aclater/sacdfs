// SPDX-License-Identifier: GPL-3.0-or-later

//! The first listing of a folder of many new images opens them no more at a time than image
//! files may be kept open (`--max-open-images`), so that it stays within the descriptors set
//! aside for images however many threads open them. Before, a folder's new images were all
//! opened at once, as many at a time as there were threads.
//!
//! A test binary of its own, with one test: it lowers the process's open files limit and
//! sets the process's thread pool and image file bound.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use common::*;
use nix::sys::resource::{Resource, getrlimit, setrlimit};
use sacdfs::disc::Tuning;
use sacdfs::sidecar::Sidecars;
use sacdfs::tree::{Output, SourceDir, Tree};

/// Image files kept open, as `--max-open-images` would set.
const IMAGE_FILES: usize = 8;

/// Threads opening images: far more than [`IMAGE_FILES`].
const THREADS: usize = 64;

/// Images in the folder.
const IMAGES: usize = 120;

fn open_now() -> u64 {
    std::fs::read_dir("/proc/self/fd").unwrap().count() as u64
}

#[test]
fn a_first_listing_of_many_images_opens_them_within_the_image_files_bound() {
    rayon::ThreadPoolBuilder::new().num_threads(THREADS).build_global().unwrap();
    sacdfs::handles::global().configure(IMAGE_FILES, sacdfs::handles::DEFAULT_IDLE);
    let base = tempfile::tempdir().unwrap();
    let album = base.path().join("Album");
    std::fs::create_dir_all(&album).unwrap();
    let image = corpus().join("images/dsd-stereo-mch-2048.iso");
    for i in 0..IMAGES {
        std::os::unix::fs::symlink(&image, album.join(format!("disc{i:03}.iso"))).unwrap();
    }
    let tree = Tree::with_options(
        SourceDir::scan_with(base.path(), &Sidecars::standard(), None).unwrap(),
        Tuning::default(),
        Output::default(),
    );
    let ino = tree.resolve(Path::new("Album")).unwrap();
    // Room for the image files kept, as many opening, and a few more: not for one per thread.
    let (soft, hard) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
    setrlimit(Resource::RLIMIT_NOFILE, open_now() + 2 * IMAGE_FILES as u64 + 8, hard).unwrap();
    let listed = tree.children(ino).unwrap().len();
    setrlimit(Resource::RLIMIT_NOFILE, soft, hard).unwrap();
    assert_eq!(listed, IMAGES, "images left out of a first listing for want of descriptors");
}
