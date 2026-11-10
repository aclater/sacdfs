// SPDX-License-Identifier: GPL-3.0-or-later

//! The completeness check of the real-disc tests (#294): an image cut short fails it, a whole
//! one passes. It needs no real disc: the check is run on a corpus image and truncated copies.

mod common;

use std::fs::OpenOptions;

use common::*;

/// The size in bytes of the image up to the end of its last audio sector.
fn last_audio_byte(path: &std::path::Path) -> u64 {
    let disc = sacdfs::disc::Disc::open_with(path, 1, sacdfs::disc::Tuning::default(), None).unwrap();
    let toc = disc.toc();
    let end = toc.areas.iter().map(|a| u64::from(a.audio_end_lsn) + 1).max().unwrap();
    std::fs::metadata(path).unwrap().len() / toc.sectors * end
}

#[test]
fn a_whole_image_is_complete() {
    for name in ["dsd-stereo-mch-2048.iso", "dsd-stereo-2048.iso"] {
        let path = corpus().join("images").join(name);
        if path.exists() {
            assert_eq!(check_image_complete(&path), Ok(()), "{name}");
        }
    }
}

#[test]
fn a_truncated_copy_fails() {
    let src = corpus().join("images/dsd-stereo-mch-2048.iso");
    let len = std::fs::metadata(&src).unwrap().len();
    let dir = tempfile::tempdir().unwrap();
    let mut failures = 0;
    for (n, keep) in [last_audio_byte(&src) - 2048, last_audio_byte(&src) - 10 * 2048, len / 2, len * 9 / 10]
        .into_iter()
        .enumerate()
    {
        let copy = dir.path().join(format!("cut-{n}.iso"));
        std::fs::copy(&src, &copy).unwrap();
        OpenOptions::new().write(true).open(&copy).unwrap().set_len(keep).unwrap();
        // An image cut through its TOC may not open at all; either way the check fails.
        let err = check_image_complete(&copy).expect_err(&format!("kept {keep} of {len} bytes"));
        eprintln!("kept {keep} of {len}: {err}");
        failures += 1;
    }
    assert_eq!(failures, 4);
}

#[test]
fn a_truncation_that_leaves_the_toc_whole_says_truncated() {
    let src = corpus().join("images/dsd-stereo-mch-2048.iso");
    let len = std::fs::metadata(&src).unwrap().len();
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("cut.iso");
    std::fs::copy(&src, &copy).unwrap();
    let keep = last_audio_byte(&src) - 2048;
    assert!(keep < len);
    OpenOptions::new().write(true).open(&copy).unwrap().set_len(keep).unwrap();
    assert!(check_image_complete(&copy).unwrap_err().contains("truncated"));
}
