// SPDX-License-Identifier: GPL-3.0-or-later

//! The request pools: threads that answer the requests that may read the source (#63).

use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send>;

/// Most threads a pool of [`Workers`] grows to. *Why 64:* the kernel sends at most 64
/// background requests (read-ahead) at once (`max_background`, set in `init`), plus the
/// few synchronous ones of its callers; past that, a request waits for a thread, and the
/// watchdog fails it if it waits too long. Each thread costs a stack of address space,
/// mostly untouched, and nothing while it is parked.
///
/// *The limit this sets (#241):* the watchdog replies `EIO` but can't free a thread blocked
/// on the source, so a client retrying through a long hang takes one more thread each time,
/// and the blocked reads needn't be simultaneous. At 64, reads of every image would wait and
/// fail until the source answered: [`HUNG_READS_PER_IMAGE`](super::HUNG_READS_PER_IMAGE) stops one image's reads short of
/// that.
pub const MAX_POOL_THREADS: usize = 64;

/// A thread above a pool's core count that has had no job for this long ends.
const SPARE_IDLE: Duration = Duration::from_secs(10);

/// Sets `n` to `f` of its value, unless `f` says `None`; whether it was set. (A loop
/// rather than `fetch_update`, deprecated in newer Rust, or `try_update`, newer than the
/// MSRV.)
fn update(n: &AtomicUsize, f: impl Fn(usize) -> Option<usize>) -> bool {
    let mut cur = n.load(Ordering::SeqCst);
    loop {
        let Some(new) = f(cur) else { return false };
        match n.compare_exchange_weak(cur, new, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(now) => cur = now,
        }
    }
}

/// A pool of threads taking jobs from a queue, `core` of them at least, growing up to
/// `max` when a job is queued and no thread is idle.
///
/// *Why it grows (#63, #72):* a thread whose job blocks on a stalled source (a hung NFS
/// share) is lost until the source answers, maybe for hours. A fixed pool would lose its
/// threads one by one, and then every request, of any source, would queue behind them. A
/// spare thread is started instead, so other requests keep being served; the bound keeps a
/// hung source from making threads without end, and the watchdog fails what then waits.
/// [`HUNG_READS_PER_IMAGE`](super::HUNG_READS_PER_IMAGE) keeps one hung image from reaching that bound (#241).
///
/// Not a rayon pool: a rayon worker waiting for another pool (here, the DST decoders)
/// runs other jobs of its own pool meanwhile, and a second read of the same file would
/// then wait, on the same thread, for the lock the first one holds.
#[derive(Debug, Clone)]
pub struct Workers {
    tx: Sender<Job>,
    inner: Arc<WorkersInner>,
}

#[derive(Debug)]
struct WorkersInner {
    rx: Mutex<Receiver<Job>>,
    name: String,
    core: usize,
    max: usize,
    /// Threads alive.
    threads: AtomicUsize,
    /// Threads waiting for a job.
    idle: AtomicUsize,
    /// Jobs sent and not yet taken.
    queued: AtomicUsize,
    /// Used to number new threads.
    started: AtomicUsize,
    /// When a job last found no idle thread, in ms since `epoch`: a spare thread ends
    /// after a job once that is [`SPARE_IDLE`] ago.
    busy_ms: AtomicU64,
    epoch: Instant,
}

impl WorkersInner {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl Workers {
    /// `n` threads named `name-<i>`, growing to [`MAX_POOL_THREADS`] (at least `n`).
    pub fn new(n: usize, name: &str) -> io::Result<Workers> {
        Self::with_max(n, MAX_POOL_THREADS, name)
    }

    /// `core` threads named `name-<i>` (at least one), growing to `max` when busy.
    pub fn with_max(core: usize, max: usize, name: &str) -> io::Result<Workers> {
        let core = core.max(1);
        let (tx, rx) = channel::<Job>();
        let inner = Arc::new(WorkersInner {
            rx: Mutex::new(rx),
            name: name.to_owned(),
            core,
            max: max.max(core),
            threads: AtomicUsize::new(0),
            idle: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            busy_ms: AtomicU64::new(0),
            epoch: Instant::now(),
        });
        for _ in 0..core {
            inner.threads.fetch_add(1, Ordering::SeqCst);
            Self::start(&inner)?;
        }
        Ok(Workers { tx, inner })
    }

    /// Starts one thread, already counted in `threads`.
    fn start(inner: &Arc<WorkersInner>) -> io::Result<()> {
        let i = inner.started.fetch_add(1, Ordering::Relaxed);
        let me = inner.clone();
        let spawned = std::thread::Builder::new().name(format!("{}-{i}", inner.name)).spawn(move || {
            loop {
                me.idle.fetch_add(1, Ordering::SeqCst);
                let job = match me.rx.lock() {
                    Ok(r) => r.recv_timeout(SPARE_IDLE),
                    Err(_) => Err(RecvTimeoutError::Disconnected),
                };
                me.idle.fetch_sub(1, Ordering::SeqCst);
                match job {
                    Ok(job) => {
                        me.queued.fetch_sub(1, Ordering::SeqCst);
                        if catch_unwind(AssertUnwindSafe(job)).is_err() {
                            log::error!("a request thread job panicked");
                        }
                        // *Why here too:* only the thread holding the queue waits with a
                        // timeout; the others wait for the queue's lock. While jobs keep
                        // coming more often than SPARE_IDLE, none of them times out, and
                        // spare threads would stay for good.
                        let quiet = me.now_ms().saturating_sub(me.busy_ms.load(Ordering::SeqCst))
                            >= SPARE_IDLE.as_millis() as u64;
                        if quiet && me.queued.load(Ordering::SeqCst) == 0 {
                            let core = me.core;
                            if update(&me.threads, |t: usize| (t > core).then(|| t - 1)) {
                                return;
                            }
                        }
                    }
                    // A spare thread with nothing to do ends; the core ones stay.
                    Err(RecvTimeoutError::Timeout) => {
                        let core = me.core;
                        let fewer = |t: usize| (t > core).then(|| t - 1);
                        if update(&me.threads, fewer) {
                            return;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        me.threads.fetch_sub(1, Ordering::SeqCst);
                        return;
                    }
                }
            }
        });
        if let Err(e) = spawned {
            inner.threads.fetch_sub(1, Ordering::SeqCst);
            return Err(e);
        }
        Ok(())
    }

    /// Runs `job` on one of the threads; starts one more if none is free and the pool is
    /// below its bound.
    pub fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        let inner = &self.inner;
        inner.queued.fetch_add(1, Ordering::SeqCst);
        if self.tx.send(Box::new(job)).is_err() {
            inner.queued.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        if inner.queued.load(Ordering::SeqCst) <= inner.idle.load(Ordering::SeqCst) {
            return;
        }
        inner.busy_ms.store(inner.now_ms(), Ordering::SeqCst);
        let max = inner.max;
        let more = |t: usize| (t < max).then_some(t + 1);
        if !update(&inner.threads, more) {
            return;
        }
        match Self::start(inner) {
            Ok(()) => log::debug!("{}: every thread busy: {} now", inner.name, self.threads()),
            Err(e) => log::warn!("{}: no spare thread: {e}", inner.name),
        }
    }

    /// Threads alive now.
    pub fn threads(&self) -> usize {
        self.inner.threads.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_pool_grows_past_a_stalled_job_and_shrinks_back() {
        let pool = Workers::with_max(2, 4, "test-pool").unwrap();
        assert_eq!(pool.threads(), 2);
        // Three jobs that stall until released: more than the core threads.
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        for _ in 0..3 {
            let r = release_rx.clone();
            pool.spawn(move || {
                let _ = r.lock().unwrap().recv();
            });
        }
        // Another job still runs, on a spare thread.
        let (done_tx, done_rx) = mpsc::channel();
        pool.spawn(move || done_tx.send(()).unwrap());
        done_rx.recv_timeout(Duration::from_secs(5)).expect("a free job ran");
        assert!(pool.threads() <= 4);
        // Bounded: with every thread stalled, a job waits.
        for _ in 0..3 {
            let r = release_rx.clone();
            pool.spawn(move || {
                let _ = r.lock().unwrap().recv();
            });
        }
        assert_eq!(pool.threads(), 4);
        drop(release_tx);
    }

    /// Spare threads end once the pool has been less busy for [`SPARE_IDLE`], even while
    /// jobs keep coming (a mount being read): else the pool only ever grows, to its
    /// busiest moment (a stall, a burst of read-ahead), and keeps that many threads.
    #[test]
    fn spare_threads_end_under_a_steady_trickle_of_jobs() {
        let pool = Workers::with_max(1, 4, "test-trickle").unwrap();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        for _ in 0..4 {
            let r = release_rx.clone();
            pool.spawn(move || {
                let _ = r.lock().unwrap().recv();
            });
        }
        assert_eq!(pool.threads(), 4);
        drop(release_tx);
        // One quick job every second, far less than one thread can do.
        let deadline = Instant::now() + SPARE_IDLE * 3;
        while Instant::now() < deadline && pool.threads() > 1 {
            let (tx, rx) = mpsc::channel();
            pool.spawn(move || tx.send(()).unwrap());
            rx.recv_timeout(Duration::from_secs(5)).expect("the job ran");
            std::thread::sleep(Duration::from_secs(1));
        }
        assert_eq!(pool.threads(), 1, "spare threads kept under a trickle of jobs");
    }
}
