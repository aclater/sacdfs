// SPDX-License-Identifier: GPL-3.0-or-later

//! The mount's start-up walk leaves no malloc arenas behind (#217): the walk reads the top-level
//! folders of SOURCE on several threads, and glibc gives each thread that allocates an arena of
//! its own (a 64 MiB reservation), which keeps what it freed for itself. On a library of 414
//! images those arenas held about 22 MiB of resident memory more than a single arena. The
//! arena limit (#73) has to be in force before the walk, not after it.
//!
//! Needs `/dev/fuse`, `fusermount3` and glibc, like `tests/mount.rs`.

#![cfg(all(target_os = "linux", target_env = "gnu"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Top-level folders in the test source: more than the walk's threads.
const FOLDERS: usize = 40;
/// The size glibc reserves for one non-main arena's heap.
const ARENA_HEAP: u64 = 64 << 20;

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

/// How many non-main arena heaps `/proc/PID/maps` shows: a mapping that starts on a 64 MiB
/// boundary, readable and writable, followed at once by an inaccessible one that makes up the
/// 64 MiB with it.
fn arena_heaps(pid: u32) -> usize {
    let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
    let rows: Vec<(u64, u64, String)> = maps
        .lines()
        .map(|l| {
            let mut f = l.split_whitespace();
            let (a, b) = f.next().unwrap().split_once('-').unwrap();
            let perms = f.next().unwrap().to_owned();
            (u64::from_str_radix(a, 16).unwrap(), u64::from_str_radix(b, 16).unwrap(), perms)
        })
        .collect();
    rows.windows(2)
        .filter(|w| {
            let (s, e, p) = (&w[0].0, &w[0].1, &w[0].2);
            s % ARENA_HEAP == 0 && p == "rw-p" && w[1].2 == "---p" && w[1].0 == *e && w[1].1 - s == ARENA_HEAP
        })
        .count()
}

#[test]
fn the_start_up_walk_leaves_no_arenas() {
    if !fuse_available() {
        return;
    }
    // A user's own setting of the limit takes the choice away from sacdfs (and from this test).
    if std::env::var_os("MALLOC_ARENA_MAX").is_some() || std::env::var_os("GLIBC_TUNABLES").is_some() {
        eprintln!("skipping: the arena limit is set in the environment");
        return;
    }
    let source = tempfile::tempdir().unwrap();
    let image = common::corpus().join("images/dsd-stereo-mch-2048.iso");
    for i in 0..FOLDERS {
        let dir = source.path().join(format!("artist {i:02}")).join(format!("album {i:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(&image, dir.join("disc.iso")).unwrap();
        std::fs::write(dir.join("folder.jpg"), b"\xff\xd8\xff\xd9").unwrap();
    }
    let mount = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let path: PathBuf = mount.path().canonicalize().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .env("XDG_CACHE_HOME", cache.path())
        .arg("-f")
        .arg(source.path())
        .arg(&path)
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !is_mounted(&path) {
        assert!(start.elapsed() < Duration::from_secs(60), "timed out waiting for the mount");
        std::thread::sleep(Duration::from_millis(20));
    }
    let heaps = arena_heaps(child.id());
    let _ = Command::new("fusermount3").arg("-u").arg("-z").arg(&path).status();
    common::stop_gracefully(&mut child);
    assert_eq!(heaps, 0, "the walk left {heaps} malloc arenas: the limit must come before it");
}
