// SPDX-License-Identifier: GPL-3.0-or-later

//! What the kernel asks of a mount, counted in fuser's request log (#61), and what `df` and
//! `stat` are told (#62, #64).
//!
//! These tests need `/dev/fuse` and `fusermount3`. Without them they skip (`common::skip!`
//! prints each one), unless `SACDFS_FUSE_TESTS=1` is set (as CI does), which makes a missing
//! FUSE a failure.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime};

use common::*;

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

/// `ls -l`, which must succeed without printing an error (#174).
fn ls_l(dir: &Path) {
    let out = Command::new("ls").arg("-l").arg(dir).output().unwrap();
    assert!(out.status.success() && out.stderr.is_empty(), "ls -l: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn probes_are_answered_once() {
    common::skip_without_fuse!();
    let mount = Mount::new(&corpus().join("images"), &["--threads", "2"]);
    let area = mount.path().join("dsd-stereo-mch-2048/Stereo");
    assert!(std::fs::read_dir(&area).unwrap().count() > 2);

    // Extended attributes (#174): none. getxattr answers ENOSYS, which the kernel remembers for
    // the whole mount: at most one GETXATTR ever (ENODATA brought back one per open through
    // Samba, about 3.4 per file in a Roon scan). listxattr answers an empty list (ENOSYS there
    // made NFS clients print "Unknown error 524" on `ls -l`), one LISTXATTR per entry listed.
    let entries = std::fs::read_dir(&area).unwrap().count();
    let files: Vec<PathBuf> = std::fs::read_dir(&area).unwrap().map(|e| e.unwrap().path()).collect();
    let scan = || {
        for p in &files {
            std::fs::metadata(p).unwrap();
            drop(std::fs::File::open(p).unwrap());
        }
    };
    assert_eq!(mount.requests("XATTR", scan), 0, "stat and open");
    assert_eq!(mount.requests("XATTR", scan), 0, "stat and open, again");
    let before = mount.count("GETXATTR");
    let n = mount.requests("LISTXATTR", || ls_l(&area));
    assert!(n <= 2 * (entries + 1), "ls -l: {n} LISTXATTR for {entries} entries");
    mount.requests("LISTXATTR", || ls_l(&area));
    // Named attributes, as Samba asks on every open: "not supported", from the kernel.
    let enotsup = |p: &Path, name: &str| {
        let out = Command::new("getfattr").args(["-n", name]).arg(p).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !out.status.success() && err.contains("Operation not supported"),
            "getfattr -n {name}: {err}"
        );
    };
    let have_getfattr = Command::new("getfattr").arg("--version").output().is_ok();
    if have_getfattr {
        mount.requests("GETXATTR", || {
            for _ in 0..2 {
                for p in &files {
                    enotsup(p, "security.capability");
                    enotsup(p, "user.x");
                }
            }
        });
    }
    let getxattr = mount.count("GETXATTR") - before;
    assert!(getxattr <= 1, "{getxattr} GETXATTR in all (ls -l twice, getfattr -n on every file)");
    if have_getfattr {
        // A dump lists nothing and prints no error.
        let out = Command::new("getfattr").arg("-d").arg(&files[0]).output().unwrap();
        assert!(out.status.success(), "getfattr -d: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout.is_empty() && out.stderr.is_empty());
    }

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

/// `df -B1` of `p`: size and available bytes.
fn df(p: &Path) -> (u64, u64) {
    let out = Command::new("df").args(["-B1", "--output=size,avail"]).arg(p).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let mut v = text.lines().nth(1).unwrap().split_whitespace().map(|s| s.parse::<u64>().unwrap());
    (v.next().unwrap(), v.next().unwrap())
}

#[test]
fn df_shows_the_images_size_full() {
    common::skip_without_fuse!();
    let images = corpus().join("images");
    let mut bytes = 0;
    let mut todo = vec![images.clone()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(d).unwrap() {
            let path = e.unwrap().path();
            let meta = std::fs::metadata(&path).unwrap();
            if meta.is_dir() {
                todo.push(path);
            } else if path.extension().is_some_and(|x| x == "iso") {
                bytes += meta.len();
            }
        }
    }
    let mount = Mount::new(&images, &[]);
    let (size, avail) = df(&mount.path());
    assert_eq!(avail, 0);
    // In blocks of 64 KiB. Images that aren't SACD images aren't counted (the corpus has some),
    // so at most that total.
    assert!(size > 0, "a 0-byte filesystem");
    assert!(size <= bytes.div_ceil(1 << 16) * (1 << 16), "{size} is over the images' {bytes}");
    assert!(size >= bytes / 2, "{size} is far under the images' {bytes}");
}

/// No images is not a 0-byte filesystem either (#62): the source's filesystem says the size.
#[test]
fn df_of_no_images_shows_the_source_size_full() {
    common::skip_without_fuse!();
    let empty = tempfile::tempdir().unwrap();
    let mount = Mount::new(empty.path(), &[]);
    let (size, avail) = df(&mount.path());
    assert_eq!(avail, 0);
    assert_eq!(size, df(empty.path()).0, "not the source's size");
}

#[test]
fn pcm_options_move_the_wav_mtime() {
    common::skip_without_fuse!();
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

/// A listing as a file server makes it (#205): the folder, then every entry's attributes,
/// then the same a while later. No track name is asked for again after the refresh interval
/// (an area is re-read for its folder art only), and no access check reaches sacdfs (the
/// kernel checks the modes itself). Samba changes directory for nearly every request, which
/// was one ACCESS each.
#[test]
fn a_listing_repeated_later_asks_for_no_track_again() {
    common::skip_without_fuse!();
    let top = tempfile::tempdir().unwrap();
    let src = top.path().canonicalize().unwrap();
    std::fs::create_dir_all(src.join("Album")).unwrap();
    std::os::unix::fs::symlink(corpus().join("images/dsd-stereo-mch-2048.iso"), src.join("Album/disc.iso"))
        .unwrap();
    let mount = Mount::new(&src, &["--threads", "2"]);
    let area = mount.path().join("Album/disc/Stereo");
    let list = || {
        let mut names = Vec::new();
        for e in std::fs::read_dir(&area).unwrap() {
            let e = e.unwrap();
            std::fs::symlink_metadata(e.path()).unwrap();
            names.push(e.file_name().into_string().unwrap());
        }
        names
    };
    let tracks: Vec<String> = list().into_iter().filter(|n| n.ends_with(".dsf")).collect();
    assert!(!tracks.is_empty());
    let track_lookups = || tracks.iter().map(|t| mount.count(&format!("LOOKUP name {t:?}"))).sum::<usize>();
    let first = track_lookups();
    // Past the refresh interval (2 s), after which the folders above are asked for again.
    std::thread::sleep(Duration::from_secs(3));
    let access = mount.requests(" ACCESS ", || {
        list();
        let st = Command::new("sh")
            .arg("-c")
            .arg("cd \"$1\" && test -r \"$2\" && test ! -w \"$2\" && test ! -x \"$2\"")
            .arg("sh")
            .arg(&area)
            .arg(&tracks[0])
            .status()
            .unwrap();
        assert!(st.success(), "access as before: readable, not writable or executable");
    });
    let again = track_lookups() - first;
    println!("track lookups: {first} on the first listing, {again} on the second");
    assert_eq!(again, 0, "track names looked up again");
    assert_eq!(access, 0, "access checks reached sacdfs");
}

/// A folder listed again comes from the kernel's cache (#205), also one that is re-read for
/// its art (an area) and the folders above it, until its listing changes: art added to the
/// image's folder shows at the first listing after the refresh interval (2 s), and art
/// removed goes. Samba and macOS's client open the same folder many times in one browse; each
/// listing was a `READDIRPLUS` and a `READDIR` more.
#[test]
fn a_repeated_listing_comes_from_the_kernel_until_the_folder_changes() {
    common::skip_without_fuse!();
    let top = tempfile::tempdir().unwrap();
    let src = top.path().canonicalize().unwrap();
    std::fs::create_dir_all(src.join("Album")).unwrap();
    std::os::unix::fs::symlink(corpus().join("images/dsd-stereo-mch-2048.iso"), src.join("Album/disc.iso"))
        .unwrap();
    std::fs::write(src.join("Album/folder.jpg"), b"\xff\xd8\xff\xe0 one").unwrap();
    let mount = Mount::new(&src, &["--threads", "2"]);
    let dirs = [mount.path(), mount.path().join("Album"), mount.path().join("Album/disc/Stereo")];
    let names = |d: &Path| {
        let mut n: Vec<String> =
            std::fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        n.sort();
        n
    };
    let area = &dirs[2];
    let first: Vec<Vec<String>> = dirs.iter().map(|d| names(d)).collect();
    assert!(first[2].iter().any(|n| n == "folder.jpg"), "{:?}", first[2]);
    let reads = mount.requests("READDIR", || {
        for _ in 0..3 {
            for (d, was) in dirs.iter().zip(&first) {
                assert_eq!(&names(d), was);
            }
        }
    });
    assert_eq!(reads, 0, "a folder listed again was read from sacdfs again");

    // Art added: the area's listing is made again, and the kernel's copy dropped.
    std::fs::write(src.join("Album/back.jpg"), b"\xff\xd8\xff\xe0 two").unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert!(names(area).iter().any(|n| n == "back.jpg"), "added art not shown: {:?}", names(area));
    let reads = mount.requests("READDIR", || {
        names(area);
    });
    assert_eq!(reads, 0, "the new listing was not kept");
    // Art removed: gone at the next listing after the interval.
    std::fs::remove_file(src.join("Album/back.jpg")).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert!(!names(area).iter().any(|n| n == "back.jpg"), "removed art still shown");
    assert_eq!(names(area), first[2]);
}
