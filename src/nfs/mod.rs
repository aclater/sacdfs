// SPDX-License-Identifier: GPL-3.0-or-later

//! The NFS front end (#420): the [`Volume`] served over NFSv3, for the Mac's own NFS client
//! (macOS has no FUSE layer: see docs/design.md, "The NFS front end"), or any other NFSv3
//! client, without FUSE. The protocol is nfsserve's ([`proto`], vendored).
//!
//! - **File handles** survive a restart ([`handle`]): they name the volume, the inode
//!   number and its generation, all three the same on every run over the same SOURCE (#18).
//!   A handle whose file is gone, or whose bytes may differ (a new generation), answers
//!   `NFS3ERR_STALE`.
//! - **Opens:** NFS has no open or close. A file is opened in the core on its first read and
//!   kept open while it is read; one idle for [`IDLE`] is closed ([`NfsFront::close_idle`]).
//! - **End of file:** a read at or past the end answers no bytes with `eof` set, NFS's own
//!   end of file, so the empty-read problem of FSKit and macFUSE (#22) does not arise.
//! - **Who may ask:** the AUTH_SYS uid of each call must be the server's user (or root),
//!   unless [`Callers::Anyone`]. A local client can forge it: this guards against mistakes,
//!   not attacks. Anyone who can reach the port can read the files, as with `--allow-other`.

pub mod cli;
pub mod handle;
pub mod proto;
pub mod serve;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::oneshot;

use crate::tree::ROOT;
use crate::vfs::{Attr, DataReply, Error, Kind, Listing, Lookup, MAX_READ, Reply, Volume};
use crate::watchdog::Fail;
use handle::Handle;
use proto::nfs::{
    cookieverf3, fattr3, fileid3, filename3, fsinfo3, ftype3, nfs_fh3, nfspath3, nfsstat3, nfstime3,
    post_op_attr, sattr3, specdata3,
};
use proto::rpc::auth_unix;
use proto::vfs::{
    DirEntry, DirEntrySimple, FsStat, NFSFileSystem, ReadDirResult, ReadDirSimpleResult, VFSCapabilities,
};

/// How long a file read through NFS stays open in the core after its last read.
pub const IDLE: Duration = Duration::from_secs(60);

/// Whose calls are answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callers {
    /// Calls with AUTH_SYS credentials of this uid, or of root.
    User(u32),
    /// Every call.
    Anyone,
}

/// A reply that hands the core's answer to the waiting NFS request. Dropped unanswered (a job
/// that never ran), the receiver sees the sender gone and answers `NFS3ERR_IO`.
struct To<T>(oneshot::Sender<Result<T, Error>>);

impl<T: Send + 'static> Fail for To<T> {
    fn fail(self, err: Error) {
        let _ = self.0.send(Err(err));
    }
}

impl<T: Send + 'static> Reply<T> for To<T> {
    fn reply(self, r: Result<T, Error>) {
        let _ = self.0.send(r);
    }
}

impl DataReply for To<Vec<u8>> {
    fn data(self, data: &[u8]) {
        let _ = self.0.send(Ok(data.to_vec()));
    }
}

/// Asks the core and waits, without holding a runtime thread, for its answer.
async fn ask<T: Send + 'static>(f: impl FnOnce(To<T>)) -> Result<T, nfsstat3> {
    let (tx, rx) = oneshot::channel();
    f(To(tx));
    match rx.await {
        Ok(r) => r.map_err(status),
        Err(_) => Err(nfsstat3::NFS3ERR_IO),
    }
}

/// The core's error as NFSv3 says it.
fn status(e: Error) -> nfsstat3 {
    match e {
        Error::NotFound => nfsstat3::NFS3ERR_NOENT,
        Error::NotDir => nfsstat3::NFS3ERR_NOTDIR,
        Error::IsDir => nfsstat3::NFS3ERR_ISDIR,
        Error::ReadOnly => nfsstat3::NFS3ERR_ROFS,
        Error::Access => nfsstat3::NFS3ERR_ACCES,
        Error::Stale => nfsstat3::NFS3ERR_STALE,
        Error::BadHandle => nfsstat3::NFS3ERR_BADHANDLE,
        // NFSv3 has no "try again later" but JUKEBOX, which clients retry.
        Error::NoMemory => nfsstat3::NFS3ERR_JUKEBOX,
        Error::Io | Error::Os(_) => nfsstat3::NFS3ERR_IO,
    }
}

fn time(t: SystemTime) -> nfstime3 {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    // NFSv3 times are 32-bit seconds: clipped, not wrapped.
    nfstime3 { seconds: u32::try_from(d.as_secs()).unwrap_or(u32::MAX), nseconds: d.subsec_nanos() }
}

fn fattr(a: &Attr, fsid: u64) -> fattr3 {
    fattr3 {
        ftype: match a.kind {
            Kind::Dir => ftype3::NF3DIR,
            Kind::File => ftype3::NF3REG,
        },
        mode: u32::from(a.perm),
        nlink: a.nlink,
        uid: a.uid,
        gid: a.gid,
        size: a.size,
        used: a.blocks.saturating_mul(512),
        rdev: specdata3::default(),
        fsid,
        fileid: a.ino,
        atime: time(a.atime),
        mtime: time(a.mtime),
        ctime: time(a.ctime),
    }
}

/// A file open in the core: its handle, its generation, and when it was last read.
#[derive(Debug, Clone, Copy)]
struct Open {
    fh: u64,
    generation: u64,
    used: Instant,
}

/// The NFS front end on a [`Volume`]. Clones share the volume and the open files.
#[derive(Debug, Clone)]
pub struct NfsFront {
    volume: Arc<Volume>,
    open: Arc<Mutex<HashMap<u64, Open>>>,
    /// Names this volume in file handles ([`handle::volume_id`]).
    volume_id: u64,
    callers: Callers,
}

impl NfsFront {
    /// The front end on `volume`, whose handles carry `volume_id`, answering `callers`.
    pub fn new(volume: Arc<Volume>, volume_id: u64, callers: Callers) -> NfsFront {
        NfsFront { volume, open: Arc::new(Mutex::new(HashMap::new())), volume_id, callers }
    }

    fn opened(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Open>> {
        self.open.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Closes the files not read for [`IDLE`]. The caller runs it now and then.
    pub fn close_idle(&self) {
        let now = Instant::now();
        self.opened().retain(|_, o| {
            let keep = now.duration_since(o.used) < IDLE;
            if !keep {
                self.volume.release(o.fh);
            }
            keep
        });
    }

    /// Closes every file: before the server stops.
    pub fn close_all(&self) {
        for (_, o) in self.opened().drain() {
            self.volume.release(o.fh);
        }
    }

    /// The generation of `ino` now: from an open file, the tree's node, or a lookup of `.`
    /// in it, which finds a number not made yet in this run (as for the kernel's NFS export,
    /// #18).
    async fn generation(&self, ino: u64) -> Result<u64, nfsstat3> {
        if let Some(o) = self.opened().get(&ino) {
            return Ok(o.generation);
        }
        let tree = self.volume.tree();
        if let Some(node) = tree.loaded(ino) {
            return Ok(tree.generation(&node));
        }
        let v = self.volume.clone();
        match ask(move |r| v.lookup(ino, OsStr::new("."), r)).await {
            Ok(Lookup::Found(e)) => Ok(e.generation),
            Ok(Lookup::Missing { .. }) | Err(nfsstat3::NFS3ERR_NOENT) => Err(nfsstat3::NFS3ERR_STALE),
            Err(e) => Err(e),
        }
    }

    /// The core's handle on `ino`, opening it on first use.
    async fn file_handle(&self, ino: u64) -> Result<u64, nfsstat3> {
        if let Some(o) = self.opened().get_mut(&ino) {
            o.used = Instant::now();
            return Ok(o.fh);
        }
        let generation = self.generation(ino).await?;
        let v = self.volume.clone();
        let fh = ask(move |r| v.open(ino, r)).await?;
        let mut open = self.opened();
        // Two first reads at once: keep one handle, close the other.
        if let Some(o) = open.get(&ino) {
            self.volume.release(fh);
            return Ok(o.fh);
        }
        open.insert(ino, Open { fh, generation, used: Instant::now() });
        Ok(fh)
    }

    async fn attr(&self, ino: u64) -> Result<Attr, nfsstat3> {
        let fh = self.opened().get(&ino).map(|o| o.fh);
        let v = self.volume.clone();
        Ok(ask(move |r| v.getattr(ino, fh, r)).await?.1)
    }

    async fn listing(&self, dir: u64) -> Result<Listing, nfsstat3> {
        let v = self.volume.clone();
        ask(move |r| v.readdir(dir, r)).await
    }

    /// The position in `dir`'s listing after the entry numbered `start_after` (0: the first
    /// entry after `.` and `..`), and the listing. Cookies are inode numbers, which stay
    /// valid across restarts.
    async fn window(&self, dir: u64, start_after: u64) -> Result<(u64, Listing), nfsstat3> {
        let l = self.listing(dir).await?;
        if start_after == 0 {
            return Ok((2, l));
        }
        let from = l.entries(2).find(|(_, ino, ..)| *ino == start_after).map(|(next, ..)| next);
        match from {
            Some(from) => Ok((from, l)),
            None => Err(nfsstat3::NFS3ERR_BAD_COOKIE),
        }
    }
}

/// `true` if `callers` may be answered for a call with credentials `auth` (`None`: not
/// AUTH_SYS).
pub fn permitted(callers: Callers, auth: Option<&auth_unix>) -> bool {
    match callers {
        Callers::Anyone => true,
        Callers::User(uid) => auth.is_some_and(|a| a.uid == uid || a.uid == 0),
    }
}

#[async_trait]
impl NFSFileSystem for NfsFront {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadOnly
    }

    fn root_dir(&self) -> fileid3 {
        ROOT
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let name = OsStr::from_bytes(&filename.0);
        if name == "." {
            return Ok(dirid);
        }
        if name == ".." {
            // The listing's second entry is the parent.
            let l = self.listing(dirid).await?;
            return l.entries(1).next().map(|(_, ino, ..)| ino).ok_or(nfsstat3::NFS3ERR_NOENT);
        }
        let (v, name) = (self.volume.clone(), name.to_owned());
        match ask(move |r| v.lookup(dirid, &name, r)).await? {
            Lookup::Found(e) => Ok(e.attr.ino),
            Lookup::Missing { .. } => Err(nfsstat3::NFS3ERR_NOENT),
        }
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        Ok(fattr(&self.attr(id).await?, self.volume_id))
    }

    async fn setattr(&self, _id: fileid3, _setattr: sattr3) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn read(&self, id: fileid3, offset: u64, count: u32) -> Result<(Vec<u8>, bool), nfsstat3> {
        let fh = self.file_handle(id).await?;
        let count = count.min(MAX_READ);
        let v = self.volume.clone();
        let data: Vec<u8> = ask(move |r| v.read(id, fh, offset, count, r)).await?;
        // The core answers fewer bytes only at the end of the file.
        let eof = data.len() < count as usize;
        Ok((data, eof))
    }

    async fn write(&self, _id: fileid3, _offset: u64, _data: &[u8]) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
        _attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create_exclusive(&self, _dirid: fileid3, _filename: &filename3) -> Result<fileid3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn mkdir(&self, _dirid: fileid3, _dirname: &filename3) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn remove(&self, _dirid: fileid3, _filename: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn rename(
        &self,
        _from_dirid: fileid3,
        _from_filename: &filename3,
        _to_dirid: fileid3,
        _to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    /// Up to `max_entries` entries after the one numbered `start_after`, with their
    /// attributes, without `.` and `..` (READDIRPLUS). Only this page's attributes are
    /// made: entries listed already answer from memory.
    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let (from, listing) = self.window(dirid, start_after).await?;
        let max = max_entries.max(1);
        let page: Vec<(u64, Vec<u8>)> = listing
            .entries(from)
            .take(max + 1)
            .map(|(_, ino, _, name)| (ino, name.as_bytes().to_vec()))
            .collect();
        let end = page.len() <= max;
        let mut entries = Vec::with_capacity(page.len().min(max));
        for (ino, name) in page.into_iter().take(max) {
            match self.attr(ino).await {
                Ok(a) => {
                    entries.push(DirEntry { fileid: ino, name: name.into(), attr: fattr(&a, self.volume_id) })
                }
                // Gone since the listing (a removed image): left out, as the FUSE front
                // end leaves it out.
                Err(nfsstat3::NFS3ERR_NOENT) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(ReadDirResult { entries, end })
    }

    /// The names only (READDIR): from the listing, no attributes.
    async fn readdir_simple(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        count: usize,
    ) -> Result<ReadDirSimpleResult, nfsstat3> {
        let (from, listing) = self.window(dirid, start_after).await?;
        let max = count.max(1);
        let mut entries: Vec<DirEntrySimple> = listing
            .entries(from)
            .take(max + 1)
            .map(|(_, ino, _, name)| DirEntrySimple { fileid: ino, name: name.as_bytes().to_vec().into() })
            .collect();
        let end = entries.len() <= max;
        entries.truncate(max);
        Ok(ReadDirSimpleResult { entries, end })
    }

    async fn symlink(
        &self,
        _dirid: fileid3,
        _linkname: &filename3,
        _symlink: &nfspath3,
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readlink(&self, _id: fileid3) -> Result<nfspath3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_INVAL)
    }

    /// A read-only volume: no symbolic links, no setting times, reads of up to [`MAX_READ`].
    async fn fsinfo(&self, root_fileid: fileid3) -> Result<fsinfo3, nfsstat3> {
        let obj_attributes = match self.getattr(root_fileid).await {
            Ok(a) => post_op_attr::attributes(a),
            Err(_) => post_op_attr::Void,
        };
        Ok(fsinfo3 {
            obj_attributes,
            rtmax: MAX_READ,
            rtpref: MAX_READ,
            rtmult: 4096,
            wtmax: 0,
            wtpref: 0,
            wtmult: 4096,
            dtpref: 64 * 1024,
            maxfilesize: u64::MAX,
            time_delta: nfstime3 { seconds: 0, nseconds: 1 },
            properties: proto::nfs::FSF_HOMOGENEOUS,
        })
    }

    /// The volume's size as `statfs` gives it on Linux: the images' total, all of it used.
    async fn fsstat(&self, _root_fileid: fileid3) -> Result<FsStat, nfsstat3> {
        let s = self.volume.statfs();
        Ok(FsStat {
            tbytes: s.blocks.saturating_mul(u64::from(s.frsize)),
            fbytes: 0,
            abytes: 0,
            tfiles: s.files,
            ffiles: 0,
            afiles: 0,
            invarsec: 0,
        })
    }

    async fn id_to_fh(&self, id: fileid3) -> nfs_fh3 {
        // A number whose generation cannot be read now gets 0: a handle that a later check
        // finds stale, unless it is a folder's.
        let generation = self.generation(id).await.unwrap_or(0);
        nfs_fh3 { data: Handle { volume: self.volume_id, ino: id, generation }.encode() }
    }

    async fn fh_to_id(&self, fh: &nfs_fh3) -> Result<fileid3, nfsstat3> {
        let h = Handle::decode(&fh.data).ok_or(nfsstat3::NFS3ERR_BADHANDLE)?;
        // Another SOURCE's handle: its files are not here.
        if h.volume != self.volume_id {
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        if self.generation(h.ino).await? != h.generation {
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        Ok(h.ino)
    }

    /// The volume ID: the same on every run over the same SOURCE, so that a client's cookie
    /// verifiers stay valid across a restart.
    fn serverid(&self) -> cookieverf3 {
        self.volume_id.to_be_bytes()
    }

    fn permits(&self, auth: Option<&auth_unix>) -> bool {
        permitted(self.callers, auth)
    }
}
