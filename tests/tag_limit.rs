// SPDX-License-Identifier: GPL-3.0-or-later

//! A tags file's tag at scarletbook's `MAX_TAG_BYTES` boundary (scarletbook #343): a tag of
//! exactly `MAX_TAG_BYTES` is served, in the DSF file and in the WAV file's `id3 ` chunk, and one
//! byte more leaves the tags file out for that track, with the disc's own tag served as without
//! the tag layer. Both through scarletbook's tag builder (a text item) and through sacdfs's own
//! appended frames (`tags::append_frames`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use sacdfs::disc::{Disc, Tuning};
use sacdfs::tags::{self, Store};
use sacdfs::track::Format;
use sacdfs::tree::{SourceDir, Tree};
use scarletbook::output::MAX_TAG_BYTES;

const IMAGE: &str = "dsd-stereo-mch-2048";

/// The tag of the first stereo track's DSF and WAV files, with `toml` as the disc's tags file
/// (none if `None`).
fn tags_of(lib: &Path, tags_dir: &Path, key: &str, toml: Option<&str>) -> (Vec<u8>, Vec<u8>) {
    let file = tags_dir.join(format!("{key}.toml"));
    match toml {
        Some(t) => fs::write(&file, t).unwrap(),
        None => {
            let _ = fs::remove_file(&file);
        }
    }
    // A new store each time, so that nothing of the last tags file is kept.
    tags::install(Some(Store::new(tags_dir)));
    let tree = Tree::new(SourceDir::scan(lib).unwrap());
    let mut files: Vec<(PathBuf, _)> = tree.all_files();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let first = |format: Format| {
        let (_, f) = files
            .iter()
            .find(|(p, f)| p.to_string_lossy().contains("Stereo") && f.format() == format)
            .unwrap();
        f.tag().to_vec()
    };
    let got = (first(Format::Dsf), first(Format::Wav));
    tags::install(None);
    got
}

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn tag_at_the_size_limit() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let tags_dir = dir.path().join("tags");
    fs::create_dir_all(&lib).unwrap();
    fs::create_dir_all(&tags_dir).unwrap();
    let image = lib.join(format!("{IMAGE}.iso"));
    common::link(common::corpus().join("images").join(format!("{IMAGE}.iso")), &image).unwrap();
    let key = tags::disc_key(Disc::open(&image, 1, Tuning::default()).unwrap().toc());
    let (plain_dsf, plain_wav) = tags_of(&lib, &tags_dir, &key, None);
    assert!(!plain_dsf.is_empty());
    assert_eq!(plain_dsf, plain_wav, "the WAV file's id3 chunk is the DSF file's tag");

    // One track's value of `key` with `n` bytes, and the tag it gives.
    let with = |key_name: &str, n: usize| {
        let toml = format!("[track.1]\n\"{key_name}\" = \"{}\"\n", "x".repeat(n));
        tags_of(&lib, &tags_dir, &key, Some(&toml))
    };
    for key_name in ["title", "TBPM"] {
        // The tag grows by the value's length: find the length that makes it MAX_TAG_BYTES.
        let small = with(key_name, 1000).0.len();
        assert_eq!(with(key_name, 2000).0.len(), small + 1000, "{key_name}");
        let n = MAX_TAG_BYTES - (small - 1000);
        let (dsf, wav) = with(key_name, n);
        assert_eq!(dsf.len(), MAX_TAG_BYTES, "{key_name}: a tag of MAX_TAG_BYTES is served");
        assert!(dsf == wav, "{key_name}: the WAV file's id3 chunk differs");
        assert!(has(&dsf, &b"x".repeat(1000)), "{key_name}");
        // One byte more: that track's tags file is left out, the disc's own tag served.
        let (dsf, wav) = with(key_name, n + 1);
        // Compared, not printed: a failure would print a megabyte.
        assert!(dsf == plain_dsf, "{key_name}: over MAX_TAG_BYTES, a tag of {} bytes", dsf.len());
        assert!(wav == plain_wav, "{key_name}: over MAX_TAG_BYTES, a WAV tag of {} bytes", wav.len());
    }
}
