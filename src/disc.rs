// SPDX-License-Identifier: GPL-3.0-or-later

//! One disc image: its TOC, its sector reader, and finding frames in its audio areas.
//!
//! An area's audio sectors hold frames in increasing frame number. A fresh
//! [`Demuxer`] fed sectors from any LSN yields correctly numbered frames from the first
//! frame that starts at or after that LSN. So to read frame `f`, it is enough to start a
//! demultiplexer at an LSN where the first frame is at most `f`, and read forward.
//!
//! Each area keeps an index of such points, (frame number, LSN), learned from every scan
//! and every probe. A frame is located from the nearest point at or before it, or by an
//! interpolating binary search over the area's sectors between the nearest known points.
//! If the points ever contradict each other (frame numbers that decrease as the LSN grows,
//! which a well-formed disc never has), the area is marked irregular and every search
//! starts at the area's first sector instead: slower, but exactly a sequential scan.
//!
//! The points can also come from the persistent seek index ([`crate::index_cache`]) of an
//! earlier mount. Those are never trusted: they aim the search, and a cursor may start at
//! one, but it checks the first frame it reads there exactly as a probe would before it
//! returns anything, and falls back to a search if the check fails. On any doubt the
//! loaded points of the whole disc are dropped.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use scarletbook::audio::audio_sector_range;
use scarletbook::{AreaFrame, Demuxer, FrameContent, FrameKind, Image, SECTOR_SIZE, Slot, Toc};

use crate::handles::Stamp;
use crate::index_cache::{AreaShape, ImageKey, IndexCache};

/// Sectors read per call to the image by a sequential scan.
const SCAN_CHUNK: u64 = 64;

/// Bounds of the sectors read per call by a probe of the binary search. Each area sizes
/// its probes to about two of its average frames, enough to find one whole frame in most
/// probes (see [`probe_chunk`]).
const PROBE_CHUNK_MIN: u64 = 4;

/// Bytes of DSF audio a seek is assumed to need first (a common read size of players and
/// of the kernel for a random read); the first read of a located cursor is sized for it.
const FIRST_READ_BYTES: u64 = 128 << 10;

/// Most sectors a probe reads before giving up on finding a frame: two of the largest
/// frames (65 536 bytes each) and a little more.
const PROBE_LIMIT: u64 = 80;

/// Thresholds of the frame search. The defaults suit real discs; tests make every read
/// search, to exercise the search on small images.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    /// A search stops when the interval left is this many sectors or fewer, and scans it.
    /// Above 1, an area whose frames are larger uses about two of its average frames
    /// instead (at most [`MAX_CLOSE`]): scanning them costs no extra call to the image,
    /// another probe does.
    pub search_close: u64,
    /// A known point this many frames or fewer before the target is scanned from directly.
    pub scan_from_point: u32,
    /// A cursor this many frames or fewer behind a read's first missing frame is moved
    /// forward rather than replaced by a search.
    pub reuse_slack: u32,
    /// Aim the first probes of a search with the track start LSNs of the area TOC. They
    /// are only hints: every probe is checked like any other.
    pub toc_hints: bool,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { search_close: 8, scan_from_point: 48, reuse_slack: 375, toc_hints: true }
    }
}

impl Tuning {
    /// Search as much as possible: every read that does not continue a cursor exactly
    /// searches down to a single sector.
    pub fn exhaustive() -> Self {
        Tuning { search_close: 1, scan_from_point: 0, reuse_slack: 0, toc_hints: true }
    }
}

/// Learn a point from a sequential scan at most once per this many frames: fewer than the
/// default `scan_from_point`, so that any frame of a stretch read once can be started at
/// directly from a point, in this mount or (through the persistent index) a later one.
const LEARN_EVERY: u32 = 40;

/// A scan does not learn a point this many frames or fewer from a known one.
const LEARN_NEAR: u32 = LEARN_EVERY / 4;

/// Most points kept per area.
const MAX_POINTS: usize = crate::index_cache::MAX_POINTS;

/// Sectors per probe call for an area of `sectors` sectors and `frames` frames: about two
/// average frames and a sector, so that a probe usually finds a whole frame in one call
/// without reading much more. A DST stereo frame takes 2 to 3 sectors on real discs, a
/// plain DSD stereo frame 5 and a 6-channel one 14.
fn probe_chunk(sectors: u64, frames: u32) -> u64 {
    let per_frame = sectors.div_ceil(u64::from(frames.max(1)));
    (2 * per_frame + 1).clamp(PROBE_CHUNK_MIN, PROBE_LIMIT)
}

/// Sectors of the first read of a cursor started by a search: the frames of
/// [`FIRST_READ_BYTES`] of DSF audio, two more (the cursor starts before its frame), and
/// the stop distance of the search, at the area's average frame size.
fn first_chunk(sectors: u64, frames: u32, channels: u8, close: u64) -> u64 {
    let per_frame = FRAME_BYTES_PER_CHANNEL * u64::from(channels.max(1));
    let want = FIRST_READ_BYTES.div_ceil(per_frame) + 2;
    let n = sectors.saturating_mul(want).div_ceil(u64::from(frames.max(1)));
    n.saturating_add(close).clamp(PROBE_CHUNK_MIN, 2 * SCAN_CHUNK)
}

/// Most sectors an area's stop distance grows to (see [`Tuning::search_close`]).
pub const MAX_CLOSE: u64 = 32;

/// The stop distance of an area's searches: see [`Tuning::search_close`].
fn close_sectors(search_close: u64, sectors: u64, frames: u32) -> u64 {
    if search_close <= 1 {
        return search_close;
    }
    let per_frame = sectors.div_ceil(u64::from(frames.max(1)));
    search_close.max((2 * per_frame).min(MAX_CLOSE))
}

/// DSD bytes of one channel of one frame.
const FRAME_BYTES_PER_CHANNEL: u64 = scarletbook::audio::FRAME_BYTES_PER_CHANNEL as u64;

/// An error reading audio.
#[derive(Debug)]
pub enum ReadError {
    /// The image could not be read.
    Io(io::Error),
    /// The engine refused a read (the image changed size, or an internal check failed).
    Engine(scarletbook::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "{e}"),
            ReadError::Engine(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<scarletbook::Error> for ReadError {
    fn from(e: scarletbook::Error) -> Self {
        match e {
            scarletbook::Error::Io(e) => ReadError::Io(e),
            e => ReadError::Engine(e),
        }
    }
}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> Self {
        ReadError::Io(e)
    }
}

/// A disc image with its parsed TOC. The handle its TOC was read through is kept for its
/// audio reads, in the process's bounded cache of open images ([`crate::handles`]); the
/// image is opened again when that handle was closed.
#[derive(Debug)]
pub struct Disc {
    /// Process-unique id, part of the frame cache key.
    pub id: u32,
    /// The image file.
    pub path: PathBuf,
    /// Modification time of the image file, shown on every node under it.
    pub mtime: SystemTime,
    /// The table of contents.
    pub toc: Toc,
    /// Per area of `toc.areas`, in the same order.
    pub areas: Vec<AreaState>,
    /// Search thresholds.
    pub tuning: Tuning,
    /// The key of the image's handle in [`crate::handles::global`].
    handle: u64,
    /// Calls to the image for audio sectors so far.
    reads: AtomicU64,
    /// Bytes of audio sectors read from the image so far.
    bytes: AtomicU64,
    /// Cursor starts at loaded points that passed their check, and that failed it.
    checks: [AtomicU64; 2],
}

/// An area's index, shared with the persistent seek index that saves it.
pub(crate) type SharedIndex = Mutex<Index>;

/// Whether the points changed since the last save; clears the flag.
pub(crate) fn take_dirty(index: &SharedIndex) -> bool {
    std::mem::take(&mut index.lock().unwrap_or_else(|p| p.into_inner()).dirty)
}

/// Marks the points as not saved.
pub(crate) fn mark_dirty(index: &SharedIndex) {
    index.lock().unwrap_or_else(|p| p.into_inner()).dirty = true;
}

/// The points to save: the checked ones and the loaded ones not yet checked, if they all
/// agree; only the checked ones otherwise.
pub(crate) fn points_for_save(index: &SharedIndex) -> Vec<Point> {
    index.lock().unwrap_or_else(|p| p.into_inner()).for_save()
}

/// Calls to images for audio sectors, and their bytes, by every disc of the process.
static TOTAL_READS: AtomicU64 = AtomicU64::new(0);
static TOTAL_BYTES: AtomicU64 = AtomicU64::new(0);

/// Calls to images for audio sectors by every disc of the process so far, and the bytes
/// they read (for diagnostics: sacdfs logs them when it stops).
pub fn total_reads() -> (u64, u64) {
    (TOTAL_READS.load(Ordering::Relaxed), TOTAL_BYTES.load(Ordering::Relaxed))
}

/// The reading state of one audio area.
#[derive(Debug)]
pub struct AreaState {
    /// Which slot the area came from.
    pub slot: Slot,
    /// Channel count from the area TOC.
    pub channels: u8,
    /// The area's audio sectors that exist in the image, `[start, end)`.
    pub start_lsn: u64,
    /// See `start_lsn`.
    pub end_lsn: u64,
    /// The area's play time in frames, a hint for the search.
    playtime: u32,
    /// Sectors per probe call.
    probe_chunk: u64,
    /// Sectors of the first read of a cursor started by a search.
    first_chunk: u64,
    /// The stop distance of a search (see [`Tuning::search_close`]).
    close: u64,
    /// (start frame, start LSN) of the tracks in the area TOC, increasing in both, as
    /// hints for the search; empty if the TOC's track starts are not in order.
    hints: Vec<Point>,
    index: Arc<SharedIndex>,
}

impl AreaState {
    /// The TOC hints around `frame`: the last track start at or before it and the first
    /// one after it.
    fn hints_around(&self, frame: u32) -> (Option<Point>, Option<Point>) {
        let i = self.hints.partition_point(|&(f, _)| f <= frame);
        (i.checked_sub(1).and_then(|j| self.hints.get(j)).copied(), self.hints.get(i).copied())
    }
}

/// A frame number and an LSN.
type Point = (u32, u64);

/// The track starts of an area TOC as search hints (see [`AreaState::hints`]).
fn toc_hints(starts: impl Iterator<Item = Point>, start_lsn: u64, end_lsn: u64) -> Vec<Point> {
    let mut hints: Vec<Point> = starts.filter(|&(_, l)| (start_lsn..end_lsn).contains(&l)).collect();
    hints.sort_unstable();
    hints.dedup_by_key(|h| h.0);
    if hints.windows(2).any(|w| w[1].1 <= w[0].1) {
        hints.clear();
    }
    hints
}

#[derive(Debug, Default)]
pub(crate) struct Index {
    /// frame number -> an LSN from which a fresh demultiplexer first yields a frame
    /// numbered at most this one, and that frame starts at or after the LSN.
    points: BTreeMap<u32, u64>,
    irregular: bool,
    /// Points loaded from the persistent seek index, not checked: increasing in frame and
    /// LSN, inside the area. They only aim searches and start cursors that check them.
    pending: Vec<Point>,
    /// Whether anything to save changed since the last save.
    dirty: bool,
}

impl Index {
    /// Records a point; marks the area irregular if it contradicts the others.
    fn insert(&mut self, frame: u32, lsn: u64) {
        self.check_and_insert(frame, lsn, true);
    }

    /// Checks a point against the others, marking the area irregular if it contradicts
    /// them, and records it if `store`.
    fn check_and_insert(&mut self, frame: u32, lsn: u64, store: bool) {
        if self.irregular {
            return;
        }
        let before = self.points.get(&frame).copied();
        let below = self.points.range(..frame).next_back().map(|(_, &l)| l);
        let above = self.points.range(frame + 1..).next().map(|(_, &l)| l);
        if below.is_some_and(|l| l > lsn) || above.is_some_and(|l| l < lsn) {
            log::debug!(
                "frame {frame} at LSN {lsn} contradicts the frame index: scanning this area from its start"
            );
            self.irregular = true;
            self.points.clear();
            self.pending.clear();
            self.dirty = true;
            return;
        }
        if !store || (self.points.len() >= MAX_POINTS && before.is_none()) {
            return;
        }
        let e = self.points.entry(frame).or_insert(lsn);
        *e = (*e).max(lsn);
        self.dirty |= before != Some(*e);
    }

    /// Drops the loaded points.
    fn discard_pending(&mut self) {
        if !self.pending.is_empty() {
            self.pending.clear();
            self.dirty = true;
        }
    }

    /// The last loaded point numbered below `frame`, and the first one at or after it.
    fn pending_around(&self, frame: u32) -> (Option<Point>, Option<Point>) {
        let i = self.pending.partition_point(|&(f, _)| f < frame);
        (i.checked_sub(1).and_then(|j| self.pending.get(j)).copied(), self.pending.get(i).copied())
    }

    /// See [`points_for_save`].
    fn for_save(&self) -> Vec<Point> {
        if self.irregular {
            return Vec::new();
        }
        let mut all = Index { points: self.points.clone(), ..Index::default() };
        for &(f, l) in &self.pending {
            all.insert(f, l);
            if all.irregular {
                return self.points.iter().map(|(&f, &l)| (f, l)).collect();
            }
        }
        all.points.into_iter().collect()
    }

    /// Whether a checked or loaded point lies within `d` frames of `frame`.
    fn has_point_near(&self, frame: u32, d: u32) -> bool {
        let (lo, hi) = (frame.saturating_sub(d), frame.saturating_add(d));
        let i = self.pending.partition_point(|&(f, _)| f < lo);
        self.points.range(lo..=hi).next().is_some() || self.pending.get(i).is_some_and(|&(f, _)| f <= hi)
    }
}

impl Disc {
    /// Opens an image and reads its TOC, with the persistent seek index installed for the
    /// process ([`crate::index_cache::install`]), if any. The file stays open for audio
    /// reads, as long as the cache of open images ([`crate::handles`]) keeps it.
    pub fn open(path: &Path, id: u32, tuning: Tuning) -> Result<Disc, scarletbook::Error> {
        Self::open_with(path, id, tuning, crate::index_cache::global().map(Arc::as_ref))
    }

    /// [`Disc::open`] with the persistent seek index `cache`: the image's saved points are
    /// loaded (unchecked) from it, and the disc's points are saved to it by
    /// [`IndexCache::flush`].
    pub fn open_with(
        path: &Path,
        id: u32,
        tuning: Tuning,
        cache: Option<&IndexCache>,
    ) -> Result<Disc, scarletbook::Error> {
        let file = File::open(path)?;
        let meta = file.metadata().ok();
        let mtime = meta.as_ref().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH);
        let disc = scarletbook::Disc::open(io::BufReader::new(file))?;
        for w in disc.warnings() {
            log::debug!("{}: {w}", path.display());
        }
        let toc = disc.toc().clone();
        let file = disc.into_inner().into_inner();
        let areas = toc
            .areas
            .iter()
            .map(|a| {
                let (start_lsn, end_lsn) = audio_sector_range(a, toc.sectors);
                let playtime = a.playtime_frames();
                let sectors = end_lsn.saturating_sub(start_lsn);
                AreaState {
                    slot: a.slot,
                    channels: a.channel_count,
                    start_lsn,
                    end_lsn,
                    playtime,
                    probe_chunk: probe_chunk(sectors, playtime),
                    close: close_sectors(tuning.search_close, sectors, playtime),
                    first_chunk: first_chunk(
                        sectors,
                        playtime,
                        a.channel_count,
                        close_sectors(tuning.search_close, sectors, playtime),
                    ),
                    hints: toc_hints(
                        a.tracks.iter().map(|t| (t.start_frame, u64::from(t.start_lsn))),
                        start_lsn,
                        end_lsn,
                    ),
                    index: Arc::new(Mutex::new(Index::default())),
                }
            })
            .collect();
        log_unspecified_layouts(path, &toc);
        let disc = Disc {
            id,
            path: path.to_owned(),
            mtime,
            toc,
            areas,
            tuning,
            handle: crate::handles::next_key(),
            reads: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            checks: [AtomicU64::new(0), AtomicU64::new(0)],
        };
        if let (Some(cache), Some(meta)) = (cache, meta.as_ref()) {
            disc.attach(cache, meta);
        }
        // Keep the handle the TOC was read through, so that the first audio read does not
        // open the image again (a round trip or two on a network share, #26).
        if let Some(meta) = meta {
            match Image::with_format(file, disc.toc.sector_format) {
                Ok(image) if image.sector_count() == disc.toc.sectors => {
                    crate::handles::global().insert(disc.handle, image, Stamp::of(&meta));
                }
                Ok(_) => {}
                Err(e) => log::debug!("{}: {e}", path.display()),
            }
        }
        Ok(disc)
    }

    /// Opens the image for audio reads; refused if its size changed since the TOC was read.
    fn open_image(&self) -> Result<(Image<File>, Stamp), ReadError> {
        let file = File::open(&self.path)?;
        let stamp = Stamp::of(&file.metadata()?);
        let image = Image::with_format(file, self.toc.sector_format)?;
        if image.sector_count() != self.toc.sectors {
            return Err(ReadError::Io(io::Error::other("image file changed size since it was mounted")));
        }
        Ok((image, stamp))
    }

    /// Loads the image's saved points from `cache` and registers the disc's for saving.
    fn attach(&self, cache: &IndexCache, meta: &std::fs::Metadata) {
        let key = match ImageKey::new(&self.path, meta) {
            Ok(k) => k,
            Err(e) => {
                log::debug!("{}: no seek index cache: {e}", self.path.display());
                return;
            }
        };
        let shapes: Vec<AreaShape> = self
            .areas
            .iter()
            .map(|a| AreaShape { start_lsn: a.start_lsn, end_lsn: a.end_lsn, channels: a.channels })
            .collect();
        if let Some(loaded) = cache.load(&key, &shapes) {
            let mut n = 0;
            for (st, points) in self.areas.iter().zip(loaded) {
                n += points.len();
                st.index.lock().unwrap_or_else(|p| p.into_inner()).pending = points;
            }
            log::debug!("{}: {n} seek index points loaded", self.path.display());
        }
        cache.register(key, shapes, self.areas.iter().map(|a| a.index.clone()).collect());
    }

    /// Drops the loaded points of every area: one of them proved wrong.
    fn discard_pending(&self) {
        for st in &self.areas {
            st.index.lock().unwrap_or_else(|p| p.into_inner()).discard_pending();
        }
    }

    /// Reads `buf.len() / 2048` sectors at `lsn`. `Ok(false)` if they are not all in the
    /// image.
    pub fn read_sectors(&self, lsn: u64, buf: &mut [u8]) -> Result<bool, ReadError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(buf.len() as u64, Ordering::Relaxed);
        TOTAL_READS.fetch_add(1, Ordering::Relaxed);
        TOTAL_BYTES.fetch_add(buf.len() as u64, Ordering::Relaxed);
        let image = crate::handles::global().get(self.handle, &self.path, || self.open_image())?;
        let mut image = image.lock().unwrap_or_else(|p| p.into_inner());
        Ok(image.read_sectors(lsn, buf)?)
    }

    /// A cursor positioned so that its next timeline position can be `frame`, found with
    /// the area's index and, if needed, a search.
    ///
    /// The cursor starts where a fresh demultiplexer first yields a frame numbered *below*
    /// `frame`, never at a frame numbered `frame`: a frame that repeats an earlier frame's
    /// number (a hostile timecode) is discarded by a scan that has seen the original, but
    /// a demultiplexer started between the two would take the copy for the original.
    pub fn locate(&self, area: usize, frame: u32) -> Result<Cursor, ReadError> {
        self.locate_from(area, frame, true)
    }

    /// [`Disc::locate`], starting at a loaded point when `loaded` is set.
    fn locate_from(&self, area: usize, frame: u32, loaded: bool) -> Result<Cursor, ReadError> {
        let st = self.area(area)?;
        let (mut lo, mut lo_frame, mut hi, mut hi_frame, cached) = {
            let index = st.index.lock().unwrap_or_else(|p| p.into_inner());
            if index.irregular || frame == 0 {
                return Ok(Cursor::new(self, area, st.start_lsn, frame));
            }
            // Start before an earlier frame, never at `frame` itself: see `Disc::locate`.
            let below = index.points.range(..frame).next_back().map(|(&f, &l)| (f, l));
            let above = index.points.range(frame..).next().map(|(&f, &l)| (f, l));
            if let Some((f, l)) = below {
                if frame - f <= self.tuning.scan_from_point {
                    return Ok(Cursor::new(self, area, l, frame));
                }
            }
            let cached = index.pending_around(frame);
            // A loaded point close before `frame`, past the closest checked one: start
            // there, and let the cursor check it.
            if let (true, Some((f, l))) = (loaded, cached.0) {
                if frame - f <= self.tuning.scan_from_point && below.is_none_or(|(_, bl)| l > bl) {
                    return Ok(Cursor::new(self, area, l, frame).checking(f));
                }
            }
            // Without points, the area starts near frame 0 and ends near its play time.
            let (lo, lo_frame) = below.map_or((st.start_lsn, Some(0)), |(f, l)| (l, Some(f)));
            let (hi, hi_frame) =
                above.map_or((st.end_lsn, Some(st.playtime)), |(f, l)| ((l + 1).min(st.end_lsn), Some(f)));
            (lo, lo_frame, hi, hi_frame, cached)
        };
        // Invariant: starting at `lo` finds `frame` (it is the area start, or a point whose
        // first frame comes before `frame`). Frames at `hi` or later are `frame` or later.
        // The probes are aimed with the closest of the TOC's track starts and the loaded
        // points around `frame`; both are only hints.
        let (toc_lo, toc_hi) = if self.tuning.toc_hints { st.hints_around(frame) } else { (None, None) };
        let hint_lo = closest(toc_lo, cached.0, |a, b| a.0 >= b.0);
        let hint_hi = closest(toc_hi, cached.1, |a, b| a.0 <= b.0);
        // The sectors the last probe at `lo` read, which the cursor reads first.
        let mut lo_buf: Option<Vec<u8>> = None;
        let mut bisect_next = false;
        // Probes in a row that did not halve the interval.
        let mut slow = 0;
        while hi > lo && hi - lo > st.close {
            // Interpolate between the closest of the known bounds and the TOC's track
            // starts inside them; the hints only aim the probe.
            let (a, af) = match hint_lo {
                Some((f, l)) if l > lo && l < hi && lo_frame.is_none_or(|lf| f >= lf) => (l, Some(f)),
                _ => (lo, lo_frame),
            };
            let (b, bf) = match hint_hi {
                Some((f, l)) if l > a && l < hi && hi_frame.is_none_or(|hf| f <= hf) => (l, Some(f)),
                _ => (hi, hi_frame),
            };
            let mid = match (af, bf, bisect_next) {
                (Some(lf), Some(hf), false) if hf > lf => {
                    // Frames are spread fairly evenly over the sectors. Aim a little past
                    // the estimate, towards the farther bound, so that a good estimate
                    // closes the interval from both sides.
                    let span = (b - a) as u128;
                    let at = u128::from(frame.saturating_sub(lf)) * span / u128::from(hf - lf);
                    let est = a + at as u64;
                    let margin = st.close / 2;
                    let est = if est - lo < hi - est { est + margin } else { est.saturating_sub(margin) };
                    est.clamp(lo + 1, hi - 1)
                }
                _ => lo + (hi - lo) / 2,
            };
            let before = hi - lo;
            let (found, buf) = self.probe(area, mid)?;
            match found {
                Some(n) if n < frame => {
                    lo = mid;
                    lo_frame = Some(n);
                    lo_buf = Some(buf);
                }
                Some(n) => {
                    hi = mid;
                    hi_frame = Some(n);
                }
                None => {
                    hi = mid;
                }
            }
            if st.index.lock().unwrap_or_else(|p| p.into_inner()).irregular {
                return Ok(Cursor::new(self, area, st.start_lsn, frame));
            }
            // Alternate to bisection when interpolation did not halve the interval, unless
            // the probe found a frame within about twice the stop distance of `frame`: then
            // the estimate was good, it moved only the bound near it, and the next
            // interpolation (aimed past the estimate, towards the farther bound) usually
            // closes the interval from the other side. Bisect at least every third probe,
            // so that a hostile disc cannot make the search linear.
            let halved = (hi - lo) * 2 <= before;
            slow = if halved { 0 } else { slow + 1 };
            let close = found.is_some_and(|n| {
                u128::from(n.abs_diff(frame)) * u128::from(st.end_lsn - st.start_lsn)
                    <= 2 * u128::from(st.close.max(1)) * u128::from(st.playtime.max(1))
            });
            let keep_aiming = close && slow < 2;
            bisect_next = !(bisect_next || halved || keep_aiming);
        }
        let mut cursor = Cursor::new(self, area, lo, frame);
        if let Some(buf) = lo_buf {
            cursor.prefill(lo, buf);
        }
        Ok(cursor)
    }

    /// The number of the first frame a fresh demultiplexer yields from `lsn`, if it finds
    /// one within [`PROBE_LIMIT`] sectors, and the sectors read from `lsn`. Records the
    /// frame in the index.
    fn probe(&self, area: usize, lsn: u64) -> Result<(Option<u32>, Vec<u8>), ReadError> {
        let st = self.area(area)?;
        let mut demux = Demuxer::new(st.channels);
        let mut buf = Vec::new();
        let mut at = lsn;
        let limit = (lsn + PROBE_LIMIT).min(st.end_lsn);
        while at < limit {
            let n = st.probe_chunk.min(limit - at);
            let old = buf.len();
            buf.resize(old + n as usize * SECTOR_SIZE, 0);
            let chunk = &mut buf[old..];
            if !self.read_sectors(at, chunk)? {
                return Err(ReadError::Io(io::Error::other("audio sectors outside the image")));
            }
            for sector in chunk.chunks_exact(SECTOR_SIZE) {
                if let Ok(s) = <&[u8; SECTOR_SIZE]>::try_from(sector) {
                    demux.push_sector(s);
                }
                if let Some(f) = demux.pop() {
                    st.index.lock().unwrap_or_else(|p| p.into_inner()).insert(f.number, lsn);
                    return Ok((Some(f.number), buf));
                }
            }
            at += n;
        }
        if limit == st.end_lsn {
            demux.finish();
            if let Some(f) = demux.pop() {
                st.index.lock().unwrap_or_else(|p| p.into_inner()).insert(f.number, lsn);
                return Ok((Some(f.number), buf));
            }
        }
        Ok((None, buf))
    }

    fn area(&self, area: usize) -> Result<&AreaState, ReadError> {
        self.areas.get(area).ok_or(ReadError::Io(io::Error::other("no such area")))
    }

    /// Whether the area's frame index has been found inconsistent.
    pub fn is_irregular(&self, area: usize) -> bool {
        self.areas.get(area).is_some_and(|st| st.index.lock().unwrap_or_else(|p| p.into_inner()).irregular)
    }

    /// Calls to the image for audio sectors so far (for tests and diagnostics): on a
    /// network share each is a round trip.
    pub fn sector_reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }

    /// Bytes of audio sectors read from the image so far (for tests and diagnostics).
    pub fn sector_bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// The number of index points of an area (for tests and diagnostics).
    pub fn index_points(&self, area: usize) -> usize {
        self.areas.get(area).map_or(0, |st| st.index.lock().unwrap_or_else(|p| p.into_inner()).points.len())
    }

    /// Cursor starts at points loaded from the persistent seek index that passed their
    /// check, and that failed it (for tests and diagnostics).
    pub fn loaded_point_checks(&self) -> (u64, u64) {
        (self.checks[0].load(Ordering::Relaxed), self.checks[1].load(Ordering::Relaxed))
    }

    /// The number of points of an area loaded from the persistent seek index and not
    /// dropped (for tests and diagnostics).
    pub fn loaded_points(&self, area: usize) -> usize {
        self.areas.get(area).map_or(0, |st| st.index.lock().unwrap_or_else(|p| p.into_inner()).pending.len())
    }
}

impl Drop for Disc {
    /// Closes the image's handle.
    fn drop(&mut self) {
        crate::handles::global().remove(self.handle);
    }
}

/// Of two optional hints, the one `better` prefers.
fn closest(a: Option<Point>, b: Option<Point>, better: impl Fn(Point, Point) -> bool) -> Option<Point> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if better(x, y) { x } else { y }),
        (x, y) => x.or(y),
    }
}

/// A forward reader of one area's timeline from some LSN: a demultiplexer and its position.
#[derive(Debug)]
pub struct Cursor {
    /// Which area of the disc.
    pub area: usize,
    disc_id: u32,
    channels: u8,
    demux: Demuxer,
    next_lsn: u64,
    end_lsn: u64,
    buf: Vec<u8>,
    buf_lsn: u64,
    buf_sectors: u64,
    ready: VecDeque<AreaFrame>,
    peeked: Option<AreaFrame>,
    finished: bool,
    exhausted: bool,
    /// The next timeline position this cursor can return.
    next_t: u32,
    /// A point being learned: the LSN of a sector and the first frame popped after it.
    learning: Option<(u64, u32)>,
    last_learned: Option<u32>,
    /// Sectors of the next read of the image.
    chunk: u64,
    /// The cursor starts at an unchecked loaded point numbered this: its first frame must
    /// be checked before anything is returned.
    unchecked: Option<u32>,
    /// While checking: the cursor gives up at this LSN, as a probe does.
    stop_at: u64,
}

impl Cursor {
    fn new(disc: &Disc, area: usize, lsn: u64, next_t: u32) -> Cursor {
        let (channels, end_lsn, chunk) =
            disc.areas.get(area).map_or((0, lsn, SCAN_CHUNK), |st| (st.channels, st.end_lsn, st.first_chunk));
        Cursor {
            area,
            disc_id: disc.id,
            channels,
            demux: Demuxer::new(channels),
            next_lsn: lsn,
            end_lsn,
            buf: Vec::new(),
            buf_lsn: 0,
            buf_sectors: 0,
            ready: VecDeque::new(),
            peeked: None,
            finished: false,
            exhausted: false,
            next_t,
            learning: None,
            last_learned: None,
            chunk,
            unchecked: None,
            stop_at: u64::MAX,
        }
    }

    /// The same cursor, started at a loaded point numbered `frame`, which it checks.
    fn checking(mut self, frame: u32) -> Cursor {
        self.unchecked = Some(frame);
        self
    }

    /// Gives the cursor the sectors at `lsn` that a probe already read.
    fn prefill(&mut self, lsn: u64, buf: Vec<u8>) {
        self.buf_sectors = (buf.len() / SECTOR_SIZE) as u64;
        self.buf_lsn = lsn;
        self.buf = buf;
        // The first read is sized for the whole first request, part of which is here.
        self.chunk = self.chunk.saturating_sub(self.buf_sectors).max(PROBE_CHUNK_MIN);
    }

    /// The next timeline position this cursor can return.
    pub fn next_position(&self) -> u32 {
        self.next_t
    }

    /// Whether this cursor reads `area` of `disc`.
    pub fn reads(&self, disc: &Disc, area: usize) -> bool {
        self.disc_id == disc.id && self.area == area
    }

    /// The content of timeline position `t`, which must be at or after
    /// [`Cursor::next_position`]. Positions before `t` are skipped.
    pub fn content(&mut self, disc: &Disc, t: u32) -> Result<FrameContent, ReadError> {
        if t < self.next_t {
            return Err(ReadError::Io(io::Error::other("cursor moved backwards")));
        }
        if let Some(cached) = self.unchecked.take() {
            self.check_start(disc, t, cached)?;
        }
        while !self.exhausted && self.peeked.as_ref().is_none_or(|f| f.number < t) {
            match self.pull(disc)? {
                Some(f) => self.peeked = Some(f),
                None => {
                    self.peeked = None;
                    self.exhausted = true;
                }
            }
        }
        self.next_t = t + 1;
        Ok(match self.peeked.take_if(|f| f.number == t) {
            Some(f) => match f.kind {
                FrameKind::Dsd => FrameContent::Dsd(f.data),
                FrameKind::Dst => FrameContent::Dst(f.data),
            },
            None => FrameContent::Missing,
        })
    }

    /// Checks the start of a cursor at a loaded point numbered `cached`, before position
    /// `t` (the first one asked for) is returned, as a probe there would be: the first
    /// frame must be found within [`PROBE_LIMIT`] sectors and numbered below `t`. Then the
    /// start is a checked point, and is recorded as one. Otherwise the cursor is replaced
    /// by a search that ignores the loaded points, and they are dropped; so are they if
    /// the frame contradicts the loaded point.
    fn check_start(&mut self, disc: &Disc, t: u32, cached: u32) -> Result<(), ReadError> {
        let start = self.next_lsn;
        self.stop_at = start.saturating_add(PROBE_LIMIT);
        let first = self.pull(disc);
        self.stop_at = u64::MAX;
        let st = disc.area(self.area)?;
        match first? {
            Some(f) if f.number < t => {
                let irregular = {
                    let mut index = st.index.lock().unwrap_or_else(|p| p.into_inner());
                    index.insert(f.number, start);
                    index.irregular
                };
                disc.checks[0].fetch_add(1, Ordering::Relaxed);
                if f.number > cached {
                    log::debug!("{}: a loaded seek index point was wrong", disc.path.display());
                    disc.discard_pending();
                }
                if irregular {
                    *self = disc.locate_from(self.area, t, false)?;
                } else {
                    self.ready.push_front(f);
                }
            }
            _ => {
                disc.checks[1].fetch_add(1, Ordering::Relaxed);
                log::debug!("{}: a loaded seek index point was wrong", disc.path.display());
                disc.discard_pending();
                *self = disc.locate_from(self.area, t, false)?;
            }
        }
        Ok(())
    }

    /// The next accepted frame of the area, `None` at its end (or at `stop_at`).
    fn pull(&mut self, disc: &Disc) -> Result<Option<AreaFrame>, ReadError> {
        loop {
            if let Some(f) = self.ready.pop_front() {
                return Ok(Some(f));
            }
            if self.finished || (self.next_lsn >= self.stop_at && self.next_lsn < self.end_lsn) {
                return Ok(None);
            }
            if self.next_lsn >= self.end_lsn {
                self.demux.finish();
                while let Some(f) = self.demux.pop() {
                    self.ready.push_back(f);
                }
                self.finished = true;
                continue;
            }
            let lsn = self.next_lsn;
            if !(self.buf_lsn..self.buf_lsn + self.buf_sectors).contains(&lsn) {
                let n = self.chunk.min(self.end_lsn - lsn);
                self.chunk = SCAN_CHUNK;
                self.buf.resize(n as usize * SECTOR_SIZE, 0);
                if !disc.read_sectors(lsn, &mut self.buf)? {
                    return Err(ReadError::Io(io::Error::other("audio sectors outside the image")));
                }
                self.buf_lsn = lsn;
                self.buf_sectors = n;
            }
            let at = (lsn - self.buf_lsn) as usize * SECTOR_SIZE;
            if let Some(s) =
                self.buf.get(at..at + SECTOR_SIZE).and_then(|s| <&[u8; SECTOR_SIZE]>::try_from(s).ok())
            {
                self.demux.push_sector(s);
            }
            self.next_lsn += 1;
            let before = self.ready.len();
            while let Some(f) = self.demux.pop() {
                self.ready.push_back(f);
            }
            self.learn(disc, lsn, before);
        }
    }

    /// Learns index points from the frames popped after pushing sector `lsn` (those at
    /// `ready[before..]`).
    ///
    /// The frames popped after a push are delivered by frame starts in that sector: all of
    /// them except possibly the first started in it. So the second one, or else the next
    /// frame popped later, started in this sector, and a fresh demultiplexer started here
    /// yields it or an earlier frame first.
    fn learn(&mut self, disc: &Disc, lsn: u64, before: usize) {
        let popped = self.ready.len() - before;
        if let Some((l, after)) = self.learning {
            if let Some(f) = self.ready.iter().skip(before).find(|f| f.number > after) {
                let n = f.number;
                self.record(disc, n, l);
            }
            if popped > 0 {
                self.learning = None;
            }
            return;
        }
        if popped == 0 || self.last_learned.is_some_and(|n| self.ready[before].number < n + LEARN_EVERY) {
            return;
        }
        if popped >= 2 {
            let n = self.ready[before + 1].number;
            self.record(disc, n, lsn);
        } else {
            self.learning = Some((lsn, self.ready[before].number));
        }
    }

    fn record(&mut self, disc: &Disc, frame: u32, lsn: u64) {
        self.last_learned = Some(frame);
        if let Some(st) = disc.areas.get(self.area) {
            let mut index = st.index.lock().unwrap_or_else(|p| p.into_inner());
            // A stretch read again from another start (in this mount, or from a loaded
            // point) learns points a frame or two from the old ones, which add nothing and
            // would pile up over remounts; but each is still checked against the others.
            let store = !index.has_point_near(frame, LEARN_NEAR);
            index.check_and_insert(frame, lsn, store);
        }
    }

    /// The channel count of the area.
    pub fn channels(&self) -> u8 {
        self.channels
    }
}

/// Logs, once per disc, the areas without a loudspeaker layout the specification defines
/// (specification 3, 3.4.1): their WAV channel mask and DSF channel type come from the
/// channel count alone ([`crate::wav::channel_mask`], [`crate::dsf::channel_type`]).
fn log_unspecified_layouts(path: &Path, toc: &Toc) {
    let unspecified: Vec<String> = toc
        .areas
        .iter()
        .filter_map(|a| match a.loudspeaker_layout() {
            scarletbook::LoudspeakerLayout::Unspecified { channels, config } => {
                Some(format!("{:?} area (channel count {channels}, configuration {config:#04x})", a.slot))
            }
            _ => None,
        })
        .collect();
    if !unspecified.is_empty() {
        log::info!(
            "{}: no defined loudspeaker layout in {}; speakers assigned from the channel count",
            path.display(),
            unspecified.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn toc_hints_in_order_only() {
        let h = toc_hints([(300, 40), (0, 10), (150, 25), (150, 26), (900, 99)].into_iter(), 10, 50);
        assert_eq!(h, [(0, 10), (150, 25), (300, 40)]);
        assert!(toc_hints([(0, 10), (150, 30), (300, 20)].into_iter(), 0, 50).is_empty());
        let st = AreaState {
            slot: scarletbook::Slot::TwoChannel,
            channels: 2,
            start_lsn: 10,
            end_lsn: 50,
            playtime: 400,
            probe_chunk: 4,
            first_chunk: 16,
            close: 8,
            hints: h,
            index: Arc::new(Mutex::new(Index::default())),
        };
        assert_eq!(st.hints_around(0), (Some((0, 10)), Some((150, 25))));
        assert_eq!(st.hints_around(200), (Some((150, 25)), Some((300, 40))));
        assert_eq!(st.hints_around(399), (Some((300, 40)), None));
    }

    #[test]
    fn points_near() {
        let mut i = Index { pending: vec![(100, 5000), (200, 6000)], ..Index::default() };
        i.insert(50, 4000);
        assert!(i.has_point_near(45, 10) && i.has_point_near(60, 10) && !i.has_point_near(61, 10));
        assert!(i.has_point_near(91, 10) && i.has_point_near(205, 10) && !i.has_point_near(150, 10));
        assert!(!i.has_point_near(u32::MAX, 10));
    }

    #[test]
    fn index_detects_contradictions() {
        let mut i = Index::default();
        i.insert(10, 100);
        i.insert(20, 200);
        i.insert(15, 150);
        i.insert(15, 160);
        assert_eq!(i.points.get(&15), Some(&160));
        assert!(!i.irregular);
        i.insert(30, 120);
        assert!(i.irregular);
        assert!(i.points.is_empty());
    }
}
