// SPDX-License-Identifier: GPL-3.0-or-later

//! Helpers shared by the integration tests: the corpus manifest and DSF decoding.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// The corpus directory.
pub fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

/// The manifest's cases.
pub fn cases() -> Vec<Value> {
    let text = std::fs::read_to_string(corpus().join("manifest.json")).unwrap();
    let m: Value = serde_json::from_str(&text).unwrap();
    m["cases"].as_array().unwrap().clone()
}

/// The image of a case.
pub fn image(case: &Value) -> PathBuf {
    corpus().join("images").join(format!("{}.iso", case["id"].as_str().unwrap()))
}

/// The directory name of a slot.
pub fn area_dir(slot: &str) -> &'static str {
    match slot {
        "2ch" => "Stereo",
        "multichannel" => "Multichannel",
        s => panic!("unknown slot {s}"),
    }
}

/// The track number a file name starts with.
pub fn track_number(name: &str) -> u32 {
    name.split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Checks a whole DSF file's structure and returns each channel's audio as the corpus
/// hashes it: DSD bytes, earliest sample in the MSB, padding removed.
pub fn dsf_channels(file: &[u8]) -> Vec<Vec<u8>> {
    assert_eq!(&file[0..4], b"DSD ");
    assert_eq!(u64_at(file, 4), 28);
    assert_eq!(u64_at(file, 12), file.len() as u64, "total size");
    assert_eq!(&file[28..32], b"fmt ");
    let channels = u32_at(file, 52) as usize;
    assert_eq!(u32_at(file, 60), 1, "bits per sample");
    let samples = u64_at(file, 64);
    let block = u32_at(file, 72) as usize;
    assert_eq!(block, 4096);
    assert_eq!(&file[80..84], b"data");
    let data_len = (u64_at(file, 84) - 12) as usize;
    let data = &file[92..92 + data_len];
    let metadata = u64_at(file, 20) as usize;
    if metadata != 0 {
        assert_eq!(metadata, 92 + data_len);
        assert_eq!(&file[metadata..metadata + 3], b"ID3");
    } else {
        assert_eq!(file.len(), 92 + data_len);
    }
    let per = (samples / 8) as usize;
    assert_eq!(data_len, per.div_ceil(block) * block * channels);
    let mut out = vec![Vec::with_capacity(per); channels];
    for group in data.chunks(block * channels) {
        for (c, b) in group.chunks(block).enumerate() {
            out[c].extend_from_slice(b);
        }
    }
    for ch in &mut out {
        assert!(ch[per..].iter().all(|&b| b == 0), "padding is zero");
        ch.truncate(per);
        for b in ch.iter_mut() {
            *b = b.reverse_bits();
        }
    }
    out
}

/// Lower-case hex SHA-256.
pub fn sha256(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}
