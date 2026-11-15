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

pub fn file_id(_m: &Metadata) -> (u64, u64) {
    (0, 0)
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

pub fn next_hole(_file: &File, _at: u64) -> Option<u64> {
    None
}

pub fn next_data(_file: &File, _at: u64) -> io::Result<Option<u64>> {
    Ok(None)
}

#[derive(Debug)]
pub struct Dir(PathBuf);

fn stat(m: &Metadata) -> Stat {
    let t = |r: io::Result<SystemTime>| r.unwrap_or(SystemTime::UNIX_EPOCH);
    Stat {
        regular: m.is_file() && !m.file_type().is_symlink(),
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
        if m.file_type().is_symlink() || !m.is_dir() {
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

pub fn fs_size(_path: &Path) -> io::Result<(u64, u32)> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

pub fn lower_thread_priority() -> Vec<(&'static str, io::Error)> {
    Vec::new()
}

pub fn process_name(_tid: u32) -> Option<String> {
    None
}
