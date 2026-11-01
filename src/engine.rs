// SPDX-License-Identifier: GPL-3.0-or-later

//! Decoding frames: the bounded cache of decoded and converted frames, and the pool of
//! decoder threads, which also converts frames to PCM for the WAV files.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lru::LruCache;
use rayon::prelude::*;
use scarletbook::{DstDecoder, FrameContent, FrameStatus, TrackFrame};

use crate::disc::{Cursor, Disc, ReadError};
use crate::pcm::{Converter, PcmConfig};

/// How a decoded frame was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// Real audio: plain DSD or a decoded DST frame.
    Audio,
    /// No frame at this position: filled with 0x69.
    Missing,
    /// An invalid DST frame: filled with 0x55.
    Invalid,
}

/// One timeline position, ready to serve: decoded DSD as scarletbook gives it (interleaved,
/// `channels x 4704` bytes, which the DSF layout lays out in blocks), or converted PCM in WAV
/// order (interleaved 24-bit little-endian samples).
#[derive(Debug)]
pub struct Frame {
    /// The bytes.
    pub data: Box<[u8]>,
    /// How the DSD frame was produced (for PCM: the DSD frame it was converted from).
    pub fill: Fill,
    /// PCM only: samples clipped by the conversion.
    pub clips: u32,
}

/// What a cache entry holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    /// Decoded DSD, interleaved.
    Dsd,
    /// PCM converted with the engine's settings.
    Pcm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    disc: u32,
    area: u8,
    frame: u32,
    kind: Kind,
}

#[derive(Debug)]
struct CacheInner {
    lru: LruCache<Key, Arc<Frame>>,
    bytes: usize,
}

/// A least-recently-used cache of decoded DSD frames and converted PCM frames, bounded in
/// bytes: one budget for both, so a mount that serves only one format spends all of it on
/// that format.
#[derive(Debug)]
pub struct FrameCache {
    inner: Mutex<CacheInner>,
    cap: usize,
}

impl FrameCache {
    /// A cache holding at most `cap` bytes of frames.
    pub fn new(cap: usize) -> Self {
        FrameCache { inner: Mutex::new(CacheInner { lru: LruCache::unbounded(), bytes: 0 }), cap }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn get(&self, key: &Key) -> Option<Arc<Frame>> {
        self.lock().lru.get(key).cloned()
    }

    fn insert(&self, key: Key, frame: Arc<Frame>) {
        if self.cap == 0 {
            return;
        }
        let mut g = self.lock();
        let len = frame.data.len();
        if let Some(old) = g.lru.put(key, frame) {
            g.bytes -= old.data.len();
        }
        g.bytes += len;
        while g.bytes > self.cap {
            match g.lru.pop_lru() {
                Some((_, f)) => g.bytes -= f.data.len(),
                None => break,
            }
        }
    }

    /// Bytes held.
    pub fn bytes(&self) -> usize {
        self.lock().bytes
    }
}

thread_local! {
    static DECODER: RefCell<DstDecoder> = RefCell::new(DstDecoder::new());
    /// The converters of this thread, by (engine id, channel count).
    static CONVERTERS: RefCell<Vec<(u64, u8, Converter)>> = const { RefCell::new(Vec::new()) };
}

/// Most consecutive frames converted in one job (#24), for frames that come from the
/// plain DSD pipeline (at most [`PIPELINE_CHUNK`] at a time): one job per four frames cut a
/// straight WAV read of a plain DSD area by 17 %, against 11 % with two and 12 % with eight.
/// Frames of a DST batch, which arrive all at once, are converted one per job: there the
/// parallelism matters more, and runs of four made the read 5 % slower. Small, so that a job
/// takes well under a millisecond and readers sharing the pool are not held up (#15).
const PCM_RUN: u32 = 4;

/// Engine ids, for the per-thread converters.
static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);

/// Decoded DSD frames (interleaved, as scarletbook gives them) that a sequential PCM reader
/// keeps between reads, so that the neighbours of its next frame are not decoded again.
#[derive(Debug, Default)]
pub struct Carry {
    frames: VecDeque<(u32, Arc<Dsd>)>,
}

/// How many decoded DSD frames a [`Carry`] keeps: the last two a read needed, which are the
/// previous neighbour and the first frame of the next sequential read.
const CARRY: usize = 2;

/// Plain DSD frames demultiplexed per step of the PCM pipeline, so that conversion starts
/// early. Once a step meets a DST frame, the rest of the range is decoded in one parallel
/// batch instead.
const PIPELINE_CHUNK: u32 = 16;

/// One decoded DSD frame in scarletbook's interleaved layout, the converter's input.
#[derive(Debug)]
struct Dsd {
    data: Vec<u8>,
    fill: Fill,
}

/// The state a reader of PCM frames keeps: its demultiplexer and the last DSD frames.
#[derive(Debug, Default)]
pub struct Stream {
    /// The demultiplexer, continued by sequential reads.
    pub cursor: Option<Cursor>,
    /// The last decoded DSD frames.
    pub carry: Carry,
}

/// The decoder: a frame cache and a thread pool for DST decoding and PCM conversion.
pub struct Engine {
    cache: FrameCache,
    pool: rayon::ThreadPool,
    pcm: PcmConfig,
    id: u64,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("threads", &self.threads())
            .field("cache_bytes", &self.cache.bytes())
            .field("pcm_rate", &self.pcm.rate.hz())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// An engine with `threads` decoder threads (0: one per CPU), a cache of `cache_bytes`
    /// and the default PCM settings.
    pub fn new(threads: usize, cache_bytes: usize) -> io::Result<Engine> {
        Self::with_pcm(threads, cache_bytes, PcmConfig::default())
    }

    /// An engine that converts to PCM with `pcm`.
    pub fn with_pcm(threads: usize, cache_bytes: usize, pcm: PcmConfig) -> io::Result<Engine> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("sacdfs-dst-{i}"))
            .panic_handler(|_| log::error!("a decoder thread panicked"))
            .build()
            .map_err(io::Error::other)?;
        let id = NEXT_ENGINE.fetch_add(1, Ordering::Relaxed);
        Ok(Engine { cache: FrameCache::new(cache_bytes), pool, pcm, id })
    }

    /// The PCM settings.
    pub fn pcm(&self) -> &PcmConfig {
        &self.pcm
    }

    /// Number of decoder threads.
    pub fn threads(&self) -> usize {
        self.pool.current_num_threads()
    }

    /// The frame cache.
    pub fn cache(&self) -> &FrameCache {
        &self.cache
    }

    fn key(disc: &Disc, area: usize, frame: u32) -> Key {
        Key { disc: disc.id, area: area as u8, frame, kind: Kind::Dsd }
    }

    fn pcm_key(disc: &Disc, area: usize, frame: u32) -> Key {
        Key { disc: disc.id, area: area as u8, frame, kind: Kind::Pcm }
    }

    /// Timeline positions `[first, end)` of an area, if all of them are cached.
    pub fn cached(&self, disc: &Disc, area: usize, first: u32, end: u32) -> Option<Vec<Arc<Frame>>> {
        let mut g = self.cache.lock();
        (first..end).map(|t| g.lru.get(&Self::key(disc, area, t)).cloned()).collect()
    }

    /// PCM frames of timeline positions `[first, end)` of an area, if all of them are
    /// cached.
    pub fn cached_pcm(&self, disc: &Disc, area: usize, first: u32, end: u32) -> Option<Vec<Arc<Frame>>> {
        let mut g = self.cache.lock();
        (first..end).map(|t| g.lru.get(&Self::pcm_key(disc, area, t)).cloned()).collect()
    }

    /// Bytes of PCM per frame of `channels` channels with the engine's settings.
    pub fn pcm_frame_bytes(&self, channels: u8) -> usize {
        self.pcm.rate.samples_per_frame() * usize::from(channels) * 3
    }

    /// PCM frames of timeline positions `[first, end)` of an area whose timeline ends at
    /// `area_end` (the first position after its last frame): from the cache, or converted
    /// in parallel on the pool from DSD frames that come from the stream's carry or are
    /// demultiplexed with its cursor and decoded.
    ///
    /// The PCM of position `t` is converted from DSD positions `t - 1`, `t` and `t + 1`;
    /// a neighbour outside `[0, area_end)` is DSD silence. So a track's first and last frames
    /// join the neighbouring tracks' audio exactly as in one stream of the whole area, and a
    /// frame's PCM does not depend on which file or read it was asked for.
    pub fn pcm_frames(
        &self,
        disc: &Disc,
        area: usize,
        stream: &mut Stream,
        first: u32,
        end: u32,
        area_end: u32,
    ) -> Result<Vec<Arc<Frame>>, ReadError> {
        let mut out: Vec<Option<Arc<Frame>>> =
            (first..end).map(|t| self.cache.get(&Self::pcm_key(disc, area, t))).collect();
        let Some(m0) = out.iter().position(Option::is_none) else {
            return Ok(out.into_iter().flatten().collect());
        };
        let m1 = out.iter().rposition(Option::is_none).map_or(m0, |i| i + 1);
        let (t0, t1) = (first + m0 as u32, first + m1 as u32);
        if t1 > area_end {
            return Err(ReadError::Io(io::Error::other("frame past the end of the area")));
        }
        // The DSD positions those frames need: [d0, d1).
        let d0 = t0.saturating_sub(1);
        let d1 = (t1 + 1).min(area_end);
        let channels = disc.areas.get(area).map_or(0, |a| a.channels);
        let frame_bytes = self.pcm_frame_bytes(channels);
        let wanted: Vec<bool> = (t0..t1).map(|t| out[(t - first) as usize].is_none()).collect();
        let converted: Mutex<Vec<(u32, Result<Frame, ReadError>)>> = Mutex::new(Vec::new());
        // A pipeline: this pool thread demultiplexes and decodes the DSD frames in chunks,
        // and each PCM frame is converted on the other pool threads as soon as its next
        // neighbour is decoded, so that demultiplexing (serial) and conversion overlap.
        self.pool.scope(|s| -> Result<(), ReadError> {
            let mut dsd: Vec<Arc<Dsd>> = Vec::with_capacity((d1 - d0) as usize);
            let mut next_job = t0;
            let spawn_ready = |dsd: &Vec<Arc<Dsd>>, next_job: &mut u32, run: u32| {
                let have = d0 + dsd.len() as u32;
                // Frame t can be converted once t + 1 is decoded, or is past the area.
                let ready = |t: u32| t < t1 && (t + 1 < have || (t < have && t + 1 >= area_end));
                while ready(*next_job) {
                    let t = *next_job;
                    *next_job += 1;
                    if !wanted[(t - t0) as usize] {
                        continue;
                    }
                    // A run of consecutive wanted frames, converted in one job (#24).
                    let mut u = t + 1;
                    while u - t < run && ready(u) && wanted[(u - t0) as usize] {
                        u += 1;
                    }
                    *next_job = u;
                    let at = |u: u32| dsd.get((u - d0) as usize).cloned();
                    let prev = if t > 0 { at(t - 1) } else { None };
                    let next = if u < area_end { at(u) } else { None };
                    let cur: Option<Vec<Arc<Dsd>>> = (t..u).map(at).collect();
                    let converted = &converted;
                    s.spawn(move |_| {
                        let r = match &cur {
                            Some(cur) => {
                                self.convert_run(channels, frame_bytes, prev.as_deref(), cur, next.as_deref())
                            }
                            None => Err(ReadError::Io(io::Error::other("frame not decoded"))),
                        };
                        let mut done = converted.lock().unwrap_or_else(|p| p.into_inner());
                        match r {
                            Ok(frames) => done.extend((t..).zip(frames.into_iter().map(Ok))),
                            Err(e) => done.push((t, Err(e))),
                        }
                    });
                }
            };
            // Runs only for pipeline chunks (plain DSD, or the carry): see PCM_RUN.
            let mut run = PCM_RUN;
            self.dsd_frames(disc, area, stream, d0, d1, |chunk| {
                run = if chunk.len() as u32 <= PIPELINE_CHUNK { PCM_RUN } else { 1 };
                dsd.extend(chunk);
                spawn_ready(&dsd, &mut next_job, run);
            })?;
            spawn_ready(&dsd, &mut next_job, run);
            Ok(())
        })?;
        let converted = converted.into_inner().unwrap_or_else(|p| p.into_inner());
        for (t, r) in converted {
            let frame = Arc::new(r?);
            self.cache.insert(Self::pcm_key(disc, area, t), frame.clone());
            out[(t - first) as usize] = Some(frame);
        }
        out.into_iter()
            .map(|f| f.ok_or_else(|| ReadError::Io(io::Error::other("frame not converted"))))
            .collect()
    }

    /// Converts a run of consecutive frames with this thread's converter.
    fn convert_run(
        &self,
        channels: u8,
        frame_bytes: usize,
        prev: Option<&Dsd>,
        cur: &[Arc<Dsd>],
        next: Option<&Dsd>,
    ) -> Result<Vec<Frame>, ReadError> {
        if let [one] = cur {
            return Ok(vec![self.convert_one(channels, frame_bytes, prev, one, next)?]);
        }
        let mut data = vec![0u8; frame_bytes * cur.len()];
        let frames: Vec<&[u8]> = cur.iter().map(|d| d.data.as_slice()).collect();
        let mut clips = vec![0u32; cur.len()];
        self.with_converter(channels, |c| {
            if c.frame_bytes() != frame_bytes {
                return Err(ReadError::Io(io::Error::other("converter frame size mismatch")));
            }
            let (p, n) = (prev.map(|d| d.data.as_slice()), next.map(|d| d.data.as_slice()));
            c.convert_run(p, &frames, n, &mut data, &mut clips);
            Ok(())
        })?;
        Ok(cur
            .iter()
            .zip(data.chunks_exact(frame_bytes))
            .zip(clips)
            .map(|((d, pcm), clips)| Frame { data: pcm.into(), fill: d.fill, clips })
            .collect())
    }

    /// Converts one frame with this thread's converter.
    fn convert_one(
        &self,
        channels: u8,
        frame_bytes: usize,
        prev: Option<&Dsd>,
        cur: &Dsd,
        next: Option<&Dsd>,
    ) -> Result<Frame, ReadError> {
        let mut data = vec![0u8; frame_bytes].into_boxed_slice();
        let clips = self.with_converter(channels, |c| {
            if c.frame_bytes() != frame_bytes {
                return Err(ReadError::Io(io::Error::other("converter frame size mismatch")));
            }
            Ok(c.convert(
                prev.map(|d| d.data.as_slice()),
                &cur.data,
                next.map(|d| d.data.as_slice()),
                &mut data,
            ))
        })?;
        Ok(Frame { data, fill: cur.fill, clips })
    }

    /// Runs `f` with this thread's converter for `channels` channels, made on first use.
    fn with_converter<R>(&self, channels: u8, f: impl FnOnce(&mut Converter) -> R) -> R {
        CONVERTERS.with(|c| {
            let mut list = c.borrow_mut();
            let i = match list.iter().position(|(e, ch, _)| *e == self.id && *ch == channels) {
                Some(i) => i,
                None => {
                    list.push((self.id, channels, Converter::new(channels, &self.pcm)));
                    list.len() - 1
                }
            };
            f(&mut list[i].2)
        })
    }

    /// Decoded DSD frames, interleaved, of positions `[d0, d1)`, handed to `sink` in order,
    /// a chunk at a time: first those the stream carries from its last read, then frames
    /// demultiplexed with its cursor and decoded (DST frames in parallel). Keeps the last
    /// ones in the carry. Runs on a pool thread.
    fn dsd_frames(
        &self,
        disc: &Disc,
        area: usize,
        stream: &mut Stream,
        d0: u32,
        d1: u32,
        mut sink: impl FnMut(Vec<Arc<Dsd>>),
    ) -> Result<(), ReadError> {
        let mut last: VecDeque<(u32, Arc<Dsd>)> = VecDeque::with_capacity(CARRY + 1);
        let mut keep = |t: u32, d: &Arc<Dsd>| {
            last.push_back((t, d.clone()));
            if last.len() > CARRY {
                last.pop_front();
            }
        };
        // The carried frames that start the range.
        let mut t = d0;
        let mut carried = Vec::new();
        while t < d1 {
            match stream.carry.frames.iter().find(|(n, _)| *n == t) {
                Some((_, d)) => {
                    keep(t, d);
                    carried.push(d.clone());
                    t += 1;
                }
                None => break,
            }
        }
        if !carried.is_empty() {
            sink(carried);
        }
        if t < d1 {
            let cursor = &mut stream.cursor;
            let reusable = cursor.as_ref().is_some_and(|c| {
                c.reads(disc, area)
                    && c.next_position() <= t
                    && t - c.next_position() <= disc.tuning.reuse_slack
            });
            if !reusable {
                *cursor = Some(disc.locate_for(area, t, d1)?);
            }
            let Some(cur) = cursor.as_mut() else {
                return Err(ReadError::Io(io::Error::other("no cursor")));
            };
            cur.reserve(disc, d1)?;
            let channels = cur.channels();
            while t < d1 {
                let mut chunk_end = (t + PIPELINE_CHUNK).min(d1);
                let mut jobs = Vec::with_capacity((chunk_end - t) as usize);
                let mut n = t;
                while n < chunk_end {
                    match cur.content(disc, n) {
                        Ok(content) => {
                            // DST frames are what costs here: decode all that are left in
                            // one parallel batch rather than 16 at a time, which would leave
                            // most threads idle while this one demultiplexes.
                            if matches!(content, FrameContent::Dst(_)) {
                                chunk_end = d1;
                            }
                            jobs.push(TrackFrame { number: n, content });
                        }
                        Err(e) => {
                            *cursor = None;
                            return Err(e);
                        }
                    }
                    n += 1;
                }
                let decoded: Vec<Result<(u32, Dsd), ReadError>> =
                    if jobs.iter().any(|j| matches!(j.content, FrameContent::Dst(_))) {
                        jobs.into_par_iter().map(|j| decode_interleaved(j, channels)).collect()
                    } else {
                        jobs.into_iter().map(|j| decode_interleaved(j, channels)).collect()
                    };
                let mut chunk = Vec::with_capacity(decoded.len());
                for r in decoded {
                    let (n, d) = r?;
                    let d = Arc::new(d);
                    keep(n, &d);
                    chunk.push(d);
                }
                sink(chunk);
                t = chunk_end;
            }
        }
        stream.carry.frames = last;
        Ok(())
    }

    /// Timeline positions `[first, end)` of an area: from the cache, or demultiplexed with
    /// `cursor` (continued if it can reach `first`, otherwise replaced by a located one)
    /// and decoded in parallel.
    pub fn frames(
        &self,
        disc: &Disc,
        area: usize,
        cursor: &mut Option<Cursor>,
        first: u32,
        end: u32,
    ) -> Result<Vec<Arc<Frame>>, ReadError> {
        let mut out: Vec<Option<Arc<Frame>>> =
            (first..end).map(|t| self.cache.get(&Self::key(disc, area, t))).collect();
        let Some(m0) = out.iter().position(Option::is_none) else {
            return Ok(out.into_iter().flatten().collect());
        };
        let m1 = out.iter().rposition(Option::is_none).map_or(m0, |i| i + 1);
        let (t0, t1) = (first + m0 as u32, first + m1 as u32);
        let reusable = cursor.as_ref().is_some_and(|c| {
            c.reads(disc, area)
                && c.next_position() <= t0
                && t0 - c.next_position() <= disc.tuning.reuse_slack
        });
        if !reusable {
            *cursor = Some(disc.locate_for(area, t0, t1)?);
        }
        let Some(cur) = cursor.as_mut() else {
            return Err(ReadError::Io(io::Error::other("no cursor")));
        };
        cur.reserve(disc, t1)?;
        let channels = cur.channels();
        let mut jobs = Vec::with_capacity((t1 - t0) as usize);
        for t in t0..t1 {
            let content = match cur.content(disc, t) {
                Ok(c) => c,
                Err(e) => {
                    *cursor = None;
                    return Err(e);
                }
            };
            if out[(t - first) as usize].is_none() {
                jobs.push(TrackFrame { number: t, content });
            }
        }
        let decoded: Vec<Result<(u32, Frame), ReadError>> =
            if jobs.iter().any(|j| matches!(j.content, FrameContent::Dst(_))) {
                self.pool.install(|| jobs.into_par_iter().map(|j| decode(j, channels)).collect())
            } else {
                jobs.into_iter().map(|j| decode(j, channels)).collect()
            };
        for r in decoded {
            let (t, frame) = r?;
            let frame = Arc::new(frame);
            self.cache.insert(Self::key(disc, area, t), frame.clone());
            out[(t - first) as usize] = Some(frame);
        }
        out.into_iter()
            .map(|f| f.ok_or_else(|| ReadError::Io(io::Error::other("frame not decoded"))))
            .collect()
    }
}

/// Decodes one timeline position, interleaved, for the cache.
fn decode(job: TrackFrame, channels: u8) -> Result<(u32, Frame), ReadError> {
    let (number, d) = decode_interleaved(job, channels)?;
    Ok((number, Frame { data: d.data.into_boxed_slice(), fill: d.fill, clips: 0 }))
}

/// Decodes one timeline position, in scarletbook's interleaved layout.
fn decode_interleaved(job: TrackFrame, channels: u8) -> Result<(u32, Dsd), ReadError> {
    let number = job.number;
    let audio = DECODER.with(|d| scarletbook::audio::decode_frame(&mut d.borrow_mut(), job, channels))?;
    let fill = match audio.status {
        FrameStatus::Missing => Fill::Missing,
        FrameStatus::Invalid(_) => Fill::Invalid,
        _ => Fill::Audio,
    };
    Ok((number, Dsd { data: audio.data, fill }))
}

/// One CPU per decoder thread unless told otherwise.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(4, NonZeroUsize::get)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn frame(len: usize) -> Arc<Frame> {
        Arc::new(Frame { data: vec![0u8; len].into_boxed_slice(), fill: Fill::Audio, clips: 0 })
    }

    #[test]
    fn cache_is_bounded() {
        let c = FrameCache::new(10_000);
        for i in 0..10 {
            c.insert(Key { disc: 1, area: 0, frame: i, kind: Kind::Dsd }, frame(4704));
        }
        assert!(c.bytes() <= 10_000);
        assert!(c.get(&Key { disc: 1, area: 0, frame: 9, kind: Kind::Dsd }).is_some());
        assert!(c.get(&Key { disc: 1, area: 0, frame: 0, kind: Kind::Dsd }).is_none());
        c.insert(Key { disc: 1, area: 0, frame: 9, kind: Kind::Dsd }, frame(100));
        assert_eq!(c.bytes(), 4704 + 100);
        let z = FrameCache::new(0);
        z.insert(Key { disc: 1, area: 0, frame: 0, kind: Kind::Dsd }, frame(1));
        assert_eq!(z.bytes(), 0);
    }
}
