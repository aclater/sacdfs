// SPDX-License-Identifier: MIT OR Apache-2.0

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

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use scarletbook::audio::audio_sector_range;
use scarletbook::{AreaFrame, Demuxer, FrameContent, FrameKind, Image, SECTOR_SIZE, Slot, Toc};

/// Sectors read per call to the image by a sequential scan.
const SCAN_CHUNK: u64 = 64;

/// Sectors read per call by a probe of the binary search.
const PROBE_CHUNK: u64 = 16;

/// Most sectors a probe reads before giving up on finding a frame: two of the largest
/// frames (65 536 bytes each) and a little more.
const PROBE_LIMIT: u64 = 80;

/// Thresholds of the frame search. The defaults suit real discs; tests make every read
/// search, to exercise the search on small images.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    /// A search stops when the interval left is this many sectors or fewer, and scans it.
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

/// Learn a point from a sequential scan at most once per this many frames.
const LEARN_EVERY: u32 = 75;

/// Most points kept per area.
const MAX_POINTS: usize = 1 << 16;

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

/// A disc image with its parsed TOC. The image file is opened on first audio read.
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
    image: Mutex<Option<Image<File>>>,
    /// Calls to the image for audio sectors so far.
    reads: AtomicU64,
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
    /// (start frame, start LSN) of the tracks in the area TOC, increasing in both, as
    /// hints for the search; empty if the TOC's track starts are not in order.
    hints: Vec<Point>,
    index: Mutex<Index>,
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
struct Index {
    /// frame number -> an LSN from which a fresh demultiplexer first yields a frame
    /// numbered at most this one, and that frame starts at or after the LSN.
    points: BTreeMap<u32, u64>,
    irregular: bool,
}

impl Index {
    /// Records a point; marks the area irregular if it contradicts the others.
    fn insert(&mut self, frame: u32, lsn: u64) {
        if self.irregular {
            return;
        }
        let below = self.points.range(..frame).next_back().map(|(_, &l)| l);
        let above = self.points.range(frame + 1..).next().map(|(_, &l)| l);
        if below.is_some_and(|l| l > lsn) || above.is_some_and(|l| l < lsn) {
            log::debug!(
                "frame {frame} at LSN {lsn} contradicts the frame index: scanning this area from its start"
            );
            self.irregular = true;
            self.points.clear();
            return;
        }
        if self.points.len() >= MAX_POINTS && !self.points.contains_key(&frame) {
            return;
        }
        let e = self.points.entry(frame).or_insert(lsn);
        *e = (*e).max(lsn);
    }
}

impl Disc {
    /// Opens an image and reads its TOC. The file is closed again until audio is read.
    pub fn open(path: &Path, id: u32, tuning: Tuning) -> Result<Disc, scarletbook::Error> {
        let file = File::open(path)?;
        let mtime = file.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
        let disc = scarletbook::Disc::open(io::BufReader::new(file))?;
        for w in disc.warnings() {
            log::debug!("{}: {w}", path.display());
        }
        let toc = disc.toc().clone();
        let areas = toc
            .areas
            .iter()
            .map(|a| {
                let (start_lsn, end_lsn) = audio_sector_range(a, toc.sectors);
                AreaState {
                    slot: a.slot,
                    channels: a.channel_count,
                    start_lsn,
                    end_lsn,
                    playtime: a.playtime_frames(),
                    hints: toc_hints(
                        a.tracks.iter().map(|t| (t.start_frame, u64::from(t.start_lsn))),
                        start_lsn,
                        end_lsn,
                    ),
                    index: Mutex::new(Index::default()),
                }
            })
            .collect();
        Ok(Disc {
            id,
            path: path.to_owned(),
            mtime,
            toc,
            areas,
            tuning,
            image: Mutex::new(None),
            reads: AtomicU64::new(0),
        })
    }

    /// Reads `buf.len() / 2048` sectors at `lsn`. `Ok(false)` if they are not all in the
    /// image.
    pub fn read_sectors(&self, lsn: u64, buf: &mut [u8]) -> Result<bool, ReadError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.image.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            let file = File::open(&self.path)?;
            let image = Image::with_format(file, self.toc.sector_format)?;
            if image.sector_count() != self.toc.sectors {
                return Err(ReadError::Io(io::Error::other("image file changed size since it was mounted")));
            }
            *guard = Some(image);
        }
        match guard.as_mut() {
            Some(image) => Ok(image.read_sectors(lsn, buf)?),
            None => Err(ReadError::Io(io::Error::other("image not open"))),
        }
    }

    /// A cursor positioned so that its next timeline position can be `frame`, found with
    /// the area's index and, if needed, a search.
    ///
    /// The cursor starts where a fresh demultiplexer first yields a frame numbered *below*
    /// `frame`, never at a frame numbered `frame`: a frame that repeats an earlier frame's
    /// number (a hostile timecode) is discarded by a scan that has seen the original, but
    /// a demultiplexer started between the two would take the copy for the original.
    pub fn locate(&self, area: usize, frame: u32) -> Result<Cursor, ReadError> {
        let st = self.area(area)?;
        let (mut lo, mut lo_frame, mut hi, mut hi_frame) = {
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
            // Without points, the area starts near frame 0 and ends near its play time.
            let (lo, lo_frame) = below.map_or((st.start_lsn, Some(0)), |(f, l)| (l, Some(f)));
            let (hi, hi_frame) =
                above.map_or((st.end_lsn, Some(st.playtime)), |(f, l)| ((l + 1).min(st.end_lsn), Some(f)));
            (lo, lo_frame, hi, hi_frame)
        };
        // Invariant: starting at `lo` finds `frame` (it is the area start, or a point whose
        // first frame comes before `frame`). Frames at `hi` or later are `frame` or later.
        let (hint_lo, hint_hi) = if self.tuning.toc_hints { st.hints_around(frame) } else { (None, None) };
        let mut bisect_next = false;
        while hi > lo && hi - lo > self.tuning.search_close {
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
                    let margin = self.tuning.search_close / 2;
                    let est = if est - lo < hi - est { est + margin } else { est.saturating_sub(margin) };
                    est.clamp(lo + 1, hi - 1)
                }
                _ => lo + (hi - lo) / 2,
            };
            let before = hi - lo;
            match self.probe(area, mid)? {
                Some(n) if n < frame => {
                    lo = mid;
                    lo_frame = Some(n);
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
            // Alternate to bisection when interpolation did not halve the interval.
            bisect_next = !bisect_next && (hi - lo) * 2 > before;
        }
        Ok(Cursor::new(self, area, lo, frame))
    }

    /// The number of the first frame a fresh demultiplexer yields from `lsn`, if it finds
    /// one within [`PROBE_LIMIT`] sectors. Records it in the index.
    fn probe(&self, area: usize, lsn: u64) -> Result<Option<u32>, ReadError> {
        let st = self.area(area)?;
        let mut demux = Demuxer::new(st.channels);
        let mut buf = vec![0u8; (PROBE_CHUNK as usize) * SECTOR_SIZE];
        let mut at = lsn;
        let limit = (lsn + PROBE_LIMIT).min(st.end_lsn);
        while at < limit {
            let n = PROBE_CHUNK.min(limit - at);
            let chunk = &mut buf[..n as usize * SECTOR_SIZE];
            if !self.read_sectors(at, chunk)? {
                return Err(ReadError::Io(io::Error::other("audio sectors outside the image")));
            }
            for sector in chunk.chunks_exact(SECTOR_SIZE) {
                if let Ok(s) = <&[u8; SECTOR_SIZE]>::try_from(sector) {
                    demux.push_sector(s);
                }
                if let Some(f) = demux.pop() {
                    st.index.lock().unwrap_or_else(|p| p.into_inner()).insert(f.number, lsn);
                    return Ok(Some(f.number));
                }
            }
            at += n;
        }
        if limit == st.end_lsn {
            demux.finish();
            if let Some(f) = demux.pop() {
                st.index.lock().unwrap_or_else(|p| p.into_inner()).insert(f.number, lsn);
                return Ok(Some(f.number));
            }
        }
        Ok(None)
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

    /// The number of index points of an area (for tests and diagnostics).
    pub fn index_points(&self, area: usize) -> usize {
        self.areas.get(area).map_or(0, |st| st.index.lock().unwrap_or_else(|p| p.into_inner()).points.len())
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
}

impl Cursor {
    fn new(disc: &Disc, area: usize, lsn: u64, next_t: u32) -> Cursor {
        let (channels, end_lsn) = disc.areas.get(area).map_or((0, lsn), |st| (st.channels, st.end_lsn));
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
        }
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

    /// The next accepted frame of the area, `None` at its end.
    fn pull(&mut self, disc: &Disc) -> Result<Option<AreaFrame>, ReadError> {
        loop {
            if let Some(f) = self.ready.pop_front() {
                return Ok(Some(f));
            }
            if self.finished {
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
                let n = SCAN_CHUNK.min(self.end_lsn - lsn);
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
            st.index.lock().unwrap_or_else(|p| p.into_inner()).insert(frame, lsn);
        }
    }

    /// The channel count of the area.
    pub fn channels(&self) -> u8 {
        self.channels
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
            hints: h,
            index: Mutex::new(Index::default()),
        };
        assert_eq!(st.hints_around(0), (Some((0, 10)), Some((150, 25))));
        assert_eq!(st.hints_around(200), (Some((150, 25)), Some((300, 40))));
        assert_eq!(st.hints_around(399), (Some((300, 40)), None));
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
