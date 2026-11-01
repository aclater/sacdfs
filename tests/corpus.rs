// SPDX-License-Identifier: GPL-3.0-or-later

//! Every case of the conformance corpus, read through sacdfs's tree and readers without
//! mounting: the files shown, their audio hashes, and random-offset reads.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use sacdfs::disc::Tuning;
use sacdfs::dsf::{BLOCK, FRAME_BYTES, HEADER_LEN};
use sacdfs::engine::Engine;
use sacdfs::track::{Reader, TrackFile};
use sacdfs::tree::{SourceDir, Tree};
use serde_json::Value;

use common::*;

fn read_all(engine: &Engine, file: &Arc<TrackFile>, chunk: u64) -> Vec<u8> {
    let reader = Reader::new(file.clone());
    let mut out = Vec::new();
    while (out.len() as u64) < file.size {
        let (data, ahead) = reader.read(engine, out.len() as u64, chunk).unwrap();
        assert!(!data.is_empty());
        out.extend_from_slice(&data);
        if let Some(a) = ahead {
            reader.read_ahead(engine, a);
        }
    }
    assert_eq!(out.len() as u64, file.size);
    out
}

fn files_of(tree: &Tree) -> BTreeMap<(String, u32), (PathBuf, Arc<TrackFile>)> {
    tree.all_files()
        .into_iter()
        .map(|(p, f)| {
            let area = p.parent().unwrap().file_name().unwrap().to_str().unwrap().to_owned();
            let n = track_number(p.file_name().unwrap().to_str().unwrap());
            ((area, n), (p, f))
        })
        .collect()
}

#[derive(Debug, Default)]
struct Stats {
    tracks: usize,
    seek_hints: usize,
    provisional_readings: u64,
    exact_readings: u64,
}

/// Checks one case with one search tuning and cache size.
fn check_case(case: &Value, tuning: Tuning, cache: usize, stats: &mut Stats) {
    let id = case["id"].as_str().unwrap();
    let tree = Tree::with_tuning(SourceDir::scan(&image(case)).unwrap(), tuning);
    let engine = Engine::new(2, cache).unwrap();
    let files = files_of(&tree);
    let expected = &case["expected"];
    if expected["outcome"] == "reject" {
        assert!(files.is_empty(), "{id}: rejected image shows files");
        assert!(
            tree.children(sacdfs::tree::ROOT).unwrap().is_empty(),
            "{id}: rejected image shows a directory"
        );
        return;
    }
    let mut want = BTreeMap::new();
    for area in expected["audio"].as_array().unwrap() {
        let dir = area_dir(area["area"].as_str().unwrap()).to_owned();
        for t in area["tracks"].as_array().unwrap() {
            let n = t["number"].as_u64().unwrap() as u32;
            if t["outcome"] == "ok" {
                want.insert((dir.clone(), n), t.clone());
            }
        }
    }
    let shown: Vec<_> = files.keys().cloned().collect();
    let wanted: Vec<_> = want.keys().cloned().collect();
    assert_eq!(shown, wanted, "{id}: files shown");
    for (key, t) in &want {
        let (path, file) = &files[key];
        assert_eq!(u64::from(file.start_frame), t["first_frame"].as_u64().unwrap(), "{id} {path:?}");
        assert_eq!(
            u64::from(file.start_frame + file.layout.frames),
            t["end_frame"].as_u64().unwrap(),
            "{id} {path:?}"
        );
        let whole = read_all(&engine, file, 128 << 10);
        let hashes: Vec<String> = dsf_channels(&whole).iter().map(|c| sha256(c)).collect();
        let expected: Vec<String> =
            t["sha256"].as_array().unwrap().iter().map(|h| h.as_str().unwrap().to_owned()).collect();
        assert_eq!(hashes, expected, "{id} {path:?}");
        // Odd chunk sizes cross every block and frame boundary differently.
        assert_eq!(read_all(&engine, file, 4099), whole, "{id} {path:?} in 4099-byte reads");
        stats.tracks += 1;
    }
    if let Some((_, (_, f))) = files.iter().next() {
        for a in 0..f.disc.areas.len() {
            stats.seek_hints += f.disc.seek_hints(a);
        }
        let (provisional, exact) = f.disc.readings();
        stats.provisional_readings += provisional;
        stats.exact_readings += exact;
    }
}

#[test]
fn corpus_with_default_search() {
    let mut stats = Stats::default();
    for case in cases() {
        check_case(&case, Tuning::default(), 64 << 20, &mut stats);
    }
    eprintln!("{stats:?}");
    assert!(stats.tracks > 100, "{stats:?}");
}

/// Every read that does not continue its cursor starts a new reading, provisionally wherever
/// the library allows it, and nothing is cached: every frame of every track must be found,
/// hostile images included.
#[test]
fn corpus_with_exhaustive_search_and_no_cache() {
    let mut stats = Stats::default();
    for case in cases() {
        check_case(&case, Tuning::exhaustive(), 0, &mut stats);
    }
    eprintln!("{stats:?}");
    assert!(stats.provisional_readings > 100 && stats.seek_hints > 100, "{stats:?}");
}

/// Random reads, each from a fresh reader (so every one locates its frames), equal the same
/// bytes of a straight read.
#[test]
fn random_reads_equal_straight_reads() {
    let mut files = Vec::new();
    for case in cases() {
        if case["expected"]["outcome"] != "ok" {
            continue;
        }
        let tree = Arc::new(Tree::with_tuning(SourceDir::scan(&image(&case)).unwrap(), Tuning::exhaustive()));
        for (_, f) in tree.all_files() {
            files.push(f);
        }
    }
    let engine = Engine::new(2, 0).unwrap();
    let wholes: Vec<Vec<u8>> = files.iter().map(|f| read_all(&engine, f, 1 << 20)).collect();
    let mut runner = TestRunner::new(Config { cases: 512, ..Config::default() });
    runner
        .run(&(0..files.len(), any::<u64>(), 1u64..300_000), |(i, off, len)| {
            let f = &files[i];
            let off = off % (f.size + 10);
            let reader = Reader::new(f.clone());
            let (data, _) = reader.read(&engine, off, len).unwrap();
            let s = (off.min(f.size)) as usize;
            let e = (off.saturating_add(len).min(f.size)) as usize;
            prop_assert_eq!(&data[..], &wholes[i][s..e]);
            Ok(())
        })
        .unwrap();
}

/// Every frame of every file, read on its own from a fresh reader with the exhaustive
/// search and no cache, equals the same bytes of a straight read: the start of each
/// channel's part of the frame, up to the end of its block. Unlike the random reads, this
/// reaches every frame, including the ones next to repeated or backwards timecodes.
#[test]
fn every_frame_alone_equals_straight_read() {
    let engine = Engine::new(2, 0).unwrap();
    let mut reads = 0;
    for case in cases() {
        if case["expected"]["outcome"] != "ok" {
            continue;
        }
        let id = case["id"].as_str().unwrap().to_owned();
        let tree = Tree::with_tuning(SourceDir::scan(&image(&case)).unwrap(), Tuning::exhaustive());
        for (path, f) in tree.all_files() {
            let whole = read_all(&engine, &f, 1 << 20);
            let c = u64::from(f.layout.channels);
            for k in 0..u64::from(f.layout.frames) {
                let b = k * FRAME_BYTES;
                for ch in 0..c {
                    let at = HEADER_LEN + (b / BLOCK) * BLOCK * c + ch * BLOCK + b % BLOCK;
                    let len = BLOCK - b % BLOCK;
                    let (data, _) = Reader::new(f.clone()).read(&engine, at, len).unwrap();
                    let (s, e) = (at as usize, (at + len) as usize);
                    assert!(data == whole[s..e], "{id} {path:?} frame {k} channel {ch}");
                    reads += 1;
                }
            }
        }
    }
    eprintln!("{reads} reads");
    assert!(reads > 1000);
}

/// Images in a directory tree: names, nesting, de-duplication and non-SACD files.
#[test]
fn directory_layout() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let src = |id: &str| corpus().join("images").join(format!("{id}.iso"));
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::create_dir_all(root.join("empty/deeper")).unwrap();
    std::fs::copy(src("dsd-stereo-mch-2048"), root.join("a/b/Disc One.iso")).unwrap();
    std::fs::copy(src("full-text"), root.join("a/x.ISO")).unwrap();
    std::fs::create_dir_all(root.join("a/x")).unwrap();
    std::fs::copy(src("no-text"), root.join("a/x/inner.iso")).unwrap();
    std::fs::write(root.join("a/not-sacd.iso"), vec![0u8; 4 << 20]).unwrap();
    std::fs::write(root.join("a/readme.txt"), b"hello").unwrap();
    let tree = Tree::new(SourceDir::scan(root).unwrap());
    let paths: Vec<String> = tree.all_files().iter().map(|(p, _)| p.display().to_string()).collect();
    let names = |ino| -> Vec<String> {
        tree.children(ino).unwrap().iter().map(|e| e.name.to_string_lossy().into_owned()).collect()
    };
    assert_eq!(names(sacdfs::tree::ROOT), ["a"]);
    let a = tree.resolve(std::path::Path::new("a")).unwrap();
    assert_eq!(names(a), ["b", "x", "x (2)"]);
    let disc = tree.resolve(std::path::Path::new("a/b/Disc One")).unwrap();
    assert_eq!(names(disc), ["Stereo", "Multichannel"]);
    assert!(paths.iter().any(|p| p.starts_with("a/b/Disc One/Stereo/01")));
    assert!(paths.iter().any(|p| p.starts_with("a/x/inner/Stereo/01")));
    assert!(paths.iter().any(|p| p.starts_with("a/x (2)/Stereo/01")));
    assert!(!paths.iter().any(|p| p.contains("not-sacd")));
}

/// The tags carry the disc and track text.
#[test]
fn tags_carry_text() {
    let case = cases().into_iter().find(|c| c["id"] == "full-text").unwrap();
    let tree = Tree::new(SourceDir::scan(&image(&case)).unwrap());
    let (_, f) = tree.all_files().into_iter().next().unwrap();
    let toc = &case["expected"]["toc"];
    let tag = f.tag();
    assert_eq!(&tag[..4], b"ID3\x04");
    let text = String::from_utf8_lossy(tag);
    let title = toc["areas"][0]["tracks"][0]["text"]["title"].as_str().unwrap();
    let album = toc["text"]["album_title"].as_str().unwrap();
    assert!(text.contains(title.trim()), "title {title:?} in tag");
    assert!(text.contains(album.trim()), "album {album:?} in tag");
    for id in ["TIT2", "TPE1", "TPE2", "TALB", "TRCK", "TCOM"] {
        assert!(text.contains(id), "{id} in tag");
    }
}

/// The DSF bytes of the whole corpus, pinned (#89): one SHA-256 over every shown file's
/// path and bytes, in tree order, with the tags removed (their changes belong to the tag
/// code; the audio and the headers are scarletbook's and ours). It catches a scarletbook
/// update that changes DSF output without the real discs; a change needs a `DSF_VERSION`
/// bump (src/ident.rs) and `GOLDEN_DSF_VERSION` with it.
#[test]
fn golden_dsf_corpus() {
    use sha2::{Digest, Sha256};
    let engine = Engine::new(2, 16 << 20).unwrap();
    let mut all = Sha256::new();
    let mut n = 0;
    for case in cases() {
        let tree = Tree::new(SourceDir::scan(&image(&case)).unwrap());
        let mut files = tree.all_files();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, f) in files.into_iter().filter(|(p, _)| p.extension().is_some_and(|e| e == "dsf")) {
            let whole = read_all(&engine, &f, 1 << 20);
            // Header, audio and padding only: up to the metadata chunk, if any.
            let meta = u64::from_le_bytes(whole[20..28].try_into().unwrap()) as usize;
            let body = if meta != 0 { &whole[..meta] } else { &whole[..] };
            all.update(case["id"].as_str().unwrap().as_bytes());
            all.update(path.to_string_lossy().as_bytes());
            all.update(sha256(body).as_bytes());
            n += 1;
        }
    }
    let hex: String = all.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect();
    println!("{n} files: {hex}");
    assert_eq!(sacdfs::ident::DSF_VERSION, GOLDEN_DSF_VERSION, "DSF_VERSION moved: update the golden");
    assert_eq!((n, hex.as_str()), GOLDEN_DSF, "DSF bytes changed: bump DSF_VERSION, then the golden (#89)");
}

/// The DSF output version (`sacdfs::ident::DSF_VERSION`) that [`GOLDEN_DSF`] was taken with.
const GOLDEN_DSF_VERSION: u32 = 3;
const GOLDEN_DSF: (usize, &str) = (457, "ad8dd50d1f81c966df7f472467311aba");
