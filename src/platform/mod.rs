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

/// A file's change time (any write or change of its attributes moves it, and a user can't set
/// it), as seconds and nanoseconds since the epoch; (0, 0) where the system has none (Windows).
pub fn ctime(m: &Metadata) -> (i64, i64) {
    imp::ctime(m)
}

/// Whether the times of open `file` may not show a write by another client: it is on a network
/// or FUSE filesystem (NFS, SMB, FUSE), or the system can't tell. Such a file's TOC is also
/// read again now and then to tell whether it holds the same disc ([`crate::disc::Disc`]).
pub fn unreliable_times(file: &File) -> bool {
    imp::unreliable_times(file)
}

/// The identity of open `file`, whose metadata is `m`: its device (volume) and file numbers.
/// Two files with the same identity are the same file. Unix reads them from `m`; Windows asks
/// the handle for the volume serial number and file index. *Fallback:* (0, 0) if the system
/// can't tell, so that only the size and time tell a replaced file.
pub fn file_id(file: &File, m: &Metadata) -> (u64, u64) {
    imp::file_id(file, m)
}

/// The metadata and identity ([`file_id`]) of the file at `path`, following links: on Unix one
/// `stat`; on Windows an open of the file, without reading it.
pub fn file_id_at(path: &Path) -> io::Result<(Metadata, (u64, u64))> {
    imp::file_id_at(path)
}

/// `text` (a log message) with the Windows verbatim paths in it written as people write them:
/// `\\?\C:\music` as `C:\music`, `\\?\UNC\nas\music` as `\\nas\music` (#419, item 12). For
/// display only: the paths sacdfs opens keep their verbatim form, under which Windows takes
/// every name literally (a trailing dot, a device name, more than 260 characters). Unchanged on
/// other systems, where no path has that prefix.
pub fn plain_text(text: &str) -> std::borrow::Cow<'_, str> {
    imp::plain_text(text)
}

/// The base folder for a per-user cache: `$XDG_CACHE_HOME`, else `~/.cache` on Unix;
/// `%LOCALAPPDATA%` on Windows. Only an absolute path counts.
pub fn cache_home() -> Option<PathBuf> {
    imp::cache_home()
}

/// Whether `e` is a passing condition of the process or the system (out of file descriptors
/// or memory, an I/O error, interrupted; on Windows also a file another program holds open
/// without sharing, as a copy in progress does) rather than about the file itself.
pub fn is_transient(e: &io::Error) -> bool {
    imp::is_transient(e)
}

/// The OS error code of an I/O error of the device (`EIO`; `ERROR_IO_DEVICE` on Windows).
pub const IO_ERROR: i32 = imp::IO_ERROR;

/// Refuses `file`, opened with [`OpenFlags::NO_FOLLOW`], if it is a link: on Unix the open
/// already failed for one; on Windows, where the link itself was opened, its attributes tell.
pub fn refuse_link(file: &File) -> io::Result<()> {
    imp::refuse_link(file)
}

/// The OS error code for "too many open files" (`EMFILE`; `ERROR_TOO_MANY_OPEN_FILES` on
/// Windows), for `vfs::Error::Os`.
pub const TOO_MANY_OPEN_FILES: i32 = imp::TOO_MANY_OPEN_FILES;

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

/// Makes folder `dir` private to its owner: mode 0700 on Unix; on Windows a protected access
/// list for its owner and SYSTEM only, so that an `--index-cache` outside the user's profile is
/// as private as one inside it.
pub fn make_private_dir(dir: &Path) -> io::Result<()> {
    imp::make_private_dir(dir)
}

/// Opens the file at `path` for reading, refusing anything but a regular file, without
/// blocking on the way: on Unix a FIFO put in place of the file would block the open until a
/// writer came, so it opens with `O_NONBLOCK`, checks the type and clears the flag again.
pub fn open_regular(path: &Path) -> io::Result<File> {
    imp::open_regular(path)
}

/// The start of the first hole of `file` at or after `at` (`SEEK_HOLE`; on Windows from
/// `FSCTL_QUERY_ALLOCATED_RANGES`). `None` on any error, including none past `at` (`ENXIO`)
/// and no support (`EINVAL`): either way, nothing more is found.
pub fn next_hole(file: &File, at: u64) -> Option<u64> {
    imp::next_hole(file, at)
}

/// The start of the first data of `file` at or after `at` (`SEEK_DATA`; on Windows from
/// `FSCTL_QUERY_ALLOCATED_RANGES`): `Ok(None)` if there is none past `at` (`ENXIO`).
pub fn next_data(file: &File, at: u64) -> io::Result<Option<u64>> {
    imp::next_data(file, at)
}

/// At most how many bytes of `file` lie in holes, when that is known without asking a file
/// server (#451): on an NFS or SMB share, the file's size less the space the server says it
/// uses (`st_blocks`), from the client's cached attributes. Compression on the server only
/// makes the bound larger. `None` elsewhere: on a local file system `SEEK_HOLE` costs no
/// round trip and also finds preallocated space that was never written, which `st_blocks`
/// counts as used; Windows reports no allocation in its metadata.
pub fn hole_bound(file: &File) -> Option<u64> {
    imp::hole_bound(file)
}

/// A folder, open, that names are resolved under one at a time, so that nothing outside it is
/// reached through a link (the sidecars' confinement to SOURCE). On Unix a descriptor
/// (`O_PATH` on Linux, read-only on macOS, which has no `O_PATH`); on Windows a handle that
/// opens are made relative to, with the folder's path kept for [`read_link_at`], which reads
/// a link by path, and for messages.
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

/// Whether this process may write in folder `dir` (`access(W_OK)` on Unix; on Windows, the
/// folder isn't read-only).
pub fn writable(dir: &Path) -> io::Result<()> {
    imp::writable(dir)
}

/// The mount points of sacdfs's own mounts, which `sacdfs verify` refuses to take extracts
/// from: on Linux the FUSE mounts named `sacdfs` in `/proc/self/mountinfo`. *Fallback:* none
/// found elsewhere so far.
pub fn sacdfs_mounts() -> Vec<PathBuf> {
    imp::sacdfs_mounts()
}

/// The folder whose entries name this process's open descriptors by number, if the system
/// has one: `/proc/PID/fd` on Linux, `/dev/fd` on macOS; `None` on Windows.
pub fn fd_dir() -> Option<PathBuf> {
    imp::fd_dir()
}

/// Sets the modification time of the regular file or folder at `path`. On Unix a symbolic
/// link is not followed, a FIFO is not waited on, and anything but a regular file or a folder
/// is refused. On Windows the open asks for the right to write attributes, which a plain
/// open for reading lacks.
pub fn set_modified(path: &Path, t: SystemTime) -> io::Result<()> {
    imp::set_modified(path, t)
}

/// The total size of the filesystem holding `path`: (blocks, block size). Windows:
/// `GetDiskFreeSpaceExW`'s total, in blocks of 4096 bytes.
pub fn fs_size(path: &Path) -> io::Result<(u64, u32)> {
    imp::fs_size(path)
}

/// Lowers the calling thread's CPU and I/O priority as far as the system allows without
/// privileges, for background work: nice 19 and the idle I/O class on Linux, `PRIO_DARWIN_BG`
/// on macOS, `THREAD_MODE_BACKGROUND_BEGIN` on Windows. Best effort: each step that failed, by
/// name.
pub fn lower_thread_priority() -> Vec<(&'static str, io::Error)> {
    imp::lower_thread_priority()
}

/// CPU seconds, user and system, of this process and of the programs it ran that ended.
/// *Fallback:* 0 where the system doesn't say (Windows so far).
pub fn cpu_seconds() -> f64 {
    imp::cpu_seconds()
}

/// The 1-minute load average. *Fallback:* `None` where the system has no `/proc/loadavg` (macOS
/// and Windows so far): nothing waits for the load there.
pub fn load_average() -> Option<f64> {
    imp::load_average()
}

/// Sets `stop` on SIGINT or SIGTERM, and ends the process with status 130 on a second one
/// while `stop` is still set. *Fallback:* nothing on Windows, where Ctrl-C ends the process at
/// once.
pub fn stop_on_signals(stop: &std::sync::Arc<std::sync::atomic::AtomicBool>) -> io::Result<()> {
    imp::stop_on_signals(stop)
}

/// The name of the process that thread `tid` belongs to (Linux: its thread group's `comm`).
/// *Fallback:* `None` where the system's process table isn't read (macOS and Windows so far),
/// or the thread is unknown.
pub fn process_name(tid: u32) -> Option<String> {
    imp::process_name(tid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file_of(len: usize) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&(0..len).map(|i| i as u8).collect::<Vec<_>>()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn positional_reads() {
        let f = file_of(1000);
        let mut buf = [0u8; 10];
        read_exact_at(f.as_file(), &mut buf, 500).unwrap();
        assert_eq!(buf[0], (500 % 256) as u8);
        assert!(read_at(f.as_file(), &mut buf, 995).unwrap() <= 5);
        let e = read_exact_at(f.as_file(), &mut buf, 995).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn identity_tells_files_apart() {
        let (a, b) = (file_of(10), file_of(10));
        let id = |f: &tempfile::NamedTempFile| file_id(f.as_file(), &f.as_file().metadata().unwrap());
        assert_eq!(id(&a), id(&a));
        assert_eq!(file_id_at(a.path()).unwrap().1, id(&a));
        assert_eq!(file_id_at(a.path()).unwrap().0.len(), 10);
        assert_ne!(id(&a), id(&b));
        assert_ne!(id(&a), (0, 0));
    }

    #[test]
    fn modification_times_match_the_system_time() {
        let f = file_of(1);
        let m = f.as_file().metadata().unwrap();
        let d = m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap();
        assert_eq!(mtime(&m), (d.as_secs() as i64, i64::from(d.subsec_nanos())));
    }

    #[test]
    fn errors_are_recognised() {
        assert!(is_stale(&stale()));
        assert!(!is_stale(&io::Error::other("x")));
        assert!(is_cross_device(&cross_device()));
        assert!(!is_cross_device(&stale()));
    }

    /// Opens under a folder are relative to its handle: a renamed folder is still the one
    /// opened, a link is never followed, and a name is one name.
    #[test]
    fn confined_opens_stay_in_the_folder_opened() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub").join("cover.jpg"), b"old").unwrap();
        let root = open_root(d.path()).unwrap();
        let sub = open_dir_at(&root, &[OsStr::new("sub")]).unwrap();
        // The folder is replaced after it was opened.
        std::fs::rename(d.path().join("sub"), d.path().join("moved")).unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub").join("cover.jpg"), b"new!").unwrap();
        let (mut f, st) = open_file_at(&sub, OsStr::new("cover.jpg")).unwrap();
        let mut got = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut got).unwrap();
        assert_eq!(got, b"old", "the folder opened, not the one now at its path");
        assert!(st.regular);
        assert_eq!(stat_at(&sub, OsStr::new("cover.jpg")).unwrap().size, 3);
        assert!(!stat_at(&root, OsStr::new("sub")).unwrap().regular);
        assert!(open_dir_at(&root, &[OsStr::new("moved"), OsStr::new("cover.jpg")]).is_err());
        // Windows takes one plain name only (callers pass plain names on every system).
        #[cfg(windows)]
        {
            for bad in ["..", ".", "", "moved\\cover.jpg", "moved/cover.jpg"] {
                assert!(open_file_at(&root, OsStr::new(bad)).is_err(), "{bad:?}");
            }
            assert!(open_file_at(&root, OsStr::new("sub")).is_err(), "a folder is not a file");
        }
    }

    #[cfg(windows)]
    #[test]
    fn confined_opens_follow_no_link() {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.jpg"), b"x").unwrap();
        // Links need the right to make them (an administrator, or developer mode).
        if symlink_dir(outside.path(), d.path().join("dirlink")).is_err() {
            eprintln!("no right to make links here: not checked");
            return;
        }
        symlink_file(outside.path().join("secret.jpg"), d.path().join("filelink.jpg")).unwrap();
        let root = open_root(d.path()).unwrap();
        assert!(open_dir_at(&root, &[OsStr::new("dirlink")]).is_err());
        if let Ok((_, st)) = open_file_at(&root, OsStr::new("filelink.jpg")) {
            assert!(!st.regular, "a link is not a regular file");
        }
        assert!(!stat_at(&root, OsStr::new("filelink.jpg")).unwrap().regular);
    }

    #[test]
    fn private_folders_stay_usable() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("cache");
        std::fs::create_dir(&p).unwrap();
        make_private_dir(&p).unwrap();
        std::fs::write(p.join("entry"), b"x").unwrap();
        assert_eq!(std::fs::read(p.join("entry")).unwrap(), b"x");
    }

    #[test]
    fn log_text_without_verbatim_prefixes() {
        let t = r"\\?\C:\music\a.iso track 1: x; \\?\UNC\nas\m\b.iso";
        if cfg!(windows) {
            assert_eq!(plain_text(t), r"C:\music\a.iso track 1: x; \\nas\m\b.iso");
        } else {
            assert_eq!(plain_text(t), t);
        }
        assert_eq!(plain_text("plain"), "plain");
    }

    #[test]
    fn path_bytes_round_trip() {
        let name = std::ffi::OsStr::new("Künstler/Ä 01 - x.dsf");
        assert_eq!(os_from_bytes(name.as_encoded_bytes().to_vec()), name);
    }

    #[test]
    fn a_folder_is_not_a_regular_file() {
        let d = tempfile::tempdir().unwrap();
        // Refused: on Unix as no regular file, on Windows already by the open.
        assert!(open_regular(d.path()).is_err());
        let f = file_of(3);
        assert_eq!(open_regular(f.path()).unwrap().metadata().unwrap().len(), 3);
    }

    #[test]
    fn a_plain_file_is_all_data() {
        let f = file_of(8192);
        assert_eq!(next_data(f.as_file(), 0).unwrap(), Some(0));
        assert_eq!(next_data(f.as_file(), 100).unwrap(), Some(100));
        // The only hole is the virtual one at the end, where a file system reports holes.
        if let Some(h) = next_hole(f.as_file(), 0) {
            assert_eq!(h, 8192);
        }
    }

    #[test]
    fn the_file_system_has_a_size() {
        let d = tempfile::tempdir().unwrap();
        let (blocks, size) = fs_size(d.path()).unwrap();
        assert!(blocks > 0 && size > 0);
    }

    #[test]
    fn folders_open_one_name_at_a_time() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("a/b")).unwrap();
        std::fs::write(d.path().join("a/b/x.jpg"), b"art").unwrap();
        let root = open_root(d.path()).unwrap();
        let dir = open_dir_at(&root, &[OsStr::new("a"), OsStr::new("b")]).unwrap();
        let st = stat_at(&dir, OsStr::new("x.jpg")).unwrap();
        assert!(st.regular);
        assert_eq!(st.size, 3);
        let (f, st2) = open_file_at(&dir, OsStr::new("x.jpg")).unwrap();
        assert_eq!(st2.size, 3);
        let mut buf = [0u8; 3];
        read_exact_at(&f, &mut buf, 0).unwrap();
        assert_eq!(&buf, b"art");
        assert!(!stat_dir(&dir).unwrap().regular);
        assert!(open_dir_at(&root, &[OsStr::new("a"), OsStr::new("b"), OsStr::new("x.jpg")]).is_err());
        assert!(open_dir_at(&root, &[OsStr::new("none")]).is_err());
    }

    #[test]
    fn the_background_priority_goes_in() {
        let failed = std::thread::spawn(lower_thread_priority).join().unwrap();
        assert!(failed.is_empty(), "{failed:?}");
    }

    #[test]
    fn a_new_temporary_folder_is_writable() {
        let d = tempfile::tempdir().unwrap();
        writable(d.path()).unwrap();
    }
}
