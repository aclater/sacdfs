// SPDX-License-Identifier: GPL-3.0-or-later

//! The FUSE filesystem: requests from the kernel, answered from the tree and the engine.
//!
//! Metadata requests (`lookup`, `getattr`, `readdir`) are answered on fuser's threads.
//! Reads are moved, with their reply, to a pool of request threads, which decode or copy
//! and reply, so a slow read never holds up the others.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuser::{
    AccessFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    KernelConfig, LockOwner, OpenAccMode, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyXattr, Request,
};

use crate::engine::Engine;
use crate::track::Reader;
use crate::tree::{Node, Tree};

/// How long the kernel may cache entries and attributes: the tree never changes.
const TTL: Duration = Duration::from_secs(24 * 3600);

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
    handles: Mutex<HashMap<u64, Arc<Reader>>>,
    next_fh: AtomicU64,
    uid: u32,
    gid: u32,
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
        Ok(SacdFs {
            tree,
            engine,
            requests,
            handles: Mutex::new(HashMap::new()),
            next_fh: AtomicU64::new(1),
            uid,
            gid,
        })
    }

    fn attr(&self, ino: u64, node: &Node) -> FileAttr {
        let (kind, size, perm, nlink, mtime) = match node {
            Node::Dir(d) => (FileType::Directory, 0, 0o555, 2, d.mtime),
            Node::File(f) => (FileType::RegularFile, f.size, 0o444, 1, f.disc.mtime),
        };
        FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: size.div_ceil(512),
            atime: mtime,
            mtime,
            ctime: mtime,
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

    fn reader(&self, fh: u64) -> Option<Arc<Reader>> {
        self.handles.lock().unwrap_or_else(|p| p.into_inner()).get(&fh).cloned()
    }
}

impl Filesystem for SacdFs {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> io::Result<()> {
        if let Err(nearest) = config.set_max_readahead(MAX_READ) {
            let _ = config.set_max_readahead(nearest);
        }
        let _ = config.set_max_background(64);
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match self.tree.lookup(parent.0, name).and_then(|ino| self.tree.get(ino).map(|n| (ino, n))) {
            Some((ino, node)) => reply.entry(&TTL, &self.attr(ino, &node), Generation(0)),
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
        let Node::File(file) = &*node else {
            reply.error(Errno::EISDIR);
            return;
        };
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(fh, Arc::new(Reader::new(file.clone())));
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
        let Some(reader) = self.reader(fh.0) else {
            reply.error(Errno::EBADF);
            return;
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
            Some(Node::File(_)) => reply.error(Errno::ENOTDIR),
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
            Some(Node::File(_)) if mask.contains(AccessFlags::X_OK) => reply.error(Errno::EACCES),
            Some(_) => reply.ok(),
        }
    }

    fn getxattr(&self, _req: &Request, _ino: INodeNo, _name: &OsStr, _size: u32, reply: ReplyXattr) {
        reply.error(Errno::NO_XATTR);
    }

    fn listxattr(&self, _req: &Request, _ino: INodeNo, size: u32, reply: ReplyXattr) {
        if size == 0 { reply.size(0) } else { reply.data(&[]) }
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        reply.statfs(0, 0, 0, self.tree.len() as u64, 0, 1 << 16, 255, 1 << 16);
    }
}
