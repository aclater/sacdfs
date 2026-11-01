// SPDX-License-Identifier: GPL-3.0-or-later

//! Converts one track of an SACD image to WAV with sacdfs's converter, for comparisons
//! with other converters. It also writes the track's DSF file (as the mount shows it), so
//! that other tools can convert exactly the same DSD.
//!
//! ```sh
//! cargo run --release --example pcm_track -- IMAGE AREA TRACK OUTDIR
//! ```
//!
//! AREA is `Stereo` or `Multichannel`, TRACK the track number. Writes `OUTDIR/track.dsf`
//! and `OUTDIR/sacdfs-RATE-CUTOFF.wav` (24-bit) for every rate and cutoff, plus
//! `OUTDIR/sacdfs-RATE-CUTOFF.f32.wav` (the same samples as 32-bit float). The track is
//! converted frame by frame, as the filesystem will: DSD silence before its first and after
//! its last frame.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use sacdfs::engine::Engine;
use sacdfs::pcm::{Converter, Cutoff, FRAME_BYTES, PcmConfig, PcmRate};
use sacdfs::track::Reader;
use sacdfs::tree::{SourceDir, Tree};

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// A WAV file (WAVE_FORMAT_EXTENSIBLE, 24-bit integer or 32-bit float).
fn write_wav(path: &Path, channels: u16, rate: u32, float: bool, data: &[u8]) {
    let bits: u16 = if float { 32 } else { 24 };
    let block = channels * bits / 8;
    let mut h = Vec::new();
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(4 + 8 + 40 + 8 + data.len() as u32).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&40u32.to_le_bytes());
    h.extend_from_slice(&0xfffeu16.to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    h.extend_from_slice(&block.to_le_bytes());
    h.extend_from_slice(&bits.to_le_bytes());
    h.extend_from_slice(&22u16.to_le_bytes());
    h.extend_from_slice(&bits.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    // KSDATAFORMAT_SUBTYPE_PCM or _IEEE_FLOAT.
    h.extend_from_slice(&(if float { 3u32 } else { 1u32 }).to_le_bytes());
    h.extend_from_slice(&[0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71]);
    h.extend_from_slice(b"data");
    h.extend_from_slice(&(data.len() as u32).to_le_bytes());
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(&h).unwrap();
    f.write_all(data).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert!(args.len() == 5, "usage: pcm_track IMAGE AREA TRACK OUTDIR");
    let (image, area, track, out) =
        (&args[1], &args[2], args[3].parse::<u32>().unwrap(), Path::new(&args[4]));
    std::fs::create_dir_all(out).unwrap();
    let tree = Arc::new(Tree::new(SourceDir::scan(Path::new(image)).unwrap()));
    let (_, file) = tree
        .all_files()
        .into_iter()
        .find(|(p, _)| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            p.parent().unwrap().ends_with(area)
                && name.split(|c: char| !c.is_ascii_digit()).next() == Some(&format!("{track:02}"))
        })
        .expect("no such track");
    let engine = Engine::new(sacdfs::engine::default_threads(), 256 << 20).unwrap();
    let reader = Reader::new(file.clone());
    let mut dsf = Vec::new();
    while (dsf.len() as u64) < file.size {
        let (data, ahead) = reader.read(&engine, dsf.len() as u64, 1 << 20).unwrap();
        dsf.extend_from_slice(&data);
        if let Some(a) = ahead {
            reader.read_ahead(&engine, a);
        }
    }
    std::fs::write(out.join("track.dsf"), &dsf).unwrap();

    // DSF to interleaved MSB-first frames.
    let ch = u32_at(&dsf, 52) as usize;
    let per = (u64_at(&dsf, 64) / 8) as usize;
    let data = &dsf[92..];
    let mut planes = vec![Vec::with_capacity(per); ch];
    for group in data.chunks(4096 * ch) {
        for (c, b) in group.chunks(4096).enumerate() {
            planes[c].extend(b.iter().map(|x| x.reverse_bits()));
        }
    }
    let frames = per / FRAME_BYTES;
    let mut dsd = vec![0u8; frames * FRAME_BYTES * ch];
    for (c, p) in planes.iter().enumerate() {
        for (i, &b) in p[..frames * FRAME_BYTES].iter().enumerate() {
            dsd[i * ch + c] = b;
        }
    }
    let frame = |f: usize| &dsd[f * FRAME_BYTES * ch..(f + 1) * FRAME_BYTES * ch];
    eprintln!("{ch} channels, {frames} frames ({:.1} s)", frames as f64 / 75.0);

    for rate in [PcmRate::R88200, PcmRate::R176400, PcmRate::R352800] {
        for cutoff in [Cutoff::Khz40, Cutoff::Khz30, Cutoff::Wide80] {
            let cfg = PcmConfig { rate, cutoff, gain_db: 0.0 };
            if cfg.validate().is_err() {
                continue;
            }
            let mut conv = Converter::new(ch as u8, &cfg);
            let mut pcm = vec![0u8; conv.frame_bytes()];
            let mut flt = vec![0f32; rate.samples_per_frame() * ch];
            let (mut out24, mut out32) = (Vec::new(), Vec::new());
            let mut clips = 0;
            let t = std::time::Instant::now();
            for f in 0..frames {
                let prev = f.checked_sub(1).map(frame);
                let next = (f + 1 < frames).then(|| frame(f + 1));
                clips += conv.convert(prev, frame(f), next, &mut pcm);
                out24.extend_from_slice(&pcm);
            }
            let secs = t.elapsed().as_secs_f64();
            for f in 0..frames {
                let prev = f.checked_sub(1).map(frame);
                let next = (f + 1 < frames).then(|| frame(f + 1));
                conv.convert_f32(prev, frame(f), next, &mut flt);
                out32.extend(flt.iter().flat_map(|v| v.to_le_bytes()));
            }
            let khz = match cutoff {
                Cutoff::Khz40 => 40,
                Cutoff::Khz30 => 30,
                Cutoff::Wide80 => 80,
            };
            let name = format!("sacdfs-{}-{khz}", rate.hz());
            write_wav(&out.join(format!("{name}.wav")), ch as u16, rate.hz(), false, &out24);
            write_wav(&out.join(format!("{name}.f32.wav")), ch as u16, rate.hz(), true, &out32);
            eprintln!("{name}: {clips} clips, {:.1}x real time on one thread", frames as f64 / 75.0 / secs);
        }
    }
}
