// SPDX-License-Identifier: GPL-3.0-or-later

//! Request deadlines: no FUSE request waits for good (#63, #72).
//!
//! Every request that may touch the source (a read, a lookup, a listing) is answered on a
//! request thread, and its reply is put in a [`Pending`] watched by one watchdog thread. A
//! request still unanswered after the timeout (`--request-timeout`, 30 s by default) is
//! logged, with what it was doing and on which thread, and failed with `EIO`. The thread
//! that was serving it keeps going (a thread blocked in the kernel on a hung share cannot be
//! stopped); when it finishes, it finds the reply gone and only logs.
//!
//! *Why fail rather than wait:* sacdfs never abandons a FUSE request on its own
//! (fuser has no `FUSE_INTERRUPT`, fuser#14), so a source that stops answering (a hung NFS
//! or SMB share, a disk in error recovery) would otherwise hold the client, and an SMB or
//! NFS server re-exporting the mount, for hours (#72: 6 h). An error after a bound lets the
//! client retry or give up. libfuse 3.18 ("request timeouts"), JuiceFS 1.3 and macFUSE 5.5
//! (`daemon_timeout`) did the same.
//!
//! *Why `EIO`:* it is what a failing disk gives, so every client handles it; `EAGAIN` from a
//! blocking read is taken by some programs as "try again at once", which would spin on a
//! stalled source. *Why 30 s:* far above the slowest healthy request measured (a cold DST
//! read of a 6-channel area, or the first listing of a directory of images on a share,
//! take well under a second to a few seconds), and below the 60 s that SMB clients wait
//! before they give up on a server.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::Thread;
use std::time::{Duration, Instant};

use crate::vfs::Error;

/// How long a request may wait by default before it fails.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A reply that can be failed with an error. Each front end implements it for its replies
/// (the FUSE front end for fuser's, in [`crate::fs`]). A reply dropped unanswered must
/// answer [`Error::Io`] (fuser's do), so that a request whose job never ran is not left open.
pub trait Fail: Send + 'static {
    /// Answers the request with `err`.
    fn fail(self, err: Error);
}

/// What a request asks for, for the log line of one that timed out.
pub struct What {
    op: &'static str,
    detail: Box<dyn fmt::Display + Send + Sync>,
}

impl What {
    /// Request `op`, described by `detail` (formatted only if it times out).
    pub fn new(op: &'static str, detail: impl fmt::Display + Send + Sync + 'static) -> What {
        What { op, detail: Box::new(detail) }
    }
}

impl fmt::Debug for What {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "What({self})")
    }
}

impl fmt::Display for What {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} of {}", self.op, self.detail)
    }
}

struct State<R> {
    reply: Option<R>,
    /// The thread serving it, and since when; `None` while it is queued.
    running: Option<(Thread, Instant)>,
}

struct Slot<R> {
    what: What,
    started: Instant,
    state: Mutex<State<R>>,
}

impl<R> Slot<R> {
    fn lock(&self) -> MutexGuard<'_, State<R>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A watched request, as the watchdog sees it.
trait Watched: Send + Sync {
    fn started(&self) -> Instant;
    /// Fails the request with `EIO`, logging why, unless it was answered meanwhile.
    fn expire(&self, timeout: Duration);
}

impl<R: Fail> Watched for Slot<R> {
    fn started(&self) -> Instant {
        self.started
    }

    fn expire(&self, timeout: Duration) {
        let (reply, running) = {
            let mut st = self.lock();
            (st.reply.take(), st.running.clone())
        };
        let Some(reply) = reply else { return };
        let waited = self.started.elapsed().as_secs_f64();
        match running {
            Some((thread, since)) => log::warn!(
                "{} has waited {waited:.1} s (running on {} for {:.1} s): failing it with EIO \
                 after the {} s request timeout",
                self.what,
                thread.name().unwrap_or("a request thread"),
                since.elapsed().as_secs_f64(),
                timeout.as_secs_f64()
            ),
            None => log::warn!(
                "{} has waited {waited:.1} s, queued (every request thread busy): failing it \
                 with EIO after the {} s request timeout",
                self.what,
                timeout.as_secs_f64()
            ),
        }
        reply.fail(Error::Io);
    }
}

struct Inner {
    timeout: Duration,
    pending: Mutex<HashMap<u64, Arc<dyn Watched>>>,
    next: AtomicU64,
    timed_out: AtomicU64,
}

impl Inner {
    fn pending(&self) -> MutexGuard<'_, HashMap<u64, Arc<dyn Watched>>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The watchdog: fails requests that waited longer than its timeout.
#[derive(Clone)]
pub struct Watchdog {
    inner: Option<Arc<Inner>>,
}

impl fmt::Debug for Watchdog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watchdog").field("timeout", &self.timeout()).finish_non_exhaustive()
    }
}

impl Watchdog {
    /// A watchdog failing requests after `timeout`, checking a few times per timeout on a
    /// thread of its own; `None` (or zero) watches nothing.
    pub fn new(timeout: Option<Duration>) -> std::io::Result<Watchdog> {
        let Some(timeout) = timeout.filter(|t| !t.is_zero()) else {
            return Ok(Watchdog { inner: None });
        };
        let inner = Arc::new(Inner {
            timeout,
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            timed_out: AtomicU64::new(0),
        });
        let weak = Arc::downgrade(&inner);
        let tick = (timeout / 4).clamp(Duration::from_millis(50), Duration::from_secs(1));
        std::thread::Builder::new().name("sacdfs-watchdog".to_owned()).spawn(move || {
            loop {
                std::thread::sleep(tick);
                let Some(inner) = weak.upgrade() else { return };
                let expired: Vec<Arc<dyn Watched>> = {
                    let mut p = inner.pending();
                    let ids: Vec<u64> = p
                        .iter()
                        .filter(|(_, w)| w.started().elapsed() >= timeout)
                        .map(|(id, _)| *id)
                        .collect();
                    ids.iter().filter_map(|id| p.remove(id)).collect()
                };
                for w in expired {
                    inner.timed_out.fetch_add(1, Ordering::Relaxed);
                    w.expire(timeout);
                }
            }
        })?;
        Ok(Watchdog { inner: Some(inner) })
    }

    /// A watchdog that watches nothing.
    pub fn off() -> Watchdog {
        Watchdog { inner: None }
    }

    /// The timeout, if requests are watched.
    pub fn timeout(&self) -> Option<Duration> {
        self.inner.as_ref().map(|i| i.timeout)
    }

    /// Requests failed for their timeout so far.
    pub fn timed_out(&self) -> u64 {
        self.inner.as_ref().map_or(0, |i| i.timed_out.load(Ordering::Relaxed))
    }

    /// Requests being watched now.
    pub fn pending(&self) -> usize {
        self.inner.as_ref().map_or(0, |i| i.pending().len())
    }

    /// Watches `reply`, the reply of request `what`, from now on.
    pub fn watch<R: Fail>(&self, what: What, reply: R) -> Pending<R> {
        let slot = Arc::new(Slot {
            what,
            started: Instant::now(),
            state: Mutex::new(State { reply: Some(reply), running: None }),
        });
        let id = match &self.inner {
            Some(inner) => {
                let id = inner.next.fetch_add(1, Ordering::Relaxed);
                inner.pending().insert(id, slot.clone());
                id
            }
            None => 0,
        };
        Pending { id, inner: self.inner.clone(), slot }
    }
}

/// The reply of a watched request: taken with [`Pending::take`] to answer it, unless the
/// watchdog failed it first.
pub struct Pending<R: Fail> {
    id: u64,
    inner: Option<Arc<Inner>>,
    slot: Arc<Slot<R>>,
}

impl<R: Fail> fmt::Debug for Pending<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pending").field("what", &self.slot.what).finish_non_exhaustive()
    }
}

impl<R: Fail> Pending<R> {
    /// Notes that the current thread now serves the request (it was queued until now).
    pub fn running(&self) {
        self.slot.lock().running = Some((std::thread::current(), Instant::now()));
    }

    /// The reply, to answer the request with; `None` if the watchdog failed it already
    /// (logged here: the work finished after all).
    pub fn take(self) -> Option<R> {
        let reply = self.slot.lock().reply.take();
        if reply.is_none() {
            log::info!(
                "{} finished {:.1} s after it was asked for, after its timeout: its answer is dropped",
                self.slot.what,
                self.slot.started.elapsed().as_secs_f64()
            );
        }
        reply
    }
}

impl<R: Fail> Drop for Pending<R> {
    fn drop(&mut self) {
        if let Some(inner) = &self.inner {
            inner.pending().remove(&self.id);
        }
        // A reply never taken (a job dropped unrun) answers EIO when dropped (fuser does that).
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A fake reply that reports how it was answered.
    struct Probe(mpsc::Sender<Option<Error>>);

    impl Fail for Probe {
        fn fail(self, err: Error) {
            let _ = self.0.send(Some(err));
        }
    }

    impl Probe {
        fn ok(self) {
            let _ = self.0.send(None);
        }
    }

    #[test]
    fn a_request_answered_in_time_is_left_alone() {
        let wd = Watchdog::new(Some(Duration::from_millis(200))).unwrap();
        let (tx, rx) = mpsc::channel();
        let p = wd.watch(What::new("read", "inode 2"), Probe(tx));
        assert_eq!(wd.pending(), 1);
        p.running();
        p.take().unwrap().ok();
        assert_eq!(rx.recv().unwrap(), None);
        assert_eq!(wd.pending(), 0);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(wd.timed_out(), 0);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_stalled_request_fails_with_eio_within_the_timeout() {
        let wd = Watchdog::new(Some(Duration::from_millis(200))).unwrap();
        let (tx, rx) = mpsc::channel();
        let t = Instant::now();
        let p = wd.watch(What::new("lookup", "inode 1"), Probe(tx));
        p.running();
        let got = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got, Some(Error::Io));
        let waited = t.elapsed();
        assert!(waited >= Duration::from_millis(200) && waited < Duration::from_secs(2), "{waited:?}");
        // The work finishing late finds no reply.
        assert!(p.take().is_none());
        assert_eq!(wd.timed_out(), 1);
        assert_eq!(wd.pending(), 0);
    }

    #[test]
    fn a_queued_request_times_out_too() {
        let wd = Watchdog::new(Some(Duration::from_millis(100))).unwrap();
        let (tx, rx) = mpsc::channel();
        let _p = wd.watch(What::new("read", "inode 3"), Probe(tx));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Some(Error::Io));
    }

    #[test]
    fn off_watches_nothing() {
        for wd in
            [Watchdog::off(), Watchdog::new(Some(Duration::ZERO)).unwrap(), Watchdog::new(None).unwrap()]
        {
            assert_eq!(wd.timeout(), None);
            let (tx, rx) = mpsc::channel();
            let p = wd.watch(What::new("read", "inode 4"), Probe(tx));
            assert_eq!(wd.pending(), 0);
            std::thread::sleep(Duration::from_millis(60));
            assert!(rx.try_recv().is_err());
            p.take().unwrap().ok();
        }
    }
}
