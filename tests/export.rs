// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent file identity through the kernel (issue #18): the same inode numbers on every
//! mount, and file handles (as the kernel NFS server keeps them) that still open the same file
//! after sacdfs restarts.
//!
//! The inode tests need `/dev/fuse` and `fusermount3`, like `tests/mount.rs`. The file handle
//! tests also need `open_by_handle_at`, which needs `CAP_DAC_READ_SEARCH` (as root, or in a
//! container with `--cap-add DAC_READ_SEARCH` where sacdfs mounts in the container's own user
//! namespace); without it they are skipped, unless `SACDFS_EXPORT_TESTS=1` is set.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// name_to_handle_at and open_by_handle_at have no safe wrapper.
#![allow(unsafe_code)]

mod common;

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirEntryExt, FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use common::corpus;

fn fuse_available() -> bool {
    let ok = Path::new("/dev/fuse").exists()
        && Command::new("fusermount3").arg("-V").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok();
    if !ok && std::env::var_os("SACDFS_FUSE_TESTS").is_some_and(|v| v == "1") {
        panic!("SACDFS_FUSE_TESTS=1 but /dev/fuse or fusermount3 is missing");
    }
    if !ok {
        eprintln!("skipping: no /dev/fuse or fusermount3");
    }
    ok
}

fn is_mounted(mountpoint: &Path) -> bool {
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mp = mountpoint.to_str().unwrap();
    info.lines().any(|l| {
        l.split(' ').nth(4) == Some(mp) && l.split(" - ").nth(1).is_some_and(|t| t.starts_with("fuse"))
    })
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// sacdfs mounted in the foreground on a given directory.
struct Mount {
    child: Option<Child>,
    at: PathBuf,
}

impl Mount {
    fn new(source: &Path, at: &Path, cache: &Path, extra: &[&str]) -> Mount {
        let child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .env("XDG_CACHE_HOME", cache)
            .arg("-f")
            .args(extra)
            .arg(source)
            .arg(at)
            .spawn()
            .unwrap();
        let at = at.canonicalize().unwrap();
        let m = Mount { child: Some(child), at };
        wait_for("the mount", || is_mounted(&m.at));
        m
    }

    /// Unmounts and waits for sacdfs to exit. The unmount is retried: right after files
    /// opened by handle are closed, the kernel can still hold the mount busy for a moment
    /// (a signal would make sacdfs try only once, and then keep serving).
    fn stop(mut self) {
        let mut child = self.child.take().unwrap();
        let start = Instant::now();
        while is_mounted(&self.at) {
            let ok =
                Command::new("fusermount3").arg("-u").arg(&self.at).stderr(Stdio::null()).status().unwrap();
            if ok.success() {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "the mount stays busy");
            std::thread::sleep(Duration::from_millis(50));
        }
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "sacdfs exited with {status}");
                break;
            }
            if start.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                panic!("sacdfs did not exit after the unmount");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!is_mounted(&self.at));
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if is_mounted(&self.at) {
            let _ = Command::new("fusermount3").arg("-u").arg("-z").arg(&self.at).status();
        }
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Every path under `root` with its inode number, as `stat` shows it.
fn inodes(root: &Path) -> BTreeMap<PathBuf, u64> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let meta = std::fs::metadata(e.path()).unwrap();
            // readdir and stat agree.
            assert_eq!(e.ino(), meta.ino(), "{:?}", e.path());
            out.insert(e.path().strip_prefix(root).unwrap().to_owned(), meta.ino());
            if meta.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out
}

/// A source tree of a few corpus images in nested folders (copies, so that their mtime can
/// be changed).
fn source() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let images = corpus().join("images");
    for (at, image) in [
        ("Artist/Album/Rip/disc.iso", "dsd-stereo-mch-2048.iso"),
        ("Artist/Other/dst.iso", "dst-stereo-5ch-2054.iso"),
        ("loose.iso", "area-tracks-255.iso"),
    ] {
        let path = dir.path().join(at);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::copy(images.join(image), &path).unwrap();
    }
    dir
}

#[test]
fn inode_numbers_are_the_same_on_every_mount() {
    if !fuse_available() {
        return;
    }
    let cache = tempfile::tempdir().unwrap();
    let at = tempfile::tempdir().unwrap();
    // The whole corpus, hostile images included: every number distinct.
    let images = corpus().join("images");
    let m = Mount::new(&images, at.path(), cache.path(), &["--threads", "2"]);
    let first = inodes(&m.at);
    m.stop();
    let mut all: Vec<u64> = first.values().copied().collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), first.len(), "inode numbers are not unique");
    let m = Mount::new(&images, at.path(), cache.path(), &["--threads", "2"]);
    assert_eq!(inodes(&m.at), first, "inode numbers changed across a remount");
    m.stop();

    // With --flatten: the images, areas and files keep their numbers.
    let src = source();
    let m = Mount::new(src.path(), at.path(), cache.path(), &[]);
    let plain = inodes(&m.at);
    m.stop();
    let m = Mount::new(src.path(), at.path(), cache.path(), &["--flatten"]);
    let flat = inodes(&m.at);
    m.stop();
    // The collapsed directories and the image directories they stand for.
    let collapsed = [("Artist/Album", "Artist/Album/Rip/disc"), ("Artist/Other", "Artist/Other/dst")];
    for (p, ino) in &flat {
        let q = collapsed
            .iter()
            .find_map(|(f, i)| p.strip_prefix(f).ok().map(|rest| Path::new(i).join(rest)))
            .unwrap_or_else(|| p.clone());
        assert_eq!(plain.get(&q), Some(ino), "{p:?} is {q:?} without --flatten");
    }
    assert_eq!(flat.len(), plain.len() - 3, "only the collapsed directories are gone");
}

/// A file handle, as `name_to_handle_at` gives it (the layout of `struct file_handle`).
#[repr(C)]
struct Handle {
    bytes: u32,
    kind: i32,
    data: [u8; 128],
}

fn name_to_handle(path: &Path) -> io::Result<Handle> {
    let mut h = Handle { bytes: 128, kind: 0, data: [0; 128] };
    let mut mount_id: libc::c_int = 0;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: valid NUL-terminated path, a handle buffer of the size it says, a valid int.
    let r = unsafe {
        libc::syscall(
            libc::SYS_name_to_handle_at,
            libc::AT_FDCWD,
            c.as_ptr(),
            &mut h as *mut Handle,
            &mut mount_id as *mut libc::c_int,
            0,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(h)
}

fn open_by_handle(mount: &Path, h: &Handle) -> io::Result<File> {
    open_by_handle_with(mount, h, libc::O_RDONLY)
}

fn open_dir_by_handle(mount: &Path, h: &Handle) -> io::Result<File> {
    open_by_handle_with(mount, h, libc::O_RDONLY | libc::O_DIRECTORY)
}

fn open_by_handle_with(mount: &Path, h: &Handle, flags: libc::c_int) -> io::Result<File> {
    let m = File::open(mount)?;
    // SAFETY: a valid descriptor and a handle filled by name_to_handle_at.
    let fd = unsafe { libc::syscall(libc::SYS_open_by_handle_at, m.as_raw_fd(), h as *const Handle, flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: open_by_handle_at returned a new descriptor that nothing else owns.
    Ok(unsafe { File::from_raw_fd(fd as libc::c_int) })
}

/// Whether handles like the one of `probe` can be opened here; panics if they must be
/// (`SACDFS_EXPORT_TESTS=1`). Directory handles need `CAP_DAC_READ_SEARCH` in the mount's
/// user namespace, file handles in the initial one (root, as the NFS server is).
fn handles_available(mount: &Path, probe: &Path) -> bool {
    let must = std::env::var_os("SACDFS_EXPORT_TESTS").is_some_and(|v| v == "1");
    let h = name_to_handle(probe).unwrap_or_else(|e| panic!("name_to_handle_at: {e}"));
    let opened = if probe.is_dir() { open_dir_by_handle(mount, &h) } else { open_by_handle(mount, &h) };
    match opened {
        Ok(_) => true,
        Err(e) if !must && e.raw_os_error() == Some(libc::EPERM) => {
            eprintln!("skipping: open_by_handle_at: {e} (needs CAP_DAC_READ_SEARCH)");
            false
        }
        Err(e) => panic!("open_by_handle_at: {e}"),
    }
}

#[test]
fn file_handles_survive_a_restart() {
    if !fuse_available() {
        return;
    }
    let src = source();
    let cache = tempfile::tempdir().unwrap();
    let at = tempfile::tempdir().unwrap();
    for extra in [&[][..], &["--flatten"][..]] {
        let m = Mount::new(src.path(), at.path(), cache.path(), extra);
        let area =
            if extra.is_empty() { "Artist/Album/Rip/disc/Multichannel" } else { "Artist/Album/Multichannel" };
        let dir = m.at.join(area);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        files.push(m.at.join("Artist/Other/dst/Stereo").read_dir().unwrap().next().unwrap().unwrap().path());
        if !handles_available(&m.at, &files[0]) {
            return;
        }
        // Read half of each file through an open handle, keep the file handles.
        let mut kept = Vec::new();
        for f in &files {
            let whole = std::fs::read(f).unwrap();
            let h = name_to_handle(f).unwrap();
            let open = open_by_handle(&m.at, &h).unwrap();
            let mut half = vec![0; whole.len() / 2];
            open.read_exact_at(&mut half, 0).unwrap();
            assert_eq!(half, whole[..half.len()]);
            kept.push((f.clone(), h, whole));
        }
        m.stop();

        // A new sacdfs: the same handles open the same files, which read on where they were.
        let m = Mount::new(src.path(), at.path(), cache.path(), extra);
        for (f, h, whole) in &kept {
            let open = open_by_handle(&m.at, h).unwrap_or_else(|e| panic!("{f:?} after restart: {e}"));
            let mut rest = vec![0; whole.len() - whole.len() / 2];
            open.read_exact_at(&mut rest, (whole.len() / 2) as u64).unwrap();
            assert_eq!(rest, whole[whole.len() / 2..], "{f:?}");
        }
        m.stop();
    }

    // A changed image: its files' handles go stale; the other image's still open.
    let m = Mount::new(src.path(), at.path(), cache.path(), &[]);
    let changed = m.at.join("Artist/Album/Rip/disc/Stereo");
    let changed = changed.read_dir().unwrap().next().unwrap().unwrap().path();
    let other = m.at.join("Artist/Other/dst/Stereo").read_dir().unwrap().next().unwrap().unwrap().path();
    let (hc, ho) = (name_to_handle(&changed).unwrap(), name_to_handle(&other).unwrap());
    m.stop();
    let image = File::options().write(true).open(src.path().join("Artist/Album/Rip/disc.iso")).unwrap();
    image.set_modified(SystemTime::now() - Duration::from_secs(3600)).unwrap();
    let m = Mount::new(src.path(), at.path(), cache.path(), &[]);
    let e = open_by_handle(&m.at, &hc).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::ESTALE), "{e}");
    open_by_handle(&m.at, &ho).unwrap();
    m.stop();
}

#[test]
fn directory_handles_survive_a_restart() {
    if !fuse_available() {
        return;
    }
    let src = source();
    let cache = tempfile::tempdir().unwrap();
    let at = tempfile::tempdir().unwrap();
    for (extra, dirs) in [
        (
            &[][..],
            &[
                "Artist",
                "Artist/Album/Rip",
                "Artist/Album/Rip/disc",
                "Artist/Album/Rip/disc/Stereo",
                "loose/Stereo",
            ][..],
        ),
        (
            &["--flatten"][..],
            &["Artist/Album", "Artist/Album/Multichannel", "Artist/Other/Stereo", "loose"][..],
        ),
    ] {
        let m = Mount::new(src.path(), at.path(), cache.path(), extra);
        if !handles_available(&m.at, &m.at.join(dirs[0])) {
            return;
        }
        let mut kept = Vec::new();
        for d in dirs {
            let p = m.at.join(d);
            let names: Vec<_> = std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().file_name()).collect();
            kept.push((p.clone(), name_to_handle(&p).unwrap(), std::fs::metadata(&p).unwrap().ino(), names));
        }
        m.stop();
        // A new sacdfs, nothing looked up yet: each handle is found from its number alone
        // (`.`), and connected to the root through `..`.
        let m = Mount::new(src.path(), at.path(), cache.path(), extra);
        for (p, h, ino, names) in &kept {
            let d = open_dir_by_handle(&m.at, h).unwrap_or_else(|e| panic!("{p:?} after restart: {e}"));
            assert_eq!(d.metadata().unwrap().ino(), *ino, "{p:?}");
            let fd = PathBuf::from(format!("/proc/self/fd/{}", d.as_raw_fd()));
            // The kernel knows its path again.
            assert_eq!(std::fs::read_link(&fd).unwrap(), *p);
            let again: Vec<_> = std::fs::read_dir(&fd).unwrap().map(|e| e.unwrap().file_name()).collect();
            assert_eq!(&again, names, "{p:?}");
        }
        m.stop();
    }
}

#[test]
fn a_busy_mount_is_detached_on_sigterm() {
    if !fuse_available() {
        return;
    }
    let src = source();
    let cache = tempfile::tempdir().unwrap();
    let at = tempfile::tempdir().unwrap();
    let mut m = Mount::new(src.path(), at.path(), cache.path(), &[]);
    let path = m.at.join("Artist/Other/dst/Stereo").read_dir().unwrap().next().unwrap().unwrap().path();
    let whole = std::fs::read(&path).unwrap();
    let open = File::open(&path).unwrap();
    let mut child = m.child.take().unwrap();
    assert!(Command::new("kill").arg("-TERM").arg(child.id().to_string()).status().unwrap().success());
    // Detached at once (as root a plain unmount would fail: the file is open) ...
    wait_for("the detach", || !is_mounted(&m.at));
    // ... while the open file still reads, until it is closed.
    let mut buf = vec![0; 4096];
    open.read_exact_at(&mut buf, (whole.len() - 4096) as u64).unwrap();
    assert_eq!(buf, whole[whole.len() - 4096..]);
    assert!(child.try_wait().unwrap().is_none(), "sacdfs stopped with a file still open");
    drop(open);
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "sacdfs did not exit after the last close");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{status}");
}
