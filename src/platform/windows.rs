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

/// Gives `dir` a protected access list: full control for its owner (the user who made it,
/// through the OWNER RIGHTS entry) and SYSTEM, inherited by what is made inside, and nothing
/// for anyone else; the entries it inherited from its parent are dropped.
#[allow(unsafe_code)]
pub fn make_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    };
    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)".encode_utf16().chain(Some(0)).collect();
    let path: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: both strings are NUL-terminated and live across the calls; `sd` is allocated by
    // the conversion and freed with LocalFree once applied.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let ok = SetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            sd,
        );
        let err = io::Error::last_os_error();
        LocalFree(sd as HLOCAL);
        if ok == 0 {
            return Err(err);
        }
    }
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

/// *Fallback:* `None`, so that holes are always looked for: the metadata's block count is the
/// size's.
pub fn hole_bound(_file: &File) -> Option<u64> {
    None
}

/// An open folder: a handle that later opens are made relative to (`NtCreateFile` with a root
/// directory), so that a folder renamed or replaced by a link after it was opened can't send a
/// sidecar open elsewhere: the Windows counterpart of `openat`. The path is kept for
/// `read_link_at` and for messages.
#[derive(Debug)]
pub struct Dir {
    handle: File,
    path: PathBuf,
}

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

/// Opens `name`, one plain name, relative to folder `dir`, never following a reparse point
/// (`FILE_OPEN_REPARSE_POINT`: a link or junction is opened itself, and refused by the caller).
/// `options` adds `FILE_DIRECTORY_FILE` or `FILE_NON_DIRECTORY_FILE`.
#[allow(unsafe_code)]
fn open_relative(dir: &File, name: &OsStr, access: u32, options: u32) -> io::Result<File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
    };
    use windows_sys::Win32::Foundation::{
        HANDLE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let wide: Vec<u16> = name.encode_wide().collect();
    // One plain name: a separator, `.` or `..` would leave the folder.
    let plain = !wide.is_empty()
        && name != "."
        && name != ".."
        && !wide.iter().any(|&c| c == u16::from(b'\\') || c == u16::from(b'/') || c == 0);
    let bytes = u16::try_from(wide.len() * 2).ok().filter(|_| plain);
    let Some(bytes) = bytes else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a plain name"));
    };
    let us = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_ptr().cast_mut() };
    let oa = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: dir.as_raw_handle() as HANDLE,
        ObjectName: &us,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: every pointer is to a live local (`wide` outlives the call through `us`); the
    // out handle is written only on success, and then owned by the `File` made from it.
    let status = unsafe {
        let mut iosb: IO_STATUS_BLOCK = std::mem::zeroed();
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE,
            &oa,
            &mut iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: a plain conversion of a status code.
        let code = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    // SAFETY: NtCreateFile succeeded, so `handle` is an open handle nothing else owns.
    Ok(unsafe { File::from_raw_handle(handle as _) })
}

pub fn open_root(path: &Path) -> io::Result<Dir> {
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    let path = path.canonicalize()?;
    // Followed if a link: the user gave this path.
    let handle = OpenOptions::new().read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(&path)?;
    if !handle.metadata()?.is_dir() {
        return Err(not_a_folder());
    }
    Ok(Dir { handle, path })
}

pub fn open_dir_at(dir: &Dir, names: &[&OsStr]) -> io::Result<Dir> {
    use windows_sys::Wdk::Storage::FileSystem::FILE_DIRECTORY_FILE;
    use windows_sys::Win32::Storage::FileSystem::{FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_TRAVERSE};
    let mut at = Dir { handle: dir.handle.try_clone()?, path: dir.path.clone() };
    for name in names {
        let handle = open_relative(
            &at.handle,
            name,
            FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES,
            FILE_DIRECTORY_FILE,
        )
        .map_err(|e| if e.raw_os_error() == Some(267) { not_a_folder() } else { e })?;
        let m = handle.metadata()?;
        if is_reparse_point(&m) || !m.is_dir() {
            return Err(not_a_folder());
        }
        at = Dir { handle, path: at.path.join(name) };
    }
    Ok(at)
}

pub fn stat_dir(dir: &Dir) -> io::Result<Stat> {
    Ok(stat(&dir.handle.metadata()?))
}

pub fn stat_at(dir: &Dir, name: &OsStr) -> io::Result<Stat> {
    use windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES;
    Ok(stat(&open_relative(&dir.handle, name, FILE_READ_ATTRIBUTES, 0)?.metadata()?))
}

pub fn open_file_at(dir: &Dir, name: &OsStr) -> io::Result<(File, Stat)> {
    use windows_sys::Wdk::Storage::FileSystem::FILE_NON_DIRECTORY_FILE;
    use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ;
    let file = open_relative(&dir.handle, name, FILE_GENERIC_READ, FILE_NON_DIRECTORY_FILE)?;
    let st = stat(&file.metadata()?);
    Ok((file, st))
}

/// The target of link `name` in `dir`, read by path: only a name to look up, which is then
/// resolved inside SOURCE one name at a time like any other.
pub fn read_link_at(dir: &Dir, name: &OsStr) -> io::Result<PathBuf> {
    std::fs::read_link(dir.path.join(name))
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
