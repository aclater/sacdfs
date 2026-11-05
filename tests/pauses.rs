// SPDX-License-Identifier: GPL-3.0-or-later

//! `--pauses kept` (#112): each track is served with its pause.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::Path;

use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::track::{Reader, TrackFile};
use sacdfs::tree::{Output, SourceDir, Tree};
use sacdfs::verify::{Status, run};

const IMAGE: &str = "dsd-stereo-mch-2048";

fn library(dir: &Path) -> std::path::PathBuf {
    let lib = dir.join("lib");
    fs::create_dir_all(lib.join("a")).unwrap();
    std::os::unix::fs::symlink(
        common::corpus().join("images").join(format!("{IMAGE}.iso")),
        lib.join("a").join(format!("{IMAGE}.iso")),
    )
    .unwrap();
    lib
}

fn files(lib: &Path, kept: bool, dsf: bool) -> Vec<(std::path::PathBuf, std::sync::Arc<TrackFile>, Vec<u8>)> {
    let out = Output { dsf, wav: !dsf, pauses_kept: kept, ..Output::default() };
    let tree = Tree::with_options(SourceDir::scan(lib).unwrap(), Tuning::default(), out);
    let engine = Engine::new(2, 16 << 20).unwrap();
    tree.all_files()
        .into_iter()
        .map(|(p, f)| {
            let (bytes, _) = Reader::new(f.clone()).read(&engine, 0, f.size).unwrap();
            (p, f, bytes)
        })
        .collect()
}

#[test]
fn kept_tracks_are_the_toc_pause_plus_track_range() {
    common::skip!("demo: a guard someone added");
    let dir = tempfile::tempdir().unwrap();
    let lib = library(dir.path());
    let plain = files(&lib, false, true);
    let kept = files(&lib, true, true);
    assert_eq!(plain.len(), kept.len());
    let mut longer = 0;
    for ((pp, pf, pb), (kp, kf, kb)) in plain.iter().zip(&kept) {
        assert_eq!(pp, kp);
        let a = &kf.disc.toc().areas[kf.area];
        let (first, end) = scarletbook::audio::track_range_with_pause(a, u32::from(kf.number)).unwrap();
        assert_eq!((kf.start_frame, kf.layout.frames), (first, end - first), "{kp:?}");
        // The kept file is exactly the range's audio: 4704 bytes per channel per frame;
        // the plain file's audio sits inside it, after the lead-in.
        let (pc, kc) = (common::dsf_channels(pb), common::dsf_channels(kb));
        let lead = (pf.start_frame - first) as usize * 4704;
        for (p, k) in pc.iter().zip(&kc) {
            assert_eq!(k.len(), (end - first) as usize * 4704, "{kp:?}");
            assert_eq!(&k[lead..lead + p.len()], &p[..], "{kp:?}");
        }
        if kc[0].len() > pc[0].len() {
            longer += 1;
        }
    }
    // The corpus has tracks with a pause (track 1 starts after frame 0, or a gap follows).
    assert!(longer > 0, "no track of the image has a pause");
}

#[test]
fn default_is_unchanged_and_wav_follows() {
    let dir = tempfile::tempdir().unwrap();
    let lib = library(dir.path());
    let dflt = {
        let tree = Tree::with_options(SourceDir::scan(&lib).unwrap(), Tuning::default(), Output::dsf_only());
        tree.all_files().into_iter().map(|(_, f)| f.size).collect::<Vec<_>>()
    };
    let plain: Vec<u64> = files(&lib, false, true).iter().map(|(_, f, _)| f.size).collect();
    assert_eq!(dflt, plain);
    for (_, f, _) in files(&lib, true, false) {
        let a = &f.disc.toc().areas[f.area];
        let (first, end) = scarletbook::audio::track_range_with_pause(a, u32::from(f.number)).unwrap();
        assert_eq!((f.start_frame, f.layout.frames), (first, end - first));
    }
}

/// An extract holding exactly the kept file's audio is IDENTICAL with `--pauses kept`.
#[test]
fn verify_kept_extract_is_identical() {
    let dir = tempfile::tempdir().unwrap();
    let lib = library(dir.path());
    let kept = files(&lib, true, true);
    let x = lib.join("a").join("x");
    fs::create_dir_all(&x).unwrap();
    for (p, _, b) in &kept {
        fs::write(x.join(p.file_name().unwrap()), b).unwrap();
    }
    let args = |pauses: &str| sacdfs::verify::Args {
        pauses: pauses.into(),
        source: lib.clone(),
        extracts: vec![x.clone()],
        map: vec![],
        output: None,
        resume: false,
        threads: 2,
        cache_mb: 8,
        quiet: true,
        verbose: 0,
    };
    let s = run(&args("kept")).unwrap();
    assert!(!s.rows.is_empty());
    for r in &s.rows {
        assert_eq!(r.status, Status::Identical, "{:?}: {}", r.path, r.reason);
    }
}
