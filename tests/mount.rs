// SPDX-License-Identifier: GPL-3.0-or-later

//! Mounts the corpus with the sacdfs binary and reads it through the kernel.
//!
//! These tests need `/dev/fuse` and `fusermount3`. Without them they pass without doing
//! anything, unless `SACDFS_FUSE_TESTS=1` is set (as CI does), which makes a missing FUSE
//! a failure.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
    // Field 5 is the mount point; after " - " come the type ("fuse" or "fuse.sacdfs",
    // depending on how it was mounted) and the source.
    info.lines().any(|l| {
        l.split(' ').nth(4) == Some(mp) && l.split(" - ").nth(1).is_some_and(|t| t.starts_with("fuse"))
    })
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Mount {
    child: Option<Child>,
    dir: tempfile::TempDir,
}

impl Mount {
    fn path(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn foreground(source: &Path, extra: &[&str]) -> Mount {
        let dir = tempfile::tempdir().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .arg("-f")
            .args(extra)
            .arg(source)
            .arg(dir.path())
            .spawn()
            .unwrap();
        let m = Mount { child: Some(child), dir };
        let p = m.path();
        wait_for("the mount", || is_mounted(&p));
        m
    }

    /// Stops sacdfs with `signal` and waits for it to unmount and exit.
    fn stop(mut self, signal: &str) {
        let mut child = self.child.take().unwrap();
        let st = Command::new("kill").arg(format!("-{signal}")).arg(child.id().to_string()).status().unwrap();
        assert!(st.success());
        let status = child.wait().unwrap();
        assert!(status.success(), "sacdfs exited with {status}");
        assert!(!is_mounted(&self.path()), "still mounted after {signal}");
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let p = self.path();
        if is_mounted(&p) {
            let _ = Command::new("fusermount3").arg("-u").arg("-z").arg(&p).status();
        }
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[test]
fn corpus_through_the_kernel() {
    if !fuse_available() {
        return;
    }
    let mount = Mount::foreground(&corpus().join("images"), &["--threads", "2"]);
    let root = mount.path();
    let mut checked = 0;
    let mut wholes = Vec::new();
    for case in cases() {
        let id = case["id"].as_str().unwrap();
        let disc = root.join(id);
        let expected = &case["expected"];
        if expected["outcome"] == "reject" {
            assert!(!disc.exists(), "{id}: rejected image is shown");
            continue;
        }
        let mut want = BTreeMap::new();
        for area in expected["audio"].as_array().unwrap() {
            let dir = area_dir(area["area"].as_str().unwrap());
            for t in area["tracks"].as_array().unwrap() {
                if t["outcome"] == "ok" {
                    want.insert((dir.to_owned(), t["number"].as_u64().unwrap() as u32), t.clone());
                }
            }
        }
        let mut shown = BTreeMap::new();
        for area in std::fs::read_dir(&disc).unwrap() {
            let area = area.unwrap();
            assert!(area.file_type().unwrap().is_dir());
            let aname = area.file_name().into_string().unwrap();
            for f in std::fs::read_dir(area.path()).unwrap() {
                let f = f.unwrap();
                let name = f.file_name().into_string().unwrap();
                assert!(name.ends_with(".dsf"), "{name}");
                shown.insert((aname.clone(), track_number(&name)), f.path());
            }
        }
        assert_eq!(shown.keys().collect::<Vec<_>>(), want.keys().collect::<Vec<_>>(), "{id}: files shown");
        for (key, t) in &want {
            let path = &shown[key];
            let data = std::fs::read(path).unwrap();
            assert_eq!(std::fs::metadata(path).unwrap().len(), data.len() as u64);
            let hashes: Vec<String> = dsf_channels(&data).iter().map(|c| sha256(c)).collect();
            let expected: Vec<String> =
                t["sha256"].as_array().unwrap().iter().map(|h| h.as_str().unwrap().to_owned()).collect();
            assert_eq!(hashes, expected, "{id} {path:?}");
            wholes.push((path.strip_prefix(&root).unwrap().to_owned(), data));
            checked += 1;
        }
    }
    assert!(checked > 100);
    mount.stop("TERM");

    // A second mount, cold: reads at scattered offsets, through fresh opens, equal the
    // bytes of the straight reads.
    let mount = Mount::foreground(&corpus().join("images"), &[]);
    let root = mount.path();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    for (path, whole) in wholes.iter().step_by(3) {
        let f = std::fs::File::open(root.join(path)).unwrap();
        for _ in 0..4 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let off = seed % whole.len() as u64;
            let len = (seed >> 40) as usize % 200_000 + 1;
            let mut buf = vec![0u8; len];
            let n = f.read_at(&mut buf, off).unwrap();
            let end = (off as usize + len).min(whole.len());
            assert_eq!(&buf[..n], &whole[off as usize..end], "{path:?} at {off}");
        }
    }
    mount.stop("INT");
}

/// Every file under `dir`, relative to it, sorted.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            } else {
                out.push(e.path().strip_prefix(dir).unwrap().to_owned());
            }
        }
    }
    out.sort();
    out
}

/// `--flatten` on a library-like tree of corpus images: single-image chains collapse below
/// the top level, everything else is kept, and the files are the same files.
#[test]
fn flatten_through_the_kernel() {
    if !fuse_available() {
        return;
    }
    let src = tempfile::tempdir().unwrap();
    let link = |path: &str, id: &str| {
        let p = src.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(corpus().join("images").join(format!("{id}.iso")), p).unwrap();
    };
    link("Artist/2001 - Album [SACD]/Artist - Album (rip)/Album/disc.iso", "dsd-stereo-mch-2048");
    link("Artist/2002 - Box/Disc 1.iso", "full-text");
    link("Artist/2002 - Box/Bonus/bonus.iso", "no-text");
    link("Solo/Only/only.iso", "dst-features-2ch-6ch");
    link("Pair/two-a.iso", "dsd-stereo-mch-2054");
    link("Pair/two-b.iso", "dsd-stereo-mch-2064");
    std::fs::write(src.path().join("Artist/2001 - Album [SACD]/cover.txt"), b"x").unwrap();

    let plain = Mount::foreground(src.path(), &[]);
    let plain_files = files_under(&plain.path());
    let plain_sizes: Vec<u64> =
        plain_files.iter().map(|p| std::fs::metadata(plain.path().join(p)).unwrap().len()).collect();
    plain.stop("TERM");

    let mount = Mount::foreground(src.path(), &["--flatten"]);
    let root = mount.path();
    let ls = |p: &str| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(root.join(p))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    };
    assert_eq!(ls(""), ["Artist", "Pair", "Solo"]);
    assert_eq!(ls("Artist"), ["2001 - Album [SACD]", "2002 - Box"]);
    assert_eq!(ls("Artist/2001 - Album [SACD]"), ["Multichannel", "Stereo"]);
    assert_eq!(ls("Artist/2002 - Box"), ["Bonus", "Disc 1"]);
    assert_eq!(ls("Artist/2002 - Box/Bonus"), ["Stereo"]);
    assert_eq!(ls("Solo"), ["Only"]);
    assert_eq!(ls("Pair"), ["two-a", "two-b"]);
    // `..` of an area leads back to the collapsed folder.
    let stereo = root.join("Artist/2001 - Album [SACD]/Stereo");
    assert_eq!(std::fs::canonicalize(stereo.join("..")).unwrap(), root.join("Artist/2001 - Album [SACD]"));

    // The same files, with the same sizes and bytes, under shorter paths.
    let files = files_under(&root);
    assert_eq!(files.len(), plain_files.len());
    let sizes: Vec<u64> = files.iter().map(|p| std::fs::metadata(root.join(p)).unwrap().len()).collect();
    let mut a = sizes.clone();
    let mut b = plain_sizes;
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b);
    let case = cases().into_iter().find(|c| c["id"] == "dsd-stereo-mch-2048").unwrap();
    let first = &case["expected"]["audio"][0]["tracks"][0];
    let name = ls("Artist/2001 - Album [SACD]/Stereo").into_iter().next().unwrap();
    let data = std::fs::read(stereo.join(name)).unwrap();
    let hashes: Vec<String> = dsf_channels(&data).iter().map(|c| sha256(c)).collect();
    let expected: Vec<String> =
        first["sha256"].as_array().unwrap().iter().map(|h| h.as_str().unwrap().to_owned()).collect();
    assert_eq!(hashes, expected);
    // Inode numbers stay put for the mount's lifetime.
    use std::os::unix::fs::MetadataExt;
    let ino = |p: &Path| std::fs::metadata(p).unwrap().ino();
    let before: Vec<u64> = files.iter().map(|p| ino(&root.join(p))).collect();
    let after: Vec<u64> = files_under(&root).iter().map(|p| ino(&root.join(p))).collect();
    assert_eq!(before, after);
    mount.stop("TERM");
}

#[test]
fn background_mount_and_unmount() {
    if !fuse_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mp = dir.path().canonicalize().unwrap();
    let image = corpus().join("images/dsd-stereo-mch-2048.iso");
    let status = Command::new(env!("CARGO_BIN_EXE_sacdfs")).arg(&image).arg(&mp).status().unwrap();
    assert!(status.success());
    // The parent returns once the daemon has mounted.
    assert!(is_mounted(&mp));
    let names: Vec<String> = std::fs::read_dir(mp.join("dsd-stereo-mch-2048"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["Stereo", "Multichannel"]);
    // Read-only.
    assert!(std::fs::write(mp.join("x"), b"x").is_err());
    let st = Command::new("fusermount3").arg("-u").arg(&mp).status().unwrap();
    assert!(st.success());
    wait_for("the unmount", || !is_mounted(&mp));
}

#[test]
fn bad_arguments_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .arg("/nonexistent/source")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("/nonexistent/source"));
    let out = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .arg(corpus().join("images"))
        .arg("/nonexistent/mountpoint")
        .output()
        .unwrap();
    assert!(!out.status.success());
}
