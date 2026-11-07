// SPDX-License-Identifier: GPL-3.0-or-later

//! The FUSE filesystem: requests from the kernel, answered from the tree and the engine.
//!
//! fuser's threads only take requests from the kernel. What is in memory already (the
//! attributes of a node made already) is answered there; everything that may read the
//! source (lookups and listings, which open images, opens, reads) is moved with its reply to
//! a pool of request threads, reads to one and metadata to another, so that a slow or
//! stalled source does not hold up the requests of the others (#63), until it has blocked
//! every thread of a pool (#241). Each moved request is watched: one that waits longer than
//! the request timeout is failed with `EIO` ([`crate::watchdog`], #72).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::FileExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fuser::{
    AccessFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    InitFlags, KernelConfig, LockOwner, OpenAccMode, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory,
    ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyXattr, Request,
};

use crate::engine::Engine;
use crate::track::Reader;
use crate::tree::{Entry, Node, ROOT, SidecarFile, Tree};
use crate::watchdog::{self, Fail, Watchdog, What};

/// How long the kernel may cache entries and attributes: the tree never changes.
const TTL: Duration = Duration::from_secs(24 * 3600);

/// How long a reading of the source's size is used before it is read again.
const STATFS_REFRESH: Duration = Duration::from_secs(60);

/// Largest read the kernel is asked to send, and the read-ahead it is asked for. It grants
/// no more read-ahead than it offers, 128 KiB unless root has raised the mount's
/// `read_ahead_kb`, so reads through the page cache arrive 128 KiB at a time (#210).
pub const MAX_READ: u32 = 1 << 20;

type Job = Box<dyn FnOnce() + Send>;

/// Most threads a pool of [`Workers`] grows to. *Why 64:* the kernel sends at most 64
/// background requests (read-ahead) at once (`max_background`, set in `init`), plus the
/// few synchronous ones of its callers; past that, a request waits for a thread, and the
/// watchdog fails it if it waits too long. Each thread costs a stack of address space,
/// mostly untouched, and nothing while it is parked.
///
/// *The limit this sets (#241):* the watchdog replies `EIO` but can't free a thread blocked
/// on the source, so a client retrying through a long hang takes one more thread each time,
/// and the blocked reads needn't be simultaneous. At 64, reads of every image wait and fail
/// until the source answers.
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
/// hung source from making threads without end, and the watchdog fails what then waits,
/// healthy images' requests included (#241).
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

/// The filesystem.
#[derive(Debug)]
pub struct SacdFs {
    shared: Arc<Shared>,
    engine: Arc<Engine>,
    /// Reads, decoding or copying.
    requests: Workers,
    /// Metadata requests that may read the source (lookups, listings, opens): see
    /// [`SacdFs::serve`].
    meta: Workers,
    watchdog: Watchdog,
    /// The size `statfs` reports, and when it was read: see [`SacdFs::statfs`].
    size: Arc<Mutex<ShareSize>>,
}

/// What the request threads share with the filesystem.
#[derive(Debug)]
struct Shared {
    tree: Arc<Tree>,
    /// The open files, by file handle, with their inode numbers.
    handles: Mutex<HashMap<u64, (u64, Open)>>,
    next_fh: AtomicU64,
    uid: u32,
    gid: u32,
}

/// An open file.
#[derive(Debug, Clone)]
enum Open {
    /// A track file, read through the engine.
    Track(Arc<Reader>),
    /// A sidecar file: its source file, open, and its node.
    Sidecar(Arc<std::fs::File>, Arc<SidecarFile>),
}

impl Open {
    /// The node the handle was opened on: what its attributes are answered from once the
    /// tree has freed it (its image or file removed from SOURCE, #153).
    fn node(&self) -> Node {
        match self {
            Open::Track(r) => Node::File(r.file.clone()),
            Open::Sidecar(_, s) => Node::Sidecar(s.clone()),
        }
    }
}

/// Reads up to `size` bytes at `offset` of `file`, fewer only at its end.
fn read_plain(file: &std::fs::File, offset: u64, size: u32) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; size as usize];
    let mut got = 0;
    while got < buf.len() {
        match file.read_at(&mut buf[got..], offset + got as u64) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    buf.truncate(got);
    Ok(buf)
}

/// The share's size, as `statfs` reports it: see [`SacdFs::statfs`].
#[derive(Debug, Clone, Copy)]
struct ShareSize {
    blocks: u64,
    frsize: u32,
    read: Instant,
    /// Read from the source's filesystem, for want of images: read again now and then.
    refresh: bool,
    refreshing: bool,
}

impl ShareSize {
    /// The images' total, in blocks of 64 KiB; with no images, what [`ShareSize::of_source`]
    /// says, so that this is never a 0-byte filesystem (#62).
    fn of_images(bytes: u64, source: &std::path::Path) -> ShareSize {
        if bytes == 0 {
            return ShareSize::of_source(source);
        }
        let frsize = 1 << 16;
        ShareSize {
            blocks: bytes.div_ceil(u64::from(frsize)),
            frsize,
            read: Instant::now(),
            refresh: false,
            refreshing: false,
        }
    }

    /// `statvfs` of `path`; 0 blocks if it fails.
    fn of_source(path: &std::path::Path) -> ShareSize {
        let (blocks, frsize) = statvfs(path).unwrap_or_else(|e| {
            log::debug!("{}: statvfs: {e}", path.display());
            (0, 1 << 16)
        });
        ShareSize { blocks, frsize, read: Instant::now(), refresh: true, refreshing: false }
    }
}

/// The total size of the filesystem holding `path`, in blocks of the returned size.
fn statvfs(path: &std::path::Path) -> io::Result<(u64, u32)> {
    let st = nix::sys::statvfs::statvfs(path).map_err(io::Error::from)?;
    let frsize = if st.fragment_size() > 0 { st.fragment_size() } else { st.block_size() };
    Ok((st.blocks(), u32::try_from(frsize).unwrap_or(1 << 16).max(1)))
}

/// The attributes of a negative entry: only the inode number 0 counts.
fn negative_attr() -> FileAttr {
    let t = std::time::SystemTime::UNIX_EPOCH;
    FileAttr {
        ino: INodeNo(0),
        size: 0,
        blocks: 0,
        atime: t,
        mtime: t,
        ctime: t,
        crtime: t,
        kind: FileType::RegularFile,
        perm: 0,
        nlink: 0,
        uid: 0,
        gid: 0,
        rdev: 0,
        blksize: 0,
        flags: 0,
    }
}

impl SacdFs {
    /// A filesystem showing `tree`, decoding with `engine`, answering reads on
    /// `request_threads` threads (more when they are busy, see [`Workers`]), and failing a
    /// request that waits longer than [`watchdog::DEFAULT_TIMEOUT`]. Files belong to
    /// `uid`/`gid`.
    pub fn new(
        tree: Arc<Tree>,
        engine: Arc<Engine>,
        request_threads: usize,
        uid: u32,
        gid: u32,
    ) -> io::Result<Self> {
        let requests = Workers::new(request_threads, "sacdfs-read")?;
        // As many as fuser's own threads, whose waiting these take over.
        let meta = Workers::new(4, "sacdfs-meta")?;
        // Read now, while the source has just been walked.
        let size = Arc::new(Mutex::new(ShareSize::of_images(tree.image_bytes(), tree.source())));
        Ok(SacdFs {
            shared: Arc::new(Shared {
                tree,
                handles: Mutex::new(HashMap::new()),
                next_fh: AtomicU64::new(1),
                uid,
                gid,
            }),
            engine,
            requests,
            meta,
            watchdog: Watchdog::new(Some(watchdog::DEFAULT_TIMEOUT))?,
            size,
        })
    }

    /// The same filesystem, failing a request that waits longer than `timeout` (`None` or
    /// zero: requests wait as long as they take).
    pub fn with_request_timeout(mut self, timeout: Option<Duration>) -> io::Result<Self> {
        self.watchdog = Watchdog::new(timeout)?;
        Ok(self)
    }

    /// The watchdog.
    pub fn watchdog(&self) -> &Watchdog {
        &self.watchdog
    }

    /// Answers a request that may read the source off fuser's threads: `work` runs on a
    /// metadata thread, then `answer` replies with what it found, or is given `None` if the
    /// watchdog failed the request meanwhile.
    ///
    /// *Why (#63):* fuser has 4 threads reading requests from the kernel. A lookup or
    /// listing that loads TOCs from a stalled share held one of them for as long as the
    /// share hung, and four of them stopped the whole mount, even for directories listed
    /// already. Now fuser's threads only hand the request over, and what was listed
    /// already still answers.
    fn serve<R: Fail, T: Send + 'static>(
        &self,
        what: What,
        reply: R,
        work: impl FnOnce(&Arc<Shared>) -> T + Send + 'static,
        answer: impl FnOnce(Option<R>, T) + Send + 'static,
    ) {
        let pending = self.watchdog.watch(what, reply);
        let shared = self.shared.clone();
        self.meta.spawn(move || {
            pending.running();
            let found = work(&shared);
            answer(pending.take(), found);
        });
    }
}

impl Shared {
    /// How long the kernel may keep an entry of `parent` numbered `ino` (a name looked up
    /// there, or the directory's own attributes): [`TTL`], or the refresh interval when
    /// either is re-read when its source changes.
    fn ttl(&self, parent: u64, ino: u64) -> Duration {
        self.tree.live_ttl(parent).or_else(|| self.tree.live_ttl(ino)).unwrap_or(TTL)
    }

    fn attr(&self, ino: u64, node: &Node) -> FileAttr {
        let (kind, size, perm, nlink) = match node {
            Node::Dir(_) => (FileType::Directory, 0, 0o555, 2),
            Node::File(f) => (FileType::RegularFile, f.size, 0o444, 1),
            Node::Sidecar(s) => (FileType::RegularFile, s.meta.size, 0o444, 1),
        };
        let mtime = self.tree.mtime(node);
        // A sidecar file shows its source file's times and blocks (#79); everything else
        // one time, its mtime.
        let (atime, ctime, blocks) = match node {
            Node::Sidecar(s) => (s.meta.atime, s.meta.ctime, s.meta.blocks),
            _ => (mtime, mtime, size.div_ceil(512)),
        };
        FileAttr {
            ino: INodeNo(ino),
            size,
            blocks,
            atime,
            mtime,
            ctime,
            crtime: mtime,
            kind,
            perm,
            nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 1 << 16,
            flags: 0,
        }
    }

    fn handle(&self, fh: u64) -> Option<Open> {
        self.handles.lock().unwrap_or_else(|p| p.into_inner()).get(&fh).map(|(_, o)| o.clone())
    }

    /// The node of a file open as `ino`, once the tree has freed it (#153): its handle
    /// (`fh`, if the kernel sent it, else any handle open on `ino`) answers for it until
    /// it is released.
    fn opened_node(&self, ino: u64, fh: Option<u64>) -> Option<Arc<Node>> {
        let handles = self.handles.lock().unwrap_or_else(|p| p.into_inner());
        let by_fh = fh.and_then(|fh| handles.get(&fh)).filter(|(i, _)| *i == ino);
        by_fh.or_else(|| handles.values().find(|(i, _)| *i == ino)).map(|(_, o)| Arc::new(o.node()))
    }

    /// The error for a number the tree doesn't find: `ENOMEM` if it isn't looking, for lack of
    /// memory (#237).
    fn not_found(&self) -> Errno {
        if self.tree.refusing() { Errno::ENOMEM } else { Errno::ENOENT }
    }

    fn lookup(&self, parent: u64, name: &OsStr) -> Result<(Duration, FileAttr, Generation), Errno> {
        let mut negative = false;
        let mut volatile = false;
        let ino = match name.as_encoded_bytes() {
            // The kernel's export code asks for `.` of an inode number from a file handle
            // and for `..` of a directory, to rebuild its path.
            b"." => self.tree.get(parent).map(|_| parent),
            b".." => match self.tree.get(parent).as_deref() {
                Some(Node::Dir(d)) => Some(d.parent),
                Some(Node::File(_) | Node::Sidecar(_)) => return Err(Errno::ENOTDIR),
                None => None,
            },
            _ => {
                // Art in an area is checked against its source, and asked about again each time (#235).
                let (ino, art) = self.tree.lookup_art(parent, name);
                volatile = art;
                negative = self.tree.children(parent).is_some();
                ino
            }
        };
        let found = ino.and_then(|ino| self.tree.get(ino).map(|n| (ino, n)));
        // Not "missing", which the kernel would remember, but short of memory (#237).
        if found.is_none() && self.tree.refusing() {
            return Err(Errno::ENOMEM);
        }
        match found {
            // A directory that is re-read when its source changes (#101), and what is in
            // it, is remembered for a short time only.
            Some((ino, node)) => {
                let mut entry = self.entry(parent, ino, &node);
                if volatile {
                    entry.0 = Duration::ZERO;
                }
                Ok(entry)
            }
            // A name missing from a directory stays missing (unless the directory is re-read,
            // #101), so the kernel may remember that it is: inode 0 is a negative entry,
            // cached for the TTL (#61). `.` and `..` of an unknown number stay errors, for the export
            // code.
            None if negative => Ok((self.ttl(parent, parent), negative_attr(), Generation(0))),
            None => Err(Errno::ENOENT),
        }
    }

    /// The entry of `node`, numbered `ino`, in directory `parent`: how long the kernel may keep
    /// it, its attributes and generation.
    ///
    /// A track file keeps the full [`TTL`] even in a folder that is re-read (#205): an area is
    /// re-read for its folder art only, which never changes its tracks' names, numbers or
    /// attributes, and a changed image is a new folder in its parent. Without this, every
    /// track's name was looked up again 2 s after the last time, on every listing. With tags
    /// (#81) a track's size and time follow its tags file, so they stay short.
    fn entry(&self, parent: u64, ino: u64, node: &Node) -> (Duration, FileAttr, Generation) {
        let ttl = match node {
            // Art in an area is asked about again each time (#235), in a listing too.
            Node::Sidecar(_) if self.tree.is_area_art(parent, node) => Duration::ZERO,
            Node::File(_) if !crate::tags::installed() => TTL,
            _ => self.ttl(parent, ino),
        };
        (ttl, self.attr(ino, node), Generation(self.tree.generation(node)))
    }

    /// The entries of directory `ino` from position `offset` on (`.` and `..` first), with
    /// what [`Shared::entry`] gives for each, for `readdirplus` (#205). A name whose node has
    /// just been let go of is left out (the next listing shows it again); `None` if `ino` is
    /// not a directory (or, short of memory, cannot be listed now).
    fn listing_plus(&self, ino: u64, offset: u64) -> Option<Vec<(u64, std::ffi::OsString, Entried)>> {
        let children = self.tree.children(ino)?;
        let parent = self.tree.parent_of(ino);
        let dots = [(ino, OsStr::new(".")), (parent, OsStr::new(".."))];
        let names = dots.into_iter().chain(children.iter().map(|e| (e.ino, e.name.as_os_str())));
        let mut out = Vec::new();
        for (i, (child, name)) in names.enumerate().skip(offset as usize) {
            let Some(node) = self.tree.get(child) else {
                if self.tree.refusing() {
                    return None;
                }
                continue;
            };
            // `.` and `..` are not entries of `ino`: the kernel links neither, only lists them.
            out.push((i as u64 + 1, name.to_owned(), self.entry(ino, child, &node)));
        }
        Some(out)
    }

    fn getattr(&self, ino: u64, node: Option<Arc<Node>>) -> Result<(Duration, FileAttr), Errno> {
        match node {
            Some(node) => Ok((self.ttl(ino, ino), self.attr(ino, &node))),
            None => Err(self.not_found()),
        }
    }

    /// Opens `node`: a new file handle.
    fn open(&self, ino: u64, node: Option<Arc<Node>>) -> Result<u64, Errno> {
        let Some(node) = node else { return Err(self.not_found()) };
        // New readers wait out a shortage, so that the ones reading now go on (#237).
        if self.tree.refusing() && matches!(&*node, Node::File(_)) {
            return Err(Errno::ENOMEM);
        }
        let open = match &*node {
            Node::File(file) => Open::Track(Arc::new(Reader::new(file.clone()))),
            Node::Sidecar(s) => match self.tree.open_sidecar(s) {
                Ok(f) => Open::Sidecar(Arc::new(f), s.clone()),
                Err(e) => {
                    log::info!("sidecar {}: open: {e}", s.target.display());
                    let errno = e.raw_os_error().unwrap_or(libc::EIO);
                    // Gone since the kernel was told of it (#235): it asks again, then.
                    if errno == libc::ENOENT && self.tree.art_told_lately(ino) {
                        return Err(Errno::ESTALE);
                    }
                    return Err(Errno::from_i32(errno));
                }
            },
            Node::Dir(_) => return Err(Errno::EISDIR),
        };
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().unwrap_or_else(|p| p.into_inner()).insert(fh, (ino, open));
        Ok(fh)
    }

    fn opendir(&self, ino: u64, node: Option<Arc<Node>>) -> Result<FopenFlags, Errno> {
        match node.as_deref() {
            // A listing that is re-read when its source changes is not cached by the kernel.
            Some(Node::Dir(_)) if self.tree.is_live(ino) => Ok(FopenFlags::empty()),
            Some(Node::Dir(_)) => Ok(FopenFlags::FOPEN_KEEP_CACHE | FopenFlags::FOPEN_CACHE_DIR),
            Some(Node::File(_) | Node::Sidecar(_)) => Err(Errno::ENOTDIR),
            None => Err(self.not_found()),
        }
    }

    fn access(node: Option<Arc<Node>>, mask: AccessFlags) -> Result<(), Errno> {
        match node.as_deref() {
            None => Err(Errno::ENOENT),
            Some(_) if mask.contains(AccessFlags::W_OK) => Err(Errno::EROFS),
            Some(Node::File(_) | Node::Sidecar(_)) if mask.contains(AccessFlags::X_OK) => Err(Errno::EACCES),
            Some(_) => Ok(()),
        }
    }
}

/// An entry's TTL, attributes and generation.
type Entried = (Duration, FileAttr, Generation);

fn reply_entry(reply: ReplyEntry, r: Result<(Duration, FileAttr, Generation), Errno>) {
    match r {
        Ok((ttl, attr, generation)) => reply.entry(&ttl, &attr, generation),
        Err(e) => reply.error(e),
    }
}

fn reply_attr(reply: ReplyAttr, r: Result<(Duration, FileAttr), Errno>) {
    match r {
        Ok((ttl, attr)) => reply.attr(&ttl, &attr),
        Err(e) => reply.error(e),
    }
}

fn reply_opened(reply: ReplyOpen, r: Result<(u64, FopenFlags), Errno>) {
    match r {
        Ok((fh, flags)) => reply.opened(FileHandle(fh), flags),
        Err(e) => reply.error(e),
    }
}

fn reply_empty(reply: ReplyEmpty, r: Result<(), Errno>) {
    match r {
        Ok(()) => reply.ok(),
        Err(e) => reply.error(e),
    }
}

/// A track file read, for the log line of one that timed out.
struct ReadOf {
    reader: Arc<Reader>,
    offset: u64,
    size: u32,
}

impl std::fmt::Display for ReadOf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let file = &self.reader.file;
        write!(
            f,
            "{} track {} at offset {} ({} bytes)",
            file.disc.path.display(),
            file.number,
            self.offset,
            self.size
        )
    }
}

/// A name in a directory, for a log line.
struct InName(u64, std::ffi::OsString);

impl std::fmt::Display for InName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} in inode {}", self.1, self.0)
    }
}

/// An inode, for a log line.
struct Ino(u64);

impl std::fmt::Display for Ino {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "inode {}", self.0)
    }
}

impl Filesystem for SacdFs {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> io::Result<()> {
        if let Err(nearest) = config.set_max_readahead(MAX_READ) {
            let _ = config.set_max_readahead(nearest);
        }
        let _ = config.set_max_background(64);
        // Lets the kernel NFS server export the mount: it then looks up `.` and `..` of
        // inode numbers from file handles, which the tree finds again after a restart.
        // Listings with attributes when the kernel sees lookups follow listings (#205).
        if let Err(missing) =
            config.add_capabilities(InitFlags::FUSE_DO_READDIRPLUS | InitFlags::FUSE_READDIRPLUS_AUTO)
        {
            log::info!("the kernel offers no readdirplus ({missing:?})");
        }
        if let Err(missing) = config.add_capabilities(InitFlags::FUSE_EXPORT_SUPPORT) {
            log::info!("the kernel offers no FUSE export support ({missing:?}): no NFS export");
        }
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name = name.to_owned();
        self.serve(
            What::new("lookup", InName(parent.0, name.clone())),
            reply,
            move |sh| sh.lookup(parent.0, &name),
            |reply, r| {
                if let Some(reply) = reply {
                    reply_entry(reply, r)
                }
            },
        );
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, fh: Option<FileHandle>, reply: ReplyAttr) {
        // A node made already is answered here, from memory: no handover for the most
        // frequent request. The root first sweeps the source (#123), and a node not made
        // yet is looked for along its path: both read the source.
        if ino.0 != ROOT {
            if let Some(node) = self.shared.tree.loaded(ino.0) {
                reply_attr(reply, self.shared.getattr(ino.0, Some(node)));
                return;
            }
        }
        self.serve(
            What::new("getattr", Ino(ino.0)),
            reply,
            move |sh| {
                if ino.0 == ROOT {
                    // A scan starts at the root: changes anywhere below move its mtime first.
                    sh.tree.sweep();
                }
                // A file open from before its image was removed (#153) is answered from its
                // handle.
                let node = sh.tree.get(ino.0).or_else(|| sh.opened_node(ino.0, fh.map(|f| f.0)));
                sh.getattr(ino.0, node)
            },
            |reply, r| {
                if let Some(reply) = reply {
                    reply_attr(reply, r)
                }
            },
        );
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if flags.acc_mode() != OpenAccMode::O_RDONLY {
            reply.error(Errno::EROFS);
            return;
        }
        // Opening a sidecar opens its source file.
        self.serve(
            What::new("open", Ino(ino.0)),
            reply,
            move |sh| (sh.open(ino.0, sh.tree.get(ino.0)), sh.clone()),
            |reply, (r, sh)| match reply {
                Some(reply) => reply_opened(reply, r.map(|fh| (fh, FopenFlags::FOPEN_KEEP_CACHE))),
                // Too late: the kernel never learns of the handle, so it is closed here.
                None => {
                    if let Ok(fh) = r {
                        sh.handles.lock().unwrap_or_else(|p| p.into_inner()).remove(&fh);
                    }
                }
            },
        );
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let reader = match self.shared.handle(fh.0) {
            Some(Open::Track(r)) => r,
            Some(Open::Sidecar(file, _)) => {
                // The source may be slow (a network filesystem): read on a request thread.
                let pending = self.watchdog.watch(What::new("sidecar read", Ino(ino.0)), reply);
                self.requests.spawn(move || {
                    pending.running();
                    let r = read_plain(&file, offset, size);
                    let Some(reply) = pending.take() else { return };
                    match r {
                        Ok(data) => reply.data(&data),
                        Err(e) => {
                            log::warn!("sidecar read at {offset}: {e}");
                            reply.error(Errno::EIO);
                        }
                    }
                });
                return;
            }
            None => {
                reply.error(Errno::EBADF);
                return;
            }
        };
        let engine = self.engine.clone();
        let pool = self.requests.clone();
        let what = What::new("read", ReadOf { reader: reader.clone(), offset, size });
        let pending = self.watchdog.watch(what, reply);
        self.requests.spawn(move || {
            pending.running();
            let result = catch_unwind(AssertUnwindSafe(|| reader.read(&engine, offset, u64::from(size))));
            let Some(reply) = pending.take() else { return };
            match result {
                Ok(Ok((data, ahead))) => {
                    reply.data(&data);
                    crate::track::give_back(data);
                    if let Some(ahead) = ahead {
                        pool.spawn(move || {
                            let _ = catch_unwind(AssertUnwindSafe(|| reader.read_ahead(&engine, ahead)));
                        });
                    }
                }
                Ok(Err(e)) => {
                    log::warn!(
                        "{} track {}: read at {offset}: {e}",
                        reader.file.disc.path.display(),
                        reader.file.number
                    );
                    reply.error(Errno::EIO);
                }
                Err(_) => {
                    log::error!(
                        "{} track {}: read at {offset} panicked",
                        reader.file.disc.path.display(),
                        reader.file.number
                    );
                    reply.error(Errno::EIO);
                }
            }
        });
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.shared.handles.lock().unwrap_or_else(|p| p.into_inner()).remove(&fh.0);
        reply.ok();
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        if let Some(node) = self.shared.tree.loaded(ino.0) {
            reply_opened(reply, self.shared.opendir(ino.0, Some(node)).map(|f| (0, f)));
            return;
        }
        self.serve(
            What::new("opendir", Ino(ino.0)),
            reply,
            move |sh| sh.opendir(ino.0, sh.tree.get(ino.0)),
            |reply, r| {
                if let Some(reply) = reply {
                    reply_opened(reply, r.map(|f| (0, f)))
                }
            },
        );
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, reply: ReplyDirectory) {
        // Listing a directory the first time opens its images (reads their TOCs).
        let tree = self.shared.tree.clone();
        let refusing = move || tree.refusing();
        self.serve(
            What::new("readdir", Ino(ino.0)),
            reply,
            move |sh| sh.tree.children(ino.0).map(|c| (c, sh.tree.parent_of(ino.0))),
            move |reply: Option<ReplyDirectory>, found: Option<(Arc<Vec<Entry>>, u64)>| {
                let Some(mut reply) = reply else { return };
                let Some((children, parent)) = found else {
                    reply.error(if refusing() { Errno::ENOMEM } else { Errno::ENOTDIR });
                    return;
                };
                let dots = [
                    (ino.0, FileType::Directory, OsStr::new(".")),
                    (parent, FileType::Directory, OsStr::new("..")),
                ];
                let entries = dots.into_iter().chain(children.iter().map(|e| {
                    (
                        e.ino,
                        if e.is_dir { FileType::Directory } else { FileType::RegularFile },
                        e.name.as_os_str(),
                    )
                }));
                for (i, (ino, kind, name)) in entries.enumerate().skip(offset as usize) {
                    if reply.add(INodeNo(ino), i as u64 + 1, kind, name) {
                        break;
                    }
                }
                reply.ok();
            },
        );
    }

    /// A listing with each entry's attributes (#205), so that a server listing a folder and
    /// then looking at each entry (Samba, NFS-Ganesha, `ls -l`) costs one request per page of
    /// names instead of one more per name. The kernel asks for it when it sees lookups
    /// follow a listing (`FUSE_READDIRPLUS_AUTO`), else for a plain `readdir`.
    fn readdirplus(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        reply: ReplyDirectoryPlus,
    ) {
        let tree = self.shared.tree.clone();
        let refusing = move || tree.refusing();
        self.serve(
            What::new("readdirplus", Ino(ino.0)),
            reply,
            move |sh| sh.listing_plus(ino.0, offset),
            move |reply: Option<ReplyDirectoryPlus>, found| {
                let Some(mut reply) = reply else { return };
                let Some(entries) = found else {
                    reply.error(if refusing() { Errno::ENOMEM } else { Errno::ENOTDIR });
                    return;
                };
                for (next, name, (ttl, attr, generation)) in &entries {
                    if reply.add(attr.ino, *next, name, ttl, attr, *generation) {
                        break;
                    }
                }
                reply.ok();
            },
        );
    }

    fn access(&self, _req: &Request, ino: INodeNo, mask: AccessFlags, reply: ReplyEmpty) {
        if let Some(node) = self.shared.tree.loaded(ino.0) {
            reply_empty(reply, Shared::access(Some(node), mask));
            return;
        }
        self.serve(
            What::new("access", Ino(ino.0)),
            reply,
            move |sh| match sh.tree.get(ino.0) {
                None => Err(sh.not_found()),
                node => Shared::access(node, mask),
            },
            |reply, r| {
                if let Some(reply) = reply {
                    reply_empty(reply, r)
                }
            },
        );
    }

    // No extended attributes (#174, docs/design.md). getxattr: ENOSYS, which the kernel
    // remembers for the whole mount (#61) and then answers every later call itself, with
    // EOPNOTSUPP. ENODATA is an answer about one file: Samba asked it again for every open
    // (security.capability, the POSIX ACL names), about 3.4 times per file in a Roon scan.
    // listxattr: an empty list, not ENOSYS. NFS-Ganesha forwards an NFSv4.2 client's
    // LISTXATTRS here and passes EOPNOTSUPP on as NFS4ERR_NOTSUPP, which the Linux NFS client
    // shows as "Unknown error 524" on every `ls -l`. Each `ls -l` entry costs one LISTXATTR,
    // answered here without touching the tree.
    fn getxattr(&self, _req: &Request, _ino: INodeNo, _name: &OsStr, _size: u32, reply: ReplyXattr) {
        reply.error(Errno::ENOSYS);
    }

    fn listxattr(&self, _req: &Request, _ino: INodeNo, size: u32, reply: ReplyXattr) {
        if size == 0 {
            reply.size(0);
        } else {
            reply.data(&[]);
        }
    }

    /// The total size of the images found at mount, all of it used, as read-only media report
    /// (#217). With no images, the size of the source's filesystem, read at most every minute
    /// in the background so that a slow source never holds up this reply: never 0 bytes (#62).
    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let size = {
            let mut s = self.size.lock().unwrap_or_else(|p| p.into_inner());
            if s.refresh && !s.refreshing && s.read.elapsed() >= STATFS_REFRESH {
                s.refreshing = true;
                let (cell, path) = (self.size.clone(), self.shared.tree.source().to_owned());
                self.requests.spawn(move || {
                    let new = ShareSize::of_source(&path);
                    *cell.lock().unwrap_or_else(|p| p.into_inner()) = new;
                });
            }
            *s
        };
        let files = self.shared.tree.len() as u64;
        reply.statfs(size.blocks, 0, 0, files, 0, 1 << 16, 255, size.frsize);
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
