// SPDX-License-Identifier: GPL-3.0-or-later

//! The bound on listed images through the kernel (#237): a mount that keeps only a few images in
//! memory shows the same tree, numbers and bytes as one that keeps all of them, and a file left
//! open while its image's neighbours are dropped goes on reading the same bytes.
//!
//! Needs `/dev/fuse` and `fusermount3`, like `tests/mount.rs`. (The same comparison without a
//! mount, with `--flatten` and with readers racing, is in `src/tree.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fuser::{Config, MountOption, Session};
use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::fuse::SacdFs;
use sacdfs::memory::{self, Cgroup};
use sacdfs::sidecar::Sidecars;
use sacdfs::tree::{Output, SourceDir, Tree};

const IMAGES: [&str; 3] = ["dsd-stereo-mch-2048", "dst-features-2ch-6ch", "album-set-disc-2-of-3"];

/// A library of `n` links to corpus images, two to a folder.
fn library(n: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..n {
        let folder = dir.path().join(format!("A{:02}", i / 2));
        fs::create_dir_all(&folder).unwrap();
        let image = common::corpus().join("images").join(format!("{}.iso", IMAGES[i % IMAGES.len()]));
        std::os::unix::fs::symlink(image, folder.join(format!("D{i:03}.iso"))).unwrap();
    }
    dir
}

struct Mount {
    child: std::process::Child,
    at: tempfile::TempDir,
    _cache: tempfile::TempDir,
}

impl Mount {
    fn new(source: &Path, extra: &[&str]) -> Mount {
        let at = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .env("XDG_CACHE_HOME", cache.path())
            .arg("-f")
            .args(extra)
            .arg(source)
            .arg(at.path())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while fs::metadata(at.path().join("A00")).is_err() {
            assert!(start.elapsed() < Duration::from_secs(30), "no mount");
            std::thread::sleep(Duration::from_millis(20));
        }
        Mount { child, at, _cache: cache }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        common::unmount_retrying(self.at.path());
        common::stop_gracefully(&mut self.child);
    }
}

/// Every path below `root`, relative, with its inode number, size and mtime; files with a hash
/// of their bytes, if `read`.
fn walk(root: &Path, read: bool) -> BTreeMap<PathBuf, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_owned()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let m = fs::metadata(e.path()).unwrap();
            let hash = if read && m.is_file() {
                common::sha256(&fs::read(e.path()).unwrap())
            } else {
                String::new()
            };
            let rel = e.path().strip_prefix(root).unwrap().to_owned();
            out.insert(rel, format!("{} {} {} {hash}", m.ino(), m.size(), m.mtime()));
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out
}

/// The number of files the file system says it holds.
fn nodes(root: &Path) -> u64 {
    let out = Command::new("stat").args(["-f", "-c", "%c"]).arg(root).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

#[test]
fn a_tiny_bound_serves_the_same_tree_numbers_and_bytes() {
    if !common::fuse_available() {
        common::report_skip("a_tiny_bound_serves_the_same_tree_numbers_and_bytes", "no FUSE");
        return;
    }
    let lib = library(90);
    let free = Mount::new(lib.path(), &["--listing-mb", "0"]);
    let want = walk(free.at.path(), true);
    assert!(want.len() > 90 * 3, "{}", want.len());
    let tiny = Mount::new(lib.path(), &["--listing-mb", "1", "--cache-mb", "8"]);
    // Whatever the kernel remembers, each pass goes back to sacdfs for what its TTL ended.
    assert_eq!(walk(tiny.at.path(), true), want);
    // Images were dropped: the file system's count of nodes is the tree's.
    assert!(nodes(tiny.at.path()) < nodes(free.at.path()) / 2);
    assert_eq!(walk(tiny.at.path(), false), walk(free.at.path(), false));
    assert_eq!(walk(tiny.at.path(), true), want);
}

#[test]
fn a_file_stays_open_and_reads_the_same_while_images_are_dropped() {
    if !common::fuse_available() {
        common::report_skip("a_file_stays_open_and_reads_the_same_while_images_are_dropped", "no FUSE");
        return;
    }
    let lib = library(30);
    let m = Mount::new(lib.path(), &["--listing-mb", "1"]);
    let folder = m.at.path().join("A00/D000");
    let area = fs::read_dir(&folder).unwrap().next().unwrap().unwrap().path();
    let path = fs::read_dir(&area)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "dsf"))
        .unwrap();
    let whole = fs::read(&path).unwrap();
    let f = fs::File::open(&path).unwrap();
    let mut chunk = vec![0; whole.len().min(1 << 16)];
    for i in 0..3 {
        walk(m.at.path(), false);
        let at = (i * whole.len() / 3) as u64;
        let n = f.read_at(&mut chunk, at).unwrap();
        assert!(n > 0);
        assert_eq!(&chunk[..n], &whole[at as usize..at as usize + n], "pass {i}");
    }
}

/// A shortage of memory reaches a client as `ENOMEM` (#237), never as a missing file, and passes:
/// the memory watch on a cgroup whose `memory.stat` this test writes, with the file system mounted
/// in this process.
#[test]
fn a_shortage_fails_new_requests_with_enomem_and_passes() {
    if !common::fuse_available() {
        common::report_skip("a_shortage_fails_new_requests_with_enomem_and_passes", "no FUSE");
        return;
    }
    const LIMIT: u64 = 100 << 20;
    let lib = library(30);
    let cgroup = tempfile::tempdir().unwrap();
    let stat = cgroup.path().join("memory.stat");
    let use_percent = |pc: u64| {
        let new = cgroup.path().join("new");
        fs::write(&new, format!("anon {}\nfile 0\nshmem 0\n", LIMIT * pc / 100)).unwrap();
        fs::rename(&new, &stat).unwrap();
    };
    use_percent(10);
    let scan = SourceDir::scan_with(lib.path(), &Sidecars::default(), None).unwrap();
    let tree = Arc::new(
        Tree::with_options(scan, Tuning::default(), Output::default()).with_listing_budget(Some(1 << 20)),
    );
    let engine = Arc::new(Engine::new(2, 8 << 20).unwrap());
    let watcher = {
        let (cg, tree, engine) = (Cgroup::at(cgroup.path().to_owned(), LIMIT), tree.clone(), engine.clone());
        std::thread::spawn(move || memory::watch(&cg, &tree, &engine))
    };
    let at = tempfile::tempdir().unwrap();
    let (uid, gid) = (fs::metadata(lib.path()).unwrap().uid(), fs::metadata(lib.path()).unwrap().gid());
    let sfs = SacdFs::new(tree.clone(), engine, 4, uid, gid).unwrap();
    let mut config = Config::default();
    config.mount_options = vec![MountOption::RO, MountOption::FSName("sacdfs-test".to_owned())];
    let session = Session::new(sfs, at.path(), &config).unwrap().spawn().unwrap();

    let wait_until = |refusing: bool| {
        let start = Instant::now();
        while tree.refusing() != refusing {
            assert!(start.elapsed() < Duration::from_secs(10), "the watch didn't act");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let enomem = |e: std::io::Error| assert_eq!(e.raw_os_error(), Some(libc::ENOMEM), "{e}");

    // A file open before the shortage, and one known but not open.
    let dsf = |image: &str| -> PathBuf {
        let area = fs::read_dir(at.path().join(image)).unwrap().next().unwrap().unwrap().path();
        let files = fs::read_dir(&area).unwrap();
        files.map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|e| e == "dsf")).unwrap()
    };
    let (open, known) = (dsf("A00/D000"), dsf("A01/D002"));
    let whole = fs::read(&open).unwrap();
    let held = fs::File::open(&open).unwrap();
    let not_listed = at.path().join("A10/D020");

    // Short of memory, but below the point of refusing, all is served.
    use_percent(90);
    std::thread::sleep(Duration::from_millis(600));
    assert!(!tree.refusing());
    assert_eq!(fs::read(&known).unwrap().len() as u64, fs::metadata(&known).unwrap().len());

    use_percent(95);
    wait_until(true);
    // New listings and opens fail with ENOMEM, as a client's retry can tell from "missing".
    enomem(fs::File::open(&known).unwrap_err());
    let listed = fs::read_dir(&not_listed).and_then(|mut d| d.next().transpose());
    enomem(listed.unwrap_err());
    // The file already open reads on.
    let mut chunk = vec![0; whole.len().min(1 << 16)];
    let n = held.read_at(&mut chunk, 0).unwrap();
    assert_eq!(&chunk[..n], &whole[..n]);

    // Once memory is back, the same requests work: nothing was remembered as missing.
    use_percent(10);
    wait_until(false);
    assert_eq!(fs::read(&known).unwrap().len() as u64, fs::metadata(&known).unwrap().len());
    assert!(fs::read_dir(&not_listed).unwrap().next().is_some());

    drop(held);
    session.umount_and_join().unwrap();
    // A cgroup that can't be read ends the watch, with nothing left refused.
    use_percent(95);
    wait_until(true);
    fs::remove_file(&stat).unwrap();
    watcher.join().unwrap();
    assert!(!tree.refusing());
}
