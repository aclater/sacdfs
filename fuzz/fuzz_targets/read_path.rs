// SPDX-License-Identifier: GPL-3.0-or-later

//! The audio read path (#390): `Reader::read` at any offset and length, with its read-ahead,
//! through the engine's frame cache and its pool of reused buffers, PCM conversion, and the
//! assembly of DSF and WAV files, over damaged images. Every read must return as many bytes as
//! the file has from its offset, up to the length asked, and the same bytes as a new reader of
//! an engine with no cache: so that no frame of another range, file or image, and no reused
//! buffer, can show through.
//!
//! Input: a byte choosing a corpus image (plain DSD or DST, 2048-, 2054- or 2064-byte sectors),
//! a byte of settings, then two fields separated by [`SEP`](sacdfs_fuzz::SEP):
//! - the reads, 9 bytes each: the file (an index into the tree's files), flags (bit 0: the
//!   offset counts back from the file's end; bit 1: a long read, up to 2 MiB, else up to
//!   128 KiB), the offset (u32, little-endian) and the length (u24, little-endian);
//! - damage to the image, 6 bytes each: an offset (u32, little-endian, wrapped to the image), a
//!   count and a byte: `count` bytes from the offset are set to the byte.
//!
//! Settings: bits 0-1 the PCM rate (88.2, 176.4 or 352.8 kHz; 3 is 176.4), bit 2 pauses kept,
//! bit 3 readers opened by a network server, bits 4-5 the cache (none, 2, 8 or 64 MiB). The
//! engines live for the whole run, so that their caches and pools carry frames from one input,
//! and image, to the next.

#![no_main]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use libfuzzer_sys::fuzz_target;
use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::pcm::{PcmConfig, PcmRate};
use sacdfs::track::Reader;
use sacdfs::tree::{Output, SourceDir, Tree};
use sacdfs_fuzz::{fields, scratch_dir};

const IMAGES: [&[u8]; 4] = [
    include_bytes!("../../tests/corpus/images/dsd-stereo-mch-2048.iso"),
    include_bytes!("../../tests/corpus/images/dst-features-2ch-6ch.iso"),
    include_bytes!("../../tests/corpus/images/dsd-stereo-mch-2064.iso"),
    include_bytes!("../../tests/corpus/images/dst-stereo-5ch-2054.iso"),
];

/// Reads per input, at most.
const MAX_READS: usize = 16;
/// Damaged runs per input, at most.
const MAX_PATCHES: usize = 64;
/// The longest read: fuser's largest (`MAX_READ`) and then some.
const MAX_LEN: u64 = 1 << 21;
/// The longest read without the flag: what clients mostly ask for.
const SHORT_LEN: u64 = 1 << 17;

const RATES: [PcmRate; 4] = [PcmRate::R88200, PcmRate::R176400, PcmRate::R352800, PcmRate::R176400];
const CACHES: [usize; 4] = [0, 2 << 20, 8 << 20, 64 << 20];

fn dir() -> &'static PathBuf {
    static D: OnceLock<PathBuf> = OnceLock::new();
    D.get_or_init(|| scratch_dir("read_path"))
}

/// The engine of a rate and cache, made once.
fn engine(rate: usize, cache: usize) -> Arc<Engine> {
    static ENGINES: OnceLock<Mutex<HashMap<(usize, usize), Arc<Engine>>>> = OnceLock::new();
    let mut m = ENGINES.get_or_init(Mutex::default).lock().unwrap_or_else(|p| p.into_inner());
    m.entry((rate, cache))
        .or_insert_with(|| {
            let pcm = PcmConfig { rate: RATES[rate], ..PcmConfig::default() };
            match Engine::with_pcm(2, CACHES[cache], pcm) {
                Ok(e) => Arc::new(e),
                Err(e) => panic!("an engine: {e}"),
            }
        })
        .clone()
}

fn u32_at(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fuzz_target!(|data: &[u8]| {
    let [Some(&pick), Some(&settings)] = [data.first(), data.get(1)] else { return };
    let [ops, damage] = fields::<2>(&data[2..]);
    let (rate, pauses, network, cache) =
        (usize::from(settings & 3), settings & 4 != 0, settings & 8 != 0, usize::from(settings >> 4 & 3));

    let mut image = IMAGES[usize::from(pick) % IMAGES.len()].to_vec();
    let n = image.len();
    for p in damage.chunks_exact(6).take(MAX_PATCHES) {
        let at = u32_at(p) as usize % n;
        let end = (at + usize::from(p[4])).min(n);
        image[at..end].fill(p[5]);
    }
    let path = dir().join("x.iso");
    if std::fs::write(&path, &image).is_err() {
        return;
    }
    let Ok(root) = SourceDir::scan(dir()) else { return };
    let pcm = PcmConfig { rate: RATES[rate], ..PcmConfig::default() };
    let output = Output {
        dsf: true,
        wav: true,
        samples_per_frame: RATES[rate].samples_per_frame() as u32,
        pauses_kept: pauses,
    };
    let tree = Tree::with_options(root, Tuning::default(), output).with_pcm(&pcm);
    let files = tree.all_files();
    if files.is_empty() {
        return;
    }
    let (eng, reference) = (engine(rate, cache), engine(rate, 0));
    let mut readers: HashMap<usize, Arc<Reader>> = HashMap::new();

    for op in ops.chunks_exact(9).take(MAX_READS) {
        let i = usize::from(op[0]) % files.len();
        let file = &files[i].1;
        let raw = u64::from(u32_at(&op[2..6]));
        let offset =
            if op[1] & 1 != 0 { file.size.saturating_sub(raw % (1 << 20)) } else { raw % (file.size + 4096) };
        let len = u64::from(u32::from_le_bytes([op[6], op[7], op[8], 0]))
            % if op[1] & 2 != 0 { MAX_LEN } else { SHORT_LEN };
        let reader = readers.entry(i).or_insert_with(|| Arc::new(Reader::opened_by(file.clone(), network)));
        let arrival = reader.arrived();
        let got = reader.read(&eng, offset, len);
        drop(arrival);
        let want = Reader::new(file.clone()).read(&reference, offset, len);
        match (got, want) {
            (Ok((got, ahead)), Ok((want, _))) => {
                let expected = len.min(file.size.saturating_sub(offset));
                assert_eq!(
                    got.len() as u64,
                    expected,
                    "{}: {offset}+{len} of {}",
                    files[i].0.display(),
                    file.size
                );
                assert!(
                    got == want,
                    "{}: {offset}+{len}: not the bytes of a reader with no cache",
                    files[i].0.display()
                );
                if let Some(a) = ahead {
                    reader.read_ahead(&eng, a);
                }
            }
            (Err(_), Err(_)) => {}
            (got, want) => panic!(
                "{}: {offset}+{len}: the cached reader {} and the reader with no cache {}",
                files[i].0.display(),
                if got.is_ok() { "read" } else { "failed" },
                if want.is_ok() { "read" } else { "failed" },
            ),
        }
    }
    let _ = std::fs::remove_file(&path);
});
