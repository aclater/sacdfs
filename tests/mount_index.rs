// SPDX-License-Identifier: GPL-3.0-or-later

//! The persistent seek index through the kernel: mount, seek, unmount, remount, seek
//! again; the second seek reads less of the image.
//!
//! Like `mount.rs`, this needs `/dev/fuse` and `fusermount3`, and passes without them
//! unless `SACDFS_FUSE_TESTS=1` is set.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::*;

fn fuse_available() -> bool {
    let ok = Path::new("/dev/fuse").exists()
        && Command::new("fusermount3").arg("-V").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok();
    if !ok && std::env::var_os("SACDFS_FUSE_TESTS").is_some_and(|v| v == "1") {
        panic!("SACDFS_FUSE_TESTS=1 but /dev/fuse or fusermount3 is missing");
    }
    ok
}

fn is_mounted(mountpoint: &Path) -> bool {
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mp = mountpoint.to_str().unwrap();
    info.lines().any(|l| l.split(' ').nth(4) == Some(mp))
}

/// Mounts `image` with `args`, reads `len` bytes at `fraction` of its first stereo file,
/// stops sacdfs with SIGTERM, and returns the bytes and the image reads it logged.
fn mount_and_seek(image: &Path, args: &[&str], fraction: f64, len: usize) -> (Vec<u8>, u64, u64) {
    let mp = tempfile::tempdir().unwrap();
    let mpath = mp.path().canonicalize().unwrap();
    let log = tempfile::NamedTempFile::new().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .args(["-f", "-v"])
        .args(args)
        .arg(image)
        .arg(&mpath)
        .env("RUST_LOG", "info")
        .stderr(log.reopen().unwrap())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !is_mounted(&mpath) {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for the mount");
        std::thread::sleep(Duration::from_millis(20));
    }
    let stem = image.file_stem().unwrap();
    let dir = mpath.join(stem).join("Stereo");
    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    let f = std::fs::File::open(&names[0]).unwrap();
    let size = f.metadata().unwrap().len();
    let off = ((size as f64 * fraction) as u64) & !4095;
    let mut buf = vec![0u8; len];
    let n = f.read_at(&mut buf, off).unwrap();
    buf.truncate(n);
    drop(f);
    let st = Command::new("kill").arg("-TERM").arg(child.id().to_string()).status().unwrap();
    assert!(st.success());
    assert!(child.wait().unwrap().success());
    assert!(!is_mounted(&mpath));
    let text = std::fs::read_to_string(log.path()).unwrap();
    let line =
        text.lines().find(|l| l.contains("image reads for audio:")).unwrap_or_else(|| panic!("{text}"));
    let nums: Vec<u64> = line
        .rsplit(':')
        .next()
        .unwrap()
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap())
        .collect();
    (buf, nums[0], nums[1])
}

#[test]
fn remount_seeks_with_the_saved_index() {
    if !fuse_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let image = corpus().join("images/dst-invalid-frames-2ch.iso");
    let cache = dir.path().join("index");
    let c = cache.to_str().unwrap();
    let (cold, cold_calls, cold_bytes) = mount_and_seek(&image, &["--index-cache", c], 0.6, 64 << 10);
    let entries: Vec<_> = std::fs::read_dir(&cache).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let name = entries[0].to_str().unwrap();
    assert!(name.ends_with(".idx") && !name.contains("dst"), "{name}");
    let (warm, warm_calls, warm_bytes) = mount_and_seek(&image, &["--index-cache", c], 0.6, 64 << 10);
    eprintln!("cold: {cold_calls} calls, {cold_bytes} bytes; warm: {warm_calls} calls, {warm_bytes} bytes");
    assert!(!cold.is_empty());
    assert_eq!(cold, warm);
    assert!(warm_bytes < cold_bytes, "warm {warm_bytes} bytes, cold {cold_bytes}");
    assert!(warm_calls < cold_calls, "warm {warm_calls} calls, cold {cold_calls}");

    // Without the cache, a remount seeks cold again, and nothing is written.
    let other = dir.path().join("unused");
    let (again, calls, bytes) = mount_and_seek(&image, &["--no-index-cache"], 0.6, 64 << 10);
    assert_eq!(again, cold);
    assert_eq!((calls, bytes), (cold_calls, cold_bytes));
    assert!(!other.exists());
}
