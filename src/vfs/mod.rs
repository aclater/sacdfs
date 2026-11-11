// SPDX-License-Identifier: GPL-3.0-or-later

//! The platform-neutral core that every front end calls (#22): what sacdfs answers, without
//! FUSE's types. See docs/design.md, "The core and its front ends".
//!
//! The Linux FUSE front end is [`crate::fs`]. A front end maps [`Error`] to its platform's
//! codes, and supplies the replies the core answers through ([`crate::watchdog::Fail`]).

mod image_reads;
mod size;
mod workers;

pub use image_reads::HUNG_READS_PER_IMAGE;
pub(crate) use image_reads::ImageReads;
pub(crate) use size::ShareSize;
pub use workers::{MAX_POOL_THREADS, Workers};

/// Why a request failed, for the front end to say in its platform's terms (an errno for
/// FUSE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No such file or folder (`ENOENT`).
    NotFound,
    /// Short of memory for now (`ENOMEM`, #237): not "missing", which a kernel remembers.
    NoMemory,
    /// Not a folder (`ENOTDIR`).
    NotDir,
    /// A folder, where a file was asked for (`EISDIR`).
    IsDir,
    /// The filesystem is read-only (`EROFS`).
    ReadOnly,
    /// Not allowed (`EACCES`).
    Access,
    /// Gone since the client was told of it, to be asked about again (`ESTALE`, #235).
    Stale,
    /// No such open file (`EBADF`).
    BadHandle,
    /// The read failed, or the request took too long (`EIO`).
    Io,
    /// A source file's own error, as the operating system gave it (`raw_os_error`: an errno
    /// on Unix).
    Os(i32),
}
