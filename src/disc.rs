// SPDX-License-Identifier: GPL-3.0-or-later

//! One disc image: its TOC, the reader of its sectors, and cursors over its audio areas.
//!
//! Seeking is scarletbook's (#28): every area keeps a scarletbook seek index, and a read
//! far into an area starts with a **provisional reading** (`SeekPolicy::Speculative`,
//! specification 4, 4.2 item 8): a search of the frame records' timecodes for a sector
//! whose first frame is numbered below the one wanted, then a reading from there, in reads
//! sized to the range asked for. The positions it gives are exactly a scan's on every
//! image whose frames start in increasing frame number order (any well-formed disc); an
//! area the library has reason to doubt (a TOC that places its audio past the image's end
//! or over the other area, or frame numbers that decrease between the sectors the search
//! read) is read exactly instead (`exact_when_suspect`). The index's seek hints (sectors
//! and the frame number of their first frame record, recorded as cursors read) are kept
//! across mounts by [`crate::index_cache`], so that a seek after a remount near a range
//! read before starts at a hint with one read call.
//!
//! sacdfs adds no SACD knowledge here: it sizes the cursors' ranges, continues them for
//! sequential reads, and reads the image through the cache of open files
//! ([`crate::handles`]).

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;

use scarletbook::audio::{FRAMES_PER_SECTOR, audio_sector_range};
use scarletbook::{
    AreaIndex, FrameContent, Image, IndexOptions, ReadAt, ReadAtReader, SeekPolicy, Slot, Speculation,
    SpeculativeOptions, Toc, TrackCursor,
};

use crate::handles::Stamp;
use crate::index_cache::{AreaShape, ImageKey, IndexCache};

/// Bytes of DSF audio a seek is assumed to need first (a common read size of players and
/// of the kernel for a random read): a located cursor's range covers its frames, so that
/// the library sizes the first read to them.
const FIRST_READ_BYTES: u64 = 128 << 10;

/// Bytes of frames under assembly a seek index stores at its checkpoints (scarletbook's
/// default is 1 MiB): past it, a start from a checkpoint replays from the frame's first
/// sector instead, still exact. *Why less:* every listed disc keeps an index per area, and
/// memory should stay bounded by `--cache-mb` plus a little.
pub const INLINE_BUDGET: usize = 64 << 10;

/// Thresholds of seeking. The defaults suit real discs; tests make every read that does not
/// continue a cursor start a new reading, to exercise seeking on small images.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    /// A cursor this many frames or fewer behind a read's first missing frame is moved
    /// forward rather than replaced by a new one.
    pub reuse_slack: u32,
    /// A range whose exact start (the area's first sector, or a checkpoint of the seek
    /// index) lies at most this many sectors before its estimated position is read exactly;
    /// any other starts with a provisional reading (`SpeculativeOptions::min_gap_sectors`).
    pub min_gap_sectors: u64,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { reuse_slack: 375, min_gap_sectors: 64 }
    }
}

impl Tuning {
    /// Seek as much as possible: every read that does not continue a cursor exactly starts
    /// a new one, provisionally wherever the library allows it.
    pub fn exhaustive() -> Self {
        Tuning { reuse_slack: 0, min_gap_sectors: 0 }
    }
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

/// Calls to images for audio sectors, and their bytes, by every disc of the process.
static TOTAL_READS: AtomicU64 = AtomicU64::new(0);
static TOTAL_BYTES: AtomicU64 = AtomicU64::new(0);

/// Calls to images for audio sectors by every disc of the process so far, and the bytes
/// they read (for diagnostics: sacdfs logs them when it stops).
pub fn total_reads() -> (u64, u64) {
    (TOTAL_READS.load(Ordering::Relaxed), TOTAL_BYTES.load(Ordering::Relaxed))
}

/// The library's disc, read through [`Source`].
pub(crate) type LibDisc = scarletbook::Disc<ReadAtReader<Source>>;

/// The library's image, read through [`Source`]: a cursor reads through a clone of its own.
type LibImage = Image<ReadAtReader<Source>>;

/// What scarletbook reads an image through: each call takes the image's handle from the
/// cache of open files ([`crate::handles`]), opening the file again if the cache closed it,
/// and reads with `pread`. *Why not a file held by the library's disc:* the cache bounds the
/// open files and closes idle ones (#26); a disc kept for every listed image would hold them
/// all open. The library's disc and its clones then keep one image identity for the
/// whole mount, which its cursors and seek index are bound to.
#[derive(Clone, Debug)]
pub struct Source {
    inner: Arc<SourceInner>,
}

#[derive(Debug)]
struct SourceInner {
    /// The image's handle key in [`crate::handles::global`].
    key: u64,
    path: PathBuf,
    /// The image file's length when its TOC was read.
    len: u64,
    /// Whether reads are counted: from the first audio read on, not the TOC's.
    counting: AtomicBool,
    reads: AtomicU64,
    bytes: AtomicU64,
}

impl Source {
    fn new(key: u64, path: &Path, len: u64) -> Source {
        Source {
            inner: Arc::new(SourceInner {
                key,
                path: path.to_owned(),
                len,
                counting: AtomicBool::new(false),
                reads: AtomicU64::new(0),
                bytes: AtomicU64::new(0),
            }),
        }
    }

    /// Opens the image again; refused if its length changed since its TOC was read.
    fn open(&self) -> io::Result<(File, Stamp)> {
        let file = File::open(&self.inner.path)?;
        let meta = file.metadata()?;
        if meta.len() != self.inner.len {
            return Err(io::Error::other("image file changed size since it was mounted"));
        }
        Ok((file, Stamp::of(&meta)))
    }
}

impl ReadAt for Source {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let s = &self.inner;
        if s.counting.load(Ordering::Relaxed) {
            s.reads.fetch_add(1, Ordering::Relaxed);
            s.bytes.fetch_add(buf.len() as u64, Ordering::Relaxed);
            TOTAL_READS.fetch_add(1, Ordering::Relaxed);
            TOTAL_BYTES.fetch_add(buf.len() as u64, Ordering::Relaxed);
        }
        let file = crate::handles::global().get(s.key, &s.path, || self.open())?;
        ReadAt::read_at(&*file, offset, buf)
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.inner.len)
    }
}

/// A disc image with its parsed TOC, and scarletbook's disc of it, which keeps the seek
/// index of each area and makes the cursors.
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
    /// Seeking thresholds.
    pub tuning: Tuning,
    lib: Arc<LibDisc>,
    source: Source,
    /// Cursors that started reading provisionally, and those that read exactly.
    readings: [AtomicU64; 2],
    /// Seek hints loaded from the persistent seek index, per area.
    loaded: Vec<usize>,
}

/// One audio area.
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
    /// The end of the positions a cursor can read: the area's play time, or less when its
    /// sectors cannot hold that many frames (specification 4, 4.8 item 3).
    readable: u32,
    /// Frames of a located cursor's first range ([`FIRST_READ_BYTES`]).
    first_frames: u32,
}

impl Disc {
    /// Opens an image and reads its TOC, with the persistent seek index installed for the
    /// process ([`crate::index_cache::install`]), if any. The file stays open for audio
    /// reads, as long as the cache of open images ([`crate::handles`]) keeps it.
    pub fn open(path: &Path, id: u32, tuning: Tuning) -> Result<Disc, scarletbook::Error> {
        Self::open_with(path, id, tuning, crate::index_cache::global().map(Arc::as_ref))
    }

    /// [`Disc::open`] with the persistent seek index `cache`: the image's saved seek
    /// indexes are loaded from it (checked by the library), and the disc's are saved to it
    /// by [`IndexCache::flush`].
    pub fn open_with(
        path: &Path,
        id: u32,
        tuning: Tuning,
        cache: Option<&IndexCache>,
    ) -> Result<Disc, scarletbook::Error> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        // The TOC is read through the handle that is kept for the audio reads, so that
        // the first audio read does not open the image again (a round trip or two on a
        // network share, #26).
        let handle = crate::handles::next_key();
        crate::handles::global().insert(handle, file, Stamp::of(&meta));
        let source = Source::new(handle, path, meta.len());
        let mut lib = match scarletbook::Disc::open(ReadAtReader::new(source.clone())) {
            Ok(d) => d,
            Err(e) => {
                crate::handles::global().remove(handle);
                return Err(e);
            }
        };
        source.inner.counting.store(true, Ordering::Relaxed);
        for w in lib.warnings() {
            log::debug!("{}: {w}", path.display());
        }
        let toc = lib.toc().clone();
        let areas: Vec<AreaState> = toc
            .areas
            .iter()
            .map(|a| {
                let (start_lsn, end_lsn) = audio_sector_range(a, toc.sectors);
                let capacity = (end_lsn - start_lsn).saturating_mul(FRAMES_PER_SECTOR);
                let per_frame = FRAME_BYTES_PER_CHANNEL * u64::from(a.channel_count.max(1));
                AreaState {
                    slot: a.slot,
                    channels: a.channel_count,
                    start_lsn,
                    end_lsn,
                    readable: a.playtime_frames().min(u32::try_from(capacity).unwrap_or(u32::MAX)),
                    first_frames: (FIRST_READ_BYTES.div_ceil(per_frame) + 2) as u32,
                }
            })
            .collect();
        let shapes: Vec<AreaShape> = areas
            .iter()
            .map(|a| AreaShape { start_lsn: a.start_lsn, end_lsn: a.end_lsn, channels: a.channels })
            .collect();
        let key = cache.and_then(|_| {
            ImageKey::new(path, &meta)
                .inspect_err(|e| log::debug!("{}: no seek index cache: {e}", path.display()))
                .ok()
        });
        let loaded = match (cache, &key) {
            (Some(c), Some(k)) => c.load(k, &shapes),
            _ => None,
        };
        let policy = SeekPolicy::Speculative(
            SpeculativeOptions::new()
                .with_min_gap_sectors(tuning.min_gap_sectors)
                .with_exact_when_suspect(true),
        );
        let mut saved = Vec::with_capacity(areas.len());
        let mut loaded_hints = vec![0; areas.len()];
        for (i, st) in areas.iter().enumerate() {
            let Some(area) = toc.area(st.slot) else {
                saved.push(None);
                continue;
            };
            let bytes = loaded.as_ref().and_then(|l| l.get(i).cloned().flatten());
            let set = bytes.as_ref().is_some_and(|b| {
                AreaIndex::from_bytes(area, toc.sectors, b, INLINE_BUDGET)
                    .and_then(|idx| {
                        let n = idx.hints();
                        lib.set_seek_index(st.slot, idx).map(|()| loaded_hints[i] = n)
                    })
                    .inspect_err(|e| log::debug!("{}: saved seek index not used: {e}", path.display()))
                    .is_ok()
            });
            if !set {
                if let Err(e) =
                    lib.enable_seek_index(st.slot, IndexOptions::new().with_inline_budget(INLINE_BUDGET))
                {
                    log::debug!("{}: no seek index for the {:?} area: {e}", path.display(), st.slot);
                }
            }
            if let Err(e) = lib.set_seek_policy(st.slot, policy) {
                log::debug!("{}: {e}", path.display());
            }
            saved.push(if set { bytes } else { None });
        }
        let hints: usize = loaded_hints.iter().sum();
        if hints > 0 {
            log::debug!("{}: {hints} seek hints loaded", path.display());
        }
        log_unspecified_layouts(path, &toc);
        let lib = Arc::new(lib);
        if let (Some(cache), Some(key)) = (cache, key) {
            cache.register(key, shapes, lib.clone(), areas.iter().map(|a| a.slot).collect(), saved);
        }
        Ok(Disc {
            id,
            path: path.to_owned(),
            mtime,
            toc,
            areas,
            tuning,
            lib,
            source,
            readings: [AtomicU64::new(0), AtomicU64::new(0)],
            loaded: loaded_hints,
        })
    }

    /// A cursor whose next timeline position can be `frame`, for a read of about a
    /// player's first read from there.
    pub fn locate(&self, area: usize, frame: u32) -> Result<Cursor, ReadError> {
        let n = self.area(area)?.first_frames;
        self.locate_for(area, frame, frame.saturating_add(n))
    }

    /// A cursor whose next timeline position can be `frame`, for positions up to `end`
    /// (which sizes its reads; [`Cursor::reserve`] grows it).
    pub fn locate_for(&self, area: usize, frame: u32, end: u32) -> Result<Cursor, ReadError> {
        let st = self.area(area)?;
        let mut c = Cursor {
            area,
            disc_id: self.id,
            channels: st.channels,
            slot: st.slot,
            readable: st.readable,
            window: st.first_frames,
            lib: self.lib.clone(),
            image: self.lib.clone_image(),
            cursor: None,
            next_t: frame,
            end: frame,
            started: false,
        };
        c.reserve(self, end.max(frame.saturating_add(1)))?;
        Ok(c)
    }

    fn area(&self, area: usize) -> Result<&AreaState, ReadError> {
        self.areas.get(area).ok_or(ReadError::Io(io::Error::other("no such area")))
    }

    /// Calls to the image for audio sectors so far.
    pub fn sector_reads(&self) -> u64 {
        self.source.inner.reads.load(Ordering::Relaxed)
    }

    /// Bytes of audio sectors read from the image so far.
    pub fn sector_bytes(&self) -> u64 {
        self.source.inner.bytes.load(Ordering::Relaxed)
    }

    /// The seek hints the area's index holds.
    pub fn seek_hints(&self, area: usize) -> usize {
        self.areas.get(area).and_then(|st| self.lib.seek_index(st.slot)).map_or(0, |i| i.hints())
    }

    /// The seek hints of the area loaded from the persistent seek index.
    pub fn loaded_hints(&self, area: usize) -> usize {
        self.loaded.get(area).copied().unwrap_or(0)
    }

    /// Cursors that started reading provisionally so far, and those that read exactly.
    pub fn readings(&self) -> (u64, u64) {
        (self.readings[0].load(Ordering::Relaxed), self.readings[1].load(Ordering::Relaxed))
    }
}

impl Drop for Disc {
    fn drop(&mut self) {
        crate::handles::global().remove(self.source.inner.key);
    }
}

/// A clone of the library disc's image (no I/O): a reader with a position of its own and
/// the disc's image identity.
trait CloneImage {
    fn clone_image(&self) -> LibImage;
}

impl CloneImage for LibDisc {
    fn clone_image(&self) -> LibImage {
        let mut d = self.clone();
        d.image_mut().clone()
    }
}

/// A forward reader of one area's timeline: a scarletbook range cursor, continued as reads
/// go on.
#[derive(Debug)]
pub struct Cursor {
    /// Which area of the disc.
    pub area: usize,
    disc_id: u32,
    channels: u8,
    slot: Slot,
    /// Positions from here on are missing: see [`AreaState::readable`].
    readable: u32,
    /// The least a range grows by when a read passes its end.
    window: u32,
    lib: Arc<LibDisc>,
    image: LibImage,
    /// The library's cursor of `[next_t, end)`; `None` past `readable`, or after a range
    /// the library refused (its positions are missing).
    cursor: Option<TrackCursor>,
    /// The next timeline position this cursor can return.
    next_t: u32,
    end: u32,
    /// Whether the cursor has read, and its reading been counted.
    started: bool,
}

impl Cursor {
    /// The next timeline position this cursor can return.
    pub fn next_position(&self) -> u32 {
        self.next_t
    }

    /// Whether this cursor reads `area` of `disc`.
    pub fn reads(&self, disc: &Disc, area: usize) -> bool {
        self.disc_id == disc.id && self.area == area
    }

    /// Grows the cursor's range to `end`, so that the library sizes its next reads to the
    /// positions a read needs.
    pub fn reserve(&mut self, disc: &Disc, end: u32) -> Result<(), ReadError> {
        let end = end.min(self.readable);
        if end <= self.end || self.next_t >= self.readable {
            return Ok(());
        }
        let next = match self.cursor.take() {
            Some(mut prev) => {
                self.lib.absorb_seek_checkpoints(&mut prev);
                self.lib.continue_range_frames(prev, self.slot, self.next_t, end)
            }
            None => self.lib.range_frames_cursor(self.slot, self.next_t, end),
        };
        match next {
            Ok(c) => self.cursor = Some(c),
            Err(e) => log::debug!("{}: frames {}..{end}: {e}", disc.path.display(), self.next_t),
        }
        self.end = end;
        Ok(())
    }

    /// The content of timeline position `t`, which must be at or after
    /// [`Cursor::next_position`]. Positions before `t` are skipped.
    pub fn content(&mut self, disc: &Disc, t: u32) -> Result<FrameContent, ReadError> {
        if t < self.next_t {
            return Err(ReadError::Io(io::Error::other("cursor moved backwards")));
        }
        while self.next_t < t {
            self.pull(disc)?;
        }
        self.pull(disc)
    }

    /// The content of position `next_t`.
    fn pull(&mut self, disc: &Disc) -> Result<FrameContent, ReadError> {
        let t = self.next_t;
        if t >= self.readable {
            self.next_t = t.saturating_add(1);
            return Ok(FrameContent::Missing);
        }
        if t >= self.end {
            self.reserve(disc, t.saturating_add(self.window))?;
        }
        let Some(cur) = self.cursor.as_mut() else {
            self.next_t = t + 1;
            return Ok(FrameContent::Missing);
        };
        let got = cur.next_frame(&mut self.image);
        if !self.started {
            self.started = true;
            let exact = !matches!(cur.speculation(), Speculation::Pending { .. });
            disc.readings[usize::from(exact)].fetch_add(1, Ordering::Relaxed);
        }
        self.next_t = t + 1;
        match got {
            Some(Ok(f)) if f.number == t => Ok(f.content),
            Some(Ok(f)) => Err(ReadError::Io(io::Error::other(format!(
                "the library returned position {} for {t}",
                f.number
            )))),
            Some(Err(e)) => {
                self.cursor = None;
                self.end = t;
                Err(e.into())
            }
            None => Ok(FrameContent::Missing),
        }
    }

    /// The channel count of the area.
    pub fn channels(&self) -> u8 {
        self.channels
    }
}

impl Drop for Cursor {
    /// Hands the seek hints and checkpoints this cursor recorded to the disc's index.
    fn drop(&mut self) {
        if let Some(c) = self.cursor.as_mut() {
            self.lib.absorb_seek_checkpoints(c);
        }
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
