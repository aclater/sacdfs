// SPDX-License-Identifier: GPL-3.0-or-later

//! Warming a folder's heads after a player's probe (#510) or a listing of the folder (#456).
//!
//! A network player of #456 probes a track as soon as it lists the album, and when playback
//! starts, minutes later, it re-reads the head of every other file of the album before (and
//! while) it opens the track. On a slow CPU the first start after a mount decoded all those
//! heads then, about 4.5 s of a 32-bit Atom's CPU, while the player waited. So when one file's
//! head was probed and the mount then goes [`QUIET`], the other files of its folder have their
//! heads decoded in the background, as far as the probe read, and kept as heads.
//!
//! - **After a probe of one file, or a listing of the folder:** the reads of audio since the mount was last quiet were of
//!   one file only, from its start, past the kernel's read-ahead of a first read, and within its
//!   head ([`Reader::probe_extent`]). A library scanner reads every file of a folder, so the
//!   reads before a pause cover several files; a tagger stops within the kernel's read-ahead;
//!   a player that plays reads past the head. A listing of a folder of track files warms that
//!   folder's heads ([`LISTED_HEAD`] of each, from its first file) once it has been the last
//!   folder listed for [`SETTLE`] and the mount has had no read for [`QUIET`]. *Why (#456,
//!   0.7.2's `-vv` log):* the player listed the album, and when playback was started a few
//!   seconds later it read the heads of all 12 files, one file at a time, taking turns with its
//!   track: no probe of one file came first, so nothing was warm, and its track got 0.56 to 0.93
//!   s of audio a second for 7.4 s while each re-read decoded (99 to 193 ms per 128 KiB on
//!   average). After a wait it started cleanly. Listing another folder of track files moves the
//!   warming there. On a machine whose speed is not measured yet the first frames decoded
//!   measure it, and a fast one stops there.
//! - **Only on a CPU slower than players' start-up bursts** (`--start-burst`): a fast one decodes
//!   the heads in the moment the player asks.
//! - **Only while the mount is idle:** it starts after [`QUIET`] with no read, and stops at the
//!   next read, of any file, after the frames it is decoding (one per decoder thread). *Why
//!   stop rather than go on beside it (#510, s071d's measurements):* decoding heads beside a
//!   player starting a track made its stalls 0.7 to 1.3 s longer on Atom-speed cores.
//! - **Not while the mount is short of memory** ([`Tree::refusing`]).
//! - **Within the heads an idle release keeps** ([`crate::engine::FrameCache::heads_budget`]),
//!   with the probed file's head kept the most recent, so they are there minutes later.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::engine::Engine;
use crate::track::{Format, Reader, TrackFile};
use crate::tree::Tree;

/// How long the mount goes without a read before warming starts. *Why 2 s:* a player starting a
/// track re-reads the album's heads and reads its track with gaps under 1 s (#456), and a
/// scanner goes on to the next file at once; the player of #456 waited 3 minutes after its probe.
pub const QUIET: Duration = Duration::from_secs(2);

/// How long a folder listed has to stay the last listed before its heads are warmed. *Why 1 s:*
/// the player of #456 lists the folder it shows together with its parent and siblings (looking
/// for disc folders in them) within a second, and ends with the one it shows.
pub const SETTLE: Duration = Duration::from_secs(1);

/// Bytes of audio of each file's head that a listing warms. *Why:* the player of #456 re-reads
/// 12 reads of 128 KiB of each file, less the DSF header (1572772 bytes of audio), when it starts
/// a track; 12 such heads of a 6-channel album take about 18 MiB of the heads' budget.
pub const LISTED_HEAD: u64 = 1536 << 10;

/// Warms a folder's heads after a probe; see the module.
#[derive(Debug)]
pub struct Warmer {
    engine: Weak<Engine>,
    tree: Weak<Tree>,
    quiet: Duration,
    settle: Duration,
    clock: Clock,
    /// Reads so far: warming stops when this moves.
    reads: AtomicU64,
    /// Changes of the folder listed last: warming stops when this moves.
    listings: AtomicU64,
    /// When the last read came, in ms since `epoch` plus one (0: none yet).
    last: AtomicU64,
    state: Mutex<State>,
    wake: Condvar,
}

/// A probe of a file: the file, the reader's number and how far into the audio it read. For a
/// listing: the folder's first file, reader 0, [`LISTED_HEAD`].
#[derive(Debug)]
struct Probe {
    file: Arc<TrackFile>,
    reader: u64,
    extent: u64,
    /// From a listing of the folder, not a probe.
    listed: bool,
}

/// The reads and the listings so far: warming stops when either moves.
type Mark = (u64, u64);

/// The warmer's time, in ms plus one (0 stands for "no read yet"). *Why not `Instant` directly:*
/// the tests set it, so that what counts as quiet does not hang on a busy machine's scheduling
/// (#563).
#[derive(Debug)]
enum Clock {
    /// Time since the warmer was made.
    Real(Instant),
    /// Time the test sets.
    #[cfg(test)]
    Manual(AtomicU64),
}

impl Clock {
    fn now_ms(&self) -> u64 {
        match self {
            Clock::Real(epoch) => epoch.elapsed().as_millis() as u64 + 1,
            #[cfg(test)]
            Clock::Manual(ms) => ms.load(Ordering::SeqCst) + 1,
        }
    }
}

/// What a burst of reads gives the warming thread now.
enum Ready {
    /// A probe (or a listing), quiet since, and the reads and listings so far.
    Probe(Probe, Mark),
    /// A probe, not yet quiet for this many ms.
    NotYet(u64),
    /// No probe to warm from.
    Nothing,
}

#[derive(Debug, Default)]
struct State {
    /// The track files read since the mount was last quiet.
    burst: Option<Burst>,
    /// The thread runs.
    thread: bool,
    /// The volume is gone: the thread ends.
    closed: bool,
    /// The folder of track files listed last.
    listed: Option<Listed>,
}

#[derive(Debug)]
struct Listed {
    /// Its first file.
    file: Arc<TrackFile>,
    /// When it was last listed, as [`Warmer::now_ms`].
    at: u64,
    /// Taken by the thread already, since it was last listed.
    taken: bool,
}

#[derive(Debug)]
struct Burst {
    /// The first file read.
    file: Arc<TrackFile>,
    /// After its last read: the reader, and how far it read as a probe, if it is one
    /// ([`Reader::probe_extent`]). *Why not the reader itself:* held here, a closed reader would
    /// still count as open where others look ([`crate::track::Traffic`]).
    probe: Option<(u64, u64)>,
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
        Self::with_clock(engine, tree, quiet, SETTLE, Clock::Real(Instant::now()))
    }

    fn with_clock(
        engine: &Arc<Engine>,
        tree: &Arc<Tree>,
        quiet: Duration,
        settle: Duration,
        clock: Clock,
    ) -> Arc<Warmer> {
        Arc::new(Warmer {
            engine: Arc::downgrade(engine),
            tree: Arc::downgrade(tree),
            quiet,
            settle,
            clock,
            reads: AtomicU64::new(0),
            listings: AtomicU64::new(0),
            last: AtomicU64::new(0),
            state: Mutex::default(),
            wake: Condvar::new(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    /// Notes that a read arrived: of a track file's audio (`file`), or of something else (`None`:
    /// a sidecar, or a track file's header or tag). Any read stops warming; only reads of audio
    /// make the reads since the mount was last quiet a probe's (one file) or not.
    pub fn read(self: &Arc<Self>, file: Option<&Arc<TrackFile>>) {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let now = self.now_ms();
        let last = self.last.swap(now, Ordering::SeqCst);
        let quiet_before = last == 0 || now.saturating_sub(last) >= self.quiet.as_millis() as u64;
        let mut st = self.lock();
        if quiet_before {
            st.burst = None;
        }
        let Some(file) = file else { return };
        match &mut st.burst {
            None => st.burst = Some(Burst { file: file.clone(), probe: None, others: false, taken: false }),
            Some(b) if b.others || Arc::ptr_eq(&b.file, file) => return,
            Some(b) => {
                b.others = true;
                return;
            }
        }
        self.run(&mut st);
    }

    /// Starts the thread if it does not run, or wakes it.
    fn run(self: &Arc<Self>, st: &mut State) {
        if !st.thread && !st.closed {
            let me = Arc::downgrade(self);
            let spawned = std::thread::Builder::new().name("sacdfs-warm".to_owned()).spawn(move || {
                // Says when it ends, also by a panic: warming is then off for the mount.
                struct Ends;
                impl Drop for Ends {
                    fn drop(&mut self) {
                        log::debug!(
                            "warm: thread ended{}",
                            if std::thread::panicking() { " by a panic" } else { "" }
                        );
                    }
                }
                let _ends = Ends;
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

    /// Notes that a folder whose first track file is `file` was listed. A listing of another
    /// folder than the last one stops warming.
    pub fn listed(self: &Arc<Self>, file: &Arc<TrackFile>) {
        let now = self.now_ms();
        let mut st = self.lock();
        match &mut st.listed {
            // Listed again: warmed again (the heads warm already cost nothing), not stopped.
            Some(l) if l.file.beside(file) => {
                l.at = now;
                l.taken = false;
            }
            l => {
                *l = Some(Listed { file: file.clone(), at: now, taken: false });
                self.listings.fetch_add(1, Ordering::SeqCst);
            }
        }
        self.run(&mut st);
    }

    /// Notes, after `reader` read the file's audio, how far it read as a probe, if it is one.
    pub fn done(&self, reader: &Reader, engine: &Engine) {
        let mut st = self.lock();
        let Some(b) = st.burst.as_mut().filter(|b| !b.others && Arc::ptr_eq(&b.file, &reader.file)) else {
            return;
        };
        b.probe = reader.probe_extent(engine).map(|x| (reader.id(), x));
        if b.probe.is_some() {
            self.wake.notify_all();
        }
    }

    /// Ends the thread.
    pub fn close(&self) {
        self.lock().closed = true;
        self.wake.notify_all();
    }

    /// Waits for a probe or a listing followed by quiet, and warms its folder; `false` when closed. Waits at
    /// most a few seconds without one, so that the thread lets go of the warmer in between.
    fn next(&self) -> bool {
        let Some((probe, mark)) = self.wait_for_probe(Duration::from_secs(10)) else {
            return !self.lock().closed;
        };
        let (Some(engine), Some(tree)) = (self.engine.upgrade(), self.tree.upgrade()) else {
            return false;
        };
        self.warm(&engine, &tree, &probe, mark);
        true
    }

    /// The probe of a burst of reads of one file, once the mount has been quiet since for
    /// `quiet`, and the reads so far then; `None` if there is none within `wait`, or closed.
    fn wait_for_probe(&self, wait: Duration) -> Option<(Probe, Mark)> {
        let deadline = Instant::now() + wait;
        let mut st = self.lock();
        loop {
            if st.closed {
                return None;
            }
            let now = Instant::now();
            let left = deadline.saturating_duration_since(now);
            match self.ready(&mut st) {
                Ready::Probe(probe, mark) => return Some((probe, mark)),
                Ready::NotYet(ms) => {
                    let more = Duration::from_millis(ms);
                    st = self.wake.wait_timeout(st, more).unwrap_or_else(|p| p.into_inner()).0;
                    continue;
                }
                Ready::Nothing => {}
            }
            if left.is_zero() {
                return None;
            }
            st = self.wake.wait_timeout(st, left).unwrap_or_else(|p| p.into_inner()).0;
        }
    }

    /// Takes the burst's probe if the mount has been quiet since for `quiet`, or else the folder
    /// listed last once it has settled; says how much longer to wait if not yet.
    fn ready(&self, st: &mut State) -> Ready {
        let now = self.now_ms();
        let since = now.saturating_sub(self.last.load(Ordering::SeqCst));
        let quiet = self.quiet.as_millis() as u64;
        let mark = || (self.reads.load(Ordering::SeqCst), self.listings.load(Ordering::SeqCst));
        if let Some(b) = st.burst.as_mut().filter(|b| !b.others && !b.taken)
            && let Some((reader, extent)) = b.probe
        {
            if since < quiet {
                return Ready::NotYet(quiet - since);
            }
            b.taken = true;
            return Ready::Probe(Probe { file: b.file.clone(), reader, extent, listed: false }, mark());
        }
        let Some(l) = st.listed.as_mut().filter(|l| !l.taken) else {
            return Ready::Nothing;
        };
        let settle = (self.settle.as_millis() as u64).saturating_sub(now.saturating_sub(l.at));
        let wait = quiet.saturating_sub(since).max(settle);
        if wait > 0 {
            return Ready::NotYet(wait);
        }
        l.taken = true;
        Ready::Probe(Probe { file: l.file.clone(), reader: 0, extent: LISTED_HEAD, listed: true }, mark())
    }

    /// Whether a read, or a listing of another folder, came since `mark`.
    fn interrupted(&self, mark: Mark) -> bool {
        (self.reads.load(Ordering::SeqCst), self.listings.load(Ordering::SeqCst)) != mark
    }

    /// Decodes the heads of the files beside `probe`'s, as far as it read, until a read comes
    /// (the reads were `reads` before). How many files it went through, and whether a read
    /// stopped it.
    fn warm(&self, engine: &Engine, tree: &Tree, probe: &Probe, mark: Mark) -> (usize, bool) {
        let (file, extent) = (&probe.file, probe.extent);
        // Short of memory (#237): nothing more is decoded for later.
        if tree.refusing() {
            return (0, false);
        }
        let files = tree.files_beside(file);
        let Some(at) = files.iter().position(|f| Arc::ptr_eq(f, file) || f.number == file.number) else {
            return (0, false);
        };
        // After a probe: the files after the probed one first, as an album plays; the probed one
        // last, so that its head is the most recently read and outlives the others' (an idle
        // release keeps the most recent). After a listing: in order, as the player of #456
        // re-reads them; none is its own.
        let order: Vec<_> = if probe.listed {
            files.iter().collect()
        } else {
            files[at + 1..].iter().chain(&files[..at]).chain(std::iter::once(file)).collect()
        };
        let own = !probe.listed;
        let who = if probe.listed { "listed".to_owned() } else { format!("r={}", probe.reader) };
        let mut room = engine.cache().heads_budget();
        let began = Instant::now();
        crate::trace_line!(
            "warm {who} start: {} files beside, {} bytes of audio each",
            order.len() - usize::from(own),
            extent
        );
        let threads = engine.threads().max(1) as u32;
        let mut done = 0;
        if own {
            // The probed file's head is counted first: it must fit as well.
            room = room.saturating_sub(file.head_frames(extent) as usize * file.cached_frame_bytes(engine));
        }
        for (i, f) in order.iter().enumerate() {
            let last = own && i + 1 == order.len();
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
                if self.interrupted(mark) {
                    crate::trace_line!(
                        "warm {who} stopped: a read or listing, after {done} files, took={}",
                        crate::trace::Ms(began.elapsed())
                    );
                    return (done, true);
                }
                // A probe only counts on a slow machine. A listing on one whose speed is not
                // measured yet decodes enough frames to measure it, and stops on a fast one.
                let slow = slow(engine, f);
                if probe.listed && slow == Some(false) {
                    crate::trace_line!("warm {who} stopped: a fast machine, after {done} files");
                    return (done, false);
                }
                let step = if slow.is_none() { threads.max(crate::engine::RATE_BATCH) } else { threads };
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
        crate::trace_line!("warm {who} done: {done} files, took={}", crate::trace::Ms(began.elapsed()));
        log::debug!("warm: decoded the heads of {done} files of a folder");
        (done, false)
    }
}

/// Whether `engine` decodes `f`'s frames slower than players' start-up bursts (`--start-burst`);
/// `None` if not measured yet.
fn slow(engine: &Engine, f: &TrackFile) -> Option<bool> {
    let burst = engine.start_burst().rate;
    if burst <= 0.0 {
        return Some(false);
    }
    engine.rates().rate(f.format() == Format::Wav, f.layout.channels).map(|r| r < burst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::TrackFile;

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
            w.read(Some(f));
            r.read(engine, off, 128 << 10).unwrap();
            if let Some(rate) = rate {
                for ch in 1..=6 {
                    engine.rates().set(false, ch, rate);
                    engine.rates().set(true, ch, rate);
                }
            }
            w.done(&r, engine);
            off += 128 << 10;
        }
        r
    }

    fn cached(engine: &Engine, f: &TrackFile, n: u32) -> bool {
        engine.cached(&f.disc, f.area, f.start_frame, f.start_frame + n).is_some()
    }

    /// The tests' quiet, in ms of the warmer's clock.
    const QUIET_MS: u64 = 50;
    /// The tests' settle, in ms of the warmer's clock.
    const SETTLE_MS: u64 = 20;

    /// A warmer on a clock that only the test moves (#563): reads, however long they take on a busy
    /// machine, come at the same instant unless the test moves the clock between them.
    fn manual(engine: &Arc<Engine>, tree: &Arc<Tree>) -> Arc<Warmer> {
        Warmer::with_clock(
            engine,
            tree,
            Duration::from_millis(QUIET_MS),
            Duration::from_millis(SETTLE_MS),
            Clock::Manual(AtomicU64::new(0)),
        )
    }

    /// A warmer on the test's clock whose thread the test is: `read` starts none.
    fn driven(engine: &Arc<Engine>, tree: &Arc<Tree>) -> Arc<Warmer> {
        let w = manual(engine, tree);
        w.lock().thread = true;
        w
    }

    impl Warmer {
        /// Moves the test's clock on by `ms`, and wakes the thread.
        fn advance(&self, ms: u64) {
            let Clock::Manual(t) = &self.clock else { panic!("not the test's clock") };
            t.fetch_add(ms, Ordering::SeqCst);
            let _st = self.lock();
            self.wake.notify_all();
        }

        /// Whether the thread would take a probe now (and takes it), without waiting.
        fn take(&self) -> bool {
            matches!(self.ready(&mut self.lock()), Ready::Probe(..))
        }
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
        // Not before the mount has been quiet for `QUIET`.
        assert!(matches!(w.ready(&mut w.lock()), Ready::NotYet(QUIET_MS)));
        w.advance(QUIET_MS - 1);
        assert!(matches!(w.ready(&mut w.lock()), Ready::NotYet(1)));
        w.advance(1);
        // Quiet now: no waiting.
        let (p, reads) = w.wait_for_probe(Duration::ZERO).expect("quiet after the probe");
        assert_eq!((p.reader, p.extent), (r.id(), extent));
        let (done, stopped) = w.warm(&engine, &tree, &p, reads);
        assert!(!stopped);
        assert_eq!(done, files.len() - 1);
        for f in &files {
            assert!(cached(&engine, f, f.head_frames(extent)), "track {} not warmed", f.number);
        }
        // Once per burst.
        w.advance(10 * QUIET_MS);
        assert!(w.wait_for_probe(Duration::ZERO).is_none());
    }

    #[test]
    fn reads_of_several_files_are_a_scan_not_a_probe() {
        let tree = corpus();
        let engine = slow_engine(64 << 20);
        let files = folder(&tree, 3);
        let w = driven(&engine, &tree);
        // Each file read 1 ms of the clock after the last, well within the quiet.
        for f in &files {
            probe(&w, &engine, f, PROBE);
            w.advance(1);
        }
        w.advance(10 * QUIET_MS);
        assert!(w.wait_for_probe(Duration::ZERO).is_none());
        // A sidecar's read neither makes nor breaks a burst, but the mount is not quiet after it.
        let w = driven(&engine, &tree);
        probe(&w, &engine, &files[0], PROBE);
        w.advance(QUIET_MS - 1);
        w.read(None);
        w.advance(QUIET_MS - 1);
        assert!(!w.take());
        w.advance(1);
        assert!(w.wait_for_probe(Duration::ZERO).is_some());
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
        w.advance(10 * QUIET_MS);
        assert!(w.wait_for_probe(Duration::ZERO).is_none());
    }

    #[test]
    fn a_read_stops_it() {
        let tree = corpus();
        let engine = slow_engine(64 << 20);
        let files = folder(&tree, 3);
        let w = driven(&engine, &tree);
        let r = probe(&w, &engine, &files[0], PROBE);
        w.advance(QUIET_MS);
        let (p, reads) = w.wait_for_probe(Duration::ZERO).unwrap();
        w.read(Some(&files[0]));
        let (done, stopped) = w.warm(&engine, &tree, &p, reads);
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
        w.advance(QUIET_MS);
        let (p, reads) = w.wait_for_probe(Duration::ZERO).unwrap();
        assert_eq!(w.warm(&engine, &tree, &p, reads), (0, false));
    }

    /// A file of another folder than `files`'.
    fn elsewhere(tree: &Tree, files: &[Arc<TrackFile>]) -> Arc<TrackFile> {
        tree.all_files().into_iter().map(|(_, f)| f).find(|f| !f.beside(&files[0])).expect("a second folder")
    }

    #[test]
    fn a_listing_then_quiet_warms_the_folder() {
        let tree = corpus();
        let engine = slow_engine(64 << 20);
        let files = folder(&tree, 3);
        let w = driven(&engine, &tree);
        w.advance(QUIET_MS);
        w.listed(&files[0]);
        // Not before it has settled: the listing of its parent and siblings may follow.
        assert!(matches!(w.ready(&mut w.lock()), Ready::NotYet(SETTLE_MS)));
        w.advance(SETTLE_MS);
        let (p, mark) = w.wait_for_probe(Duration::ZERO).expect("settled and quiet");
        assert!(p.listed);
        assert_eq!(p.extent, LISTED_HEAD);
        let (done, stopped) = w.warm(&engine, &tree, &p, mark);
        assert!(!stopped);
        assert_eq!(done, files.len());
        for f in &files {
            assert!(cached(&engine, f, f.head_frames(LISTED_HEAD)), "track {} not warmed", f.number);
        }
        // Once per listing; a listing again warms again.
        assert!(w.wait_for_probe(Duration::ZERO).is_none());
        w.listed(&files[1]);
        w.advance(SETTLE_MS);
        assert!(w.wait_for_probe(Duration::ZERO).is_some());
    }

    #[test]
    fn a_listing_waits_for_quiet_and_a_read_or_another_folder_stops_it() {
        let tree = corpus();
        let engine = slow_engine(64 << 20);
        let files = folder(&tree, 3);
        let other = elsewhere(&tree, &files);
        let w = driven(&engine, &tree);
        // A player reading: the listing waits for the mount to be quiet.
        probe(&w, &engine, &files[0], 64 << 10);
        w.listed(&files[0]);
        w.advance(SETTLE_MS);
        assert!(matches!(w.ready(&mut w.lock()), Ready::NotYet(ms) if ms == QUIET_MS - SETTLE_MS));
        w.advance(QUIET_MS);
        let (p, mark) = w.wait_for_probe(Duration::ZERO).unwrap();
        // Listing the same folder again does not stop it; a read does.
        w.listed(&files[1]);
        assert!(!w.interrupted(mark));
        w.read(None);
        assert_eq!(w.warm(&engine, &tree, &p, mark), (0, true));
        // A listing of another folder stops it, and the next warming is of that folder.
        w.advance(QUIET_MS);
        let (_, mark) = w.wait_for_probe(Duration::ZERO).unwrap();
        w.listed(&other);
        assert!(w.interrupted(mark));
        w.advance(SETTLE_MS);
        let (p, _) = w.wait_for_probe(Duration::ZERO).unwrap();
        assert!(p.file.beside(&other));
    }

    #[test]
    fn a_listing_on_a_fast_machine_measures_it_and_stops() {
        let tree = corpus();
        let files = folder(&tree, 3);
        // Not measured yet, and no machine is slower than this.
        let burst = |rate| crate::engine::StartBurst { rate, secs: 8 };
        let fast = Arc::new(Engine::new(2, 64 << 20).unwrap().with_start_burst(burst(1e-6)));
        let w = driven(&fast, &tree);
        w.advance(QUIET_MS);
        w.listed(&files[0]);
        w.advance(SETTLE_MS);
        let (p, mark) = w.wait_for_probe(Duration::ZERO).unwrap();
        assert_eq!(w.warm(&fast, &tree, &p, mark), (0, false));
        let n = crate::engine::RATE_BATCH;
        assert!(cached(&fast, &files[0], n), "the frames that measured it");
        assert!(!cached(&fast, &files[0], files[0].head_frames(LISTED_HEAD)));
        assert!(fast.rates().rate(false, files[0].layout.channels).is_some());
        // The same, unmeasured, on a machine slower than every player: it warms the folder.
        let slow = Arc::new(Engine::new(2, 64 << 20).unwrap().with_start_burst(burst(1e6)));
        let w = driven(&slow, &tree);
        w.advance(QUIET_MS);
        w.listed(&files[0]);
        w.advance(SETTLE_MS);
        let (p, mark) = w.wait_for_probe(Duration::ZERO).unwrap();
        assert_eq!(w.warm(&slow, &tree, &p, mark), (files.len(), false));
        // With the help for slow machines off, nothing.
        let off = Arc::new(Engine::new(2, 64 << 20).unwrap().with_start_burst(burst(0.0)));
        let w = driven(&off, &tree);
        w.advance(QUIET_MS);
        w.listed(&files[0]);
        w.advance(SETTLE_MS);
        let (p, mark) = w.wait_for_probe(Duration::ZERO).unwrap();
        assert_eq!(w.warm(&off, &tree, &p, mark), (0, false));
        assert!(!cached(&off, &files[0], 1));
    }

    /// The deadlines here only keep a broken thread from hanging the test; nothing waits on them.
    #[test]
    fn the_thread_warms_after_quiet_and_ends_when_closed() {
        let tree = corpus();
        let engine = slow_engine(64 << 20);
        let files = folder(&tree, 2);
        let w = manual(&engine, &tree);
        let r = probe(&w, &engine, &files[0], PROBE);
        let n = files[1].head_frames(r.probe_extent(&engine).unwrap());
        // The clock has not moved: the mount is not quiet, so the thread cannot have warmed.
        assert!(!cached(&engine, &files[1], n));
        w.advance(QUIET_MS);
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
