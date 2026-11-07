// SPDX-License-Identifier: GPL-3.0-or-later

//! `tags import` and a generated title (#185, phase 2). In a test binary of its own: the tag store
//! is process-wide, and `tests/fallback.rs` installs one for this same disc, whose title would
//! then count as the disc's.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::sync::Arc;

use sacdfs::engine::Engine;
use sacdfs::tags;
use sacdfs::track::{Format, Reader, TrackFile};
use sacdfs::tree::{Output, SourceDir, Tree};
use sacdfs::verify;

/// An extract copied from a mount has the generated title: nothing is lost with it, so `verify`
/// doesn't report it and `import` doesn't store it as if edited by hand, where it would keep a
/// lookup's title out (#185, phase 2). A title of any other form is the user's.
#[test]
fn import_leaves_out_a_generated_title() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    std::os::unix::fs::symlink(common::corpus().join("images/no-text.iso"), lib.join("disc.iso")).unwrap();
    let t = Tree::with_options(SourceDir::scan(&lib).unwrap(), Default::default(), Output::default());
    let by_number = |n| -> Arc<TrackFile> {
        t.all_files()
            .into_iter()
            .map(|(_, f)| f)
            .find(|f| f.number == n && f.format() == Format::Dsf)
            .unwrap()
    };
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
