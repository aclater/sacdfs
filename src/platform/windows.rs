// SPDX-License-Identifier: GPL-3.0-or-later

//! [`super`] on Windows.

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{OpenFlags, Stat};

/// `FILE_FLAG_OPEN_REPARSE_POINT`: a link is opened as itself.
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    // `seek_read` moves the handle's position, but every read in sacdfs names its offset, so
    // readers sharing a handle don't disturb each other.
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

pub fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match read_at(file, buf, offset) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn mtime(m: &Metadata) -> (i64, i64) {
    match m.modified().map(|t| t.duration_since(std::time::UNIX_EPOCH)) {
        Ok(Ok(d)) => (i64::try_from(d.as_secs()).unwrap_or(i64::MAX), i64::from(d.subsec_nanos())),
        _ => (0, 0),
    }
}

#[allow(unsafe_code)]
pub fn file_id(file: &File, _m: &Metadata) -> (u64, u64) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
    // SAFETY: an all-zero BY_HANDLE_FILE_INFORMATION is valid; the handle is open for the
    // call, and the structure is written by it.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0 {
            return (0, 0);
        }
        (
            u64::from(info.dwVolumeSerialNumber),
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        )
    }
}

pub fn file_id_at(path: &Path) -> io::Result<(Metadata, (u64, u64))> {
    // No read or write access: only the attributes, which an open file of another process
    // still allows.
    let file = OpenOptions::new().access_mode(0).open(path)?;
    let m = file.metadata()?;
    let id = file_id(&file, &m);
    Ok((m, id))
}

pub fn cache_home() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).filter(|p| p.is_absolute())
}

pub fn is_transient(e: &io::Error) -> bool {
    use windows_sys::Win32::Foundation::{
        ERROR_IO_DEVICE, ERROR_LOCK_VIOLATION, ERROR_NOT_ENOUGH_MEMORY, ERROR_OUTOFMEMORY,
        ERROR_SHARING_VIOLATION, ERROR_TOO_MANY_OPEN_FILES,
    };
    let code = e.raw_os_error().and_then(|c| u32::try_from(c).ok());
    // A file another program holds open without sharing (a copy in progress) is readable
    // again once it lets go: its folder is listed again later.
    matches!(
        code,
        Some(
            ERROR_TOO_MANY_OPEN_FILES
                | ERROR_NOT_ENOUGH_MEMORY
                | ERROR_OUTOFMEMORY
                | ERROR_IO_DEVICE
                | ERROR_SHARING_VIOLATION
                | ERROR_LOCK_VIOLATION
        )
    ) || matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::OutOfMemory)
}

pub const TOO_MANY_OPEN_FILES: i32 = windows_sys::Win32::Foundation::ERROR_TOO_MANY_OPEN_FILES as i32;

pub const IO_ERROR: i32 = windows_sys::Win32::Foundation::ERROR_IO_DEVICE as i32;

/// Whether `m` is a reparse point of any kind: a link or junction, but also a cloud
/// placeholder, a deduplicated file or a WSL or app-execution link. None is followed or served.
fn is_reparse_point(m: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0
}

pub fn refuse_link(file: &File) -> io::Result<()> {
    if is_reparse_point(&file.metadata()?) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a link (reparse point): not used"));
    }
    Ok(())
}

/// The marker of [`stale`]: Windows has no such error code.
#[derive(Debug)]
struct Stale;

impl std::fmt::Display for Stale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stale file handle")
    }
}

impl std::error::Error for Stale {}

pub fn stale() -> io::Error {
    io::Error::other(Stale)
}

pub fn is_stale(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|i| i.is::<Stale>())
}

pub fn cross_device() -> io::Error {
    io::Error::new(io::ErrorKind::CrossesDevices, "leaves SOURCE")
}

pub fn is_cross_device(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::CrossesDevices
}

pub fn os_from_bytes(b: Vec<u8>) -> OsString {
    String::from_utf8_lossy(&b).into_owned().into()
}

pub fn set_open_flags(o: &mut OpenOptions, flags: OpenFlags) -> &mut OpenOptions {
    o.custom_flags(if flags.has(OpenFlags::NO_FOLLOW) { FILE_FLAG_OPEN_REPARSE_POINT } else { 0 })
}

pub fn is_link_refused(_e: &io::Error) -> bool {
    false
}

pub fn set_create_mode(o: &mut OpenOptions, _mode: u32) -> &mut OpenOptions {
    o
}

pub fn make_private_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

pub fn open_regular(path: &Path) -> io::Result<File> {
    // No FIFOs to block on: a plain open, then the same check.
    let file = File::open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    Ok(file)
}

/// The first allocated range of `file` that ends after `at`, from `FSCTL_QUERY_ALLOCATED_RANGES`
/// over `[at, end of file)`: `Ok(None)` if there is none. A file that isn't sparse is one range.
#[allow(unsafe_code)]
fn allocated_after(file: &File, at: u64) -> io::Result<Option<(u64, u64)>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{FILE_ALLOCATED_RANGE_BUFFER, FSCTL_QUERY_ALLOCATED_RANGES};
    let len = file.metadata()?.len();
    if at >= len {
        return Ok(None);
    }
    let to_i64 = |v: u64| i64::try_from(v).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput));
    let query = FILE_ALLOCATED_RANGE_BUFFER { FileOffset: to_i64(at)?, Length: to_i64(len - at)? };
    // One range is enough: the call fills what fits and fails with ERROR_MORE_DATA past it.
    let mut out = FILE_ALLOCATED_RANGE_BUFFER { FileOffset: 0, Length: 0 };
    let mut got = 0u32;
    let size = std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>() as u32;
    // SAFETY: the handle is open for the call; both buffers are valid for `size` bytes and
    // `got` for a u32.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_QUERY_ALLOCATED_RANGES,
            std::ptr::from_ref(&query).cast(),
            size,
            std::ptr::from_mut(&mut out).cast(),
            size,
            &mut got,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(windows_sys::Win32::Foundation::ERROR_MORE_DATA as i32) {
            return Err(e);
        }
    }
    if got < size {
        return Ok(None);
    }
    let start = u64::try_from(out.FileOffset).unwrap_or(0);
    Ok(Some((start, start.saturating_add(u64::try_from(out.Length).unwrap_or(0)))))
}

pub fn next_hole(file: &File, at: u64) -> Option<u64> {
    let len = file.metadata().ok()?.len();
    match allocated_after(file, at).ok()? {
        // Data from `at` on: the hole starts where it ends (at the end of the file, as
        // `SEEK_HOLE`'s virtual hole there, if nothing follows).
        Some((start, end)) if start <= at => Some(end.min(len)),
        Some(_) => Some(at),
        None => (at < len).then_some(at),
    }
}

pub fn next_data(file: &File, at: u64) -> io::Result<Option<u64>> {
    Ok(allocated_after(file, at)?.map(|(start, _)| start.max(at)))
}

#[derive(Debug)]
pub struct Dir(PathBuf);

fn stat(m: &Metadata) -> Stat {
    let t = |r: io::Result<SystemTime>| r.unwrap_or(SystemTime::UNIX_EPOCH);
    Stat {
        regular: m.is_file() && !m.file_type().is_symlink() && !is_reparse_point(m),
        size: m.len(),
        blocks: m.len().div_ceil(512),
        atime: t(m.accessed()),
        mtime: t(m.modified()),
        ctime: t(m.modified()),
    }
}

fn not_a_folder() -> io::Error {
    io::Error::new(io::ErrorKind::NotADirectory, "not a folder")
}

pub fn open_root(path: &Path) -> io::Result<Dir> {
    let path = path.canonicalize()?;
    if !std::fs::metadata(&path)?.is_dir() {
        return Err(not_a_folder());
    }
    Ok(Dir(path))
}

pub fn open_dir_at(dir: &Dir, names: &[&OsStr]) -> io::Result<Dir> {
    let mut at = dir.0.clone();
    for name in names {
        at.push(name);
        let m = std::fs::symlink_metadata(&at)?;
        if m.file_type().is_symlink() || is_reparse_point(&m) || !m.is_dir() {
            return Err(not_a_folder());
        }
    }
    Ok(Dir(at))
}

pub fn stat_dir(dir: &Dir) -> io::Result<Stat> {
    Ok(stat(&std::fs::symlink_metadata(&dir.0)?))
}

pub fn stat_at(dir: &Dir, name: &OsStr) -> io::Result<Stat> {
    Ok(stat(&std::fs::symlink_metadata(dir.0.join(name))?))
}

pub fn open_file_at(dir: &Dir, name: &OsStr) -> io::Result<(File, Stat)> {
    let file =
        OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(dir.0.join(name))?;
    let st = stat(&file.metadata()?);
    Ok((file, st))
}

pub fn read_link_at(dir: &Dir, name: &OsStr) -> io::Result<PathBuf> {
    std::fs::read_link(dir.0.join(name))
}

pub fn writable(dir: &Path) -> io::Result<()> {
    if std::fs::metadata(dir)?.permissions().readonly() {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(())
}

pub fn set_modified(path: &Path, t: SystemTime) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES};
    // Backup semantics: a folder opens too.
    OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .set_modified(t)
}

pub fn sacdfs_mounts() -> Vec<PathBuf> {
    Vec::new()
}

pub fn fd_dir() -> Option<PathBuf> {
    None
}

#[allow(unsafe_code)]
pub fn fs_size(path: &Path) -> io::Result<(u64, u32)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut total = 0u64;
    // SAFETY: `wide` is NUL-terminated and lives through the call; `total` is a valid u64 to
    // write; the other outputs are not asked for.
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), std::ptr::null_mut(), &mut total, std::ptr::null_mut()) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((total / 4096, 4096))
}

#[allow(unsafe_code)]
pub fn lower_thread_priority() -> Vec<(&'static str, io::Error)> {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };
    // SAFETY: the pseudo-handle of the calling thread, which needs no closing.
    if unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) } == 0 {
        return vec![("background", io::Error::last_os_error())];
    }
    Vec::new()
}

pub fn process_name(_tid: u32) -> Option<String> {
    None
}
