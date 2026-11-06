// SPDX-License-Identifier: GPL-3.0-or-later

//! A stalled source (#63, #72): an image that stops answering must neither hang its
//! requests for good nor hold up the requests of other images.
//!
//! The stall is real: an image is reached through a symbolic link outside SOURCE, which is
//! pointed at a FIFO once sacdfs is running. Opening a FIFO for reading blocks until a
//! writer comes, just as an open or a read on a hung share blocks in the kernel. The
//! directories of SOURCE do not change, so sacdfs does not notice and re-read them.
//!
//! Needs `/dev/fuse` and `fusermount3`, like `tests/mount.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The request timeout the mount is given.
const TIMEOUT: Duration = Duration::from_secs(2);

/// How much later than the timeout a stalled request may fail: the watchdog checks every
/// half second at this timeout, and a loaded CI runner is slow.
const SLACK: Duration = Duration::from_secs(4);

fn is_mounted(mountpoint: &Path) -> bool {
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mp = mountpoint.to_str().unwrap();
    info.lines().any(|l| {
        l.split(' ').nth(4) == Some(mp) && l.split(" - ").nth(1).is_some_and(|t| t.starts_with("fuse"))
    })
}

struct Mount {
    child: Option<Child>,
    path: PathBuf,
    _dir: tempfile::TempDir,
    _cache: tempfile::TempDir,
}

impl Mount {
    fn new(source: &Path, extra: &[&str]) -> Mount {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .env("XDG_CACHE_HOME", cache.path())
            .arg("-f")
            .args(extra)
            .arg(source)
            .arg(&path)
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !is_mounted(&path) {
            assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for the mount");
            std::thread::sleep(Duration::from_millis(20));
        }
        Mount { child: Some(child), path, _dir: dir, _cache: cache }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if is_mounted(&self.path) {
            let _ = Command::new("fusermount3").arg("-u").arg("-z").arg(&self.path).status();
        }
        if let Some(mut c) = self.child.take() {
            common::stop_gracefully(&mut c);
        }
    }
}

/// The first DSF file below `dir`.
fn first_dsf(dir: &Path) -> PathBuf {
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "dsf") {
                return p;
            }
        }
    }
    panic!("no DSF file below {}", dir.display());
}

/// Runs `f` on a thread of its own: the result and how long it took, or `None` if it has
/// not finished after `within`.
fn timed<T: Send + 'static>(
    within: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<(T, Duration)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let t = Instant::now();
        let r = f();
        let _ = tx.send((r, t.elapsed()));
    });
    rx.recv_timeout(within).ok()
}

/// Points `link` at a new FIFO: from then on, opening the image through it blocks.
fn stall(link: &Path, fifo: &Path) {
    assert!(Command::new("mkfifo").arg(fifo).status().unwrap().success());
    let tmp = link.with_extension("tmp");
    std::os::unix::fs::symlink(fifo, &tmp).unwrap();
    std::fs::rename(&tmp, link).unwrap();
}

/// Lets whatever is blocked opening `fifo` go on (and then read its end).
fn release(fifo: &Path) {
    // O_RDWR on a FIFO never blocks on Linux; the blocked reader's open then returns.
    let _w = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(fifo);
}

#[test]
fn a_stalled_image_fails_its_requests_and_holds_up_no_other() {
    common::skip_without_fuse!();
    let corpus = common::corpus().join("images");
    // SOURCE has one directory per image, each holding a link to a link outside SOURCE
    // (`links/`) to the corpus image; `links/` is what is switched to FIFOs.
    let base = tempfile::tempdir().unwrap();
    let (src, links) = (base.path().join("src"), base.path().join("links"));
    std::fs::create_dir_all(&links).unwrap();
    let names =
        [("Listed", "dsd-stereo-mch-2048"), ("Unlisted", "dsd-1ch"), ("Other", "dst-features-2ch-6ch")];
    for (dir, id) in names {
        std::fs::create_dir_all(src.join(dir)).unwrap();
        let outer = links.join(format!("{dir}.iso"));
        std::os::unix::fs::symlink(corpus.join(format!("{id}.iso")), &outer).unwrap();
        std::os::unix::fs::symlink(&outer, src.join(dir).join(format!("{id}.iso"))).unwrap();
    }
    let secs = TIMEOUT.as_secs().to_string();
    let m = Mount::new(&src, &["--request-timeout", &secs, "--no-index-cache"]);
    let mnt = m.path.clone();

    // A file of "Listed" is found (its image opened, its TOC read) while all is well.
    let listed = first_dsf(&mnt.join("Listed"));

    // 1. A listing that opens a stalled image (#63) fails within the timeout...
    let fifo_unlisted = base.path().join("unlisted.fifo");
    stall(&links.join("Unlisted.iso"), &fifo_unlisted);
    let dir = mnt.join("Unlisted");
    let listing = timed(TIMEOUT + SLACK + Duration::from_secs(5), move || {
        std::fs::read_dir(&dir).and_then(|d| d.collect::<Result<Vec<_>, _>>()).map(|v| v.len())
    });
    let (r, took) = listing.expect("the listing of a stalled image hangs");
    let e = r.expect_err("the listing of a stalled image succeeded");
    assert_eq!(e.raw_os_error(), Some(libc::EIO), "{e}");
    assert!(took >= TIMEOUT - Duration::from_millis(100) && took < TIMEOUT + SLACK, "failed after {took:?}");

    // 2. ... and while its thread is still blocked, the other images answer: a listing
    // and a whole file of one listed already, and the first listing of another.
    let start = Instant::now();
    let data = std::fs::read(&listed).unwrap();
    assert!(data.len() > 92 && &data[..4] == b"DSD ");
    let other = first_dsf(&mnt.join("Other"));
    assert!(std::fs::read(&other).unwrap().len() > 92);
    assert!(start.elapsed() < TIMEOUT, "other images waited for the stalled one: {:?}", start.elapsed());

    // 3. A read of a file whose image stalls after it was opened (#72) fails within the
    // timeout too. The image is opened again once its handle is checked against its path
    // (at most once a second), which is what then blocks.
    let fifo_listed = base.path().join("listed.fifo");
    stall(&links.join("Listed.iso"), &fifo_listed);
    std::thread::sleep(Duration::from_millis(1500));
    let file = first_dsf(&mnt.join("Listed"));
    let file = file.parent().unwrap().read_dir().unwrap().map(|e| e.unwrap().path()).max().unwrap_or(file);
    let read = timed(TIMEOUT + SLACK + Duration::from_secs(5), move || {
        // Its first frames may be cached from the reads above: read on to the stall.
        std::fs::read(&file)
    });
    let (r, took) = read.expect("a read of a stalled image hangs");
    let e = r.expect_err("a read of a stalled image succeeded");
    assert_eq!(e.raw_os_error(), Some(libc::EIO), "{e}");
    // Each request fails on its own timeout, but one read by a program can be several
    // requests: the kernel's read-ahead, then the page itself, then the program's retry
    // at the same offset (`read` reads on after a short read).
    assert!(took < 3 * TIMEOUT + SLACK, "failed after {took:?}");

    // 4. The other image still reads, start to end, while both stalled threads wait.
    let start = Instant::now();
    let mut seen = 0;
    for e in std::fs::read_dir(other.parent().unwrap()).unwrap() {
        seen += std::fs::read(e.unwrap().path()).unwrap().len();
    }
    assert!(seen > 0);
    assert!(start.elapsed() < TIMEOUT + SLACK, "the other image waited: {:?}", start.elapsed());

    release(&fifo_unlisted);
    release(&fifo_listed);
    drop(m);
}
