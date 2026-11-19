// SPDX-License-Identifier: GPL-3.0-or-later

//! A track shown as a DSF or WAV file, and reading byte ranges of it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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

/// A reader counts as a player only after this long, and once it has read [`FAST_AFTER`] or
/// read for as long as that takes at [`PLAYER_FACTOR`] times the file's byte rate
/// ([`Reader::judged`]). *Why:* a scanner opening its next file, a tagger or an import's few
/// reads look like a player for their first 4 MiB; had they counted, scanners alone made each
/// other wait, up to 16 % slower on 32 threads.
const PLAYER_AFTER: std::time::Duration = std::time::Duration::from_millis(500);

/// Frames this close behind a reader are not given up ([`Traffic`]). *Why:* the kernel sends a
/// reader's requests concurrently, so one may arrive after a later one, and a frame gone by then
/// means decoding it again and locating the stream anew.
const GIVE_UP_MARGIN: u64 = 1 << 20;

/// While someone plays, a reader that is not playing reads ahead at least this many frames...
const SHARED_WINDOW_MIN: u32 = 16;

/// A reader not yet judged a player or a reader for throughput ([`PLAYER_AFTER`]) reads ahead
/// only this many frames while a player reads (#363). *Why:* a player that starts beside copies
/// starts with them, and none of them can be told apart in their first half second. Each read
/// ahead a player's 2 s window then, so the new player's first reads waited behind about
/// 600 frames of the copies' work, 0.4 to 0.9 s on 4 cores. The readers' own reads are not
/// limited, and the window comes back once they are judged.
const UNJUDGED_WINDOW: u32 = SHARED_WINDOW_MIN;

/// ...and in slices of this many frames per decoder thread, at least [`SHARED_WINDOW_MIN`]:
/// about 20 ms of work per slice, whatever the number of threads.
const SLICE_FRAMES_PER_THREAD: u32 = 8;

/// The longest that a reader not playing waits for players, before each slice. *Why bounded:*
/// a player may be waiting for a stalled source, and enough players could always be busy.
const YIELD_MAX: std::time::Duration = std::time::Duration::from_millis(50);

/// A fast reader not heard from for this long no longer counts ([`FastReaders`]): one that
/// paused needs no share of the cache, and it gets one back at its next read.
const FAST_READER_IDLE: std::time::Duration = std::time::Duration::from_secs(1);

/// A player not heard from for this long, with no read in progress, no longer counts
/// ([`Traffic`]). *Why longer than [`FAST_READER_IDLE`] (#357):* on a server too slow for a
/// player and the copies beside it, the player's client could go more than a second without a
/// read reaching sacdfs, while its own requests waited in the client or in the SMB server. The
/// copies then counted as players for a while, read ahead as players and stalled it for longer.
const PLAYER_IDLE: std::time::Duration = std::time::Duration::from_secs(5);

/// A list of readers at the moment: those reading for throughput, which share what the cache can
/// hold ahead of them, or the players ([`Traffic`]). Asked once per read; the list is as long as
/// the readers.
#[derive(Debug)]
pub struct FastReaders {
    seen: Mutex<Vec<Seen>>,
    /// How long one not heard from, with no read in progress, still counts.
    idle: std::time::Duration,
}

impl Default for FastReaders {
    fn default() -> Self {
        Self::with_idle(FAST_READER_IDLE)
    }
}

/// A reader on a [`FastReaders`] list.
#[derive(Debug)]
struct Seen {
    id: u64,
    /// When it was last heard from.
    at: std::time::Instant,
    /// Since when its pace is measured.
    since: std::time::Instant,
    /// Its reads in progress, if counted ([`Reader::arrived`]).
    reads: Option<Arc<AtomicU32>>,
}

impl Seen {
    /// Whether it still counts: heard from lately, or waiting for a read. *Why a read in
    /// progress counts (#357):* a player that waits seconds for a read is still playing. Left
    /// to expire, it let the copies beside it count as players and read ahead as one, which
    /// stalled it longer.
    fn lately(&self, now: std::time::Instant, idle: std::time::Duration) -> bool {
        now.saturating_duration_since(self.at) < idle
            || self.reads.as_ref().is_some_and(|r| r.load(Ordering::Relaxed) > 0)
    }
}

impl FastReaders {
    /// An empty list whose readers count for `idle` after they were last heard from.
    fn with_idle(idle: std::time::Duration) -> Self {
        FastReaders { seen: Mutex::default(), idle }
    }

    /// Notes that reader `id` is fast now, and says how many are.
    fn note(&self, id: u64, now: std::time::Instant) -> usize {
        self.note_since(id, now, now, None)
    }

    /// Notes that reader `id`, whose pace is measured since `since` and whose reads in
    /// progress are `reads`, is on the list now, and says how many are.
    fn note_since(
        &self,
        id: u64,
        now: std::time::Instant,
        since: std::time::Instant,
        reads: Option<Arc<AtomicU32>>,
    ) -> usize {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.retain(|r| r.id != id && r.lately(now, self.idle));
        seen.push(Seen { id, at: now, since, reads });
        seen.len()
    }

    /// Whether a reader other than `id`, heard from lately, has been measured since before
    /// `since` ([`Reader::pace`]).
    fn any_before(&self, id: u64, since: std::time::Instant, now: std::time::Instant) -> bool {
        let seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.iter().any(|r| r.id != id && r.since < since && r.lately(now, self.idle))
    }

    /// Reader `id` closed. *Why not left to expire:* a `cp` opens the next file at once, and
    /// would count as several readers, with a short window, for its whole run.
    fn forget(&self, id: u64) {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).retain(|r| r.id != id);
    }

    /// Whether any reader has been heard from in the last [`FAST_READER_IDLE`], or waits for a
    /// read. *Why not the list's own `idle`:* a player's client fetches in bursts about a second
    /// apart, and the #230 rules need not hold the readers beside it between them; only which
    /// reader goes first does ([`FastReaders::any_before`]).
    fn any(&self, now: std::time::Instant) -> bool {
        let seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.iter().any(|r| r.lately(now, FAST_READER_IDLE))
    }

    /// Whether a reader other than `id` has been heard from in the last [`FAST_READER_IDLE`], or
    /// waits for a read.
    fn any_but(&self, id: u64, now: std::time::Instant) -> bool {
        let seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.iter().any(|r| r.id != id && r.lately(now, FAST_READER_IDLE))
    }
}

/// Frames of a slice of read-ahead: [`SLICE_FRAMES_PER_THREAD`] per decoder thread, at least
/// [`SHARED_WINDOW_MIN`].
fn slice_frames(engine: &Engine) -> u32 {
    (engine.threads() as u32).saturating_mul(SLICE_FRAMES_PER_THREAD).max(SHARED_WINDOW_MIN)
}

/// How far the start-of-track decode-ahead goes past the furthest frame read
/// ([`START_LEAD`]).
fn start_lead(engine: &Engine) -> u32 {
    slice_frames(engine).min(START_LEAD)
}

/// Whether `read` bytes in `secs` of a file of `byte_rate` bytes per second is more than a player
/// reads ([`Traffic`]).
fn faster_than_a_player(read: u64, secs: f64, byte_rate: f64) -> bool {
    read >= FAST_AFTER && read as f64 > PLAYER_FACTOR * byte_rate * secs
}

/// Who is reading, so that a whole-file reader doesn't starve someone playing (#230).
///
/// A reader that, since it last jumped, has read 4 MiB (`FAST_AFTER`) at more than 1.5 times
/// the file's byte rate (`PLAYER_FACTOR`) reads for *throughput*. Any other that has been reading
/// for 0.5 s (`PLAYER_AFTER`) is a *player*. While a player is active, readers for throughput do
/// three things differently:
///
/// 1. **They give up the frames they have passed** ([`Engine::give_up`]). *Why this is the fix:*
///    the cache is least recently used, a player's read-ahead waits there for 1 to 2 s, and a
///    whole-file reader at 30 MB/s replaces 32 MiB in 1 s. The player's frames were evicted before
///    it got to them; it converted them twice and then read 4 frames at a time, each behind
///    the scanner's whole window on the decoder threads. That is why a cache of 8 MiB helped,
///    oddly: no read-ahead survived for anyone, so every reader's work came in small pieces
///    and they took turns.
/// 2. **They share a quarter of the cache**, however few frames that is.
/// 3. **They read ahead in slices and wait for players between them**, so that a player's work
///    waits for a slice, not for a window of frames.
///
/// With no player active they do none of this, so a scan alone costs what it did.
#[derive(Debug)]
pub struct Traffic {
    /// Readers for throughput. Their read-ahead windows share a quarter of the cache.
    fast: FastReaders,
    /// Players.
    players: FastReaders,
    /// Players' requests decoding now.
    decoding: Mutex<usize>,
    done: Condvar,
    /// Start-of-track decode-aheads running now ([`START_RUNNING_MAX`]).
    starting: std::sync::atomic::AtomicUsize,
}

impl Default for Traffic {
    fn default() -> Self {
        Traffic {
            fast: FastReaders::default(),
            players: FastReaders::with_idle(PLAYER_IDLE),
            decoding: Mutex::default(),
            done: Condvar::default(),
            starting: std::sync::atomic::AtomicUsize::new(0),
        }
    }
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

/// A read this far from the furthest byte read so far, either way, is a jump for the reader's
/// pace ([`Traffic`]): it is measured again from there. *Why this far (#271):* Samba serves a
/// kernel SMB client's 4 MiB reads concurrently, so a straight copy's reads reach sacdfs up to
/// about 20 MiB out of order (measured). With the sequential gaps above as the test, a copy over
/// SMB kept starting its pace again, lost its long read-ahead window and was 8 to 11 % slower
/// than in 0.4.1. *Why that is safe:* the pace counts the bytes read, so a player that seeks
/// less far than this still reads at its own rate.
const PACE_JUMP: u64 = 64 << 20;

/// A read landing more than this far behind the furthest byte read so far shows that a network
/// client with its own read-ahead stands between the application and sacdfs ([`PACE_LEAD`]).
/// *Why this far:* through the kernel's FUSE read-ahead a local reader's reads land at most
/// 0.5 MiB behind (measured); through Samba a player's land 3 to 11 MiB behind, a copy's up to
/// 56 MiB, from its first second.
const NETWORK_REORDER: u64 = 2 << 20;

/// A reader behind a network client ([`NETWORK_REORDER`]) reads for throughput only once it is
/// this far ahead of playing time, as well as faster than a player. *Why (#271):* the client
/// reads ahead of the application. The Linux kernel SMB client keeps a player 8 to 16 MiB ahead
/// (measured), and fetches the first 4 to 8 MiB at once, so measured from its start a player
/// looked like a copy for its first 3 to 7 s. Counted as reading for throughput, it gave up its
/// place: four copies starting beside it made it stall for up to 1.5 s. A copy gets this far ahead
/// within 1 to 4 s on four cores, 8 to 13 s on two; until then it is unsure ([`Pace::Unsure`]).
/// *Why only behind a network client:* a scanner reading files locally would be unsure for most
/// of a short file.
const PACE_LEAD: u64 = 32 << 20;

/// A read arriving after the reader had none in progress for this long shows that its client
/// paused: what is behind it is paced by playback ([`Seq::paused`]). *Why this long (#357):* the
/// Linux kernel SMB client fetches a player's audio in bursts about a second apart, each answered
/// in milliseconds when the server keeps up; a copy's client keeps several reads outstanding, so
/// one is always in progress.
const CLIENT_PAUSE: std::time::Duration = std::time::Duration::from_millis(500);

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

/// The end of the start-of-track decode-ahead (#481, `Reader::start_ahead`): the first 8 s of
/// a track, within a quarter of the cache. *Why:* a player starting a track from stopped reads its
/// first seconds as fast as it can, to fill its buffer. Before, its first 1 MiB was decoded read
/// by read, and then a 2 s window in one batch, which held the reads behind it: on two slow cores
/// decoding 6-channel DST at twice real time, one read waited a second and the first second of
/// reads brought half a second of audio. Playback over SMB glitched for 5 to 7 s from a cold
/// start, though it held once running (#456). *Why 8 s:* with a cache that held them, 4 s gave
/// worse worst reads than 8 s (the window after it came while the player's buffer was still
/// small), and 12 s no better.
pub const START_AHEAD_FRAMES: u32 = 8 * 75;

/// Bytes of a file's audio from its start whose frames a read keeps in the cache as its head
/// ([`crate::engine::FrameCache::keep`], #456): a network player's probe of a 6-channel DSF
/// file reads its first 1.6 MiB.
const HEAD_KEEP: u64 = 2 << 20;

/// Most frames the start-of-track decode-ahead decodes past the furthest frame read
/// ([`Reader::start_ahead`]), and less with fewer than 4 decoder threads: a slice. *Why bounded:*
/// a reader that stops (a scanner reading more than its head) has those decoded for nothing. A
/// player starting a track reads ahead of decoding, so the bound does not hold it up.
const START_LEAD: u32 = 32;

/// Above this many times real time, a reader close behind the read-ahead after the
/// start-of-track decode-ahead reads like a copy, and its windows go in one batch
/// ([`Reader::plan_read_ahead`], #456). A player on a CPU too slow to fill its buffer quickly
/// reads at no more than the CPU's decoding speed, about 1 to 2 times real time on an Atom.
const SLICE_PACE_MAX: f64 = 4.0;

/// How long the start-of-track decode-ahead waits for the next read once it has got
/// [`START_LEAD`] ahead, before it ends: the client paused, or read the head and left
/// without closing (an NFS client never closes).
const START_IDLE: std::time::Duration = CLIENT_PAUSE;

/// The longest a read waits for the start-of-track decode-ahead to reach its frames, before it
/// decodes them itself ([`Reader::wait_for_start`]): a source that stalls holds the decode-ahead.
const START_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Most start-of-track decode-aheads running at once, per engine. *Why:* a library scan opens
/// many files at once; each could otherwise take decoder threads for its head's next slices.
const START_RUNNING_MAX: usize = 2;

/// What a file's node holds without its trailer, in bytes: the struct, the layout or header, the
/// name and entry, the node and its place in the map, with the allocator's overhead on them.
const FILE_HELD: usize = 1200;

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
pub(crate) fn pause_range(area: &scarletbook::Area, track: &Track) -> Option<(u32, u32)> {
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

    /// About what this file's node holds on the heap, for the bound on listed images (#237):
    /// its tag and trailer, which a tags file can make large, and the rest as measured.
    pub(crate) fn held_bytes(&self) -> usize {
        FILE_HELD + self.trailer.capacity() * if self.dsf.is_some() { 2 } else { 1 }
    }

    /// Frames read ahead of a fast sequential reader. For a WAV file, about 8 MiB of PCM: a
    /// longer window than DSF's keeps the pipeline of demultiplexing and conversion busy for
    /// a fast reader (`cp`) of a plain DSD area, where conversion is the only cost.
    ///
    /// For a DSF file, [`FAST_FRAMES_PER_THREAD`] per decoder thread, at most
    /// [`FAST_FRAMES_MAX`] and a share of a quarter of the cache, at least
    /// [`READ_AHEAD_FRAMES`]. `fast_readers` are the readers for throughput at the moment
    /// ([`Traffic`]), this one included.
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

    /// The end of the start-of-track decode-ahead (#481): [`START_AHEAD_FRAMES`], within the
    /// file and a quarter of a cache of `cache_bytes` (the decode-ahead can get that far ahead
    /// of the reads). 0 if that quarter can't hold what it decodes past the furthest read
    /// (`start_lead`) and a slice of `slice` frames.
    pub fn start_ahead_frames(&self, cache_bytes: usize, lead: u32, slice: u32) -> u32 {
        let frame = (self.byte_rate() as u64 / 75).max(1);
        let quarter = cache_bytes as u64 / 4 / frame;
        if quarter < u64::from(lead) + u64::from(slice) {
            return 0;
        }
        quarter.min(u64::from(START_AHEAD_FRAMES)).min(u64::from(self.layout.frames)) as u32
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
    /// without what came from the image's path ([`dsf::stored_tag`]). A generated `Track NN` title
    /// stays. *Why:* it depends on nothing that can change, so an extract that has it from a mount
    /// loses nothing when it goes, and `import` storing it as if edited by hand would keep a
    /// lookup's title out (#185).
    pub fn stored_frames(&self) -> Vec<crate::verify::id3::Frame> {
        if !self.filled {
            return crate::verify::id3::frames(self.tag());
        }
        let toc = self.disc.toc();
        let tag = toc.areas.get(self.area).and_then(|a| {
            let track = a.tracks.iter().find(|t| t.number == self.number)?;
            Some(dsf::stored_tag(toc, a, track))
        });
        let mut frames = crate::verify::id3::frames(&tag.unwrap_or_default());
        if !frames.iter().any(|f| f.key == "TIT2" && !f.value.trim().is_empty()) {
            frames.push(crate::verify::id3::Frame {
                key: "TIT2".to_owned(),
                value: crate::names::fallback_title(self.number),
            });
        }
        frames
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
const READ_BUFFER_MAX: usize = crate::vfs::MAX_READ as usize;

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

/// A reader's pace ([`Reader::pace`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pace {
    /// No faster than a player.
    Slow,
    /// Faster than a player, and behind a network client far ahead of playing time.
    Fast,
    /// Faster than a player behind a network client, but not yet far ahead of playing time.
    Unsure,
}

/// Frames of a file (indices `[first, end)`) to decode ahead of a sequential reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadAhead {
    /// First frame.
    pub first: u32,
    /// End frame (exclusive).
    pub end: u32,
    /// Whether this is the start-of-track decode-ahead (`Reader::start_ahead`), which
    /// decides its frames as it goes.
    pub start: bool,
    /// Whether it is decoded in slices, so that a read of its frames waits for a slice.
    pub sliced: bool,
}

/// Whether a process is a network file server's ([`Reader::opened_by`]).
pub type IsServer = fn(u32) -> bool;

#[derive(Debug, Default)]
struct Seq {
    /// The furthest byte read so far.
    max_end: u64,
    /// Frames up to here (file index, exclusive) are decoded or being decoded ahead.
    ahead_until: u32,
    /// Reads of audio so far.
    audio_reads: u64,
    /// When the reader last jumped, and the lowest byte read since: what its pace is measured
    /// from.
    pace_from: Option<(std::time::Instant, u64)>,
    /// Bytes of audio read since then ([`PACE_JUMP`]).
    paced: u64,
    /// The furthest byte read since then.
    pace_end: u64,
    /// Whether a read has landed far behind the others: a network client reads ahead
    /// ([`NETWORK_REORDER`]), or a network file server's process opened the file.
    networked: bool,
    /// The process that opened the file, and how to tell whether it is a network file
    /// server's, until that is asked ([`Reader::opened_by`]).
    opener: Option<(u32, IsServer)>,
    /// When the first read of audio came, and where it started.
    started: Option<(std::time::Instant, u64)>,
    /// The start-of-track decode-ahead has just ended, and the next window goes in slices.
    after_start: bool,
    /// The last window went in slices, after the start-of-track decode-ahead: the next one does
    /// too while the reader is still within a slice of what is decoded (#456).
    slicing: bool,
    /// When a window in slices was planned, until its job starts ([`Reader::sliced_running`]).
    sliced_planned: Option<std::time::Instant>,
    /// Frames below this one (file index) are given up already ([`Engine::give_up`]).
    given_up: u32,
    /// Whether its client has paused since it last jumped ([`CLIENT_PAUSE`]). *Why (#357):*
    /// on a server too slow for a player and the copies beside it, every reader reads at about
    /// the rate the server allows, so pace alone can't tell a copy from a player: copies counted
    /// as players after their first seconds, or from their start. The player then had no
    /// precedence, and every reader read ahead a player's window, which overflowed a small cache:
    /// frames evicted before they were read were converted again. The player's worst read was 3
    /// times 0.6.1's. A copy's client never pauses; a player's does whenever the server keeps up.
    paused: bool,
}

/// The start-of-track decode-ahead of a reader ([`Reader::start_ahead`]).
#[derive(Debug, Default)]
struct StartAhead {
    /// The reader's first read of audio was within the decode-ahead's frames, and it has not
    /// started yet.
    eligible: bool,
    /// The frame (file index) of the reader's first read of audio.
    from: u32,
    /// From a read past the kernel's first window, after a first read of the start of the
    /// audio, until the reader jumps, closes or pauses, or the decode-ahead reaches `end`.
    active: bool,
    /// Whether a job runs it now. Reads wait for it only then.
    running: bool,
    /// When a job was last planned to run it.
    planned: Option<std::time::Instant>,
    /// The first frame (file index) not yet taken by the job.
    next: u32,
    /// Frames before this one are wanted now: the furthest read and the lead.
    target: u32,
    /// Its last frame (exclusive).
    end: u32,
    /// Frames up to here are decoded (or failed), for the read-ahead that follows it.
    done: u32,
    /// Slices taken so far.
    slices: u32,
    /// A slice failed to decode (for the trace).
    failed: bool,
}

/// A reader's reads in progress, from their arrival to their reply ([`Reader::arrived`]).
#[derive(Debug, Default)]
struct Flight {
    /// Since when none has been in progress, if one ever was.
    idle_from: Option<std::time::Instant>,
    /// Whether one arrived after none had been in progress for [`CLIENT_PAUSE`], since this
    /// was last asked ([`Reader::paused`]).
    paused: bool,
}

/// A read of a [`Reader`] in progress, until it drops ([`Reader::arrived`]).
#[derive(Debug)]
pub struct Arrival(Arc<Reader>);

impl Drop for Arrival {
    fn drop(&mut self) {
        let mut f = self.0.flight.lock().unwrap_or_else(|p| p.into_inner());
        if self.0.reads.fetch_sub(1, Ordering::Relaxed) == 1 {
            f.idle_from = Some(std::time::Instant::now());
        }
    }
}

/// An open file: the file and its own demultiplexer state (and, for a WAV file, its last
/// decoded DSD frames), so that a sequential read continues where the last one stopped.
#[derive(Debug)]
pub struct Reader {
    /// The file.
    pub file: Arc<TrackFile>,
    stream: Mutex<Stream>,
    seq: Mutex<Seq>,
    /// Reads in progress ([`Reader::arrived`]).
    reads: Arc<AtomicU32>,
    flight: Mutex<Flight>,
    id: u64,
    /// Where it was counted, if it was.
    counted: OnceLock<Arc<Traffic>>,
    /// Whether it reads for throughput ([`Traffic`]).
    throughput: AtomicBool,
    /// Whether it is a player ([`Traffic`]).
    player: AtomicBool,
    start: Mutex<StartAhead>,
    /// Reads waiting for `stream` ([`Reader::lock_stream`]).
    stream_waits: AtomicU32,
    /// Told when the start-of-track decode-ahead has more to do, or must stop.
    start_more: Condvar,
    /// A window of read-ahead in slices is being decoded ([`Reader::plan_read_ahead`]).
    sliced_running: AtomicBool,
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
        Self::opened_by(file, 0, |_| false)
    }

    /// A reader of `file`, opened by process `pid`: behind a network client, whatever the order
    /// of its reads, if `is_server(pid)` says it is a network file server's (#362). That is
    /// asked once, and only when it matters: when the reader has read `FAST_AFTER` (4 MiB), since
    /// below that its pace is a player's either way. *Why not at the open (#403):* the answer
    /// costs two reads of `/proc`, about 25 microseconds, and a scan of short files opened
    /// through the mount was 10 % slower for it.
    pub fn opened_by(file: Arc<TrackFile>, pid: u32, is_server: IsServer) -> Self {
        Reader {
            file,
            stream: Mutex::new(Stream::default()),
            seq: Mutex::new(Seq { opener: Some((pid, is_server)), ..Seq::default() }),
            reads: Arc::default(),
            flight: Mutex::new(Flight::default()),
            id: NEXT_READER.fetch_add(1, Ordering::Relaxed),
            counted: OnceLock::new(),
            throughput: AtomicBool::new(false),
            player: AtomicBool::new(false),
            start: Mutex::default(),
            stream_waits: AtomicU32::new(0),
            start_more: Condvar::new(),
            sliced_running: AtomicBool::new(false),
        }
    }

    /// The reader's number, which the read trace names it by ([`crate::trace`]).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Notes that the file was closed: its start-of-track decode-ahead stops after its slice.
    pub fn close(&self) {
        self.stop_start();
    }

    fn stop_start(&self) {
        self.start.lock().unwrap_or_else(|p| p.into_inner()).active = false;
        self.start_more.notify_all();
    }

    /// Bytes `[offset, offset + len)` of the file (fewer at its end), and the frames (file
    /// indices) to decode ahead, if the reads look sequential.
    pub fn read(
        &self,
        engine: &Engine,
        offset: u64,
        len: u64,
    ) -> Result<(Vec<u8>, Option<ReadAhead>), ReadError> {
        self.read_traced(engine, offset, len, None)
    }

    /// [`Reader::read`], noting in `trace`, if given, whether its audio was in the cache when it
    /// came and how long it waited for the start-of-track decode-ahead ([`crate::trace`]).
    pub fn read_traced(
        &self,
        engine: &Engine,
        offset: u64,
        len: u64,
        trace: Option<&mut crate::trace::ReadTrace>,
    ) -> Result<(Vec<u8>, Option<ReadAhead>), ReadError> {
        let f = &self.file;
        let end = offset.saturating_add(len).min(f.size);
        let (k0, k1) = f.frames_for(offset, end);
        if let Some(t) = trace {
            t.hit = k0 >= k1 || self.in_cache(engine, k0, k1);
            let waiting = std::time::Instant::now();
            if k0 < k1 {
                self.wait_for_start(engine, k0, k1);
            }
            t.start_wait = waiting.elapsed();
        } else if k0 < k1 {
            self.wait_for_start(engine, k0, k1);
        }
        let frames = if k0 < k1 { self.decoded(engine, k0, k1, true)? } else { Vec::new() };
        let data = f.assemble(offset, len, k0, &frames);
        // Read ahead only after reads of audio: a tagger reading the header or the tags must
        // not make the file decode (and convert) its first seconds in the background.
        let ahead = if k0 < k1 { self.plan_read_ahead(engine, offset, end, k0, k1) } else { None };
        Ok((data, ahead))
    }

    /// Notes that a read arrived, as soon as it does and before it waits for a thread: it is in
    /// progress until the result drops, after its reply (`CLIENT_PAUSE`, `Seen::lately`).
    pub fn arrived(self: &Arc<Self>) -> Arrival {
        self.arrived_at(std::time::Instant::now())
    }

    fn arrived_at(self: &Arc<Self>, now: std::time::Instant) -> Arrival {
        let mut f = self.flight.lock().unwrap_or_else(|p| p.into_inner());
        if self.reads.fetch_add(1, Ordering::Relaxed) == 0
            && f.idle_from.is_some_and(|t| now.saturating_duration_since(t) >= CLIENT_PAUSE)
        {
            f.paused = true;
        }
        drop(f);
        Arrival(self.clone())
    }

    /// Whether the reader's client has paused since this was last asked ([`Flight::paused`]).
    fn paused(&self) -> bool {
        std::mem::take(&mut self.flight.lock().unwrap_or_else(|p| p.into_inner()).paused)
    }

    /// Decodes the frames of `ahead` into the cache, if they are not there already.
    ///
    /// *Why slices only while someone plays:* one batch decodes faster, since the threads wait
    /// for the slowest frame at the end of every batch. Alone, a reader keeps that.
    pub fn read_ahead(&self, engine: &Engine, ahead: ReadAhead) {
        if ahead.start {
            self.start_ahead(engine);
            return;
        }
        struct Sliced<'a>(&'a Reader);
        impl Drop for Sliced<'_> {
            fn drop(&mut self) {
                self.0.sliced_running.store(false, Ordering::Relaxed);
            }
        }
        // Running before the plan is forgotten, so that no read plans another in between.
        let _sliced = ahead.sliced.then(|| {
            self.sliced_running.store(true, Ordering::Relaxed);
            self.seq.lock().unwrap_or_else(|p| p.into_inner()).sliced_planned = None;
            Sliced(self)
        });
        let slice = slice_frames(engine);
        let mut first = ahead.first;
        while first < ahead.end {
            let sliced = ahead.sliced
                || self.throughput.load(Ordering::Relaxed)
                    && engine.traffic().players.any(std::time::Instant::now());
            let end = if sliced { first.saturating_add(slice).min(ahead.end) } else { ahead.end };
            if sliced {
                self.let_reads_go_first();
            }
            if let Err(e) = self.decoded(engine, first, end, false) {
                log::debug!("read-ahead of {}: {e}", self.file.disc.path.display());
                return;
            }
            first = end;
        }
    }

    /// The start-of-track decode-ahead (#481), planned by [`Reader::plan_start`]: decodes the
    /// file's next frames into the cache, a slice at a time, up to [`START_AHEAD_FRAMES`] (within
    /// a quarter of the cache) and [`START_LEAD`] frames past the furthest read, or to the end
    /// while reads wait for it ([`Reader::wait_for_start`]). It stops when the reader jumps or
    /// closes, or does not read for [`START_IDLE`] once it is that far ahead; the read-ahead then
    /// goes on from where it got, its first window in slices.
    ///
    /// *Why slices:* a read of frames being decoded waits for their slice, not for a whole
    /// window. *Why from the first read rather than the open:* a scanner or a file server opens
    /// files it never reads (an SMB or NFS server opens before it reads), and the first read
    /// comes milliseconds after a player's open.
    fn start_ahead(&self, engine: &Engine) {
        struct Running<'a>(&'a Reader, &'a Traffic, &'static str);
        impl Drop for Running<'_> {
            fn drop(&mut self) {
                // Also after a panic: reads must not wait for a job that is gone.
                let mut st = self.0.start.lock().unwrap_or_else(|p| p.into_inner());
                st.running = false;
                st.active = false;
                let done = st.done;
                drop(st);
                self.0.start_more.notify_all();
                self.1.starting.fetch_sub(1, Ordering::Relaxed);
                crate::trace_line!("start-ahead r={} stop: {} at {done}", self.0.id, self.2);
            }
        }
        // The job takes its place here, not when planned: a plan may be dropped unrun (`sacdfs
        // verify` reads without read-ahead; a read the watchdog answered first spawns nothing).
        {
            let mut st = self.start.lock().unwrap_or_else(|p| p.into_inner());
            if !st.active || st.running {
                return;
            }
            let starting = &engine.traffic().starting;
            if starting.fetch_add(1, Ordering::Relaxed) >= START_RUNNING_MAX {
                starting.fetch_sub(1, Ordering::Relaxed);
                st.active = false;
                crate::trace_line!("start-ahead r={} stop: {START_RUNNING_MAX} running already", self.id);
                return;
            }
            st.running = true;
        }
        let mut running = Running(self, engine.traffic(), "panic");
        let slice = slice_frames(engine);
        loop {
            let (first, end) = {
                let mut st = self.start.lock().unwrap_or_else(|p| p.into_inner());
                loop {
                    if !st.active || st.next >= st.end {
                        running.2 = if st.active {
                            "end"
                        } else if st.failed {
                            "decode error"
                        } else {
                            "jump or close"
                        };
                        st.active = false;
                        return;
                    }
                    if st.next < st.target {
                        break;
                    }
                    let (g, waited) =
                        self.start_more.wait_timeout(st, START_IDLE).unwrap_or_else(|p| p.into_inner());
                    st = g;
                    if waited.timed_out() && st.next >= st.target {
                        running.2 = "idle";
                        st.active = false;
                        return;
                    }
                }
                // The first slices are short, so that the reads that came with the first one
                // wait little: two frames per thread, then twice as many each time.
                let size = (engine.threads() as u32)
                    .max(1)
                    .saturating_mul(2)
                    .checked_shl(st.slices)
                    .unwrap_or(u32::MAX)
                    .min(slice);
                st.slices = st.slices.saturating_add(1);
                let first = st.next;
                st.next = st.target.min(first.saturating_add(size));
                (first, st.next)
            };
            // Beside a player, a slice at a time after its decoding, like a reader for
            // throughput's read-ahead (#363).
            if engine.traffic().players.any_but(self.id, std::time::Instant::now()) {
                engine.traffic().yield_to_players();
            }
            self.let_reads_go_first();
            let r = self.decoded(engine, first, end, false);
            let mut st = self.start.lock().unwrap_or_else(|p| p.into_inner());
            st.done = st.done.max(end);
            if let Err(e) = r {
                st.active = false;
                st.failed = true;
                log::debug!("start-ahead of {}: {e}", self.file.disc.path.display());
            }
            drop(st);
            self.start_more.notify_all();
        }
    }

    /// Before a read of frames `[k0, k1)` decodes them itself: while the start-of-track
    /// decode-ahead runs and will reach them, has it go as far, and waits until it has, for
    /// [`START_WAIT`] at most. *Why:* the reads of a player starting a track come faster than
    /// decoding; each decoding its own few frames used the decoder threads poorly and took
    /// turns with the decode-ahead's slices for the open file's demultiplexer.
    fn wait_for_start(&self, engine: &Engine, k0: u32, k1: u32) {
        let mut st = self.start.lock().unwrap_or_else(|p| p.into_inner());
        let lead = start_lead(engine);
        // A read far past what it was asked for (a seek) decodes its own frames.
        if !st.active
            || !st.running
            || k1 > st.end
            || k1 <= st.done
            || k0 > st.target.max(st.next).saturating_add(lead)
        {
            return;
        }
        // The reader is ahead of decoding, as a player filling its buffer is: whole slices up to
        // the end, not a read's few frames at a time behind it.
        st.target = st.end;
        self.start_more.notify_all();
        let deadline = std::time::Instant::now() + START_WAIT;
        while st.active && st.running && st.done < k1 {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return;
            }
            st = self.start_more.wait_timeout(st, left).unwrap_or_else(|p| p.into_inner()).0;
        }
    }

    /// Plans the start-of-track decode-ahead at a read of frames `[k0, k1)` (#481): starts it
    /// at the first read in order past the head window (#78) of a reader whose first read of
    /// audio was of the start, unless another reader plays (#363); extends it at the next reads
    /// in order, and stops it at a jump. `None` if it does not run (the read-ahead plans as
    /// before), else what to spawn (`Some(None)`: it runs, or was planned a moment ago).
    fn plan_start(
        &self,
        engine: &Engine,
        s: &mut Seq,
        sequential: bool,
        k0: u32,
        k1: u32,
    ) -> Option<Option<ReadAhead>> {
        let mut st = self.start.lock().unwrap_or_else(|p| p.into_inner());
        let end =
            || self.file.start_ahead_frames(engine.cache().cap(), start_lead(engine), slice_frames(engine));
        if s.audio_reads == 1 {
            // Not only from frame 0 (#456): a player's earlier opens of the file (probes reading
            // its head) leave that head in the kernel's page cache (`FOPEN_KEEP_CACHE`), so the
            // reads that reach sacdfs when it plays start where they ended, 1.5 MiB in.
            let end = end();
            st.eligible = k0 < end;
            st.from = k0;
            if !st.eligible {
                crate::trace_line!(
                    "start-ahead r={} no: first audio read at frame {k0}, its end is {end}",
                    self.id
                );
            }
            return None;
        }
        if st.eligible {
            // Not within a scanner's read of the head (#78), which holds the kernel's read-ahead
            // of the first read (#32): a tagger or a library import reads it and leaves. A reader
            // that began past the start (#456): not within the kernel's read-ahead of its first
            // read, nor within the file's head, which a scanner reads with part of it cached.
            let head = s.started.is_some_and(|(_, at)| {
                if st.from == 0 {
                    s.max_end <= at.saturating_add(HEAD_WINDOW.max(KERNEL_FIRST_WINDOW))
                } else {
                    s.max_end <= at.saturating_add(KERNEL_FIRST_WINDOW)
                        || s.max_end <= self.file.header_len().saturating_add(HEAD_WINDOW)
                }
            });
            if !sequential {
                st.eligible = false;
                crate::trace_line!("start-ahead r={} no: a jump to frame {k0} before it started", self.id);
            }
            if !sequential || head {
                return None;
            }
            st.eligible = false;
            // Beside a player, a reader not yet judged reads ahead little (#363): a copy starting
            // there looks the same as a player.
            let end = end();
            if k1 >= end {
                crate::trace_line!("start-ahead r={} no: read at frame {k1}, its end is {end}", self.id);
                return None;
            }
            if engine.traffic().players.any_but(self.id, std::time::Instant::now()) {
                crate::trace_line!("start-ahead r={} no: another reader plays", self.id);
                return None;
            }
            *st = StartAhead { active: true, next: k1, end, ..StartAhead::default() };
            crate::trace_line!("start-ahead r={} start: frames {k1}-{end}", self.id);
        } else if !st.active {
            // Done or stopped: the read-ahead goes on from where it got.
            let done = std::mem::take(&mut st.done);
            if done > 0 {
                s.ahead_until = s.ahead_until.max(done);
                s.after_start = true;
            }
            return None;
        } else if !sequential {
            // A jump. *Why not stop it for a reader for throughput (`cp`) too, which reads with
            // a longer window:* a local player filling its buffer reads 4 MiB faster than 1.5
            // times real time and counts as one; its next read waited for that whole window.
            st.active = false;
            crate::trace_line!("start-ahead r={} stopping: a jump to frame {k0}", self.id);
            drop(st);
            self.start_more.notify_all();
            return None;
        }
        let lead = start_lead(engine);
        st.target = st.target.max(st.end.min(k1.saturating_add(lead)));
        self.start_more.notify_all();
        // Planned again if a plan has not started its job within START_IDLE: it was dropped.
        let now = std::time::Instant::now();
        if st.running || st.planned.is_some_and(|t| now.saturating_duration_since(t) < START_IDLE) {
            return Some(None);
        }
        st.planned = Some(now);
        Some(Some(ReadAhead { first: st.next, end: st.target, start: true, sliced: true }))
    }

    /// The open file's demultiplexer, for a read: counted while it waits ([`Reader::let_reads_go_first`]).
    fn lock_stream(&self, for_read: bool) -> std::sync::MutexGuard<'_, Stream> {
        if !for_read {
            return self.stream.lock().unwrap_or_else(|p| p.into_inner());
        }
        self.stream_waits.fetch_add(1, Ordering::Relaxed);
        let g = self.stream.lock().unwrap_or_else(|p| p.into_inner());
        self.stream_waits.fetch_sub(1, Ordering::Relaxed);
        g
    }

    /// Before a slice of read-ahead: waits, for [`YIELD_MAX`] at most, until no read of this file
    /// waits for its demultiplexer. *Why:* the lock is not fair, so a job of slices took it again
    /// at once after each one, and a read behind it waited for the whole window (#481).
    fn let_reads_go_first(&self) {
        let start = std::time::Instant::now();
        while self.stream_waits.load(Ordering::Relaxed) > 0 && start.elapsed() < YIELD_MAX {
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    }

    /// Before decoding: a reader for throughput lets players go first, and a player says it is
    /// decoding, until the guard drops.
    fn before_decoding<'a>(&self, engine: &'a Engine) -> Option<Decoding<'a>> {
        if self.throughput.load(Ordering::Relaxed) {
            engine.traffic().yield_to_players();
            None
        } else if self.player.load(Ordering::Relaxed) {
            Some(engine.traffic().decoding())
        } else {
            None
        }
    }

    /// Whether frames `[k0, k1)` of the file are all in the cache (for the trace only).
    fn in_cache(&self, engine: &Engine, k0: u32, k1: u32) -> bool {
        let f = &self.file;
        let (t0, t1) = (f.start_frame + k0, f.start_frame + k1);
        match f.audio {
            Audio::Dsf(_) => engine.cached(&f.disc, f.area, t0, t1).is_some(),
            Audio::Wav(_) => engine.cached_pcm(&f.disc, f.area, t0, t1).is_some(),
        }
    }

    /// Frames `[k0, k1)` of the file, for a read (`for_read`) or for read-ahead.
    fn decoded(
        &self,
        engine: &Engine,
        k0: u32,
        k1: u32,
        for_read: bool,
    ) -> Result<Vec<Arc<Frame>>, ReadError> {
        let frames = self.decoded_frames(engine, k0, k1, for_read)?;
        // A read of the head or the last frame: keep those frames (#456).
        if for_read {
            let f = &self.file;
            let head = ((HEAD_KEEP / (f.byte_rate() as u64 / 75).max(1)) as u32).min(f.layout.frames);
            let pcm = matches!(f.audio, Audio::Wav(_));
            if k0 < head {
                engine.keep_heads(&f.disc, f.area, pcm, f.start_frame + k0, f.start_frame + k1.min(head));
            }
            let last = f.layout.frames.saturating_sub(1);
            if k1 > last && k1 > head {
                engine.keep_heads(&f.disc, f.area, pcm, f.start_frame + k0.max(last), f.start_frame + k1);
            }
        }
        Ok(frames)
    }

    fn decoded_frames(
        &self,
        engine: &Engine,
        k0: u32,
        k1: u32,
        for_read: bool,
    ) -> Result<Vec<Arc<Frame>>, ReadError> {
        let f = &self.file;
        let (t0, t1) = (f.start_frame + k0, f.start_frame + k1);
        match f.audio {
            Audio::Dsf(_) => {
                if let Some(frames) = engine.cached(&f.disc, f.area, t0, t1) {
                    return Ok(frames);
                }
                let _decoding = self.before_decoding(engine);
                let mut stream = self.lock_stream(for_read);
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
                let mut stream = self.lock_stream(for_read);
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

    /// The reader's pace since it last jumped: [`Pace::Fast`] after at least [`FAST_AFTER`]
    /// bytes at more than [`PLAYER_FACTOR`] times the file's byte rate, and, behind a network
    /// client, [`PACE_LEAD`] ahead of playing time; [`Pace::Unsure`] if it is behind a network
    /// client and only the first holds. *Why since it last jumped:* a player that seeks has a
    /// new pace to measure. *Why the bytes it read, but no further than it got:* a player that
    /// seeks forward has not read what it skipped (#271), and one whose client reads a range
    /// twice has not got further.
    fn pace(&self, s: &Seq, now: std::time::Instant) -> Pace {
        let Some((t, from)) = s.pace_from else { return Pace::Slow };
        let secs = now.saturating_duration_since(t).as_secs_f64();
        let read = s.paced.min(s.pace_end.saturating_sub(from));
        let rate = self.file.byte_rate();
        if !faster_than_a_player(read, secs, rate) {
            Pace::Slow
        } else if !s.networked || read as f64 > rate * secs + PACE_LEAD as f64 {
            Pace::Fast
        } else {
            Pace::Unsure
        }
    }

    /// Whether the reader has read long enough to be judged a player: [`PLAYER_AFTER`] since its
    /// first read of audio, and either [`FAST_AFTER`] since it last jumped or, reading slower,
    /// as long as [`FAST_AFTER`] takes at [`PLAYER_FACTOR`] times the file's byte rate (0.9 s
    /// for a 6-channel WAV file, 4 s for a stereo DSF file). *Why not 0.5 s alone (#362):* a
    /// reader not yet judged reads ahead little beside a player ([`UNJUDGED_WINDOW`]), so on a
    /// busy server copies starting beside a player had read only 1.7 to 3.4 MiB after 0.5 s,
    /// 1.1 to 2.1 times playing time, as the player had (1.3 times with its client's
    /// read-ahead). Each then counted as a player with a 2 s window until it reached 4 MiB, and
    /// the player beside four of them waited up to 0.5 s. *Why the time since the first read:* a
    /// player that seeks is judged again by its pace, but it has been playing.
    fn judged(&self, s: &Seq, now: std::time::Instant) -> bool {
        let Some((t, _)) = s.started else { return false };
        let secs = now.saturating_duration_since(t);
        secs >= PLAYER_AFTER
            && (s.paced >= FAST_AFTER
                || secs.as_secs_f64() * PLAYER_FACTOR * self.file.byte_rate() >= FAST_AFTER as f64)
    }

    fn plan_read_ahead(&self, engine: &Engine, offset: u64, end: u64, k0: u32, k1: u32) -> Option<ReadAhead> {
        let mut s = self.seq.lock().unwrap_or_else(|p| p.into_inner());
        // A jump to the file's last frame is a look at what follows the audio, as a player
        // reads a DSF file's tag before it plays (#456): the reads go on from where they were.
        // Counted, it was a jump, and no later read was in order with it: no read-ahead at all.
        if k0 > 0 && k1 >= self.file.layout.frames && offset > s.max_end.saturating_add(SEQ_GAP_AHEAD) {
            return None;
        }
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
        // A jump, for the reader's pace, is a read far from where it got to: farther than an
        // SMB server's concurrent reads land out of order (#271).
        let jumped = s.pace_from.is_none()
            || offset > s.max_end.saturating_add(PACE_JUMP)
            || offset.saturating_add(PACE_JUMP) < s.max_end;
        if !jumped && offset.saturating_add(NETWORK_REORDER) < s.pace_end {
            s.networked = true;
        }
        s.max_end = s.max_end.max(end);
        if !sequential {
            s.ahead_until = 0;
            s.slicing = false;
        }
        if jumped {
            s.paused = false;
            s.given_up = k0;
            s.pace_from = Some((std::time::Instant::now(), offset));
            s.paced = 0;
            s.pace_end = offset;
        }
        s.paced = s.paced.saturating_add(end.saturating_sub(offset));
        s.pace_end = s.pace_end.max(end);
        // A client's reads may land before the first one: measure from the lowest.
        if let Some((_, from)) = s.pace_from.as_mut() {
            *from = (*from).min(offset);
        }
        if self.paused() {
            s.paused = true;
        }
        // Whether a file server opened it matters only from FAST_AFTER on ([`Reader::pace`]
        // and the slow rule below), so it is asked then, once (#403).
        if s.paced >= FAST_AFTER {
            if let Some((pid, is_server)) = s.opener.take() {
                s.networked = s.networked || is_server(pid);
            }
        }
        let now = std::time::Instant::now();
        let traffic = engine.traffic();
        // An unsure reader behind a network client may be a copy starting or a player whose
        // client read ahead: it plays only if no player has been reading since before it (#271).
        // So does a slow one whose client has not paused, once it has read enough to judge: on a
        // server too slow for all, a copy reads no faster than a player (#357).
        let after_a_player = || s.pace_from.is_some_and(|(t, _)| traffic.players.any_before(self.id, t, now));
        let throughput = match self.pace(&s, now) {
            Pace::Slow => s.networked && !s.paused && s.paced >= FAST_AFTER && after_a_player(),
            Pace::Fast => true,
            Pace::Unsure => after_a_player(),
        };
        self.counted.get_or_init(|| traffic.clone());
        self.throughput.store(throughput, Ordering::Relaxed);
        let player = !throughput && self.judged(&s, now);
        self.player.store(player, Ordering::Relaxed);
        if !throughput {
            traffic.fast.forget(self.id);
        }
        if player {
            let since = s.pace_from.map_or(now, |(t, _)| t);
            traffic.players.note_since(self.id, now, since, Some(self.reads.clone()));
        } else {
            traffic.players.forget(self.id);
        }
        if let Some(start) = self.plan_start(engine, &mut s, sequential, k0, k1) {
            return start;
        }
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
            let n = traffic.fast.note(self.id, now);
            let someone_plays = traffic.players.any(now);
            let f = &self.file;
            let margin = (GIVE_UP_MARGIN / (f.byte_rate() as u64 / 75).max(1)) as u32;
            let behind = k0.saturating_sub(margin);
            if s.given_up < behind {
                if someone_plays {
                    engine.give_up(
                        &f.disc,
                        f.area,
                        f.format(),
                        f.start_frame + s.given_up,
                        f.start_frame + behind,
                    );
                }
                s.given_up = behind;
            }
            let own = if self.fast(&s) {
                self.file.read_ahead_frames(engine.threads(), engine.cache().cap(), n)
            } else {
                READ_AHEAD_FRAMES
            };
            if someone_plays { own.min(self.file.shared_frames(engine.cache().cap(), n)) } else { own }
        } else if !player && traffic.players.any_but(self.id, now) {
            UNJUDGED_WINDOW
        } else {
            READ_AHEAD_FRAMES
        };
        // While a window in slices is decoded, no other (#456): one decoded at once beside it
        // took the decoder threads and the demultiplexer, and the player reading right behind
        // the slices waited for that whole window, a second twice on a 4-thread Atom.
        if self.sliced_running.load(Ordering::Relaxed)
            || s.sliced_planned.is_some_and(|t| now.saturating_duration_since(t) < START_IDLE)
        {
            return None;
        }
        if k1 >= frames || s.ahead_until >= frames.min(k1 + window / 2) {
            return None;
        }
        // The first window after the start-of-track decode-ahead goes in slices too (#481): a
        // player still filling its buffer reads right behind it, and would wait for the whole
        // window, a second on a slow CPU. So does the next while the reader is still within a
        // slice of what is decoded and reads at less than SLICE_PACE_MAX times real time (#456):
        // on a CPU that decodes little faster than real time, a player stays that close for as
        // long as it fills its buffer. *Why the pace:* a copy on a fast CPU is always that close
        // too, and in slices a whole 6-channel file read 9 % slower. *Why not "not for
        // throughput":* a player filling its buffer at 1.5 times real time counts as one; and a
        // bound in frames only moved the wait to where it ended.
        let slow = s.pace_from.is_some_and(|(t, _)| {
            let secs = now.saturating_duration_since(t).as_secs_f64().max(0.5);
            (s.paced as f64) < SLICE_PACE_MAX * self.file.byte_rate() * secs
        });
        let sliced = std::mem::take(&mut s.after_start)
            || s.slicing && slow && k1.saturating_add(slice_frames(engine)) >= s.ahead_until;
        s.slicing = sliced;
        let start = s.ahead_until.max(k1);
        let stop = frames.min(k1 + window);
        s.ahead_until = stop;
        if sliced && start < stop {
            s.sliced_planned = Some(now);
        }
        (start < stop).then_some(ReadAhead { first: start, end: stop, start: false, sliced })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

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

    fn longest_dsf(tree: &crate::tree::Tree) -> Arc<TrackFile> {
        tree.all_files()
            .into_iter()
            .filter(|(p, _)| p.extension().is_some_and(|e| e == "dsf"))
            .map(|(_, f)| f)
            .max_by_key(|f| f.layout.frames)
            .expect("a DSF file in the corpus")
    }

    fn corpus() -> crate::tree::Tree {
        let images = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/images");
        crate::tree::Tree::with_options(
            crate::tree::SourceDir::scan(&images).unwrap(),
            crate::disc::Tuning::default(),
            crate::tree::Output::default(),
        )
    }

    /// A reader's state after `FAST_AFTER` bytes, begun `ago`. No file of the corpus is that
    /// long, so the test sets it instead of reading it.
    fn has_read(file: &Arc<TrackFile>, ago: std::time::Duration) -> Reader {
        let reader = Reader::new(file.clone());
        {
            let mut s = reader.seq.lock().unwrap();
            s.audio_reads = 1;
            s.started = Some((std::time::Instant::now() - ago, 0));
            s.max_end = FAST_AFTER;
            s.pace_from = s.started;
            s.paced = FAST_AFTER;
            s.pace_end = FAST_AFTER;
        }
        reader
    }

    /// A reader far ahead of real time gets the long window and shares it with the others as
    /// long as it reads; one at the file's rate keeps the 2 s window and doesn't count.
    #[test]
    fn only_a_fast_reader_gets_the_long_window() {
        let tree = corpus();
        let f = longest_dsf(&tree);
        let engine = Engine::new(32, 32 << 20).unwrap();
        let (cap, frames) = (engine.cache().cap(), f.layout.frames);
        let plan = |r: &Reader| r.plan_read_ahead(&engine, FAST_AFTER, FAST_AFTER + 4096, 0, 0).unwrap();
        let t0 = std::time::Instant::now();

        let player = has_read(&f, std::time::Duration::from_secs(3600));
        assert_eq!(plan(&player).end, frames.min(READ_AHEAD_FRAMES));
        assert_eq!(engine.traffic().fast.note(u64::MAX, t0), 1, "a player counted as fast");
        engine.traffic().fast.forget(u64::MAX);
        // While a player is about, readers for throughput share less (#230); this test is of the
        // windows with none.
        drop(player);

        let fast = has_read(&f, std::time::Duration::ZERO);
        let ahead = plan(&fast);
        assert_eq!((ahead.first, ahead.end), (0, frames.min(f.read_ahead_frames(32, cap, 1))));
        let second = has_read(&f, std::time::Duration::ZERO);
        assert_eq!(plan(&second).end, frames.min(f.read_ahead_frames(32, cap, 2)));

        let now = std::time::Instant::now();
        assert_eq!(engine.traffic().fast.note(u64::MAX, now), 3, "both readers and this one");
        drop(fast);
        assert_eq!(engine.traffic().fast.note(u64::MAX, now), 2, "closed readers stop counting");
        drop(second);
        assert_eq!(engine.traffic().fast.note(u64::MAX, now), 1);
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

    /// A copy over SMB reads out of order, several MiB either way, and still reads for
    /// throughput; a real jump measures its pace again (#271).
    #[test]
    fn reads_out_of_order_keep_the_pace() {
        let tree = corpus();
        let f = longest_dsf(&tree);
        let engine = Engine::new(4, 32 << 20).unwrap();
        let reader = Reader::new(f.clone());
        let read = |off: u64| {
            reader.plan_read_ahead(&engine, off, off + (128 << 10), 0, 1);
        };
        // Samba's concurrent 4 MiB reads, as sacdfs saw them: each 4 MiB block in 128 KiB
        // pieces, blocks arriving out of order by up to 4 blocks.
        let block = 4u64 << 20;
        for b in [0u64, 2, 1, 3, 5, 4, 7, 6, 8, 11, 9, 10] {
            for i in 0..32 {
                read(b * block + i * (128 << 10));
            }
        }
        let s = reader.seq.lock().unwrap();
        assert_eq!(s.pace_from.map(|(_, o)| o), Some(0), "no jump within the client's reads");
        assert_eq!(s.paced, 12 * block);
        drop(s);
        assert!(reader.throughput.load(Ordering::Relaxed), "a copy reads for throughput");

        read(12 * block + PACE_JUMP + 1);
        let s = reader.seq.lock().unwrap();
        assert_eq!(s.pace_from.map(|(_, o)| o), Some(12 * block + PACE_JUMP + 1), "a jump");
        assert_eq!(s.paced, 128 << 10);
        drop(s);
        assert!(!reader.throughput.load(Ordering::Relaxed), "too early to tell after a jump");
    }

    /// Behind a network client, a player whose client fetched ahead in a burst still plays; a
    /// copy reads for throughput once it is far enough ahead of playing time (#271).
    #[test]
    fn behind_a_network_client_a_player_is_not_a_copy() {
        let tree = corpus();
        let f = longest_dsf(&tree);
        let rate = f.byte_rate();
        let reader = Reader::new(f.clone());
        let now = std::time::Instant::now();
        let secs = 2.0;
        let seq = |read: u64, end: u64, networked: bool| Seq {
            pace_from: Some((now - std::time::Duration::from_secs_f64(secs), 0)),
            paced: read,
            pace_end: end,
            networked,
            ..Seq::default()
        };
        let playing = (rate * secs) as u64;
        // Two seconds of playing and the 16 MiB a kernel SMB client keeps ahead of it.
        let player = playing + (16 << 20);
        assert_eq!(reader.pace(&seq(player, player, false), now), Pace::Fast, "locally, a copy");
        assert_eq!(reader.pace(&seq(player, player, true), now), Pace::Unsure, "behind a client, unsure");
        let copy = playing + PACE_LEAD + (1 << 20);
        assert_eq!(reader.pace(&seq(copy, copy, true), now), Pace::Fast, "far ahead: a copy");
        // Ranges read twice don't take a reader further.
        assert_eq!(reader.pace(&seq(copy, player, true), now), Pace::Unsure, "read twice");
        assert_eq!(reader.pace(&seq(1 << 20, 1 << 20, true), now), Pace::Slow, "a player");

        // Reads in order, as through the kernel's FUSE read-ahead, are not a network client's;
        // one landing 3 MiB behind is.
        let engine = Engine::new(4, 32 << 20).unwrap();
        let read = |off: u64| {
            reader.plan_read_ahead(&engine, off, off + (128 << 10), 0, 1);
        };
        for i in 0..64 {
            read(i * (128 << 10));
        }
        assert!(!reader.seq.lock().unwrap().networked);
        read(5 << 20);
        assert!(reader.seq.lock().unwrap().networked);
    }

    /// An unsure reader behind a network client plays only if no player has been reading since
    /// before it (#271).
    #[test]
    fn an_unsure_reader_lets_an_earlier_player_go_first() {
        let tree = corpus();
        let f = longest_dsf(&tree);
        let engine = Engine::new(4, 32 << 20).unwrap();
        let copy = Reader::new(f.clone());
        let read = |off: u64| {
            copy.plan_read_ahead(&engine, off, off + (128 << 10), 0, 1);
        };
        // Two of Samba's concurrent 4 MiB reads, the second first: 8 MiB at once, behind a
        // network client, not far ahead of playing time.
        for b in [1u64, 0] {
            for i in 0..32 {
                read(b * (4 << 20) + i * (128 << 10));
            }
        }
        let since = {
            let s = copy.seq.lock().unwrap();
            assert!(s.networked);
            assert_eq!(s.pace_from.map(|(_, o)| o), Some(0), "measured from the lowest byte");
            assert_eq!(copy.pace(&s, std::time::Instant::now()), Pace::Unsure);
            s.pace_from.unwrap().0
        };
        assert!(!copy.throughput.load(Ordering::Relaxed), "no player to let go first");

        let traffic = engine.traffic();
        let now = std::time::Instant::now();
        traffic.players.note_since(u64::MAX - 1, now, now, None);
        read(8 << 20);
        assert!(!copy.throughput.load(Ordering::Relaxed), "a player since after it: not held back");

        traffic.players.note_since(u64::MAX, now, since - std::time::Duration::from_secs(5), None);
        read((8 << 20) + (128 << 10));
        assert!(copy.throughput.load(Ordering::Relaxed), "a player since before it goes first");
    }

    /// Behind a network client, a reader that reads no faster than a player still lets an
    /// earlier player go first, until its client pauses (#357).
    #[test]
    fn a_slow_copy_lets_an_earlier_player_go_first_until_its_client_pauses() {
        let tree = corpus();
        let f = longest_dsf(&tree);
        let engine = Engine::new(4, 32 << 20).unwrap();
        let copy = Arc::new(Reader::new(f.clone()));
        let read = |off: u64| {
            let _arrival = copy.arrived();
            copy.plan_read_ahead(&engine, off, off + (128 << 10), 0, 1);
        };
        // The copy, behind a network client (a read landing 4 MiB behind another), and so
        // slow that it reads no faster than a player.
        let slow = || {
            let mut s = copy.seq.lock().unwrap();
            s.pace_from = Some((std::time::Instant::now() - std::time::Duration::from_secs(30), 0));
            s.started = s.pace_from;
            s.paced = s.paced.max(FAST_AFTER);
            assert_eq!(copy.pace(&s, std::time::Instant::now()), Pace::Slow);
        };
        read(4 << 20);
        read(0);
        assert!(copy.seq.lock().unwrap().networked);
        slow();
        read(128 << 10);
        assert!(copy.player.load(Ordering::Relaxed), "no player before it: it plays");

        // A player since before it: the copy lets it go first, however slowly it reads.
        let traffic = engine.traffic();
        let player = u64::MAX;
        let early = std::time::Instant::now() - std::time::Duration::from_secs(60);
        traffic.players.note_since(player, std::time::Instant::now(), early, None);
        slow();
        read(256 << 10);
        assert!(copy.throughput.load(Ordering::Relaxed), "a player before it goes first");
        assert!(!copy.player.load(Ordering::Relaxed));

        // Its client pauses: what is behind it is paced by playback, so it plays.
        copy.flight.lock().unwrap().idle_from = Some(std::time::Instant::now() - CLIENT_PAUSE);
        slow();
        read(384 << 10);
        assert!(copy.player.load(Ordering::Relaxed), "paused: a player");
        slow();
        read(512 << 10);
        assert!(copy.player.load(Ordering::Relaxed), "until it jumps");

        // A reader starting behind a network client is not judged before it read 4 MiB.
        let starting = Arc::new(Reader::new(f.clone()));
        {
            let mut s = starting.seq.lock().unwrap();
            s.started = Some((std::time::Instant::now() - judged_after(&f), 0));
        }
        for off in [2u64 << 20, 0] {
            let _arrival = starting.arrived();
            starting.plan_read_ahead(&engine, off, off + (128 << 10), 0, 1);
        }
        assert!(starting.seq.lock().unwrap().networked);
        assert!(starting.player.load(Ordering::Relaxed), "too early to tell: a player");
    }

    /// A reader opened by a network file server's process is behind a network client whatever
    /// the order of its reads (#362). Whose process it is, is asked once, and only once the
    /// reader has read enough for it to matter (#403).
    #[test]
    fn a_reader_opened_by_a_network_server_is_networked_once_it_matters() {
        static ASKED: AtomicU32 = AtomicU32::new(0);
        fn server(pid: u32) -> bool {
            ASKED.fetch_add(1, Ordering::Relaxed);
            pid == 7
        }
        let tree = corpus();
        let f = longest_dsf(&tree);
        let engine = Engine::new(4, 32 << 20).unwrap();
        let local = Reader::opened_by(f.clone(), 8, server);
        let served = Reader::opened_by(f.clone(), 7, server);
        let read = |r: &Reader, i: u64| {
            r.plan_read_ahead(&engine, i * (128 << 10), (i + 1) * (128 << 10), 0, 1);
        };
        // A scanner's short read: nobody asks.
        for r in [&local, &served] {
            for i in 0..8 {
                read(r, i);
            }
        }
        assert_eq!(ASKED.load(Ordering::Relaxed), 0, "not asked below FAST_AFTER");
        assert!(!served.seq.lock().unwrap().networked);
        // In order, past FAST_AFTER: asked once per reader.
        for r in [&local, &served] {
            for i in 8..48 {
                read(r, i);
            }
        }
        assert_eq!(ASKED.load(Ordering::Relaxed), 2, "asked once each");
        assert!(!local.seq.lock().unwrap().networked, "in order, opened locally");
        assert!(served.seq.lock().unwrap().networked, "in order, opened by a file server");
    }

    /// A reader not yet judged reads ahead only a few frames while someone else plays (#363).
    #[test]
    fn an_unjudged_reader_reads_ahead_little_beside_a_player() {
        let tree = corpus();
        let f = longest_dsf(&tree);
        let engine = Engine::new(4, 32 << 20).unwrap();
        let reader = Reader::new(f.clone());
        let window =
            |off: u64| reader.plan_read_ahead(&engine, off, off + (128 << 10), 0, 1).map(|a| a.end - a.first);
        // Past the head window, in order, at once: not yet judged. The first read is not of the
        // start, so that no start-of-track decode-ahead (#481) plans instead.
        let past = HEAD_WINDOW + (128 << 10);
        reader.plan_read_ahead(&engine, 0, 128 << 10, 1, 2);
        let alone = window(past).expect("read-ahead with no player");
        assert_eq!(alone, READ_AHEAD_FRAMES.min(f.layout.frames - 1));

        let player = u64::MAX;
        let now = std::time::Instant::now();
        engine.traffic().players.note_since(player, now, now, None);
        let fresh = Reader::new(f.clone());
        fresh.plan_read_ahead(&engine, 0, 128 << 10, 0, 1);
        let ahead = fresh.plan_read_ahead(&engine, past, past + (128 << 10), 0, 1).unwrap();
        assert_eq!(ahead.end - ahead.first, UNJUDGED_WINDOW.min(f.layout.frames - 1), "beside a player");
        assert!(!fresh.player.load(Ordering::Relaxed));

        // Once it is a player, it gets a player's window back.
        fresh.seq.lock().unwrap().started = Some((now - judged_after(&f), 0));
        fresh.seq.lock().unwrap().ahead_until = 0;
        let ahead = fresh.plan_read_ahead(&engine, past + (128 << 10), past + (256 << 10), 0, 1).unwrap();
        assert!(fresh.player.load(Ordering::Relaxed));
        assert_eq!(ahead.end - ahead.first, READ_AHEAD_FRAMES.min(f.layout.frames - 1));
    }

    /// How long a reader of `f` that reads no faster than a player takes to be judged one.
    fn judged_after(f: &TrackFile) -> std::time::Duration {
        let secs = FAST_AFTER as f64 / (PLAYER_FACTOR * f.byte_rate());
        PLAYER_AFTER.max(std::time::Duration::from_secs_f64(secs)) + std::time::Duration::from_millis(10)
    }

    /// A reader slower than a player is judged one only once it has read 4 MiB, or for as long
    /// as that takes at 1.5 times the file's rate; a player that seeks is judged at once (#362).
    #[test]
    fn a_reader_is_judged_a_player_on_evidence() {
        let tree = corpus();
        let f = longest_dsf(&tree);
        let rate = f.byte_rate();
        let reader = Reader::new(f.clone());
        let now = std::time::Instant::now();
        let seq = |ago: std::time::Duration, read: u64| Seq {
            started: Some((now - ago, 0)),
            pace_from: Some((now - ago, 0)),
            paced: read,
            pace_end: read,
            ..Seq::default()
        };
        let after = judged_after(&f);
        assert!(after > PLAYER_AFTER, "4 MiB at 1.5 times the rate of this file takes longer");
        // A copy limited to a few frames ahead reads about as fast as a player in its first
        // half second: not judged yet.
        let half = (rate * 0.75) as u64;
        assert_eq!(reader.pace(&seq(PLAYER_AFTER, half), now), Pace::Slow);
        assert!(!reader.judged(&seq(PLAYER_AFTER, half), now), "too early");
        assert!(!reader.judged(&seq(PLAYER_AFTER / 2, FAST_AFTER), now), "before 0.5 s");
        assert!(reader.judged(&seq(after, (rate * after.as_secs_f64()) as u64), now), "long enough");
        // After a seek its pace is measured again, but it has been reading for long.
        let mut seeked = seq(std::time::Duration::from_secs(60), 128 << 10);
        seeked.pace_from = Some((now, 100 << 20));
        assert!(reader.judged(&seeked, now), "a player that seeks");
    }

    /// A reader on the list counts while it is heard from lately or waits for a read (#357).
    #[test]
    fn a_player_waiting_for_a_read_still_counts() {
        let r = FastReaders::default();
        let t0 = std::time::Instant::now();
        let reads = Arc::new(AtomicU32::new(1));
        r.note_since(1, t0, t0, Some(reads.clone()));
        let later = t0 + FAST_READER_IDLE * 10;
        assert!(r.any(later), "waiting for a read");
        assert!(r.any_before(2, t0 + FAST_READER_IDLE, later));
        reads.store(0, Ordering::Relaxed);
        assert!(!r.any(later), "not heard from, nothing in progress");
    }

    /// Reads in progress count from their arrival until the result drops; a pause is a read
    /// arriving after none was in progress for [`CLIENT_PAUSE`].
    #[test]
    fn a_pause_is_a_time_with_no_read_in_progress() {
        let tree = corpus();
        let reader = Arc::new(Reader::new(longest_dsf(&tree)));
        let t0 = std::time::Instant::now();
        let a = reader.arrived_at(t0);
        let b = reader.arrived_at(t0);
        drop(a);
        assert_eq!(reader.reads.load(Ordering::Relaxed), 1);
        drop(reader.arrived_at(t0 + CLIENT_PAUSE * 4));
        assert!(!reader.paused(), "one was in progress all along");
        drop(b);
        assert_eq!(reader.reads.load(Ordering::Relaxed), 0);
        let idle_from = reader.flight.lock().unwrap().idle_from.unwrap();
        drop(reader.arrived_at(idle_from + CLIENT_PAUSE / 2));
        assert!(!reader.paused(), "too short a gap");
        let idle_from = reader.flight.lock().unwrap().idle_from.unwrap();
        drop(reader.arrived_at(idle_from + CLIENT_PAUSE));
        assert!(reader.paused());
        assert!(!reader.paused(), "asked once");
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
        // Linux only: on shared macOS runners the scheduler's delay alone exceeded the bound
        // (73 to 88 ms against 50), so the time there tells nothing about the wake-up.
        #[cfg(target_os = "linux")]
        assert!(waiter < YIELD_MAX, "woken when the player is done: {waiter:?}");
        #[cfg(not(target_os = "linux"))]
        let _ = waiter;
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

    fn longest(tree: &crate::tree::Tree, format: Format) -> Arc<TrackFile> {
        tree.all_files()
            .into_iter()
            .map(|(_, f)| f)
            .filter(|f| f.format() == format)
            .max_by_key(|f| f.layout.frames)
            .expect("a file of the format in the corpus")
    }

    /// The whole file, read at once by a reader of its own on an engine of its own.
    fn whole(f: &Arc<TrackFile>) -> Vec<u8> {
        let engine = Engine::new(2, 0).unwrap();
        Reader::new(f.clone()).read(&engine, 0, f.size).unwrap().0
    }

    /// No file of the corpus is longer than a scanner's head (`HEAD_WINDOW`), so the reads that
    /// start the decode-ahead are given as plans: a first read of frames `[0, 1)` at the start,
    /// then one of `[1, 2)` past the head. What it plans.
    fn plan_start_past_the_head(engine: &Engine, reader: &Reader) -> Option<ReadAhead> {
        assert!(reader.plan_read_ahead(engine, 0, 128 << 10, 0, 1).is_none());
        reader.plan_read_ahead(engine, HEAD_WINDOW, HEAD_WINDOW + (128 << 10), 1, 2)
    }

    /// The decode-ahead's end: its frames, within the file, and none if a quarter of the cache
    /// can't hold its lead and a slice.
    #[test]
    fn the_start_ahead_is_bounded_by_the_cache_and_the_file() {
        let tree = corpus();
        let f = longest(&tree, Format::Dsf);
        let frame = (f.byte_rate() / 75.0) as usize;
        assert_eq!(f.start_ahead_frames(0, 32, 32), 0, "no cache");
        assert_eq!(f.start_ahead_frames(4 * frame * 63, 32, 32), 0, "too small for lead and slice");
        assert_eq!(f.start_ahead_frames(4 * frame * 64, 32, 32), f.layout.frames.min(START_AHEAD_FRAMES));
        assert!(f.layout.frames < START_AHEAD_FRAMES, "the corpus's files end first");
    }

    /// Past the head, after a first read of the start, the next frames are decoded into the
    /// cache, and the file's bytes are what a single read gives.
    #[test]
    fn the_start_of_a_track_is_decoded_ahead() {
        let tree = corpus();
        for format in [Format::Dsf, Format::Wav] {
            let f = longest(&tree, format);
            let engine = Engine::new(2, 32 << 20).unwrap();
            let reader = Reader::new(f.clone());
            let ahead = plan_start_past_the_head(&engine, &reader).expect("planned");
            assert!(ahead.start && ahead.first == 2 && ahead.end > 2, "{format:?}: {ahead:?}");
            assert_eq!(engine.traffic().starting.load(Ordering::Relaxed), 0, "a place is taken when it runs");
            reader.read_ahead(&engine, ahead);
            assert_eq!(engine.traffic().starting.load(Ordering::Relaxed), 0, "{format:?}: done");
            // With no more reads, it goes its lead past the furthest read, waits for one and ends.
            let done = f.layout.frames.min(2 + start_lead(&engine));
            let st = reader.start.lock().unwrap();
            assert!(!st.active && !st.running, "{format:?}");
            assert_eq!(st.done, done, "{format:?}");
            drop(st);
            let (t0, t1) = (f.start_frame + 2, f.start_frame + done);
            let cached = match format {
                Format::Dsf => engine.cached(&f.disc, f.area, t0, t1),
                Format::Wav => engine.cached_pcm(&f.disc, f.area, t0, t1),
            };
            if cached.is_none() {
                let missing: Vec<u32> = (2..done)
                    .filter(|&k| {
                        let t = f.start_frame + k;
                        match format {
                            Format::Dsf => engine.cached(&f.disc, f.area, t, t + 1).is_none(),
                            Format::Wav => engine.cached_pcm(&f.disc, f.area, t, t + 1).is_none(),
                        }
                    })
                    .collect();
                let again = reader.decoded(&engine, 2, done, false).map(|v| v.len());
                panic!(
                    "{format:?}: not decoded ahead: {missing:?} of 2..{done}; decoding them now: {again:?}"
                );
            }
            // The read-ahead goes on from where it got.
            let next =
                reader.plan_read_ahead(&engine, HEAD_WINDOW + (128 << 10), HEAD_WINDOW + (256 << 10), 2, 3);
            assert!(next.is_none_or(|a| !a.start && a.first == done), "{format:?}: {next:?}");
            assert!(reader.read(&engine, 0, f.size).unwrap().0 == whole(&f), "{format:?}: the same bytes");
        }
    }

    /// A tagger (the head, then the tag), a scanner reading the head, a read of the middle first
    /// (past the decode-ahead's frames), a reader beside a player and a mount without a cache
    /// start none.
    #[test]
    fn only_a_reader_going_on_from_the_start_decodes_it_ahead() {
        let tree = corpus();
        let f = longest(&tree, Format::Dsf);
        let engine = Engine::new(2, 32 << 20).unwrap();
        let starts = |engine: &Engine, reader: &Reader, reads: &[(u64, u32)]| {
            reads.iter().any(|&(off, k)| {
                reader.plan_read_ahead(engine, off, off + (128 << 10), k, k + 1).is_some_and(|a| a.start)
            })
        };
        let head: Vec<(u64, u32)> = (0..6).map(|i| (i * (128 << 10), i as u32)).collect();
        assert!(!starts(&engine, &Reader::new(f.clone()), &head), "a scanner's head");
        let tagger = [(0, 0), (128 << 10, 1), (64 << 20, 20), (HEAD_WINDOW, 2)];
        assert!(!starts(&engine, &Reader::new(f.clone()), &tagger), "a tagger");
        // The corpus's files are shorter than the decode-ahead: past its end is past theirs.
        let end = f.layout.frames;
        assert!(
            !starts(&engine, &Reader::new(f.clone()), &[(HEAD_WINDOW, end), (HEAD_WINDOW * 2, end + 1)]),
            "the middle"
        );
        let no_cache = Engine::new(2, 0).unwrap();
        assert!(
            plan_start_past_the_head(&no_cache, &Reader::new(f.clone())).is_none_or(|a| !a.start),
            "no cache"
        );
        let now = std::time::Instant::now();
        engine.traffic().players.note_since(u64::MAX, now, now, None);
        assert!(
            plan_start_past_the_head(&engine, &Reader::new(f.clone())).is_none_or(|a| !a.start),
            "beside a player"
        );
        engine.traffic().players.forget(u64::MAX);
        assert!(
            plan_start_past_the_head(&engine, &Reader::new(f.clone())).is_some_and(|a| a.start),
            "a player"
        );
        assert_eq!(
            engine.traffic().starting.load(Ordering::Relaxed),
            0,
            "a reader dropped gives its place back"
        );
    }

    /// A player whose head is in the page cache from its earlier opens (#456): its first read
    /// reaching sacdfs is past the start, and it starts the decode-ahead once past the kernel's
    /// first window and the file's head. A first read past the decode-ahead's end, or reads that
    /// stay within the head (a scanner with part of the head cached), start none.
    #[test]
    fn a_player_whose_head_was_cached_decodes_ahead() {
        let tree = corpus();
        for format in [Format::Dsf, Format::Wav] {
            let f = longest(&tree, format);
            let engine = Engine::new(2, 32 << 20).unwrap();
            let end = f.start_ahead_frames(engine.cache().cap(), start_lead(&engine), slice_frames(&engine));
            assert!(end > 8, "{format:?}: {end}");
            // Reads of 128 KiB from `from`, the n-th of frame k (the offsets need not match the
            // frames: the corpus's files are shorter than the head).
            let plan = |r: &Reader, from: u64, n: u64, k: u32| {
                let off = from + n * (128 << 10);
                r.plan_read_ahead(&engine, off, off + (128 << 10), k, k + 1).filter(|a| a.start)
            };
            let past_head = f.header_len() + HEAD_WINDOW + (512 << 10);

            let player = Reader::new(f.clone());
            assert!(plan(&player, past_head, 0, 2).is_none(), "{format:?}: the first read");
            assert!(plan(&player, past_head, 1, 3).is_none(), "{format:?}: the kernel's first window");
            let ahead = plan(&player, past_head, 2, 4).expect("started");
            assert_eq!((ahead.first, player.start.lock().unwrap().end), (5, end), "{format:?}");

            let late = Reader::new(f.clone());
            assert!(
                (0..8).all(|n| plan(&late, past_head, n, end + n as u32).is_none()),
                "{format:?}: past its end"
            );

            // A scanner reading the head, its first 512 KiB cached.
            let scanner = Reader::new(f.clone());
            let from = f.header_len() + (512 << 10);
            assert!((0..4).all(|n| plan(&scanner, from, n, 2 + n as u32).is_none()), "{format:?}: the head");
            // Past it, it reads more than a head, and starts.
            assert!(plan(&scanner, from, 4, 6).is_some(), "{format:?}: past the head");
        }
    }

    /// After the start-of-track decode-ahead, read-ahead goes one window in slices at a time
    /// while the reader is within a slice of what is decoded, as a player is on a CPU that
    /// decodes little faster than real time (#456); further behind, in one batch as before.
    #[test]
    fn the_windows_after_the_start_ahead_go_in_slices_one_at_a_time() {
        let tree = corpus();
        let f = longest(&tree, Format::Wav);
        // The corpus's files are a few frames long: the same file, longer (nothing is decoded).
        let f = Arc::new(TrackFile {
            disc: f.disc.clone(),
            area: f.area,
            number: f.number,
            start_frame: f.start_frame,
            layout: Layout { frames: 75 * 60, ..f.layout },
            area_end: f.area_end,
            audio: f.audio,
            dsf: None,
            header: f.header.clone(),
            trailer: f.trailer.clone(),
            tag: f.tag.clone(),
            size: f.size,
            tags: f.tags,
            filled: f.filled,
            warned: AtomicBool::new(false),
            warned_clips: AtomicBool::new(false),
        });
        let engine = Engine::new(2, 32 << 20).unwrap();
        let slice = slice_frames(&engine);
        let r = Reader::new(f);
        // Reads of 128 KiB in order (the frames need not match the offsets).
        let n = std::cell::Cell::new(0u64);
        let read = |k: u32| {
            let off = n.get() * (128 << 10);
            n.set(n.get() + 1);
            r.plan_read_ahead(&engine, off, off + (128 << 10), k, k + 1)
        };
        assert!(read(0).is_none());
        // No start-ahead here: where it got to is set below.
        r.start.lock().unwrap().eligible = false;
        while (n.get() + 1) * (128 << 10) <= HEAD_WINDOW {
            assert!(read(1).is_none(), "the head");
        }
        {
            let mut s = r.seq.lock().unwrap();
            s.after_start = true;
            s.ahead_until = slice;
        }
        let a = read(slice - 1).expect("the window after the start-ahead");
        assert!(a.sliced && a.first == slice, "{a:?}");
        // A read in its second half, which plans the next window as a rule: not while it is
        // planned or its job runs.
        let mid = a.end - 10;
        assert!(read(mid).is_none(), "planned, its job not started");
        r.seq.lock().unwrap().sliced_planned = None;
        r.sliced_running.store(true, Ordering::Relaxed);
        assert!(read(mid).is_none(), "its job runs");
        r.sliced_running.store(false, Ordering::Relaxed);

        // Done, and the reader within a slice of it: the next window in slices too.
        let b = read(mid).expect("the next window");
        assert!(b.sliced && b.first == a.end, "{b:?}");
        r.seq.lock().unwrap().sliced_planned = None;

        // Further behind: in one batch, and so on after that.
        let c = read(b.end - 3 * slice).expect("a window further ahead");
        assert!(!c.sliced && c.first == b.end, "{c:?}");
        let d = read(c.end - 5).expect("the window after");
        assert!(!d.sliced, "{d:?}");

        // Close behind again, but reading far faster than real time, as a copy does: in one batch.
        let k = {
            let mut s = r.seq.lock().unwrap();
            s.slicing = true;
            s.pace_from = Some((std::time::Instant::now(), 0));
            s.paced = 1 << 30;
            s.ahead_until = d.end;
            d.end
        };
        let e = read(k - 4).expect("a copy's window");
        assert!(!e.sliced, "{e:?}");
    }

    /// A read of a DSF file's last frame after a jump (a player reading the tag before it
    /// plays, #456) is not a jump: the reads after it are in order and start the decode-ahead;
    /// nor does it stop one that runs, or count as the first read of audio.
    #[test]
    fn a_look_at_the_tag_is_not_a_jump() {
        let tree = corpus();
        let f = longest(&tree, Format::Dsf);
        let engine = Engine::new(2, 32 << 20).unwrap();
        let (t0, t1) = f.frames_for(f.size - 412, f.size);
        assert!(t0 < t1 && t1 == f.layout.frames, "the last bytes hold the last frame: {t0}..{t1}");
        // The corpus's files are shorter than a jump: the tag read is placed further on.
        let tag = 64 << 20;
        let peek = |r: &Reader| r.plan_read_ahead(&engine, tag, tag + 412, t0, t1);

        let player = Reader::new(f.clone());
        assert!(player.plan_read_ahead(&engine, 0, 128 << 10, 0, 1).is_none());
        assert!(peek(&player).is_none());
        let ahead = player.plan_read_ahead(&engine, HEAD_WINDOW, HEAD_WINDOW + (128 << 10), 1, 2);
        assert!(ahead.is_some_and(|a| a.start), "head, tag, then on: {ahead:?}");
        assert!(peek(&player).is_none());
        assert!(player.start.lock().unwrap().active, "the tag read stopped it");

        let tag_first = Reader::new(f.clone());
        assert!(peek(&tag_first).is_none());
        assert!(plan_start_past_the_head(&engine, &tag_first).is_some_and(|a| a.start), "tag first");

        // Not a jump to the last frame: the reads after it are not in order.
        let jumped = Reader::new(f.clone());
        assert!(jumped.plan_read_ahead(&engine, 0, 128 << 10, 0, 1).is_none());
        assert!(jumped.plan_read_ahead(&engine, tag, tag + 4096, t0 - 1, t0).is_none());
        let ahead = jumped.plan_read_ahead(&engine, HEAD_WINDOW, HEAD_WINDOW + (128 << 10), 1, 2);
        assert!(ahead.is_none(), "a jump elsewhere: {ahead:?}");
    }

    /// A reader that closes or jumps stops its decode-ahead before its next slice; and only so
    /// many run at once.
    #[test]
    fn the_start_ahead_stops_at_a_close_or_a_jump_and_is_bounded() {
        let tree = corpus();
        let f = longest(&tree, Format::Dsf);
        let engine = Engine::new(2, 32 << 20).unwrap();
        let cached_at =
            |k: u32| engine.cached(&f.disc, f.area, f.start_frame + k, f.start_frame + k + 1).is_some();

        let closed = Reader::new(f.clone());
        let ahead = plan_start_past_the_head(&engine, &closed).unwrap();
        closed.close();
        closed.read_ahead(&engine, ahead);
        assert!(!cached_at(ahead.first), "closed: nothing decoded ahead");
        assert_eq!(engine.traffic().starting.load(Ordering::Relaxed), 0);

        let jumped = Reader::new(f.clone());
        let ahead = plan_start_past_the_head(&engine, &jumped).unwrap();
        let again = jumped.plan_read_ahead(&engine, 64 << 20, (64 << 20) + 4096, 20, 21);
        assert!(again.is_none_or(|a| !a.start));
        jumped.read_ahead(&engine, ahead);
        assert!(!jumped.start.lock().unwrap().active, "jumped");
        assert!(!cached_at(ahead.first), "jumped: nothing decoded ahead");

        // With every place taken, a planned job ends at once and the read-ahead goes on as before.
        let full = Reader::new(f.clone());
        let ahead = plan_start_past_the_head(&engine, &full).unwrap();
        engine.traffic().starting.store(START_RUNNING_MAX, Ordering::Relaxed);
        full.read_ahead(&engine, ahead);
        assert!(!full.start.lock().unwrap().active, "no place");
        assert!(!cached_at(ahead.first), "no place: nothing decoded ahead");
        engine.traffic().starting.store(0, Ordering::Relaxed);
        assert_eq!(engine.traffic().starting.load(Ordering::Relaxed), 0, "places given back");
    }

    /// A plan dropped unrun (`sacdfs verify`, or a read the watchdog answered first) holds
    /// nothing: reads don't wait for it, it is planned again after `START_IDLE`, and no place is
    /// taken.
    #[test]
    fn a_start_ahead_planned_and_dropped_holds_nothing() {
        let tree = corpus();
        let f = longest(&tree, Format::Dsf);
        let engine = Engine::new(2, 32 << 20).unwrap();
        let reader = Reader::new(f.clone());
        let dropped = plan_start_past_the_head(&engine, &reader).unwrap();
        assert!(dropped.start);
        let t = std::time::Instant::now();
        let (data, _) = reader.read(&engine, 0, f.size).unwrap();
        assert!(t.elapsed() < START_WAIT / 2, "a read waited {:?} for a job not running", t.elapsed());
        assert!(data == whole(&f));
        assert_eq!(engine.traffic().starting.load(Ordering::Relaxed), 0);
        let again = |off: u64| reader.plan_read_ahead(&engine, off, off + 4096, 3, 4);
        assert!(again(HEAD_WINDOW + (256 << 10)).is_none(), "planned a moment ago");
        reader.start.lock().unwrap().planned = Some(std::time::Instant::now() - START_IDLE);
        assert!(again(HEAD_WINDOW + (384 << 10)).is_some_and(|a| a.start), "planned again");
    }

    /// Reads that come while the decode-ahead runs wait for it, or decode their own frames, and
    /// give the same bytes either way.
    #[test]
    fn reads_beside_the_start_ahead_give_the_same_bytes() {
        let tree = corpus();
        for format in [Format::Dsf, Format::Wav] {
            let f = longest(&tree, format);
            let want = whole(&f);
            for piece in [4096u64, 32 << 10, 100_000] {
                let engine = Arc::new(Engine::new(3, 32 << 20).unwrap());
                let reader = Arc::new(Reader::new(f.clone()));
                let ahead = plan_start_past_the_head(&engine, &reader).unwrap();
                let job = {
                    let (e, r) = (engine.clone(), reader.clone());
                    std::thread::spawn(move || r.read_ahead(&e, ahead))
                };
                let mut got = Vec::new();
                while (got.len() as u64) < f.size {
                    got.extend(reader.read(&engine, got.len() as u64, piece).unwrap().0);
                }
                reader.close();
                job.join().unwrap();
                assert!(got == want, "{format:?}, reads of {piece}");
                assert_eq!(engine.traffic().starting.load(Ordering::Relaxed), 0);
            }
        }
    }
}
