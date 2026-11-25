// SPDX-License-Identifier: GPL-3.0-or-later

//! Converts a DSF file to a 24-bit WAV file with sacdfs's converter, as the WAV files of a
//! mount are made, for the quality suite (`suites/sacdfs.toml`).
//!
//! ```sh
//! cargo run --release --example dsf_to_wav -- [--rate HZ] [--cutoff 40|30|wide] [--filter precise|fast] IN.dsf OUT.wav
//! ```
//!
//! The DSF is split into SACD frames of 4704 bytes per channel (a short last frame is
//! padded with DSD silence) and each frame is converted with its neighbours, as the
//! filesystem does. The gain is 0 dB.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;

use sacdfs::pcm::{Converter, Cutoff, DSD_SILENCE, FRAME_BYTES, Filter, PcmConfig, PcmRate};

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// The header of a 24-bit PCM WAVE_FORMAT_EXTENSIBLE file with the default channel mask of
/// `channels` (stereo: FL FR), followed by a `data` chunk of `len` bytes.
fn header(channels: u16, rate: u32, len: u32) -> Vec<u8> {
    let block = channels * 3;
    let mask: u32 = match channels {
        1 => 0x4,
        2 => 0x3,
        _ => (1u32 << channels) - 1,
    };
    let mut h = Vec::new();
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(4 + 8 + 40 + 8 + len).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&40u32.to_le_bytes());
    h.extend_from_slice(&0xfffeu16.to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    h.extend_from_slice(&block.to_le_bytes());
    h.extend_from_slice(&24u16.to_le_bytes());
    h.extend_from_slice(&22u16.to_le_bytes());
    h.extend_from_slice(&24u16.to_le_bytes());
    h.extend_from_slice(&mask.to_le_bytes());
    h.extend_from_slice(&1u32.to_le_bytes());
    h.extend_from_slice(&[0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71]);
    h.extend_from_slice(b"data");
    h.extend_from_slice(&len.to_le_bytes());
    h
}

fn main() {
    let mut cfg = PcmConfig::default();
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--rate" => {
                cfg.rate = match args.next().unwrap().as_str() {
                    "88200" => PcmRate::R88200,
                    "176400" => PcmRate::R176400,
                    "352800" => PcmRate::R352800,
                    r => panic!("rate {r}"),
                };
            }
            "--cutoff" => {
                cfg.cutoff = match args.next().unwrap().as_str() {
                    "40" => Cutoff::Khz40,
                    "30" => Cutoff::Khz30,
                    "wide" => Cutoff::Wide80,
                    c => panic!("cutoff {c}"),
                };
            }
            // FILTER: the line the suite removes to build the 0.7.2 reference, which has no filter.
            "--filter" => {
                cfg.filter = match args.next().unwrap().as_str() {
                    "fast" => Filter::Fast,
                    "precise" => Filter::Precise,
                    f => panic!("filter {f}: use precise or fast"),
                }
            }
            _ if a.starts_with("--") => panic!("unknown option {a}"),
            _ => files.push(a),
        }
    }
    assert!(
        files.len() == 2,
        "usage: dsf_to_wav [--rate HZ] [--cutoff 40|30|wide] [--filter precise|fast] IN.dsf OUT.wav"
    );
    // The converter would fall back to the 40 kHz cutoff for this: refuse, so that no measurement
    // is of a different filter than the one asked for.
    cfg.validate().expect("the cutoff does not fit the rate");
    let dsf = std::fs::read(&files[0]).unwrap();
    assert!(&dsf[..4] == b"DSD ", "not a DSF file");
    let ch = u32_at(&dsf, 52) as usize;
    let per = (u64_at(&dsf, 64) / 8) as usize;
    let lsb_first = u32_at(&dsf, 60) == 1;
    let block = u32_at(&dsf, 72) as usize;
    let data = &dsf[92..];
    let frames = per.div_ceil(FRAME_BYTES);
    let mut planes = vec![Vec::with_capacity(frames * FRAME_BYTES); ch];
    for group in data.chunks(block * ch) {
        for (c, b) in group.chunks(block).enumerate().take(ch) {
            planes[c].extend(b.iter().map(|&x| if lsb_first { x.reverse_bits() } else { x }));
        }
    }
    let mut dsd = vec![DSD_SILENCE; frames * FRAME_BYTES * ch];
    for (c, p) in planes.iter().enumerate() {
        for (i, &b) in p[..per.min(p.len())].iter().enumerate() {
            dsd[i * ch + c] = b;
        }
    }
    let frame = |f: usize| &dsd[f * FRAME_BYTES * ch..(f + 1) * FRAME_BYTES * ch];
    let mut conv = Converter::new(ch as u8, &cfg);
    let mut pcm = vec![0u8; conv.frame_bytes()];
    let mut out = Vec::new();
    for f in 0..frames {
        let prev = f.checked_sub(1).map(frame);
        let next = (f + 1 < frames).then(|| frame(f + 1));
        conv.convert(prev, frame(f), next, &mut pcm);
        out.extend_from_slice(&pcm);
    }
    assert!(u32::try_from(out.len()).is_ok(), "the output is over 4 GiB: the header's length would overflow");
    let mut w = std::fs::File::create(&files[1]).unwrap();
    w.write_all(&header(ch as u16, cfg.rate.hz(), out.len() as u32)).unwrap();
    w.write_all(&out).unwrap();
}
