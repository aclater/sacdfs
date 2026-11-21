// SPDX-License-Identifier: GPL-3.0-or-later

//! Which tracks of an area are listed, and over which frames.
//!
//! A track's frames come from the area TOC: its start and duration, clamped by the library
//! to the area's play time. On a damaged, truncated or padded image those can claim more
//! than the image holds, or overlap: several tracks covering the same frames. Listing them
//! as given would show files far larger than the image's audio, and an area's files would
//! add up to more than one area's worth. Here, before an area's files are made:
//!
//! 1. Each track is checked against what the image holds ([`check_track`], with the image
//!    sector count of [`crate::area_data::data_sectors`]), as the specification's track
//!    check (4.8) does: one that fails is left out.
//! 2. Overlaps among the tracks left are resolved by start frame ([`fit_overlaps`]): a
//!    track that runs into the next track to start ends where that one starts; a track
//!    that starts where an earlier-numbered one starts lies inside it and is left out.
//!    *Why by start:* the starts are what a player seeks to; a duration that runs past the
//!    next start is the part of the TOC that is wrong. *Why after the check:* this step
//!    only shortens or leaves out tracks the check passed, so it never shows a file that
//!    the check alone would not, and a track the check refuses does not cut another short.
//! 3. With `--pauses kept`, a track is served with its pause only if the range with pause
//!    passes the same check and overlaps no other listed track; otherwise it is served
//!    without its pause. On a normal disc the ranges with pause only touch, so every
//!    track keeps its pause.
//!
//! The listed tracks then never share a frame and all end within the area's play time and
//! capacity ([`capacity`]), so their total stays within one area's worth. A normal disc's tracks do not
//! overlap and fit, and are listed exactly as before.

use std::fmt;
use std::path::Path;

use scarletbook::audio::{FRAMES_PER_SECTOR, audio_sector_range, check_range, check_track};
use scarletbook::{Area, Slot, Track};

/// A track's frame range `[start, end)`, by track number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// Track number.
    pub number: u8,
    /// First frame.
    pub start: u32,
    /// The frame after the last.
    pub end: u32,
}

/// What [`fit_overlaps`] does with a span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Kept as it is.
    Whole,
    /// Kept, ending at `end`: where track `next` starts.
    Shortened {
        /// The new end frame.
        end: u32,
        /// The track that starts there.
        next: u8,
    },
    /// Left out: it starts where the earlier-numbered track `earlier` starts, so it lies
    /// inside that track's frames.
    Inside {
        /// The track it lies inside.
        earlier: u8,
    },
}

/// Resolves overlapping spans, returning one [`Fit`] per span in the order given.
///
/// Spans are taken by start frame (by track number on a tie). Of the spans starting at the
/// same frame, the lowest-numbered is kept and the others are [`Fit::Inside`]. A kept span
/// that runs past the next later start ends there ([`Fit::Shortened`]). Empty spans
/// (`start >= end`) neither shorten nor hide others and are left [`Fit::Whole`]: the
/// track check refuses them. The kept spans then never overlap; spans that only touch
/// (one ends where the next starts) are not changed.
pub fn fit_overlaps(spans: &[Span]) -> Vec<Fit> {
    let mut fits = vec![Fit::Whole; spans.len()];
    let mut order: Vec<usize> = (0..spans.len()).filter(|&i| spans[i].start < spans[i].end).collect();
    order.sort_by_key(|&i| (spans[i].start, spans[i].number, i));
    let mut kept: Option<usize> = None;
    for (k, &i) in order.iter().enumerate() {
        let s = spans[i];
        if let Some(j) = kept.filter(|&j| spans[j].start == s.start) {
            fits[i] = Fit::Inside { earlier: spans[j].number };
            continue;
        }
        kept = Some(i);
        // The next later start: the spans after this one in `order` start at or after it.
        if let Some(&n) = order[k + 1..].iter().find(|&&n| spans[n].start > s.start) {
            let next = spans[n];
            if next.start < s.end {
                fits[i] = Fit::Shortened { end: next.start, next: next.number };
            }
        }
    }
    fits
}

/// An area's capacity in frames for an image of `image_sectors` sectors: the frames its
/// existing audio sectors can hold at most (specification 4, 4.8 item 3, the rule of
/// [`check_track`]).
pub fn capacity(area: &Area, image_sectors: u64) -> u64 {
    let (start, end) = audio_sector_range(area, image_sectors);
    end.saturating_sub(start).saturating_mul(FRAMES_PER_SECTOR)
}

/// Why a track is listed differently from its TOC range, or not at all.
#[derive(Debug)]
pub enum Note {
    /// Refused by the track check: not shown.
    Refused(scarletbook::Error),
    /// Not shown: it lies inside track `earlier`.
    Inside {
        /// The earlier track.
        earlier: u8,
    },
    /// Shown, ending at frame `end` where track `next` starts.
    Shortened {
        /// The new end frame.
        end: u32,
        /// The track that starts there.
        next: u8,
    },
    /// Shown without its pause (`--pauses kept`): with it, it would overlap another track
    /// or not fit the image.
    PauseDropped,
}

/// One track of an area as it is listed.
#[derive(Debug)]
pub struct Listed {
    /// Track number.
    pub number: u8,
    /// The track with the frames to serve (its pause included when kept), or `None` when
    /// it is not shown.
    pub track: Option<Track>,
    /// Why it differs from its TOC range, if it does.
    pub note: Option<Note>,
}

impl Listed {
    /// Logs why the track is not shown or is shortened, if it is, at info.
    pub fn log(&self, image: &Path, slot: Slot) {
        let (path, n) = (image.display(), self.number);
        match &self.note {
            None => {}
            Some(Note::Refused(e)) => log::info!("{path}: {slot} track {n}: not shown: {e}"),
            Some(Note::Inside { earlier }) => {
                log::info!("{path}: {slot} track {n}: not shown: starts where track {earlier} starts")
            }
            Some(Note::Shortened { end, next }) => {
                log::info!("{path}: {slot} track {n}: shown to frame {end}: runs into track {next}")
            }
            Some(Note::PauseDropped) => {
                log::info!("{path}: {slot} track {n}: shown without its pause: it overlaps another track")
            }
        }
    }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Note::Refused(e) => write!(f, "{e}"),
            Note::Inside { earlier } => write!(f, "starts where track {earlier} starts"),
            Note::Shortened { end, next } => write!(f, "ends at frame {end}, where track {next} starts"),
            Note::PauseDropped => f.write_str("without its pause"),
        }
    }
}

fn ranged(track: &Track, start: u32, end: u32) -> Track {
    let mut t = track.clone();
    t.start_frame = start;
    t.duration_frames = end - start;
    t
}

/// The tracks of `area` as they are listed, in TOC order: see the module documentation.
/// `image_sectors` is the sector count the tracks are checked against; `pauses` serves
/// each track with its pause where it can be (`--pauses kept`).
pub fn listed(area: &Area, image_sectors: u64, pauses: bool) -> Vec<Listed> {
    let mut out: Vec<Listed> = area
        .tracks
        .iter()
        .map(|t| match check_track(area, t, image_sectors) {
            Ok(()) => Listed { number: t.number, track: Some(t.clone()), note: None },
            Err(e) => Listed { number: t.number, track: None, note: Some(Note::Refused(e)) },
        })
        .collect();
    let shown: Vec<usize> = (0..out.len()).filter(|&i| out[i].track.is_some()).collect();
    let spans: Vec<Span> = shown
        .iter()
        .filter_map(|&i| {
            let t = out[i].track.as_ref()?;
            Some(Span { number: t.number, start: t.start_frame, end: t.end_frame() })
        })
        .collect();
    for ((&i, span), fit) in shown.iter().zip(&spans).zip(fit_overlaps(&spans)) {
        let item = &mut out[i];
        match fit {
            Fit::Whole => {}
            Fit::Shortened { end, next } => {
                item.track = item.track.as_ref().map(|t| ranged(t, span.start, end));
                item.note = Some(Note::Shortened { end, next });
            }
            Fit::Inside { earlier } => {
                item.track = None;
                item.note = Some(Note::Inside { earlier });
            }
        }
    }
    if pauses {
        keep_pauses(area, image_sectors, &mut out);
    }
    debug_assert!(
        listed_frames(&out) <= capacity(area, image_sectors).min(u64::from(area.playtime_frames())),
        "{} track frames listed in a {} area of capacity {} and play time {}",
        listed_frames(&out),
        area.slot,
        capacity(area, image_sectors),
        area.playtime_frames()
    );
    out
}

/// Widens each listed track to its range with pause, in TOC order, where that range passes
/// the track check and overlaps no other listed track (as widened so far).
fn keep_pauses(area: &Area, image_sectors: u64, out: &mut [Listed]) {
    let range = |l: &Listed| l.track.as_ref().map(|t| (t.start_frame, t.end_frame()));
    for i in 0..out.len() {
        let Some(track) = out[i].track.as_ref() else { continue };
        let Some(raw) = area.track(u32::from(track.number)) else { continue };
        let Some((first, end)) = crate::track::pause_range(area, raw) else { continue };
        if (first, end) == (track.start_frame, track.end_frame()) {
            continue;
        }
        let fits = check_range(area, first, end, image_sectors).is_ok();
        let clear = out
            .iter()
            .enumerate()
            .filter(|&(j, _)| j != i)
            .filter_map(|(_, l)| range(l))
            .all(|(s, e)| s >= e || end <= s || first >= e);
        // A shortened track keeps its shorter range: its range with pause runs past it.
        if fits && clear && out[i].note.is_none() {
            out[i].track = Some(ranged(track, first, end));
        } else if out[i].note.is_none() {
            out[i].note = Some(Note::PauseDropped);
        }
    }
}

/// The total frames of the listed tracks.
pub fn listed_frames(listed: &[Listed]) -> u64 {
    listed.iter().filter_map(|l| l.track.as_ref()).map(|t| u64::from(t.duration_frames)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(r: &[(u8, u32, u32)]) -> Vec<Span> {
        r.iter().map(|&(number, start, end)| Span { number, start, end }).collect()
    }

    fn kept_total(s: &[Span], f: &[Fit]) -> u64 {
        s.iter()
            .zip(f)
            .map(|(s, f)| match *f {
                Fit::Whole if s.start < s.end => u64::from(s.end - s.start),
                Fit::Shortened { end, .. } => u64::from(end - s.start),
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn ordered_tracks_are_unchanged() {
        let s = spans(&[(1, 0, 4), (2, 4, 8), (3, 8, 12)]);
        assert_eq!(fit_overlaps(&s), vec![Fit::Whole; 3]);
    }

    #[test]
    fn gaps_and_reversed_order_are_unchanged() {
        let s = spans(&[(1, 8, 12), (2, 4, 6), (3, 0, 4)]);
        assert_eq!(fit_overlaps(&s), vec![Fit::Whole; 3]);
    }

    #[test]
    fn same_start_keeps_the_lowest_number() {
        // The corpus case track-starts-overlap: three tracks over the whole area.
        let s = spans(&[(1, 0, 12), (2, 0, 12), (3, 0, 12)]);
        let f = fit_overlaps(&s);
        assert_eq!(f, vec![Fit::Whole, Fit::Inside { earlier: 1 }, Fit::Inside { earlier: 1 }]);
        assert_eq!(kept_total(&s, &f), 12);
    }

    #[test]
    fn running_into_the_next_start_is_shortened() {
        // The corpus case track-duration-max: every duration clamped to the play time.
        let s = spans(&[(1, 0, 12), (2, 4, 12), (3, 8, 12)]);
        let f = fit_overlaps(&s);
        assert_eq!(
            f,
            vec![Fit::Shortened { end: 4, next: 2 }, Fit::Shortened { end: 8, next: 3 }, Fit::Whole]
        );
        assert_eq!(kept_total(&s, &f), 12);
    }

    #[test]
    fn touching_ranges_are_not_overlaps() {
        let s = spans(&[(1, 0, 5), (2, 5, 9)]);
        assert_eq!(fit_overlaps(&s), vec![Fit::Whole, Fit::Whole]);
    }

    #[test]
    fn empty_spans_do_not_shorten_or_hide() {
        // An empty track at frame 0 before a real one there, and one inside a range.
        let s = spans(&[(1, 0, 0), (2, 0, 10), (3, 5, 5)]);
        assert_eq!(fit_overlaps(&s), vec![Fit::Whole; 3]);
    }

    #[test]
    fn a_later_number_starting_earlier_shortens_by_start() {
        let s = spans(&[(1, 6, 12), (2, 0, 9)]);
        assert_eq!(fit_overlaps(&s), vec![Fit::Whole, Fit::Shortened { end: 6, next: 1 }]);
    }

    #[test]
    fn same_start_with_the_lower_number_shorter() {
        let s = spans(&[(1, 0, 3), (2, 0, 12), (3, 3, 6)]);
        assert_eq!(fit_overlaps(&s), vec![Fit::Whole, Fit::Inside { earlier: 1 }, Fit::Whole]);
    }

    /// Whatever the spans, the kept ones never overlap, and so add up to at most the frames
    /// from the earliest start to the latest end.
    #[test]
    fn kept_spans_never_overlap() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = |m: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % u64::from(m)) as u32
        };
        for _ in 0..2000 {
            let n = 1 + next(12) as u8;
            let s: Vec<Span> = (1..=n)
                .map(|number| {
                    let start = next(40);
                    Span { number, start, end: start + next(30) }
                })
                .collect();
            let f = fit_overlaps(&s);
            let mut kept: Vec<(u32, u32)> = s
                .iter()
                .zip(&f)
                .filter(|(s, _)| s.start < s.end)
                .filter_map(|(s, f)| match *f {
                    Fit::Whole => Some((s.start, s.end)),
                    Fit::Shortened { end, .. } => Some((s.start, end)),
                    Fit::Inside { .. } => None,
                })
                .collect();
            kept.sort();
            for w in kept.windows(2) {
                assert!(w[0].1 <= w[1].0, "{s:?} -> {f:?}");
            }
            let lo = s.iter().map(|s| s.start).min().unwrap_or(0);
            let hi = s.iter().map(|s| s.end).max().unwrap_or(0);
            assert!(kept_total(&s, &f) <= u64::from(hi.saturating_sub(lo)), "{s:?}");
            // A span is only ever left out for one that starts at the same frame.
            for (sp, fit) in s.iter().zip(&f) {
                if let Fit::Inside { earlier } = fit {
                    let e = s.iter().find(|x| x.number == *earlier).unwrap();
                    assert_eq!(e.start, sp.start);
                    assert!(e.number < sp.number);
                }
            }
        }
    }
}
