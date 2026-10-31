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
use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::pcm::{PcmConfig, PcmRate};
use sacdfs::track::Reader;
use sacdfs::tree::{Output, SourceDir, Tree};

/// The corpus read with the library instead of through the kernel.
struct Library {
    tree: Tree,
    engine: Engine,
}

impl Library {
    /// The corpus as the binary shows it by default, with WAV files at `rate`.
    fn new(rate: PcmRate) -> Library {
        let out = Output { samples_per_frame: rate.samples_per_frame() as u32, ..Output::default() };
        let tree =
            Tree::with_options(SourceDir::scan(&corpus().join("images")).unwrap(), Tuning::default(), out);
        let engine = Engine::with_pcm(2, 64 << 20, PcmConfig { rate, ..PcmConfig::default() }).unwrap();
        Library { tree, engine }
    }

    /// The file at `path` under the mount at `root`.
    fn read(&self, root: &Path, path: &Path) -> Vec<u8> {
        let ino = self.tree.resolve(path.strip_prefix(root).unwrap()).unwrap();
        let Some(node) = self.tree.get(ino) else { panic!("no node") };
        let sacdfs::tree::Node::File(f) = &*node else { panic!("not a file") };
        let (data, _) = Reader::new(f.clone()).read(&self.engine, 0, f.size).unwrap();
        data
    }
}

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
    let library = Library::new(PcmRate::R176400);
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
        let mut shown_wav = BTreeMap::new();
        for area in std::fs::read_dir(&disc).unwrap() {
            let area = area.unwrap();
            assert!(area.file_type().unwrap().is_dir());
            let aname = area.file_name().into_string().unwrap();
            for f in std::fs::read_dir(area.path()).unwrap() {
                let f = f.unwrap();
                let name = f.file_name().into_string().unwrap();
                let key = (aname.clone(), track_number(&name));
                if name.ends_with(".dsf") {
                    shown.insert(key, f.path());
                } else {
                    assert!(name.ends_with(".wav"), "{name}");
                    shown_wav.insert(key, f.path());
                }
            }
        }
        assert_eq!(shown.keys().collect::<Vec<_>>(), want.keys().collect::<Vec<_>>(), "{id}: files shown");
        // A WAV file next to every DSF file, with the same name but for the extension.
        assert_eq!(shown.keys().collect::<Vec<_>>(), shown_wav.keys().collect::<Vec<_>>(), "{id}: WAV files");
        for (key, path) in &shown_wav {
            assert_eq!(path.with_extension(""), shown[key].with_extension(""));
            let data = std::fs::read(path).unwrap();
            assert_eq!(std::fs::metadata(path).unwrap().len(), data.len() as u64);
            let w = parse_wav(&data);
            assert_eq!(w.rate, 176_400);
            assert_eq!(&data[..], &library.read(&root, path)[..], "{id} {path:?}");
            wholes.push((path.strip_prefix(&root).unwrap().to_owned(), data));
        }
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
    let files: Vec<String> = std::fs::read_dir(mp.join("dsd-stereo-mch-2048/Stereo"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(files.iter().any(|n| n.ends_with(".dsf")) && files.iter().any(|n| n.ends_with(".wav")));
    // Read-only.
    assert!(std::fs::write(mp.join("x"), b"x").is_err());
    let st = Command::new("fusermount3").arg("-u").arg(&mp).status().unwrap();
    assert!(st.success());
    wait_for("the unmount", || !is_mounted(&mp));
}

/// `--format` shows one format only; `--pcm-rate` sets the WAV files' rate.
#[test]
fn format_and_rate_options() {
    if !fuse_available() {
        return;
    }
    let images = corpus().join("images");
    let mount = Mount::foreground(&images, &["--format", "wav", "--pcm-rate", "88200", "--pcm-cutoff", "30"]);
    let root = mount.path();
    let area = root.join("dsd-stereo-mch-2048/Multichannel");
    let mut names: Vec<_> =
        std::fs::read_dir(&area).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    assert!(!names.is_empty());
    assert!(names.iter().all(|n| n.ends_with(".wav")), "{names:?}");
    let path = area.join(&names[0]);
    let data = std::fs::read(&path).unwrap();
    let w = parse_wav(&data);
    assert_eq!(w.rate, 88_200);
    assert_eq!(w.mask.count_ones(), u32::from(w.channels));
    // The library with the same rate (the cutoff changes only the audio) gives the same
    // layout.
    let lib = Library::new(PcmRate::R88200).read(&root, &path);
    assert_eq!(lib.len(), data.len());
    assert_eq!(lib[..w.data.start], data[..w.data.start]);
    mount.stop("TERM");

    let mount = Mount::foreground(&images, &["--format", "dsf"]);
    let area = mount.path().join("dsd-stereo-mch-2048/Stereo");
    assert!(
        std::fs::read_dir(&area).unwrap().all(|e| e.unwrap().file_name().to_string_lossy().ends_with(".dsf"))
    );
    mount.stop("TERM");
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
    for bad in [
        ["--format", "flac"],
        ["--format", ""],
        ["--pcm-rate", "44100"],
        ["--pcm-cutoff", "50"],
        ["--pcm-gain", "30"],
        ["--pcm-gain", "NaN"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .args(bad)
            .arg(corpus().join("images"))
            .arg(dir.path())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{bad:?} accepted");
    }
    // A negative gain is a value, not an option.
    let out =
        Command::new(env!("CARGO_BIN_EXE_sacdfs")).args(["--pcm-gain", "-6", "--help"]).output().unwrap();
    assert!(out.status.success());
}
