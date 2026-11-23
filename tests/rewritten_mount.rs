// SPDX-License-Identifier: GPL-3.0-or-later

//! An image rewritten in place with another disc under a live mount (#588): the file open on
//! the old disc fails with `ESTALE` (or `EIO`) instead of reading old and new bytes mixed, and
//! the folder lists the new disc within the refresh interval, without its own time moving.
//!
//! Needs `/dev/fuse` and `fusermount3`; skips without them like the other mount tests.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime};

use common::*;
use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::sidecar::Sidecars;
use sacdfs::track::Reader;
use sacdfs::tree::{Output, SourceDir, Tree};

const IMAGE: &str = "images/no-igl.iso";
const OTHER: &str = "images/no-text.iso";

struct Mount {
    child: Option<Child>,
    dir: tempfile::TempDir,
    _cache: tempfile::TempDir,
}

impl Mount {
    fn start(source: &Path) -> Mount {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .env("XDG_CACHE_HOME", cache.path())
            .arg("-f")
            .args(["--threads", "2"])
            .arg(source)
            .arg(dir.path())
            .spawn()
            .unwrap();
        let m = Mount { child: Some(child), dir, _cache: cache };
        let p = m.path();
        wait_for("the mount", || is_mounted(&p));
        m
    }

    fn path(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let p = self.path();
        if is_mounted(&p) {
            let _ = Command::new("fusermount3").arg("-u").arg("-z").arg(&p).status();
        }
        if let Some(mut c) = self.child.take() {
            common::stop_gracefully(&mut c);
        }
    }
}

/// The first regular file below `dir` that is a DSF track, in name order.
fn first_dsf(dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "dsf") {
            return Some(p);
        }
        if e.file_type().ok()?.is_dir() {
            if let Some(f) = first_dsf(&p) {
                return Some(f);
            }
        }
    }
    None
}

/// The first track file of `image` as the library shows it.
fn reference(image: &str) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(corpus().join(image), dir.path().join("disc.iso")).unwrap();
    let tree = Tree::with_options(
        SourceDir::scan(&dir.path().canonicalize().unwrap()).unwrap(),
        Tuning::default(),
        Output::dsf_only(),
    )
    .watching(&Sidecars::none());
    let (_, f) = tree.all_files().into_iter().next().unwrap();
    Reader::new(f.clone()).read(&Engine::new(1, 1 << 20).unwrap(), 0, f.size).unwrap().0
}

#[test]
fn an_image_rewritten_under_a_mount_goes_stale_then_lists_anew() {
    skip_without_fuse!();
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(lib.join("artist/album")).unwrap();
    let image = lib.join("artist/album/disc.iso");
    std::fs::copy(corpus().join(IMAGE), &image).unwrap();
    let other = std::fs::read(corpus().join(OTHER)).unwrap();
    let expected = reference(OTHER);
    let old = reference(IMAGE);
    assert_ne!(old, expected);

    let mount = Mount::start(&lib);
    let root = mount.path();
    let track = first_dsf(&root).expect("a track file");
    // Open, and read the head, so that frames are cached.
    let open = std::fs::File::open(&track).unwrap();
    let mut buf = vec![0u8; old.len()];
    open.read_exact_at(&mut buf[..4096], 0).unwrap();
    assert_eq!(&buf[..4096], &old[..4096]);
    let tail = (old.len() - 4096) as u64;
    let folder = std::fs::metadata(image.parent().unwrap()).unwrap().modified().unwrap();

    // Rewritten in place with another disc, the modification time put back.
    let mtime = std::fs::metadata(&image).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let f = std::fs::File::options().write(true).open(&image).unwrap();
    write_all_at(&f, &other, 0).unwrap();
    f.set_modified(mtime).unwrap();
    drop(f);
    assert_eq!(std::fs::metadata(image.parent().unwrap()).unwrap().modified().unwrap(), folder);

    // The kernel's page cache holds the old snapshot of what was read; it is dropped, as
    // memory pressure would, so that reads reach sacdfs.
    let drop_cache = || {
        nix::fcntl::posix_fadvise(&open, 0, 0, nix::fcntl::PosixFadviseAdvice::POSIX_FADV_DONTNEED).unwrap();
    };
    // The open file goes stale: every read from here on fails, none returns other bytes than
    // the old disc's (a read in the first second may still be served before it is looked at).
    let start = Instant::now();
    let err = loop {
        drop_cache();
        match open.read_at(&mut buf[..4096], tail) {
            Ok(n) => assert_eq!(&buf[..n], &old[old.len() - 4096..][..n], "mixed bytes"),
            Err(e) => break e,
        }
        assert!(start.elapsed() < Duration::from_secs(15), "the open file never went stale");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(matches!(err.raw_os_error(), Some(libc::ESTALE | libc::EIO)), "{err}");
    drop_cache();
    let again = open.read_at(&mut buf[..4096], tail - 4096).unwrap_err();
    assert!(matches!(again.raw_os_error(), Some(libc::ESTALE | libc::EIO)), "{again}");

    // The folder lists the new disc, and a new open reads it.
    wait_for("the new disc", || {
        first_dsf(&root).and_then(|t| std::fs::read(t).ok()).is_some_and(|b| b == expected)
    });
    assert_eq!(std::fs::metadata(image.parent().unwrap()).unwrap().modified().unwrap(), folder);
    let _ = SystemTime::now();
}
