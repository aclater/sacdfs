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
//! too: where its provisional positions differ from the exact ones (an image made so), they
//! are refuted once a pass from the area's first sector reaches them, while sacdfs's
//! locator gives the exact positions from the start.

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

/// Provisional (cold) readings of every 4-frame range of every area of `path`, each compared
/// with the exact reading, then a pass of the seek index that settles them: the readings
/// made, those whose positions differ from the exact ones, and whether every one of those
/// was refuted (and how many were refuted in all); and the ranges where sacdfs's locator
/// differs from the exact reading, which fails the test if `locator_exact`.
fn provisional_readings(path: &Path, locator_exact: bool) -> (usize, usize, bool, usize, usize) {
    let disc = Disc::open_with(path, 1, Tuning::default(), None).unwrap();
    let mut image = library_image(&disc);
    let (mut provisional, mut differ, mut all_refuted, mut refuted, mut located_differ) = (0, 0, true, 0, 0);
    for (a, area) in disc.toc.areas.iter().enumerate() {
        let p = area.playtime_frames();
        if p < 2 {
            continue;
        }
        let mut index = AreaIndex::new(area, disc.toc.sectors, IndexOptions::new().with_interval(1)).unwrap();
        let options = SpeculativeOptions::new().with_min_gap_sectors(0);
        let mut differing = Vec::new();
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
            if located(&disc, a, first, end) != exact {
                assert!(!locator_exact, "sacdfs's locator differs at {first}");
                located_differ += 1;
            }
            if matches!(c.speculation(), Speculation::Pending { .. }) {
                provisional += 1;
                if got != exact {
                    differ += 1;
                    differing.push(c.speculation_id().unwrap());
                }
                index.absorb(&mut c).unwrap();
            }
        }
        while !index.extend(&mut image, 4096).unwrap() {}
        let mut settled = Vec::new();
        for e in index.take_events() {
            if let Speculation::Refuted { exact_from, .. } = e.speculation {
                assert!(exact_from > 0);
                settled.push(e.claim);
                refuted += 1;
            }
        }
        all_refuted &= differing.iter().all(|id| settled.contains(id));
    }
    (provisional, differ, all_refuted, refuted, located_differ)
}

/// The library's cold seek on a hostile corpus image, `area-audio-past-end` (frames stored
/// past the area's play range raise the next acceptable frame number of a scan from the
/// first sector). Up to scarletbook 0.5.0 its provisional readings there returned frames
/// that a scan does not; on 0.5.2 none of them differ (no corpus image gives a differing
/// provisional reading on 0.5.2: `provisional_readings_that_differ_are_refuted` makes one).
/// Any that differed would have to be refuted; sacdfs's locator gives the exact positions.
#[test]
fn provisional_readings_on_area_audio_past_end() {
    let case = cases().into_iter().find(|c| c["id"] == "area-audio-past-end").expect("corpus case");
    let (provisional, differ, all_refuted, refuted, _) = provisional_readings(&image(&case), true);
    eprintln!("provisional {provisional}, differing {differ}, refuted {refuted}");
    assert!(provisional > 0, "no provisional reading was made");
    assert!(all_refuted, "{differ} differing, not all refuted");
}

/// A provisional reading that really differs, and is refuted: a copy of a corpus image whose
/// stereo area has a run of its last audio sectors written again over sectors near its
/// start. A scan from the area's first sector then accepts those late frames early, and so
/// discards every frame numbered below them that follows (specification 4, 4.5): the exact
/// positions in between are missing. A provisional reading that begins past the copy never
/// sees it and returns the frames stored there, so it differs, and the seek index's pass
/// must refute every such reading. (The refuted count may be larger: a refuted reading's
/// positions are only *possibly* different.) sacdfs's locator, which starts a fresh reading
/// at a sector found by frame numbers too, cannot see the copy either and differs from the
/// exact reading there as well: its count is printed, not asserted.
#[test]
fn provisional_readings_that_differ_are_refuted() {
    use std::os::unix::fs::FileExt;
    let case = cases().into_iter().find(|c| c["id"] == "dsd-stereo-mch-2048").expect("corpus case");
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("late-frames-early.iso");
    std::fs::copy(image(&case), &copy).unwrap();
    let (start, end) = {
        let d = Disc::open_with(&copy, 1, Tuning::default(), None).unwrap();
        let st = d.areas.iter().find(|a| a.slot == scarletbook::Slot::TwoChannel).unwrap();
        (st.start_lsn, st.end_lsn)
    };
    let run = 32.min((end - start) / 4);
    let f = File::options().read(true).write(true).open(&copy).unwrap();
    let mut late = vec![0u8; run as usize * 2048];
    f.read_exact_at(&mut late, (end - run) * 2048).unwrap();
    f.write_all_at(&late, (start + 4) * 2048).unwrap();
    drop(f);
    let (provisional, differ, all_refuted, refuted, located_differ) = provisional_readings(&copy, false);
    eprintln!(
        "provisional {provisional}, differing {differ}, refuted {refuted}; sacdfs's locator differs in {located_differ}"
    );
    assert!(differ > 0, "no provisional reading differs ({provisional} made)");
    assert!(all_refuted && refuted >= differ, "{differ} differing, {refuted} refuted, not all of them");
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
