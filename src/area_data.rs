// SPDX-License-Identifier: GPL-3.0-or-later

//! How many of an area's audio sectors hold data, for the track check at listing time.
//!
//! The track check ([`scarletbook::audio::check_track`]) refuses a track whose end frame
//! is past what the area's sectors in the image can hold, 7 frames a sector. A truncated
//! image is handled by that: the missing sectors do not count. A damaged or padded image
//! can instead be long enough while holding no audio over much of an area: a hole (a sparse
//! region of the file) or a long run of zero bytes. Its tracks would then be listed at
//! their TOC sizes, gigabytes from a few megabytes of data.
//!
//! [`data_sectors`] gives the sector count to check an area's tracks against: the image's
//! own, less the area's audio sectors that lie in a long hole or a long zero run, so that
//! such sectors count as missing, as a truncated image's do. Only gaps of at least
//! [`MIN_GAP_BYTES`] count: a few zero sectors on a damaged disc change nothing.
//!
//! - **Holes** are found with `lseek` `SEEK_HOLE`/`SEEK_DATA`, which costs no reads. A file
//!   system that does not report holes (NFS and SMB shares often do not, and some local
//!   ones) reports the whole file as data: nothing is found and nothing is hidden. That is a
//!   safe fallback, not detection; the zero sampling below still applies there, since a
//!   hole reads as zeros.
//! - **Not looked for on a share when they cannot matter** (#451). On NFS 4.2 each `lseek`
//!   is a round trip to the server, once per area listed. On an NFS or SMB share the
//!   file's size less the space it uses bounds its holes ([`crate::platform::hole_bound`],
//!   from cached attributes). When holes that large would leave the area's play time
//!   within [`SUSPICIOUS_FRAMES_PER_SECTOR`] frame per sector holding data, they could
//!   neither start the zero sampling nor refuse a track (whose frames end within the play
//!   time, below the capacity rule's 7 a sector), so the area lists exactly as it would
//!   with them looked for, and no `lseek` is made ([`holes_cannot_matter`]). An intact real
//!   disc is always in that case, even on a compressed file system. Space allocated but
//!   never written (a preallocated file) counts as used there, so on a share such an image
//!   is caught only by the zero sampling; on a local disk `SEEK_HOLE` still finds it.
//! - **Zero runs** are sampled, which costs reads, and only in an area whose play time
//!   claims more than [`SUSPICIOUS_FRAMES_PER_SECTOR`] frame per sector holding data. A
//!   real disc's area uses about 0.22 (plain DSD stereo, less with more channels) to 0.5
//!   (DST) frames per sector, so an intact real disc is never sampled. Plain DSD
//!   silence is the byte 0x69 and every DST frame has a header: a long all-zero run is not
//!   audio. At most [`MAX_PROBES`] sector reads are made per area.
//!
//! Subtracting gap sectors cannot hide a real disc's track: at 7 frames a sector, the
//! capacity is about 14 to 30 times what a real disc's tracks use, so a real disc would
//! have to lose nearly all of an area to a gap before a track was refused, and then its
//! frames are not in the image anyway.
//!
//! An image padded with written zeros to more sectors than its play time's frames is not
//! sampled: its tracks list at their TOC sizes, bounded by the image's size (a DSF file's
//! 9408 bytes per stereo frame against 2048 bytes a sector: at most about 4.6 times it).

use std::fs::File;

use scarletbook::Area;
use scarletbook::audio::audio_sector_range;

use crate::disc::Disc;

/// The shortest hole or zero run that counts as missing data: 1 MiB, about 512 sectors.
pub const MIN_GAP_BYTES: u64 = 1 << 20;

/// An area is sampled for zero runs only when its play time claims more frames than this
/// per audio sector holding data (after holes). Real discs use 0.07 (plain DSD, 6
/// channels) to about 0.5 (DST): 1 is twice the densest real use, and still 7 times
/// below the capacity rule's 7, so an area claiming more than it is padded or damaged.
pub const SUSPICIOUS_FRAMES_PER_SECTOR: u64 = 1;

/// The most sector reads made to sample one area for zero runs.
pub const MAX_PROBES: u64 = 64;

/// The most hole/data boundaries followed in one area: a file fragmented into more is
/// treated as holding data past them.
const MAX_EXTENTS: usize = 4096;

/// The sector count to check the tracks of `area` of `disc` against: see the module
/// documentation. On any error, the image's own count (nothing is hidden).
pub fn data_sectors(disc: &Disc, area: &Area) -> u64 {
    let toc = disc.toc();
    let n = toc.sectors;
    let file = match disc.file() {
        Ok(f) => f,
        Err(e) => {
            log::debug!("{}: {} area: data not looked at: {e}", disc.path.display(), area.slot);
            return n;
        }
    };
    let stored = toc.sector_format.stored_size();
    if crate::platform::hole_bound(&file).is_some_and(|b| holes_cannot_matter(area, n, stored, b)) {
        // What the search below gives with no gaps.
        let (start, end) = audio_sector_range(area, n);
        return end.max(start);
    }
    let gaps = gap_sectors(&file, stored, area, n);
    if gaps > 0 {
        log::debug!(
            "{}: {} area: {gaps} audio sectors hold no data (holes or zero runs)",
            disc.path.display(),
            area.slot
        );
    }
    let (start, end) = audio_sector_range(area, n);
    // The area's existing sectors become [start, end - gaps): `audio_sector_range` ends
    // at the smaller of the area's end and the image's count.
    (end - gaps.min(end - start)).max(start)
}

/// Whether holes of at most `bound` bytes in an image of `n` sectors of `stored` bytes each
/// leave the result of the track check of `area` as it is without them: the area's play time
/// stays within [`SUSPICIOUS_FRAMES_PER_SECTOR`] frame per audio sector left, so no zero
/// sampling starts and every track (its frames end within the play time) fits the capacity
/// of the sectors left.
pub fn holes_cannot_matter(area: &Area, n: u64, stored: u64, bound: u64) -> bool {
    const _: () = assert!(SUSPICIOUS_FRAMES_PER_SECTOR <= scarletbook::audio::FRAMES_PER_SECTOR);
    let (start, end) = audio_sector_range(area, n);
    if end <= start || stored == 0 {
        return true;
    }
    let left = (end - start).saturating_sub(bound.div_ceil(stored));
    u64::from(area.playtime_frames()) <= left.saturating_mul(SUSPICIOUS_FRAMES_PER_SECTOR)
}

/// The audio sectors of `area` that lie in a long hole or a long zero run of `file`, an
/// image of `n` sectors of `stored` bytes each.
pub fn gap_sectors(file: &File, stored: u64, area: &Area, n: u64) -> u64 {
    let (start, end) = audio_sector_range(area, n);
    if end <= start || stored == 0 {
        return 0;
    }
    let holes = holes(file, stored, start, end);
    let hole_sectors: u64 = holes.iter().map(|(a, b)| b - a).sum();
    let present = (end - start) - hole_sectors;
    let claimed = u64::from(area.playtime_frames());
    if claimed <= present.saturating_mul(SUSPICIOUS_FRAMES_PER_SECTOR) {
        return hole_sectors;
    }
    let data = complement(start, end, &holes);
    hole_sectors + zero_sectors(file, stored, &data, present)
}

/// The sector ranges of `[start, end)` that lie wholly in a hole of at least
/// [`MIN_GAP_BYTES`], in order.
fn holes(file: &File, stored: u64, start: u64, end: u64) -> Vec<(u64, u64)> {
    let (first, last) = (start * stored, end * stored);
    let mut out = Vec::new();
    let mut pos = first;
    for _ in 0..MAX_EXTENTS {
        if pos >= last {
            break;
        }
        let Some(hole) = crate::platform::next_hole(file, pos) else { break };
        if hole >= last {
            break;
        }
        // No data after the hole: it runs to the end of the file.
        let data = match crate::platform::next_data(file, hole) {
            Ok(Some(d)) => d.min(last),
            Ok(None) => last,
            Err(_) => break,
        };
        if data <= hole {
            break;
        }
        if data - hole >= MIN_GAP_BYTES {
            let (a, b) = (hole.div_ceil(stored), data / stored);
            if a < b {
                out.push((a, b));
            }
        }
        pos = data;
    }
    out
}

/// `[start, end)` less the sorted, disjoint ranges `holes`.
fn complement(start: u64, end: u64, holes: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut at = start;
    for &(a, b) in holes {
        if a > at {
            out.push((at, a));
        }
        at = at.max(b);
    }
    if at < end {
        out.push((at, end));
    }
    out
}

/// The absolute sector of the `k`th sector of the ranges `data`, taken in order.
fn nth(data: &[(u64, u64)], mut k: u64) -> Option<u64> {
    for &(a, b) in data {
        if k < b - a {
            return Some(a + k);
        }
        k -= b - a;
    }
    None
}

/// Estimates the sectors of `data` (`present` sectors in all) that lie in zero runs of at
/// least [`MIN_GAP_BYTES`]. The sectors are cut into equal segments of at least that size,
/// at most `MAX_PROBES / 2` of them; a segment whose first and last stored sectors are
/// both all zero bytes counts as a zero run.
fn zero_sectors(file: &File, stored: u64, data: &[(u64, u64)], present: u64) -> u64 {
    let per_segment = MIN_GAP_BYTES.div_ceil(stored);
    let segments = (present / per_segment).min(MAX_PROBES / 2);
    if segments == 0 {
        return 0;
    }
    let mut buf = vec![0u8; stored as usize];
    let mut zero = |sector: Option<u64>| {
        sector.is_some_and(|s| {
            crate::platform::read_exact_at(file, &mut buf, s * stored).is_ok() && buf.iter().all(|&b| b == 0)
        })
    };
    let mut found = 0;
    for k in 0..segments {
        let (a, b) = (k * present / segments, (k + 1) * present / segments);
        if zero(nth(data, a)) && zero(nth(data, b - 1)) {
            found += b - a;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complement_of_holes() {
        assert_eq!(complement(10, 100, &[]), vec![(10, 100)]);
        assert_eq!(complement(10, 100, &[(10, 20), (50, 60)]), vec![(20, 50), (60, 100)]);
        assert_eq!(complement(10, 100, &[(20, 100)]), vec![(10, 20)]);
    }

    #[test]
    fn nth_sector_of_ranges() {
        let d = [(20, 50), (60, 100)];
        assert_eq!(nth(&d, 0), Some(20));
        assert_eq!(nth(&d, 29), Some(49));
        assert_eq!(nth(&d, 30), Some(60));
        assert_eq!(nth(&d, 69), Some(99));
        assert_eq!(nth(&d, 70), None);
    }
}
