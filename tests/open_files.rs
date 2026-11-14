// SPDX-License-Identifier: GPL-3.0-or-later

//! Open files: what one client opens does not take the descriptors the images need, and an
//! image that fails to open for a passing reason is not left out of its folder for good.
//! Also: an image path replaced by a FIFO does not hang a listing.
//!
//! The mount tests need `/dev/fuse` and `fusermount3`, like `tests/mount.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeSet;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::*;
use sacdfs::disc::Tuning;
use sacdfs::sidecar::Sidecars;
use sacdfs::tree::{Output, SourceDir, Tree};

/// The hard open files limit sacdfs runs under here: low enough that the opens below would
/// use it up if nothing bounded them.
const NOFILE: u32 = 256;

/// The soft limit it starts with, which it raises to [`NOFILE`].
const SOFT: u32 = 128;

/// The bound on open sidecar files under [`NOFILE`] with the default 64 image files:
/// `sacdfs::vfs::sidecar_slots::cap_for(256, 64)`.
const SIDECAR_CAP: usize = 64;

/// Opens of one sidecar file tried, more than [`NOFILE`].
const OPENS: usize = 400;

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
    /// sacdfs on `source`, with its open files limit at [`SOFT`], hard [`NOFILE`].
    fn limited(source: &Path) -> Mount {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        let child = Command::new("sh")
            .arg("-c")
            .arg(format!("ulimit -Sn {SOFT} && ulimit -Hn {NOFILE} && exec \"$0\" \"$@\""))
            .arg(env!("CARGO_BIN_EXE_sacdfs"))
            .env("XDG_CACHE_HOME", cache.path())
            .args(["-f", "--no-allow-other"])
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

/// The names in directory `dir`.
fn names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect()
}

/// The soft and hard open files limits of process `pid`.
fn nofile_of(pid: u32) -> (u64, u64) {
    let limits = std::fs::read_to_string(format!("/proc/{pid}/limits")).unwrap();
    let line = limits.lines().find(|l| l.starts_with("Max open files")).unwrap();
    let v: Vec<u64> = line.split_whitespace().filter_map(|w| w.parse().ok()).collect();
    (v[0], v[1])
}

/// A client holding many opens of one sidecar file used up sacdfs's descriptors: the images
/// of a folder listed then failed to open, were left out, and stayed out after the client
/// closed its files. Now sacdfs raises its soft limit to the hard one, and open sidecar files
/// are bounded below the limit: the opens past the bound fail with `EMFILE`, and every image
/// is listed and reads, during the opens and after.
#[test]
fn held_sidecar_opens_stop_at_a_bound_and_leave_the_images_listed_and_reading() {
    common::skip_without_fuse!();
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    let (a, b) = (src.join("A"), src.join("B"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    // A: every corpus image; B: one image with a cover.
    let mut want = BTreeSet::new();
    for case in cases() {
        let id = case["id"].as_str().unwrap();
        std::os::unix::fs::symlink(image(&case), a.join(format!("{id}.iso"))).unwrap();
        if case["expected"]["outcome"] != "reject" {
            want.insert(id.to_owned());
        }
    }
    assert!(want.len() > 100);
    let id = "dsd-stereo-mch-2048";
    std::os::unix::fs::symlink(corpus().join(format!("images/{id}.iso")), b.join(format!("{id}.iso")))
        .unwrap();
    std::fs::write(b.join("cover.jpg"), b"front").unwrap();

    let m = Mount::limited(&src);
    let pid = m.child.as_ref().unwrap().id();
    assert_eq!(nofile_of(pid), (u64::from(NOFILE), u64::from(NOFILE)), "soft limit not raised");
    let cover = m.path.join("B/cover.jpg");
    let mut held = Vec::new();
    let mut refused = None;
    for _ in 0..OPENS {
        match std::fs::File::open(&cover) {
            Ok(f) => held.push(f),
            Err(e) => {
                refused = Some(e);
                break;
            }
        }
    }
    // Listed for the first time while the opens are held.
    let during = names(&m.path.join("A"));
    let first_track = |root: &Path| {
        let stereo = root.join("A").join(id).join("Stereo");
        let first = std::fs::read_dir(&stereo)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "dsf"))
            .min()
            .unwrap();
        dsf_channels(&std::fs::read(first).unwrap()).iter().map(|c| sha256(c)).collect::<Vec<_>>()
    };
    let case = cases().into_iter().find(|c| c["id"] == id).unwrap();
    let hashes: Vec<String> = case["expected"]["audio"][0]["tracks"][0]["sha256"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.as_str().unwrap().to_owned())
        .collect();
    let audio_during = during.contains(id).then(|| first_track(&m.path));
    // Every held open still reads.
    for f in held.iter().step_by(37) {
        let mut buf = [0u8; 5];
        f.read_exact_at(&mut buf, 0).unwrap();
        assert_eq!(&buf, b"front");
    }
    let opened = held.len();
    drop(held);
    // Open again once closed.
    assert_eq!(std::fs::read(&cover).unwrap(), b"front");
    let after = names(&m.path.join("A"));
    let missing = |shown: &BTreeSet<String>| want.difference(shown).cloned().collect::<Vec<_>>();
    let refused = refused.unwrap_or_else(|| panic!("all {OPENS} opens of the cover held: no bound"));
    assert_eq!(refused.raw_os_error(), Some(libc::EMFILE), "{refused}");
    assert_eq!(opened, SIDECAR_CAP, "opens held before the bound refused one");
    assert_eq!(missing(&during), Vec::<String>::new(), "images left out while the cover was held open");
    assert_eq!(audio_during, Some(hashes.clone()), "audio while the cover was held open");
    assert_eq!(missing(&after), Vec::<String>::new(), "images left out after the cover was closed");
    assert_eq!(first_track(&m.path), hashes, "audio after the cover was closed");
}

/// Runs `f` on a thread of its own: its result, or `None` if it has not finished after `within`.
fn within<T: Send + 'static>(within: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(within).ok()
}

/// An image whose path became a FIFO after the scan (a link pointed at one) is refused at
/// once, as not a regular file, instead of the open waiting for a writer for good; the other
/// images of the folder are listed.
#[test]
fn a_fifo_in_place_of_an_image_does_not_hang_a_listing() {
    let base = tempfile::tempdir().unwrap();
    let (src, links) = (base.path().join("src"), base.path().join("links"));
    let album = src.join("Album");
    std::fs::create_dir_all(&album).unwrap();
    std::fs::create_dir_all(&links).unwrap();
    let images = corpus().join("images");
    for (name, id) in [("good", "dsd-stereo-mch-2048"), ("swapped", "dsd-1ch")] {
        let outer = links.join(format!("{name}.iso"));
        std::os::unix::fs::symlink(images.join(format!("{id}.iso")), &outer).unwrap();
        std::os::unix::fs::symlink(&outer, album.join(format!("{name}.iso"))).unwrap();
    }
    let tree = Tree::with_options(
        SourceDir::scan_with(&src, &Sidecars::standard(), None).unwrap(),
        Tuning::default(),
        Output::default(),
    );
    // After the scan, before the listing: the link now names a FIFO.
    let fifo = base.path().join("swapped.fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR).unwrap();
    let tmp = links.join("swapped.tmp");
    std::os::unix::fs::symlink(&fifo, &tmp).unwrap();
    std::fs::rename(&tmp, links.join("swapped.iso")).unwrap();

    let listed = within(Duration::from_secs(20), move || {
        let album = tree.resolve(Path::new("Album")).unwrap();
        let mut v: Vec<String> =
            tree.children(album).unwrap().iter().map(|e| e.name.to_string_lossy().into_owned()).collect();
        v.sort();
        v
    });
    // A writer lets a blocked open go on, so that the thread ends either way.
    let _w = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(&fifo);
    assert_eq!(listed.expect("the listing hangs on the FIFO"), ["good"]);
}
