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

/// A parsed WAV file.
#[derive(Debug)]
pub struct Wav {
    pub rf64: bool,
    pub channels: u16,
    pub rate: u32,
    pub bits: u16,
    pub mask: u32,
    /// The data chunk body.
    pub data: std::ops::Range<usize>,
    /// Samples per channel (from ds64 for RF64, else from the data length).
    pub samples: u64,
    /// The `LIST`/`INFO` fields.
    pub info: Vec<(String, String)>,
    /// The body of the `id3 ` chunk.
    pub id3: Option<Vec<u8>>,
    /// Chunk ids in file order.
    pub chunks: Vec<String>,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

/// Parses and checks a whole WAV file: RIFF or RF64, `WAVE_FORMAT_EXTENSIBLE` 24-bit PCM,
/// sizes that add up exactly to the file length.
pub fn parse_wav(file: &[u8]) -> Wav {
    let rf64 = match &file[0..4] {
        b"RIFF" => false,
        b"RF64" => true,
        other => panic!("not a WAV file: {other:?}"),
    };
    assert_eq!(&file[8..12], b"WAVE");
    let mut riff_size = u64::from(u32_at(file, 4));
    let mut data_size64 = None;
    let mut samples64 = None;
    let mut at = 12;
    let mut w = Wav {
        rf64,
        channels: 0,
        rate: 0,
        bits: 0,
        mask: 0,
        data: 0..0,
        samples: 0,
        info: Vec::new(),
        id3: None,
        chunks: Vec::new(),
    };
    while at < file.len() {
        let id = std::str::from_utf8(&file[at..at + 4]).unwrap().to_owned();
        let mut size = u64::from(u32_at(file, at + 4));
        let body = at + 8;
        if id == "data" && rf64 {
            assert_eq!(size, u64::from(u32::MAX));
            size = data_size64.unwrap();
        }
        let end = body + size as usize;
        assert!(end <= file.len(), "chunk {id} runs past the end");
        match id.as_str() {
            "ds64" => {
                assert!(rf64 && w.chunks.is_empty(), "ds64 first, in RF64 only");
                assert_eq!(u32_at(file, 4), u32::MAX);
                riff_size = u64_at(file, body);
                data_size64 = Some(u64_at(file, body + 8));
                samples64 = Some(u64_at(file, body + 16));
            }
            "fmt " => {
                assert_eq!(size, 40);
                assert_eq!(u16_at(file, body), 0xFFFE, "WAVE_FORMAT_EXTENSIBLE");
                w.channels = u16_at(file, body + 2);
                w.rate = u32_at(file, body + 4);
                let align = u16_at(file, body + 12);
                w.bits = u16_at(file, body + 14);
                assert_eq!(align, w.channels * w.bits / 8);
                assert_eq!(u32_at(file, body + 8), w.rate * u32::from(align));
                assert_eq!(u16_at(file, body + 16), 22);
                assert_eq!(u16_at(file, body + 18), w.bits, "valid bits");
                w.mask = u32_at(file, body + 20);
                assert_eq!(
                    &file[body + 24..body + 40],
                    &[0x01, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71],
                    "PCM subformat"
                );
            }
            "data" => {
                w.data = body..end;
                let align = u64::from(w.channels) * 3;
                assert_eq!(size % align, 0);
                w.samples = samples64.unwrap_or(size / align);
                assert_eq!(w.samples, size / align);
            }
            "LIST" => {
                assert_eq!(&file[body..body + 4], b"INFO");
                let mut p = body + 4;
                while p < end {
                    let fid = std::str::from_utf8(&file[p..p + 4]).unwrap().to_owned();
                    let n = u32_at(file, p + 4) as usize;
                    let text = &file[p + 8..p + 8 + n];
                    assert_eq!(text.last(), Some(&0), "INFO strings end in NUL");
                    w.info.push((fid, String::from_utf8(text[..n - 1].to_vec()).unwrap()));
                    p += 8 + n + n % 2;
                }
                assert_eq!(p, end);
            }
            "id3 " => w.id3 = Some(file[body..end].to_vec()),
            "JUNK" => assert!(file[body..end].iter().all(|&b| b == 0), "JUNK is zeros"),
            other => panic!("unexpected chunk {other:?}"),
        }
        w.chunks.push(id);
        at = end + (size as usize % 2);
    }
    assert_eq!(at, file.len());
    assert_eq!(riff_size, file.len() as u64 - 8, "RIFF size");
    assert_eq!(w.bits, 24);
    w
}
