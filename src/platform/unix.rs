// SPDX-License-Identifier: GPL-3.0-or-later

//! [`super`] on Unix: Linux, with macOS's differences.

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use nix::fcntl::{AtFlags, OFlag, openat, readlinkat};
use nix::sys::stat::{FileStat, Mode, SFlag, fstat, fstatat};

use super::{OpenFlags, Stat};

pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

pub fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

pub fn mtime(m: &Metadata) -> (i64, i64) {
    (m.mtime(), m.mtime_nsec())
}

pub fn file_id(_file: &File, m: &Metadata) -> (u64, u64) {
    (m.dev(), m.ino())
}

pub fn file_id_at(path: &Path) -> io::Result<(Metadata, (u64, u64))> {
    let m = std::fs::metadata(path)?;
    let id = (m.dev(), m.ino());
    Ok((m, id))
}

pub fn plain_text(text: &str) -> std::borrow::Cow<'_, str> {
    std::borrow::Cow::Borrowed(text)
}

pub fn cache_home() -> Option<PathBuf> {
    let abs = |v: OsString| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
    std::env::var_os("XDG_CACHE_HOME")
        .and_then(abs)
        .or_else(|| std::env::var_os("HOME").and_then(abs).map(|h| h.join(".cache")))
}

pub fn is_transient(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOMEM | libc::EIO | libc::EAGAIN | libc::EINTR)
    )
}

pub const TOO_MANY_OPEN_FILES: i32 = libc::EMFILE;

pub const IO_ERROR: i32 = libc::EIO;

pub fn refuse_link(_file: &File) -> io::Result<()> {
    Ok(())
}

pub fn stale() -> io::Error {
    io::Error::from_raw_os_error(libc::ESTALE)
}

pub fn is_stale(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ESTALE)
}

pub fn cross_device() -> io::Error {
    io::Error::from_raw_os_error(libc::EXDEV)
}

pub fn is_cross_device(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::EXDEV)
}

pub fn os_from_bytes(b: Vec<u8>) -> OsString {
    std::os::unix::ffi::OsStringExt::from_vec(b)
}

pub fn set_open_flags(o: &mut OpenOptions, flags: OpenFlags) -> &mut OpenOptions {
    let mut f = 0;
    if flags.has(OpenFlags::NO_FOLLOW) {
        f |= libc::O_NOFOLLOW;
    }
    if flags.has(OpenFlags::NON_BLOCK) {
        f |= libc::O_NONBLOCK;
    }
    if flags.has(OpenFlags::NO_CTTY) {
        f |= libc::O_NOCTTY;
    }
    o.custom_flags(f)
}

pub fn is_link_refused(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ELOOP)
}

pub fn set_create_mode(o: &mut OpenOptions, mode: u32) -> &mut OpenOptions {
    o.mode(mode)
}

pub fn make_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

pub fn open_regular(path: &Path) -> io::Result<File> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    let flags = OFlag::from_bits_truncate(fcntl(&file, FcntlArg::F_GETFL)?);
    fcntl(&file, FcntlArg::F_SETFL(flags - OFlag::O_NONBLOCK))?;
    Ok(file)
}

fn seek(file: &File, at: u64, whence: nix::unistd::Whence) -> Result<u64, nix::errno::Errno> {
    use std::os::fd::AsFd;
    let at = i64::try_from(at).map_err(|_| nix::errno::Errno::EOVERFLOW)?;
    nix::unistd::lseek(file.as_fd(), at, whence).map(|p| u64::try_from(p).unwrap_or(0))
}

pub fn next_hole(file: &File, at: u64) -> Option<u64> {
    seek(file, at, nix::unistd::Whence::SeekHole).ok()
}

pub fn next_data(file: &File, at: u64) -> io::Result<Option<u64>> {
    match seek(file, at, nix::unistd::Whence::SeekData) {
        Ok(d) => Ok(Some(d)),
        Err(nix::errno::Errno::ENXIO) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn hole_bound(file: &File) -> Option<u64> {
    let m = file.metadata().ok()?;
    network_share(file, m.dev()).then(|| m.len().saturating_sub(m.blocks().saturating_mul(512)))
}

/// Whether `file`, on device `dev`, is on an NFS or SMB share: one `fstatfs` per device (on
/// NFS that is a round trip too), remembered for the process.
fn network_share(file: &File, dev: u64) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static KNOWN: OnceLock<Mutex<HashMap<u64, bool>>> = OnceLock::new();
    let known = KNOWN.get_or_init(Mutex::default);
    if let Some(&net) = known.lock().unwrap_or_else(|e| e.into_inner()).get(&dev) {
        return net;
    }
    let net = is_network(file);
    known.lock().unwrap_or_else(|e| e.into_inner()).insert(dev, net);
    net
}

/// NFS, SMB 1 and CIFS, SMB 2 and 3 (Linux's magic numbers, compared as 32 bits: `f_type` is
/// 32 bits wide on some targets).
#[cfg(target_os = "linux")]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::unnecessary_cast)]
fn is_network(file: &File) -> bool {
    const NETWORK: [u32; 4] = [0x6969, 0x517b, 0xff53_4d42, 0xfe53_4d42];
    nix::sys::statfs::fstatfs(file).is_ok_and(|s| NETWORK.contains(&(s.filesystem_type().0 as u32)))
}

#[cfg(target_os = "macos")]
fn is_network(file: &File) -> bool {
    nix::sys::statfs::fstatfs(file).is_ok_and(|s| matches!(s.filesystem_type_name(), "nfs" | "smbfs"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn is_network(_file: &File) -> bool {
    false
}

pub type Dir = OwnedFd;

/// How a folder is opened to walk from: `O_PATH` on Linux, which needs no read permission and
/// can't read; read-only elsewhere (macOS has no `O_PATH`), which needs read permission on each
/// folder walked.
#[cfg(target_os = "linux")]
const DIR_WALK: OFlag = OFlag::O_PATH;
#[cfg(not(target_os = "linux"))]
const DIR_WALK: OFlag = OFlag::O_RDONLY;

fn time(secs: i64, nanos: i64) -> SystemTime {
    let nanos = u32::try_from(nanos.clamp(0, 999_999_999)).unwrap_or(0);
    if secs >= 0 {
        SystemTime::UNIX_EPOCH + Duration::new(secs.unsigned_abs(), nanos)
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_secs(secs.unsigned_abs())
            + Duration::from_nanos(u64::from(nanos))
    }
}

// The `stat` fields' types differ between Linux and macOS (`i64` and `u64`, `i64` and `long`).
#[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
fn stat(st: &FileStat) -> Stat {
    Stat {
        regular: SFlag::from_bits_truncate(st.st_mode & SFlag::S_IFMT.bits()) == SFlag::S_IFREG,
        size: u64::try_from(st.st_size).unwrap_or(0),
        blocks: u64::try_from(st.st_blocks).unwrap_or(0),
        atime: time(st.st_atime as i64, st.st_atime_nsec as i64),
        mtime: time(st.st_mtime as i64, st.st_mtime_nsec as i64),
        ctime: time(st.st_ctime as i64, st.st_ctime_nsec as i64),
    }
}

pub fn open_root(path: &Path) -> io::Result<Dir> {
    Ok(nix::fcntl::open(path, DIR_WALK | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC, Mode::empty())?)
}

pub fn open_dir_at(dir: &Dir, names: &[&OsStr]) -> io::Result<Dir> {
    let flags = DIR_WALK | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let mut fd = openat(dir, ".", flags, Mode::empty())?;
    for name in names {
        fd = openat(&fd, *name, flags, Mode::empty())?;
    }
    Ok(fd)
}

pub fn stat_dir(dir: &Dir) -> io::Result<Stat> {
    Ok(stat(&fstat(dir)?))
}

pub fn stat_at(dir: &Dir, name: &OsStr) -> io::Result<Stat> {
    Ok(stat(&fstatat(dir, name, AtFlags::AT_SYMLINK_NOFOLLOW)?))
}

pub fn open_file_at(dir: &Dir, name: &OsStr) -> io::Result<(File, Stat)> {
    // O_NONBLOCK: a FIFO put there since the listing must not hold the open; the caller
    // refuses it. It means nothing for a regular file.
    let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK | OFlag::O_NOCTTY;
    let fd = openat(dir, name, flags, Mode::empty())?;
    let st = stat(&fstat(&fd)?);
    Ok((File::from(fd), st))
}

pub fn read_link_at(dir: &Dir, name: &OsStr) -> io::Result<PathBuf> {
    Ok(PathBuf::from(readlinkat(dir, name)?))
}

#[allow(unsafe_code)]
pub fn writable(dir: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: `c` is a valid NUL-terminated string for the duration of the call.
    if unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
pub fn sacdfs_mounts() -> Vec<PathBuf> {
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    info.lines()
        .filter_map(|l| {
            let mp = l.split(' ').nth(4)?;
            let mut tail = l.split(" - ").nth(1)?.split(' ');
            let (fstype, source) = (tail.next()?, tail.next()?);
            let ours = fstype == "fuse.sacdfs" || (fstype.starts_with("fuse") && source == "sacdfs");
            ours.then(|| PathBuf::from(unescape_octal(mp)))
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
pub fn sacdfs_mounts() -> Vec<PathBuf> {
    Vec::new()
}

/// Undoes mountinfo's octal escapes (`\040` for a space).
#[cfg(any(target_os = "linux", test))]
fn unescape_octal(s: &str) -> OsString {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = b[i + 1..i + 4].iter().fold(0u32, |a, &c| a * 8 + u32::from(c - b'0'));
            out.push((v & 0xFF) as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    os_from_bytes(out)
}

#[cfg(target_os = "linux")]
pub fn fd_dir() -> Option<PathBuf> {
    Some(Path::new("/proc").join(std::process::id().to_string()).join("fd"))
}

#[cfg(not(target_os = "linux"))]
pub fn fd_dir() -> Option<PathBuf> {
    Some(PathBuf::from("/dev/fd"))
}

pub fn set_modified(path: &Path, t: SystemTime) -> io::Result<()> {
    File::open(path)?.set_modified(t)
}

pub fn fs_size(path: &Path) -> io::Result<(u64, u32)> {
    let st = nix::sys::statvfs::statvfs(path).map_err(io::Error::from)?;
    let frsize = if st.fragment_size() > 0 { st.fragment_size() } else { st.block_size() };
    // `fsblkcnt_t` is 64 bits on Linux and 32 on macOS.
    #[allow(clippy::useless_conversion)]
    let blocks = u64::from(st.blocks());
    Ok((blocks, u32::try_from(frsize).unwrap_or(1 << 16).max(1)))
}

/// Nice 19 and the idle I/O class for this thread.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn lower_thread_priority() -> Vec<(&'static str, io::Error)> {
    // ioprio_set(IOPRIO_WHO_PROCESS, tid, IOPRIO_PRIO_VALUE(IOPRIO_CLASS_IDLE, 0)).
    const IOPRIO_WHO_PROCESS: libc::c_int = 1;
    const IOPRIO_CLASS_IDLE: libc::c_int = 3;
    let mut failed = Vec::new();
    // The raw call: glibc before 2.30 (the release's oldest baseline) has no gettid(). A
    // thread ID is positive and fits `pid_t` and `id_t`.
    // SAFETY: a system call with no arguments, which cannot fail.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as libc::pid_t;
    // SAFETY: a system call on the calling thread, with integer arguments only.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, 19) } != 0 {
        failed.push(("nice", io::Error::last_os_error()));
    }
    // SAFETY: a system call on the calling thread, with integer arguments only.
    if unsafe { libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, tid, IOPRIO_CLASS_IDLE << 13) } != 0 {
        failed.push(("ioprio", io::Error::last_os_error()));
    }
    failed
}

/// macOS's background band for this thread: lower CPU priority and throttled I/O, the
/// counterpart of nice 19 and the idle I/O class.
#[cfg(target_vendor = "apple")]
#[allow(unsafe_code)]
pub fn lower_thread_priority() -> Vec<(&'static str, io::Error)> {
    // SAFETY: a plain system call on the calling thread (`who` 0), with no pointers.
    if unsafe { libc::setpriority(libc::PRIO_DARWIN_THREAD, 0, libc::PRIO_DARWIN_BG) } != 0 {
        return vec![("background", io::Error::last_os_error())];
    }
    Vec::new()
}

/// Other Unix systems: the thread keeps its priority.
#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
pub fn lower_thread_priority() -> Vec<(&'static str, io::Error)> {
    Vec::new()
}

pub fn cpu_seconds() -> f64 {
    use nix::sys::resource::{UsageWho, getrusage};
    let secs = |who| {
        getrusage(who).map_or(0.0, |u| {
            let (a, b) = (u.user_time(), u.system_time());
            a.tv_sec() as f64 + b.tv_sec() as f64 + (a.tv_usec() + b.tv_usec()) as f64 / 1e6
        })
    };
    secs(UsageWho::RUSAGE_SELF) + secs(UsageWho::RUSAGE_CHILDREN)
}

pub fn load_average() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg").ok()?.split_whitespace().next()?.parse().ok()
}

pub fn stop_on_signals(stop: &std::sync::Arc<std::sync::atomic::AtomicBool>) -> io::Result<()> {
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        // The first stops at the next safe point; a second ends the run now.
        signal_hook::flag::register_conditional_shutdown(sig, 130, stop.clone())?;
        signal_hook::flag::register(sig, stop.clone())?;
    }
    Ok(())
}

/// The `comm` of thread `tid`'s thread group, from `/proc`.
#[cfg(target_os = "linux")]
pub fn process_name(tid: u32) -> Option<String> {
    let tgid = std::fs::read_to_string(format!("/proc/{tid}/status"))
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("Tgid:").map(|v| v.trim().to_owned())))?;
    let comm = std::fs::read_to_string(format!("/proc/{tgid}/comm")).ok()?;
    Some(comm.trim_end().to_owned())
}

#[cfg(not(target_os = "linux"))]
pub fn process_name(_tid: u32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_escapes() {
        assert_eq!(unescape_octal(r"/a\040b"), OsString::from("/a b"));
        assert_eq!(unescape_octal(r"/a\04"), OsString::from(r"/a\04"));
    }
}
