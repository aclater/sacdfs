// SPDX-License-Identifier: GPL-3.0-or-later

//! The multichannel album suffix (#97): the album tag of the multichannel area's files ends
//! in " (Multichannel)", stereo files are unchanged, and `sacdfs verify` (audio only) still
//! calls an extract of a multichannel track identical whatever its tag says.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::sync::Arc;

use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::track::{Reader, TrackFile};
use sacdfs::tree::{Output, SourceDir, Tree};
use sacdfs::verify::{Args, Status, run};

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn multichannel_album_suffix_and_verify() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    let image = "dsd-stereo-mch-2048.iso";
    std::os::unix::fs::symlink(common::corpus().join("images").join(image), lib.join(image)).unwrap();
    let tree = Tree::with_options(SourceDir::scan(&lib).unwrap(), Tuning::default(), Output::dsf_only());
    let engine = Engine::new(2, 16 << 20).unwrap();
    let files: Vec<(std::path::PathBuf, Arc<TrackFile>, Vec<u8>)> = tree
        .all_files()
        .into_iter()
        .map(|(p, f)| {
            let (bytes, _) = Reader::new(f.clone()).read(&engine, 0, f.size).unwrap();
            (p, f, bytes)
        })
        .collect();
    let tag_of = |b: &[u8], f: &TrackFile| b[f.data_end() as usize..].to_vec();
    let mut mc = 0;
    let mut stereo = 0;
    for (p, f, b) in &files {
        let tag = tag_of(b, f);
        if p.to_string_lossy().contains("Multichannel") {
            mc += 1;
            assert!(has(&tag, b" (Multichannel)"), "{p:?}");
        } else {
            stereo += 1;
            assert!(!has(&tag, b"(Multichannel)"), "{p:?}");
        }
    }
    assert!(mc > 0 && stereo > 0);

    // Extracts of one multichannel track: as served (suffix), with the old tag (none), and
    // with another album. All three have the served audio, so all three are identical.
    let ext = lib.join("ext");
    fs::create_dir_all(&ext).unwrap();
    let (_, f, b) = files.iter().find(|(p, _, _)| p.to_string_lossy().contains("Multichannel")).unwrap();
    let end = f.data_end() as usize;
    fs::write(ext.join("01 - served.dsf"), b).unwrap();
    let mut untagged = b[..end].to_vec();
    let total = untagged.len() as u64;
    untagged[12..20].copy_from_slice(&total.to_le_bytes());
    untagged[20..28].copy_from_slice(&0u64.to_le_bytes());
    fs::write(ext.join("02 - untagged.dsf"), &untagged).unwrap();
    let s = run(&Args {
        source: lib.clone(),
        extracts: vec![lib.clone()],
        map: vec![],
        output: None,
        resume: false,
        threads: 2,
        cache_mb: 8,
        quiet: true,
        verbose: 0,
    })
    .unwrap();
    assert_eq!(s.rows.len(), 2);
    assert!(s.rows.iter().all(|r| r.status == Status::Identical), "{}", s.render());
}
