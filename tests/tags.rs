// SPDX-License-Identifier: GPL-3.0-or-later

//! The tag layer (#81): `sacdfs tags import` writes a tags file from extracts' tags, without
//! touching the extracts; the served DSF and WAV files then carry those tags, with a new
//! generation and mtime; a disc without a tags file is byte for byte as without the layer;
//! and the binary's `--tags` serves the same through the kernel.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::SystemTime;

use sacdfs::disc::{Disc, Tuning};
use sacdfs::engine::Engine;
use sacdfs::tags::{self, Overrides, Store, TagsFile};
use sacdfs::track::{Reader, TrackFile};
use sacdfs::tree::{Node, SourceDir, Tree};

const TAGGED: &str = "dsd-stereo-mch-2048";
const OTHER: &str = "dst-features-2ch-6ch";
const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0 a tiny not-quite JPEG";

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Every file of the tree: its bytes, generation and mtime, by path.
type Snapshot = BTreeMap<PathBuf, (Arc<TrackFile>, Vec<u8>, u64, SystemTime)>;

fn snapshot(lib: &Path, engine: &Engine) -> Snapshot {
    let tree = Tree::new(SourceDir::scan(lib).unwrap());
    tree.all_files()
        .into_iter()
        .map(|(p, f)| {
            let (bytes, _) = Reader::new(f.clone()).read(engine, 0, f.size).unwrap();
            let node = Node::File(f.clone());
            (p, (f, bytes, tree.generation(&node), tree.mtime(&node)))
        })
        .collect()
}

/// `tags::install` sets a process-wide store; the tests here must not overlap while one is set.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn library() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    for image in [TAGGED, OTHER] {
        let image = format!("{image}.iso");
        std::os::unix::fs::symlink(common::corpus().join("images").join(&image), lib.join(&image)).unwrap();
    }
    (dir, lib)
}

fn sha(path: &Path) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(fs::read(path).unwrap()).to_vec()
}

#[test]
fn import_then_serve() {
    let _g = serial();
    let (dir, lib) = library();
    let engine = Engine::new(2, 32 << 20).unwrap();
    let before = snapshot(&lib, &engine);

    // Extracts of every stereo DSF track of the tagged disc: the served file with a genre,
    // a MusicBrainz id and a picture added to its tag, as a tagger would leave it.
    let ext = dir.path().join("ext");
    fs::create_dir_all(&ext).unwrap();
    let extra = Overrides {
        frames: vec![(*b"TCON", None, "Jazz".into())],
        art: Some(("image/jpeg", Arc::new(JPEG.to_vec()))),
        ..Overrides::default()
    };
    let mut n = 0;
    for (p, (f, bytes, _, _)) in &before {
        let s = p.to_string_lossy();
        if !s.contains(TAGGED) || !s.contains("Stereo") || !s.ends_with(".dsf") {
            continue;
        }
        let end = f.data_end() as usize;
        let mut tag = tags::append_frames(f.tag(), &extra).unwrap();
        // A TXXX frame: description, value (UTF-8).
        let mut body = vec![3u8];
        body.extend_from_slice(b"MusicBrainz Album Id\0mb-album");
        tag.truncate(10 + syncsafe_len(&tag));
        frame(&mut tag, b"TXXX", &body);
        let mut out = bytes[..end].to_vec();
        out.extend_from_slice(&tag);
        let total = out.len() as u64;
        out[12..20].copy_from_slice(&total.to_le_bytes());
        fs::write(ext.join(format!("{n:02}.dsf")), &out).unwrap();
        n += 1;
    }
    assert!(n >= 2, "the stereo area has {n} tracks");
    let ext_hashes: Vec<Vec<u8>> = (0..n).map(|i| sha(&ext.join(format!("{i:02}.dsf")))).collect();

    let tags_dir = dir.path().join("tags");
    let args = sacdfs::verify::Args {
        pauses: "dropped".into(),
        source: lib.clone(),
        extracts: vec![ext.clone()],
        map: vec![ext.clone(), lib.clone()],
        output: None,
        resume: false,
        threads: 2,
        cache_mb: 8,
        quiet: true,
        verbose: 0,
    };
    let sum = sacdfs::tags_cli::import(&tags_dir, args).unwrap();
    assert_eq!((sum.files, sum.written, sum.created, sum.pictures), (n, 1, 1, 1), "{}", sum.render());
    for (i, h) in ext_hashes.iter().enumerate() {
        assert_eq!(&sha(&ext.join(format!("{i:02}.dsf"))), h, "an extract changed");
    }
    let key =
        tags::disc_key(Disc::open(&lib.join(format!("{TAGGED}.iso")), 0, Tuning::default()).unwrap().toc());
    let toml_path = tags_dir.join(format!("{key}.toml"));
    let file = TagsFile::parse(&fs::read_to_string(&toml_path).unwrap()).unwrap();
    assert_eq!(file.stereo.album.get("genre").map(String::as_str), Some("Jazz"), "{file:?}");
    assert_eq!(file.stereo.album.get("TXXX:MusicBrainz Album Id").map(String::as_str), Some("mb-album"));
    assert_eq!(file.album.get("art"), Some(&format!("{key}.jpg")));
    assert_eq!(fs::read(tags_dir.join(format!("{key}.jpg"))).unwrap(), JPEG);
    // A second import adds nothing.
    let again = sacdfs::tags_cli::import(
        &tags_dir,
        sacdfs::verify::Args {
            pauses: "dropped".into(),
            source: lib.clone(),
            extracts: vec![ext.clone()],
            map: vec![ext.clone(), lib.clone()],
            output: None,
            resume: false,
            threads: 2,
            cache_mb: 8,
            quiet: true,
            verbose: 0,
        },
    )
    .unwrap();
    assert_eq!(again.written, 0);

    // Served with the tags directory.
    tags::install(Some(Store::new(&tags_dir)));
    let after = snapshot(&lib, &engine);
    assert_eq!(after.len(), before.len());
    let tags_mtime = fs::metadata(&toml_path).unwrap().modified().unwrap();
    for (p, (f, bytes, generation, mtime)) in &after {
        let (_, old, old_gen, old_mtime) = &before[p];
        let s = p.to_string_lossy();
        if !s.contains(TAGGED) {
            assert_eq!(bytes, old, "{s}: a disc without a tags file changed");
            assert_eq!((generation, mtime), (old_gen, old_mtime), "{s}");
            assert!(f.tags.is_none());
            continue;
        }
        assert_ne!(generation, old_gen, "{s}");
        assert!(*mtime >= tags_mtime, "{s}");
        assert!(has(bytes, JPEG), "{s}: the picture");
        assert!(has(bytes, b"APIC"), "{s}");
        // The audio is the same: only the tag differs.
        let (h, e) = (f.header_len() as usize, f.data_end() as usize);
        assert_eq!(bytes[h..e], old[h..e], "{s}");
        let mc = s.contains("Multichannel");
        assert_eq!(has(bytes, b"Jazz"), !mc, "{s}: genre only in the stereo area");
        assert_eq!(has(bytes, b"mb-album"), !mc, "{s}");
        assert_eq!(has(bytes, b" (Multichannel)"), mc, "{s}: the suffix stays");
        if s.ends_with(".wav") {
            assert!(has(bytes, b"id3 "), "{s}");
        }
    }

    // A hand edit naming the multichannel album: no suffix there.
    let mut edited = file.clone();
    edited.multichannel.album.insert("album".into(), "Hand Named".into());
    fs::write(&toml_path, edited.to_text()).unwrap();
    let edits = snapshot(&lib, &engine);
    for (p, (_, bytes, _, _)) in &edits {
        let s = p.to_string_lossy();
        if s.contains(TAGGED) && s.contains("Multichannel") {
            assert!(has(bytes, b"Hand Named") && !has(bytes, b" (Multichannel)"), "{s}");
        }
    }
    tags::install(None);
    // Without the tags directory: as before, byte for byte.
    let none = snapshot(&lib, &engine);
    for (p, (_, bytes, generation, _)) in &none {
        assert_eq!((bytes, generation), (&before[p].1, &before[p].2), "{p:?}");
    }
}

fn syncsafe_len(tag: &[u8]) -> usize {
    tag[6..10].iter().fold(0usize, |a, &b| (a << 7) | usize::from(b & 0x7F))
}

/// Appends an ID3v2.4 frame to `tag` and fixes the tag's size.
fn frame(tag: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    let n = body.len() as u32;
    tag.extend_from_slice(id);
    tag.extend_from_slice(&[
        (n >> 21) as u8 & 0x7F,
        (n >> 14) as u8 & 0x7F,
        (n >> 7) as u8 & 0x7F,
        n as u8 & 0x7F,
    ]);
    tag.extend_from_slice(&[0, 0]);
    tag.extend_from_slice(body);
    let s = (tag.len() - 10) as u32;
    tag[6..10].copy_from_slice(&[
        (s >> 21) as u8 & 0x7F,
        (s >> 14) as u8 & 0x7F,
        (s >> 7) as u8 & 0x7F,
        s as u8 & 0x7F,
    ]);
}

#[test]
fn key_command_and_mount_with_tags() {
    let _g = serial();
    let (dir, lib) = library();
    let tags_dir = dir.path().join("tags");
    fs::create_dir_all(&tags_dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .args(["tags", "key", "--tags"])
        .arg(&tags_dir)
        .arg(lib.join(format!("{TAGGED}.iso")))
        .output()
        .unwrap();
    assert!(out.status.success());
    let line = String::from_utf8(out.stdout).unwrap();
    let key = line.split('\t').next().unwrap().to_owned();
    assert_eq!(key.len(), 32);
    assert!(line.contains("\tnone\t"), "{line}");
    fs::write(tags_dir.join(format!("{key}.toml")), "[track.1]\ntitle = \"Through The Kernel\"\n").unwrap();

    common::skip_without_fuse!();
    let mp = dir.path().join("mnt");
    fs::create_dir_all(&mp).unwrap();
    let cache = dir.path().join("cache");
    let mut child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .env("XDG_CACHE_HOME", &cache)
        .args(["-f", "--format", "dsf", "--tags"])
        .arg(&tags_dir)
        .arg(&lib)
        .arg(&mp)
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    let stereo = mp.join(TAGGED).join("Stereo");
    while !stereo.is_dir() {
        assert!(start.elapsed().as_secs() < 30, "the mount");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut names: Vec<PathBuf> = fs::read_dir(&stereo).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    let first = fs::read(&names[0]).unwrap();
    let second = fs::read(&names[1]).unwrap();
    // Track 1 has the title; the file name is the TOC's, as before.
    assert!(has(&first, b"Through The Kernel"));
    assert!(!has(&second, b"Through The Kernel"));
    // The other disc: as without the tag layer.
    let engine = Engine::new(2, 32 << 20).unwrap();
    let tree = Tree::with_options(
        SourceDir::scan(&lib).unwrap(),
        Tuning::default(),
        sacdfs::tree::Output::dsf_only(),
    );
    for (p, f) in tree.all_files() {
        if !p.to_string_lossy().contains(OTHER) {
            continue;
        }
        let (want, _) = Reader::new(f.clone()).read(&engine, 0, f.size).unwrap();
        assert_eq!(fs::read(mp.join(&p)).unwrap(), want, "{p:?}");
    }
    common::unmount_retrying(&mp);
    let _ = child.wait();
}

/// The exit status of `sacdfs tags import` (#198): 0 when every extract was handled, 2 when any
/// wasn't, with what could be done done and the rows written, and 1 when it couldn't run.
#[test]
fn import_exit_status() {
    let _g = serial();
    let (dir, lib) = library();
    fs::remove_file(lib.join(format!("{OTHER}.iso"))).unwrap();
    let image = lib.join(format!("{TAGGED}.iso"));
    let import = |tags: &Path| {
        Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .args(["tags", "import", "-q", "--tags"])
            .arg(tags)
            .arg(&lib)
            .output()
            .unwrap()
    };
    let say = |o: &std::process::Output| String::from_utf8_lossy(&o.stderr).into_owned();

    // Every extract matches: 0. Its tag is the served one, so nothing is added.
    let tree = Tree::new(SourceDir::scan(&lib).unwrap());
    let engine = Engine::new(2, 32 << 20).unwrap();
    let file = tree.all_files().into_iter().next().unwrap().1;
    let (bytes, _) = Reader::new(file.clone()).read(&engine, 0, file.size).unwrap();
    fs::write(lib.join("good.dsf"), &bytes).unwrap();
    let tags = dir.path().join("t0");
    let out = import(&tags);
    assert_eq!(out.status.code(), Some(0), "{}", say(&out));
    assert!(say(&out).contains("1 matched extracts read"), "{}", say(&out));
    assert!(!say(&out).contains("not imported"), "{}", say(&out));

    // A FLAC file can't be compared, and verify doesn't count it: still 0.
    fs::write(lib.join("note.flac"), b"fLaC").unwrap();
    assert_eq!(import(&dir.path().join("t1")).status.code(), Some(0));

    // An extract with an ERROR row: 2; the other is read, and the report has both.
    fs::write(lib.join("broken.dsf"), b"DSD not really").unwrap();
    let tags = dir.path().join("t2");
    let out = import(&tags);
    assert_eq!(out.status.code(), Some(2), "{}", say(&out));
    assert!(say(&out).contains("1 matched extracts read"), "{}", say(&out));
    assert!(say(&out).contains("not imported: 1 extracts"), "{}", say(&out));
    let report = fs::read_to_string(tags.join(".last-import.tsv")).unwrap();
    assert!(report.lines().any(|l| l.starts_with("ERROR\t") && l.contains("broken.dsf")), "{report}");
    assert!(report.lines().any(|l| l.starts_with("IDENTICAL\t") && l.contains("good.dsf")), "{report}");
    fs::remove_file(lib.join("broken.dsf")).unwrap();

    // A tags file that can't be read or written: 2, with a `failed:` line. An extract with a tag
    // of its own, so that there is something to write.
    let key = tags::disc_key(Disc::open(&image, 0, Tuning::default()).unwrap().toc());
    let mut tag = file.tag().to_vec();
    frame(&mut tag, b"TCON", b"\x03Jazz");
    let audio_end = bytes.len() - file.tag().len();
    let mut tagged = bytes[..audio_end].to_vec();
    tagged.extend_from_slice(&tag);
    let total = (tagged.len() as u64).to_le_bytes();
    tagged[12..20].copy_from_slice(&total);
    fs::write(lib.join("good.dsf"), &tagged).unwrap();
    let tags = dir.path().join("t3");
    fs::create_dir_all(tags.join(format!("{key}.toml"))).unwrap();
    let out = import(&tags);
    assert_eq!(out.status.code(), Some(2), "{}", say(&out));
    assert!(say(&out).contains("failed: "), "{}", say(&out));

    // A matched extract whose tag is too large to read: 2, with a `failed:` line naming it.
    let n: u32 = 17 << 20;
    let mut large = bytes[..audio_end].to_vec();
    large.extend_from_slice(b"ID3\x04\0\0");
    large.extend_from_slice(&[
        (n >> 21) as u8 & 0x7F,
        (n >> 14) as u8 & 0x7F,
        (n >> 7) as u8 & 0x7F,
        n as u8 & 0x7F,
    ]);
    large.resize(audio_end + 10 + n as usize, 0);
    fs::write(lib.join("good.dsf"), &large).unwrap();
    let out = import(&dir.path().join("t5"));
    assert_eq!(out.status.code(), Some(2), "{}", say(&out));
    assert!(say(&out).contains("failed: ") && say(&out).contains("too large"), "{}", say(&out));

    // It can't run: 1.
    let out = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .args(["tags", "import", "-q", "--tags"])
        .arg(dir.path().join("t4"))
        .arg(dir.path().join("no-such-source"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", say(&out));
}
