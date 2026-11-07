// SPDX-License-Identifier: GPL-3.0-or-later

//! `Engine::frames` on the batches a read can't always ask for whole: frames already in the
//! cache in the middle of a batch, and a source that fails in the middle of one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sacdfs::disc::{Cursor, Tuning};
use sacdfs::engine::Engine;
use sacdfs::track::TrackFile;
use sacdfs::tree::{Output, SourceDir, Tree};

/// A DST image of the corpus: its frames take the groups' path through `Engine::frames`.
const IMAGE: &str = "images/dst-features-2ch-6ch.iso";

/// The longest DSF file of the tree.
fn longest_dsf(tree: &Tree) -> Arc<TrackFile> {
    tree.all_files()
        .into_iter()
        .filter(|(p, _)| p.extension().is_some_and(|e| e == "dsf"))
        .map(|(_, f)| f)
        .max_by_key(|f| f.layout.frames)
        .expect("a DSF file")
}

fn tree_of(dir: &Path) -> Tree {
    Tree::with_options(SourceDir::scan(dir).unwrap(), Tuning::default(), Output::default())
}

fn frames(
    engine: &Engine,
    f: &TrackFile,
    cursor: &mut Option<Cursor>,
    k0: u32,
    k1: u32,
) -> Result<Vec<Vec<u8>>, sacdfs::disc::ReadError> {
    let (t0, t1) = (f.start_frame + k0, f.start_frame + k1);
    let got = engine.frames(&f.disc, f.area, cursor, t0, t1)?;
    Ok(got.iter().map(|g| g.data.to_vec()).collect())
}

/// A batch around a frame already in the cache gives the bytes of one without it, and the
/// cached frame itself, not a second copy.
#[test]
fn a_cached_frame_in_the_middle_of_a_batch() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(common::corpus().join(IMAGE), dir.path().join("disc.iso")).unwrap();
    let tree = tree_of(dir.path());
    let f = longest_dsf(&tree);
    let n = f.layout.frames;
    assert!(n >= 3, "too short a file: {n} frames");
    let want = frames(&Engine::new(2, 0).unwrap(), &f, &mut None, 0, n).unwrap();

    let engine = Engine::new(2, 64 << 20).unwrap();
    let mid = n / 2;
    let t = f.start_frame + mid;
    let cached = engine.frames(&f.disc, f.area, &mut None, t, t + 1).unwrap().remove(0);
    let (t0, t1) = (f.start_frame, f.start_frame + n);
    let got = engine.frames(&f.disc, f.area, &mut None, t0, t1).unwrap();
    assert!(Arc::ptr_eq(&got[mid as usize], &cached), "the cached frame was decoded again");
    let got: Vec<Vec<u8>> = got.iter().map(|g| g.data.to_vec()).collect();
    assert_eq!(got, want);
}

/// A source that fails during a batch fails the read and drops the cursor, whose position is
/// no longer known. Once the source is back, the same frames read as they should.
#[test]
fn a_source_that_fails_during_a_batch() {
    // Every read checks the image, so that the failure comes at the next one.
    sacdfs::handles::global().set_revalidate_every(Duration::ZERO);
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disc.iso");
    std::fs::copy(common::corpus().join(IMAGE), &image).unwrap();
    let tree = tree_of(dir.path());
    let f = longest_dsf(&tree);
    let n = f.layout.frames;
    assert!(n >= 3, "too short a file: {n} frames");
    let want = frames(&Engine::new(2, 0).unwrap(), &f, &mut None, 0, n).unwrap();

    // No cache: every frame is demultiplexed. The cursor is in place after a first frame.
    let engine = Engine::new(2, 0).unwrap();
    let mut cursor = None;
    assert_eq!(frames(&engine, &f, &mut cursor, 0, 1).unwrap(), want[..1]);
    assert!(cursor.is_some());

    let bytes = std::fs::read(&image).unwrap();
    std::fs::File::options().write(true).open(&image).unwrap().set_len(bytes.len() as u64 / 2).unwrap();
    let failed = frames(&engine, &f, &mut cursor, 1, n);
    assert!(failed.is_err(), "a read of a truncated image gave {:?} frames", failed.map(|v| v.len()));
    assert!(cursor.is_none(), "the cursor of a failed batch was kept");

    std::fs::write(&image, &bytes).unwrap();
    assert_eq!(frames(&engine, &f, &mut cursor, 1, n).unwrap(), want[1..]);
}
