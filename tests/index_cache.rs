// SPDX-License-Identifier: GPL-3.0-or-later

//! The persistent seek index, without mounting: reads with a warm index (loaded from the
//! cache of an earlier "mount") equal straight reads; hostile, stale and changed entries
//! cost only speed.
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

/// The process-wide cache, in a fresh directory.
fn cache() -> &'static IndexCache {
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

/// A search that starts at a point (checked or loaded) up to the default distance before
/// its frame, and otherwise searches down to one sector, with no cursor reuse.
fn warm_tuning() -> Tuning {
    Tuning { search_close: 1, reuse_slack: 0, ..Tuning::default() }
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
/// tree, so that only the loaded points are known, and compares it with `wholes`. The
/// checks of loaded points that passed and failed.
fn every_frame_fresh(
    engine: &Engine,
    image: &Path,
    tuning: Tuning,
    wholes: &[Vec<u8>],
    what: &str,
) -> (u64, u64) {
    let (mut ok, mut failed) = (0, 0);
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
            let (o, e) = fresh.disc.loaded_point_checks();
            ok += o;
            failed += e;
        }
    }
    (ok, failed)
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

/// A straight read of every file of every case (learning the index), saved; then every
/// frame alone and random ranges, read with the index loaded by a fresh tree, equal the
/// straight read.
#[test]
fn warm_index_reads_equal_straight_reads() {
    let cache = cache();
    let engine = Engine::new(2, 0).unwrap();
    let mut loaded = 0;
    let mut reads = 0;
    let mut checks = (0, 0);
    let mut all = Vec::new();
    for (id, image) in ok_images() {
        let cold = files(&image, Tuning::exhaustive());
        let wholes: Vec<Vec<u8>> = cold.iter().map(|(_, f)| read_all(&engine, f)).collect();
        // Seeks too, so that the index holds probe points as well as scan points.
        for (_, f) in &cold {
            for k in (0..f.layout.frames).step_by(5) {
                let at =
                    HEADER_LEN + u64::from(k) * FRAME_BYTES / BLOCK * BLOCK * u64::from(f.layout.channels);
                Reader::new(f.clone()).read(&engine, at, 1).unwrap();
            }
        }
        cache.flush().unwrap();
        let (ok, failed) = every_frame_fresh(&engine, &image, warm_tuning(), &wholes, &id);
        checks.0 += ok;
        checks.1 += failed;
        for tuning in [warm_tuning(), Tuning::exhaustive()] {
            let warm = files(&image, tuning);
            assert_eq!(warm.len(), cold.len());
            if let Some((_, f)) = warm.first() {
                for a in 0..f.disc.areas.len() {
                    loaded += f.disc.loaded_points(a);
                }
            }
            for ((path, f), whole) in warm.iter().zip(&wholes) {
                reads += every_frame_alone(&engine, f, whole, &format!("{id} {path:?}"));
                assert_eq!(&read_all(&engine, f), whole, "{id} {path:?}");
            }
            all.extend(warm.into_iter().map(|(_, f)| f).zip(wholes.iter().cloned()));
        }
    }
    eprintln!("{loaded} points loaded, {reads} reads, loaded point checks passed and failed: {checks:?}");
    assert!(loaded > 100, "{loaded}");
    // The saved points were right, so starts at them pass their checks.
    assert!(checks.0 > 1000 && checks.1 == 0, "{checks:?}");
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

/// A warm seek reads less than a cold one: the same seek after a "remount".
#[test]
fn warm_seek_reads_less() {
    let cache = cache();
    let engine = Engine::new(2, 0).unwrap();
    // A copy, so that no other test writes this image's entry.
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("seek.iso");
    std::fs::copy(corpus().join("images/dst-invalid-frames-2ch.iso"), &copy).unwrap();
    let seek = |tuning| {
        let (_, f) = files(&copy, tuning).into_iter().next().unwrap();
        let at = HEADER_LEN + f.layout.frames as u64 * 3 / 5 * FRAME_BYTES / BLOCK * BLOCK * 2;
        let (data, _) = Reader::new(f.clone()).read(&engine, at, 64 << 10).unwrap();
        (data, f.disc.sector_reads(), f.disc.sector_bytes(), f.disc.loaded_points(f.area))
    };
    let (cold, cold_calls, cold_bytes, none) = seek(Tuning::default());
    assert_eq!(none, 0);
    cache.flush().unwrap();
    let (warm, warm_calls, warm_bytes, loaded) = seek(Tuning::default());
    eprintln!("cold {cold_calls} calls {cold_bytes} bytes, warm {warm_calls} calls {warm_bytes} bytes");
    assert!(loaded > 0);
    assert_eq!(cold, warm);
    assert!(warm_calls < cold_calls && warm_bytes < cold_bytes);
}

/// Hostile entries (points in order, with a valid checksum, but anywhere) cost only
/// speed: every frame read alone still equals the straight read.
#[test]
fn hostile_entries_cost_only_speed() {
    let cache = cache();
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
        let disc = &cold[0].1.disc;
        let meta = std::fs::metadata(&copy).unwrap();
        let key = ImageKey::new(&copy, &meta).unwrap();
        let shapes: Vec<AreaShape> = disc
            .areas
            .iter()
            .map(|a| AreaShape { start_lsn: a.start_lsn, end_lsn: a.end_lsn, channels: a.channels })
            .collect();
        subjects.push((id, copy, wholes, key, shapes));
    }
    let mut runner = TestRunner::new(Config { cases: 48, ..Config::default() });
    let points = proptest::collection::vec((0u32..200, any::<u64>()), 0..40);
    let checks = std::cell::Cell::new((0, 0));
    runner
        .run(&(0..subjects.len(), points.clone(), points), |(s, p0, p1)| {
            let (id, copy, wholes, key, shapes) = &subjects[s];
            let areas: Vec<Vec<(u32, u64)>> = shapes
                .iter()
                .zip([p0, p1])
                .map(|(sh, p)| {
                    let span = sh.end_lsn - sh.start_lsn;
                    let mut f: Vec<u32> = p.iter().map(|x| x.0).collect();
                    let mut l: Vec<u64> = p.iter().map(|x| sh.start_lsn + x.1 % span).collect();
                    f.sort_unstable();
                    f.dedup();
                    l.sort_unstable();
                    f.into_iter().zip(l).collect()
                })
                .collect();
            cache.store(key, shapes, &areas).unwrap();
            let warm = files(copy, warm_tuning());
            for ((path, f), whole) in warm.iter().zip(wholes) {
                every_frame_alone(&engine, f, whole, &format!("{id} {path:?}"));
            }
            let (ok, failed) = every_frame_fresh(&engine, copy, warm_tuning(), wholes, id);
            let (o, e) = checks.get();
            checks.set((o + ok, e + failed));
            Ok(())
        })
        .unwrap();
    // Points in order but anywhere mostly start a cursor early, which is right (only
    // slower); some start it too late, which its check catches.
    let (ok, failed) = checks.get();
    eprintln!("random points: loaded point checks passed {ok}, failed {failed}");
    assert!(ok > 0 && failed > 0);

    // Every point too late: frame f at the f-th last sector of its area.
    let mut failed = 0;
    for (id, copy, wholes, key, shapes) in &subjects {
        let areas: Vec<Vec<(u32, u64)>> = shapes
            .iter()
            .map(|sh| (1..60u32).map(|f| (f, sh.end_lsn - 61 + u64::from(f))).collect())
            .collect();
        cache.store(key, shapes, &areas).unwrap();
        failed += every_frame_fresh(&engine, copy, warm_tuning(), wholes, id).1;
    }
    eprintln!("late points: {failed} checks failed");
    assert!(failed > 10, "{failed}");
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
    let learned: Vec<usize> = (0..d.areas.len()).map(|a| d.index_points(a)).collect();
    assert!(learned.iter().all(|&n| n > 0), "{learned:?}");
    assert_eq!(local.flush().unwrap(), 1);
    assert_eq!(local.flush().unwrap(), 0, "nothing changed since");
    let loaded: Vec<usize> = {
        let d = open();
        (0..d.areas.len()).map(|a| d.loaded_points(a)).collect()
    };
    // The saved points are thinned, never empty.
    assert!(loaded.iter().zip(&learned).all(|(&l, &n)| l > 0 && l <= n), "{loaded:?} {learned:?}");

    let f = std::fs::File::options().write(true).open(&copy).unwrap();
    let mtime = f.metadata().unwrap().modified().unwrap();
    f.set_modified(mtime + std::time::Duration::from_secs(1)).unwrap();
    assert_eq!(open().loaded_points(0), 0, "mtime changed");
    f.set_modified(mtime).unwrap();
    assert!(open().loaded_points(0) > 0, "mtime restored");
    f.set_len(f.metadata().unwrap().len() + 2048).unwrap();
    f.set_modified(mtime).unwrap();
    assert_eq!(open().loaded_points(0), 0, "size changed");
}
