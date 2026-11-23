// SPDX-License-Identifier: GPL-3.0-or-later

//! What an area lists stays within what its image holds: tracks whose frames overlap share
//! none once listed, and a damaged, truncated or padded image (a hole, or a long run of zero
//! bytes, where the audio should be) does not list tracks far larger than its data.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use sacdfs::disc::Tuning;
use sacdfs::tree::{Output, SourceDir, Tree};

use common::*;

/// Huge play time and track durations, and a stereo area that claims to run to LSN 2^32-1.
const PAST_END: &str = "track-duration-max-area-past-end";

/// The DSF files of the image or directory `src`: (area directory, track number) to (first
/// frame, frames).
fn listing(src: &Path, kept: bool) -> BTreeMap<(String, u32), (u32, u32)> {
    let out = Output { pauses_kept: kept, ..Output::default() };
    let tree = Tree::with_options(SourceDir::scan(src).unwrap(), Tuning::default(), out);
    tree.all_files()
        .into_iter()
        .filter(|(p, _)| p.extension().is_some_and(|e| e == "dsf"))
        .map(|(p, f)| {
            let area = p.parent().unwrap().file_name().unwrap().to_str().unwrap().to_owned();
            let n = track_number(p.file_name().unwrap().to_str().unwrap());
            ((area, n), (f.start_frame, f.layout.frames))
        })
        .collect()
}

fn stereo(l: &BTreeMap<(String, u32), (u32, u32)>) -> Vec<(u32, u32, u32)> {
    l.iter().filter(|((a, _), _)| a == "Stereo").map(|((_, n), &(s, f))| (*n, s, f)).collect()
}

/// A copy of the corpus image `id` in `dir`.
fn copy(dir: &Path, id: &str) -> PathBuf {
    let to = dir.join(format!("{id}.iso"));
    fs::copy(corpus().join(format!("images/{id}.iso")), &to).unwrap();
    to
}

/// Sets the stereo area's play time, in both copies of its TOC, to one minute (4500
/// frames): small enough that an image padded to a few MiB could hold it.
fn one_minute_stereo(image: &Path) {
    let bytes = fs::read(image).unwrap();
    let f = OpenOptions::new().write(true).open(image).unwrap();
    let mut n = 0;
    for at in (0..bytes.len()).step_by(2048) {
        if bytes[at..].starts_with(b"TWOCHTOC") {
            common::write_all_at(&f, &[1, 0, 0], at as u64 + 64).unwrap();
            n += 1;
        }
    }
    assert_eq!(n, 2, "both stereo TOC copies");
}

/// Pads `image` to `len` bytes with written zeros.
fn pad_with_zeros(image: &Path, len: u64) {
    let mut f = OpenOptions::new().append(true).open(image).unwrap();
    let have = f.metadata().unwrap().len();
    f.write_all(&vec![0u8; (len - have) as usize]).unwrap();
}

#[test]
fn hole_padded_image_lists_no_track_past_its_data() {
    let dir = tempfile::tempdir().unwrap();
    let image = copy(dir.path(), PAST_END);
    let before = listing(&image, false);
    // About 400 MB, nearly all of it a hole: room for the play time at 7 frames a sector.
    fs::File::options().write(true).open(&image).unwrap().set_len(409_600_000).unwrap();
    let after = listing(&image, false);
    assert!(stereo(&before).is_empty(), "{before:?}");
    assert_eq!(after, before, "the hole adds no tracks");
    assert_eq!(listing(&image, true), before, "nor with pauses kept");
}

/// The stereo play time cut to one minute: tracks, by start, [0, 4500), [4, 4500) and
/// [8, 4500). The image's 157 stereo sectors hold at most 1099 frames, so all are refused;
/// padded to 8 MiB (3550 sectors, 24850 frames), they would all pass the track check.
fn one_minute_image(dir: &Path) -> PathBuf {
    let image = copy(dir, PAST_END);
    one_minute_stereo(&image);
    assert!(stereo(&listing(&image, false)).is_empty());
    image
}

#[test]
fn hole_padded_image_lists_what_its_data_holds() {
    let dir = tempfile::tempdir().unwrap();
    let image = one_minute_image(dir.path());
    let short = listing(&image, false);
    fs::File::options().write(true).open(&image).unwrap().set_len(8 << 20).unwrap();
    assert_eq!(listing(&image, false), short, "8 MiB, a hole past the copy's data");
    assert_eq!(listing(&image, true), short, "and with pauses kept");
}

#[test]
fn zero_padded_image_lists_what_its_data_holds() {
    let dir = tempfile::tempdir().unwrap();
    let image = one_minute_image(dir.path());
    let short = listing(&image, false);
    pad_with_zeros(&image, 8 << 20);
    assert_eq!(listing(&image, false), short, "8 MiB, written zeros past the copy's data");
    assert_eq!(listing(&image, true), short, "and with pauses kept");
}

/// Without the zero run, the same 8 MiB image with audio-like bytes (0x69, DSD silence)
/// lists its last track to the play time: what holds data is not hidden.
#[test]
fn padding_that_looks_like_audio_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let image = one_minute_image(dir.path());
    let mut f = OpenOptions::new().append(true).open(&image).unwrap();
    let have = f.metadata().unwrap().len();
    f.write_all(&vec![0x69u8; ((8 << 20) - have) as usize]).unwrap();
    drop(f);
    assert_eq!(stereo(&listing(&image, false)), vec![(1, 0, 4), (2, 4, 4), (3, 8, 4492)]);
}

#[test]
fn overlapping_tracks_share_no_frames() {
    let l = listing(&image_of("track-starts-overlap"), false);
    assert_eq!(stereo(&l), vec![(1, 0, 12)]);
    let l = listing(&image_of("track-duration-max"), false);
    assert_eq!(stereo(&l), vec![(1, 0, 4), (2, 4, 4), (3, 8, 4)]);
}

/// With pauses kept, a track's range with pause is used only where it overlaps no other
/// track: the reversed starts keep their own ranges, as with pauses dropped.
#[test]
fn kept_pauses_never_overlap_another_track() {
    let src = image_of("track-starts-reversed");
    assert_eq!(listing(&src, true), listing(&src, false));
    assert_eq!(stereo(&listing(&src, false)), vec![(1, 8, 4), (2, 4, 4), (3, 0, 4)]);
    // A play time far past the sectors: the last track's pause would run to it.
    assert_eq!(stereo(&listing(&image_of("area-playtime-max"), true))[2], (3, 8, 4));
}

fn image_of(id: &str) -> PathBuf {
    corpus().join(format!("images/{id}.iso"))
}

/// Every area of every corpus image, with pauses dropped and kept, lists at most its play
/// time and at most the capacity of its sectors that hold data.
#[test]
fn listed_frames_stay_within_the_area() {
    for kept in [false, true] {
        let out = Output { pauses_kept: kept, ..Output::default() };
        let tree =
            Tree::with_options(SourceDir::scan(&corpus().join("images")).unwrap(), Tuning::default(), out);
        let mut totals: BTreeMap<(PathBuf, usize), (u64, u64)> = BTreeMap::new();
        for (p, f) in tree.all_files() {
            if p.extension().is_none_or(|e| e != "dsf") {
                continue;
            }
            let area = &f.disc.toc().areas[f.area];
            let sectors = sacdfs::area_data::data_sectors(&f.disc, area);
            let bound = scarletbook::audio::area_capacity(area, sectors)
                .capacity
                .min(u64::from(area.playtime_frames()));
            let e = totals.entry((f.disc.path.clone(), f.area)).or_insert((0, bound));
            e.0 += u64::from(f.layout.frames);
        }
        assert!(totals.len() > 100);
        for ((path, area), (listed, bound)) in totals {
            assert!(listed <= bound, "{path:?} area {area} (pauses kept: {kept}): {listed} > {bound}");
        }
    }
}

/// The tracks of `area` as listed for `sectors` image sectors: number, and first frame and
/// frames if shown.
fn checked(area: &scarletbook::Area, sectors: u64, kept: bool) -> Vec<(u8, Option<(u32, u32)>)> {
    sacdfs::track_ranges::listed(area, sectors, kept)
        .into_iter()
        .map(|l| (l.number, l.track.map(|t| (t.start_frame, t.duration_frames))))
        .collect()
}

/// #451: on a share, holes are not looked for when [`sacdfs::area_data::holes_cannot_matter`]
/// says that holes up to the bound leave the listing as it is. For every corpus area, at the
/// largest bound it accepts, the listing with that many sectors fewer is the listing with
/// none fewer; one sector more is not accepted.
#[test]
fn holes_within_the_bound_change_no_listing() {
    use sacdfs::area_data::{SUSPICIOUS_FRAMES_PER_SECTOR, holes_cannot_matter};
    use scarletbook::audio::audio_sector_range;
    let tree = Tree::with_options(
        SourceDir::scan(&corpus().join("images")).unwrap(),
        Tuning::default(),
        Output::default(),
    );
    let mut seen = std::collections::BTreeSet::new();
    let mut accepted = 0;
    for (_, f) in tree.all_files() {
        if !seen.insert((f.disc.path.clone(), f.area)) {
            continue;
        }
        let toc = f.disc.toc();
        let (n, stored) = (toc.sectors, toc.sector_format.stored_size());
        let area = &toc.areas[f.area];
        let (start, end) = audio_sector_range(area, n);
        let need = u64::from(area.playtime_frames()).div_ceil(SUSPICIOUS_FRAMES_PER_SECTOR);
        let Some(most) = (end - start).checked_sub(need) else {
            assert!(!holes_cannot_matter(area, n, stored, 0), "{:?} {}", f.disc.path, f.area);
            continue;
        };
        assert!(holes_cannot_matter(area, n, stored, most * stored), "{:?} {}", f.disc.path, f.area);
        assert!(!holes_cannot_matter(area, n, stored, (most + 1) * stored), "{:?} {}", f.disc.path, f.area);
        for kept in [false, true] {
            let whole = checked(area, end, kept);
            for fewer in [most / 2, most] {
                assert_eq!(checked(area, end - fewer, kept), whole, "{:?} {} -{fewer}", f.disc.path, f.area);
            }
        }
        accepted += 1;
    }
    assert!(accepted > 50, "{accepted} areas accepted");
}

/// The hole-padded images above are not accepted at their hole's size: there, holes are
/// looked for on a share too.
#[test]
fn a_large_hole_is_looked_for() {
    let dir = tempfile::tempdir().unwrap();
    let image = one_minute_image(dir.path());
    let size = fs::metadata(&image).unwrap().len();
    let disc = sacdfs::disc::Disc::open(&image, 0, Tuning::default()).unwrap();
    let toc = disc.toc();
    let stereo = toc.areas.iter().find(|a| a.slot == scarletbook::Slot::TwoChannel).unwrap();
    let stored = toc.sector_format.stored_size();
    // As padded to 8 MiB with a hole: the hole is at least 8 MiB less the copy's size.
    let padded = (8 << 20) / stored;
    assert!(!sacdfs::area_data::holes_cannot_matter(stereo, padded, stored, (8 << 20) - size));
}

/// The library's area capacity (#595) is what sacdfs's own check was: the sectors an area
/// has in the image, at 7 frames a sector. The old formula is kept here as the reference,
/// and compared on every area of every corpus image at its own sector count, at the
/// counts a truncated, hole-trimmed or padded image gives around the area's edges, and
/// through `scarletbook::Disc::area_capacity` on the opened image.
#[test]
fn library_area_capacity_matches_the_old_check() {
    use scarletbook::audio::{FRAMES_PER_SECTOR, area_capacity, audio_sector_range};
    let old = |area: &scarletbook::Area, n: u64| {
        let (start, end) = audio_sector_range(area, n);
        end.saturating_sub(start).saturating_mul(FRAMES_PER_SECTOR)
    };
    let mut areas = 0;
    for entry in fs::read_dir(corpus().join("images")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "iso") {
            continue;
        }
        let Ok(disc) = sacdfs::disc::Disc::open_with(&path, 1, Tuning::default(), None) else { continue };
        let toc = disc.toc();
        let lib = scarletbook::Disc::open(fs::File::open(&path).unwrap()).unwrap();
        for area in &toc.areas {
            let (start, end) = audio_sector_range(area, toc.sectors);
            let counts = [
                0,
                1,
                start.saturating_sub(1),
                start,
                start + 1,
                (start + end) / 2,
                end.saturating_sub(1),
                end,
                end + 1,
                toc.sectors,
                toc.sectors + 1,
                u64::from(u32::MAX),
                u64::MAX / 2,
            ];
            for n in counts {
                assert_eq!(
                    area_capacity(area, n).capacity,
                    old(area, n),
                    "{path:?} {} at {n} sectors",
                    area.slot
                );
            }
            assert_eq!(
                lib.area_capacity(area.slot).unwrap().capacity,
                old(area, toc.sectors),
                "{path:?} {}",
                area.slot
            );
            areas += 1;
        }
    }
    assert!(areas > 100, "{areas} areas");
}
