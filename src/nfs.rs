// SPDX-License-Identifier: GPL-3.0-or-later

//! A front end that serves the [`Volume`] over NFSv3 from the local machine (#420, spike):
//! the macOS front end that needs no kernel extension, no FUSE-T or macFUSE, and no
//! third-party driver. The Mac's own NFS client mounts it from `localhost`, as FUSE-T's NFS
//! backend and `rclone nfsmount` do.
//!
//! *Spike:* it answers every read-only request through [`Volume`], with the protocol from the
//! `nfsserve` crate. What is still missing is listed in #420.
//!
//! - File handles are the inode numbers (with the server's start time, so a handle from an
//!   earlier run is stale, as nfsserve makes them).
//! - NFS has no open or close: a file is opened through the core on its first read and kept
//!   open while it is being read; handles idle for [`IDLE`] are closed.
//! - A read at or past the end answers no bytes with `eof` set: NFS's own end of file, so the
//!   empty-read problem of FSKit and macFUSE (#22) does not arise.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nfsserve::nfs::{
    fattr3, fileid3, filename3, fsinfo3, ftype3, nfspath3, nfsstat3, nfstime3, post_op_attr, sattr3,
    specdata3,
};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};
use tokio::sync::oneshot;

use crate::tree::ROOT;
use crate::vfs::{Attr, DataReply, Error, Kind, Listing, ListingPlus, Lookup, MAX_READ, Reply, Volume};
use crate::watchdog::Fail;

/// How long a file read through NFS stays open in the core after its last read.
pub const IDLE: Duration = Duration::from_secs(60);

/// A reply that hands the core's answer to the waiting NFS request. Dropped unanswered (a job
/// that never ran), the receiver sees the sender gone and answers `EIO`.
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
        // NFSv3 has no "try again later" a client acts on; JUKEBOX is the nearest.
        Error::NoMemory => nfsstat3::NFS3ERR_JUKEBOX,
        Error::Io | Error::Os(_) => nfsstat3::NFS3ERR_IO,
    }
}

fn time(t: SystemTime) -> nfstime3 {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    // NFSv3 times are 32-bit seconds: clipped, not wrapped.
    nfstime3 { seconds: u32::try_from(d.as_secs()).unwrap_or(u32::MAX), nseconds: d.subsec_nanos() }
}

fn fattr(a: &Attr) -> fattr3 {
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
        fsid: 0,
        fileid: a.ino,
        atime: time(a.atime),
        mtime: time(a.mtime),
        ctime: time(a.ctime),
    }
}

/// An open file: the core's handle, and when it was last read.
#[derive(Debug, Clone, Copy)]
struct Open {
    fh: u64,
    used: Instant,
}

/// The NFS front end on a [`Volume`]. Clones share the volume and the open files.
#[derive(Debug, Clone)]
pub struct NfsFront {
    volume: Arc<Volume>,
    open: Arc<Mutex<HashMap<u64, Open>>>,
}

impl NfsFront {
    pub fn new(volume: Arc<Volume>) -> NfsFront {
        NfsFront { volume, open: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// Closes the files not read for [`IDLE`]. The caller runs it now and then.
    pub fn close_idle(&self) {
        let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        open.retain(|_, o| {
            let keep = now.duration_since(o.used) < IDLE;
            if !keep {
                self.volume.release(o.fh);
            }
            keep
        });
    }

    /// The core's handle on `ino`, opening it on first use.
    async fn handle(&self, ino: u64) -> Result<u64, nfsstat3> {
        if let Some(o) = self.open.lock().unwrap_or_else(|p| p.into_inner()).get_mut(&ino) {
            o.used = Instant::now();
            return Ok(o.fh);
        }
        let v = self.volume.clone();
        let fh = ask(move |r| v.open(ino, r)).await?;
        let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
        // Two first reads at once: keep one handle, close the other.
        if let Some(o) = open.get(&ino) {
            self.volume.release(fh);
            return Ok(o.fh);
        }
        open.insert(ino, Open { fh, used: Instant::now() });
        Ok(fh)
    }

    async fn attr(&self, ino: u64) -> Result<Attr, nfsstat3> {
        let fh = self.open.lock().unwrap_or_else(|p| p.into_inner()).get(&ino).map(|o| o.fh);
        let v = self.volume.clone();
        Ok(ask(move |r| v.getattr(ino, fh, r)).await?.1)
    }

    async fn listing(&self, dir: u64) -> Result<Listing, nfsstat3> {
        let v = self.volume.clone();
        ask(move |r| v.readdir(dir, r)).await
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
        Ok(fattr(&self.attr(id).await?))
    }

    async fn setattr(&self, _id: fileid3, _setattr: sattr3) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn read(&self, id: fileid3, offset: u64, count: u32) -> Result<(Vec<u8>, bool), nfsstat3> {
        let fh = self.handle(id).await?;
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

    /// The entries after the one numbered `start_after` (0: from the first), with their
    /// attributes, without `.` and `..`. nfsserve uses inode numbers as the cookies.
    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        // Position 2 is the first entry after `.` and `..`.
        let mut from = 2;
        if start_after != 0 {
            let l = self.listing(dirid).await?;
            match l.entries(2).find(|(_, ino, ..)| *ino == start_after) {
                Some((next, ..)) => from = next,
                None => return Err(nfsstat3::NFS3ERR_BAD_COOKIE),
            }
        }
        let v = self.volume.clone();
        let plus: ListingPlus = ask(move |r| v.readdir_plus(dirid, from, r)).await?;
        let end = plus.len() <= max_entries;
        let entries = plus
            .into_iter()
            .take(max_entries)
            .map(|(_, name, e)| DirEntry {
                fileid: e.attr.ino,
                name: name.as_bytes().into(),
                attr: fattr(&e.attr),
            })
            .collect();
        Ok(ReadDirResult { entries, end })
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

    /// nfsserve's defaults, but a read-only volume: no symbolic links, no setting times, and
    /// reads of up to [`MAX_READ`].
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
            properties: nfsserve::nfs::FSF_HOMOGENEOUS,
        })
    }
}
