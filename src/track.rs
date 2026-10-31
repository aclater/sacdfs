// SPDX-License-Identifier: MIT OR Apache-2.0

//! A track shown as a DSF file, and reading byte ranges of it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use scarletbook::toc::{DiscTextItem, TrackTextItem};
use scarletbook::{Area, Track};

use crate::disc::{Cursor, Disc, ReadError};
use crate::dsf::{self, HEADER_LEN, Layout, Piece, Tags};
use crate::engine::{Engine, Fill, Frame};

/// Frames decoded ahead of a sequential reader: 2 s of audio.
pub const READ_AHEAD_FRAMES: u32 = 150;

/// A read starting this close after the furthest byte read so far still counts as
/// sequential.
const SEQ_GAP_AHEAD: u64 = 1 << 20;

/// A read starting this far before the furthest byte read so far still counts as
/// sequential (the kernel sends read-ahead requests concurrently, slightly out of order).
const SEQ_GAP_BEHIND: u64 = 8 << 20;

/// One track of one area, shown as a DSF file.
#[derive(Debug)]
pub struct TrackFile {
    /// The disc.
    pub disc: Arc<Disc>,
    /// Index of the area in the disc's TOC.
    pub area: usize,
    /// Track number, 1-based.
    pub number: u8,
    /// First timeline position of the track.
    pub start_frame: u32,
    /// The audio layout.
    pub layout: Layout,
    header: [u8; HEADER_LEN as usize],
    tag: Vec<u8>,
    /// File size in bytes.
    pub size: u64,
    warned: AtomicBool,
}

impl TrackFile {
    /// The file of `track` in area `area` of `disc`, with its tag.
    pub fn new(disc: Arc<Disc>, area: usize, track: &Track) -> Option<TrackFile> {
        let a = disc.toc.areas.get(area)?;
        let layout = Layout { channels: a.channel_count, frames: track.duration_frames };
        let tag = tags(&disc, a, track).to_id3();
        let header = dsf::header(&layout, tag.len() as u64);
        let size = HEADER_LEN + layout.data_len() + tag.len() as u64;
        Some(TrackFile {
            disc,
            area,
            number: track.number,
            start_frame: track.start_frame,
            layout,
            header,
            tag,
            size,
            warned: AtomicBool::new(false),
        })
    }

    /// The ID3v2.4 tag bytes.
    pub fn tag(&self) -> &[u8] {
        &self.tag
    }

    fn data_end(&self) -> u64 {
        HEADER_LEN + self.layout.data_len()
    }

    /// The file indices of the frames that bytes `[offset, end)` of the file need.
    pub fn frames_for(&self, offset: u64, end: u64) -> (u32, u32) {
        let s = offset.max(HEADER_LEN).min(self.data_end()) - HEADER_LEN;
        let e = end.max(HEADER_LEN).min(self.data_end()) - HEADER_LEN;
        self.layout.frames_for(s, e)
    }

    /// Bytes `[offset, offset + len)` of the file, clipped at its end, built from `frames`,
    /// the decoded frames for file indices `first..`.
    pub fn assemble(&self, offset: u64, len: u64, first: u32, frames: &[Arc<Frame>]) -> Vec<u8> {
        let end = offset.saturating_add(len).min(self.size);
        let mut out = Vec::with_capacity(end.saturating_sub(offset) as usize);
        if offset >= end {
            return out;
        }
        // Header.
        if offset < HEADER_LEN {
            let e = end.min(HEADER_LEN);
            out.extend_from_slice(&self.header[offset as usize..e as usize]);
        }
        // Audio.
        let (ds, de) = (offset.max(HEADER_LEN), end.min(self.data_end()));
        if ds < de {
            let mut fill = None;
            for p in self.layout.pieces(ds - HEADER_LEN, de - HEADER_LEN) {
                match p {
                    Piece::Pad { len } => out.resize(out.len() + len, 0),
                    Piece::Audio { channel, byte, len } => {
                        let k = (byte / dsf::FRAME_BYTES) as u32;
                        let at = (byte % dsf::FRAME_BYTES) as usize;
                        match k.checked_sub(first).and_then(|i| frames.get(i as usize)) {
                            Some(f) => {
                                if f.fill != Fill::Audio {
                                    fill = Some(f.fill);
                                }
                                match f.plane(channel).get(at..at + len) {
                                    Some(src) => out.extend_from_slice(src),
                                    None => out.resize(out.len() + len, 0),
                                }
                            }
                            None => out.resize(out.len() + len, 0),
                        }
                    }
                }
            }
            if let Some(fill) = fill {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    let what = match fill {
                        Fill::Missing => "missing frames (filled with 0x69)",
                        _ => "invalid DST frames (filled with 0x55)",
                    };
                    log::warn!(
                        "{} area {} track {}: {what}",
                        self.disc.path.display(),
                        self.area,
                        self.number
                    );
                }
            }
        }
        // Tag.
        if end > self.data_end() {
            let s = (offset.max(self.data_end()) - self.data_end()) as usize;
            let e = (end - self.data_end()) as usize;
            if let Some(t) = self.tag.get(s..e) {
                out.extend_from_slice(t);
            }
        }
        out
    }
}

/// The tag of a track, from the disc and track text.
fn tags(disc: &Disc, area: &Area, track: &Track) -> Tags {
    let m = &disc.toc.master;
    let album = |a: DiscTextItem, b: DiscTextItem| m.text.get(a).or_else(|| m.text.get(b));
    let album_artist = album(DiscTextItem::AlbumArtist, DiscTextItem::DiscArtist);
    let mut t = Tags::default();
    t.set(b"TIT2", track.text.get(TrackTextItem::Title));
    t.set(b"TPE1", track.text.get(TrackTextItem::Performer).or(album_artist));
    t.set(b"TPE2", album_artist);
    t.set(b"TALB", album(DiscTextItem::AlbumTitle, DiscTextItem::DiscTitle));
    t.set(b"TRCK", Some(&format!("{}/{}", track.number, area.tracks.len())));
    if m.album_set_size > 1 {
        t.set(b"TPOS", Some(&format!("{}/{}", m.album_sequence_number, m.album_set_size)));
    }
    t.set(b"TCOM", track.text.get(TrackTextItem::Composer));
    t.set(b"TDRC", date(m.date.year, m.date.month, m.date.day).as_deref());
    t.set(b"TSRC", track.isrc.as_deref());
    let genre = track.genre.and_then(|g| g.name()).or_else(|| m.album_genres.iter().find_map(|g| g.name()));
    t.set(b"TCON", genre.filter(|g| !g.starts_with("not ")));
    t.set(b"TPUB", album(DiscTextItem::AlbumPublisher, DiscTextItem::DiscPublisher));
    t.set(b"TCOP", album(DiscTextItem::AlbumCopyright, DiscTextItem::DiscCopyright));
    t
}

/// An ID3v2.4 timestamp from a stored date: `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, as far as
/// the fields are set and plausible.
fn date(year: u16, month: u8, day: u8) -> Option<String> {
    if year == 0 || year > 9999 {
        return None;
    }
    if !(1..=12).contains(&month) {
        return Some(format!("{year:04}"));
    }
    if !(1..=31).contains(&day) {
        return Some(format!("{year:04}-{month:02}"));
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// Frames of a file (indices `[first, end)`) to decode ahead of a sequential reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadAhead {
    /// First frame.
    pub first: u32,
    /// End frame (exclusive).
    pub end: u32,
}

#[derive(Debug, Default)]
struct Seq {
    /// The furthest byte read so far.
    max_end: u64,
    /// Frames up to here (file index, exclusive) are decoded or being decoded ahead.
    ahead_until: u32,
}

/// An open file: the file and its own demultiplexer state, so that a sequential read
/// continues where the last one stopped.
#[derive(Debug)]
pub struct Reader {
    /// The file.
    pub file: Arc<TrackFile>,
    cursor: Mutex<Option<Cursor>>,
    seq: Mutex<Seq>,
}

impl Reader {
    /// A reader of `file`.
    pub fn new(file: Arc<TrackFile>) -> Self {
        Reader { file, cursor: Mutex::new(None), seq: Mutex::new(Seq::default()) }
    }

    /// Bytes `[offset, offset + len)` of the file (fewer at its end), and the frames (file
    /// indices) to decode ahead, if the reads look sequential.
    pub fn read(
        &self,
        engine: &Engine,
        offset: u64,
        len: u64,
    ) -> Result<(Vec<u8>, Option<ReadAhead>), ReadError> {
        let f = &self.file;
        let end = offset.saturating_add(len).min(f.size);
        let (k0, k1) = f.frames_for(offset, end);
        let frames = if k0 < k1 { self.decoded(engine, k0, k1)? } else { Vec::new() };
        let data = f.assemble(offset, len, k0, &frames);
        Ok((data, self.plan_read_ahead(offset, end, k1)))
    }

    /// Decodes the frames of `ahead` into the cache, if they are not there already.
    pub fn read_ahead(&self, engine: &Engine, ahead: ReadAhead) {
        if let Err(e) = self.decoded(engine, ahead.first, ahead.end) {
            log::debug!("read-ahead of {}: {e}", self.file.disc.path.display());
        }
    }

    fn decoded(&self, engine: &Engine, k0: u32, k1: u32) -> Result<Vec<Arc<Frame>>, ReadError> {
        let f = &self.file;
        let (t0, t1) = (f.start_frame + k0, f.start_frame + k1);
        if let Some(frames) = engine.cached(&f.disc, f.area, t0, t1) {
            return Ok(frames);
        }
        let mut cursor = self.cursor.lock().unwrap_or_else(|p| p.into_inner());
        engine.frames(&f.disc, f.area, &mut cursor, t0, t1)
    }

    fn plan_read_ahead(&self, offset: u64, end: u64, k1: u32) -> Option<ReadAhead> {
        let mut s = self.seq.lock().unwrap_or_else(|p| p.into_inner());
        let sequential = offset == 0
            || (offset <= s.max_end.saturating_add(SEQ_GAP_AHEAD)
                && offset.saturating_add(SEQ_GAP_BEHIND) >= s.max_end);
        s.max_end = s.max_end.max(end);
        if !sequential {
            s.ahead_until = 0;
            return None;
        }
        let frames = self.file.layout.frames;
        if k1 >= frames || s.ahead_until >= frames.min(k1 + READ_AHEAD_FRAMES / 2) {
            return None;
        }
        let start = s.ahead_until.max(k1);
        let stop = frames.min(k1 + READ_AHEAD_FRAMES);
        s.ahead_until = stop;
        (start < stop).then_some(ReadAhead { first: start, end: stop })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn dates() {
        assert_eq!(date(0, 1, 1), None);
        assert_eq!(date(2003, 0, 0).as_deref(), Some("2003"));
        assert_eq!(date(2003, 13, 1).as_deref(), Some("2003"));
        assert_eq!(date(2003, 4, 0).as_deref(), Some("2003-04"));
        assert_eq!(date(2003, 4, 9).as_deref(), Some("2003-04-09"));
        assert_eq!(date(65535, 4, 9), None);
    }
}
