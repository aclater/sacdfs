// SPDX-License-Identifier: GPL-3.0-or-later

//! A track shown as a DSF or WAV file, and reading byte ranges of it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use scarletbook::Track;

use crate::disc::{Disc, ReadError};
use crate::dsf::{self, Layout};
use crate::engine::{Engine, Fill, Frame, Stream};
use crate::wav;

/// Frames decoded ahead of a sequential reader of a DSF file, and the fewest for a WAV file:
/// 2 s of audio.
pub const READ_AHEAD_FRAMES: u32 = 150;

/// PCM converted ahead of a sequential reader of a WAV file, in bytes, within
/// [`READ_AHEAD_FRAMES`] and [`READ_AHEAD_FRAMES_MAX`] frames.
pub const READ_AHEAD_PCM_BYTES: u64 = 8 << 20;

/// Most frames converted ahead of a WAV reader: 8 s of audio.
pub const READ_AHEAD_FRAMES_MAX: u32 = 600;

/// Frames decoded ahead of a fast reader of a DSF file, per decoder thread (#210).
pub const FAST_FRAMES_PER_THREAD: u32 = 32;

/// Most frames decoded ahead of a fast reader of a DSF file.
pub const FAST_FRAMES_MAX: u32 = 1200;

/// A reader gets the long read-ahead window only after reading this much...
const FAST_AFTER: u64 = 4 << 20;

/// ...at more than this many times the file's byte rate.
const FAST_FACTOR: f64 = 4.0;

/// A reader that has averaged more than this many times the file's byte rate is not playing
/// it. Below [`FAST_FACTOR`], because on a small machine a scanner sharing the decoder threads
/// with others often gets little more than this.
const PLAYER_FACTOR: f64 = 1.5;

/// While someone plays, a reader that is not playing reads ahead at least this many frames...
const SHARED_WINDOW_MIN: u32 = 16;

/// ...and in slices of this many frames per decoder thread, at least [`SHARED_WINDOW_MIN`]:
/// about 20 ms of work per slice, whatever the number of threads.
const SLICE_FRAMES_PER_THREAD: u32 = 8;

/// The longest that a reader not playing waits for players, before each slice. *Why bounded:*
/// a player may be waiting for a stalled source, and enough players could always be busy.
const YIELD_MAX: std::time::Duration = std::time::Duration::from_millis(50);

/// A fast reader not heard from for this long no longer counts ([`FastReaders`]): one that
/// paused needs no share of the cache, and it gets one back at its next read.
const FAST_READER_IDLE: std::time::Duration = std::time::Duration::from_secs(1);

/// The fast readers at the moment, which share what the cache can hold ahead of them.
/// Asked once per read of a fast reader; the list is as long as the readers.
#[derive(Debug, Default)]
pub struct FastReaders {
    seen: Mutex<Vec<(u64, std::time::Instant)>>,
}

impl FastReaders {
    /// Notes that reader `id` is fast now, and says how many are.
    fn note(&self, id: u64, now: std::time::Instant) -> usize {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.retain(|(i, t)| *i != id && now.saturating_duration_since(*t) < FAST_READER_IDLE);
        seen.push((id, now));
        seen.len()
    }

    /// Reader `id` closed. *Why not left to expire:* a `cp` opens the next file at once, and
    /// would count as several readers, with a short window, for its whole run.
    fn forget(&self, id: u64) {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).retain(|(i, _)| *i != id);
    }

    /// Whether any reader has been heard from lately.
    fn any(&self, now: std::time::Instant) -> bool {
        let seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.iter().any(|(_, t)| now.saturating_duration_since(*t) < FAST_READER_IDLE)
    }
}

/// Whether `read` bytes in `secs` of a file of `byte_rate` bytes per second is more than a player
/// reads ([`Traffic`]).
fn faster_than_a_player(read: u64, secs: f64, byte_rate: f64) -> bool {
    read >= FAST_AFTER && read as f64 > PLAYER_FACTOR * byte_rate * secs
}

/// Who is reading, so that a whole-file reader doesn't starve someone playing (#230).
///
/// A reader averaging at most [`PLAYER_FACTOR`] times the file's byte rate is a *player*; one
/// averaging more, past [`FAST_AFTER`] bytes, reads for *throughput*. Throughput readers do three
/// things differently:
///
/// 1. **They give up the frames they have passed** ([`Engine::give_up`]), always. *Why this
///    is the fix:* the cache is least recently used, a player's read-ahead waits there for
///    1 to 2 s, and a whole-file reader at 30 MB/s replaces 32 MiB in 1 s. The player's frames were
///    evicted before it got to them; it converted them twice and then read 4 frames at a
///    time, each behind the scanner's whole window on the decoder threads: 10 s behind in 30 s.
///    That is why a cache of 8 MiB helped, oddly: no read-ahead survived for anyone, so
///    every reader's work came in small pieces and they took turns.
/// 2. **They share a quarter of the cache**, while someone plays, however few frames that is.
///    Alone, they keep their full window.
/// 3. **They read ahead in slices and wait for players between them**, while someone plays,
///    so that a player's work waits for a slice, not for a window of frames.
///
/// A scanner alone does only the first, which costs it nothing: frames behind it are of no use.
#[derive(Debug, Default)]
pub struct Traffic {
    /// Readers for throughput. Their read-ahead windows share a quarter of the cache.
    fast: FastReaders,
    /// Players.
    players: FastReaders,
    /// Players' requests decoding now.
    decoding: Mutex<usize>,
    done: Condvar,
}

impl Traffic {
    /// Waits, for [`YIELD_MAX`] at most, until no player is decoding.
    fn yield_to_players(&self) {
        let g = self.decoding.lock().unwrap_or_else(|p| p.into_inner());
        drop(self.done.wait_timeout_while(g, YIELD_MAX, |n| *n > 0).unwrap_or_else(|p| p.into_inner()));
    }

    /// Marks a player's decoding until the guard drops.
    fn decoding(&self) -> Decoding<'_> {
        *self.decoding.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        Decoding(self)
    }
}

/// See [`Traffic::decoding`].
struct Decoding<'a>(&'a Traffic);

impl Drop for Decoding<'_> {
    fn drop(&mut self) {
        let mut n = self.0.decoding.lock().unwrap_or_else(|p| p.into_inner());
        *n -= 1;
        if *n == 0 {
            self.0.done.notify_all();
        }
    }
}

/// The next [`Reader`]'s number, for [`FastReaders`].
static NEXT_READER: AtomicU64 = AtomicU64::new(0);

/// A read starting this close after the furthest byte read so far still counts as
/// sequential.
const SEQ_GAP_AHEAD: u64 = 1 << 20;

/// A read starting this far before the furthest byte read so far still counts as
/// sequential (the kernel sends read-ahead requests concurrently, slightly out of order).
const SEQ_GAP_BEHIND: u64 = 8 << 20;

/// Reads that end within this many bytes of where a file's first read of audio started
/// start no read-ahead: that is the kernel's own read-ahead of a first read (128 KiB, then
/// 128 KiB more in the background, whatever `max_readahead` says), which a tagger reading a
/// DSF file's header causes as much as a player does (#32).
pub const KERNEL_FIRST_WINDOW: u64 = 256 << 10;

/// Reads that end within this many bytes of where a file's first read of audio started start
/// no read-ahead either (#78): a library scanner reads a file's head and leaves. Roon's import
/// reads the first 768 KiB, then 36 KiB at the middle and the tag; read-ahead after that head
/// decoded 2 s of audio nobody read, three times the head's own decoding for a 6-channel DST
/// file, and held the decoder threads while the scanner went on to the next file.
pub const HEAD_WINDOW: u64 = 1 << 20;

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

/// The range of `track`'s track with pause, if it is a valid range longer than the track.
fn pause_range(area: &scarletbook::Area, track: &Track) -> Option<(u32, u32)> {
    let (first, end) = scarletbook::audio::track_range_with_pause(area, u32::from(track.number))?;
    (first <= track.start_frame && end >= track.end_frame() && end > first).then_some((first, end))
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
    /// A DSF file's layout, scarletbook's (`output::Layout`): its header, its tag and where
    /// each audio byte comes from.
    dsf: Option<scarletbook::output::Layout>,
    header: wav::Header,
    trailer: Vec<u8>,
    tag: std::ops::Range<usize>,
    /// File size in bytes.
    pub size: u64,
    /// What a tags file changed in the file's tag (#81), if anything.
    pub tags: Option<crate::tags::Stamp>,
    /// Whether the file's tag or name has text the disc lacks (#185).
    pub filled: bool,
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
        Self::with_pauses(disc, area, track, format, samples_per_frame, false)
    }

    /// Like [`TrackFile::with_format`]; with `pauses` set, the file is the track with pause
    /// (`--pauses kept`, #112): from frame 0 (track one) or the track's own start, to the
    /// start of the next track (the area's play time for the last), as the
    /// extractors that keep the pauses write it. A track whose range is not valid is served
    /// without its pause.
    pub fn with_pauses(
        disc: Arc<Disc>,
        area: usize,
        track: &Track,
        format: Format,
        samples_per_frame: u32,
        pauses: bool,
    ) -> Option<TrackFile> {
        let a = disc.toc().areas.get(area)?;
        let kept;
        let track = match pauses.then(|| pause_range(a, track)).flatten() {
            Some((first, end)) => {
                let mut t = track.clone();
                t.start_frame = first;
                t.duration_frames = end - first;
                kept = t;
                &kept
            }
            None => track,
        };
        let layout = Layout { channels: a.channel_count, frames: track.duration_frames };
        let mut lib = None;
        let tagged = dsf::tagged(disc.toc(), a, track, Some(&disc.path));
        let stamp = tagged.as_ref().and_then(|t| t.stamp);
        let mut filled = tagged.as_ref().is_some_and(|t| t.filled);
        let (audio, header, trailer, tag) = match format {
            Format::Dsf => {
                let l = tagged?.layout;
                let header =
                    wav::Header::from_bytes(dsf::region(&l, scarletbook::output::RegionKind::Header));
                let tag = dsf::region(&l, scarletbook::output::RegionKind::Trailer);
                let range = 0..tag.len();
                lib = Some(l);
                (Audio::Dsf(layout), header, tag, range)
            }
            Format::Wav => {
                // The DSF file's tag as the `id3 ` chunk, and the INFO chunk from its items.
                let (set, id3) = match tagged {
                    Some(t) => {
                        let id3 = dsf::region(&t.layout, scarletbook::output::RegionKind::Trailer);
                        (t.set, id3)
                    }
                    None => {
                        let options = dsf::options(a, track);
                        let (set, f) = dsf::tag_set(disc.toc(), a, track, &options, Some(&disc.path));
                        filled = f;
                        (set, Vec::new())
                    }
                };
                let spf = u64::from(samples_per_frame);
                if spf == 0 || dsf::FRAME_BYTES * 8 % spf != 0 {
                    return None;
                }
                let rate = (u64::from(dsf::SAMPLE_RATE) * spf / (dsf::FRAME_BYTES * 8)) as u32;
                let wl =
                    wav::Layout { channels: layout.channels, frames: layout.frames, samples_per_frame, rate };
                let t = wav::trailer(&set, &id3);
                let header = wav::header_parts(&wl, a.loudspeaker_layout(), t.bytes.len() as u64);
                (Audio::Wav(wl), header, t.bytes, t.tag)
            }
        };
        let size = match (&lib, audio) {
            (Some(l), _) => l.size(),
            (None, Audio::Wav(l)) => header.len() + l.data_len() + trailer.len() as u64,
            (None, Audio::Dsf(_)) => return None,
        };
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
            dsf: lib,
            header,
            trailer,
            tag,
            size,
            tags: stamp,
            filled,
            warned: AtomicBool::new(false),
            warned_clips: AtomicBool::new(false),
        })
    }

    /// Frames read ahead of a fast sequential reader. For a WAV file, about 8 MiB of PCM: a
    /// longer window than DSF's keeps the pipeline of demultiplexing and conversion busy for
    /// a fast reader (`cp`) of a plain DSD area, where conversion is the only cost.
    ///
    /// For a DSF file, [`FAST_FRAMES_PER_THREAD`] per decoder thread, at most
    /// [`FAST_FRAMES_MAX`] and a share of a quarter of the cache, at least
    /// [`READ_AHEAD_FRAMES`]. `fast_readers` are the fast readers at the moment, this one
    /// included.
    /// *Why (#210):* the decoder threads wait for the slowest frame at the end of every
    /// batch, so that the wait's share of the time is about the threads over the window.
    /// *Why a share of a quarter of the cache:* frames evicted before they are read are
    /// decoded again. A quarter for each reader let four readers fill the cache between them:
    /// on 32 threads they read 62 to 70 % slower than with the 2 s window.
    pub fn read_ahead_frames(&self, threads: usize, cache_bytes: usize, fast_readers: usize) -> u32 {
        match self.audio {
            Audio::Dsf(l) => {
                let frame = (dsf::FRAME_BYTES * u64::from(l.channels)).max(1);
                let by_threads = (threads as u64).saturating_mul(u64::from(FAST_FRAMES_PER_THREAD));
                by_threads
                    .min(u64::from(FAST_FRAMES_MAX))
                    .min(cache_bytes as u64 / 4 / fast_readers.max(1) as u64 / frame)
                    .max(u64::from(READ_AHEAD_FRAMES)) as u32
            }
            Audio::Wav(l) => (READ_AHEAD_PCM_BYTES / l.frame_bytes().max(1))
                .clamp(u64::from(READ_AHEAD_FRAMES), u64::from(READ_AHEAD_FRAMES_MAX))
                as u32,
        }
    }

    /// The share of a quarter of the cache, in frames, of one of `readers` reading for
    /// throughput while someone plays ([`Traffic`]).
    pub fn shared_frames(&self, cache_bytes: usize, readers: usize) -> u32 {
        let frame = self.byte_rate() as u64 / 75;
        let share = cache_bytes as u64 / 4 / readers.max(1) as u64 / frame.max(1);
        share.clamp(u64::from(SHARED_WINDOW_MIN), u64::from(READ_AHEAD_FRAMES_MAX)) as u32
    }

    /// Bytes of the file's audio per second of playing.
    pub fn byte_rate(&self) -> f64 {
        let per_frame = match self.audio {
            Audio::Dsf(l) => dsf::FRAME_BYTES * u64::from(l.channels),
            Audio::Wav(l) => l.frame_bytes(),
        };
        per_frame as f64 * 75.0
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

    /// The frames an extract's tag is compared with (`verify`, `tags import`): the file's tag,
    /// without what came from the image's path ([`dsf::stored_tag`]).
    pub fn stored_frames(&self) -> Vec<crate::verify::id3::Frame> {
        if !self.filled {
            return crate::verify::id3::frames(self.tag());
        }
        let toc = self.disc.toc();
        let tag = toc.areas.get(self.area).and_then(|a| {
            let track = a.tracks.iter().find(|t| t.number == self.number)?;
            Some(dsf::stored_tag(toc, a, track))
        });
        crate::verify::id3::frames(&tag.unwrap_or_default())
    }

    /// Length of everything before the first audio byte.
    pub fn header_len(&self) -> u64 {
        self.header.len()
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
        match (&self.dsf, self.audio) {
            (Some(l), _) => {
                let r = l.positions_for(offset..end);
                (r.start.saturating_sub(self.start_frame), r.end.saturating_sub(self.start_frame))
            }
            (None, Audio::Wav(l)) => l.frames_for(s, e),
            (None, Audio::Dsf(_)) => (0, 0),
        }
    }

    /// Bytes `[offset, offset + len)` of the file, clipped at its end, built from `frames`,
    /// the frames for file indices `first..` (decoded DSD for a DSF file, PCM for a WAV
    /// file).
    pub fn assemble(&self, offset: u64, len: u64, first: u32, frames: &[Arc<Frame>]) -> Vec<u8> {
        let end = offset.saturating_add(len).min(self.size);
        if let Some(l) = &self.dsf {
            return self.assemble_dsf(l, offset, end, first, frames);
        }
        let mut out = Vec::with_capacity(end.saturating_sub(offset) as usize);
        if offset >= end {
            return out;
        }
        let h = self.header_len();
        // Header.
        if offset < h {
            let e = end.min(h);
            self.header.extend_into(offset, e, &mut out);
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
                Audio::Dsf(_) => {}
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

    /// Bytes `[offset, end)` of a DSF file, from scarletbook's layout `l`, the audio from
    /// `frames` (decoded DSD, interleaved, for file indices `first..`). A frame not given is
    /// filled with zeros, as the file's padding is.
    fn assemble_dsf(
        &self,
        l: &scarletbook::output::Layout,
        offset: u64,
        end: u64,
        first: u32,
        frames: &[Arc<Frame>],
    ) -> Vec<u8> {
        let mut out = read_buffer();
        out.resize(end.saturating_sub(offset) as usize, 0);
        if out.is_empty() {
            return out;
        }
        let channels = usize::from(self.layout.channels);
        let mut src = Frames {
            frames,
            first: self.start_frame.saturating_add(first),
            zeros: ZEROS.get(..channels * dsf::FRAME_BYTES as usize).unwrap_or(&[]),
            fill: None,
        };
        // Whatever the layout leaves out is zeros, not an earlier read's bytes.
        let filled = l.fill(offset, &mut out, &mut src).unwrap_or_else(|e| {
            log::debug!("{} area {} track {}: {e}", self.disc.path.display(), self.area, self.number);
            0
        });
        out.get_mut(filled..).unwrap_or(&mut []).fill(0);
        self.warn(src.fill, 0);
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

/// A frame of zeros of six channels, whose start is the padding of fewer.
static ZEROS: [u8; 6 * dsf::FRAME_BYTES as usize] = [0; 6 * dsf::FRAME_BYTES as usize];

/// Most reply buffers kept ([`give_back`]): as many as reads ever overlap in a straight read,
/// and a few more.
const READ_BUFFERS_KEPT: usize = 8;

/// Longest reply buffer kept, the largest read the kernel is asked to send.
const READ_BUFFER_MAX: usize = crate::fs::MAX_READ as usize;

/// Reply buffers not in use (#210), taken and given back as the DST decoders are.
///
/// *Why:* a new zeroed buffer per read, 128 KiB at glibc's mapping threshold, took about a
/// tenth of a straight read of plain DSD, whose reads are answered within some 30 us. A
/// buffer keeps its length, so a read of the same size as the last writes no zeros.
static READ_BUFFERS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

/// A reply buffer of any length, holding an earlier read's bytes.
fn read_buffer() -> Vec<u8> {
    READ_BUFFERS.lock().unwrap_or_else(|p| p.into_inner()).pop().unwrap_or_default()
}

/// Gives a read's bytes back, once they are sent, for the next read's reply.
pub fn give_back(buffer: Vec<u8>) {
    if buffer.capacity() <= READ_BUFFER_MAX {
        let mut kept = READ_BUFFERS.lock().unwrap_or_else(|p| p.into_inner());
        if kept.len() < READ_BUFFERS_KEPT {
            kept.push(buffer);
        }
    }
}

/// Drops the reply buffers kept (an idle engine, #150).
pub fn drop_read_buffers() {
    drop(std::mem::take(&mut *READ_BUFFERS.lock().unwrap_or_else(|p| p.into_inner())));
}

/// The decoded frames of a read, as scarletbook's layout asks for them: by timeline position.
struct Frames<'a> {
    frames: &'a [Arc<Frame>],
    /// The timeline position of `frames[0]`.
    first: u32,
    zeros: &'a [u8],
    /// Whether any frame given was filled.
    fill: Option<Fill>,
}

impl scarletbook::output::PositionSource for Frames<'_> {
    fn content(&mut self, t: u32) -> Result<std::borrow::Cow<'_, [u8]>, scarletbook::Error> {
        let f = t.checked_sub(self.first).and_then(|i| self.frames.get(i as usize));
        Ok(match f {
            Some(f) if f.data.len() == self.zeros.len() => {
                if f.fill != Fill::Audio {
                    self.fill = Some(f.fill);
                }
                std::borrow::Cow::Borrowed(&f.data[..])
            }
            _ => std::borrow::Cow::Borrowed(self.zeros),
        })
    }
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
    /// Reads of audio so far.
    audio_reads: u64,
    /// When the reader last jumped, and where to: what its pace is measured from.
    pace_from: Option<(std::time::Instant, u64)>,
    /// When the first read of audio came, and where it started.
    started: Option<(std::time::Instant, u64)>,
    /// Frames below this one (file index) are given up already ([`Engine::give_up`]).
    given_up: u32,
}

/// An open file: the file and its own demultiplexer state (and, for a WAV file, its last
/// decoded DSD frames), so that a sequential read continues where the last one stopped.
#[derive(Debug)]
pub struct Reader {
    /// The file.
    pub file: Arc<TrackFile>,
    stream: Mutex<Stream>,
    seq: Mutex<Seq>,
    id: u64,
    /// Where it was counted, if it was.
    counted: OnceLock<Arc<Traffic>>,
    /// Whether it reads for throughput, not as a player ([`Traffic`]).
    throughput: AtomicBool,
}

impl Drop for Reader {
    fn drop(&mut self) {
        if let Some(traffic) = self.counted.get() {
            traffic.fast.forget(self.id);
            traffic.players.forget(self.id);
        }
    }
}

impl Reader {
    /// A reader of `file`.
    pub fn new(file: Arc<TrackFile>) -> Self {
        Reader {
            file,
            stream: Mutex::new(Stream::default()),
            seq: Mutex::new(Seq::default()),
            id: NEXT_READER.fetch_add(1, Ordering::Relaxed),
            counted: OnceLock::new(),
            throughput: AtomicBool::new(false),
        }
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
        // Read ahead only after reads of audio: a tagger reading the header or the tags must
        // not make the file decode (and convert) its first seconds in the background.
        let ahead = if k0 < k1 { self.plan_read_ahead(engine, offset, end, k0, k1) } else { None };
        Ok((data, ahead))
    }

    /// Decodes the frames of `ahead` into the cache, if they are not there already.
    ///
    /// *Why slices only while someone plays:* one batch decodes faster, since the threads wait
    /// for the slowest frame at the end of every batch. Alone, a reader keeps that.
    pub fn read_ahead(&self, engine: &Engine, ahead: ReadAhead) {
        let slice = (engine.threads() as u32).saturating_mul(SLICE_FRAMES_PER_THREAD).max(SHARED_WINDOW_MIN);
        let mut first = ahead.first;
        while first < ahead.end {
            let sliced = self.throughput.load(Ordering::Relaxed)
                && engine.traffic().players.any(std::time::Instant::now());
            let end = if sliced { first.saturating_add(slice).min(ahead.end) } else { ahead.end };
            if let Err(e) = self.decoded(engine, first, end) {
                log::debug!("read-ahead of {}: {e}", self.file.disc.path.display());
                return;
            }
            first = end;
        }
    }

    /// Before decoding: a reader for throughput lets players go first, and a player says it is
    /// decoding, until the guard drops.
    fn before_decoding<'a>(&self, engine: &'a Engine) -> Option<Decoding<'a>> {
        if self.throughput.load(Ordering::Relaxed) {
            engine.traffic().yield_to_players();
            None
        } else {
            Some(engine.traffic().decoding())
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
                let _decoding = self.before_decoding(engine);
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
                let _decoding = self.before_decoding(engine);
                let mut stream = self.stream.lock().unwrap_or_else(|p| p.into_inner());
                engine.pcm_frames(&f.disc, f.area, &mut stream, t0, t1, f.area_end)
            }
        }
    }

    /// Whether the file is read far faster than it plays (`cp`, a scanner): at least
    /// [`FAST_AFTER`] bytes at more than [`FAST_FACTOR`] times its byte rate. Such a reader
    /// gets the file's long read-ahead window; a player keeps the 2 s one, so that its bursts
    /// of conversion stay short and don't hold up the other readers' requests.
    fn fast(&self, s: &Seq) -> bool {
        let Some((t, off)) = s.started else { return false };
        let read = s.max_end.saturating_sub(off);
        let secs = t.elapsed().as_secs_f64();
        read >= FAST_AFTER && read as f64 > FAST_FACTOR * self.file.byte_rate() * secs
    }

    /// Whether the reader, since it last jumped, has read at least [`FAST_AFTER`] bytes at more
    /// than [`PLAYER_FACTOR`] times the file's byte rate. *Why since it last jumped:* a player
    /// that seeks forward has not read what it skipped.
    fn reads_for_throughput(&self, s: &Seq, end: u64, now: std::time::Instant) -> bool {
        let Some((t, off)) = s.pace_from else { return false };
        let secs = now.saturating_duration_since(t).as_secs_f64();
        faster_than_a_player(end.saturating_sub(off), secs, self.file.byte_rate())
    }

    fn plan_read_ahead(&self, engine: &Engine, offset: u64, end: u64, k0: u32, k1: u32) -> Option<ReadAhead> {
        let mut s = self.seq.lock().unwrap_or_else(|p| p.into_inner());
        // The first read of audio never starts read-ahead: a tagger walking a WAV file's
        // chunks touches the audio once, at the data chunk's header, 256 KiB after the
        // header it read, and must not make the file convert seconds of audio. A player or
        // `cp` sends its next read at once, and that one starts it.
        s.audio_reads += 1;
        if s.started.is_none() {
            s.started = Some((std::time::Instant::now(), offset));
        }
        let sequential = s.audio_reads > 1
            && (offset <= s.max_end.saturating_add(SEQ_GAP_AHEAD)
                && offset.saturating_add(SEQ_GAP_BEHIND) >= s.max_end);
        s.max_end = s.max_end.max(end);
        if !sequential {
            s.ahead_until = 0;
            s.given_up = k0;
            s.pace_from = Some((std::time::Instant::now(), offset));
        }
        let now = std::time::Instant::now();
        let throughput = self.reads_for_throughput(&s, end, now);
        let traffic = engine.traffic();
        self.counted.get_or_init(|| traffic.clone());
        self.throughput.store(throughput, Ordering::Relaxed);
        let (mine, other) =
            if throughput { (&traffic.fast, &traffic.players) } else { (&traffic.players, &traffic.fast) };
        other.forget(self.id);
        let n = mine.note(self.id, now);
        if !sequential {
            return None;
        }
        // Nor does the kernel's read-ahead of the first read: a DSF file's audio starts at
        // byte 92, so a tagger's read of the header is a read of audio, and the kernel
        // follows it with a second one at once. Nor a scanner's read of the head
        // (HEAD_WINDOW, which holds KERNEL_FIRST_WINDOW). A player's reads go past it.
        if s.started
            .is_some_and(|(_, at)| s.max_end <= at.saturating_add(HEAD_WINDOW.max(KERNEL_FIRST_WINDOW)))
        {
            return None;
        }
        let frames = self.file.layout.frames;
        let window = if throughput {
            if s.given_up < k0 {
                let f = &self.file;
                engine.give_up(&f.disc, f.area, f.format(), f.start_frame + s.given_up, f.start_frame + k0);
                s.given_up = k0;
            }
            let own = if self.fast(&s) {
                self.file.read_ahead_frames(engine.threads(), engine.cache().cap(), n)
            } else {
                READ_AHEAD_FRAMES
            };
            if traffic.players.any(now) {
                own.min(self.file.shared_frames(engine.cache().cap(), n))
            } else {
                own
            }
        } else {
            READ_AHEAD_FRAMES
        };
        if k1 >= frames || s.ahead_until >= frames.min(k1 + window / 2) {
            return None;
        }
        let start = s.ahead_until.max(k1);
        let stop = frames.min(k1 + window);
        s.ahead_until = stop;
        (start < stop).then_some(ReadAhead { first: start, end: stop })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Readers count while they read and until they close or pause.
    #[test]
    fn fast_readers_are_those_reading_now() {
        let r = FastReaders::default();
        let t0 = std::time::Instant::now();
        assert_eq!(r.note(1, t0), 1);
        assert_eq!(r.note(1, t0), 1, "the same reader again");
        assert_eq!(r.note(2, t0), 2);
        r.forget(1);
        assert_eq!(r.note(2, t0), 1, "closed");
        assert_eq!(r.note(3, t0), 2);
        let later = t0 + FAST_READER_IDLE + std::time::Duration::from_millis(1);
        assert_eq!(r.note(3, later), 1, "the other paused");
    }

    /// A paced reader is a player, however far it gets; one that reads faster is not, once it
    /// has read enough to tell.
    #[test]
    fn pace_tells_a_player_from_a_scan() {
        let rate = 3_175_200.0;
        assert!(!faster_than_a_player(FAST_AFTER - 1, 0.001, rate), "too early to tell");
        assert!(faster_than_a_player(FAST_AFTER, 0.001, rate));
        assert!(!faster_than_a_player(32 << 20, 10.0, rate), "real time");
        assert!(!faster_than_a_player(47_000_000, 10.0, rate), "catching up");
        assert!(faster_than_a_player(48_000_000, 10.0, rate));
    }

    /// Readers not playing wait for a player's decoding, but not for long if it never ends.
    #[test]
    fn traffic_yields_to_players_for_a_bounded_time() {
        let t = Arc::new(Traffic::default());
        let start = std::time::Instant::now();
        t.yield_to_players();
        assert!(start.elapsed() < YIELD_MAX, "no player: no wait");

        let held = t.decoding();
        let start = std::time::Instant::now();
        t.yield_to_players();
        assert!(start.elapsed() >= YIELD_MAX, "bounded, not skipped");
        drop(held);

        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = {
            let t = t.clone();
            let held = t.clone();
            let guard = std::thread::spawn(move || {
                let g = held.decoding();
                tx.send(()).unwrap();
                std::thread::sleep(YIELD_MAX / 5);
                drop(g);
            });
            rx.recv().unwrap();
            let start = std::time::Instant::now();
            t.yield_to_players();
            guard.join().unwrap();
            start.elapsed()
        };
        assert!(waiter < YIELD_MAX, "woken when the player is done: {waiter:?}");
    }

    /// Only so many buffers are kept, none over the largest read, and an idle engine drops them.
    #[test]
    fn reply_buffers_are_bounded() {
        drop_read_buffers();
        for _ in 0..READ_BUFFERS_KEPT + 3 {
            give_back(vec![1; 4096]);
        }
        give_back(Vec::with_capacity(READ_BUFFER_MAX + 1));
        assert!(READ_BUFFERS.lock().unwrap_or_else(|p| p.into_inner()).len() <= READ_BUFFERS_KEPT);
        assert!(read_buffer().capacity() <= READ_BUFFER_MAX);
        drop_read_buffers();
    }
}
