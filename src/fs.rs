// SPDX-License-Identifier: GPL-3.0-or-later

//! The FUSE filesystem: requests from the kernel, answered from the tree and the engine.
//!
//! Metadata requests (`lookup`, `getattr`, `readdir`) are answered on fuser's threads.
//! Reads are moved, with their reply, to a pool of request threads, which decode or copy
//! and reply, so a slow read never holds up the others.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::FileExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fuser::{
    AccessFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    InitFlags, KernelConfig, LockOwner, OpenAccMode, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory,
    ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyXattr, Request,
};

use crate::engine::Engine;
use crate::track::Reader;
use crate::tree::{Node, Tree};

/// How long the kernel may cache entries and attributes: the tree never changes.
const TTL: Duration = Duration::from_secs(24 * 3600);

/// How long a reading of the source's size is used before it is read again.
const STATFS_REFRESH: Duration = Duration::from_secs(60);

/// Largest read the kernel is asked to send, and its read-ahead.
pub const MAX_READ: u32 = 1 << 20;

type Job = Box<dyn FnOnce() + Send>;

/// A plain pool of threads taking jobs from a queue.
///
/// Not a rayon pool: a rayon worker waiting for another pool (here, the DST decoders)
/// runs other jobs of its own pool meanwhile, and a second read of the same file would
/// then wait, on the same thread, for the lock the first one holds.
#[derive(Debug, Clone)]
pub struct Workers {
    tx: Sender<Job>,
}

impl Workers {
    /// `n` threads named `name-<i>`.
    pub fn new(n: usize, name: &str) -> io::Result<Workers> {
        let (tx, rx) = channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..n.max(1) {
            let rx: Arc<Mutex<Receiver<Job>>> = rx.clone();
            std::thread::Builder::new().name(format!("{name}-{i}")).spawn(move || {
                loop {
                    let job = match rx.lock() {
                        Ok(r) => r.recv(),
                        Err(_) => return,
                    };
                    match job {
                        Ok(job) => {
                            if catch_unwind(AssertUnwindSafe(job)).is_err() {
                                log::error!("a request thread job panicked");
                            }
                        }
                        Err(_) => return,
                    }
                }
            })?;
        }
        Ok(Workers { tx })
    }

    /// Runs `job` on one of the threads.
    pub fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        let _ = self.tx.send(Box::new(job));
    }
}

/// The filesystem.
#[derive(Debug)]
pub struct SacdFs {
    tree: Arc<Tree>,
    engine: Arc<Engine>,
    requests: Workers,
    handles: Mutex<HashMap<u64, Open>>,
    next_fh: AtomicU64,
    uid: u32,
    gid: u32,
    /// The source filesystem's size, and when it was read: see [`SacdFs::statfs`].
    source_size: Arc<Mutex<SourceSize>>,
}

/// An open file.
#[derive(Debug, Clone)]
enum Open {
    /// A track file, read through the engine.
    Track(Arc<Reader>),
    /// A sidecar file: its source file, open.
    Sidecar(Arc<std::fs::File>),
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

/// The size of the source filesystem, as `statfs` reports it.
#[derive(Debug, Clone, Copy)]
struct SourceSize {
    blocks: u64,
    frsize: u32,
    read: Instant,
    refreshing: bool,
}

impl SourceSize {
    /// `statvfs` of `path`; 0 blocks if it fails.
    fn of(path: &std::path::Path) -> SourceSize {
        let (blocks, frsize) = statvfs(path).unwrap_or_else(|e| {
            log::debug!("{}: statvfs: {e}", path.display());
            (0, 1 << 16)
        });
        SourceSize { blocks, frsize, read: Instant::now(), refreshing: false }
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
    /// `request_threads` threads. Files belong to `uid`/`gid`.
    pub fn new(
        tree: Arc<Tree>,
        engine: Arc<Engine>,
        request_threads: usize,
        uid: u32,
        gid: u32,
    ) -> io::Result<Self> {
        let requests = Workers::new(request_threads, "sacdfs-read")?;
        // Read now, while the source has just been walked; refreshed in the background.
        let source_size = Arc::new(Mutex::new(SourceSize::of(tree.source())));
        Ok(SacdFs {
            tree,
            engine,
            requests,
            handles: Mutex::new(HashMap::new()),
            next_fh: AtomicU64::new(1),
            uid,
            gid,
            source_size,
        })
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
        self.handles.lock().unwrap_or_else(|p| p.into_inner()).get(&fh).cloned()
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
        if let Err(missing) = config.add_capabilities(InitFlags::FUSE_EXPORT_SUPPORT) {
            log::info!("the kernel offers no FUSE export support ({missing:?}): no NFS export");
        }
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let mut negative = false;
        let ino = match name.as_encoded_bytes() {
            // The kernel's export code asks for `.` of an inode number from a file handle
            // and for `..` of a directory, to rebuild its path.
            b"." => self.tree.get(parent.0).map(|_| parent.0),
            b".." => match self.tree.get(parent.0).as_deref() {
                Some(Node::Dir(d)) => Some(d.parent),
                Some(Node::File(_) | Node::Sidecar(_)) => {
                    reply.error(Errno::ENOTDIR);
                    return;
                }
                None => None,
            },
            _ => match self.tree.children(parent.0) {
                Some(children) => {
                    negative = true;
                    children.iter().find(|e| e.name == name).map(|e| e.ino)
                }
                None => None,
            },
        };
        match ino.and_then(|ino| self.tree.get(ino).map(|n| (ino, n))) {
            Some((ino, node)) => {
                reply.entry(&TTL, &self.attr(ino, &node), Generation(self.tree.generation(&node)))
            }
            // A name missing from a directory stays missing (its listing is made once), so
            // the kernel may remember that it is: inode 0 is a negative entry, cached for
            // the TTL (#61). `.` and `..` of an unknown number stay errors, for the export
            // code.
            None if negative => reply.entry(&TTL, &negative_attr(), Generation(0)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.tree.get(ino.0) {
            Some(node) => reply.attr(&TTL, &self.attr(ino.0, &node)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if flags.acc_mode() != OpenAccMode::O_RDONLY {
            reply.error(Errno::EROFS);
            return;
        }
        let Some(node) = self.tree.get(ino.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let open = match &*node {
            Node::File(file) => Open::Track(Arc::new(Reader::new(file.clone()))),
            Node::Sidecar(s) => match self.tree.open_sidecar(s) {
                Ok(f) => Open::Sidecar(Arc::new(f)),
                Err(e) => {
                    log::info!("sidecar {}: open: {e}", s.target.display());
                    reply.error(Errno::from_i32(e.raw_os_error().unwrap_or(libc::EIO)));
                    return;
                }
            },
            Node::Dir(_) => {
                reply.error(Errno::EISDIR);
                return;
            }
        };
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().unwrap_or_else(|p| p.into_inner()).insert(fh, open);
        reply.opened(FileHandle(fh), FopenFlags::FOPEN_KEEP_CACHE);
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let reader = match self.handle(fh.0) {
            Some(Open::Track(r)) => r,
            Some(Open::Sidecar(file)) => {
                // The source may be slow (a network filesystem): read on a request thread.
                self.requests.spawn(move || match read_plain(&file, offset, size) {
                    Ok(data) => reply.data(&data),
                    Err(e) => {
                        log::warn!("sidecar read at {offset}: {e}");
                        reply.error(Errno::EIO);
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
        self.requests.spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| reader.read(&engine, offset, u64::from(size))));
            match result {
                Ok(Ok((data, ahead))) => {
                    reply.data(&data);
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
        self.handles.lock().unwrap_or_else(|p| p.into_inner()).remove(&fh.0);
        reply.ok();
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        match self.tree.get(ino.0).as_deref() {
            Some(Node::Dir(_)) => {
                reply.opened(FileHandle(0), FopenFlags::FOPEN_KEEP_CACHE | FopenFlags::FOPEN_CACHE_DIR)
            }
            Some(Node::File(_) | Node::Sidecar(_)) => reply.error(Errno::ENOTDIR),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let Some(children) = self.tree.children(ino.0) else {
            reply.error(Errno::ENOTDIR);
            return;
        };
        let parent = self.tree.parent_of(ino.0);
        let dots =
            [(ino.0, FileType::Directory, OsStr::new(".")), (parent, FileType::Directory, OsStr::new(".."))];
        let entries = dots.into_iter().chain(children.iter().map(|e| {
            (e.ino, if e.is_dir { FileType::Directory } else { FileType::RegularFile }, e.name.as_os_str())
        }));
        for (i, (ino, kind, name)) in entries.enumerate().skip(offset as usize) {
            if reply.add(INodeNo(ino), i as u64 + 1, kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn access(&self, _req: &Request, ino: INodeNo, mask: AccessFlags, reply: ReplyEmpty) {
        match self.tree.get(ino.0).as_deref() {
            None => reply.error(Errno::ENOENT),
            Some(_) if mask.contains(AccessFlags::W_OK) => reply.error(Errno::EROFS),
            Some(Node::File(_) | Node::Sidecar(_)) if mask.contains(AccessFlags::X_OK) => {
                reply.error(Errno::EACCES)
            }
            Some(_) => reply.ok(),
        }
    }

    // No extended attributes. ENOSYS, not ENODATA or an empty list: the kernel then remembers
    // that the filesystem has none and answers every later request itself, with EOPNOTSUPP
    // (#61), instead of sending one per file listed.
    fn getxattr(&self, _req: &Request, _ino: INodeNo, _name: &OsStr, _size: u32, reply: ReplyXattr) {
        reply.error(Errno::ENOSYS);
    }

    fn listxattr(&self, _req: &Request, _ino: INodeNo, _size: u32, reply: ReplyXattr) {
        reply.error(Errno::ENOSYS);
    }

    /// The source filesystem's size, all of it used: `df` and SMB clients see a filesystem
    /// of that size, full (#62). Read at mount and again at most every minute, in the
    /// background, so that a slow source never holds up this reply.
    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let size = {
            let mut s = self.source_size.lock().unwrap_or_else(|p| p.into_inner());
            if !s.refreshing && s.read.elapsed() >= STATFS_REFRESH {
                s.refreshing = true;
                let (cell, path) = (self.source_size.clone(), self.tree.source().to_owned());
                self.requests.spawn(move || {
                    let new = SourceSize::of(&path);
                    *cell.lock().unwrap_or_else(|p| p.into_inner()) = new;
                });
            }
            *s
        };
        let files = self.tree.len() as u64;
        reply.statfs(size.blocks, 0, 0, files, 0, 1 << 16, 255, size.frsize);
    }
}
