// SPDX-License-Identifier: GPL-3.0-or-later

//! Warming a folder's heads after a player's probe (#510).
//!
//! A network player of #456 probes a track as soon as it lists the album, and when playback
//! starts, minutes later, it re-reads the head of every other file of the album before (and
//! while) it opens the track. On a slow CPU the first start after a mount decoded all those
//! heads then, about 4.5 s of a 32-bit Atom's CPU, while the player waited. So when one file's
//! head was probed and the mount then goes [`QUIET`], the other files of its folder have their
//! heads decoded in the background, as far as the probe read, and kept as heads.
//!
//! - **Only after a probe of one file:** the reads of audio since the mount was last quiet were of
//!   one file only, from its start, past the kernel's read-ahead of a first read, and within its
//!   head ([`Reader::probe_extent`]). A library scanner reads every file of a folder, so the
//!   reads before a pause cover several files; a tagger stops within the kernel's read-ahead;
//!   a player that plays reads past the head.
//! - **Only on a CPU slower than players' start-up bursts** (`--start-burst`): a fast one decodes
//!   the heads in the moment the player asks.
//! - **Only while the mount is idle:** it starts after [`QUIET`] with no read, and stops at the
//!   next read, of any file, after the frames it is decoding (one per decoder thread). *Why
//!   stop rather than go on beside it (#510, s071d's measurements):* decoding heads beside a
//!   player starting a track made its stalls 0.7 to 1.3 s longer on Atom-speed cores.
//! - **Within the heads an idle release keeps** ([`crate::engine::FrameCache::heads_budget`]),
//!   with the probed file's head kept the most recent, so they are there minutes later.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::engine::Engine;
use crate::track::Reader;
use crate::tree::Tree;

/// How long the mount goes without a read before warming starts. *Why 2 s:* a player starting a
/// track re-reads the album's heads and reads its track with gaps under 1 s (#456), and a
/// scanner goes on to the next file at once; the player of #456 waited 3 minutes after its probe.
pub const QUIET: Duration = Duration::from_secs(2);

/// Warms a folder's heads after a probe; see the module.
#[derive(Debug)]
pub struct Warmer {
    engine: Weak<Engine>,
    tree: Weak<Tree>,
    quiet: Duration,
    epoch: Instant,
    /// Reads so far: warming stops when this moves.
    reads: AtomicU64,
    /// When the last read came, in ms since `epoch` plus one (0: none yet).
    last: AtomicU64,
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Debug, Default)]
struct State {
    /// The track files read since the mount was last quiet.
    burst: Option<Burst>,
    /// The thread runs.
    thread: bool,
    /// The volume is gone: the thread ends.
    closed: bool,
}

#[derive(Debug)]
struct Burst {
    /// The last reader of the first file read.
    reader: Arc<Reader>,
    /// Another file was read too.
    others: bool,
    /// Taken by the thread already.
    taken: bool,
}

impl Warmer {
    /// A warmer for the files of `tree`, decoding with `engine`. It starts its thread at the first
    /// read of a track file.
    pub fn new(engine: &Arc<Engine>, tree: &Arc<Tree>) -> Arc<Warmer> {
        Self::with_quiet(engine, tree, QUIET)
    }

    fn with_quiet(engine: &Arc<Engine>, tree: &Arc<Tree>, quiet: Duration) -> Arc<Warmer> {
        Arc::new(Warmer {
            engine: Arc::downgrade(engine),
            tree: Arc::downgrade(tree),
            quiet,
            epoch: Instant::now(),
            reads: AtomicU64::new(0),
            last: AtomicU64::new(0),
            state: Mutex::default(),
            wake: Condvar::new(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64 + 1
    }

    /// Notes that a read arrived: of a track file's audio through `reader`, or of something else
    /// (`None`: a sidecar, or a track file's header or tag). Any read stops warming; only reads of
    /// audio make the reads since the mount was last quiet a probe's (one file) or not.
    pub fn read(self: &Arc<Self>, reader: Option<&Arc<Reader>>) {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let now = self.now_ms();
        let last = self.last.swap(now, Ordering::SeqCst);
        let quiet_before = last == 0 || now.saturating_sub(last) >= self.quiet.as_millis() as u64;
        let mut st = self.lock();
        if quiet_before {
            st.burst = None;
        }
        let Some(reader) = reader else { return };
        match &mut st.burst {
            None => st.burst = Some(Burst { reader: reader.clone(), others: false, taken: false }),
            Some(b) if b.others => return,
            Some(b) if Arc::ptr_eq(&b.reader.file, &reader.file) => {
                if !Arc::ptr_eq(&b.reader, reader) {
                    b.reader = reader.clone();
                }
            }
            Some(b) => {
                b.others = true;
                return;
            }
        }
        if !st.thread && !st.closed {
            let me = Arc::downgrade(self);
            let spawned = std::thread::Builder::new().name("sacdfs-warm".to_owned()).spawn(move || {
                while let Some(w) = me.upgrade() {
                    if !w.next() {
                        return;
                    }
                }
            });
            match spawned {
                Ok(_) => st.thread = true,
                Err(e) => log::debug!("warm: no thread: {e}"),
            }
        } else {
            self.wake.notify_all();
        }
    }

    /// Ends the thread.
    pub fn close(&self) {
        self.lock().closed = true;
        self.wake.notify_all();
    }

    /// Waits for a probe followed by quiet, and warms its folder; `false` when closed. Waits at
    /// most a few seconds without one, so that the thread lets go of the warmer in between.
    fn next(&self) -> bool {
        let Some((reader, reads)) = self.wait_for_probe(Duration::from_secs(10)) else {
            return !self.lock().closed;
        };
        let (Some(engine), Some(tree)) = (self.engine.upgrade(), self.tree.upgrade()) else {
            return false;
        };
        self.warm(&engine, &tree, &reader, reads);
        true
    }

    /// The reader of a burst of reads of one file, once the mount has been quiet since for
    /// `quiet`, and the reads so far then; `None` if there is none within `wait`, or closed.
    fn wait_for_probe(&self, wait: Duration) -> Option<(Arc<Reader>, u64)> {
        let deadline = Instant::now() + wait;
        let mut st = self.lock();
        loop {
            if st.closed {
                return None;
            }
            let now = Instant::now();
            let left = deadline.saturating_duration_since(now);
            let pending = st.burst.as_ref().is_some_and(|b| !b.others && !b.taken);
            if pending {
                let since = self.now_ms().saturating_sub(self.last.load(Ordering::SeqCst));
                let quiet = self.quiet.as_millis() as u64;
                if since >= quiet {
                    let reads = self.reads.load(Ordering::SeqCst);
                    let b = st.burst.as_mut()?;
                    b.taken = true;
                    return Some((b.reader.clone(), reads));
                }
                let more = Duration::from_millis(quiet - since);
                st = self.wake.wait_timeout(st, more).unwrap_or_else(|p| p.into_inner()).0;
                continue;
            }
            if left.is_zero() {
                return None;
            }
            st = self.wake.wait_timeout(st, left).unwrap_or_else(|p| p.into_inner()).0;
        }
    }

    /// Whether a read came since there were `reads`.
    fn interrupted(&self, reads: u64) -> bool {
        self.reads.load(Ordering::SeqCst) != reads
    }

    /// Decodes the heads of the files beside `probe`'s, as far as it read, until a read comes
    /// (the reads were `reads` before). How many files it went through, and whether a read
    /// stopped it.
    fn warm(&self, engine: &Engine, tree: &Tree, probe: &Reader, reads: u64) -> (usize, bool) {
        let Some(extent) = probe.probe_extent(engine) else { return (0, false) };
        let file = &probe.file;
        let files = tree.files_beside(file);
        let Some(at) = files.iter().position(|f| Arc::ptr_eq(f, file) || f.number == file.number) else {
            return (0, false);
        };
        // The files after the probed one first, as an album plays; the probed one last, so that
        // its head is the most recently read and outlives the others' (an idle release keeps
        // the most recent).
        let order: Vec<_> = files[at + 1..].iter().chain(&files[..at]).chain(std::iter::once(file)).collect();
        let mut room = engine.cache().heads_budget();
        let began = Instant::now();
        crate::trace_line!(
            "warm r={} start: {} files beside, {} bytes of audio each",
            probe.id(),
            order.len() - 1,
            extent
        );
        let step = engine.threads().max(1) as u32;
        let mut done = 0;
        // The probed file's head is counted first: it must fit as well.
        let own = file.head_frames(extent) as usize * file.cached_frame_bytes(engine);
        room = room.saturating_sub(own);
        for (i, f) in order.iter().enumerate() {
            let last = i + 1 == order.len();
            let n = f.head_frames(extent);
            if !last {
                let bytes = n as usize * f.cached_frame_bytes(engine);
                if bytes > room {
                    continue;
                }
                room -= bytes;
            }
            let reader = Reader::new(Arc::clone(f));
            let mut k = 0;
            while k < n {
                if self.interrupted(reads) {
                    crate::trace_line!(
                        "warm r={} stopped: a read, after {done} files, took={}",
                        probe.id(),
                        crate::trace::Ms(began.elapsed())
                    );
                    return (done, true);
                }
                let end = (k + step).min(n);
                if let Err(e) = reader.warm(engine, k, end) {
                    log::debug!("warm: {}: {e}", f.disc.path.display());
                    break;
                }
                k = end;
            }
            if !last {
                done += 1;
            }
        }
        crate::trace_line!(
            "warm r={} done: {done} files, took={}",
            probe.id(),
            crate::trace::Ms(began.elapsed())
        );
        log::debug!("warm: decoded the heads of {done} files beside a probed one");
        (done, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::{Format, TrackFile};

    /// More than the kernel's read-ahead of a first read, less than the corpus's longest files.
    const PROBE: u64 = 300 << 10;

    fn corpus() -> Arc<Tree> {
        let images = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/images");
        Arc::new(Tree::new(crate::tree::SourceDir::scan(&images).unwrap()))
    }

    /// A slow engine (it decodes below the start burst) with a cache of `bytes`.
    fn slow_engine(bytes: usize) -> Arc<Engine> {
        let e = Engine::new(2, bytes).unwrap();
        for ch in 1..=6 {
            e.rates().set(false, ch, 1.5);
            e.rates().set(true, ch, 1.5);
        }
        Arc::new(e)
    }

    /// The corpus's folder of DSF files whose shortest is the longest, with at least `n` files.
    fn folder(tree: &Tree, n: usize) -> Vec<Arc<TrackFile>> {
        let mut best: Vec<Arc<TrackFile>> = Vec::new();
        let shortest = |v: &[Arc<TrackFile>]| v.iter().map(|f| f.size).min().unwrap_or(0);
        for (_, f) in tree.all_files() {
            if f.format() != Format::Dsf {
                continue;
            }
            let v = tree.files_beside(&f);
            if v.len() >= n && shortest(&v) > shortest(&best) {
                best = v;
            }
        }
        assert!(best.len() >= n && shortest(&best) > PROBE, "no folder of {n} files in the corpus");
        best
    }

    /// Reads the first `len` bytes of `f` with a new reader, 128 KiB at a time, as a probe does.
    /// A slow engine stays slow: the decoding measures the machine this runs on.
    fn probe(w: &Arc<Warmer>, engine: &Engine, f: &Arc<TrackFile>, len: u64) -> Arc<Reader> {
        let rate = engine.rates().rate(false, f.layout.channels);
        let r = Arc::new(Reader::new(f.clone()));
        let mut off = 0;
        while off < len.min(f.size) {
            w.read(Some(&r));
            r.read(engine, off, 128 << 10).unwrap();
            off += 128 << 10;
        }
        if let Some(rate) = rate {
            for ch in 1..=6 {
                engine.rates().set(false, ch, rate);
                engine.rates().set(true, ch, rate);
            }
        }
        r
    }

    fn cached(engine: &Engine, f: &TrackFile, n: u32) -> bool {
        engine.cached(&f.disc, f.area, f.start_frame, f.start_frame + n).is_some()
    }

    /// A warmer whose thread the test is: `read` starts none.
    fn driven(engine: &Arc<Engine>, tree: &Arc<Tree>) -> Arc<Warmer> {
        let w = Warmer::with_quiet(engine, tree, Duration::from_millis(50));
        w.lock().thread = true;
        w
    }

    #[test]
    fn a_probe_then_quiet_warms_the_folder() {
        let tree = corpus();
        let engine = slow_engine(64 << 20);
        let files = folder(&tree, 3);
        let w = driven(&engine, &tree);
        let r = probe(&w, &engine, &files[0], PROBE);
        let extent = r.probe_extent(&engine).expect("a probe");
        let n = files[1].head_frames(extent);
        assert!(n > 0);
        assert!(!cached(&engine, &files[1], n));
        // Not before the mount has been quiet.
        let early = Instant::now();
        let (reader, reads) = w.wait_for_probe(Duration::from_secs(2)).expect("quiet after the probe");
        assert!(early.elapsed() >= Duration::from_millis(40));
        assert!(Arc::ptr_eq(&reader, &r));
        let (done, stopped) = w.warm(&engine, &tree, &reader, reads);
        assert!(!stopped);
        assert_eq!(done, files.len() - 1);
        for f in &files {
            assert!(cached(&engine, f, f.head_frames(extent)), "track {} not warmed", f.number);
        }
        // Once per burst.
        assert!(w.wait_for_probe(Duration::from_millis(200)).is_none());
    }

    #[test]
    fn reads_of_several_files_are_a_scan_not_a_probe() {
        let tree = corpus();
        let engine = slow_engine(64 << 20);
        let files = folder(&tree, 3);
        let w = driven(&engine, &tree);
        for f in &files {
            probe(&w, &engine, f, PROBE);
        }
        assert!(w.wait_for_probe(Duration::from_millis(300)).is_none());
        // A sidecar's read neither makes nor breaks a burst.
        let w = driven(&engine, &tree);
        probe(&w, &engine, &files[0], PROBE);
        w.read(None);
        assert!(w.wait_for_probe(Duration::from_secs(2)).is_some());
    }

    #[test]
    fn a_tagger_or_a_fast_machine_is_no_probe() {
        let tree = corpus();
        let files = folder(&tree, 2);
        let engine = slow_engine(64 << 20);
        let w = driven(&engine, &tree);
        // The head's first read and the kernel's read-ahead of it.
        let r = probe(&w, &engine, &files[0], 256 << 10);
        assert_eq!(r.probe_extent(&engine), None);
        let fast = Arc::new(Engine::new(2, 64 << 20).unwrap());
        for ch in 1..=6 {
            fast.rates().set(false, ch, 20.0);
        }
        let r = probe(&w, &fast, &files[0], PROBE);
        assert_eq!(r.probe_extent(&fast), None);
    }

    #[test]
    fn a_read_stops_it() {
        let tree = corpus();
        let engine = slow_engine(64 << 20);
        let files = folder(&tree, 3);
        let w = driven(&engine, &tree);
        let r = probe(&w, &engine, &files[0], PROBE);
        let (reader, reads) = w.wait_for_probe(Duration::from_secs(2)).unwrap();
        w.read(Some(&r));
        let (done, stopped) = w.warm(&engine, &tree, &reader, reads);
        assert!(stopped);
        assert_eq!(done, 0);
        let extent = r.probe_extent(&engine).unwrap();
        assert!(!cached(&engine, &files[1], files[1].head_frames(extent)));
    }

    #[test]
    fn it_stays_within_the_heads_budget() {
        let tree = corpus();
        let files = folder(&tree, 3);
        // Half of 1 MiB holds the probed head and no other.
        let engine = slow_engine(1 << 20);
        let w = driven(&engine, &tree);
        let r = probe(&w, &engine, &files[0], PROBE);
        let extent = r.probe_extent(&engine).unwrap();
        let per = files[1].head_frames(extent) as usize * files[1].cached_frame_bytes(&engine);
        assert!(per > engine.cache().heads_budget() / 2, "the test needs heads of {per} bytes");
        let (reader, reads) = w.wait_for_probe(Duration::from_secs(2)).unwrap();
        assert_eq!(w.warm(&engine, &tree, &reader, reads), (0, false));
    }

    #[test]
    fn the_thread_warms_after_quiet_and_ends_when_closed() {
        let tree = corpus();
        let engine = slow_engine(64 << 20);
        let files = folder(&tree, 2);
        let w = Warmer::with_quiet(&engine, &tree, Duration::from_millis(50));
        let r = probe(&w, &engine, &files[0], PROBE);
        let n = files[1].head_frames(r.probe_extent(&engine).unwrap());
        let deadline = Instant::now() + Duration::from_secs(20);
        while !cached(&engine, &files[1], n) {
            assert!(Instant::now() < deadline, "not warmed");
            std::thread::sleep(Duration::from_millis(20));
        }
        w.close();
        // The thread lets go of the warmer.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&w) > 1 {
            assert!(Instant::now() < deadline, "the thread holds on");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
