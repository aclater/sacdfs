// SPDX-License-Identifier: GPL-3.0-or-later

//! The cache of open image files (#26): a walk of the whole corpus keeps at most
//! `--max-open-images` images open, and reads the same bytes as with every image open.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::*;
use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::handles;
use sacdfs::track::Reader;
use sacdfs::tree::{Output, SourceDir, Tree};

/// File descriptors of process `pid` open on a corpus image.
fn image_fds(pid: &str) -> usize {
    let images = corpus().join("images").canonicalize().unwrap();
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|d| {
            d.filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
                .filter(|t| t.starts_with(&images))
                .count()
        })
        .unwrap_or(0)
}

fn tree() -> Tree {
    Tree::with_options(
        SourceDir::scan(&corpus().join("images")).unwrap(),
        Tuning::default(),
        Output::default(),
    )
}

/// Every file of the corpus read whole, then pieces of all of them interleaved (so that
/// handles are closed and opened again in the middle of a file), with at most 3 images
/// open: the same bytes as a walk with the default bound, and never more than 3 images
/// open between reads.
#[test]
fn library_walk_stays_within_the_bound() {
    const MAX: usize = 3;
    // The default bound holds every corpus image: the reference.
    let reference: Vec<Vec<u8>> = {
        let t = tree();
        let engine = Engine::new(2, 0).unwrap();
        t.all_files()
            .iter()
            .map(|(_, f)| Reader::new(f.clone()).read(&engine, 0, f.size).unwrap().0)
            .collect()
    };
    handles::global().configure(MAX, handles::DEFAULT_IDLE);
    let t = tree();
    let files = t.all_files();
    assert_eq!(files.len(), reference.len());
    assert!(handles::global().open_count() <= MAX);
    assert!(image_fds("self") <= MAX, "{} images open after listing", image_fds("self"));
    let images: std::collections::BTreeSet<_> = files.iter().map(|(p, _)| p.components().next()).collect();
    assert!(images.len() > 4 * MAX, "the corpus has too few images for this test");
    // No frame cache, so every read goes to the image.
    let engine = Engine::new(2, 0).unwrap();
    for ((path, f), want) in files.iter().zip(&reference) {
        let got = Reader::new(f.clone()).read(&engine, 0, f.size).unwrap().0;
        assert!(got == *want, "{}: differs", path.display());
        assert!(image_fds("self") <= MAX, "{} images open", image_fds("self"));
    }
    let readers: Vec<_> = files.iter().map(|(_, f)| Reader::new(f.clone())).collect();
    let mut at = vec![0u64; files.len()];
    let piece = 20_000;
    let opens = handles::global().opens();
    loop {
        let mut any = false;
        for (i, r) in readers.iter().enumerate() {
            if at[i] >= r.file.size {
                continue;
            }
            any = true;
            let got = r.read(&engine, at[i], piece).unwrap().0;
            let end = (at[i] + piece).min(r.file.size) as usize;
            assert!(
                got == reference[i][at[i] as usize..end],
                "{}: differs at {}",
                files[i].0.display(),
                at[i]
            );
            at[i] += piece;
            assert!(handles::global().open_count() <= MAX);
            assert!(image_fds("self") <= MAX, "{} images open", image_fds("self"));
        }
        if !any {
            break;
        }
    }
    assert!(handles::global().opens() > opens, "the interleaved walk reopened no image");
}

fn fuse_available() -> bool {
    let ok = Path::new("/dev/fuse").exists()
        && Command::new("fusermount3").arg("-V").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok();
    if !ok && std::env::var_os("SACDFS_FUSE_TESTS").is_some_and(|v| v == "1") {
        panic!("SACDFS_FUSE_TESTS=1 but /dev/fuse or fusermount3 is missing");
    }
    ok
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Through the kernel, with `--max-open-images 2`: list everything, read the header, some
/// audio and the end of every file, and sacdfs never holds more than 2 images open.
#[test]
fn mount_walk_stays_within_the_bound() {
    if !fuse_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .args(["-f", "--no-index-cache", "--max-open-images", "2"])
        .arg(corpus().join("images"))
        .arg(dir.path())
        .spawn()
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    let pid = child.id().to_string();
    wait_for("the mount", || {
        std::fs::read_to_string("/proc/self/mountinfo")
            .unwrap_or_default()
            .lines()
            .any(|l| l.split(' ').nth(4) == root.to_str())
    });
    let mut most = 0;
    let mut files = 0;
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let f = std::fs::File::open(&p).unwrap();
            let len = f.metadata().unwrap().len();
            let mut buf = vec![0u8; 8192];
            for off in [0, len / 2, len.saturating_sub(4096)] {
                f.read_at(&mut buf, off).unwrap();
            }
            files += 1;
            most = most.max(image_fds(&pid));
        }
        most = most.max(image_fds(&pid));
    }
    let st = Command::new("fusermount3").arg("-u").arg(&root).status().unwrap();
    assert!(st.success());
    assert!(child.wait().unwrap().success());
    assert!(files > 100, "{files} files");
    assert!(most <= 2, "sacdfs held {most} images open");
}
