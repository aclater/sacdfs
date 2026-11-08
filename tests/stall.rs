// SPDX-License-Identifier: GPL-3.0-or-later

//! A stalled source (#63, #72): an image that stops answering must neither hang its
//! requests for good nor hold up the requests of other images, even while clients keep
//! retrying its reads through a long hang (#241).
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

/// The files with extension `ext` below `dir`, sorted.
fn files_below(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == ext) {
                found.push(p);
            }
        }
    }
    found.sort();
    found
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

/// WAV reads demultiplex in a pipeline with their conversion. That reading of the image must not
/// happen on a decoder thread: a read stalled there held the thread for as long as the source
/// hung, so a stalled image's WAV reads could take every decoder thread, and then no read of any
/// other image could decode. (With more threads than stalled reads, a healthy read failed only
/// sometimes: a decoder thread waiting for its own decoding can take up a stalled read.)
#[test]
fn stalled_wav_reads_hold_no_decoder_thread() {
    common::skip_without_fuse!();
    let corpus = common::corpus().join("images");
    let base = tempfile::tempdir().unwrap();
    let (src, links) = (base.path().join("src"), base.path().join("links"));
    std::fs::create_dir_all(&links).unwrap();
    for (dir, id) in [("Stalled", "dsd-stereo-mch-2048"), ("Other", "dst-features-2ch-6ch")] {
        std::fs::create_dir_all(src.join(dir)).unwrap();
        let outer = links.join(format!("{dir}.iso"));
        std::os::unix::fs::symlink(corpus.join(format!("{id}.iso")), &outer).unwrap();
        std::os::unix::fs::symlink(&outer, src.join(dir).join(format!("{id}.iso"))).unwrap();
    }
    // Two decoder threads, as a small NAS has: two stalled reads were enough to take them.
    let secs = TIMEOUT.as_secs().to_string();
    let m = Mount::new(&src, &["--threads", "2", "--request-timeout", &secs, "--no-index-cache"]);
    let stalled = files_below(&m.path.join("Stalled"), "wav");
    let other_dsf = files_below(&m.path.join("Other"), "dsf");
    let other_wav = files_below(&m.path.join("Other"), "wav");
    assert!(stalled.len() >= 2 && !other_dsf.is_empty() && !other_wav.is_empty());

    let fifo = base.path().join("stalled.fifo");
    stall(&links.join("Stalled.iso"), &fifo);
    // The image's open handle is checked against its path at most once a second.
    std::thread::sleep(Duration::from_millis(1500));

    // Reads of the stalled image, each on a thread of its own, wait or fail.
    let wedged: Vec<_> = stalled
        .into_iter()
        .take(2)
        .map(|f| {
            std::thread::spawn(move || {
                let _ = std::fs::read(&f);
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(500));
    assert!(wedged.iter().all(|t| !t.is_finished()), "the stalled image's reads did not stall");

    // The other image reads on, in both formats, DST decoding included.
    let start = Instant::now();
    for f in other_wav.iter().take(2).chain(other_dsf.iter().take(2)) {
        let f = f.clone();
        let shown = f.display().to_string();
        let (r, took) = timed(TIMEOUT + SLACK, move || std::fs::read(f)).expect("a healthy read hangs");
        assert!(r.is_ok_and(|d| d.len() > 92), "{shown}: a healthy read failed, after {took:?}");
    }
    assert!(start.elapsed() < TIMEOUT, "the other image waited: {:?}", start.elapsed());

    release(&fifo);
    for t in wedged {
        t.join().unwrap();
    }
    drop(m);
}

/// The request threads of the process `pid` (named `sacdfs-read-<i>`).
fn read_threads(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .map(|d| {
            d.filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
                .filter(|c| c.starts_with("sacdfs-read"))
                .count()
        })
        .unwrap_or(0)
}

/// Clients that retry the reads of a stalled image after each `EIO` took one more read thread
/// each time, as the blocked threads can't be freed, until all 64 were blocked and reads of
/// every other image failed too (#241: 64 in about 24 s with these settings). An image with
/// several reads blocked past the timeout now has its new reads failed at once, without a
/// thread.
///
/// *Why the threads are counted from when the cap holds, not from the start (#272):* how many
/// threads the hang takes before that depends on the kernel, not on sacdfs. Every read the
/// kernel sends within the timeout gets a thread, and for the clients' first reads it sends
/// its read-ahead too: up to three requests per file here, and the clients share five files.
/// Some of those requests are answered (the image's open handle still serves them) and their
/// threads stay idle for 10 s (`SPARE_IDLE`) before they end. That took 8 to 18 threads
/// locally and 15 on CI, against a fixed bound of 14. What #241 changed is what comes after:
/// once 4 reads are blocked past the timeout, no read of the image gets a thread, so the
/// count must not grow from then on, however long the clients retry. Before #257 it grew by
/// about one thread per client every timeout.
#[test]
fn retrying_clients_of_a_stalled_image_leave_threads_to_the_others() {
    common::skip_without_fuse!();
    use std::os::unix::fs::FileExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const CLIENTS: usize = 8;
    let corpus = common::corpus().join("images");
    let base = tempfile::tempdir().unwrap();
    let (src, links) = (base.path().join("src"), base.path().join("links"));
    std::fs::create_dir_all(&links).unwrap();
    for (dir, id) in [("Stalled", "dsd-stereo-mch-2048"), ("Other", "dst-features-2ch-6ch")] {
        std::fs::create_dir_all(src.join(dir)).unwrap();
        let outer = links.join(format!("{dir}.iso"));
        std::os::unix::fs::symlink(corpus.join(format!("{id}.iso")), &outer).unwrap();
        std::os::unix::fs::symlink(&outer, src.join(dir).join(format!("{id}.iso"))).unwrap();
    }
    let secs = TIMEOUT.as_secs().to_string();
    let m = Mount::new(&src, &["--threads", "2", "--request-timeout", &secs, "--no-index-cache"]);
    let pid = m.child.as_ref().unwrap().id();
    let stalled = files_below(&m.path.join("Stalled"), "wav");
    let mut other = files_below(&m.path.join("Other"), "dsf");
    other.truncate(2);
    other.extend(files_below(&m.path.join("Other"), "wav").into_iter().take(2));
    assert!(!stalled.is_empty() && other.len() == 4);
    let open: Vec<_> =
        (0..CLIENTS).map(|i| std::fs::File::open(&stalled[i % stalled.len()]).unwrap()).collect();

    let fifo = base.path().join("stalled.fifo");
    stall(&links.join("Stalled.iso"), &fifo);
    // The image's open handle is checked against its path at most once a second.
    std::thread::sleep(Duration::from_millis(1500));

    // Clients read the stalled image's files on, again at once after each error. (On, as a
    // page read once comes from the kernel's cache.)
    let stop = Arc::new(AtomicBool::new(false));
    let fast_eio = Arc::new(AtomicUsize::new(0));
    let clients: Vec<_> = open
        .into_iter()
        .map(|f| {
            let (stop, fast_eio) = (stop.clone(), fast_eio.clone());
            std::thread::spawn(move || {
                let len = f.metadata().unwrap().len();
                let mut buf = vec![0u8; 65536];
                let mut at = 0;
                while !stop.load(Ordering::Relaxed) {
                    let t = Instant::now();
                    let r = f.read_at(&mut buf, at);
                    at = (at + buf.len() as u64) % len;
                    if r.is_err() && t.elapsed() < Duration::from_millis(500) {
                        fast_eio.fetch_add(1, Ordering::Relaxed);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            })
        })
        .collect();
    // Long enough for the clients to have taken more than half of the 64 threads, had nothing
    // stopped them.
    let start = Instant::now();
    let until = start + 7 * TIMEOUT;
    // The cap holds once a read fails at once; a second later, any read the kernel sent before
    // that has its thread. Without the cap, no read fails at once: then counted from when it
    // should have held by, the timeout and the watchdog's slack.
    while fast_eio.load(Ordering::Relaxed) == 0 && start.elapsed() < TIMEOUT + SLACK {
        std::thread::sleep(Duration::from_millis(20));
    }
    let capped = fast_eio.load(Ordering::Relaxed) > 0;
    if capped {
        std::thread::sleep(Duration::from_secs(1));
    }
    let at_cap = read_threads(pid);
    let mut most = at_cap;
    while Instant::now() < until {
        most = most.max(read_threads(pid));
        std::thread::sleep(Duration::from_millis(100));
    }
    // Not one more thread while the clients retry: spare threads only end.
    assert!(
        most <= at_cap,
        "the retries took read threads: {at_cap} once the cap should hold, then up to {most}"
    );
    assert!(capped, "no read of the stalled image failed at once");

    // The other image reads on, in both formats, while the clients keep retrying.
    let start = Instant::now();
    for f in other {
        let shown = f.display().to_string();
        let (r, took) = timed(TIMEOUT + SLACK, move || std::fs::read(f)).expect("a healthy read hangs");
        assert!(r.is_ok_and(|d| d.len() > 92), "{shown}: a healthy read failed, after {took:?}");
    }
    assert!(start.elapsed() < TIMEOUT, "the other image waited: {:?}", start.elapsed());

    stop.store(true, Ordering::Relaxed);
    release(&fifo);
    for c in clients {
        c.join().unwrap();
    }
    drop(m);
}
