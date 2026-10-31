// SPDX-License-Identifier: GPL-3.0-or-later

//! The WAV files of every case of the conformance corpus, read through sacdfs's tree and
//! readers without mounting: structure, exact sizes, tags, the area-stream reference
//! (gapless joins), random and per-frame reads, and that listing never decodes.
//!
//! Nothing here depends on how the converter filters: every check holds for any converter
//! whose output for a frame depends only on that frame and its two neighbours.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use sacdfs::disc::{Disc, Tuning};
use sacdfs::engine::Engine;
use sacdfs::pcm::{Converter, Cutoff, PcmConfig, PcmRate};
use sacdfs::track::{Format, Reader, TrackFile};
use sacdfs::tree::{Output, SourceDir, Tree};
use sacdfs::wav;
use scarletbook::{DstDecoder, TrackFrame};
use serde_json::Value;

use common::*;

const RATES: [PcmRate; 3] = [PcmRate::R88200, PcmRate::R176400, PcmRate::R352800];

fn config(rate: PcmRate) -> PcmConfig {
    PcmConfig { rate, ..PcmConfig::default() }
}

fn output(rate: PcmRate, dsf: bool) -> Output {
    Output { dsf, wav: true, samples_per_frame: rate.samples_per_frame() as u32 }
}

fn ok_cases() -> Vec<Value> {
    cases().into_iter().filter(|c| c["expected"]["outcome"] == "ok").collect()
}

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

fn wav_files(tree: &Tree) -> Vec<(std::path::PathBuf, Arc<TrackFile>)> {
    tree.all_files().into_iter().filter(|(_, f)| f.format() == Format::Wav).collect()
}

/// The PCM of every timeline position `[0, end)` of an area, converted the simple way:
/// the area's DSD frames scanned in order from its first sector, each converted with a
/// fresh converter from the frame before and after it (silence outside the area). A WAV
/// file of a track must hold exactly the positions of the track.
fn area_reference(disc: &Disc, area: usize, end: u32, area_end: u32, cfg: &PcmConfig) -> Vec<Vec<u8>> {
    let channels = disc.areas[area].channels;
    let mut cursor = disc.locate(area, 0).unwrap();
    let mut decoder = DstDecoder::new();
    let dsd_end = (end + 1).min(area_end);
    let dsd: Vec<Vec<u8>> = (0..dsd_end)
        .map(|t| {
            let content = cursor.content(disc, t).unwrap();
            scarletbook::audio::decode_frame(&mut decoder, TrackFrame { number: t, content }, channels)
                .unwrap()
                .data
        })
        .collect();
    let mut conv = Converter::new(channels, cfg);
    (0..end)
        .map(|t| {
            let i = t as usize;
            let prev = (t > 0).then(|| dsd[i - 1].as_slice());
            let next = (t + 1 < area_end).then(|| dsd[i + 1].as_slice());
            let mut out = vec![0u8; conv.frame_bytes()];
            conv.convert(prev, &dsd[i], next, &mut out);
            out
        })
        .collect()
}

/// Checks the WAV files of one case at one rate: structure, sizes, tags (the same as the
/// DSF file's), chunked reads, and the audio against the area-stream reference.
fn check_case(case: &Value, rate: PcmRate, tuning: Tuning, cache: usize) -> usize {
    let id = case["id"].as_str().unwrap();
    let cfg = config(rate);
    let tree = Tree::with_options(SourceDir::scan(&image(case)).unwrap(), tuning, output(rate, true));
    let engine = Engine::with_pcm(2, cache, cfg).unwrap();
    let all = tree.all_files();
    let dsf: BTreeMap<_, _> = all
        .iter()
        .filter(|(_, f)| f.format() == Format::Dsf)
        .map(|(p, f)| (p.with_extension(""), f.clone()))
        .collect();
    let wavs: Vec<_> = all.iter().filter(|(_, f)| f.format() == Format::Wav).cloned().collect();
    assert_eq!(wavs.len(), dsf.len(), "{id}: one WAV per DSF");
    let mut references: BTreeMap<usize, Vec<Vec<u8>>> = BTreeMap::new();
    let reference_disc = Disc::open(&image(case), u32::MAX, Tuning::default()).unwrap();
    for (path, f) in &wavs {
        let d = &dsf[&path.with_extension("")];
        assert_eq!((d.start_frame, d.layout), (f.start_frame, f.layout), "{id} {path:?}");
        let spf = rate.samples_per_frame() as u64;
        let c = u64::from(f.layout.channels);
        let whole = read_all(&engine, f, 128 << 10);
        let w = parse_wav(&whole);
        // Exact sizes, known from the TOC alone.
        let data_len = u64::from(f.layout.frames) * spf * c * 3;
        assert_eq!(w.data.len() as u64, data_len, "{id} {path:?}");
        assert!(!w.rf64);
        // The audio starts past the read-ahead of a header read and ends on a page boundary,
        // so that reading the tags never needs audio.
        assert_eq!(w.data.start as u64, f.header_len());
        assert!(w.data.start as u64 >= wav::AUDIO_START_MIN);
        assert_eq!(w.data.end as u64 % wav::PAGE, 0);
        assert_eq!(u32::from(w.channels), u32::from(f.layout.channels));
        assert_eq!(w.rate, rate.hz());
        assert_eq!(w.mask, wav::channel_mask(f.layout.channels));
        assert_eq!(w.mask.count_ones(), u32::from(w.channels));
        assert_eq!(w.samples, u64::from(f.layout.frames) * spf);
        // The same ID3v2.4 tag as the DSF file, and INFO fields that agree with it.
        assert_eq!(w.id3.as_deref().unwrap_or(&[]), d.tag(), "{id} {path:?}");
        assert_eq!(f.tag(), d.tag());
        let id3 = String::from_utf8_lossy(d.tag()).into_owned();
        for (field, text) in &w.info {
            assert!(["INAM", "IART", "IPRD", "ITRK", "ICRD", "IGNR"].contains(&field.as_str()));
            assert!(id3.contains(text.as_str()), "{id} {path:?}: {field} {text:?} not in the ID3 tag");
        }
        let order: Vec<&str> = w
            .chunks
            .iter()
            .map(String::as_str)
            .filter(|c| *c != "LIST" && *c != "id3 " && *c != "JUNK")
            .collect();
        assert_eq!(order, ["fmt ", "data"]);
        // Odd chunk sizes cross every frame boundary differently.
        assert_eq!(read_all(&engine, f, 4099), whole, "{id} {path:?} in 4099-byte reads");
        // The audio is the track's positions of one stream of the whole area: the joins
        // between tracks are those of the area, not silence (gapless).
        let end = f.start_frame + f.layout.frames;
        let reference = references.entry(f.area).or_default();
        if reference.len() < end as usize {
            let shown_end = wavs
                .iter()
                .filter(|(_, g)| g.area == f.area)
                .map(|(_, g)| g.start_frame + g.layout.frames)
                .max()
                .unwrap();
            *reference = area_reference(&reference_disc, f.area, shown_end, f.area_end, &cfg);
        }
        let want: Vec<u8> = reference[f.start_frame as usize..end as usize].concat();
        assert!(whole[w.data.clone()] == want[..], "{id} {path:?}: audio differs from the area stream");
    }
    // The area's WAV files, concatenated in track order, are the same stream over the
    // same positions.
    for (area, reference) in &references {
        let mut tracks: Vec<_> = wavs.iter().filter(|(_, f)| f.area == *area).collect();
        tracks.sort_by_key(|(_, f)| f.start_frame);
        let joined: Vec<u8> = tracks
            .iter()
            .flat_map(|(_, f)| {
                let whole = read_all(&engine, f, 1 << 20);
                whole[f.header_len() as usize..f.data_end() as usize].to_vec()
            })
            .collect();
        let want: Vec<u8> = tracks
            .iter()
            .flat_map(|(_, f)| {
                reference[f.start_frame as usize..(f.start_frame + f.layout.frames) as usize].concat()
            })
            .collect();
        assert!(joined == want, "{id} area {area}: concatenated WAV files differ from the area stream");
    }
    wavs.len()
}

#[test]
fn corpus_wav_every_rate() {
    for rate in RATES {
        let mut files = 0;
        for case in ok_cases() {
            files += check_case(&case, rate, Tuning::default(), 64 << 20);
        }
        eprintln!("{} Hz: {files} WAV files", rate.hz());
        assert!(files > 100);
    }
}

/// Every read that does not continue its cursor searches, and nothing is cached.
#[test]
fn corpus_wav_exhaustive_search_no_cache() {
    let mut files = 0;
    for case in ok_cases() {
        files += check_case(&case, PcmRate::R176400, Tuning::exhaustive(), 0);
    }
    assert!(files > 100);
}

/// Random reads of WAV files at every rate, each from a fresh reader (so every one locates
/// its frames and decodes its neighbours), equal the same bytes of a straight read.
#[test]
fn random_wav_reads_equal_straight_reads() {
    for rate in RATES {
        let engine = Engine::with_pcm(2, 0, config(rate)).unwrap();
        let mut files = Vec::new();
        for case in ok_cases() {
            let tree = Tree::with_options(
                SourceDir::scan(&image(&case)).unwrap(),
                Tuning::exhaustive(),
                output(rate, false),
            );
            files.extend(wav_files(&tree).into_iter().map(|(_, f)| f));
        }
        let wholes: Vec<Vec<u8>> = files.iter().map(|f| read_all(&engine, f, 1 << 20)).collect();
        let mut runner = TestRunner::new(Config { cases: 256, ..Config::default() });
        runner
            .run(&(0..files.len(), any::<u64>(), 1u64..400_000), |(i, off, len)| {
                let f = &files[i];
                let off = off % (f.size + 10);
                let (data, _) = Reader::new(f.clone()).read(&engine, off, len).unwrap();
                let s = (off.min(f.size)) as usize;
                let e = (off.saturating_add(len).min(f.size)) as usize;
                prop_assert_eq!(&data[..], &wholes[i][s..e]);
                Ok(())
            })
            .unwrap();
    }
}

/// Every frame of every WAV file, read on its own from a fresh reader with the exhaustive
/// search and no cache, equals the same bytes of a straight read; so do a few bytes across
/// every join of two frames.
#[test]
fn every_wav_frame_alone_equals_straight_read() {
    let engine = Engine::with_pcm(2, 0, PcmConfig::default()).unwrap();
    let mut reads = 0;
    for case in ok_cases() {
        let id = case["id"].as_str().unwrap().to_owned();
        let tree = Tree::with_options(
            SourceDir::scan(&image(&case)).unwrap(),
            Tuning::exhaustive(),
            output(PcmRate::R176400, false),
        );
        for (path, f) in wav_files(&tree) {
            let whole = read_all(&engine, &f, 1 << 20);
            let fb = match f.audio {
                sacdfs::track::Audio::Wav(l) => l.frame_bytes(),
                _ => unreachable!(),
            };
            let h = f.header_len();
            for k in 0..u64::from(f.layout.frames) {
                let at = h + k * fb;
                for (s, len) in [(at, fb), (at.saturating_sub(6).max(h), 12)] {
                    let (data, _) = Reader::new(f.clone()).read(&engine, s, len).unwrap();
                    let e = (s + len).min(f.size);
                    assert!(data == whole[s as usize..e as usize], "{id} {path:?} frame {k} at {s}");
                    reads += 1;
                }
            }
        }
    }
    eprintln!("{reads} reads");
    assert!(reads > 1000);
}

/// Listing, sizes, and reading the header and tags of every file decode and convert
/// nothing: no audio sector is read and nothing enters the frame cache.
#[test]
fn listing_and_tags_never_decode() {
    let engine = Engine::new(2, 64 << 20).unwrap();
    let tree = Tree::new(SourceDir::scan(&corpus().join("images")).unwrap());
    let files = tree.all_files();
    assert!(files.iter().any(|(_, f)| f.format() == Format::Wav));
    assert!(files.iter().any(|(_, f)| f.format() == Format::Dsf));
    for (path, f) in &files {
        let reader = Reader::new(f.clone());
        let (head, ahead) = reader.read(&engine, 0, f.header_len()).unwrap();
        assert_eq!(head.len() as u64, f.header_len());
        assert!(ahead.is_none(), "{path:?}: read-ahead after a header read");
        let tail = f.size - f.data_end();
        let (t, ahead) = reader.read(&engine, f.data_end(), tail + 100).unwrap();
        assert!(ahead.is_none(), "{path:?}: read-ahead after a tag read");
        assert_eq!(t.len() as u64, tail, "{path:?}");
        if !f.tag().is_empty() {
            assert!(t.windows(f.tag().len()).any(|w| w == f.tag()), "{path:?}: tag in the trailer");
        }
    }
    for (_, f) in &files {
        assert_eq!(f.disc.sector_reads(), 0, "{:?}: audio read while listing", f.disc.path);
    }
    assert_eq!(engine.cache().bytes(), 0);
}

/// A sequential read of a WAV file decodes each DSD frame once: it makes about as many
/// calls to the image as a straight read of the DSF file (the WAV needs one more frame, the
/// neighbour after the track's last).
#[test]
fn sequential_wav_read_decodes_each_frame_once() {
    let mut compared = 0;
    for case in ok_cases() {
        let calls = |format: Format| -> Vec<u64> {
            let out = Output { dsf: format == Format::Dsf, wav: format == Format::Wav, ..Output::default() };
            let tree = Tree::with_options(SourceDir::scan(&image(&case)).unwrap(), Tuning::default(), out);
            let engine = Engine::new(2, 64 << 20).unwrap();
            tree.all_files()
                .into_iter()
                .map(|(_, f)| {
                    let before = f.disc.sector_reads();
                    read_all(&engine, &f, 128 << 10);
                    f.disc.sector_reads() - before
                })
                .collect()
        };
        let (d, w) = (calls(Format::Dsf), calls(Format::Wav));
        assert_eq!(d.len(), w.len());
        for (d, w) in d.iter().zip(&w) {
            assert!(*w <= d + 3, "{}: WAV read made {w} image calls, DSF {d}", case["id"]);
            compared += 1;
        }
    }
    assert!(compared > 100);
}

/// Other settings change the audio but not the layout; a large gain clips, and clipping is
/// clamped, not an error.
#[test]
fn settings_keep_layout() {
    let case = ok_cases().into_iter().find(|c| c["id"] == "dsd-stereo-mch-2048").unwrap();
    let base = Engine::new(2, 0).unwrap();
    let tree = Tree::with_options(
        SourceDir::scan(&image(&case)).unwrap(),
        Tuning::default(),
        output(PcmRate::R176400, false),
    );
    let files = wav_files(&tree);
    for cfg in [
        PcmConfig { cutoff: Cutoff::Khz30, ..PcmConfig::default() },
        PcmConfig { gain_db: 24.0, ..PcmConfig::default() },
        PcmConfig { gain_db: -60.0, ..PcmConfig::default() },
    ] {
        let engine = Engine::with_pcm(2, 0, cfg).unwrap();
        for (path, f) in &files {
            let a = read_all(&base, f, 1 << 20);
            let b = read_all(&engine, f, 1 << 20);
            assert_eq!(a.len(), b.len());
            assert_eq!(a[..f.header_len() as usize], b[..f.header_len() as usize], "{path:?}");
            assert_eq!(a[f.data_end() as usize..], b[f.data_end() as usize..], "{path:?}");
        }
    }
    // A file made for one rate refuses an engine with another, instead of serving wrong
    // bytes.
    let engine = Engine::with_pcm(2, 0, config(PcmRate::R88200)).unwrap();
    let (_, f) = &files[0];
    assert!(Reader::new(f.clone()).read(&engine, f.header_len(), 100).is_err());
}

/// DSF and WAV side by side: one WAV next to each DSF, with the same name but for the
/// extension; `Output` selects either.
#[test]
fn side_by_side_names() {
    let case = ok_cases().into_iter().find(|c| c["id"] == "full-text").unwrap();
    let names = |out: Output| -> Vec<String> {
        let tree = Tree::with_options(SourceDir::scan(&image(&case)).unwrap(), Tuning::default(), out);
        tree.all_files().iter().map(|(p, _)| p.display().to_string()).collect()
    };
    let both = names(Output::default());
    let dsf = names(Output::dsf_only());
    let wav = names(Output { dsf: false, ..Output::default() });
    assert!(!dsf.is_empty());
    assert_eq!(both.len(), 2 * dsf.len());
    assert!(dsf.iter().all(|n| n.ends_with(".dsf")));
    assert!(wav.iter().all(|n| n.ends_with(".wav")));
    let stems = |v: &[String]| -> Vec<String> { v.iter().map(|n| n[..n.len() - 4].to_owned()).collect() };
    assert_eq!(stems(&dsf), stems(&wav));
    let mut merged: Vec<String> = dsf.iter().chain(&wav).cloned().collect();
    merged.sort();
    assert_eq!(merged, both);
}

/// The same checks on real discs, run locally: `SACDFS_REAL_IMAGES=DIR cargo test --release
/// --test wav real_discs -- --ignored --nocapture` (optionally `SACDFS_PCM_RATE=88200`).
/// Every WAV file's audio equals its positions of the area stream (compared by SHA-256,
/// streaming, so that a whole area need not fit in memory), and random ranges equal the
/// straight read.
#[test]
#[ignore = "needs real disc images: set SACDFS_REAL_IMAGES"]
fn real_discs() {
    use sha2::{Digest, Sha256};
    let Some(dir) = std::env::var_os("SACDFS_REAL_IMAGES") else { return };
    let rate = match std::env::var("SACDFS_PCM_RATE").ok().as_deref() {
        Some("88200") => PcmRate::R88200,
        Some("352800") => PcmRate::R352800,
        _ => PcmRate::R176400,
    };
    let cfg = config(rate);
    let tree = Tree::with_options(
        SourceDir::scan(std::path::Path::new(&dir)).unwrap(),
        Tuning::default(),
        output(rate, false),
    );
    let engine = Engine::with_pcm(0, 256 << 20, cfg).unwrap();
    let files = wav_files(&tree);
    // Group by disc and area.
    let mut areas: BTreeMap<(std::path::PathBuf, usize), Vec<Arc<TrackFile>>> = BTreeMap::new();
    for (_, f) in &files {
        areas.entry((f.disc.path.clone(), f.area)).or_default().push(f.clone());
    }
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut total = 0;
    for ((path, area), mut tracks) in areas {
        tracks.sort_by_key(|f| f.start_frame);
        let t0 = std::time::Instant::now();
        // The WAV files, read straight, and random ranges of them.
        let mut got = Vec::new();
        for f in &tracks {
            let whole = read_all(&engine, f, 1 << 20);
            let w = parse_wav(&whole);
            assert_eq!(w.samples, u64::from(f.layout.frames) * rate.samples_per_frame() as u64);
            assert_eq!(w.data.len() as u64, f.data_end() - f.header_len());
            for _ in 0..16 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let off = seed % f.size;
                let len = (seed >> 44) % (3 << 20) + 1;
                let (data, _) = Reader::new(f.clone()).read(&engine, off, len).unwrap();
                let e = (off + len).min(f.size) as usize;
                assert!(data == whole[off as usize..e], "{path:?} area {area} track {} at {off}", f.number);
            }
            got.push(Sha256::digest(&whole[w.data.clone()]).to_vec());
        }
        let read_s = t0.elapsed().as_secs_f64();
        // The area stream, frame by frame, hashed per track.
        let disc = Disc::open(&path, u32::MAX, Tuning::default()).unwrap();
        let channels = disc.areas[area].channels;
        let end = tracks.iter().map(|f| f.start_frame + f.layout.frames).max().unwrap();
        let area_end = tracks[0].area_end;
        let mut cursor = disc.locate(area, 0).unwrap();
        let mut decoder = DstDecoder::new();
        let mut conv = Converter::new(channels, &cfg);
        let mut decode = |t: u32| -> Vec<u8> {
            let content = cursor.content(&disc, t).unwrap();
            scarletbook::audio::decode_frame(&mut decoder, TrackFrame { number: t, content }, channels)
                .unwrap()
                .data
        };
        let mut hashers: Vec<Sha256> = tracks.iter().map(|_| Sha256::new()).collect();
        let (mut prev, mut cur) = (None::<Vec<u8>>, decode(0));
        let mut out = vec![0u8; conv.frame_bytes()];
        for t in 0..end {
            let next = (t + 1 < area_end).then(|| decode(t + 1));
            conv.convert(prev.as_deref(), &cur, next.as_deref(), &mut out);
            for (i, f) in tracks.iter().enumerate() {
                if (f.start_frame..f.start_frame + f.layout.frames).contains(&t) {
                    hashers[i].update(&out);
                }
            }
            prev = Some(std::mem::replace(&mut cur, next.unwrap_or_default()));
        }
        for (i, h) in hashers.into_iter().enumerate() {
            assert_eq!(h.finalize().to_vec(), got[i], "{path:?} area {area} track {}", tracks[i].number);
        }
        total += tracks.len();
        eprintln!(
            "area {area} of an image: {} tracks equal the area stream (read {read_s:.1} s)",
            tracks.len()
        );
    }
    eprintln!("{total} WAV files checked at {} Hz", rate.hz());
    assert!(total > 0);
}

/// Writes a sparse RF64 file made by sacdfs's own header and trailer code (six channels,
/// 25 minutes at 176.4 kHz: 4.6 GB of silence), for checking with other programs locally:
/// `SACDFS_RF64_OUT=FILE cargo test --test wav rf64_file -- --ignored`. No real disc in the
/// local set has a track long enough to need RF64.
#[test]
#[ignore = "writes a 4.6 GB sparse file: set SACDFS_RF64_OUT"]
fn rf64_file() {
    use std::io::{Seek, SeekFrom, Write};
    let Some(path) = std::env::var_os("SACDFS_RF64_OUT") else { return };
    let layout = wav::Layout { channels: 6, frames: 25 * 60 * 75, samples_per_frame: 2352, rate: 176_400 };
    let mut tags = sacdfs::dsf::Tags::default();
    tags.set(b"TIT2", Some("Long track"));
    tags.set(b"TALB", Some("Album"));
    tags.set(b"TRCK", Some("1/1"));
    let t = wav::trailer(&tags);
    let h = wav::header(&layout, t.bytes.len() as u64);
    assert!(wav::needs_rf64(&layout, t.bytes.len() as u64));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(&h).unwrap();
    f.seek(SeekFrom::Start(h.len() as u64 + layout.data_len())).unwrap();
    f.write_all(&t.bytes).unwrap();
    drop(f);
    let size = std::fs::metadata(&path).unwrap().len();
    assert_eq!(size, h.len() as u64 + layout.data_len() + t.bytes.len() as u64);
    eprintln!("{size} bytes, {} samples per channel", layout.samples());
}
