// SPDX-License-Identifier: MIT OR Apache-2.0

//! Decoding frames: the bounded cache of decoded frames and the pool of decoder threads.

use std::cell::RefCell;
use std::io;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use lru::LruCache;
use rayon::prelude::*;
use scarletbook::{DstDecoder, FrameContent, FrameStatus, TrackFrame};

use crate::disc::{Cursor, Disc, ReadError};
use crate::dsf;

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

/// One decoded timeline position in DSF order: `channels` planes of 4704 bytes, earliest
/// sample in the least significant bit.
#[derive(Debug)]
pub struct Frame {
    /// The planes, channel after channel.
    pub data: Box<[u8]>,
    /// How it was produced.
    pub fill: Fill,
}

impl Frame {
    /// Channel `c`'s 4704 bytes.
    pub fn plane(&self, c: u8) -> &[u8] {
        let n = dsf::FRAME_BYTES as usize;
        let at = usize::from(c) * n;
        self.data.get(at..at + n).unwrap_or(&[])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    disc: u32,
    area: u8,
    frame: u32,
}

#[derive(Debug)]
struct CacheInner {
    lru: LruCache<Key, Arc<Frame>>,
    bytes: usize,
}

/// A least-recently-used cache of decoded frames, bounded in bytes.
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
}

/// The decoder: a frame cache and a thread pool for DST decoding.
#[derive(Debug)]
pub struct Engine {
    cache: FrameCache,
    pool: rayon::ThreadPool,
}

impl Engine {
    /// An engine with `threads` decoder threads (0: one per CPU) and a cache of
    /// `cache_bytes`.
    pub fn new(threads: usize, cache_bytes: usize) -> io::Result<Engine> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("sacdfs-dst-{i}"))
            .panic_handler(|_| log::error!("a decoder thread panicked"))
            .build()
            .map_err(io::Error::other)?;
        Ok(Engine { cache: FrameCache::new(cache_bytes), pool })
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
        Key { disc: disc.id, area: area as u8, frame }
    }

    /// Timeline positions `[first, end)` of an area, if all of them are cached.
    pub fn cached(&self, disc: &Disc, area: usize, first: u32, end: u32) -> Option<Vec<Arc<Frame>>> {
        let mut g = self.cache.lock();
        (first..end).map(|t| g.lru.get(&Self::key(disc, area, t)).cloned()).collect()
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
            *cursor = Some(disc.locate(area, t0)?);
        }
        let Some(cur) = cursor.as_mut() else {
            return Err(ReadError::Io(io::Error::other("no cursor")));
        };
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

/// Decodes one timeline position and converts it to DSF order.
fn decode(job: TrackFrame, channels: u8) -> Result<(u32, Frame), ReadError> {
    let number = job.number;
    let audio = DECODER.with(|d| scarletbook::audio::decode_frame(&mut d.borrow_mut(), job, channels))?;
    let fill = match audio.status {
        FrameStatus::Missing => Fill::Missing,
        FrameStatus::Invalid(_) => Fill::Invalid,
        _ => Fill::Audio,
    };
    let mut data = vec![0u8; audio.data.len()].into_boxed_slice();
    dsf::planarize(&audio.data, channels, &mut data);
    Ok((number, Frame { data, fill }))
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
        Arc::new(Frame { data: vec![0u8; len].into_boxed_slice(), fill: Fill::Audio })
    }

    #[test]
    fn cache_is_bounded() {
        let c = FrameCache::new(10_000);
        for i in 0..10 {
            c.insert(Key { disc: 1, area: 0, frame: i }, frame(4704));
        }
        assert!(c.bytes() <= 10_000);
        assert!(c.get(&Key { disc: 1, area: 0, frame: 9 }).is_some());
        assert!(c.get(&Key { disc: 1, area: 0, frame: 0 }).is_none());
        c.insert(Key { disc: 1, area: 0, frame: 9 }, frame(100));
        assert_eq!(c.bytes(), 4704 + 100);
        let z = FrameCache::new(0);
        z.insert(Key { disc: 1, area: 0, frame: 0 }, frame(1));
        assert_eq!(z.bytes(), 0);
    }
}
