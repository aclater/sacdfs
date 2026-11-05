// SPDX-License-Identifier: GPL-3.0-or-later

//! Names for discs without text (#185, phase 1): a disc with no text gets `Track NN` titles and
//! file names, and an album and artist from its path; its audio stays as it was, its files alone
//! get a new mtime and generation; a tags file wins over all of it; and `verify` and `tags import`
//! don't take the guesses for what the disc holds.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sacdfs::engine::Engine;
use sacdfs::ident;
use sacdfs::tags::{self, Store};
use sacdfs::track::{Format, Reader, TrackFile};
use sacdfs::tree::{Node, Output, SourceDir, Tree};
use sacdfs::verify::{self, id3};

const NO_TEXT: &str = "no-text.iso";
const TEXT: &str = "dsd-stereo-mch-2048.iso";
const IN_FOLDERS: &str = "Some One/2001 - An Album [SACD]/Some One - An Album.iso";

/// A library with `image` of the corpus at `path`.
fn library(items: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    for (image, path) in items {
        let at = lib.join(path);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(common::corpus().join("images").join(image), at).unwrap();
    }
    (dir, lib)
}

fn tree(lib: &Path) -> Tree {
    let output = Output::default();
    Tree::with_options(SourceDir::scan(lib).unwrap(), Default::default(), output)
}

fn text(f: &TrackFile, key: &str) -> Option<String> {
    id3::frames(f.tag()).into_iter().find(|x| x.key == key).map(|x| x.value)
}

/// The files of a tree under their paths as text, DSF and WAV.
fn files(t: &Tree) -> BTreeMap<String, Arc<TrackFile>> {
    t.all_files().into_iter().map(|(p, f)| (p.to_string_lossy().into_owned(), f)).collect()
}

#[test]
fn a_disc_without_text_gets_names_and_new_times() {
    let (_dir, lib) = library(&[(NO_TEXT, IN_FOLDERS), (TEXT, "Other/2002 - Named/Other - Named.iso")]);
    let t = tree(&lib);
    let all = files(&t);
    let image = fs::metadata(lib.join(IN_FOLDERS)).unwrap().modified().unwrap();
    let mut filled = 0;
    let mut formats = std::collections::HashSet::new();
    for (path, f) in &all {
        if !path.starts_with("Some One") {
            // A disc with its text is as it was: no fill-in, its format's mtime.
            assert!(!f.filled, "{path}");
            assert_eq!(text(f, "TALB").as_deref().map(|a| a.contains("Plain DSD")), Some(true), "{path}");
            continue;
        }
        filled += 1;
        formats.insert(f.format());
        let n = f.number;
        assert!(path.ends_with(&format!("/{n:02} - Track {n:02}.{}", f.format().extension())), "{path}");
        assert!(f.filled, "{path}");
        assert_eq!(text(f, "TIT2"), Some(format!("Track {n:02}")), "{path}");
        assert_eq!(text(f, "TPE1").as_deref(), Some("Some One"), "{path}");
        assert_eq!(text(f, "TPE2").as_deref(), Some("Some One"), "{path}");
        assert_eq!(text(f, "TALB").as_deref(), Some("An Album"), "{path}");
        // One output version past the file without the fill-in; the folders above show no older.
        let node = Node::File(f.clone());
        let (dsf, wav) = (f.format() == Format::Dsf, f.format() == Format::Wav);
        let moved = image + ident::newest_offset(dsf, wav, 0);
        assert!(moved > image + ident::version_offset(f.format()));
        assert_eq!(t.mtime(&node), moved, "{path}");
        let root = t.get(sacdfs::tree::ROOT).unwrap();
        assert!(t.mtime(&root) >= t.mtime(&node), "{path}");
    }
    assert!(filled > 0);
    assert_eq!(formats.len(), 2, "both formats");
}

#[test]
fn titles_stay_per_area_and_the_multichannel_suffix_applies() {
    let (_dir, lib) = library(&[("master-no-text-sector.iso", "x - 2001 - y.iso")]);
    let all = files(&tree(&lib));
    assert!(all.keys().any(|p| p.contains("Multichannel")) && all.keys().any(|p| p.contains("Stereo")));
    for (path, f) in all {
        let want = if path.contains("Multichannel") { "y (Multichannel)" } else { "y" };
        assert_eq!(text(&f, "TALB").as_deref(), Some(want), "{path}");
        assert_eq!(text(&f, "TPE2").as_deref(), Some("x"), "{path}");
        // This disc's stereo tracks have titles and its multichannel ones don't.
        let title = text(&f, "TIT2").unwrap();
        assert_eq!(title.starts_with("Track "), path.contains("Multichannel"), "{path}: {title}");
        assert!(path.contains(&title), "{path}: {title}");
    }
}

/// The audio does not depend on the tag: the same bytes at every path, whatever it gives.
#[test]
fn the_audio_is_the_same_under_every_name() {
    let (_dir, a) = library(&[(NO_TEXT, IN_FOLDERS)]);
    let (_dir2, b) = library(&[(NO_TEXT, "plain.iso")]);
    let engine = Engine::new(2, 32 << 20).unwrap();
    let audio = |f: &Arc<TrackFile>| {
        let (bytes, _) = Reader::new(f.clone()).read(&engine, 0, f.size).unwrap();
        bytes[f.header_len() as usize..f.data_end() as usize].to_vec()
    };
    let by_track = |lib: &Path| {
        files(&tree(lib))
            .into_values()
            .map(|f| ((f.area, f.number, f.format().extension()), audio(&f)))
            .collect::<BTreeMap<_, _>>()
    };
    let (x, y) = (by_track(&a), by_track(&b));
    assert!(!x.is_empty());
    assert_eq!(x, y);
}

/// The tags file wins, an empty value included; what it leaves still comes from the path. The one
/// test of this file that installs a tags directory: nothing else here has a tags file.
#[test]
fn a_tags_file_wins_and_verify_and_import_keep_what_the_extract_holds() {
    let (dir, lib) = library(&[(NO_TEXT, IN_FOLDERS)]);
    let tagsdir = dir.path().join("tags");
    fs::create_dir_all(&tagsdir).unwrap();
    let t = tree(&lib);
    let (path, first) =
        files(&t).into_iter().find(|(_, f)| f.number == 1 && f.format() == Format::Dsf).unwrap();
    let toc = first.disc.toc();
    let key = tags::disc_key(toc);

    // An extract of track 1 whose tag says what the path says, plus a title of its own. Its album
    // is not a loss to the path alone: a folder can be renamed. As served, with another title.
    let engine = Engine::new(2, 32 << 20).unwrap();
    let (bytes, _) = Reader::new(first.clone()).read(&engine, 0, first.size).unwrap();
    let end = first.data_end() as usize;
    let mut tag = b"ID3\x04\0\0\0\0\0\0".to_vec();
    for (id, value) in [(b"TALB", "An Album"), (b"TIT2", "Real Title")] {
        let mut body = vec![3];
        body.extend_from_slice(value.as_bytes());
        tag.extend_from_slice(id);
        tag.extend_from_slice(&[0, 0, 0, body.len() as u8, 0, 0]);
        tag.extend_from_slice(&body);
    }
    let size = tag.len() - 10;
    tag[8] = (size >> 7) as u8;
    tag[9] = (size & 0x7f) as u8;
    let mut extract = bytes[..end].to_vec();
    extract.extend_from_slice(&tag);
    let total = extract.len() as u64;
    extract[12..20].copy_from_slice(&total.to_le_bytes());
    extract[20..28].copy_from_slice(&(end as u64).to_le_bytes());
    let ext = dir.path().join("ext");
    fs::create_dir_all(&ext).unwrap();
    fs::write(ext.join("01 - a.dsf"), extract).unwrap();
    let args = verify::Args {
        pauses: "dropped".into(),
        source: lib.clone(),
        extracts: vec![ext.clone()],
        map: vec![ext.clone(), lib.join(IN_FOLDERS).parent().unwrap().to_owned()],
        output: None,
        resume: false,
        threads: 2,
        cache_mb: 8,
        quiet: true,
        verbose: 0,
    };
    let sum = sacdfs::tags_cli::import(&tagsdir, args).unwrap();
    assert_eq!(sum.written, 1, "{}", sum.render());
    let imported = fs::read_to_string(tagsdir.join(format!("{key}.toml"))).unwrap();
    assert!(imported.contains("An Album"), "the album is kept though the path says the same: {imported}");
    assert!(imported.contains("Real Title"), "{imported}");

    // By hand: an album and a title, and a title removed.
    fs::write(
        tagsdir.join(format!("{key}.toml")),
        "[album]\nalbum = \"Mine\"\n\n[track.1]\ntitle = \"Real Title\"\n\n[track.2]\ntitle = \"\"\n",
    )
    .unwrap();
    tags::install(Some(Store::new(&tagsdir)));
    let t = tree(&lib);
    let mut seen = 0;
    for (p, f) in files(&t) {
        assert_eq!(text(&f, "TALB").as_deref(), Some("Mine"), "{p}: no suffix on a name the user gave");
        assert_eq!(text(&f, "TPE1").as_deref(), Some("Some One"), "{p}: not set by hand, from the path");
        match f.number {
            1 => assert_eq!(text(&f, "TIT2").as_deref(), Some("Real Title"), "{p}"),
            2 => assert_eq!(text(&f, "TIT2"), None, "{p}"),
            n => assert_eq!(text(&f, "TIT2"), Some(format!("Track {n:02}")), "{p}"),
        }
        assert!(f.tags.is_some(), "{p}");
        seen += 1;
    }
    tags::install(None);
    assert!(seen > 0, "{path}");
}

/// An extract copied from a mount has the generated title: nothing is lost with it, so `verify`
/// doesn't report it and `import` doesn't store it as if edited by hand, where it would keep a
/// lookup's title out (#185, phase 2). A title of any other form is the user's.
#[test]
fn import_leaves_out_a_generated_title() {
    let (dir, lib) = library(&[(NO_TEXT, "disc.iso")]);
    let t = tree(&lib);
    let by_number = |n| files(&t).into_values().find(|f| f.number == n && f.format() == Format::Dsf).unwrap();
    let engine = Engine::new(2, 32 << 20).unwrap();
    let ext = dir.path().join("ext");
    fs::create_dir_all(&ext).unwrap();
    for (n, title) in [(1, "Track 01"), (2, "Track 1")] {
        let f = by_number(n);
        let (bytes, _) = Reader::new(f.clone()).read(&engine, 0, f.size).unwrap();
        let end = f.data_end() as usize;
        let mut body = vec![3];
        body.extend_from_slice(title.as_bytes());
        let mut tag = b"ID3\x04\0\0\0\0\0".to_vec();
        tag.push(10 + body.len() as u8);
        tag.extend_from_slice(b"TIT2");
        tag.extend_from_slice(&[0, 0, 0, body.len() as u8, 0, 0]);
        tag.extend_from_slice(&body);
        let mut extract = bytes[..end].to_vec();
        extract.extend_from_slice(&tag);
        let total = extract.len() as u64;
        extract[12..20].copy_from_slice(&total.to_le_bytes());
        extract[20..28].copy_from_slice(&(end as u64).to_le_bytes());
        fs::write(ext.join(format!("{n:02} - a.dsf")), extract).unwrap();
    }
    let tagsdir = dir.path().join("tags");
    let args = verify::Args {
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
    let sum = sacdfs::tags_cli::import(&tagsdir, args).unwrap();
    assert_eq!(sum.files, 2, "{}", sum.render());
    let key = tags::disc_key(by_number(1).disc.toc());
    let imported = fs::read_to_string(tagsdir.join(format!("{key}.toml"))).unwrap();
    assert!(!imported.contains("Track 01"), "generated: {imported}");
    assert!(imported.contains("title = \"Track 1\""), "on track 2, not generated: {imported}");
    let report = fs::read_to_string(tagsdir.join(".last-import.tsv")).unwrap();
    let row = |name: &str| report.lines().find(|l| l.contains(name)).unwrap().to_owned();
    assert!(!row("01 - a.dsf").contains("TIT2"), "{report}");
    assert!(row("02 - a.dsf").contains("TIT2"), "{report}");
}
