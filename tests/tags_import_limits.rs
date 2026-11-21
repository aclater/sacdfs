// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs tags import` keeps only what the mount uses: a picture over `tags::MAX_ART_BYTES`, a
//! value over `tags_cli::MAX_VALUE_BYTES` and keys past `tags_cli::MAX_TRACK_KEYS` are not
//! written but listed as not kept, with exit status 2, so that their extracts are kept. And
//! everything it writes is served through the mount.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "a test fails by panicking")]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sacdfs::disc::{Disc, Tuning};
use sacdfs::engine::Engine;
use sacdfs::tags::{self, TagsFile};
use sacdfs::tags_cli::{MAX_FILE_BYTES, MAX_LISTED, MAX_TRACK_KEYS, MAX_VALUE_BYTES};
use sacdfs::track::Reader;
use sacdfs::tree::{SourceDir, Tree};
use scarletbook::Slot;

const TAGGED: &str = "dsd-stereo-mch-2048";

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn syncsafe(n: usize) -> [u8; 4] {
    let n = n as u32;
    [(n >> 21) as u8 & 0x7F, (n >> 14) as u8 & 0x7F, (n >> 7) as u8 & 0x7F, n as u8 & 0x7F]
}

/// Appends an ID3v2.4 frame to `tag` and fixes the tag's size.
fn frame(tag: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    tag.extend_from_slice(id);
    tag.extend_from_slice(&syncsafe(body.len()));
    tag.extend_from_slice(&[0, 0]);
    tag.extend_from_slice(body);
    let s = syncsafe(tag.len() - 10);
    tag[6..10].copy_from_slice(&s);
}

fn txxx(tag: &mut Vec<u8>, desc: &str, value: &str) {
    let mut body = vec![3u8];
    body.extend_from_slice(desc.as_bytes());
    body.push(0);
    body.extend_from_slice(value.as_bytes());
    frame(tag, b"TXXX", &body);
}

/// Writes into `ext` an extract of each stereo DSF track of the image in `lib`, its tag as the
/// mount serves it plus what `add` appends (with the track's number and its index). The tracks'
/// numbers, in order.
fn write_extracts(lib: &Path, ext: &Path, mut add: impl FnMut(u32, usize, &mut Vec<u8>)) -> Vec<u32> {
    let engine = Engine::new(2, 32 << 20).unwrap();
    let tree = Tree::new(SourceDir::scan(lib).unwrap());
    let mut stereo: Vec<(PathBuf, _)> = tree
        .all_files()
        .into_iter()
        .filter(|(p, _)| p.to_string_lossy().contains("Stereo") && p.extension().is_some_and(|e| e == "dsf"))
        .collect();
    stereo.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(stereo.len() >= 2, "the stereo area has {} tracks", stereo.len());
    fs::create_dir_all(ext).unwrap();
    let mut numbers = Vec::new();
    for (i, (p, f)) in stereo.iter().enumerate() {
        let number = common::track_number(&p.file_name().unwrap().to_string_lossy());
        numbers.push(number);
        let (bytes, _) = Reader::new(f.clone()).read(&engine, 0, f.size).unwrap();
        let end = f.data_end() as usize;
        let mut tag = f.tag().to_vec();
        assert!(tag.starts_with(b"ID3\x04"), "{p:?}");
        tag.truncate(10 + tag[6..10].iter().fold(0usize, |a, &b| (a << 7) | usize::from(b & 0x7F)));
        add(number, i, &mut tag);
        let mut out = bytes[..end].to_vec();
        out.extend_from_slice(&tag);
        let total = (out.len() as u64).to_le_bytes();
        out[12..20].copy_from_slice(&total);
        fs::write(ext.join(format!("{number:02}.dsf")), &out).unwrap();
    }
    numbers
}

/// Runs `sacdfs tags import` of the extracts in `ext` against `lib` into `tags_dir`: its exit
/// status and what it said.
fn import(lib: &Path, ext: &Path, tags_dir: &Path) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .args(["tags", "import", "-q", "--pauses", "dropped", "--tags"])
        .arg(tags_dir)
        .arg("--map")
        .arg(ext)
        .arg(lib)
        .arg(lib)
        .arg(ext)
        .output()
        .unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// A tags file there already, near the bound on what import writes: what import would add to
/// it is not written, and the disc is listed as not kept. Many values too large are listed up
/// to `MAX_LISTED`, then counted.
#[test]
fn import_leaves_a_full_tags_file_and_bounds_its_list() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    let iso = format!("{TAGGED}.iso");
    common::link(common::corpus().join("images").join(&iso), lib.join(&iso)).unwrap();
    let ext = dir.path().join("ext");
    let big = "b".repeat(MAX_VALUE_BYTES + 1);
    let many = MAX_LISTED + 8;
    let numbers = write_extracts(&lib, &ext, |_, i, tag| {
        frame(tag, b"TCON", b"\x03New Genre");
        if i == 0 {
            for k in 0..many {
                txxx(tag, &format!("Big {k:02}"), &big);
            }
        }
    });
    let tags_dir = dir.path().join("tags");
    fs::create_dir_all(&tags_dir).unwrap();
    let key = tags::disc_key(Disc::open(&lib.join(&iso), 0, Tuning::default()).unwrap().toc());
    let path = tags_dir.join(format!("{key}.toml"));

    // Many values too large, and no tags file yet: listed up to the bound.
    let (code, say) = import(&lib, &ext, &tags_dir);
    assert_eq!(code, Some(2), "{say}");
    let not_kept: Vec<&str> = say.lines().filter(|l| l.starts_with("not kept: ")).collect();
    assert_eq!(not_kept.len(), MAX_LISTED + 1, "{say}");
    assert_eq!(not_kept[MAX_LISTED], format!("not kept: +{} more", many - MAX_LISTED), "{say}");
    let written = TagsFile::parse(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(written.fields_for(Slot::TwoChannel, numbers[0] as u8).contains_key("genre"));

    // A tags file near the bound, without that genre: left as it was, and the disc listed.
    let own = format!("[album]\ncopyright = \"{}\"\n", "c".repeat(MAX_FILE_BYTES - 100));
    fs::write(&path, &own).unwrap();
    let (code, say) = import(&lib, &ext, &tags_dir);
    assert_eq!(code, Some(2), "{say}");
    assert_eq!(fs::read_to_string(&path).unwrap(), own);
    let not_kept: Vec<&str> = say.lines().filter(|l| l.starts_with("not kept: ")).collect();
    assert_eq!(not_kept.len(), 1, "{say}");
    assert!(not_kept[0].contains(&format!("{key}.toml")) && not_kept[0].contains("nothing added"), "{say}");
    assert!(say.contains("keep those extracts"), "{say}");
}

#[test]
fn import_keeps_only_what_the_mount_serves() {
    common::skip_without_fuse!();
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    let iso = format!("{TAGGED}.iso");
    common::link(common::corpus().join("images").join(&iso), lib.join(&iso)).unwrap();

    // Extracts of the stereo DSF tracks, as a tagger would leave them: each with a genre, a
    // value of its own and a 1.2 MiB cover; track 1 with a 3 MiB text field as well, and
    // track 2 with more keys than a track can have.
    let ext = dir.path().join("ext");
    let mut cover = b"\xFF\xD8\xFF\xE0".to_vec();
    cover.resize(1200 << 10, 0x55);
    let mut apic = vec![0u8];
    apic.extend_from_slice(b"image/jpeg\0\x03\0");
    apic.extend_from_slice(&cover);
    let notes = "n".repeat(3 << 20);
    let many = MAX_TRACK_KEYS + 6;
    let numbers = write_extracts(&lib, &ext, |number, i, tag| {
        frame(tag, b"TCON", b"\x03Kept Genre");
        txxx(tag, "Own", &format!("kept-own-{number}"));
        frame(tag, b"APIC", &apic);
        if i == 0 {
            txxx(tag, "Liner Notes", &notes);
        }
        if i == 1 {
            for k in 0..many {
                txxx(tag, &format!("Key {k:02}"), &format!("many-{k:02}"));
            }
        }
    });
    let stereo = &numbers;

    let tags_dir = dir.path().join("tags");
    let (code, say) = import(&lib, &ext, &tags_dir);
    // Something not kept: 2, and each is listed with its extract.
    assert_eq!(code, Some(2), "{say}");
    let not_kept: Vec<&str> = say.lines().filter(|l| l.starts_with("not kept: ")).collect();
    assert_eq!(not_kept.iter().filter(|l| l.contains("picture of 1200 KiB")).count(), stereo.len(), "{say}");
    assert_eq!(
        not_kept
            .iter()
            .filter(|l| l.contains(&format!("{:02}.dsf", numbers[0])) && l.contains("TXXX:Liner Notes"))
            .count(),
        1,
        "{say}"
    );
    let over =
        |l: &&&str| l.contains(&format!("{:02}.dsf", numbers[1])) && l.contains("keys a track can have");
    // The genre and its own value come first: those two and the first 62 of the many are kept.
    assert_eq!(not_kept.iter().filter(over).count(), many - (MAX_TRACK_KEYS - 2), "{say}");
    assert!(say.contains("keep those extracts"), "{say}");
    assert!(!say.contains("failed: "), "{say}");

    // The tags file has the rest, and no oversized picture or value.
    let key = tags::disc_key(Disc::open(&lib.join(&iso), 0, Tuning::default()).unwrap().toc());
    let file = TagsFile::parse(&fs::read_to_string(tags_dir.join(format!("{key}.toml"))).unwrap()).unwrap();
    assert!(!tags_dir.join(format!("{key}.jpg")).exists());
    let mut kept = Vec::new();
    for &n in &numbers {
        let fields = file.fields_for(Slot::TwoChannel, n as u8);
        assert!(!fields.contains_key("art"), "{fields:?}");
        assert!(fields.values().all(|v| v.len() <= MAX_VALUE_BYTES), "track {n}");
        assert!(fields.len() <= MAX_TRACK_KEYS, "track {n}: {}", fields.len());
        assert_eq!(fields.get("TXXX:Own"), Some(&format!("kept-own-{n}")), "{fields:?}");
        assert_eq!(fields.get("genre").map(String::as_str), Some("Kept Genre"));
        kept.push((n, fields));
    }
    assert_eq!(kept[1].1.len(), MAX_TRACK_KEYS);

    // Mounted with the tags directory: every value import kept is in the served file.
    let mp = dir.path().join("mnt");
    fs::create_dir_all(&mp).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .env("XDG_CACHE_HOME", dir.path().join("cache"))
        .args(["-f", "--format", "dsf", "--tags"])
        .arg(&tags_dir)
        .arg(&lib)
        .arg(&mp)
        .spawn()
        .unwrap();
    let area = mp.join(TAGGED).join("Stereo");
    let start = std::time::Instant::now();
    while !area.is_dir() {
        assert!(start.elapsed().as_secs() < 30, "the mount");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let served = |n: u32| -> Vec<u8> {
        let entry = fs::read_dir(&area)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.extension().is_some_and(|e| e == "dsf")
                    && common::track_number(&p.file_name().unwrap().to_string_lossy()) == n
            })
            .unwrap();
        fs::read(entry).unwrap()
    };
    let check = || -> Result<(), String> {
        for (n, fields) in &kept {
            let bytes = served(*n);
            for (k, v) in fields {
                if !has(&bytes, v.as_bytes()) {
                    return Err(format!("track {n}: {k} = {v:?} is not served"));
                }
            }
            if has(&bytes, b"nnnnnnnnnnnnnnnnnnnn") {
                return Err(format!("track {n}: the notes are served"));
            }
        }
        Ok(())
    };
    let r = check();
    common::unmount_retrying(&mp);
    let _ = child.wait();
    r.unwrap();
}

/// A value too large whose TXXX description holds a line end and escape sequences: its
/// "not kept" line shows them escaped, and makes no line of its own.
#[test]
fn not_kept_names_are_escaped() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    let iso = format!("{TAGGED}.iso");
    common::link(common::corpus().join("images").join(&iso), lib.join(&iso)).unwrap();
    let ext = dir.path().join("ext");
    let big = "b".repeat(MAX_VALUE_BYTES + 1);
    write_extracts(&lib, &ext, |_, i, tag| {
        if i == 0 {
            txxx(tag, "Notes\nFORGED line\x1b]0;t\x07\x1b[2J", &big);
        }
    });
    let (code, say) = import(&lib, &ext, &dir.path().join("tags"));
    assert_eq!(code, Some(2), "{say}");
    assert!(!say.lines().any(|l| l.starts_with("FORGED")), "{say}");
    assert!(!say.chars().any(|c| c.is_control() && c != '\n'), "{say:?}");
    assert!(say.contains("TXXX:Notes\\x0aFORGED line\\x1b]0;t\\x07\\x1b[2J"), "{say}");
}
