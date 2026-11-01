// SPDX-License-Identifier: GPL-3.0-or-later

//! The persistent seek index, without mounting: reads with a warm index (scarletbook's seek
//! hints, loaded from the cache of an earlier "mount") equal straight reads; damaged, stale
//! and changed entries cost only speed.
//!
//! The cache is process-wide ([`sacdfs::index_cache::install`]), and each test file is its
//! own process, so every test here shares one cache directory. Tests that write entries
//! of their own use other images than the shared ones, or their own `IndexCache`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use sacdfs::disc::{Disc, Tuning};
use sacdfs::dsf::{BLOCK, FRAME_BYTES, HEADER_LEN};
use sacdfs::engine::Engine;
use sacdfs::index_cache::{self, AreaShape, ImageKey, IndexCache};
use sacdfs::track::{Reader, TrackFile};
use sacdfs::tree::{SourceDir, Tree};

use common::*;

/// The process-wide cache, in a fresh directory, held by one test at a time.
///
/// Every disc any test opens registers with this cache, and a flush writes the entries of
/// all of them. A test that flushes while `hostile_entries_cost_only_speed` runs would
/// replace its hostile entries with the real points its discs learned (correctly: that
/// is what a flush is for), so that fewer bad hints are left to catch. Hence the lock;
/// the reads stay correct either way.
fn cache() -> (std::sync::MutexGuard<'static, ()>, &'static IndexCache) {
    static EXCLUSIVE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = EXCLUSIVE.lock().unwrap_or_else(|p| p.into_inner());
    (guard, shared_cache())
}

fn shared_cache() -> &'static IndexCache {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("index-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        index_cache::install(Arc::new(IndexCache::open(&d).unwrap()));
        d
    });
    let c = index_cache::global().unwrap();
    assert_eq!(c.dir(), dir);
    c
}

/// The default seeking, with no cursor reuse.
fn warm_tuning() -> Tuning {
    Tuning { reuse_slack: 0, ..Tuning::default() }
}

fn read_all(engine: &Engine, file: &Arc<TrackFile>) -> Vec<u8> {
    let reader = Reader::new(file.clone());
    let mut out = Vec::new();
    while (out.len() as u64) < file.size {
        let (data, _) = reader.read(engine, out.len() as u64, 1 << 20).unwrap();
        assert!(!data.is_empty());
        out.extend_from_slice(&data);
    }
    out
}

/// The images of the corpus cases that show audio.
fn ok_images() -> Vec<(String, PathBuf)> {
    cases()
        .into_iter()
        .filter(|c| c["expected"]["outcome"] == "ok")
        .map(|c| (c["id"].as_str().unwrap().to_owned(), image(&c)))
        .collect()
}

/// The files of a fresh tree of `image`, sorted by path.
fn files(image: &Path, tuning: Tuning) -> Vec<(PathBuf, Arc<TrackFile>)> {
    let mut f = Tree::with_tuning(SourceDir::scan(image).unwrap(), tuning).all_files();
    f.sort_by(|a, b| a.0.cmp(&b.0));
    f
}

/// Reads the start of every frame of every file of `image` (channel 0), each from a fresh
/// tree, so that only the loaded seek index is known, and compares it with `wholes`. The
/// provisional and exact readings made.
fn every_frame_fresh(
    engine: &Engine,
    image: &Path,
    tuning: Tuning,
    wholes: &[Vec<u8>],
    what: &str,
) -> (u64, u64) {
    let (mut provisional, mut exact) = (0, 0);
    let shape = files(image, tuning);
    for (i, (path, f)) in shape.iter().enumerate() {
        let c = u64::from(f.layout.channels);
        for k in 0..u64::from(f.layout.frames) {
            let b = k * FRAME_BYTES;
            let at = HEADER_LEN + (b / BLOCK) * BLOCK * c + b % BLOCK;
            let len = BLOCK - b % BLOCK;
            let (_, fresh) = &files(image, tuning)[i];
            let (data, _) = Reader::new(fresh.clone()).read(engine, at, len).unwrap();
            assert!(data == wholes[i][at as usize..(at + len) as usize], "{what} {path:?} frame {k}");
            let (p, e) = fresh.disc.readings();
            provisional += p;
            exact += e;
        }
    }
    (provisional, exact)
}

/// Reads every frame of `file` on its own from a fresh reader and compares it with `whole`.
fn every_frame_alone(engine: &Engine, file: &Arc<TrackFile>, whole: &[u8], what: &str) -> usize {
    let c = u64::from(file.layout.channels);
    let mut reads = 0;
    for k in 0..u64::from(file.layout.frames) {
        let b = k * FRAME_BYTES;
        for ch in 0..c {
            let at = HEADER_LEN + (b / BLOCK) * BLOCK * c + ch * BLOCK + b % BLOCK;
            let len = BLOCK - b % BLOCK;
            let (data, _) = Reader::new(file.clone()).read(engine, at, len).unwrap();
            assert!(data == whole[at as usize..(at + len) as usize], "{what} frame {k} channel {ch}");
            reads += 1;
        }
    }
    reads
}

/// A straight read of every file of every case and seeks into it (recording seek hints),
/// saved; then every frame alone and random ranges, read with the index loaded by a fresh
/// tree, equal the straight read.
#[test]
fn warm_index_reads_equal_straight_reads() {
    let (_exclusive, cache) = cache();
    let engine = Engine::new(2, 0).unwrap();
    let mut loaded = 0;
    let mut reads = 0;
    let mut readings = (0, 0);
    let mut all = Vec::new();
    for (id, image) in ok_images() {
        let cold = files(&image, Tuning::exhaustive());
        let wholes: Vec<Vec<u8>> = cold.iter().map(|(_, f)| read_all(&engine, f)).collect();
        // Seeks, which record hints (a straight read from an area's start is exact, and
        // records checkpoints, which are not saved).
        for (_, f) in &cold {
            for k in (0..f.layout.frames).step_by(5) {
                let at =
                    HEADER_LEN + u64::from(k) * FRAME_BYTES / BLOCK * BLOCK * u64::from(f.layout.channels);
                Reader::new(f.clone()).read(&engine, at, 1).unwrap();
            }
        }
        cache.flush().unwrap();
        let (p, e) = every_frame_fresh(&engine, &image, warm_tuning(), &wholes, &id);
        readings.0 += p;
        readings.1 += e;
        for tuning in [warm_tuning(), Tuning::exhaustive()] {
            let warm = files(&image, tuning);
            assert_eq!(warm.len(), cold.len());
            if let Some((_, f)) = warm.first() {
                for a in 0..f.disc.areas.len() {
                    loaded += f.disc.loaded_hints(a);
                }
            }
            for ((path, f), whole) in warm.iter().zip(&wholes) {
                reads += every_frame_alone(&engine, f, whole, &format!("{id} {path:?}"));
                assert_eq!(&read_all(&engine, f), whole, "{id} {path:?}");
            }
            all.extend(warm.into_iter().map(|(_, f)| f).zip(wholes.iter().cloned()));
        }
    }
    eprintln!("{loaded} seek hints loaded, {reads} reads, provisional and exact readings: {readings:?}");
    assert!(loaded > 100, "{loaded}");
    assert!(readings.0 > 100, "{readings:?}");
    assert!(reads > 1000, "{reads}");

    // Random ranges, each from a fresh reader, on the warm trees.
    let mut runner = TestRunner::new(Config { cases: 512, ..Config::default() });
    runner
        .run(&(0..all.len(), any::<u64>(), 1u64..300_000), |(i, off, len)| {
            let (f, whole) = &all[i];
            let off = off % (f.size + 10);
            let (data, _) = Reader::new(f.clone()).read(&engine, off, len).unwrap();
            let s = off.min(f.size) as usize;
            let e = off.saturating_add(len).min(f.size) as usize;
            prop_assert_eq!(&data[..], &whole[s..e]);
            Ok(())
        })
        .unwrap();
}

/// A warm seek reads no more than a cold one, from the loaded hints: the same seek after a
/// "remount".
#[test]
fn warm_seek_reads_less() {
    let (_exclusive, cache) = cache();
    let engine = Engine::new(2, 0).unwrap();
    // A copy, so that no other test writes this image's entry.
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("seek.iso");
    std::fs::copy(corpus().join("images/dst-invalid-frames-2ch.iso"), &copy).unwrap();
    let seek = |tuning| {
        let (_, f) = files(&copy, tuning).into_iter().next().unwrap();
        let at = HEADER_LEN + f.layout.frames as u64 * 3 / 5 * FRAME_BYTES / BLOCK * BLOCK * 2;
        let (data, _) = Reader::new(f.clone()).read(&engine, at, 64 << 10).unwrap();
        (data, f.disc.sector_reads(), f.disc.sector_bytes(), f.disc.loaded_hints(f.area))
    };
    let (cold, cold_calls, cold_bytes, none) = seek(Tuning::default());
    assert_eq!(none, 0);
    cache.flush().unwrap();
    let (warm, warm_calls, warm_bytes, loaded) = seek(Tuning::default());
    eprintln!("cold {cold_calls} calls {cold_bytes} bytes, warm {warm_calls} calls {warm_bytes} bytes");
    assert!(loaded > 0);
    assert_eq!(cold, warm);
    // The corpus image is small: a cold seek already reads the frames in one call, so a
    // warm one cannot read less (real discs: docs/performance.md, "Calls and bytes per seek").
    assert!(warm_calls <= cold_calls && warm_bytes <= cold_bytes);
}

/// Damaged and stale entries cost only speed: every frame read alone still equals the
/// straight read. Damaged: the saved indexes with bytes changed at random (the library
/// refuses them). Stale: an entry that matches the image's path, size and modification time
/// but was saved before audio sectors changed (a re-rip with the same TOC), whose hints then
/// point at sectors that no longer hold their frames.
#[test]
fn damaged_and_stale_entries_cost_only_speed() {
    let (_exclusive, cache) = cache();
    let engine = Engine::new(2, 0).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Copies of a few images with several areas and hostile timelines, so that no other
    // test writes their entries.
    let mut subjects = Vec::new();
    for id in ["dst-features-2ch-6ch", "dsd-stereo-mch-2048", "dst-invalid-frames-2ch", "track-duration-max"]
    {
        let copy = dir.path().join(format!("{id}.iso"));
        std::fs::copy(corpus().join(format!("images/{id}.iso")), &copy).unwrap();
        let cold = files(&copy, Tuning::exhaustive());
        let wholes: Vec<Vec<u8>> = cold.iter().map(|(_, f)| read_all(&engine, f)).collect();
        for (_, f) in &cold {
            for k in (1..f.layout.frames).step_by(3) {
                let at =
                    HEADER_LEN + u64::from(k) * FRAME_BYTES / BLOCK * BLOCK * u64::from(f.layout.channels);
                Reader::new(f.clone()).read(&engine, at, 1).unwrap();
            }
        }
        cache.flush().unwrap();
        let disc = &cold[0].1.disc;
        let meta = std::fs::metadata(&copy).unwrap();
        let key = ImageKey::new(&copy, &meta).unwrap();
        let shapes: Vec<AreaShape> = disc
            .areas
            .iter()
            .map(|a| AreaShape { start_lsn: a.start_lsn, end_lsn: a.end_lsn, channels: a.channels })
            .collect();
        let saved = cache.load(&key, &shapes).unwrap();
        assert!(saved.iter().any(Option::is_some), "{id}: nothing saved");
        subjects.push((id, copy, wholes, key, shapes, saved));
    }
    let mut runner = TestRunner::new(Config { cases: 48, ..Config::default() });
    let edits = proptest::collection::vec((any::<usize>(), any::<u8>()), 1..6);
    runner
        .run(&(0..subjects.len(), edits), |(s, edits)| {
            let (id, copy, wholes, key, shapes, saved) = &subjects[s];
            let mut damaged = saved.clone();
            for (at, v) in edits {
                if let Some(b) = damaged.iter_mut().flatten().next() {
                    let n = b.len();
                    b[at % n] ^= v | 1;
                }
            }
            cache.store(key, shapes, &damaged).unwrap();
            let warm = files(copy, warm_tuning());
            for ((path, f), whole) in warm.iter().zip(wholes) {
                every_frame_alone(&engine, f, whole, &format!("{id} {path:?}"));
            }
            every_frame_fresh(&engine, copy, warm_tuning(), wholes, id);
            Ok(())
        })
        .unwrap();

    // Stale: audio sectors moved by one sector (the area's first audio sector repeated),
    // with the modification time kept, under the entry saved for the original.
    let mut stale_hints = 0;
    for (id, copy, _, key, shapes, saved) in &subjects {
        let f = std::fs::File::options().read(true).write(true).open(copy).unwrap();
        let mtime = f.metadata().unwrap().modified().unwrap();
        let sector = |l: u64| {
            use std::os::unix::fs::FileExt;
            let mut b = vec![0u8; 2048];
            f.read_exact_at(&mut b, l * 2048).unwrap();
            b
        };
        let sh = shapes[0];
        let first = sector(sh.start_lsn);
        let mut moved = Vec::new();
        for l in sh.start_lsn..sh.end_lsn - 1 {
            moved.push(sector(l));
        }
        {
            use std::os::unix::fs::FileExt;
            f.write_all_at(&first, sh.start_lsn * 2048).unwrap();
            for (i, b) in moved.iter().enumerate() {
                f.write_all_at(b, (sh.start_lsn + 1 + i as u64) * 2048).unwrap();
            }
        }
        f.set_modified(mtime).unwrap();
        drop(f);
        // The straight reads of the changed image, with no seek index.
        let cold = files(copy, Tuning::exhaustive());
        let wholes: Vec<Vec<u8>> = cold.iter().map(|(_, f)| read_all(&engine, f)).collect();
        cache.store(key, shapes, saved).unwrap();
        let warm = files(copy, warm_tuning());
        stale_hints += warm[0].1.disc.loaded_hints(0);
        for ((path, f), whole) in warm.iter().zip(&wholes) {
            every_frame_alone(&engine, f, whole, &format!("stale {id} {path:?}"));
        }
        cache.store(key, shapes, saved).unwrap();
        every_frame_fresh(&engine, copy, warm_tuning(), &wholes, id);
    }
    eprintln!("stale entries: {stale_hints} seek hints loaded");
    assert!(stale_hints > 0, "the stale entries were not used");
}

/// A changed image (size or mtime) misses its entry; the entry round-trips otherwise.
#[test]
fn changed_image_misses_its_entry() {
    let dir = tempfile::tempdir().unwrap();
    let local = IndexCache::open(&dir.path().join("cache")).unwrap();
    let copy = dir.path().join("x.iso");
    std::fs::copy(corpus().join("images/dst-features-2ch-6ch.iso"), &copy).unwrap();
    let open = || Disc::open_with(&copy, 1, Tuning::default(), Some(&local)).unwrap();
    let d = open();
    for area in 0..d.areas.len() {
        for frame in [10, 20, 30] {
            let mut c = d.locate(area, frame).unwrap();
            c.content(&d, frame).unwrap();
        }
    }
    let learned: Vec<usize> = (0..d.areas.len()).map(|a| d.seek_hints(a)).collect();
    assert!(learned.iter().all(|&n| n > 0), "{learned:?}");
    assert_eq!(local.flush().unwrap(), 1);
    assert_eq!(local.flush().unwrap(), 0, "nothing changed since");
    let loaded: Vec<usize> = {
        let d = open();
        (0..d.areas.len()).map(|a| d.loaded_hints(a)).collect()
    };
    assert_eq!(loaded, learned);

    let f = std::fs::File::options().write(true).open(&copy).unwrap();
    let mtime = f.metadata().unwrap().modified().unwrap();
    f.set_modified(mtime + std::time::Duration::from_secs(1)).unwrap();
    assert_eq!(open().loaded_hints(0), 0, "mtime changed");
    f.set_modified(mtime).unwrap();
    assert!(open().loaded_hints(0) > 0, "mtime restored");
    f.set_len(f.metadata().unwrap().len() + 2048).unwrap();
    f.set_modified(mtime).unwrap();
    assert_eq!(open().loaded_hints(0), 0, "size changed");
}
