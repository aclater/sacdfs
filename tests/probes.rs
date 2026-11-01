// SPDX-License-Identifier: GPL-3.0-or-later

//! What the kernel asks of a mount, counted in fuser's request log (#61), and what `df` and
//! `stat` are told (#62, #64).
//!
//! These tests need `/dev/fuse` and `fusermount3`. Without them they pass without doing
//! anything, unless `SACDFS_FUSE_TESTS=1` is set (as CI does), which makes a missing FUSE
//! a failure.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use common::*;

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

/// A foreground sacdfs whose fuser request log goes to a file.
struct Mount {
    child: Option<Child>,
    dir: tempfile::TempDir,
    log: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Mount {
    fn new(source: &Path, extra: &[&str]) -> Mount {
        let dir = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("requests.log");
        let child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .env("XDG_CACHE_HOME", tmp.path())
            .env("RUST_LOG", "fuser::request=debug")
            .arg("-f")
            .args(extra)
            .arg(source)
            .arg(dir.path())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let m = Mount { child: Some(child), dir, log, _tmp: tmp };
        let p = m.path();
        let start = Instant::now();
        while !is_mounted(&p) {
            assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for the mount");
            std::thread::sleep(Duration::from_millis(20));
        }
        m
    }

    fn path(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    /// The requests logged so far whose line contains `what`.
    fn count(&self, what: &str) -> usize {
        std::fs::read_to_string(&self.log).unwrap().lines().filter(|l| l.contains(what)).count()
    }

    /// Runs `f`, then waits until no request has been logged for a moment, and returns how
    /// many requests containing `what` it caused.
    fn requests(&self, what: &str, f: impl FnOnce()) -> usize {
        let before = self.count(what);
        f();
        let mut len = 0;
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let now = std::fs::metadata(&self.log).unwrap().len();
            if now == len {
                break;
            }
            len = now;
        }
        self.count(what) - before
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let p = self.path();
        if is_mounted(&p) {
            let _ = Command::new("fusermount3").arg("-u").arg(&p).status();
        }
        if let Some(mut c) = self.child.take() {
            let _ = c.wait_timeout_or_kill();
        }
    }
}

trait WaitOrKill {
    fn wait_timeout_or_kill(&mut self) -> std::io::Result<()>;
}

impl WaitOrKill for Child {
    fn wait_timeout_or_kill(&mut self) -> std::io::Result<()> {
        let start = Instant::now();
        while self.try_wait()?.is_none() {
            if start.elapsed() > Duration::from_secs(10) {
                let _ = self.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.wait().map(|_| ())
    }
}

fn ls_l(dir: &Path) {
    let st = Command::new("ls").arg("-l").arg(dir).stdout(Stdio::null()).status().unwrap();
    assert!(st.success());
}

#[test]
fn probes_are_answered_once() {
    if !fuse_available() {
        return;
    }
    let mount = Mount::new(&corpus().join("images"), &["--threads", "2"]);
    let area = mount.path().join("dsd-stereo-mch-2048/Stereo");
    assert!(std::fs::read_dir(&area).unwrap().count() > 2);

    // Extended attributes: the first ENOSYS tells the kernel there are none, and a second
    // listing asks nothing (it sent one LISTXATTR per file before).
    mount.requests("XATTR", || ls_l(&area));
    assert_eq!(mount.requests("LISTXATTR", || ls_l(&area)), 0, "second ls -l");
    assert_eq!(mount.requests("XATTR", || ls_l(&area)), 0, "third ls -l");

    // Missing names: one LOOKUP each, then the kernel's negative entry answers.
    for name in ["cover.jpg", "folder.jpg", "desktop.ini", ".hidden"] {
        let lookup = format!("LOOKUP name {name:?}");
        let p = area.join(name);
        let n = mount.requests(&lookup, || {
            for _ in 0..3 {
                assert_eq!(std::fs::metadata(&p).unwrap_err().kind(), std::io::ErrorKind::NotFound);
            }
        });
        assert_eq!(n, 1, "{name}: lookups");
    }
    // Names that exist are still found, and `..` of a directory still answers.
    let first = std::fs::read_dir(&area).unwrap().next().unwrap().unwrap().path();
    assert!(std::fs::metadata(&first).unwrap().is_file());
    assert!(area.join("..").join("Stereo").is_dir());
}

#[test]
fn df_shows_the_source_size_full() {
    if !fuse_available() {
        return;
    }
    let images = corpus().join("images");
    let mount = Mount::new(&images, &[]);
    let df = |p: &Path| -> (u64, u64) {
        let out = Command::new("df").args(["-B1", "--output=size,avail"]).arg(p).output().unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        let mut v = text.lines().nth(1).unwrap().split_whitespace().map(|s| s.parse::<u64>().unwrap());
        (v.next().unwrap(), v.next().unwrap())
    };
    let (size, avail) = df(&mount.path());
    assert!(size > 0, "a 0-byte filesystem");
    assert_eq!(avail, 0);
    assert_eq!(size, df(&images).0, "not the source's size");
}

#[test]
fn pcm_options_move_the_wav_mtime() {
    if !fuse_available() {
        return;
    }
    let images = corpus().join("images");
    let stats = |extra: &[&str]| -> Vec<(String, u64, SystemTime)> {
        let mount = Mount::new(&images, extra);
        let area = mount.path().join("dsd-stereo-mch-2048/Stereo");
        let mut v: Vec<_> = std::fs::read_dir(&area)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let m = e.metadata().unwrap();
                (e.file_name().into_string().unwrap(), m.len(), m.modified().unwrap())
            })
            .collect();
        v.sort();
        v
    };
    let image = std::fs::metadata(images.join("dsd-stereo-mch-2048.iso")).unwrap().modified().unwrap();
    let base = stats(&[]);
    let shown = |name: &str| {
        let format =
            if name.ends_with(".dsf") { sacdfs::track::Format::Dsf } else { sacdfs::track::Format::Wav };
        image + sacdfs::ident::version_offset(format)
    };
    assert!(
        base.iter().all(|f| f.2 == shown(&f.0)),
        "default options: the image's mtime, moved by the output version"
    );
    // Another mount with the same options: the same mtimes.
    assert_eq!(stats(&[]), base);
    // A gain keeps every size and moves only the WAV files' mtime, forward.
    let gain = stats(&["--pcm-gain", "-3"]);
    assert_eq!(gain.len(), base.len());
    for (a, b) in base.iter().zip(&gain) {
        assert_eq!((&a.0, a.1), (&b.0, b.1));
        if a.0.ends_with(".wav") {
            assert!(b.2 > a.2, "{}: mtime not moved", a.0);
        } else {
            assert_eq!(a.2, b.2, "{}", a.0);
        }
    }
}
