// SPDX-License-Identifier: GPL-3.0-or-later

//! The open image files: a bounded, least-recently-used cache of file handles shared by
//! every disc of the process (issue #26).
//!
//! *Why:* on a network share, opening an image costs a round trip or two (0.6 to 0.9 ms
//! over NFS), as much as the one read a seek with a warm seek index needs. So a disc keeps
//! the handle it read its TOC through, and every later read reuses it; but a library has
//! hundreds of discs, so the handles are bounded ([`DEFAULT_MAX_OPEN`], `--max-open-images`)
//! and the least recently used one is closed first. A handle idle for [`DEFAULT_IDLE`] is
//! closed too, so that an idle mount holds no files open on the share (which keeps the
//! server from deleting a replaced file, and NFSv4 state alive).
//!
//! A handle is an `Arc<File>` read with positional reads (`pread`), so the reads of one
//! image run at the same time on any number of threads: scarletbook reads the image through
//! a [`crate::disc`] source that takes the cached handle for each call (no I/O, no lock)
//! (sacdfs#28). *Why:* with one `Mutex<Image>` per image, every read of a disc queued behind
//! the one in progress, so two players and a tagger on one disc waited for each other; on a
//! network share each of those reads is a round trip.
//!
//! A handle is checked against its path at most once per [`REVALIDATE_EVERY`] when it is
//! used: if the image's size, modification time or file (device and inode) changed, it is
//! closed and the image opened again, exactly as a first open would. *Why not on every
//! read:* a `stat` of the path is a round trip on a share whose attribute cache has
//! expired; once a second is noise for a straight read and never stale for long.

use std::fs::{File, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use lru::LruCache;

/// Most image handles kept open by default.
pub const DEFAULT_MAX_OPEN: usize = 64;

/// A handle not used for this long is closed.
pub const DEFAULT_IDLE: Duration = Duration::from_secs(300);

/// A handle is checked against its path at most this often.
pub const REVALIDATE_EVERY: Duration = Duration::from_secs(1);

/// An open image file, shared by the reads of one disc: each read uses it for the length of
/// one call, so the file is closed when the cache has dropped the handle and the last read
/// holding it is done.
pub type SharedFile = Arc<File>;

/// What identifies the content of an image file: the file, its size and modification time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
}

impl Stamp {
    /// The stamp of a file's metadata.
    pub fn of(m: &Metadata) -> Stamp {
        Stamp { dev: m.dev(), ino: m.ino(), len: m.len(), mtime: (m.mtime(), m.mtime_nsec()) }
    }
}

#[derive(Debug)]
struct Entry {
    file: SharedFile,
    stamp: Stamp,
    used: Instant,
    checked: Instant,
}

/// The cache of open image handles, keyed by a disc's handle key ([`next_key`]).
#[derive(Debug)]
pub struct Handles {
    lru: Mutex<LruCache<u64, Entry>>,
    max: AtomicUsize,
    idle_ms: AtomicU64,
    revalidate_ms: AtomicU64,
    opens: AtomicU64,
}

impl Default for Handles {
    fn default() -> Self {
        Handles::new(DEFAULT_MAX_OPEN, DEFAULT_IDLE)
    }
}

impl Handles {
    /// A cache of at most `max` handles (at least 1), each closed after `idle` unused.
    pub fn new(max: usize, idle: Duration) -> Handles {
        Handles {
            lru: Mutex::new(LruCache::unbounded()),
            max: AtomicUsize::new(max.max(1)),
            idle_ms: AtomicU64::new(idle.as_millis() as u64),
            revalidate_ms: AtomicU64::new(REVALIDATE_EVERY.as_millis() as u64),
            opens: AtomicU64::new(0),
        }
    }

    /// Changes the bound and the idle timeout; handles over the new bound are closed.
    pub fn configure(&self, max: usize, idle: Duration) {
        self.max.store(max.max(1), Ordering::Relaxed);
        self.idle_ms.store(idle.as_millis() as u64, Ordering::Relaxed);
        let mut g = self.lock();
        self.trim(&mut g);
    }

    /// Changes how often a handle is checked against its path (tests use zero).
    pub fn set_revalidate_every(&self, every: Duration) {
        self.revalidate_ms.store(every.as_millis() as u64, Ordering::Relaxed);
    }

    fn lock(&self) -> MutexGuard<'_, LruCache<u64, Entry>> {
        self.lru.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn idle(&self) -> Duration {
        Duration::from_millis(self.idle_ms.load(Ordering::Relaxed))
    }

    /// Drops handles over the bound, least recently used first. A dropped handle closes
    /// once the reads holding it finish.
    fn trim(&self, g: &mut LruCache<u64, Entry>) {
        let max = self.max.load(Ordering::Relaxed);
        while g.len() > max {
            g.pop_lru();
        }
    }

    /// The handle of `key`, opened with `open` if there is none, or if the file at `path`
    /// changed since it was opened.
    pub fn get<E: From<io::Error>>(
        &self,
        key: u64,
        path: &Path,
        open: impl FnOnce() -> Result<(File, Stamp), E>,
    ) -> Result<SharedFile, E> {
        let now = Instant::now();
        let every = Duration::from_millis(self.revalidate_ms.load(Ordering::Relaxed));
        let found = {
            let mut g = self.lock();
            self.close_idle_locked(&mut g, now);
            g.get_mut(&key).map(|e| {
                e.used = now;
                (e.file.clone(), e.stamp, now.saturating_duration_since(e.checked) >= every)
            })
        };
        if let Some((file, stamp, check)) = found {
            if !check {
                return Ok(file);
            }
            let same = std::fs::metadata(path).is_ok_and(|m| Stamp::of(&m) == stamp);
            let mut g = self.lock();
            if same {
                if let Some(e) = g.peek_mut(&key) {
                    e.checked = now;
                }
                return Ok(file);
            }
            log::debug!("{}: the image file changed: opening it again", path.display());
            if g.peek(&key).is_some_and(|e| Arc::ptr_eq(&e.file, &file)) {
                g.pop(&key);
            }
        }
        let (file, stamp) = open()?;
        self.opens.fetch_add(1, Ordering::Relaxed);
        Ok(self.insert(key, file, stamp))
    }

    /// Keeps `file` (opened with `stamp`) as the handle of `key`, unless another read put
    /// one there meanwhile; returns the one kept.
    pub fn insert(&self, key: u64, file: File, stamp: Stamp) -> SharedFile {
        let now = Instant::now();
        let mut g = self.lock();
        if let Some(e) = g.get_mut(&key) {
            if e.stamp == stamp {
                e.used = now;
                return e.file.clone();
            }
        }
        let file = Arc::new(file);
        g.put(key, Entry { file: file.clone(), stamp, used: now, checked: now });
        self.trim(&mut g);
        file
    }

    /// Closes the handle of `key`, if open (a disc that is dropped).
    pub fn remove(&self, key: u64) {
        self.lock().pop(&key);
    }

    /// Closes the handles not used since `now` minus the idle timeout.
    pub fn close_idle(&self, now: Instant) -> usize {
        let mut g = self.lock();
        self.close_idle_locked(&mut g, now)
    }

    fn close_idle_locked(&self, g: &mut LruCache<u64, Entry>, now: Instant) -> usize {
        let idle = self.idle();
        let mut n = 0;
        while g.peek_lru().is_some_and(|(_, e)| now.saturating_duration_since(e.used) >= idle) {
            g.pop_lru();
            n += 1;
        }
        n
    }

    /// Handles held open.
    pub fn open_count(&self) -> usize {
        self.lock().len()
    }

    /// Images opened by [`Handles::get`] so far (for tests and diagnostics); a handle kept
    /// from reading the TOC does not count.
    pub fn opens(&self) -> u64 {
        self.opens.load(Ordering::Relaxed)
    }
}

static GLOBAL: OnceLock<Handles> = OnceLock::new();

/// The cache every disc of the process uses.
pub fn global() -> &'static Handles {
    GLOBAL.get_or_init(Handles::default)
}

/// A key for one disc's handle, unique in the process. *Why not the disc id:* callers
/// choose that id, and tests reuse it.
pub fn next_key() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Starts a thread that closes the global cache's idle handles, checking every tenth of
/// the idle timeout (1 to 30 s), so that an idle mount does not keep images open.
pub fn start_idle_closer() -> io::Result<()> {
    std::thread::Builder::new().name("sacdfs-handles".to_owned()).spawn(|| {
        loop {
            let every = (global().idle() / 10).clamp(Duration::from_secs(1), Duration::from_secs(30));
            std::thread::sleep(every);
            let n = global().close_idle(Instant::now());
            if n > 0 {
                log::debug!("{n} idle image files closed");
            }
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file(len: usize) -> (tempfile::NamedTempFile, Stamp) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&vec![0u8; len]).unwrap();
        f.flush().unwrap();
        let s = Stamp::of(&f.as_file().metadata().unwrap());
        (f, s)
    }

    fn opener(path: &Path) -> impl FnOnce() -> io::Result<(File, Stamp)> + '_ {
        move || {
            let f = File::open(path)?;
            let s = Stamp::of(&f.metadata()?);
            Ok((f, s))
        }
    }

    #[test]
    fn reuses_and_bounds() {
        let h = Handles::new(2, DEFAULT_IDLE);
        let files: Vec<_> = (0..4).map(|_| file(4096)).collect();
        for (i, (f, _)) in files.iter().enumerate() {
            h.get(i as u64, f.path(), opener(f.path())).unwrap();
            assert!(h.open_count() <= 2);
        }
        assert_eq!(h.opens(), 4);
        // 2 and 3 are kept; 3 again opens nothing, 0 again opens and evicts 2.
        h.get(3, files[3].0.path(), opener(files[3].0.path())).unwrap();
        assert_eq!(h.opens(), 4);
        h.get(0, files[0].0.path(), opener(files[0].0.path())).unwrap();
        assert_eq!(h.opens(), 5);
        h.get(3, files[3].0.path(), opener(files[3].0.path())).unwrap();
        assert_eq!(h.opens(), 5);
        h.get(2, files[2].0.path(), opener(files[2].0.path())).unwrap();
        assert_eq!(h.opens(), 6);
        h.remove(2);
        assert_eq!(h.open_count(), 1);
    }

    #[test]
    fn closes_idle_handles() {
        let h = Handles::new(8, Duration::from_secs(60));
        let (f, _) = file(4096);
        h.get(1, f.path(), opener(f.path())).unwrap();
        assert_eq!(h.close_idle(Instant::now()), 0);
        assert_eq!(h.close_idle(Instant::now() + Duration::from_secs(61)), 1);
        assert_eq!(h.open_count(), 0);
        h.get(1, f.path(), opener(f.path())).unwrap();
        assert_eq!(h.opens(), 2);
    }

    #[test]
    fn changed_file_gets_a_fresh_handle() {
        let h = Handles::new(8, DEFAULT_IDLE);
        h.set_revalidate_every(Duration::ZERO);
        let (mut f, stamp) = file(4096);
        h.insert(1, opener(f.path())().unwrap().0, stamp);
        let a = h.get(1, f.path(), opener(f.path())).unwrap();
        assert_eq!(h.opens(), 0);
        // Another size.
        f.write_all(&[1u8; 2048]).unwrap();
        f.flush().unwrap();
        let b = h.get(1, f.path(), opener(f.path())).unwrap();
        assert_eq!(h.opens(), 1);
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(b.metadata().unwrap().len(), 4096 + 2048);
        // Another modification time, same size.
        let t = std::time::SystemTime::now() - Duration::from_secs(3600);
        f.as_file().set_modified(t).unwrap();
        let c = h.get(1, f.path(), opener(f.path())).unwrap();
        assert_eq!(h.opens(), 2);
        assert!(!Arc::ptr_eq(&b, &c));
        // Unchanged: the same handle.
        let d = h.get(1, f.path(), opener(f.path())).unwrap();
        assert!(Arc::ptr_eq(&c, &d));
        assert_eq!(h.opens(), 2);
        // A file put in its place.
        let (g, _) = file(4096 + 2048);
        std::fs::rename(g.path(), f.path()).unwrap();
        let e = h.get(1, f.path(), opener(f.path())).unwrap();
        assert_eq!(h.opens(), 3);
        assert!(!Arc::ptr_eq(&d, &e));
    }

    #[test]
    fn not_revalidated_within_the_interval() {
        let h = Handles::new(8, DEFAULT_IDLE);
        h.set_revalidate_every(Duration::from_secs(3600));
        let (mut f, stamp) = file(4096);
        h.insert(1, opener(f.path())().unwrap().0, stamp);
        f.write_all(&[1u8; 2048]).unwrap();
        f.flush().unwrap();
        h.get(1, f.path(), opener(f.path())).unwrap();
        assert_eq!(h.opens(), 0);
    }
}
