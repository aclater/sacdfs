// SPDX-License-Identifier: GPL-3.0-or-later

//! sacdfs's frame locator against scarletbook's seek index (#28).
//!
//! scarletbook 0.4's `AreaIndex` starts a range from saved reading state, with results
//! exactly those of a scan from the area's first sector by the specification (4.2 item 7).
//! That makes it an oracle for sacdfs's own locator, which starts a fresh demultiplexer at
//! a sector it found by frame numbers: every frame and random ranges located by sacdfs
//! must equal the same positions read through a complete library index, and a straight
//! read without one, on every corpus case (hostile ones included) and on real discs.
//!
//! The library's provisional reading (`SeekPolicy::Speculative`, its cold seek) is checked
//! too, for why sacdfs does not use it: on a hostile corpus image its provisional positions
//! differ from the exact ones and are refuted only once a pass from the area's first sector
//! reaches them, while sacdfs's locator gives the exact positions from the start.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use common::*;
use sacdfs::disc::{Disc, Tuning};
use scarletbook::{
    AreaIndex, FrameContent, Image, IndexOptions, ReadAtReader, Speculation, SpeculativeOptions, TrackCursor,
};

type Img = Image<ReadAtReader<Arc<File>>>;

fn library_image(disc: &Disc) -> Img {
    let file = Arc::new(File::open(&disc.path).unwrap());
    Image::with_format(ReadAtReader::new(file), disc.toc.sector_format).unwrap()
}

/// The positions `[first, end)` of an area read by `cursor`.
fn read_all(mut cursor: TrackCursor, image: &mut Img) -> Vec<FrameContent> {
    let mut out = Vec::new();
    while let Some(f) = cursor.next_frame(image) {
        out.push(f.unwrap().content);
    }
    out
}

/// A complete seek index of area `a` of `disc`, built in one pass.
fn complete_index(disc: &Disc, a: usize, interval: u32, image: &mut Img) -> AreaIndex {
    let area = &disc.toc.areas[a];
    let mut index =
        AreaIndex::new(area, disc.toc.sectors, IndexOptions::new().with_interval(interval)).unwrap();
    while !index.extend(image, 4096).unwrap() {}
    index
}

/// Positions `[first, end)` located by sacdfs: a fresh cursor at `first`, read forward.
fn located(disc: &Disc, a: usize, first: u32, end: u32) -> Vec<FrameContent> {
    let mut cursor = disc.locate(a, first).unwrap();
    (first..end).map(|t| cursor.content(disc, t).unwrap()).collect()
}

/// A small deterministic generator (xorshift64*).
fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed >> 12;
    *seed ^= *seed << 25;
    *seed ^= *seed >> 27;
    seed.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// Checks one image: every frame alone and `ranges` random ranges, located by sacdfs with
/// `tuning`, against reads through a complete library index (interval `interval`), and the
/// whole play range against a straight library read without an index. Returns the number
/// of positions compared.
fn check_image(path: &Path, tuning: Tuning, interval: u32, every_frame: bool, ranges: usize) -> u64 {
    let Ok(disc) = Disc::open_with(path, 1, tuning, None) else { return 0 };
    let mut image = library_image(&disc);
    let mut seed = 0x9E37_79B9_7F4A_7C15u64 ^ disc.toc.sectors;
    let mut compared = 0;
    for (a, area) in disc.toc.areas.iter().enumerate() {
        let p = area.playtime_frames();
        if p == 0 || scarletbook::audio::check_range(area, 0, p, disc.toc.sectors).is_err() {
            continue;
        }
        let straight = read_all(TrackCursor::for_range(area, 0, p, disc.toc.sectors).unwrap(), &mut image);
        assert_eq!(straight.len(), p as usize);
        let index = complete_index(&disc, a, interval, &mut image);
        // sacdfs reading the area straight through, as a player of every track would.
        assert!(located(&disc, a, 0, p) == straight, "{}: area {a}: straight read differs", path.display());
        let indexed = |first: u32, end: u32, image: &mut Img| {
            read_all(
                TrackCursor::for_range_indexed(area, first, end, disc.toc.sectors, &index).unwrap(),
                image,
            )
        };
        if every_frame {
            for t in 0..p {
                let lib = indexed(t, t + 1, &mut image);
                assert!(
                    lib[..] == straight[t as usize..=t as usize],
                    "{}: library index at {t}",
                    path.display()
                );
                assert!(
                    located(&disc, a, t, t + 1) == lib,
                    "{}: area {a}: frame {t} located by sacdfs differs from the seek index's",
                    path.display()
                );
                compared += 1;
            }
        }
        for _ in 0..ranges {
            let first = (next(&mut seed) % u64::from(p)) as u32;
            let len = 1 + (next(&mut seed) % 40) as u32;
            let end = first.saturating_add(len).min(p);
            let lib = indexed(first, end, &mut image);
            assert!(
                lib[..] == straight[first as usize..end as usize],
                "{}: library index range",
                path.display()
            );
            assert!(
                located(&disc, a, first, end) == lib,
                "{}: area {a}: range {first}..{end} located by sacdfs differs from the seek index's",
                path.display()
            );
            compared += u64::from(end - first);
        }
    }
    compared
}

/// Every corpus case, hostile ones included: every frame alone and random ranges, with the
/// default search and with the exhaustive one (every read searches down to one sector).
#[test]
fn locator_equals_the_seek_index_on_the_corpus() {
    let mut compared = 0;
    let mut images = 0;
    for case in cases() {
        let path = image(&case);
        for tuning in [Tuning::default(), Tuning::exhaustive()] {
            let n = check_image(&path, tuning, 4, true, 40);
            compared += n;
            images += usize::from(n > 0);
        }
    }
    assert!(images > 20, "too few corpus images read: {images}");
    assert!(compared > 10_000, "too few positions compared: {compared}");
}

/// The library's cold seek on a hostile corpus image, `area-audio-past-end` (frames stored
/// past the area's play range raise the next acceptable frame number of a scan from the
/// first sector). Up to scarletbook 0.5.0 its provisional readings there returned frames
/// that a scan does not, refuted only when the seek index's pass reached them; on 0.5.2
/// every one of them equals the exact reading. This keeps what matters either way: every provisional
/// reading that differs is refuted, and sacdfs's locator gives the exact positions at once.
#[test]
fn provisional_readings_that_differ_are_refuted() {
    let case = cases().into_iter().find(|c| c["id"] == "area-audio-past-end").expect("corpus case");
    let disc = Disc::open_with(&image(&case), 1, Tuning::default(), None).unwrap();
    let mut image = library_image(&disc);
    let (mut provisional, mut differ, mut refuted) = (0, 0, 0);
    for (a, area) in disc.toc.areas.iter().enumerate() {
        let p = area.playtime_frames();
        if p < 2 {
            continue;
        }
        let mut index = AreaIndex::new(area, disc.toc.sectors, IndexOptions::new().with_interval(1)).unwrap();
        let options = SpeculativeOptions::new().with_min_gap_sectors(0);
        let mut ids = Vec::new();
        for first in 1..p {
            let end = (first + 4).min(p);
            let mut c =
                TrackCursor::for_range_speculative(area, first, end, disc.toc.sectors, Some(&index), options)
                    .unwrap();
            let mut got = Vec::new();
            while let Some(f) = c.next_frame(&mut image) {
                got.push(f.unwrap().content);
            }
            let exact =
                read_all(TrackCursor::for_range(area, first, end, disc.toc.sectors).unwrap(), &mut image);
            assert!(located(&disc, a, first, end) == exact, "sacdfs's locator differs at {first}");
            if matches!(c.speculation(), Speculation::Pending { .. }) {
                provisional += 1;
                differ += usize::from(got != exact);
                ids.push(c.speculation_id());
                index.absorb(&mut c).unwrap();
            }
        }
        while !index.extend(&mut image, 4096).unwrap() {}
        for e in index.take_events() {
            if let Speculation::Refuted { exact_from, .. } = e.speculation {
                assert!(ids.contains(&Some(e.claim)));
                assert!(exact_from > 0);
                refuted += 1;
            }
        }
    }
    assert!(provisional > 0, "no provisional reading was made");
    assert!(refuted >= differ, "{refuted} refuted, {differ} differing");
}

/// The same checks on real discs, run locally: `SACDFS_REAL_IMAGES=DIR cargo test --release
/// --test library_index real_discs -- --ignored --nocapture`. Each area is read straight
/// by sacdfs and compared with a straight library read; 300 random ranges per area located
/// by sacdfs (default search) equal reads through a complete library index (interval 64,
/// the library's default).
#[test]
#[ignore = "needs real disc images: set SACDFS_REAL_IMAGES"]
fn real_discs() {
    let Some(dir) = std::env::var_os("SACDFS_REAL_IMAGES") else { return };
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("iso")))
        .collect();
    paths.sort();
    let mut total = 0;
    for path in paths {
        let t = std::time::Instant::now();
        let n = check_image(&path, Tuning::default(), 64, false, 300);
        eprintln!("image {total}: {n} random-range positions equal ({:.1} s)", t.elapsed().as_secs_f64());
        total += 1;
    }
    assert!(total > 0);
}
