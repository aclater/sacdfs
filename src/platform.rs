// SPDX-License-Identifier: GPL-3.0-or-later

//! The few calls the core makes that differ between Unix and Windows (#419): positional
//! reads, file times as seconds and nanoseconds, the "stale" error, and the thread priority of
//! background work. The Unix branch of each is what the core called before, unchanged.
//!
//! *Spike (#419):* the Windows branches are the simplest that keep the core's behaviour where a
//! mount reads; what each still lacks is listed in #419.

use std::fs::{File, Metadata};
use std::io;

/// Reads into `buf` at `offset` of `file`, without moving a shared position: one call, maybe
/// short.
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        // `seek_read` moves the handle's position, but every read here names its offset, so
        // concurrent readers of one handle don't disturb each other.
        std::os::windows::fs::FileExt::seek_read(file, buf, offset)
    }
}

/// Fills `buf` from `offset` of `file`; `UnexpectedEof` if the file ends first.
#[cfg(unix)]
pub fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

/// Fills `buf` from `offset` of `file`; `UnexpectedEof` if the file ends first.
#[cfg(not(unix))]
pub fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    {
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
}

/// A file's modification time as seconds and nanoseconds since the epoch.
pub fn mtime(m: &Metadata) -> (i64, i64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (m.mtime(), m.mtime_nsec())
    }
    #[cfg(not(unix))]
    {
        match m.modified().map(|t| t.duration_since(std::time::UNIX_EPOCH)) {
            Ok(Ok(d)) => (i64::try_from(d.as_secs()).unwrap_or(i64::MAX), i64::from(d.subsec_nanos())),
            _ => (0, 0),
        }
    }
}

/// The file's device and file numbers, its identity on its volume. *Windows (spike):* 0 and 0,
/// so a file replaced by one of the same size and time isn't noticed; the volume serial
/// number and file index (`GetFileInformationByHandle`) are the equivalent.
pub fn dev_ino(m: &Metadata) -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (m.dev(), m.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        (0, 0)
    }
}

/// The error of a file that changed since it was listed (`ESTALE` on Unix).
pub fn stale() -> io::Error {
    #[cfg(unix)]
    {
        io::Error::from_raw_os_error(libc::ESTALE)
    }
    #[cfg(not(unix))]
    {
        io::Error::other(Stale)
    }
}

/// Whether `e` is [`stale`].
pub fn is_stale(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::ESTALE)
    }
    #[cfg(not(unix))]
    {
        e.get_ref().is_some_and(|i| i.is::<Stale>())
    }
}

/// The marker of [`stale`] where the platform has no such error code.
#[cfg(not(unix))]
#[derive(Debug)]
struct Stale;

#[cfg(not(unix))]
impl std::fmt::Display for Stale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stale file handle")
    }
}

#[cfg(not(unix))]
impl std::error::Error for Stale {}

/// The error of a path that leaves SOURCE (`EXDEV` on Unix).
pub fn cross_device() -> io::Error {
    #[cfg(unix)]
    {
        io::Error::from_raw_os_error(libc::EXDEV)
    }
    #[cfg(not(unix))]
    {
        io::Error::new(io::ErrorKind::CrossesDevices, "leaves SOURCE")
    }
}

/// Whether `e` is [`cross_device`].
pub fn is_cross_device(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::EXDEV)
    }
    #[cfg(not(unix))]
    {
        e.kind() == io::ErrorKind::CrossesDevices
    }
}

/// The path bytes `b` as an OS string: as they are on Unix; as UTF-8 on Windows (spike), where
/// they were written from [`std::ffi::OsStr::as_encoded_bytes`] of a valid name.
pub fn os_from_bytes(b: Vec<u8>) -> std::ffi::OsString {
    #[cfg(unix)]
    {
        std::os::unix::ffi::OsStringExt::from_vec(b)
    }
    #[cfg(not(unix))]
    {
        String::from_utf8_lossy(&b).into_owned().into()
    }
}
