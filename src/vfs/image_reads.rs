// SPDX-License-Identifier: GPL-3.0-or-later

//! The reads of each image, to fail at once the new reads of an image whose source hangs
//! (#241).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How many reads of one image may be unfinished past the request timeout before its new
/// reads fail at once with `EIO` (#241).
///
/// *Why a cap on these:* the watchdog answers a read that took too long, but its thread stays
/// blocked until the source answers, and a client retrying through a long hang took one more
/// read thread each time, until every one was blocked and reads of healthy images failed too.
/// A read unfinished past the timeout has failed already, so counting only those never fails
/// a read that could still have been answered in time, however many readers a slow image
/// has. Past the cap, an image's new reads fail at once instead of after the timeout, and
/// take no thread. *Why 4:* a few such reads can be one slow moment (a disk's retry, a
/// share's reconnect) and must not cut the image off at once, while 4 per hung image leave
/// most of the 64 read threads ([`MAX_POOL_THREADS`](super::MAX_POOL_THREADS)) to the others. *Rejected:* a cap on
/// all reads of an image, which would fail a busy healthy image's parallel reads; and
/// cancellable reads of the source, which blocking reads of a hung share can't be.
pub const HUNG_READS_PER_IMAGE: usize = 4;

/// The reads of each image asked for and not finished, to fail at once the new reads of an
/// image whose source hangs rather than give them a thread to block on it (#241): see
/// [`HUNG_READS_PER_IMAGE`]. Read-ahead isn't counted: a hang starts at most one per reader,
/// as only a read that succeeded starts one.
#[derive(Debug)]
pub(crate) struct ImageReads {
    /// The request timeout: a read older than this has failed, and its thread is stuck.
    /// `None`: requests are never failed, so a client has nothing to retry and none is.
    timeout: Option<Duration>,
    /// Per image ([`crate::disc::Disc::id`]): its unfinished reads, by token, and when each
    /// was asked for.
    running: Mutex<HashMap<u32, Running>>,
    next: AtomicU64,
}

#[derive(Debug, Default)]
struct Running {
    reads: Vec<(u64, Instant)>,
    /// Its new reads are failed at once: logged when this starts and when it ends.
    refusing: bool,
    /// The image file, for that log.
    path: std::path::PathBuf,
}

impl Running {
    fn hung(&self, timeout: Duration) -> usize {
        self.reads.iter().filter(|(_, t)| t.elapsed() >= timeout).count()
    }
}

/// A read counted in [`ImageReads`] until it finishes.
pub(crate) struct ImageRead {
    reads: Arc<ImageReads>,
    image: u32,
    token: u64,
}

impl ImageReads {
    pub(crate) fn new(timeout: Option<Duration>) -> Arc<ImageReads> {
        Arc::new(ImageReads { timeout, running: Mutex::new(HashMap::new()), next: AtomicU64::new(1) })
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<u32, Running>> {
        self.running.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Counts a read of image `id` (file `path`) from now until the returned guard drops,
    /// unless the image has [`HUNG_READS_PER_IMAGE`] reads unfinished past the timeout
    /// already: then `None`, and the read is to fail with `EIO` at once.
    pub(crate) fn start(self: &Arc<Self>, id: u32, path: &std::path::Path) -> Option<ImageRead> {
        let mut running = self.running();
        let image = running.entry(id).or_default();
        let hung = match self.timeout {
            Some(t) if image.reads.len() >= HUNG_READS_PER_IMAGE => image.hung(t),
            _ => 0,
        };
        if let (Some(timeout), true) = (self.timeout, hung >= HUNG_READS_PER_IMAGE) {
            if !image.refusing {
                image.refusing = true;
                path.clone_into(&mut image.path);
                log::warn!(
                    "{}: {hung} reads still blocked after the {} s request timeout: failing its \
                     new reads with EIO at once until they finish, so that it takes no more \
                     read threads",
                    path.display(),
                    timeout.as_secs_f64()
                );
            }
            return None;
        }
        let token = self.next.fetch_add(1, Ordering::Relaxed);
        image.reads.push((token, Instant::now()));
        Some(ImageRead { reads: self.clone(), image: id, token })
    }
}

impl Drop for ImageRead {
    fn drop(&mut self) {
        let mut running = self.reads.running();
        let Some(image) = running.get_mut(&self.image) else { return };
        image.reads.retain(|(t, _)| *t != self.token);
        let hung = self.reads.timeout.map_or(0, |t| image.hung(t));
        if image.refusing && hung < HUNG_READS_PER_IMAGE {
            log::warn!("{}: its blocked reads finish: reading it again", image.path.display());
            image.refusing = false;
        }
        if image.reads.is_empty() {
            running.remove(&self.image);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An image's new reads fail at once only once [`HUNG_READS_PER_IMAGE`] of its reads are
    /// unfinished past the timeout; reads younger than that, however many, and other images'
    /// reads go on, and the image reads again once one finishes (#241).
    #[test]
    fn an_image_with_reads_hung_past_the_timeout_refuses_new_ones() {
        let path = std::path::Path::new("hung.iso");
        let reads = ImageReads::new(Some(Duration::from_millis(100)));
        let mut hung: Vec<_> = (0..HUNG_READS_PER_IMAGE).map(|_| reads.start(1, path).unwrap()).collect();
        // Many reads in time, none refused.
        let young: Vec<_> = (0..20).map(|_| reads.start(1, path).unwrap()).collect();
        drop(young);
        std::thread::sleep(Duration::from_millis(150));
        assert!(reads.start(1, path).is_none(), "a hung image's read was let through");
        assert!(reads.start(2, path).is_some(), "another image's read was refused");
        hung.pop();
        let again = reads.start(1, path).expect("refused after a blocked read finished");
        drop((hung, again));
        assert!(reads.running().is_empty());
        // No timeout: nothing is refused.
        let reads = ImageReads::new(None);
        let _all: Vec<_> = (0..2 * HUNG_READS_PER_IMAGE).map(|_| reads.start(1, path).unwrap()).collect();
    }
}
