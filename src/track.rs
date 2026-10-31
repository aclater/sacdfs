// SPDX-License-Identifier: GPL-3.0-or-later

//! A track shown as a DSF or WAV file, and reading byte ranges of it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use scarletbook::toc::{DiscTextItem, TrackTextItem};
use scarletbook::{Area, Track};

use crate::disc::{Disc, ReadError};
use crate::dsf::{self, Layout, Piece, Tags};
use crate::engine::{Engine, Fill, Frame, Stream};
use crate::wav;

/// Frames decoded ahead of a sequential reader: 2 s of audio.
pub const READ_AHEAD_FRAMES: u32 = 150;

/// A read starting this close after the furthest byte read so far still counts as
/// sequential.
const SEQ_GAP_AHEAD: u64 = 1 << 20;

/// A read starting this far before the furthest byte read so far still counts as
/// sequential (the kernel sends read-ahead requests concurrently, slightly out of order).
const SEQ_GAP_BEHIND: u64 = 8 << 20;

/// The file format of a track file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// DSF: the DSD audio as it is.
    Dsf,
    /// WAV: the audio converted to 24-bit PCM.
    Wav,
}

impl Format {
    /// The file name extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Dsf => "dsf",
            Format::Wav => "wav",
        }
    }
}

/// How the audio of a file is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audio {
    /// DSF blocks.
    Dsf(Layout),
    /// WAV frames of PCM.
    Wav(wav::Layout),
}

/// One track of one area, shown as a DSF or WAV file.
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
    /// The track's channel count and number of frames (timeline positions).
    pub layout: Layout,
    /// The first timeline position after the area's last frame: the later of the area's
    /// play time and the end of its last track. A WAV file's neighbouring DSD frames at or
    /// past it are silence.
    pub area_end: u32,
    /// The audio layout of the file.
    pub audio: Audio,
    header: Vec<u8>,
    trailer: Vec<u8>,
    tag: std::ops::Range<usize>,
    /// File size in bytes.
    pub size: u64,
    warned: AtomicBool,
    warned_clips: AtomicBool,
}

impl TrackFile {
    /// The DSF file of `track` in area `area` of `disc`, with its tag.
    pub fn new(disc: Arc<Disc>, area: usize, track: &Track) -> Option<TrackFile> {
        Self::with_format(disc, area, track, Format::Dsf, 0)
    }

    /// The file of `track` in area `area` of `disc` in `format`; a WAV file has
    /// `samples_per_frame` samples per channel per frame.
    pub fn with_format(
        disc: Arc<Disc>,
        area: usize,
        track: &Track,
        format: Format,
        samples_per_frame: u32,
    ) -> Option<TrackFile> {
        let a = disc.toc.areas.get(area)?;
        let layout = Layout { channels: a.channel_count, frames: track.duration_frames };
        let tags = tags(&disc, a, track);
        let (audio, header, trailer, tag) = match format {
            Format::Dsf => {
                let tag = tags.to_id3();
                let header = dsf::header(&layout, tag.len() as u64).to_vec();
                let range = 0..tag.len();
                (Audio::Dsf(layout), header, tag, range)
            }
            Format::Wav => {
                let spf = u64::from(samples_per_frame);
                if spf == 0 || dsf::FRAME_BYTES * 8 % spf != 0 {
                    return None;
                }
                let rate = (u64::from(dsf::SAMPLE_RATE) * spf / (dsf::FRAME_BYTES * 8)) as u32;
                let wl =
                    wav::Layout { channels: layout.channels, frames: layout.frames, samples_per_frame, rate };
                let t = wav::trailer(&tags);
                let header = wav::header(&wl, t.bytes.len() as u64);
                (Audio::Wav(wl), header, t.bytes, t.tag)
            }
        };
        let data_len = match audio {
            Audio::Dsf(l) => l.data_len(),
            Audio::Wav(l) => l.data_len(),
        };
        let size = header.len() as u64 + data_len + trailer.len() as u64;
        let track_end = a.tracks.iter().map(|t| t.end_frame()).max().unwrap_or(0);
        let area_end = a.playtime_frames().max(track_end);
        Some(TrackFile {
            disc,
            area,
            number: track.number,
            start_frame: track.start_frame,
            layout,
            area_end,
            audio,
            header,
            trailer,
            tag,
            size,
            warned: AtomicBool::new(false),
            warned_clips: AtomicBool::new(false),
        })
    }

    /// The file format.
    pub fn format(&self) -> Format {
        match self.audio {
            Audio::Dsf(_) => Format::Dsf,
            Audio::Wav(_) => Format::Wav,
        }
    }

    /// The ID3v2.4 tag bytes.
    pub fn tag(&self) -> &[u8] {
        self.trailer.get(self.tag.clone()).unwrap_or(&[])
    }

    /// Length of everything before the first audio byte.
    pub fn header_len(&self) -> u64 {
        self.header.len() as u64
    }

    /// Offset of the first byte after the audio.
    pub fn data_end(&self) -> u64 {
        self.size - self.trailer.len() as u64
    }

    /// The file indices of the frames that bytes `[offset, end)` of the file need.
    pub fn frames_for(&self, offset: u64, end: u64) -> (u32, u32) {
        let h = self.header_len();
        let s = offset.max(h).min(self.data_end()) - h;
        let e = end.max(h).min(self.data_end()) - h;
        match self.audio {
            Audio::Dsf(l) => l.frames_for(s, e),
            Audio::Wav(l) => l.frames_for(s, e),
        }
    }

    /// Bytes `[offset, offset + len)` of the file, clipped at its end, built from `frames`,
    /// the frames for file indices `first..` (decoded DSD for a DSF file, PCM for a WAV
    /// file).
    pub fn assemble(&self, offset: u64, len: u64, first: u32, frames: &[Arc<Frame>]) -> Vec<u8> {
        let end = offset.saturating_add(len).min(self.size);
        let mut out = Vec::with_capacity(end.saturating_sub(offset) as usize);
        if offset >= end {
            return out;
        }
        let h = self.header_len();
        // Header.
        if offset < h {
            let e = end.min(h);
            out.extend_from_slice(&self.header[offset as usize..e as usize]);
        }
        // Audio.
        let (ds, de) = (offset.max(h), end.min(self.data_end()));
        if ds < de {
            let mut fill = None;
            let mut clips = 0;
            let mut note = |f: &Frame| {
                if f.fill != Fill::Audio {
                    fill = Some(f.fill);
                }
                clips += u64::from(f.clips);
            };
            match self.audio {
                Audio::Dsf(layout) => {
                    for p in layout.pieces(ds - h, de - h) {
                        match p {
                            Piece::Pad { len } => out.resize(out.len() + len, 0),
                            Piece::Audio { channel, byte, len } => {
                                let k = (byte / dsf::FRAME_BYTES) as u32;
                                let at = (byte % dsf::FRAME_BYTES) as usize;
                                match k.checked_sub(first).and_then(|i| frames.get(i as usize)) {
                                    Some(f) => {
                                        note(f);
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
                }
                Audio::Wav(layout) => {
                    let fb = layout.frame_bytes();
                    let mut d = ds - h;
                    while d < de - h {
                        let k = (d / fb) as u32;
                        let at = d % fb;
                        let n = (fb - at).min(de - h - d) as usize;
                        match k.checked_sub(first).and_then(|i| frames.get(i as usize)) {
                            Some(f) => {
                                note(f);
                                match f.data.get(at as usize..at as usize + n) {
                                    Some(src) => out.extend_from_slice(src),
                                    None => out.resize(out.len() + n, 0),
                                }
                            }
                            None => out.resize(out.len() + n, 0),
                        }
                        d += n as u64;
                    }
                }
            }
            self.warn(fill, clips);
        }
        // Trailer.
        if end > self.data_end() {
            let s = (offset.max(self.data_end()) - self.data_end()) as usize;
            let e = (end - self.data_end()) as usize;
            if let Some(t) = self.trailer.get(s..e) {
                out.extend_from_slice(t);
            }
        }
        out
    }

    /// Logs, once per file, that it holds filled frames or clipped samples.
    fn warn(&self, fill: Option<Fill>, clips: u64) {
        if let Some(fill) = fill {
            if !self.warned.swap(true, Ordering::Relaxed) {
                let what = match fill {
                    Fill::Missing => "missing frames (filled with 0x69)",
                    _ => "invalid DST frames (filled with 0x55)",
                };
                log::warn!("{} area {} track {}: {what}", self.disc.path.display(), self.area, self.number);
            }
        }
        if clips > 0 && !self.warned_clips.swap(true, Ordering::Relaxed) {
            log::warn!(
                "{} area {} track {}: {clips} PCM samples clipped (lower --pcm-gain to avoid it)",
                self.disc.path.display(),
                self.area,
                self.number
            );
        }
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

/// An open file: the file and its own demultiplexer state (and, for a WAV file, its last
/// decoded DSD frames), so that a sequential read continues where the last one stopped.
#[derive(Debug)]
pub struct Reader {
    /// The file.
    pub file: Arc<TrackFile>,
    stream: Mutex<Stream>,
    seq: Mutex<Seq>,
}

impl Reader {
    /// A reader of `file`.
    pub fn new(file: Arc<TrackFile>) -> Self {
        Reader { file, stream: Mutex::new(Stream::default()), seq: Mutex::new(Seq::default()) }
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
        match f.audio {
            Audio::Dsf(_) => {
                if let Some(frames) = engine.cached(&f.disc, f.area, t0, t1) {
                    return Ok(frames);
                }
                let mut stream = self.stream.lock().unwrap_or_else(|p| p.into_inner());
                engine.frames(&f.disc, f.area, &mut stream.cursor, t0, t1)
            }
            Audio::Wav(l) => {
                if engine.pcm_frame_bytes(l.channels) as u64 != l.frame_bytes() {
                    return Err(ReadError::Io(std::io::Error::other(
                        "the file's PCM rate differs from the engine's",
                    )));
                }
                if let Some(frames) = engine.cached_pcm(&f.disc, f.area, t0, t1) {
                    return Ok(frames);
                }
                let mut stream = self.stream.lock().unwrap_or_else(|p| p.into_inner());
                engine.pcm_frames(&f.disc, f.area, &mut stream, t0, t1, f.area_end)
            }
        }
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
