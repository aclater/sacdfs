// SPDX-License-Identifier: GPL-3.0-or-later

//! No heap growth across mounts (#151): N cycles of mount → list → read → re-read (an image
//! and a sidecar added, then removed, #101) → tags reload (#81, #121) → unmount, all in this
//! process, with dhat-rs counting every allocation. After a warm-up cycle (process-wide
//! state set up once: the rayon pool, the tags store, the handle cache's map), the heap
//! still live after each unmount must stay flat.
//!
//! *Why in-process:* dhat sees only its own process's allocations, so the filesystem runs
//! here on a [`fuser::BackgroundSession`] and the kernel's requests come back to it, as with
//! the binary. *Why one test in the file:* dhat allows one profiler per process.
//!
//! The long-running counterparts (hours of churn, heaptrack, LeakSanitizer) are in
//! `scripts/soak.py` and `scripts/leak-check.sh`, off PR CI: see docs/development.md.
//!
//! Needs `/dev/fuse` and `fusermount3`, like `tests/mount.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fuser::{Config, MountOption, Session};
use sacdfs::disc::{Disc, Tuning};
use sacdfs::engine::Engine;
use sacdfs::fuse::SacdFs;
use sacdfs::sidecar::Sidecars;
use sacdfs::tree::{Output, SourceDir, Tree};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Mount cycles; the first is the warm-up.
const CYCLES: usize = 8;

/// Heap growth allowed from the end of cycle 2 to the end of the last, in bytes: allocator
/// noise (a thread that has not quite exited, a lazily grown map) but far less than one
/// cycle's tree, discs and read buffers, which are hundreds of KiB.
const SLACK_BYTES: u64 = 16 << 10;

/// The request timeout: its watchdog thread wakes every half second, and ends at the first
/// wake after the unmount.
const WATCHDOG: Duration = Duration::from_secs(2);

const IMAGE: &str = "dsd-stereo-mch-2048";
const CHURN: &str = "dst-features-2ch-6ch";

fn link(image: &str, to: &Path) {
    std::os::unix::fs::symlink(common::corpus().join("images").join(format!("{image}.iso")), to).unwrap();
}

/// Every file below `dir`, read whole, in name order.
fn read_all(dir: &Path) -> usize {
    let mut names: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    let mut n = 0;
    for p in names {
        if p.is_dir() {
            n += read_all(&p);
        } else {
            n += fs::read(&p).unwrap().len();
        }
    }
    n
}

/// Waits until `cond` holds, for up to 10 s.
fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One cycle: mount `lib` on `mp`, list and read everything, re-read after churn, reload the
/// tags file, unmount.
fn cycle(i: usize, lib: &Path, mp: &Path, tags_file: &Path) {
    let root = SourceDir::scan_with(lib, &Sidecars::standard(), Some(mp)).unwrap();
    let tree = Tree::with_options(root, Tuning::default(), Output::dsf_only())
        .with_tag_watch(None)
        .watching(&Sidecars::standard())
        .with_refresh(Duration::ZERO);
    let engine = Arc::new(Engine::new(2, 8 << 20).unwrap());
    // The files belong to whoever owns the test's directories: this user.
    let meta = fs::metadata(lib).unwrap();
    let (uid, gid) = (meta.uid(), meta.gid());
    let tree = Arc::new(tree);
    let fs =
        SacdFs::new(tree, engine.clone(), 4, uid, gid).unwrap().with_request_timeout(Some(WATCHDOG)).unwrap();
    let mut config = Config::default();
    config.mount_options = vec![MountOption::RO, MountOption::FSName("sacdfs-no-growth".to_owned())];
    config.n_threads = Some(2);
    let session = Session::new(fs, mp, &config).unwrap().spawn().unwrap();

    let album = mp.join(IMAGE);
    wait_for("the mount", || album.is_dir());
    // List and read, then read again (the second read is served from the frame cache).
    assert!(read_all(mp) > 0);
    assert!(read_all(mp) > 0);

    // Re-read (#101): an image and a sidecar appear, are read, and go.
    link(CHURN, &lib.join(format!("{CHURN}.iso")));
    fs::write(lib.join("folder.jpg"), format!("cycle {i}\n")).unwrap();
    wait_for("the new image", || mp.join(CHURN).is_dir() && mp.join("folder.jpg").exists());
    read_all(mp);
    fs::remove_file(lib.join(format!("{CHURN}.iso"))).unwrap();
    fs::remove_file(lib.join("folder.jpg")).unwrap();
    wait_for("the image to go", || !mp.join(CHURN).exists() && !mp.join("folder.jpg").exists());

    // Tags reload (#121): the file changes while mounted; the root's attributes run the sweep.
    fs::write(tags_file, format!("[track.1]\ntitle = \"Cycle {i} edited, a longer title\"\n")).unwrap();
    let _ = fs::metadata(mp);
    read_all(&album);

    session.umount_and_join().unwrap();
    // The DST decoders are pooled process-wide (#150), up to as many as ever decoded at once:
    // a high-water mark set by how the reads happened to overlap, not growth. Emptied here, as
    // an idle mount does, so that only what the mount itself left behind is counted.
    assert!(engine.release_if_idle(Duration::ZERO));
    drop(engine);
}

/// The live heap, once background threads of the last mount (the watchdog's, the spare
/// request threads) have ended: the lowest of a few samples over more than one watchdog tick.
fn settled_bytes() -> u64 {
    let mut low = u64::MAX;
    for _ in 0..7 {
        std::thread::sleep(Duration::from_millis(100));
        low = low.min(dhat::HeapStats::get().curr_bytes as u64);
    }
    low
}

#[test]
fn no_heap_growth_across_mount_cycles() {
    common::skip_without_fuse!();
    let _profiler = dhat::Profiler::builder().testing().build();
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let mp = dir.path().join("mnt");
    let tags = dir.path().join("tags");
    for d in [&lib, &mp, &tags] {
        fs::create_dir_all(d).unwrap();
    }
    link(IMAGE, &lib.join(format!("{IMAGE}.iso")));
    sacdfs::tags::install(Some(sacdfs::tags::Store::new(&tags)));
    let key = {
        let disc = Disc::open(&lib.join(format!("{IMAGE}.iso")), 0, Tuning::default()).unwrap();
        sacdfs::tags::disc_key(disc.toc())
    };
    let tags_file = tags.join(format!("{key}.toml"));

    let mut after = Vec::new();
    for i in 0..CYCLES {
        fs::write(&tags_file, format!("[track.1]\ntitle = \"Cycle {i}\"\n")).unwrap();
        cycle(i, &lib, &mp, &tags_file);
        after.push(settled_bytes());
    }
    eprintln!("live heap after each cycle (bytes): {after:?}");
    let base = after[1];
    let last = after[CYCLES - 1];
    assert!(
        last <= base + SLACK_BYTES,
        "the heap grows across mount cycles: {base} bytes after cycle 2, {last} after cycle {CYCLES} \
         (allowed {SLACK_BYTES} more); per cycle: {after:?}"
    );
}
