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
    /// The persistent seek index's directory (`XDG_CACHE_HOME`), so that tests never
    /// write to the user's.
    _cache: tempfile::TempDir,
}

impl Mount {
    fn path(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn foreground(source: &Path, extra: &[&str]) -> Mount {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .env("XDG_CACHE_HOME", cache.path())
            .arg("-f")
            .args(extra)
            .arg(source)
            .arg(dir.path())
            .spawn()
            .unwrap();
        let m = Mount { child: Some(child), dir, _cache: cache };
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

/// Folder art, scans and playlists next to an image are served byte for byte (#79, #75),
/// with and without `--flatten`; other files are not, nor a link into the mount (#67); the
/// audio files are the same as without them.
#[test]
fn sidecars_through_the_kernel() {
    if !fuse_available() {
        return;
    }
    let top = tempfile::tempdir().unwrap();
    let src = top.path().canonicalize().unwrap();
    let album = src.join("Artist/Album");
    std::fs::create_dir_all(album.join("Artwork")).unwrap();
    std::os::unix::fs::symlink(corpus().join("images/dsd-stereo-mch-2048.iso"), album.join("disc.iso"))
        .unwrap();
    // Larger than one FUSE read (1 MiB), not a multiple of a page.
    let art: Vec<u8> = (0..3_000_001u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    std::fs::write(album.join("folder.jpg"), &art).unwrap();
    std::fs::write(album.join("Artwork/back.png"), b"back").unwrap();
    std::fs::write(album.join("notes.txt"), b"not passed").unwrap();
    // The mount point inside SOURCE, and a link into it: never followed.
    let mnt = src.join("mnt");
    std::fs::create_dir(&mnt).unwrap();
    std::os::unix::fs::symlink("../../mnt/Artist/Album/folder.jpg", album.join("loop.jpg")).unwrap();
    let mount = |extra: &[&str]| {
        let cache = tempfile::tempdir().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .env("XDG_CACHE_HOME", cache.path())
            .arg("-f")
            .args(extra)
            .arg(&src)
            .arg(&mnt)
            .spawn()
            .unwrap();
        wait_for("the mount", || is_mounted(&mnt));
        (child, cache)
    };
    let stop = |(mut child, _cache): (Child, tempfile::TempDir)| {
        let st = Command::new("kill").arg("-TERM").arg(child.id().to_string()).status().unwrap();
        assert!(st.success());
        assert!(child.wait().unwrap().success());
        assert!(!is_mounted(&mnt));
    };
    let ls = |p: &str| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(mnt.join(p))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    };

    // Without sidecars: only the image, as before; its first track, for later.
    let m = mount(&["--sidecars", "none"]);
    assert_eq!(ls("Artist"), ["Album"]);
    assert_eq!(ls("Artist/Album"), ["disc"]);
    let track = ls("Artist/Album/disc/Stereo").into_iter().find(|n| n.ends_with(".dsf")).unwrap();
    let dsf = std::fs::read(mnt.join("Artist/Album/disc/Stereo").join(&track)).unwrap();
    stop(m);
    // A playlist whose entries are relative paths inside the mount.
    std::fs::write(src.join("Artist/list.m3u8"), format!("#EXTM3U\nAlbum/disc/Stereo/{track}\n")).unwrap();

    let m = mount(&[]);
    assert_eq!(ls("Artist"), ["Album", "list.m3u8"]);
    assert_eq!(ls("Artist/Album"), ["Artwork", "disc", "folder.jpg"]);
    assert_eq!(ls("Artist/Album/Artwork"), ["back.png"]);
    let shown = mnt.join("Artist/Album/folder.jpg");
    assert_eq!(std::fs::read(&shown).unwrap(), art);
    let (a, b) = (std::fs::metadata(&shown).unwrap(), std::fs::metadata(album.join("folder.jpg")).unwrap());
    assert_eq!(a.len(), b.len());
    assert_eq!(a.modified().unwrap(), b.modified().unwrap());
    assert!(a.permissions().readonly());
    assert_eq!(std::fs::read(mnt.join("Artist/Album/Artwork/back.png")).unwrap(), b"back");
    let list = std::fs::read_to_string(mnt.join("Artist/list.m3u8")).unwrap();
    for entry in list.lines().filter(|l| !l.starts_with('#')) {
        assert!(mnt.join("Artist").join(entry).is_file(), "{entry}");
    }
    // The audio is the same file as without sidecars.
    assert_eq!(std::fs::read(mnt.join("Artist/Album/disc/Stereo").join(&track)).unwrap(), dsf);
    assert!(std::fs::File::create(mnt.join("Artist/Album/new.jpg")).is_err());
    stop(m);

    // --flatten: next to the areas of the collapsed folder.
    let m = mount(&["--flatten"]);
    assert_eq!(ls("Artist/Album"), ["Artwork", "Multichannel", "Stereo", "folder.jpg"]);
    assert_eq!(std::fs::read(mnt.join("Artist/Album/folder.jpg")).unwrap(), art);
    stop(m);
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
    // Retried: the kernel can hold the mount busy for a moment after `read_dir` closes (#41).
    unmount_retrying(&mp);
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

    // The wide cutoff at 352.8 kHz mounts and reads.
    let mount =
        Mount::foreground(&images, &["--format", "wav", "--pcm-rate", "352800", "--pcm-cutoff", "wide"]);
    let area = mount.path().join("dsd-stereo-mch-2048/Stereo");
    let f = std::fs::read_dir(&area).unwrap().next().unwrap().unwrap().path();
    assert_eq!(parse_wav(&std::fs::read(&f).unwrap()).rate, 352_800);
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
        ["--pcm-cutoff", "80"],
        ["--pcm-gain", "30"],
        ["--pcm-gain", "NaN"],
        ["--sidecars", "iso"],
        ["--sidecars", "a/b"],
        ["--sidecars", "-none"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .args(bad)
            .arg(corpus().join("images"))
            .arg(dir.path())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{bad:?} accepted");
    }
    // The wide cutoff needs 176.4 or 352.8 kHz, and says so.
    let out = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .args(["--pcm-cutoff", "wide", "--pcm-rate", "88200"])
        .arg(corpus().join("images"))
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--pcm-cutoff wide needs --pcm-rate"));
    // A negative gain is a value, not an option.
    let out =
        Command::new(env!("CARGO_BIN_EXE_sacdfs")).args(["--pcm-gain", "-6", "--help"]).output().unwrap();
    assert!(out.status.success());
}

/// The FUSE mounts on `mountpoint`: more than one when one is mounted over another.
fn fuse_mounts_on(mountpoint: &Path) -> usize {
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mp = mountpoint.to_str().unwrap();
    info.lines()
        .filter(|l| {
            l.split(' ').nth(4) == Some(mp) && l.split(" - ").nth(1).is_some_and(|t| t.starts_with("fuse"))
        })
        .count()
}

/// Waits for `child` to exit, for up to 30 s, and returns whether it succeeded.
fn wait_exit(child: &mut Child, what: &str) -> bool {
    let start = Instant::now();
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s.success();
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("{what} did not exit");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A sacdfs restarted on the same mount point while the previous one is still there (a
/// service manager or a script restarting it) waits for it to go instead of mounting over
/// it, and the previous one's unmount, or its detach while a file is open, leaves the new
/// mount alone (#51). Before, the new mount went on top, inside the old one, and the old
/// one's unmount by path removed it.
#[test]
// Both children are waited for, the old one on one path or the other.
#[allow(clippy::zombie_processes)]
fn a_restart_on_the_same_path_survives_the_old_sacdfs() {
    if !fuse_available() {
        return;
    }
    let image = corpus().join("images/dsd-stereo-mch-2048.iso");
    for busy in [false, true] {
        let mut old = Mount::foreground(&image, &[]);
        let at = old.path();
        let file = at
            .join(files_under(&at).into_iter().find(|p| p.extension().is_some_and(|e| e == "dsf")).unwrap());
        let whole = std::fs::read(&file).unwrap();
        let open = busy.then(|| std::fs::File::open(&file).unwrap());
        let cache = tempfile::tempdir().unwrap();
        let mut new = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .env("XDG_CACHE_HOME", cache.path())
            .arg("-f")
            .arg(&image)
            .arg(&at)
            .spawn()
            .unwrap();
        // The new one waits: nothing is mounted over the old one.
        std::thread::sleep(Duration::from_millis(300));
        assert!(new.try_wait().unwrap().is_none(), "the new sacdfs exited (busy: {busy})");
        assert_eq!(fuse_mounts_on(&at), 1, "mounted over the old mount (busy: {busy})");
        let mut old_child = old.child.take().unwrap();
        let st = Command::new("kill").arg("-TERM").arg(old_child.id().to_string()).status().unwrap();
        assert!(st.success());
        if !busy {
            assert!(wait_exit(&mut old_child, "the old sacdfs"));
        }
        // The new one mounts once the old one is unmounted (or detached: busy).
        wait_for("the new mount", || {
            std::fs::read(&file).is_ok_and(|d| d == whole) && fuse_mounts_on(&at) == 1
        });
        if let Some(f) = open {
            // The old one still serves its open file, and stops when it is closed.
            let mut buf = vec![0; 4096];
            f.read_exact_at(&mut buf, 0).unwrap();
            assert_eq!(buf, whole[..4096]);
            drop(f);
            assert!(wait_exit(&mut old_child, "the old sacdfs"));
        }
        // The old one has exited; the new mount is still there and serving.
        std::thread::sleep(Duration::from_millis(200));
        assert!(new.try_wait().unwrap().is_none(), "the new sacdfs stopped (busy: {busy})");
        assert_eq!(fuse_mounts_on(&at), 1, "busy: {busy}");
        assert_eq!(std::fs::read(&file).unwrap(), whole, "busy: {busy}");
        unmount_retrying(&at);
        assert!(wait_exit(&mut new, "the new sacdfs"));
        assert_eq!(fuse_mounts_on(&at), 0);
    }
}

/// A sacdfs whose mount was unmounted from outside (`fusermount3 -u`) and that is still on
/// its way out leaves alone the mount that is on the path by then (#51). fuser 0.18 unmounted
/// by path when the session ended, and its check that the mount still existed always passed:
/// the old sacdfs removed the new mount. The old one is held stopped (SIGSTOP) to make the
/// order certain: unmounted, new mount, then the old one exits.
#[test]
fn an_exiting_sacdfs_leaves_the_next_mount_alone() {
    if !fuse_available() {
        return;
    }
    let image = corpus().join("images/dsd-stereo-mch-2048.iso");
    let mut old = Mount::foreground(&image, &[]);
    let at = old.path();
    let file =
        at.join(files_under(&at).into_iter().find(|p| p.extension().is_some_and(|e| e == "dsf")).unwrap());
    let whole = std::fs::read(&file).unwrap();
    let mut old_child = old.child.take().unwrap();
    let pid = old_child.id().to_string();
    assert!(Command::new("kill").arg("-STOP").arg(&pid).status().unwrap().success());
    unmount_retrying(&at);
    let cache = tempfile::tempdir().unwrap();
    let mut new = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .env("XDG_CACHE_HOME", cache.path())
        .arg("-f")
        .arg(&image)
        .arg(&at)
        .spawn()
        .unwrap();
    wait_for("the new mount", || std::fs::read(&file).is_ok_and(|d| d == whole));
    assert!(Command::new("kill").arg("-CONT").arg(&pid).status().unwrap().success());
    assert!(wait_exit(&mut old_child, "the old sacdfs"));
    std::thread::sleep(Duration::from_millis(200));
    assert!(new.try_wait().unwrap().is_none(), "the new sacdfs stopped");
    assert_eq!(fuse_mounts_on(&at), 1);
    assert_eq!(std::fs::read(&file).unwrap(), whole);
    unmount_retrying(&at);
    assert!(wait_exit(&mut new, "the new sacdfs"));
}

/// The source field of the mount on `mountpoint` in `/proc/self/mountinfo` (escaped).
fn mount_source(mountpoint: &Path) -> Option<String> {
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mp = mountpoint.to_str().unwrap().replace(' ', "\\040");
    info.lines()
        .filter(|l| l.split(' ').nth(4) == Some(mp.as_str()))
        .filter_map(|l| l.split(" - ").nth(1)?.split(' ').nth(1).map(str::to_owned))
        .next_back()
}

/// The first DSF track's channel hashes of the manifest's `id` case.
fn first_track_hashes(id: &str) -> Vec<String> {
    let case = cases().into_iter().find(|c| c["id"] == id).unwrap();
    let t = &case["expected"]["audio"][0]["tracks"][0];
    t["sha256"].as_array().unwrap().iter().map(|h| h.as_str().unwrap().to_owned()).collect()
}

/// A SOURCE whose path holds a comma, a backslash, a space or `,ro` mounts and reads, and
/// no character of it becomes a mount option (#55): fuser 0.18 passed the path to
/// fusermount3 as `fsname=` unescaped, so `Bach, J.S.` failed ("unknown option ' J.S.'") and
/// `x,allow_other` would have applied the option. The mount is named `sacdfs` instead.
#[test]
fn a_source_path_with_commas_and_backslashes_mounts() {
    if !fuse_available() {
        return;
    }
    let id = "dsd-stereo-mch-2048";
    let want = first_track_hashes(id);
    let top = tempfile::tempdir().unwrap();
    for name in ["Bach, J.S.", "back\\slash", "a space", "x,ro", "y,allow_other", "z\\,rw"] {
        // As a directory, and as the image file itself.
        let dir = top.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        // A copy, not a link: SOURCE is made canonical, which would resolve a link.
        let image = dir.join(format!("{name}.iso"));
        std::fs::copy(corpus().join("images").join(format!("{id}.iso")), &image).unwrap();
        for source in [&dir, &image] {
            let mount = Mount::foreground(source, &[]);
            let root = mount.path();
            assert_eq!(mount_source(&root).as_deref(), Some("sacdfs"), "{name}");
            let stereo = root.join(name).join("Stereo");
            let first = std::fs::read_dir(&stereo)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|e| e == "dsf"))
                .min()
                .unwrap();
            let hashes: Vec<String> =
                dsf_channels(&std::fs::read(&first).unwrap()).iter().map(|c| sha256(c)).collect();
            assert_eq!(hashes, want, "{name}");
            mount.stop("TERM");
        }
    }
}

/// Aborts the FUSE connection of the mount on `mountpoint`, if any, through
/// `/sys/fs/fuse/connections` (its number is the mount's minor device number).
fn abort_connection(mountpoint: &Path) {
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mp = mountpoint.to_str().unwrap();
    for l in info.lines().filter(|l| l.split(' ').nth(4) == Some(mp)) {
        if let Some(minor) = l.split(' ').nth(2).and_then(|d| d.split(':').nth(1)) {
            let _ = std::fs::write(format!("/sys/fs/fuse/connections/{minor}/abort"), "1");
        }
    }
}

/// Runs sacdfs in the foreground with `args`, expecting it to refuse before mounting:
/// returns its standard error. A sacdfs that mounts instead (and would hang) is killed and
/// its mount detached.
fn refused(args: &[&Path], mountpoint: &Path) -> String {
    let cache = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .env("XDG_CACHE_HOME", cache.path())
        .arg("-f")
        .args(args)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(5) {
            // Mounted on its own source, sacdfs hangs as soon as anything looks inside,
            // beyond SIGKILL: abort the connection first.
            abort_connection(mountpoint);
            let _ = child.kill();
            let _ = Command::new("fusermount3").arg("-u").arg("-z").arg(mountpoint).status();
            panic!("sacdfs {args:?} did not refuse");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "sacdfs {args:?} succeeded");
    assert!(start.elapsed() < Duration::from_secs(2), "slow refusal");
    assert!(!is_mounted(mountpoint));
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A SOURCE at or below the mount point, images inside the mount point, and a seek index
/// cache inside it are refused before mounting (#56): sacdfs would read (or write) them
/// through its own mount and hang, beyond the reach of SIGKILL.
#[test]
fn a_source_or_cache_under_the_mount_point_is_refused() {
    let top = tempfile::tempdir().unwrap();
    let x = top.path().canonicalize().unwrap().join("X");
    let sub = x.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    // A copy, not a link: SOURCE is made canonical, which would resolve a link.
    let image = sub.join("disc.iso");
    std::fs::copy(corpus().join("images/dsd-stereo-mch-2048.iso"), &image).unwrap();
    let err = refused(&[&x, &x], &x);
    assert!(err.contains("is the mount point"), "{err}");
    let err = refused(&[&sub, &x], &x);
    assert!(err.contains("is inside the mount point"), "{err}");
    let err = refused(&[&image, &x], &x);
    assert!(err.contains("is inside the mount point"), "{err}");
    // Through a symbolic link to the mount point.
    let link = top.path().join("link");
    std::os::unix::fs::symlink(&x, &link).unwrap();
    let err = refused(&[&link.join("sub"), &x], &x);
    assert!(err.contains("is inside the mount point"), "{err}");
    // The mount point inside SOURCE, holding images.
    let err = refused(&[&x, &sub], &sub);
    assert!(err.contains("holds images"), "{err}");
    // An --index-cache inside the mount point.
    let mnt = top.path().join("mnt");
    std::fs::create_dir(&mnt).unwrap();
    let cache = mnt.join("cache");
    let err = refused(&[Path::new("--index-cache"), &cache, &image, &mnt], &mnt);
    assert!(err.contains("choose another --index-cache"), "{err}");
}

/// After sacdfs is killed (SIGKILL: no unmount), its dead mount fails every access with
/// ENOTCONN. A restart detaches it and mounts at once (#57); before, it waited 10 s and failed
/// with "already a FUSE mount".
#[test]
fn a_restart_after_sigkill_clears_the_dead_mount() {
    if !fuse_available() {
        return;
    }
    let image = corpus().join("images/dsd-stereo-mch-2048.iso");
    let mut old = Mount::foreground(&image, &[]);
    let at = old.path();
    let file =
        at.join(files_under(&at).into_iter().find(|p| p.extension().is_some_and(|e| e == "dsf")).unwrap());
    let whole = std::fs::read(&file).unwrap();
    let mut child = old.child.take().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    let e = std::fs::read_dir(&at).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::ENOTCONN), "{e}");
    let start = Instant::now();
    let cache = tempfile::tempdir().unwrap();
    let mut new = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .env("XDG_CACHE_HOME", cache.path())
        .arg("-f")
        .arg(&image)
        .arg(&at)
        .spawn()
        .unwrap();
    wait_for("the new mount", || std::fs::read(&file).is_ok_and(|d| d == whole));
    assert!(start.elapsed() < Duration::from_secs(3), "the restart took {:?}", start.elapsed());
    assert_eq!(fuse_mounts_on(&at), 1);
    unmount_retrying(&at);
    assert!(wait_exit(&mut new, "the new sacdfs"));
    assert_eq!(fuse_mounts_on(&at), 0);
}

/// A SIGTERM at any moment of the start leaves no dead mount behind (#57): sacdfs either
/// stops before mounting, or mounts and then unmounts. Before, the handlers were set up
/// after mounting, and a SIGTERM in between killed it with the mount in place.
#[test]
fn a_sigterm_right_after_start_leaves_no_mount() {
    if !fuse_available() {
        return;
    }
    let image = corpus().join("images/dsd-stereo-mch-2048.iso");
    let dir = tempfile::tempdir().unwrap();
    let at = dir.path().canonicalize().unwrap();
    let cache = tempfile::tempdir().unwrap();
    for i in 0..60u64 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .env("XDG_CACHE_HOME", cache.path())
            .arg("-f")
            .arg(&image)
            .arg(&at)
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // 0 to 29.5 ms after the start, spread over the start-up.
        std::thread::sleep(Duration::from_micros(i * 500));
        let st = Command::new("kill").arg("-TERM").arg(child.id().to_string()).status().unwrap();
        assert!(st.success());
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if start.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = Command::new("fusermount3").arg("-u").arg("-z").arg(&at).status();
                panic!("sacdfs did not stop after SIGTERM ({i})");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if fuse_mounts_on(&at) != 0 {
            let _ = Command::new("fusermount3").arg("-u").arg("-z").arg(&at).status();
            panic!("a SIGTERM {} us after the start left a mount behind", i * 500);
        }
    }
}

/// Folder art is also in the areas (#95), and files added to or removed from SOURCE show up
/// without a remount (#101), through the kernel: its caches (names, listings) must not hold
/// the old state for long.
#[test]
fn folder_art_in_the_areas_and_changes_without_a_remount() {
    use std::os::unix::fs::MetadataExt;
    if !fuse_available() {
        return;
    }
    let top = tempfile::tempdir().unwrap();
    let src = top.path().canonicalize().unwrap();
    let album = src.join("Artist/Album");
    std::fs::create_dir_all(&album).unwrap();
    std::os::unix::fs::symlink(corpus().join("images/dsd-stereo-mch-2048.iso"), album.join("disc.iso"))
        .unwrap();
    std::fs::write(album.join("folder.jpg"), b"front").unwrap();
    std::fs::write(album.join("list.m3u8"), b"#EXTM3U\n").unwrap();
    let m = Mount::foreground(&src, &[]);
    let mnt = m.path();
    let ls = |p: &str| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(mnt.join(p))
            .map(|rd| rd.map(|e| e.unwrap().file_name().into_string().unwrap()).collect())
            .unwrap_or_default();
        v.sort();
        v
    };
    let has = |p: &str, n: &str| ls(p).iter().any(|x| x == n);

    // Art in each area, the same file as in the folder; playlists only in the folder.
    for area in ["Stereo", "Multichannel"] {
        let p = format!("Artist/Album/disc/{area}");
        assert!(has(&p, "folder.jpg"), "{area}");
        assert!(!has(&p, "list.m3u8"));
        let (a, b) = (
            std::fs::metadata(mnt.join(&p).join("folder.jpg")).unwrap(),
            std::fs::metadata(mnt.join("Artist/Album/folder.jpg")).unwrap(),
        );
        assert_eq!(a.ino(), b.ino());
        assert_eq!(std::fs::read(mnt.join(&p).join("folder.jpg")).unwrap(), b"front");
    }
    // A name looked up before it exists, an open file, and a listing already read.
    assert!(!mnt.join("Artist/Album/back.png").exists());
    assert!(!mnt.join("Artist/Album/disc/Stereo/back.png").exists());
    let mut open = std::fs::File::open(mnt.join("Artist/Album/folder.jpg")).unwrap();

    std::fs::write(album.join("back.png"), b"back").unwrap();
    std::fs::remove_file(album.join("folder.jpg")).unwrap();
    std::fs::create_dir_all(src.join("Artist/Second")).unwrap();
    std::os::unix::fs::symlink(
        corpus().join("images/dsd-stereo-mch-2048.iso"),
        src.join("Artist/Second/d.iso"),
    )
    .unwrap();
    wait_for("the new art", || mnt.join("Artist/Album/back.png").exists());
    wait_for("the new art in the area", || mnt.join("Artist/Album/disc/Stereo/back.png").exists());
    assert_eq!(std::fs::read(mnt.join("Artist/Album/disc/Stereo/back.png")).unwrap(), b"back");
    wait_for("the removal", || !mnt.join("Artist/Album/folder.jpg").exists());
    assert!(!has("Artist/Album/disc/Stereo", "folder.jpg"));
    assert!(std::fs::File::open(mnt.join("Artist/Album/folder.jpg")).is_err());
    // The file opened before still reads.
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut open, &mut bytes).unwrap();
    assert_eq!(bytes, b"front");
    drop(open);
    // A new image shows, with its audio.
    wait_for("the new image", || has("Artist", "Second"));
    assert!(ls("Artist/Second/d/Stereo").iter().any(|n| n.ends_with(".dsf")));
    m.stop("TERM");
}
