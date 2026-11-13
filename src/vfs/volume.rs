// SPDX-License-Identifier: GPL-3.0-or-later

//! [`Volume`]: the requests every front end asks, answered from the tree and the engine.
//!
//! What is in memory already (the attributes of a node made already) is answered on the
//! caller's thread; everything that may read the source (lookups and listings, which open
//! images, opens, reads) is moved with its reply to a pool of request threads, reads to one
//! and metadata to another, so that a slow or stalled source does not hold up the requests of
//! the others (#63). Each moved request is watched: one that waits longer than the request
//! timeout is failed with [`Error::Io`] ([`crate::watchdog`], #72), and an image with several
//! reads blocked past it has its new reads failed at once, so that clients retrying through a
//! long hang can't take every read thread ([`super::HUNG_READS_PER_IMAGE`], #241).

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::fs::FileExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::{Error, ImageReads, ShareSize, Workers};
use crate::engine::Engine;
use crate::track::Reader;
use crate::tree::{Entry, Node, ROOT, SidecarFile, Tree};
use crate::watchdog::{self, Fail, Watchdog, What};

/// How long a client may cache entries and attributes: the tree never changes.
pub const TTL: Duration = Duration::from_secs(24 * 3600);

/// How long a reading of the source's size is used before it is read again.
const STATFS_REFRESH: Duration = Duration::from_secs(60);

/// A reply that takes a request's result. A front end implements it for its replies, once per
/// kind of result; the core calls it once, on any thread.
pub trait Reply<T>: Fail {
    /// Answers the request with `r`.
    fn reply(self, r: Result<T, Error>);
}

/// The reply of a read: the bytes, borrowed for the call.
pub trait DataReply: Fail {
    /// Answers the read with `data`.
    fn data(self, data: &[u8]);
}

/// A file or a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
}

/// The attributes of a file or folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attr {
    pub ino: u64,
    pub size: u64,
    /// In 512-byte blocks.
    pub blocks: u64,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub crtime: SystemTime,
    pub kind: Kind,
    pub perm: u16,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub blksize: u32,
}

/// An entry as a lookup finds it: how long the client may keep it, its attributes and its
/// generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryAttr {
    pub ttl: Duration,
    pub attr: Attr,
    pub generation: u64,
}

/// What a lookup found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    Found(EntryAttr),
    /// The name is missing from the folder, and stays missing for `ttl` (a negative entry, #61).
    Missing {
        ttl: Duration,
    },
}

/// An open folder: whether the client's cached listing of it is still its listing (#205).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirOpen {
    pub keep_cache: bool,
}

/// What `access` is asked about; reading is always allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Access {
    pub write: bool,
    pub execute: bool,
}

/// The size of the volume, as `statfs` reports it: the images' total, all of it used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatFs {
    /// Blocks of `frsize` bytes.
    pub blocks: u64,
    pub frsize: u32,
    /// The preferred block size.
    pub bsize: u32,
    /// Files and folders.
    pub files: u64,
    /// Longest name, in bytes.
    pub namelen: u32,
}

/// A folder's listing: `.`, `..` and its entries, borrowed from the tree.
#[derive(Debug, Clone)]
pub struct Listing {
    ino: u64,
    parent: u64,
    children: Arc<Vec<Entry>>,
}

impl Listing {
    /// The entries from position `offset` on, `.` and `..` first: each with the position of
    /// the one after it, its inode number, its kind and its name.
    pub fn entries(&self, offset: u64) -> impl Iterator<Item = (u64, u64, Kind, &OsStr)> + '_ {
        let dots = [(self.ino, Kind::Dir, OsStr::new(".")), (self.parent, Kind::Dir, OsStr::new(".."))];
        let entries = dots.into_iter().chain(
            self.children
                .iter()
                .map(|e| (e.ino, if e.is_dir { Kind::Dir } else { Kind::File }, e.name.as_os_str())),
        );
        entries
            .enumerate()
            .skip(offset as usize)
            .map(|(i, (ino, kind, name))| (i as u64 + 1, ino, kind, name))
    }
}

/// The entries of a folder with what [`Volume::lookup`] gives for each: the position of the
/// one after it, its name, its entry.
pub type ListingPlus = Vec<(u64, OsString, EntryAttr)>;

/// The filesystem, as every front end sees it.
#[derive(Debug)]
pub struct Volume {
    shared: Arc<Shared>,
    engine: Arc<Engine>,
    /// Reads, decoding or copying.
    requests: Workers,
    /// Metadata requests that may read the source (lookups, listings, opens): see
    /// [`Volume::serve`].
    meta: Workers,
    watchdog: Watchdog,
    /// Reads running per image, to stop a hung image taking every read thread (#241).
    image_reads: Arc<ImageReads>,
    /// The size `statfs` reports, and when it was read: see [`Volume::statfs`].
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

impl Volume {
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
        Ok(Volume {
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
            image_reads: ImageReads::new(Some(watchdog::DEFAULT_TIMEOUT)),
            size,
        })
    }

    /// The same filesystem, failing a request that waits longer than `timeout` (`None` or
    /// zero: requests wait as long as they take).
    pub fn with_request_timeout(mut self, timeout: Option<Duration>) -> io::Result<Self> {
        self.watchdog = Watchdog::new(timeout)?;
        self.image_reads = ImageReads::new(self.watchdog.timeout());
        Ok(self)
    }

    /// The watchdog.
    pub fn watchdog(&self) -> &Watchdog {
        &self.watchdog
    }

    /// The tree shown.
    pub fn tree(&self) -> &Arc<Tree> {
        &self.shared.tree
    }

    /// Answers a request that may read the source off the caller's thread: `work` runs on a
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

    /// Looks `name` up in folder `parent`.
    pub fn lookup(&self, parent: u64, name: &OsStr, reply: impl Reply<Lookup>) {
        let name = name.to_owned();
        self.serve(
            What::new("lookup", InName(parent, name.clone())),
            reply,
            move |sh| sh.lookup(parent, &name),
            |reply, r| {
                if let Some(reply) = reply {
                    reply.reply(r)
                }
            },
        );
    }

    /// The attributes of `ino`; `fh`, a handle open on it if the front end has one, answers
    /// for a file whose image was removed since it was opened (#153).
    pub fn getattr(&self, ino: u64, fh: Option<u64>, reply: impl Reply<(Duration, Attr)>) {
        // A node made already is answered here, from memory: no handover for the most
        // frequent request. The root first sweeps the source (#123), and a node not made
        // yet is looked for along its path: both read the source.
        if ino != ROOT {
            if let Some(node) = self.shared.tree.loaded(ino) {
                reply.reply(self.shared.getattr(ino, Some(node)));
                return;
            }
        }
        self.serve(
            What::new("getattr", Ino(ino)),
            reply,
            move |sh| {
                if ino == ROOT {
                    // A scan starts at the root: changes anywhere below move its mtime first.
                    sh.tree.sweep();
                }
                // A file open from before its image was removed (#153) is answered from its
                // handle.
                let node = sh.tree.get(ino).or_else(|| sh.opened_node(ino, fh));
                sh.getattr(ino, node)
            },
            |reply, r| {
                if let Some(reply) = reply {
                    reply.reply(r)
                }
            },
        );
    }

    /// Opens file `ino` for reading: a new file handle. The front end refuses an open for
    /// writing itself, with [`Error::ReadOnly`].
    pub fn open(&self, ino: u64, reply: impl Reply<u64>) {
        self.open_by(ino, 0, reply);
    }

    /// [`Volume::open`] for the process (thread) `pid` of the request, 0 if unknown. A network
    /// file server's process opens it for a network client ([`network_server`]).
    pub fn open_by(&self, ino: u64, pid: u32, reply: impl Reply<u64>) {
        // Opening a sidecar opens its source file.
        self.serve(
            What::new("open", Ino(ino)),
            reply,
            move |sh| (sh.open(ino, sh.tree.get(ino), pid), sh.clone()),
            |reply, (r, sh)| match reply {
                Some(reply) => reply.reply(r),
                // Too late: the client never learns of the handle, so it is closed here.
                None => {
                    if let Ok(fh) = r {
                        sh.handles.lock().unwrap_or_else(|p| p.into_inner()).remove(&fh);
                    }
                }
            },
        );
    }

    /// Reads up to `size` bytes at `offset` of the file open as `fh` (inode `ino`, for the
    /// log), fewer only at its end.
    pub fn read(&self, ino: u64, fh: u64, offset: u64, size: u32, reply: impl DataReply) {
        let reader = match self.shared.handle(fh) {
            Some(Open::Track(r)) => r,
            Some(Open::Sidecar(file, sidecar)) => {
                // The source may be slow (a network filesystem): read on a request thread.
                let pending = self.watchdog.watch(What::new("sidecar read", Ino(ino)), reply);
                self.requests.spawn(move || {
                    pending.running();
                    let r = read_plain(&file, offset, size);
                    let Some(reply) = pending.take() else { return };
                    match r {
                        Ok(data) => reply.data(&data),
                        Err(e) => {
                            let target = sidecar.target.display();
                            crate::errlog::failed(
                                &format!("sidecar {target}"),
                                format_args!("sidecar {target} (inode {ino}): read at {offset}: {e}: EIO"),
                            );
                            reply.fail(Error::Io);
                        }
                    }
                });
                return;
            }
            None => {
                crate::errlog::failed(
                    &format!("handle {fh}"),
                    format_args!("read of inode {ino} at {offset}: no open file {fh}: EBADF"),
                );
                reply.fail(Error::BadHandle);
                return;
            }
        };
        // An image with reads blocked past the timeout gets no thread to block one more (#241).
        let disc = &reader.file.disc;
        let Some(counted) = self.image_reads.start(disc.id, &disc.path) else {
            let path = disc.path.display();
            crate::errlog::failed(
                &path.to_string(),
                format_args!(
                    "{path} track {} (inode {ino}): read at {offset}: refused, the image has \
                     reads blocked past the request timeout: EIO",
                    reader.file.number
                ),
            );
            reply.fail(Error::Io);
            return;
        };
        // In progress from now, while it may wait for a thread, until after its reply (#357).
        let arrival = reader.arrived();
        let engine = self.engine.clone();
        let pool = self.requests.clone();
        let key = reader.file.disc.path.display().to_string();
        let what = What::keyed("read", key, ReadOf { reader: reader.clone(), offset, size });
        let pending = self.watchdog.watch(what, reply);
        self.requests.spawn(move || {
            pending.running();
            let result = catch_unwind(AssertUnwindSafe(|| reader.read(&engine, offset, u64::from(size))));
            drop(counted);
            let Some(reply) = pending.take() else { return };
            match result {
                Ok(Ok((data, ahead))) => {
                    reply.data(&data);
                    drop(arrival);
                    crate::track::give_back(data);
                    if let Some(ahead) = ahead {
                        pool.spawn(move || {
                            let _ = catch_unwind(AssertUnwindSafe(|| reader.read_ahead(&engine, ahead)));
                        });
                    }
                }
                Ok(Err(e)) => {
                    let path = reader.file.disc.path.display();
                    crate::errlog::failed(
                        &path.to_string(),
                        format_args!(
                            "{path} track {} (inode {ino}): read at {offset}: {e}: EIO",
                            reader.file.number
                        ),
                    );
                    reply.fail(Error::Io);
                }
                Err(_) => {
                    let path = reader.file.disc.path.display();
                    crate::errlog::failed(
                        &path.to_string(),
                        format_args!(
                            "{path} track {} (inode {ino}): read at {offset} panicked: EIO",
                            reader.file.number
                        ),
                    );
                    reply.fail(Error::Io);
                }
            }
        });
    }

    /// Closes the file open as `fh`.
    pub fn release(&self, fh: u64) {
        self.shared.handles.lock().unwrap_or_else(|p| p.into_inner()).remove(&fh);
    }

    /// Opens folder `ino`.
    pub fn opendir(&self, ino: u64, reply: impl Reply<DirOpen>) {
        // A folder listed already and never re-read is answered here; one that may be re-read
        // (or listed) first reads its source, on a request thread.
        if let Some(node) = self.shared.tree.loaded(ino) {
            let keep = match &*node {
                Node::Dir(_) => self.shared.tree.keep_listing(ino, false),
                _ => Some(false),
            };
            if let Some(keep) = keep {
                reply.reply(self.shared.opendir(Some(node), keep));
                return;
            }
        }
        self.serve(
            What::new("opendir", Ino(ino)),
            reply,
            move |sh| {
                let keep = sh.tree.keep_listing(ino, true).unwrap_or(false);
                sh.opendir(sh.tree.get(ino), keep)
            },
            |reply, r| {
                if let Some(reply) = reply {
                    reply.reply(r)
                }
            },
        );
    }

    /// The listing of folder `ino`.
    pub fn readdir(&self, ino: u64, reply: impl Reply<Listing>) {
        // Listing a directory the first time opens its images (reads their TOCs).
        let tree = self.shared.tree.clone();
        let refusing = move || tree.refusing();
        self.serve(
            What::new("readdir", Ino(ino)),
            reply,
            move |sh| sh.tree.children(ino).map(|c| (c, sh.tree.parent_of(ino))),
            move |reply, found: Option<(Arc<Vec<Entry>>, u64)>| {
                let Some(reply) = reply else { return };
                let Some((children, parent)) = found else {
                    reply.reply(Err(if refusing() { Error::NoMemory } else { Error::NotDir }));
                    return;
                };
                reply.reply(Ok(Listing { ino, parent, children }));
            },
        );
    }

    /// The entries of folder `ino` from position `offset` on (`.` and `..` first), each with
    /// what its lookup gives (#205).
    pub fn readdir_plus(&self, ino: u64, offset: u64, reply: impl Reply<ListingPlus>) {
        let tree = self.shared.tree.clone();
        let refusing = move || tree.refusing();
        self.serve(
            What::new("readdirplus", Ino(ino)),
            reply,
            move |sh| sh.listing_plus(ino, offset),
            move |reply, found| {
                let Some(reply) = reply else { return };
                let Some(entries) = found else {
                    reply.reply(Err(if refusing() { Error::NoMemory } else { Error::NotDir }));
                    return;
                };
                reply.reply(Ok(entries));
            },
        );
    }

    /// Whether `ino` may be accessed as `want` asks.
    pub fn access(&self, ino: u64, want: Access, reply: impl Reply<()>) {
        if let Some(node) = self.shared.tree.loaded(ino) {
            reply.reply(Shared::access(Some(node), want));
            return;
        }
        self.serve(
            What::new("access", Ino(ino)),
            reply,
            move |sh| match sh.tree.get(ino) {
                None => Err(sh.not_found()),
                node => Shared::access(node, want),
            },
            |reply, r| {
                if let Some(reply) = reply {
                    reply.reply(r)
                }
            },
        );
    }

    /// The total size of the images found at mount, all of it used, as read-only media report
    /// (#217). With no images, the size of the source's filesystem, read at most every minute
    /// in the background so that a slow source never holds up this reply: never 0 bytes (#62).
    pub fn statfs(&self) -> StatFs {
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
        StatFs { blocks: size.blocks, frsize: size.frsize, bsize: 1 << 16, files, namelen: 255 }
    }
}

impl Shared {
    /// How long the client may keep an entry of `parent` numbered `ino` (a name looked up
    /// there, or the directory's own attributes): [`TTL`], or the refresh interval when
    /// either is re-read when its source changes.
    fn ttl(&self, parent: u64, ino: u64) -> Duration {
        self.tree.live_ttl(parent).or_else(|| self.tree.live_ttl(ino)).unwrap_or(TTL)
    }

    fn attr(&self, ino: u64, node: &Node) -> Attr {
        let (kind, size, perm, nlink) = match node {
            Node::Dir(_) => (Kind::Dir, 0, 0o555, 2),
            Node::File(f) => (Kind::File, f.size, 0o444, 1),
            Node::Sidecar(s) => (Kind::File, s.meta.size, 0o444, 1),
        };
        let mtime = self.tree.mtime(node);
        // A sidecar file shows its source file's times and blocks (#79); everything else
        // one time, its mtime.
        let (atime, ctime, blocks) = match node {
            Node::Sidecar(s) => (s.meta.atime, s.meta.ctime, s.meta.blocks),
            _ => (mtime, mtime, size.div_ceil(512)),
        };
        Attr {
            ino,
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
            blksize: 1 << 16,
        }
    }

    fn handle(&self, fh: u64) -> Option<Open> {
        self.handles.lock().unwrap_or_else(|p| p.into_inner()).get(&fh).map(|(_, o)| o.clone())
    }

    /// The node of a file open as `ino`, once the tree has freed it (#153): its handle
    /// (`fh`, if the client sent it, else any handle open on `ino`) answers for it until
    /// it is released.
    fn opened_node(&self, ino: u64, fh: Option<u64>) -> Option<Arc<Node>> {
        let handles = self.handles.lock().unwrap_or_else(|p| p.into_inner());
        let by_fh = fh.and_then(|fh| handles.get(&fh)).filter(|(i, _)| *i == ino);
        by_fh.or_else(|| handles.values().find(|(i, _)| *i == ino)).map(|(_, o)| Arc::new(o.node()))
    }

    /// The error for a number the tree doesn't find: [`Error::NoMemory`] if it isn't looking,
    /// for lack of memory (#237).
    fn not_found(&self) -> Error {
        if self.tree.refusing() { Error::NoMemory } else { Error::NotFound }
    }

    fn lookup(&self, parent: u64, name: &OsStr) -> Result<Lookup, Error> {
        let mut negative = false;
        let mut volatile = false;
        let ino = match name.as_encoded_bytes() {
            // The kernel's export code asks for `.` of an inode number from a file handle
            // and for `..` of a directory, to rebuild its path.
            b"." => self.tree.get(parent).map(|_| parent),
            b".." => match self.tree.get(parent).as_deref() {
                Some(Node::Dir(d)) => Some(d.parent),
                Some(Node::File(_) | Node::Sidecar(_)) => return Err(Error::NotDir),
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
            return Err(Error::NoMemory);
        }
        match found {
            // A directory that is re-read when its source changes (#101), and what is in
            // it, is remembered for a short time only.
            Some((ino, node)) => {
                let mut entry = self.entry(parent, ino, &node);
                if volatile {
                    entry.ttl = Duration::ZERO;
                }
                Ok(Lookup::Found(entry))
            }
            // A name missing from a directory stays missing (unless the directory is re-read,
            // #101), so the kernel may remember that it is: a negative entry, cached for the
            // TTL (#61). `.` and `..` of an unknown number stay errors, for the export code.
            None if negative => Ok(Lookup::Missing { ttl: self.ttl(parent, parent) }),
            None => Err(Error::NotFound),
        }
    }

    /// The entry of `node`, numbered `ino`, in directory `parent`: how long the client may keep
    /// it, its attributes and generation.
    ///
    /// A track file keeps the full [`TTL`] even in a folder that is re-read (#205): an area is
    /// re-read for its folder art only, which never changes its tracks' names, numbers or
    /// attributes, and a changed image is a new folder in its parent. Without this, every
    /// track's name was looked up again 2 s after the last time, on every listing. With tags
    /// (#81) a track's size and time follow its tags file, so they stay short.
    fn entry(&self, parent: u64, ino: u64, node: &Node) -> EntryAttr {
        let ttl = match node {
            // Art in an area is asked about again each time (#235), in a listing too.
            Node::Sidecar(_) if self.tree.is_area_art(parent, node) => Duration::ZERO,
            Node::File(_) if !crate::tags::installed() => TTL,
            _ => self.ttl(parent, ino),
        };
        EntryAttr { ttl, attr: self.attr(ino, node), generation: self.tree.generation(node) }
    }

    /// The entries of directory `ino` from position `offset` on (`.` and `..` first), with
    /// what [`Shared::entry`] gives for each, for `readdirplus` (#205). A name whose node has
    /// just been let go of is left out (the next listing shows it again); `None` if `ino` is
    /// not a directory (or, short of memory, cannot be listed now).
    fn listing_plus(&self, ino: u64, offset: u64) -> Option<ListingPlus> {
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
                // The kernel caches this page without the name: not past the next open.
                self.tree.unkeep_listing(ino);
                continue;
            };
            // `.` and `..` are not entries of `ino`: the kernel links neither, only lists them.
            out.push((i as u64 + 1, name.to_owned(), self.entry(ino, child, &node)));
        }
        Some(out)
    }

    fn getattr(&self, ino: u64, node: Option<Arc<Node>>) -> Result<(Duration, Attr), Error> {
        match node {
            Some(node) => Ok((self.ttl(ino, ino), self.attr(ino, &node))),
            None => Err(self.not_found()),
        }
    }

    /// Opens `node`: a new file handle.
    fn open(&self, ino: u64, node: Option<Arc<Node>>, pid: u32) -> Result<u64, Error> {
        let Some(node) = node else { return Err(self.not_found()) };
        // New readers wait out a shortage, so that the ones reading now go on (#237).
        if self.tree.refusing() && matches!(&*node, Node::File(_)) {
            return Err(Error::NoMemory);
        }
        let open = match &*node {
            Node::File(file) => Open::Track(Arc::new(Reader::opened_by(file.clone(), network_server(pid)))),
            Node::Sidecar(s) => match self.tree.open_sidecar(s) {
                Ok(f) => Open::Sidecar(Arc::new(f), s.clone()),
                Err(e) => {
                    let gone = e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ESTALE);
                    if gone {
                        // A cover removed or replaced: the client asks again (#235).
                        log::info!("sidecar {}: open: {e}", s.target.display());
                    } else {
                        let target = s.target.display();
                        crate::errlog::failed(
                            &format!("sidecar {target}"),
                            format_args!("sidecar {target} (inode {ino}): open: {e}"),
                        );
                    }
                    // Gone since the client was told of it (#235): it asks again, then.
                    let missing = e.raw_os_error().is_some() && e.kind() == io::ErrorKind::NotFound;
                    if missing && self.tree.art_told_lately(ino) {
                        return Err(Error::Stale);
                    }
                    return Err(e.raw_os_error().map_or(Error::Io, Error::Os));
                }
            },
            Node::Dir(_) => return Err(Error::IsDir),
        };
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().unwrap_or_else(|p| p.into_inner()).insert(fh, (ino, open));
        Ok(fh)
    }

    /// Opens directory `ino` (`keep`: from [`Tree::keep_listing`]). The kernel caches every
    /// listing (#205), and keeps it at an open only while it is still the directory's: a
    /// folder that is re-read gets the cache dropped when its listing was made again.
    fn opendir(&self, node: Option<Arc<Node>>, keep: bool) -> Result<DirOpen, Error> {
        match node.as_deref() {
            Some(Node::Dir(_)) => Ok(DirOpen { keep_cache: keep }),
            Some(Node::File(_) | Node::Sidecar(_)) => Err(Error::NotDir),
            None => Err(self.not_found()),
        }
    }

    fn access(node: Option<Arc<Node>>, want: Access) -> Result<(), Error> {
        match node.as_deref() {
            None => Err(Error::NotFound),
            Some(_) if want.write => Err(Error::ReadOnly),
            Some(Node::File(_) | Node::Sidecar(_)) if want.execute => Err(Error::Access),
            Some(_) => Ok(()),
        }
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
struct InName(u64, OsString);

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

/// Whether process (thread) `pid` is a network file server's: Samba's `smbd`, NFS-Ganesha's
/// `ganesha.nfsd`, or the kernel's `nfsd` or `ksmbd` threads. Its reads come from a network
/// client with its own read-ahead (#362). *Why not only the order of the reads
/// (`NETWORK_REORDER` in `track.rs`):* NFS-Ganesha's reads can reach sacdfs in order, so
/// copies behind it went undetected. *Why the thread group's name:* FUSE gives the calling
/// thread, and NFS-Ganesha's workers have names of their own. 0, or a process in another PID
/// namespace, is unknown: the order of the reads still tells.
fn network_server(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let tgid = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("Tgid:").map(|v| v.trim().to_owned())));
    let Some(tgid) = tgid else { return false };
    let Ok(comm) = std::fs::read_to_string(format!("/proc/{tgid}/comm")) else { return false };
    is_network_server(comm.trim_end())
}

/// Whether a process named `comm` is a network file server's ([`network_server`]). Samba 4.18
/// and later name their processes `smbd[client]` or `smbd: client`, cut to 15 bytes.
fn is_network_server(comm: &str) -> bool {
    comm.starts_with("smbd") || comm == "ganesha.nfsd" || comm == "nfsd" || comm.starts_with("ksmbd")
}

#[cfg(test)]
mod network_tests {
    use super::*;

    #[test]
    fn network_servers_by_name() {
        for c in ["smbd", "smbd[192.168.1.", "smbd: client [", "ganesha.nfsd", "nfsd", "ksmbd:r445"] {
            assert!(is_network_server(c), "{c}");
        }
        for c in ["python3", "cat", "RoonAppliance", "Plex Media Serv", "nfsdcld", "ganesha", "sacdfs"] {
            assert!(!is_network_server(c), "{c}");
        }
        assert!(!network_server(0));
        assert!(!network_server(std::process::id()), "this test is no file server");
    }

    /// By name only: a local program named like a file server counts as one. Its reader is then
    /// treated as behind a network client: it lets an earlier player go first until it pauses
    /// or reads far ahead, and its audio is the same.
    #[test]
    fn a_local_process_named_smbd_counts_as_a_file_server() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("smbd");
        std::fs::copy("/bin/sleep", &fake).unwrap();
        let mut child = std::process::Command::new(&fake).arg("5").spawn().unwrap();
        let pid = child.id();
        let seen = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            network_server(pid)
        });
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(seen, "a process named smbd");
        assert!(!network_server(pid), "gone");
    }
}
