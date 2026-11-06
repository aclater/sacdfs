// SPDX-License-Identifier: GPL-3.0-or-later

//! Concurrent reads of one image (#28): the reads of a disc run on several threads at once
//! through clones of its one open handle (positional reads), and give the bytes of reads
//! one at a time, also while the bounded cache of open images closes handles under them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::*;
use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::handles;
use sacdfs::track::{Reader, TrackFile};
use sacdfs::tree::{Output, SourceDir, Tree};

fn tree(tuning: Tuning) -> Tree {
    Tree::with_options(SourceDir::scan(&corpus().join("images")).unwrap(), tuning, Output::default())
}

/// Every file of every disc, read by 4 threads at once per disc (each its own readers, in
/// another order, in pieces of odd sizes), with all discs' threads running together, no
/// frame cache, every read searching (so that probes and cursors of one disc interleave)
/// and at most 2 images open: the bytes of a straight read, one file at a time.
#[test]
fn readers_of_one_disc_on_several_threads() {
    let reference: BTreeMap<std::path::PathBuf, Vec<u8>> = {
        let t = tree(Tuning::default());
        let engine = Engine::new(2, 0).unwrap();
        t.all_files()
            .into_iter()
            .map(|(p, f)| {
                let bytes = Reader::new(f.clone()).read(&engine, 0, f.size).unwrap().0;
                (p, bytes)
            })
            .collect()
    };
    handles::global().configure(2, handles::DEFAULT_IDLE);
    let t = tree(Tuning::exhaustive());
    let mut by_disc: BTreeMap<u32, Vec<(std::path::PathBuf, Arc<TrackFile>)>> = BTreeMap::new();
    for (p, f) in t.all_files() {
        by_disc.entry(f.disc.id).or_default().push((p, f));
    }
    assert!(by_disc.values().any(|v| v.len() > 2), "no disc with several files");
    let engine = Engine::new(4, 0).unwrap();
    std::thread::scope(|s| {
        for files in by_disc.values() {
            for k in 0..4usize {
                let (engine, reference) = (&engine, &reference);
                s.spawn(move || {
                    let n = files.len();
                    let piece = [7_001u64, 20_000, 65_536, 4_096][k];
                    for i in 0..n {
                        let (path, f) = &files[(i + k) % n];
                        let want = &reference[path];
                        let reader = Reader::new(f.clone());
                        let mut at = 0;
                        while at < f.size {
                            let got = reader.read(engine, at, piece).unwrap().0;
                            let end = (at + piece).min(f.size) as usize;
                            assert!(got == want[at as usize..end], "{}: differs at {at}", path.display());
                            at += piece;
                        }
                    }
                });
            }
        }
    });
    assert!(handles::global().open_count() <= 2);
}

/// Fast readers of DSF files share a quarter of the cache (#210): one keeps it all, four don't
/// fill the cache between them, and nobody falls below the 2 s window. Few threads never reach it.
#[test]
fn fast_readers_share_a_quarter_of_the_cache() {
    let t = tree(Tuning::default());
    let all = t.all_files();
    let (_, f) = all
        .iter()
        .find(|(p, _)| p.extension().is_some_and(|e| e == "dsf"))
        .expect("a DSF file in the corpus");
    let frame = 4704 * u64::from(f.layout.channels);
    let cache = 32usize << 20;
    let quarter = (cache as u64 / 4 / frame) as u32;
    let by_threads = 32 * 32;
    assert_eq!(f.read_ahead_frames(32, cache, 1), by_threads.min(quarter).max(150));
    let four = f.read_ahead_frames(32, cache, 4);
    assert_eq!(four, by_threads.min(quarter / 4).max(150));
    assert_eq!(f.read_ahead_frames(32, cache, 1000), 150);
    assert_eq!(f.read_ahead_frames(4, cache, 1), 150);
    assert_eq!(f.read_ahead_frames(32, cache, 0), f.read_ahead_frames(32, cache, 1));
}

/// While someone plays, readers for throughput share a quarter of the cache however few frames
/// that is (#230): below the 2 s window that a player keeps, but never down to nothing.
#[test]
fn readers_share_less_while_someone_plays() {
    let t = tree(Tuning::default());
    let all = t.all_files();
    let (_, f) = all
        .iter()
        .find(|(p, _)| p.extension().is_some_and(|e| e == "wav"))
        .expect("a WAV file in the corpus");
    let cache = 32usize << 20;
    let one = f.shared_frames(cache, 1);
    let four = f.shared_frames(cache, 4);
    assert!(four < one && one < f.read_ahead_frames(32, cache, 1), "{four} {one}");
    assert_eq!(f.shared_frames(cache, 1000), 16);
    assert_eq!(f.shared_frames(cache, 0), one);
}
