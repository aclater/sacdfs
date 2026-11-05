// SPDX-License-Identifier: GPL-3.0-or-later

//! Decoding frames: the bounded cache of decoded and converted frames, and the pool of
//! decoder threads, which also converts frames to PCM for the WAV files.

use std::collections::VecDeque;
use std::io;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    pub data: FrameBuf,
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

/// Bytes of one slab of frame buffers (`Pool`), before rounding to whole slots and pages:
/// 55 two-channel or 18 six-channel DSD frames. *Why this size:* small enough that a slab
/// empties soon after its frames stop being used (it is given back only when empty), large
/// enough that a 256 MiB cache is 512 mappings, far below the kernel's limit on mappings.
const SLAB_BYTES: usize = 512 << 10;

/// The buffer of one frame: a slot of a slab of the cache's pool, or, without a cache
/// or past its cap, a buffer of its own on the heap.
pub struct FrameBuf {
    ptr: std::ptr::NonNull<u8>,
    len: usize,
    home: Home,
}

enum Home {
    Slab { pool: Arc<Pool>, slab: u32, slot: u32 },
    Heap(#[allow(dead_code)] Box<[u8]>),
}

// SAFETY: a FrameBuf owns its bytes exclusively (a slot is handed to one buffer at a time,
// and the slab stays mapped while the buffer's pool has it), like a Box<[u8]>.
#[allow(unsafe_code)]
unsafe impl Send for FrameBuf {}
// SAFETY: as for Send: shared access only reads the bytes.
#[allow(unsafe_code)]
unsafe impl Sync for FrameBuf {}

impl std::ops::Deref for FrameBuf {
    type Target = [u8];
    #[allow(unsafe_code)]
    fn deref(&self) -> &[u8] {
        // SAFETY: ptr..ptr+len is this buffer's slot (or its box), valid while it lives.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl std::ops::DerefMut for FrameBuf {
    #[allow(unsafe_code)]
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as for deref, and &mut self makes the access exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl std::fmt::Debug for FrameBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pooled = matches!(self.home, Home::Slab { .. });
        f.debug_struct("FrameBuf").field("len", &self.len).field("pooled", &pooled).finish()
    }
}

impl From<Vec<u8>> for FrameBuf {
    fn from(v: Vec<u8>) -> Self {
        let mut b = v.into_boxed_slice();
        let len = b.len();
        let ptr = std::ptr::NonNull::new(b.as_mut_ptr()).unwrap_or(std::ptr::NonNull::dangling());
        FrameBuf { ptr, len, home: Home::Heap(b) }
    }
}

impl FrameBuf {
    /// A zeroed buffer of `len` bytes on the heap.
    fn heap(len: usize) -> Self {
        vec![0u8; len].into()
    }
}

impl Drop for FrameBuf {
    fn drop(&mut self) {
        if let Home::Slab { pool, slab, slot } = &self.home {
            pool.release(*slab, *slot);
        }
    }
}

/// An anonymous memory mapping, unmapped when dropped.
struct Mapping {
    ptr: std::ptr::NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is plain memory; its slots are handed out to one FrameBuf at a time.
#[allow(unsafe_code)]
unsafe impl Send for Mapping {}
// SAFETY: as for Send.
#[allow(unsafe_code)]
unsafe impl Sync for Mapping {}

impl Mapping {
    #[cfg(unix)]
    #[allow(unsafe_code)]
    fn new(len: usize) -> Option<Mapping> {
        // SAFETY: an anonymous private mapping at an address of the kernel's choice; it is
        // only used through this Mapping, and unmapped once in drop.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return None;
        }
        std::ptr::NonNull::new(p.cast::<u8>()).map(|ptr| Mapping { ptr, len })
    }

    #[cfg(not(unix))]
    fn new(_len: usize) -> Option<Mapping> {
        None
    }
}

impl Drop for Mapping {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: the mapping made in new, unmapped once; no slot of it is in use (a slab is
        // dropped only when empty, or with its pool, which every slot keeps alive).
        unsafe {
            libc::munmap(self.ptr.as_ptr().cast(), self.len);
        }
    }
}

struct Slab {
    map: Mapping,
    /// Bytes per slot.
    len: usize,
    free: VecDeque<u32>,
    used: u32,
}

#[derive(Default)]
struct PoolInner {
    /// Slabs by id; `None` for an id given back.
    slabs: Vec<Option<Slab>>,
    /// Ids of slabs with a free slot, by slot length.
    open: std::collections::HashMap<usize, Vec<u32>>,
    /// Bytes mapped.
    mapped: usize,
    /// Slabs with no slot in use: only these can be unmapped, so a full pool with none is
    /// not searched for one.
    empty: usize,
}

/// The memory of a frame cache (#73): slabs of equal-sized slots mapped outside the heap,
/// at most the cache's cap of them. Frames take a slot of their length, and give it back
/// when dropped; a slab is unmapped only to make room for a slab of another length.
///
/// *Why:* frames on the heap left holes there when evicted (frames of 2, 5 and 6 channels
/// and PCM frames differ in size, and readers drop frames late), holes which the
/// allocator could neither give back nor fill with frames of another size: a library
/// import's replay held 20 to 40 MiB above the cap, in a heap whose free bytes were 5 to
/// 12 MiB at the end. A slot freed in a slab is reused by the next frame of its length,
/// and the slabs together never pass the cap, so the cache's memory is its cap.
struct Pool {
    inner: Mutex<PoolInner>,
    cap: usize,
}

impl Pool {
    fn lock(&self) -> std::sync::MutexGuard<'_, PoolInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Bytes mapped for a slab of slots of `len` bytes, and its slot count.
    fn slab_shape(len: usize) -> (usize, u32) {
        let slots = (SLAB_BYTES / len).max(1);
        let bytes = (slots * len).div_ceil(4096) * 4096;
        // Whole pages: fill the rounding with more slots.
        (bytes, u32::try_from(bytes / len).unwrap_or(u32::MAX))
    }

    /// Adds slots of `len` bytes to `out` until it holds `n` buffers, under one lock: free
    /// ones, or ones of new slabs while they fit within the cap (after unmapping empty slabs
    /// of other lengths). Their content is unspecified.
    fn alloc_into(self: &Arc<Self>, len: usize, n: usize, out: &mut Vec<FrameBuf>) {
        if len == 0 || out.len() >= n {
            return;
        }
        let mut g = self.lock();
        while out.len() < n {
            match self.alloc_locked(&mut g, len) {
                Some(b) => out.push(b),
                None => break,
            }
        }
    }

    fn alloc_locked(self: &Arc<Self>, g: &mut PoolInner, len: usize) -> Option<FrameBuf> {
        let id = match g.open.get(&len).and_then(|v| v.last().copied()) {
            Some(id) => id,
            None => {
                let (bytes, slots) = Self::slab_shape(len);
                if g.mapped + bytes > self.cap {
                    for i in 0..g.slabs.len() {
                        if g.mapped + bytes <= self.cap || g.empty == 0 {
                            break;
                        }
                        if g.slabs[i].as_ref().is_some_and(|s| s.used == 0) {
                            if let Some(s) = g.slabs[i].take() {
                                g.mapped -= s.map.len;
                                g.empty -= 1;
                                if let Some(v) = g.open.get_mut(&s.len) {
                                    v.retain(|&x| x as usize != i);
                                }
                            }
                        }
                    }
                    if g.mapped + bytes > self.cap {
                        return None;
                    }
                }
                let map = Mapping::new(bytes)?;
                g.mapped += bytes;
                let slab = Slab { map, len, free: (0..slots).collect(), used: 0 };
                g.empty += 1;
                let id = match g.slabs.iter().position(Option::is_none) {
                    Some(i) => {
                        g.slabs[i] = Some(slab);
                        i
                    }
                    None => {
                        g.slabs.push(Some(slab));
                        g.slabs.len() - 1
                    }
                };
                let id = id as u32;
                g.open.entry(len).or_default().push(id);
                id
            }
        };
        let s = g.slabs.get_mut(id as usize)?.as_mut()?;
        let slot = s.free.pop_front()?;
        if s.used == 0 {
            g.empty -= 1;
        }
        s.used += 1;
        // SAFETY: slot < slots, so the slot lies within the mapping.
        #[allow(unsafe_code)]
        let ptr = unsafe { s.map.ptr.add(slot as usize * len) };
        if s.free.is_empty() {
            if let Some(v) = g.open.get_mut(&len) {
                v.retain(|&x| x != id);
            }
        }
        Some(FrameBuf { ptr, len, home: Home::Slab { pool: Arc::clone(self), slab: id, slot } })
    }

    fn release(&self, slab: u32, slot: u32) {
        let mut g = self.lock();
        let g = &mut *g;
        let Some(Some(s)) = g.slabs.get_mut(slab as usize) else { return };
        s.free.push_back(slot);
        s.used -= 1;
        if s.used == 0 {
            g.empty += 1;
        }
        if s.free.len() == 1 {
            g.open.entry(s.len).or_default().push(slab);
        }
    }

    /// Bytes mapped.
    fn mapped(&self) -> usize {
        self.lock().mapped
    }

    /// Unmaps every slab with no slot in use.
    fn unmap_empty(&self) {
        let freed: Vec<Slab> = {
            let mut g = self.lock();
            let g = &mut *g;
            let mut freed = Vec::new();
            for (i, slot) in g.slabs.iter_mut().enumerate() {
                if slot.as_ref().is_some_and(|s| s.used == 0) {
                    if let Some(s) = slot.take() {
                        g.mapped -= s.map.len;
                        g.empty -= 1;
                        if let Some(v) = g.open.get_mut(&s.len) {
                            v.retain(|&x| x as usize != i);
                        }
                        freed.push(s);
                    }
                }
            }
            freed
        };
        drop(freed);
    }
}

/// Bytes of a cache's cap that cached frames leave free: an eighth of the cap, at most
/// 32 MiB, a read-ahead's worth of frames. The pool's slabs fill the whole cap, so these are
/// free slots that new frames take without evicting first. *Why:* with frames filling the
/// whole cap, every new frame evicted one first, and straight reads of plain DSD took about
/// 5 % longer (1.80 against 1.73 s for a stereo area at 256 MiB, medians of 10).
fn free_reserve(cap: usize) -> usize {
    (cap / 8).min(32 << 20)
}

#[derive(Debug)]
struct CacheInner {
    lru: LruCache<Key, Arc<Frame>>,
    bytes: usize,
}

/// A least-recently-used cache of decoded DSD frames and converted PCM frames, bounded in
/// bytes: one budget for both, so a mount that serves only one format spends all of it on
/// that format.
///
/// The frames' buffers come from the cache's pool (`Pool`), slabs mapped outside the heap and
/// bounded by the cap, so the memory the cache holds, cached frames and free slots, stays
/// within its cap (#73). A frame that needs a slot when the pool is full evicts the least
/// recently used frames until one is free.
pub struct FrameCache {
    inner: Mutex<CacheInner>,
    pool: Arc<Pool>,
    cap: usize,
    /// When the cache was last used, in ms since `epoch` ([`FrameCache::release_if_idle`]).
    used: AtomicU64,
    epoch: Instant,
}

impl std::fmt::Debug for FrameCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameCache").field("cap", &self.cap).finish_non_exhaustive()
    }
}

impl FrameCache {
    /// A cache holding at most `cap` bytes of frames.
    pub fn new(cap: usize) -> Self {
        FrameCache {
            inner: Mutex::new(CacheInner { lru: LruCache::unbounded(), bytes: 0 }),
            pool: Arc::new(Pool { inner: Mutex::new(PoolInner::default()), cap }),
            cap,
            used: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    /// `n` buffers of `len` bytes for new frames: free slots of the pool, evicting the least
    /// recently used frames to free some when it is full; past that (no cache, or frames
    /// that readers still hold fill it), buffers on the heap. Their content is unspecified.
    fn buffers(&self, len: usize, n: usize) -> Vec<FrameBuf> {
        let mut out = Vec::with_capacity(n);
        while self.cap > 0 && out.len() < n {
            self.pool.alloc_into(len, n, &mut out);
            if out.len() >= n {
                break;
            }
            // Evict as many frames as buffers are missing, outside the pool's lock; an
            // evicted frame gives its slot back when dropped, here or by the last reader
            // that holds it.
            let evicted: Vec<Arc<Frame>> = {
                let mut g = self.lock();
                let mut v = Vec::with_capacity(n - out.len());
                while v.len() < n - out.len() {
                    let Some((_, f)) = g.lru.pop_lru() else { break };
                    g.bytes -= f.data.len();
                    v.push(f);
                }
                v
            };
            if evicted.is_empty() {
                break;
            }
            drop(evicted);
        }
        out.resize_with(n, || FrameBuf::heap(len));
        out
    }

    /// One buffer of `len` bytes for a new frame, as [`FrameCache::buffers`].
    fn buffer(&self, len: usize) -> FrameBuf {
        self.buffers(len, 1).pop().unwrap_or_else(|| FrameBuf::heap(len))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheInner> {
        self.used.store(self.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Drops every cached frame and unmaps the slabs left empty, if the cache has not been
    /// used for `idle`; whether it did (#150). Frames readers still hold stay until dropped.
    pub fn release_if_idle(&self, idle: Duration) -> bool {
        let now = self.epoch.elapsed().as_millis() as u64;
        if now.saturating_sub(self.used.load(Ordering::Relaxed)) < idle.as_millis() as u64 {
            return false;
        }
        let frames: Vec<Arc<Frame>> = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.bytes = 0;
            let mut v = Vec::with_capacity(g.lru.len());
            while let Some((_, f)) = g.lru.pop_lru() {
                v.push(f);
            }
            v
        };
        let any = !frames.is_empty() || self.pool.mapped() > 0;
        drop(frames);
        self.pool.unmap_empty();
        any
    }

    fn get(&self, key: &Key) -> Option<Arc<Frame>> {
        self.lock().lru.get(key).cloned()
    }

    fn insert(&self, key: Key, frame: Arc<Frame>) {
        if self.cap == 0 {
            return;
        }
        let mut evicted = Vec::new();
        {
            let mut g = self.lock();
            let len = frame.data.len();
            if let Some(old) = g.lru.put(key, frame) {
                g.bytes -= old.data.len();
                evicted.push(old);
            }
            g.bytes += len;
            // Cached frames keep an eighth of the cap free (`free_reserve`), so that new
            // frames mostly take free slots rather than evict first; heap buffers (past the
            // pool) count too.
            while g.bytes > self.cap - free_reserve(self.cap) {
                match g.lru.pop_lru() {
                    Some((_, f)) => {
                        g.bytes -= f.data.len();
                        evicted.push(f);
                    }
                    None => break,
                }
            }
        }
        drop(evicted);
    }

    /// Bytes of cached frames.
    pub fn bytes(&self) -> usize {
        self.lock().bytes
    }

    /// The cache's cap in bytes.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Bytes the cache's pool has mapped for frames: at most the cap.
    pub fn mapped(&self) -> usize {
        self.pool.mapped()
    }
}

/// DST decoders not in use (#150): a decoder thread takes one for a frame and gives it back,
/// so there are only as many as frames were ever decoded at once.
///
/// *Why not one per thread:* a decoder's tables and buffers (about 64 KiB per filter, up to
/// about 400 KiB for six channels) stayed with every decoder thread that had ever decoded
/// DST, one per CPU, though seek-heavy reads such as a library import decode a few frames at
/// a time. *Trade-off:* a lock taken twice per frame, against a frame's decoding of about
/// 1 ms; a decoder may move between threads (its tables are rebuilt for every frame anyway).
static DECODERS: Mutex<Vec<DstDecoder>> = Mutex::new(Vec::new());

/// Runs `f` with a DST decoder from [`DECODERS`], or a new one.
fn with_decoder<R>(f: impl FnOnce(&mut DstDecoder) -> R) -> R {
    let pool = || DECODERS.lock().unwrap_or_else(|p| p.into_inner());
    let mut d = pool().pop().unwrap_or_default();
    let r = f(&mut d);
    pool().push(d);
    r
}

/// PCM converters not in use, by (engine id, channel count), taken and given back as
/// [`DECODERS`] are (#150), for the same reason: not one per thread that ever converted.
static CONVERTERS: Mutex<Vec<(u64, u8, Converter)>> = Mutex::new(Vec::new());

/// Most consecutive frames converted in one job (#24), for frames that come from the
/// plain DSD pipeline (at most [`PIPELINE_CHUNK`] at a time): one job per four frames cut a
/// straight WAV read of a plain DSD area by 17 %, against 11 % with two and 12 % with eight.
/// Frames of a DST batch, which arrive all at once, are converted one per job: there the
/// parallelism matters more, and runs of four made the read 5 % slower. Small, so that a job
/// takes well under a millisecond and readers sharing the pool are not held up (#15).
const PCM_RUN: u32 = 4;

/// Engine ids, for the pooled converters.
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

/// Drops the pooled converters of engine `id`.
fn drop_converters(id: u64) {
    let mine: Vec<_> = {
        let mut list = CONVERTERS.lock().unwrap_or_else(|p| p.into_inner());
        let (mine, rest) = std::mem::take(&mut *list).into_iter().partition(|(e, _, _)| *e == id);
        *list = rest;
        mine
    };
    drop(mine);
}

/// How long an engine's cache goes unused before [`Engine::start_idle_release`] gives its
/// memory back (#150). *Why 60 s:* longer than the gaps of a player, a scanner or `cp`
/// between reads, which keep their read-ahead; a reader that comes back later loses a few
/// ms to decoding.
pub const IDLE_RELEASE: Duration = Duration::from_secs(60);

impl Drop for Engine {
    fn drop(&mut self) {
        drop_converters(self.id);
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

    /// Gives back what an idle engine holds (#150), if its cache has not been used for
    /// `idle`: the cached frames and their slabs, the decoders and converters not in use, and
    /// the heap's free pages (glibc's `malloc_trim`). Whether it did.
    ///
    /// *Why:* between scans and playback a mount held its whole cache and every decoder it
    /// had made; a reader that comes back decodes its frames again, a few ms, and the
    /// kernel's page cache often still holds what it read.
    pub fn release_if_idle(&self, idle: Duration) -> bool {
        if !self.cache.release_if_idle(idle) {
            return false;
        }
        drop(std::mem::take(&mut *DECODERS.lock().unwrap_or_else(|p| p.into_inner())));
        drop_converters(self.id);
        trim_heap();
        true
    }

    /// Starts a thread (`sacdfs-idle`) that calls [`Engine::release_if_idle`] with
    /// [`IDLE_RELEASE`] every 15 s, for as long as the engine lives.
    pub fn start_idle_release(self: &Arc<Self>) -> io::Result<()> {
        let weak = Arc::downgrade(self);
        std::thread::Builder::new().name("sacdfs-idle".to_owned()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(15));
                match weak.upgrade() {
                    Some(e) => {
                        if e.release_if_idle(IDLE_RELEASE) {
                            log::debug!("idle: cache and decoders released");
                        }
                    }
                    None => return,
                }
            }
        })?;
        Ok(())
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

    /// Converts a run of consecutive frames with a pooled converter.
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
        let bufs = self.cache.buffers(frame_bytes, cur.len());
        Ok(cur
            .iter()
            .zip(data.chunks_exact(frame_bytes))
            .zip(clips)
            .zip(bufs)
            .map(|(((d, pcm), clips), mut buf)| {
                buf.copy_from_slice(pcm);
                Frame { data: buf, fill: d.fill, clips }
            })
            .collect())
    }

    /// Converts one frame with a pooled converter.
    fn convert_one(
        &self,
        channels: u8,
        frame_bytes: usize,
        prev: Option<&Dsd>,
        cur: &Dsd,
        next: Option<&Dsd>,
    ) -> Result<Frame, ReadError> {
        let mut data = self.cache.buffer(frame_bytes);
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

    /// Runs `f` with a converter for `channels` channels from [`CONVERTERS`], or a new one.
    fn with_converter<R>(&self, channels: u8, f: impl FnOnce(&mut Converter) -> R) -> R {
        let pool = || CONVERTERS.lock().unwrap_or_else(|p| p.into_inner());
        let taken = {
            let mut list = pool();
            list.iter()
                .rposition(|(e, ch, _)| *e == self.id && *ch == channels)
                .map(|i| list.swap_remove(i).2)
        };
        let mut c = taken.unwrap_or_else(|| Converter::new(channels, &self.pcm));
        let r = f(&mut c);
        pool().push((self.id, channels, c));
        r
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
        let bufs = self.cache.buffers(usize::from(channels) * FRAME_BYTES_PER_CHANNEL, jobs.len());
        let jobs: Vec<(TrackFrame, Option<FrameBuf>)> =
            jobs.into_iter().zip(bufs.into_iter().map(Some)).collect();
        let decoded: Vec<Result<(u32, Frame), ReadError>> =
            if jobs.iter().any(|(j, _)| matches!(j.content, FrameContent::Dst(_))) {
                self.pool.install(|| jobs.into_par_iter().map(|(j, b)| decode(j, channels, b)).collect())
            } else {
                jobs.into_iter().map(|(j, b)| decode(j, channels, b)).collect()
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

/// Bytes per channel of a decoded DSD frame.
const FRAME_BYTES_PER_CHANNEL: usize = 4704;

/// Decodes one timeline position, interleaved, for the cache, into `buf` (`channels x 4704`
/// bytes, from [`FrameCache::buffers`], or a new one): what `scarletbook::audio::decode_frame` gives, but
/// written into a buffer of the cache rather than a new one (#73).
fn decode(job: TrackFrame, channels: u8, buf: Option<FrameBuf>) -> Result<(u32, Frame), ReadError> {
    let bad = |m: &str| Err(ReadError::Io(io::Error::other(m.to_owned())));
    let len = usize::from(channels) * FRAME_BYTES_PER_CHANNEL;
    let mut buf = buf.unwrap_or_else(|| FrameBuf::heap(len));
    if !(1..=6).contains(&channels) || buf.len() != len {
        return bad("bad channel count");
    }
    let fill = match job.content {
        FrameContent::Dsd(d) => {
            if d.len() != buf.len() {
                return bad("a plain DSD frame of the wrong size");
            }
            buf.copy_from_slice(&d);
            Fill::Audio
        }
        FrameContent::Missing => {
            buf.fill(scarletbook::audio::MISSING_FILL);
            Fill::Missing
        }
        FrameContent::Dst(bytes) => match with_decoder(|d| d.decode(&bytes, channels, &mut buf)) {
            Ok(()) => Fill::Audio,
            Err(_) => {
                buf.fill(scarletbook::audio::INVALID_FILL);
                Fill::Invalid
            }
        },
    };
    Ok((job.number, Frame { data: buf, fill, clips: 0 }))
}

/// Decodes one timeline position, in scarletbook's interleaved layout.
fn decode_interleaved(job: TrackFrame, channels: u8) -> Result<(u32, Dsd), ReadError> {
    let number = job.number;
    let audio = with_decoder(|d| scarletbook::audio::decode_frame(d, job, channels))?;
    let fill = match audio.status {
        FrameStatus::Missing => Fill::Missing,
        FrameStatus::Invalid(_) => Fill::Invalid,
        _ => Fill::Audio,
    };
    Ok((number, Dsd { data: audio.data, fill }))
}

/// Malloc arenas glibc may use, set by [`limit_malloc_arenas`].
pub const MALLOC_ARENAS: i32 = 1;

/// Limits glibc's malloc to [`MALLOC_ARENAS`] arenas, unless `MALLOC_ARENA_MAX` or
/// `GLIBC_TUNABLES` already sets it. To be called before any thread is started; returns
/// whether it was set.
///
/// *Why (#73):* glibc gives each thread that allocates its own arena, up to eight per CPU,
/// and every arena keeps the memory freed in it for that arena alone. Decoder, FUSE and
/// read-ahead threads all allocate and free frames and read buffers, so each arena ended up
/// holding its own copy of the working set: a seek-heavy scan (a library import) of 60
/// images held 473 MiB at a 256 MiB cache and 125 MiB at 16 MiB, against 314 and 56 MiB with
/// one arena. *Why one:* two and four held 330 and 368 MiB; decoding allocates little per
/// frame (the cache's buffers are reused), so the shared lock did not slow reads (see
/// docs/performance.md).
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(unsafe_code)]
pub fn limit_malloc_arenas() -> bool {
    let tuned = std::env::var_os("MALLOC_ARENA_MAX").is_some()
        || std::env::var("GLIBC_TUNABLES").is_ok_and(|t| t.contains("arena_max"));
    if tuned {
        return false;
    }
    // SAFETY: mallopt takes two integers and only changes allocator settings; M_ARENA_MAX
    // is a documented parameter, and changing it is safe at any time (it bounds arenas
    // created afterwards).
    unsafe { libc::mallopt(libc::M_ARENA_MAX, MALLOC_ARENAS) == 1 }
}

/// Gives the heap's free pages back to the system (glibc's `malloc_trim`).
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(unsafe_code)]
fn trim_heap() {
    // SAFETY: malloc_trim takes an integer and only releases free memory of the allocator.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Elsewhere, nothing to trim.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn trim_heap() {}

/// Elsewhere there is no glibc arena to limit.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn limit_malloc_arenas() -> bool {
    false
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
        Arc::new(Frame { data: vec![0u8; len].into(), fill: Fill::Audio, clips: 0 })
    }

    fn key(i: u32) -> Key {
        Key { disc: 1, area: 0, frame: i, kind: Kind::Dsd }
    }

    #[test]
    fn cache_is_bounded() {
        let c = FrameCache::new(10_000);
        for i in 0..10 {
            c.insert(key(i), frame(4704));
        }
        assert!(c.bytes() <= 10_000);
        assert!(c.get(&key(9)).is_some());
        assert!(c.get(&key(0)).is_none());
        c.insert(key(9), frame(100));
        // Only frame 9 fitted beside the free eighth, and it was replaced.
        assert_eq!(c.bytes(), 100);
        // A slab does not fit in so small a cap: buffers come from the heap.
        assert_eq!(c.mapped(), 0);
        let z = FrameCache::new(0);
        assert_eq!(z.buffers(8, 2).len(), 2);
        z.insert(key(0), frame(1));
        assert_eq!(z.bytes(), 0);
    }

    /// Frames of the pool: the mapping stays within the cap, slots are reused once their
    /// frames are evicted and dropped, and an empty slab makes room for another length.
    #[test]
    fn an_idle_cache_gives_its_memory_back() {
        let c = FrameCache::new(4 << 20);
        for i in 0..200 {
            let b = c.buffer(9408);
            c.insert(key(i), Arc::new(Frame { data: b, fill: Fill::Audio, clips: 0 }));
        }
        let held = c.get(&key(199)).unwrap();
        // Just used: kept.
        assert!(!c.release_if_idle(Duration::from_secs(60)));
        assert!(c.bytes() > 0);
        // Idle: frames dropped, and every slab but the held frame's unmapped.
        assert!(c.release_if_idle(Duration::ZERO));
        assert_eq!(c.bytes(), 0);
        assert!(c.get(&key(0)).is_none());
        assert_eq!(c.mapped(), Pool::slab_shape(9408).0);
        drop(held);
        assert!(c.release_if_idle(Duration::ZERO));
        assert_eq!(c.mapped(), 0);
        // Still usable.
        let b = c.buffer(9408);
        c.insert(key(1), Arc::new(Frame { data: b, fill: Fill::Audio, clips: 0 }));
        assert!(c.get(&key(1)).is_some());
    }

    #[test]
    fn an_idle_engine_drops_its_pools() {
        let e = Arc::new(Engine::new(2, 4 << 20).unwrap());
        e.start_idle_release().unwrap();
        // A converter and a decoder are pooled after use.
        e.with_converter(2, |_| ());
        with_decoder(|_| ());
        let mine = || CONVERTERS.lock().unwrap().iter().filter(|(id, _, _)| *id == e.id).count();
        assert_eq!(mine(), 1);
        e.with_converter(2, |_| ());
        assert_eq!(mine(), 1, "reused, not made again");
        let b = e.cache().buffer(9408);
        e.cache().insert(key(0), Arc::new(Frame { data: b, fill: Fill::Audio, clips: 0 }));
        assert!(!e.release_if_idle(Duration::from_secs(60)));
        assert!(e.release_if_idle(Duration::ZERO));
        assert_eq!(mine(), 0);
        assert_eq!(e.cache().bytes(), 0);
        assert_eq!(e.cache().mapped(), 0);
    }

    #[test]
    fn pooled_frames_stay_within_the_cap() {
        let cap = 4 << 20;
        let c = FrameCache::new(cap);
        let insert = |c: &FrameCache, i: u32, len: usize| {
            let mut b = c.buffer(len);
            b.fill(i as u8);
            c.insert(key(i), Arc::new(Frame { data: b, fill: Fill::Audio, clips: 0 }));
        };
        for i in 0..2000 {
            insert(&c, i, 9408);
            assert!(c.mapped() <= cap && c.bytes() <= cap);
        }
        // The cache is full of pooled frames, and the newest are intact.
        assert!(c.bytes() > cap - (1 << 20));
        let f = c.get(&key(1999)).unwrap();
        assert!(matches!(f.data.home, Home::Slab { .. }));
        assert!(f.data.iter().all(|&b| b == (1999 % 256) as u8));
        drop(f);
        // Another length takes over the cap as the old frames are evicted.
        for i in 2000..2400 {
            insert(&c, i, 28224);
            assert!(c.mapped() <= cap);
        }
        let f = c.get(&key(2399)).unwrap();
        assert!(matches!(f.data.home, Home::Slab { .. }) && f.data.len() == 28224);
        assert!(c.get(&key(1999)).is_none());
        // A frame a reader holds keeps its slot until dropped; the cache stays usable.
        let held: Vec<_> = (2300..2400).filter_map(|i| c.get(&key(i))).collect();
        for i in 2400..2600 {
            insert(&c, i, 28224);
        }
        assert!(c.mapped() <= cap);
        drop(held);
        drop(c);
    }
}
