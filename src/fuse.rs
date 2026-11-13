// SPDX-License-Identifier: GPL-3.0-or-later

//! The Linux FUSE front end: requests from the kernel, through fuser, answered by the core
//! ([`crate::vfs::Volume`], #22).
//!
//! Each callback converts fuser's types and hands the request, with its reply, to the core,
//! which answers on fuser's thread what is in memory and moves the rest to its request
//! threads, under the watchdog. What is left here is FUSE's own: `INIT`, the kernel's caching
//! flags, the decoding of open flags and access masks, extended attributes, and the errno of
//! each core error.

use std::ffi::OsStr;
use std::io;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fuser::{
    AccessFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    InitFlags, KernelConfig, LockOwner, OpenAccMode, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory,
    ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyXattr, Request,
};

use crate::engine::Engine;
use crate::tree::Tree;
use crate::vfs::{self, Access, Attr, DataReply, DirOpen, Kind, Listing, ListingPlus, Lookup, Reply, Volume};
use crate::watchdog::{Fail, Watchdog};

pub use crate::vfs::MAX_READ;

/// The errno for a core error.
fn errno(e: vfs::Error) -> Errno {
    match e {
        vfs::Error::NotFound => Errno::ENOENT,
        vfs::Error::NoMemory => Errno::ENOMEM,
        vfs::Error::NotDir => Errno::ENOTDIR,
        vfs::Error::IsDir => Errno::EISDIR,
        vfs::Error::ReadOnly => Errno::EROFS,
        vfs::Error::Access => Errno::EACCES,
        vfs::Error::Stale => Errno::ESTALE,
        vfs::Error::BadHandle => Errno::EBADF,
        vfs::Error::Io => Errno::EIO,
        vfs::Error::Os(n) => Errno::from_i32(n),
    }
}

macro_rules! fail_impl {
    ($($t:ty),*) => {$(
        impl Fail for $t {
            fn fail(self, err: vfs::Error) {
                self.error(errno(err));
            }
        }
    )*};
}
fail_impl!(ReplyAttr, ReplyData, ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyOpen);

fn file_type(kind: Kind) -> FileType {
    match kind {
        Kind::Dir => FileType::Directory,
        Kind::File => FileType::RegularFile,
    }
}

fn file_attr(a: &Attr) -> FileAttr {
    FileAttr {
        ino: INodeNo(a.ino),
        size: a.size,
        blocks: a.blocks,
        atime: a.atime,
        mtime: a.mtime,
        ctime: a.ctime,
        crtime: a.crtime,
        kind: file_type(a.kind),
        perm: a.perm,
        nlink: a.nlink,
        uid: a.uid,
        gid: a.gid,
        rdev: 0,
        blksize: a.blksize,
        flags: 0,
    }
}

/// The attributes of a negative entry: only the inode number 0 counts.
fn negative_attr() -> FileAttr {
    let t = SystemTime::UNIX_EPOCH;
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

impl Reply<Lookup> for ReplyEntry {
    fn reply(self, r: Result<Lookup, vfs::Error>) {
        match r {
            Ok(Lookup::Found(e)) => self.entry(&e.ttl, &file_attr(&e.attr), Generation(e.generation)),
            // Inode 0 is a negative entry, which the kernel caches for the TTL (#61).
            Ok(Lookup::Missing { ttl }) => self.entry(&ttl, &negative_attr(), Generation(0)),
            Err(e) => self.error(errno(e)),
        }
    }
}

impl Reply<(Duration, Attr)> for ReplyAttr {
    fn reply(self, r: Result<(Duration, Attr), vfs::Error>) {
        match r {
            Ok((ttl, attr)) => self.attr(&ttl, &file_attr(&attr)),
            Err(e) => self.error(errno(e)),
        }
    }
}

/// A file's handle: the kernel keeps its cached pages at the next open, as the files never
/// change.
impl Reply<u64> for ReplyOpen {
    fn reply(self, r: Result<u64, vfs::Error>) {
        match r {
            Ok(fh) => self.opened(FileHandle(fh), FopenFlags::FOPEN_KEEP_CACHE),
            Err(e) => self.error(errno(e)),
        }
    }
}

/// A folder's handle: the kernel caches every listing (#205), and keeps it at an open only
/// while the core says it is still the folder's.
impl Reply<DirOpen> for ReplyOpen {
    fn reply(self, r: Result<DirOpen, vfs::Error>) {
        match r {
            Ok(DirOpen { keep_cache: true }) => {
                self.opened(FileHandle(0), FopenFlags::FOPEN_KEEP_CACHE | FopenFlags::FOPEN_CACHE_DIR)
            }
            Ok(DirOpen { keep_cache: false }) => self.opened(FileHandle(0), FopenFlags::FOPEN_CACHE_DIR),
            Err(e) => self.error(errno(e)),
        }
    }
}

impl Reply<()> for ReplyEmpty {
    fn reply(self, r: Result<(), vfs::Error>) {
        match r {
            Ok(()) => self.ok(),
            Err(e) => self.error(errno(e)),
        }
    }
}

impl DataReply for ReplyData {
    fn data(self, data: &[u8]) {
        ReplyData::data(self, data);
    }
}

/// A `readdir` reply and the position it starts at.
struct Dir(ReplyDirectory, u64);

impl Fail for Dir {
    fn fail(self, err: vfs::Error) {
        self.0.error(errno(err));
    }
}

impl Reply<Listing> for Dir {
    fn reply(self, r: Result<Listing, vfs::Error>) {
        let Dir(mut reply, offset) = self;
        let listing = match r {
            Ok(l) => l,
            Err(e) => return reply.error(errno(e)),
        };
        for (next, ino, kind, name) in listing.entries(offset) {
            if reply.add(INodeNo(ino), next, file_type(kind), name) {
                break;
            }
        }
        reply.ok();
    }
}

impl Reply<ListingPlus> for ReplyDirectoryPlus {
    fn reply(mut self, r: Result<ListingPlus, vfs::Error>) {
        let entries = match r {
            Ok(entries) => entries,
            Err(e) => return self.error(errno(e)),
        };
        for (next, name, e) in &entries {
            let attr = file_attr(&e.attr);
            if self.add(attr.ino, *next, name, &e.ttl, &attr, Generation(e.generation)) {
                break;
            }
        }
        self.ok();
    }
}

/// The filesystem, as fuser sees it.
#[derive(Debug)]
pub struct SacdFs {
    volume: Volume,
}

impl SacdFs {
    /// A filesystem showing `tree`, decoding with `engine`: see [`Volume::new`].
    pub fn new(
        tree: Arc<Tree>,
        engine: Arc<Engine>,
        request_threads: usize,
        uid: u32,
        gid: u32,
    ) -> io::Result<Self> {
        Ok(SacdFs { volume: Volume::new(tree, engine, request_threads, uid, gid)? })
    }

    /// The same filesystem, failing a request that waits longer than `timeout`: see
    /// [`Volume::with_request_timeout`].
    pub fn with_request_timeout(self, timeout: Option<Duration>) -> io::Result<Self> {
        Ok(SacdFs { volume: self.volume.with_request_timeout(timeout)? })
    }

    /// The watchdog.
    pub fn watchdog(&self) -> &Watchdog {
        self.volume.watchdog()
    }

    /// The core this front end calls.
    pub fn volume(&self) -> &Volume {
        &self.volume
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
        self.volume.lookup(parent.0, name, reply);
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, fh: Option<FileHandle>, reply: ReplyAttr) {
        self.volume.getattr(ino.0, fh.map(|f| f.0), reply);
    }

    fn open(&self, req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if flags.acc_mode() != OpenAccMode::O_RDONLY {
            reply.error(errno(vfs::Error::ReadOnly));
            return;
        }
        self.volume.open_by(ino.0, req.pid(), reply);
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
        self.volume.read(ino.0, fh.0, offset, size, reply);
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
        self.volume.release(fh.0);
        reply.ok();
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        self.volume.opendir(ino.0, reply);
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, reply: ReplyDirectory) {
        self.volume.readdir(ino.0, Dir(reply, offset));
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
        self.volume.readdir_plus(ino.0, offset, reply);
    }

    fn access(&self, _req: &Request, ino: INodeNo, mask: AccessFlags, reply: ReplyEmpty) {
        let want =
            Access { write: mask.contains(AccessFlags::W_OK), execute: mask.contains(AccessFlags::X_OK) };
        self.volume.access(ino.0, want, reply);
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

    /// The volume's size: see [`Volume::statfs`].
    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let s = self.volume.statfs();
        reply.statfs(s.blocks, 0, 0, s.files, 0, s.bsize, s.namelen, s.frsize);
    }
}
