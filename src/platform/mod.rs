// SPDX-License-Identifier: GPL-3.0-or-later

//! The calls of the core that differ between operating systems (#419, #420), in one place:
//! the rest of the core calls these functions and has no `cfg` of its own for them.
//!
//! Each function has one meaning on every system. `unix.rs` is the Linux code the core called
//! before, unchanged, with macOS's differences inside it; `windows.rs` is Windows's. Where a
//! system has no equivalent yet, the function says what it does instead (a fallback), never
//! panics, and the core's behaviour on Linux does not depend on it. See docs/design.md, "The
//! core and its front ends".

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::ops::BitOr;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// Reads into `buf` at `offset` of `file`, without moving a position other readers of the file
/// rely on: one call, which may read less.
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    imp::read_at(file, buf, offset)
}

/// Fills `buf` from `offset` of `file`; `UnexpectedEof` if the file ends first.
pub fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    imp::read_exact_at(file, buf, offset)
}

/// A file's modification time, as seconds and nanoseconds since the epoch.
pub fn mtime(m: &Metadata) -> (i64, i64) {
    imp::mtime(m)
}

/// A file's identity on its system: its device (volume) and file numbers. Two paths with the
/// same identity are the same file. *Fallback:* (0, 0) where the system's numbers aren't read
/// yet, so that only the size and time tell a replaced file.
pub fn file_id(m: &Metadata) -> (u64, u64) {
    imp::file_id(m)
}

/// The error of a file that changed since it was listed (`ESTALE` on Unix).
pub fn stale() -> io::Error {
    imp::stale()
}

/// Whether `e` is [`stale`].
pub fn is_stale(e: &io::Error) -> bool {
    imp::is_stale(e)
}

/// The error of a path that leaves SOURCE (`EXDEV` on Unix).
pub fn cross_device() -> io::Error {
    imp::cross_device()
}

/// Whether `e` is [`cross_device`].
pub fn is_cross_device(e: &io::Error) -> bool {
    imp::is_cross_device(e)
}

/// The OS string of path bytes `b` that [`std::ffi::OsStr::as_encoded_bytes`] gave: the same
/// bytes on Unix; on Windows read as UTF-8, any invalid sequence replaced.
pub fn os_from_bytes(b: Vec<u8>) -> OsString {
    imp::os_from_bytes(b)
}

/// How an open is hardened, as [`set_open_flags`] applies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenFlags(u8);

impl OpenFlags {
    /// A final symbolic link (a reparse point on Windows) is not followed: on Unix the open
    /// fails (`ELOOP`); on Windows the link itself is opened, and is no regular file.
    pub const NO_FOLLOW: OpenFlags = OpenFlags(1);
    /// The open doesn't wait for a writer or a device (`O_NONBLOCK`); nothing on Windows.
    pub const NON_BLOCK: OpenFlags = OpenFlags(2);
    /// A terminal opened doesn't become the controlling one (`O_NOCTTY`); nothing on Windows.
    pub const NO_CTTY: OpenFlags = OpenFlags(4);

    fn has(self, f: OpenFlags) -> bool {
        self.0 & f.0 != 0
    }
}

impl BitOr for OpenFlags {
    type Output = OpenFlags;
    fn bitor(self, o: OpenFlags) -> OpenFlags {
        OpenFlags(self.0 | o.0)
    }
}

/// Sets `flags` on `o`, replacing any flags set before.
pub fn set_open_flags(o: &mut OpenOptions, flags: OpenFlags) -> &mut OpenOptions {
    imp::set_open_flags(o, flags)
}

/// Whether `e` is the open of a link refused by [`OpenFlags::NO_FOLLOW`] (`ELOOP`). On Windows
/// the link is opened instead, and the caller's check for a regular file refuses it.
pub fn is_link_refused(e: &io::Error) -> bool {
    imp::is_link_refused(e)
}

/// Sets the permission bits a new file is created with (Unix); nothing on Windows, where the
/// folder's access list applies.
pub fn set_create_mode(o: &mut OpenOptions, mode: u32) -> &mut OpenOptions {
    imp::set_create_mode(o, mode)
}

/// Makes folder `dir` private to its owner (mode 0700 on Unix); nothing on Windows, where a
/// folder under the user's profile is private already.
pub fn make_private_dir(dir: &Path) -> io::Result<()> {
    imp::make_private_dir(dir)
}

/// Opens the file at `path` for reading, refusing anything but a regular file, without
/// blocking on the way: on Unix a FIFO put in place of the file would block the open until a
/// writer came, so it opens with `O_NONBLOCK`, checks the type and clears the flag again.
pub fn open_regular(path: &Path) -> io::Result<File> {
    imp::open_regular(path)
}

/// The start of the first hole of `file` at or after `at` (`SEEK_HOLE`). `None` on any error,
/// including none past `at` (`ENXIO`) and no support (`EINVAL`): either way, nothing more is
/// found. *Fallback:* always `None` on Windows, where `FSCTL_QUERY_ALLOCATED_RANGES` is the
/// equivalent.
pub fn next_hole(file: &File, at: u64) -> Option<u64> {
    imp::next_hole(file, at)
}

/// The start of the first data of `file` at or after `at` (`SEEK_DATA`): `Ok(None)` if there
/// is none past `at` (`ENXIO`).
pub fn next_data(file: &File, at: u64) -> io::Result<Option<u64>> {
    imp::next_data(file, at)
}

/// A folder, open, that names are resolved under one at a time, so that nothing outside it is
/// reached through a link (the sidecars' confinement to SOURCE). On Unix a descriptor
/// (`O_PATH` on Linux, read-only on macOS, which has no `O_PATH`); on Windows its path, each
/// folder on the way checked for a reparse point, which isn't race-free like `openat`.
pub use imp::Dir;

/// What [`stat_at`], [`stat_dir`] and [`open_file_at`] tell of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    /// A regular file (not a link, folder, FIFO or device).
    pub regular: bool,
    /// Size in bytes.
    pub size: u64,
    /// 512-byte blocks used.
    pub blocks: u64,
    /// Last access.
    pub atime: SystemTime,
    /// Last modification.
    pub mtime: SystemTime,
    /// Last status change (on Windows, the last modification).
    pub ctime: SystemTime,
}

/// Opens folder `path` as a [`Dir`] (followed if it is a link: the user gave it).
pub fn open_root(path: &Path) -> io::Result<Dir> {
    imp::open_root(path)
}

/// Opens the folder that `names` lead to from `dir`, one at a time, following no link; no
/// names opens `dir` again.
pub fn open_dir_at(dir: &Dir, names: &[&OsStr]) -> io::Result<Dir> {
    imp::open_dir_at(dir, names)
}

/// The attributes of the open folder `dir`.
pub fn stat_dir(dir: &Dir) -> io::Result<Stat> {
    imp::stat_dir(dir)
}

/// The attributes of `name` in `dir`, not followed if a link.
pub fn stat_at(dir: &Dir, name: &OsStr) -> io::Result<Stat> {
    imp::stat_at(dir, name)
}

/// Opens `name` in `dir` for reading, following no link and not blocking on a FIFO or
/// device, with its attributes from the open file. The caller checks [`Stat::regular`].
pub fn open_file_at(dir: &Dir, name: &OsStr) -> io::Result<(File, Stat)> {
    imp::open_file_at(dir, name)
}

/// What the link `name` in `dir` points to.
pub fn read_link_at(dir: &Dir, name: &OsStr) -> io::Result<PathBuf> {
    imp::read_link_at(dir, name)
}

/// The total size of the filesystem holding `path`: (blocks, block size).
pub fn fs_size(path: &Path) -> io::Result<(u64, u32)> {
    imp::fs_size(path)
}

/// Lowers the calling thread's CPU and I/O priority as far as the system allows without
/// privileges, for background work. Best effort: each step that failed, by name.
pub fn lower_thread_priority() -> Vec<(&'static str, io::Error)> {
    imp::lower_thread_priority()
}

/// The name of the process that thread `tid` belongs to (Linux: its thread group's `comm`).
/// *Fallback:* `None` where the system's process table isn't read (macOS and Windows so far),
/// or the thread is unknown.
pub fn process_name(tid: u32) -> Option<String> {
    imp::process_name(tid)
}
