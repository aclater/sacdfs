// SPDX-License-Identifier: GPL-3.0-or-later

//! The WinFsp file system over the core (#419).
//!
//! WinFsp asks by path; [`crate::paths`] turns paths into inodes from the folders' listings,
//! kept per folder. Every core request is answered through a reply the calling WinFsp thread
//! waits for: WinFsp's callbacks answer before they return (docs/design.md, "Threads"). A read
//! is copied once, from the core's buffer straight into WinFsp's.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sacdfs::vfs::{self, Attr, DataReply, EntryAttr, Kind, Reply, Volume};
use sacdfs::watchdog::Fail;

use winfsp::filesystem::{
    DirBuffer, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo, VolumeInfo,
    WideNameInfo,
};
use winfsp::notify::{Notifier, NotifyInfo, NotifyingFileSystemContext};
use winfsp::{FspError, U16CStr};

use crate::ahead::Ahead;
use crate::paths::{self, ChangeKind, Lister, Missing, Paths};

// NTSTATUS values the file system answers with.
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034_u32 as i32;
const STATUS_OBJECT_PATH_NOT_FOUND: i32 = 0xC000_003A_u32 as i32;
const STATUS_INSUFFICIENT_RESOURCES: i32 = 0xC000_009A_u32 as i32;
const STATUS_NOT_A_DIRECTORY: i32 = 0xC000_0103_u32 as i32;
const STATUS_FILE_IS_A_DIRECTORY: i32 = 0xC000_00BA_u32 as i32;
const STATUS_MEDIA_WRITE_PROTECTED: i32 = 0xC000_00A2_u32 as i32;
const STATUS_ACCESS_DENIED: i32 = 0xC000_0022_u32 as i32;
const STATUS_FILE_INVALID: i32 = 0xC000_0098_u32 as i32;
const STATUS_INVALID_HANDLE: i32 = 0xC000_0008_u32 as i32;
const STATUS_UNEXPECTED_IO_ERROR: i32 = 0xC000_00E9_u32 as i32;
const STATUS_END_OF_FILE: i32 = 0xC000_0011_u32 as i32;
const STATUS_TOO_MANY_OPENED_FILES: i32 = 0xC000_011F_u32 as i32;

/// Files larger than this are read past Windows's cache manager (see `open`).
const NO_DATA_CACHE_ABOVE: u64 = 64 << 20;

// CreateOptions bits.
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;

// Access rights that read a file's data.
const FILE_READ_DATA: u32 = 0x0000_0001;
const FILE_EXECUTE: u32 = 0x0000_0020;

// File attributes.
const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;

// Change notification: filters and actions (FILE_NOTIFY_CHANGE_*, FILE_ACTION_*).
const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 0x0001;
const FILE_NOTIFY_CHANGE_DIR_NAME: u32 = 0x0002;
const FILE_NOTIFY_CHANGE_SIZE: u32 = 0x0008;
const FILE_NOTIFY_CHANGE_LAST_WRITE: u32 = 0x0010;
const FILE_ACTION_ADDED: u32 = 1;
const FILE_ACTION_REMOVED: u32 = 2;
const FILE_ACTION_MODIFIED: u32 = 3;

fn nt(status: i32) -> FspError {
    FspError::NTSTATUS(status)
}

/// The NTSTATUS for a core error.
pub fn status(e: vfs::Error) -> FspError {
    match e {
        vfs::Error::NotFound => nt(STATUS_OBJECT_NAME_NOT_FOUND),
        vfs::Error::NoMemory => nt(STATUS_INSUFFICIENT_RESOURCES),
        vfs::Error::NotDir => nt(STATUS_NOT_A_DIRECTORY),
        vfs::Error::IsDir => nt(STATUS_FILE_IS_A_DIRECTORY),
        vfs::Error::ReadOnly => nt(STATUS_MEDIA_WRITE_PROTECTED),
        vfs::Error::Access => nt(STATUS_ACCESS_DENIED),
        vfs::Error::Stale => nt(STATUS_FILE_INVALID),
        vfs::Error::BadHandle => nt(STATUS_INVALID_HANDLE),
        vfs::Error::Io => nt(STATUS_UNEXPECTED_IO_ERROR),
        // The sidecar cap: the core's own `Os` code, an errno value.
        vfs::Error::Os(libc::EMFILE) => nt(STATUS_TOO_MANY_OPENED_FILES),
        // A source file's own error: on Windows a Win32 error code.
        vfs::Error::Os(n) => FspError::WIN32(n as u32),
    }
}

fn missing(m: Missing) -> FspError {
    match m {
        Missing::Name => nt(STATUS_OBJECT_NAME_NOT_FOUND),
        Missing::Path => nt(STATUS_OBJECT_PATH_NOT_FOUND),
        Missing::Core(e) => status(e),
    }
}

/// A reply the calling WinFsp thread waits for.
struct Wait<T>(SyncSender<Result<T, vfs::Error>>);

impl<T: Send + 'static> Fail for Wait<T> {
    fn fail(self, err: vfs::Error) {
        let _ = self.0.send(Err(err));
    }
}

impl<T: Send + 'static> Reply<T> for Wait<T> {
    fn reply(self, r: Result<T, vfs::Error>) {
        let _ = self.0.send(r);
    }
}

/// Runs a core request and waits for its answer. A reply dropped unanswered (its job never
/// ran) is `Io`.
fn call<T: Send + 'static>(f: impl FnOnce(Wait<T>)) -> Result<T, vfs::Error> {
    let (tx, rx) = sync_channel(1);
    f(Wait(tx));
    rx.recv().unwrap_or(Err(vfs::Error::Io))
}

/// A read's reply: copies the bytes into the waiting thread's buffer, then says how many.
///
/// The buffer is WinFsp's, borrowed by [`WinFs::read`] for as long as it waits on the channel.
/// It waits until this reply sends, or is dropped unanswered; the bytes are written before the
/// send, and only by `data`, which takes the reply by value: so no write comes after the wait
/// ends.
struct Fill {
    buf: *mut u8,
    len: usize,
    done: SyncSender<Result<usize, vfs::Error>>,
}

// SAFETY: the pointer is written only as described on [`Fill`], while its owner waits.
unsafe impl Send for Fill {}

impl Fail for Fill {
    fn fail(self, err: vfs::Error) {
        let _ = self.done.send(Err(err));
    }
}

impl DataReply for Fill {
    fn data(self, data: &[u8]) {
        let n = data.len().min(self.len);
        // SAFETY: `buf` holds `len` bytes and is borrowed by the waiting thread (see `Fill`).
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.buf, n) };
        let _ = self.done.send(Ok(n));
    }
}

/// 100-nanosecond intervals since 1601, from a time.
fn filetime(t: SystemTime) -> u64 {
    const EPOCH_1601: u64 = 116_444_736_000_000_000;
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => EPOCH_1601 + d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos()) / 100,
        Err(_) => EPOCH_1601,
    }
}

fn attributes(kind: Kind) -> u32 {
    match kind {
        Kind::Dir => FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_READONLY,
        Kind::File => FILE_ATTRIBUTE_READONLY,
    }
}

fn fill(info: &mut FileInfo, a: &Attr) {
    info.file_attributes = attributes(a.kind);
    info.reparse_tag = 0;
    info.file_size = a.size;
    info.allocation_size = a.size.div_ceil(4096) * 4096;
    info.creation_time = filetime(a.crtime);
    info.last_access_time = filetime(a.atime);
    info.last_write_time = filetime(a.mtime);
    info.change_time = filetime(a.ctime);
    info.index_number = a.ino;
    info.hard_links = 0;
    info.ea_size = 0;
}

/// The core, as [`Paths`] asks it.
pub struct Core {
    volume: Arc<Volume>,
}

/// The core's handle of an open file, released when the last user of it is done: the open file
/// and any read still running on it.
struct CoreHandle {
    fh: u64,
    volume: Arc<Volume>,
}

impl Drop for CoreHandle {
    fn drop(&mut self) {
        self.volume.release(self.fh);
    }
}

impl std::fmt::Debug for CoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CoreHandle({})", self.fh)
    }
}

impl Lister for Core {
    fn list(&self, ino: u64) -> Result<Vec<(String, EntryAttr)>, vfs::Error> {
        let listing = call(|r| self.volume.readdir_plus(ino, 0, r))?;
        Ok(listing.into_iter().map(|(_, name, entry)| (name.to_string_lossy().into_owned(), entry)).collect())
    }

    fn attr(&self, ino: u64) -> Result<Attr, vfs::Error> {
        call(|r| self.volume.getattr(ino, None, r)).map(|(_, a)| a)
    }

    fn keep_for(&self, ino: u64) -> Option<Duration> {
        self.volume.tree().live_ttl(ino)
    }
}

/// An open file or folder.
#[derive(Debug)]
pub struct Open {
    attr: Attr,
    /// The path in stored case, for a folder's listing.
    path: String,
    /// The core's handle, for a file: opened with the file when it is opened to read its data,
    /// else at its first read. Most opens (Explorer's, for attributes) never read. Let go of at
    /// cleanup (see [`WinFs::cleanup`]).
    fh: Mutex<Option<Arc<CoreHandle>>>,
    /// The program closed its last handle (cleanup); Windows may still read through this open.
    cleaned: AtomicBool,
    /// Read-ahead for small sequential reads.
    ahead: Mutex<Ahead>,
    dir: DirBuffer,
}

/// The file system.
pub struct WinFs {
    paths: Paths<Core>,
    security: Vec<u8>,
    /// Reads answered, for the log at the end.
    reads: Arc<Reads>,
}

/// Reads answered and bytes read.
#[derive(Debug, Default)]
pub struct Reads {
    count: AtomicU64,
    bytes: AtomicU64,
    /// Requests to the core.
    core: AtomicU64,
}

impl Reads {
    /// Reads answered, bytes read, and read requests to the core so far.
    pub fn get(&self) -> (u64, u64, u64) {
        (
            self.count.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
            self.core.load(Ordering::Relaxed),
        )
    }
}

impl WinFs {
    pub fn new(volume: Volume, security: Vec<u8>) -> WinFs {
        WinFs { paths: Paths::new(Core { volume: Arc::new(volume) }), security, reads: Arc::default() }
    }

    /// The count of reads answered.
    pub fn reads(&self) -> Arc<Reads> {
        self.reads.clone()
    }

    fn volume(&self) -> &Volume {
        &self.paths.lister().volume
    }

    fn copy_security(&self, out: Option<&mut [c_void]>) {
        if let Some(out) = out
            && out.len() >= self.security.len()
        {
            // SAFETY: `out` holds at least `security.len()` bytes; the regions don't overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.security.as_ptr(),
                    out.as_mut_ptr().cast::<u8>(),
                    self.security.len(),
                );
            }
        }
    }

    /// Reads `buf.len()` bytes at `offset` from the core (fewer only at the end of the file),
    /// straight into `buf`.
    fn core_read(&self, ino: u64, fh: u64, offset: u64, buf: &mut [u8]) -> winfsp::Result<usize> {
        let mut got = 0;
        while got < buf.len() {
            let n = (buf.len() - got).min(vfs::MAX_READ as usize);
            let (tx, rx) = sync_channel(1);
            let reply = Fill { buf: buf[got..].as_mut_ptr(), len: n, done: tx };
            self.reads.core.fetch_add(1, Ordering::Relaxed);
            self.volume().read(ino, fh, offset + got as u64, n as u32, reply);
            let read = rx.recv().unwrap_or(Err(vfs::Error::Io)).map_err(status)?;
            if read == 0 {
                break;
            }
            got += read;
        }
        Ok(got)
    }

    /// Opens file `ino` in the core.
    fn core_open(&self, ino: u64) -> winfsp::Result<Arc<CoreHandle>> {
        let fh = call(|r| self.volume().open(ino, r)).map_err(status)?;
        Ok(Arc::new(CoreHandle { fh, volume: self.paths.lister().volume.clone() }))
    }

    /// The core's handle of `open`, opening it now if it isn't yet. After cleanup (Windows's
    /// cache manager reading a file its program closed) the handle is for this read only.
    fn handle(&self, open: &Open) -> winfsp::Result<Arc<CoreHandle>> {
        let mut fh = open.fh.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(h) = &*fh {
            return Ok(h.clone());
        }
        let new = self.core_open(open.attr.ino)?;
        if !open.cleaned.load(Ordering::Acquire) {
            *fh = Some(new.clone());
        }
        Ok(new)
    }
}

impl FileSystemContext for WinFs {
    type FileContext = Open;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let found = self.paths.resolve(&file_name.to_string_lossy()).map_err(missing)?;
        self.copy_security(security_descriptor);
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: self.security.len() as u64,
            attributes: attributes(found.attr.kind),
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let found = self.paths.resolve(&file_name.to_string_lossy()).map_err(missing)?;
        match found.attr.kind {
            Kind::Dir if create_options & FILE_NON_DIRECTORY_FILE != 0 => {
                return Err(nt(STATUS_FILE_IS_A_DIRECTORY));
            }
            Kind::File if create_options & FILE_DIRECTORY_FILE != 0 => {
                return Err(nt(STATUS_NOT_A_DIRECTORY));
            }
            _ => {}
        }
        let fh = match found.attr.kind {
            // Opened now when the data is wanted, so that an image that can't be read fails
            // the open, as on Linux.
            Kind::File if granted_access & (FILE_READ_DATA | FILE_EXECUTE) != 0 => {
                Some(self.core_open(found.attr.ino)?)
            }
            _ => None,
        };
        // Large files aren't kept by Windows's cache manager: a buffered copy of a 5 GB track
        // ran at 11 MB/s through it against 58 MB/s without (Windows 11 VM, #419), and nobody
        // reads a track that size twice soon. Their attributes are still cached.
        if found.attr.kind == Kind::File && found.attr.size > NO_DATA_CACHE_ABOVE {
            // SAFETY: called while answering this open (WinFsp's operation context is set).
            unsafe {
                self.with_operation_response(|rsp| rsp.Rsp.Create.Opened.set_DisableCache(1));
            }
        }
        fill(file_info.as_mut(), &found.attr);
        let normal: Vec<u16> = found.path.encode_utf16().collect();
        file_info.set_normalized_name(&normal, None);
        Ok(Open {
            attr: found.attr,
            path: found.path,
            fh: Mutex::new(fh),
            cleaned: AtomicBool::new(false),
            ahead: Mutex::default(),
            dir: DirBuffer::new(),
        })
    }

    /// Nothing is created: the volume is read-only.
    #[allow(clippy::too_many_arguments)]
    fn create(
        &self,
        _file_name: &U16CStr,
        _create_options: u32,
        _granted_access: u32,
        _file_attributes: u32,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        _file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        Err(nt(STATUS_MEDIA_WRITE_PROTECTED))
    }

    /// The program closed its last handle of the file: its read-ahead window and its core
    /// handle go now, not at the close, which Windows may send much later for a file it caches.
    /// *Why:* the core's handle of a cover keeps the source file open, and Windows refuses to
    /// rename or delete a folder while a file inside it is open.
    fn cleanup(&self, context: &Self::FileContext, _file_name: Option<&U16CStr>, _flags: u32) {
        context.cleaned.store(true, Ordering::Release);
        context.fh.lock().unwrap_or_else(|p| p.into_inner()).take();
        context.ahead.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    fn close(&self, context: Self::FileContext) {
        drop(context);
    }

    fn get_file_info(&self, context: &Self::FileContext, file_info: &mut FileInfo) -> winfsp::Result<()> {
        fill(file_info, &context.attr);
        Ok(())
    }

    fn get_security(
        &self,
        _context: &Self::FileContext,
        security_descriptor: Option<&mut [c_void]>,
    ) -> winfsp::Result<u64> {
        self.copy_security(security_descriptor);
        Ok(self.security.len() as u64)
    }

    fn read_directory(
        &self,
        context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: DirMarker<'_>,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        if context.attr.kind != Kind::Dir {
            return Err(nt(STATUS_NOT_A_DIRECTORY));
        }
        if let Ok(lock) = context.dir.acquire(marker.is_none(), None) {
            let folder = self.paths.folder(context.attr.ino, &context.path, true).map_err(status)?;
            if context.path != "\\" {
                for dot in [".", ".."] {
                    let mut di: DirInfo<255> = DirInfo::new();
                    di.set_name(dot)?;
                    fill(di.file_info_mut(), &context.attr);
                    lock.write(&mut di)?;
                }
            }
            for e in &folder.entries {
                let mut di: DirInfo<255> = DirInfo::new();
                di.set_name(&e.name)?;
                fill(di.file_info_mut(), &e.attr);
                lock.write(&mut di)?;
            }
        }
        Ok(context.dir.read(marker, buffer))
    }

    fn get_dir_info_by_name(
        &self,
        context: &Self::FileContext,
        file_name: &U16CStr,
        out_dir_info: &mut DirInfo,
    ) -> winfsp::Result<()> {
        let folder = self.paths.folder(context.attr.ino, &context.path, true).map_err(status)?;
        let child = folder.get(&file_name.to_string_lossy()).ok_or(nt(STATUS_OBJECT_NAME_NOT_FOUND))?;
        out_dir_info.set_name(&child.name)?;
        fill(out_dir_info.file_info_mut(), &child.attr);
        Ok(())
    }

    fn read(&self, context: &Self::FileContext, buffer: &mut [u8], offset: u64) -> winfsp::Result<u32> {
        if context.attr.kind != Kind::File {
            return Err(nt(STATUS_FILE_IS_A_DIRECTORY));
        }
        let size = context.attr.size;
        // End of file is a status, never an empty read (#22).
        if offset >= size {
            return Err(nt(STATUS_END_OF_FILE));
        }
        let handle = self.handle(context)?;
        let fh = handle.fh;
        let want = buffer.len().min((size - offset) as usize);
        let buffer = &mut buffer[..want];
        // Small reads in sequence: from a window read ahead (see `crate::ahead`).
        let mut ahead = context.ahead.lock().unwrap_or_else(|p| p.into_inner());
        let mut how = "window";
        let got = match ahead.serve(offset, buffer) {
            Some(n) => {
                drop(ahead);
                n
            }
            None => match ahead.plan(offset, want, size) {
                Some(window) => {
                    how = "new window";
                    let mut data = vec![0u8; window];
                    match self.core_read(context.attr.ino, fh, offset, &mut data) {
                        Ok(n) => {
                            data.truncate(n);
                            let used = n.min(want);
                            buffer[..used].copy_from_slice(&data[..used]);
                            ahead.fill(offset, data, used, window);
                            drop(ahead);
                            used
                        }
                        Err(e) => {
                            ahead.cancel(window);
                            return Err(e);
                        }
                    }
                }
                None => {
                    how = "core";
                    drop(ahead);
                    self.core_read(context.attr.ino, fh, offset, buffer)?
                }
            },
        };
        if offset + got as u64 >= size {
            context.ahead.lock().unwrap_or_else(|p| p.into_inner()).clear();
        }
        log::trace!("read inode {} at {offset}, {want} bytes: {got} ({how})", context.attr.ino);
        if got == 0 {
            return Err(nt(STATUS_END_OF_FILE));
        }
        self.reads.count.fetch_add(1, Ordering::Relaxed);
        self.reads.bytes.fetch_add(got as u64, Ordering::Relaxed);
        Ok(got as u32)
    }

    fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> winfsp::Result<()> {
        let s = self.volume().statfs();
        out_volume_info.total_size = s.blocks.saturating_mul(u64::from(s.frsize));
        out_volume_info.free_size = 0;
        out_volume_info.set_volume_label("sacdfs");
        Ok(())
    }

    fn dispatcher_stopped(&self, normally: bool) {
        if !normally {
            log::warn!("the WinFsp dispatcher stopped: the volume was unmounted from outside");
        }
    }
}

/// Windows is told what changed in the folders it listed, at each tick of WinFsp's timer.
impl NotifyingFileSystemContext<Vec<paths::Change>> for WinFs {
    fn should_notify(&self) -> Option<Vec<paths::Change>> {
        self.paths.refresh();
        let changes = self.paths.take_changes();
        (!changes.is_empty()).then_some(changes)
    }

    fn notify(&self, changes: Vec<paths::Change>, notifier: &Notifier) {
        for c in changes {
            let mut info: NotifyInfo<1024> = NotifyInfo::new();
            if info.set_name(&c.path).is_err() {
                continue;
            }
            info.filter = match (c.kind, c.what) {
                (Kind::Dir, ChangeKind::Modified) => FILE_NOTIFY_CHANGE_LAST_WRITE,
                (Kind::Dir, _) => FILE_NOTIFY_CHANGE_DIR_NAME,
                (Kind::File, ChangeKind::Modified) => FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE,
                (Kind::File, _) => FILE_NOTIFY_CHANGE_FILE_NAME,
            };
            info.action = match c.what {
                ChangeKind::Added => FILE_ACTION_ADDED,
                ChangeKind::Removed => FILE_ACTION_REMOVED,
                ChangeKind::Modified => FILE_ACTION_MODIFIED,
            };
            log::debug!("change notification: {:?} {}", c.what, c.path);
            notifier.notify(&info);
        }
    }
}
