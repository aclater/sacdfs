// SPDX-License-Identifier: GPL-3.0-or-later

//! Measures what a seek costs: calls to the image, bytes read and time, without FUSE.
//!
//! ```sh
//! cargo run --release --example seek_latency -- [--seeks N] [--len BYTES] [--evict] IMAGE...
//! ```
//!
//! Every seek runs on a fresh tree, so its disc starts with an empty in-memory index, as
//! after a mount. Scenarios:
//!
//! - `cold`: a read of `--len` bytes at a spread of positions in every area of every image;
//! - `first`: the first `--len` bytes of every track;
//! - `warm-same` (with `--cache DIR`): each `cold` seek again after a "remount" that loads
//!   the persistent index saved by the first;
//! - `warm-played` (with `--cache DIR`): a seek into a track that a previous "mount" read
//!   straight through.
//!
//! `--evict` drops each image from the page cache before every seek
//! (`posix_fadvise(POSIX_FADV_DONTNEED)`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use sacdfs::disc::Disc;
use sacdfs::dsf::HEADER_LEN;
use sacdfs::engine::Engine;
use sacdfs::track::{Reader, TrackFile};
use sacdfs::tree::{SourceDir, Tree};

#[derive(Debug, Default, Clone, Copy)]
struct Cost {
    seeks: u64,
    calls: u64,
    bytes: u64,
    secs: f64,
}

impl Cost {
    fn add(&mut self, o: Cost) {
        self.seeks += o.seeks;
        self.calls += o.calls;
        self.bytes += o.bytes;
        self.secs += o.secs;
    }

    fn line(&self, what: &str) -> String {
        let n = self.seeks.max(1) as f64;
        format!(
            "{what:<28} seeks {:>5}  calls/seek {:>6.2}  KiB/seek {:>8.1}  ms/seek {:>7.3}",
            self.seeks,
            self.calls as f64 / n,
            self.bytes as f64 / n / 1024.0,
            self.secs * 1000.0 / n
        )
    }
}

struct Opts {
    seeks: u64,
    len: u64,
    evict: bool,
    cache: Option<PathBuf>,
    scenarios: Vec<String>,
    images: Vec<PathBuf>,
}

fn opts() -> Opts {
    let mut o = Opts {
        seeks: 50,
        len: 128 << 10,
        evict: false,
        cache: None,
        scenarios: Vec::new(),
        images: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--seeks" => o.seeks = args.next().unwrap().parse().unwrap(),
            "--len" => o.len = args.next().unwrap().parse().unwrap(),
            "--evict" => o.evict = true,
            "--cache" => o.cache = Some(args.next().unwrap().into()),
            "--scenario" => o.scenarios.push(args.next().unwrap()),
            _ => o.images.push(a.into()),
        }
    }
    if o.scenarios.is_empty() {
        o.scenarios = vec!["cold".into(), "first".into()];
        if o.cache.is_some() {
            o.scenarios.extend(["warm-same".into(), "warm-played".into()]);
        }
    }
    o
}

#[allow(unsafe_code)]
fn evict(path: &Path) {
    use std::os::fd::AsRawFd;
    let f = std::fs::File::open(path).unwrap();
    // SAFETY: a valid open descriptor; the advice only drops cached pages.
    unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
}

/// The files of a fresh tree of one image, grouped by area, in track order.
fn areas(image: &Path) -> Vec<Vec<Arc<TrackFile>>> {
    let tree = Tree::new(SourceDir::scan(image).unwrap());
    let mut files = tree.all_files();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out: Vec<Vec<Arc<TrackFile>>> = Vec::new();
    for (_, f) in files {
        if out.len() <= f.area {
            out.resize(f.area + 1, Vec::new());
        }
        out[f.area].push(f);
    }
    out.retain(|a| !a.is_empty());
    out
}

/// Seek `i` of `n` in an area: a track and a byte offset in it.
fn position(files: &[Arc<TrackFile>], i: u64, len: u64) -> (usize, u64) {
    let track = (i as usize * 7 + i as usize / files.len().max(1)) % files.len();
    let f = &files[track];
    let span = f.size.saturating_sub(HEADER_LEN + len);
    // A low-discrepancy spread of fractions, different for each seek.
    let frac = ((i as f64 + 0.5) * 0.618_033_988_75).fract();
    let off = HEADER_LEN + ((span as f64 * frac) as u64 & !4095);
    (track, off)
}

/// One read of `len` bytes at `off` of `file` with a fresh reader; the cost.
fn read_once(engine: &Engine, file: &Arc<TrackFile>, off: u64, len: u64) -> Cost {
    let disc: &Disc = &file.disc;
    let (c0, b0) = (disc.sector_reads(), disc.sector_bytes());
    let t = Instant::now();
    let (data, _) = Reader::new(file.clone()).read(engine, off, len).unwrap();
    let secs = t.elapsed().as_secs_f64();
    assert!(!data.is_empty());
    Cost { seeks: 1, calls: disc.sector_reads() - c0, bytes: disc.sector_bytes() - b0, secs }
}

/// The same file in a fresh tree (a "remount").
fn fresh(image: &Path, area_index: usize, track: usize) -> Arc<TrackFile> {
    areas(image)[area_index][track].clone()
}

fn flush() {
    if let Some(c) = sacdfs::index_cache::global() {
        c.flush().unwrap();
    }
}

fn main() {
    let o = opts();
    if let Some(dir) = &o.cache {
        let cache = sacdfs::index_cache::IndexCache::open(dir).unwrap();
        sacdfs::index_cache::install(Arc::new(cache));
    }
    let engine = Engine::new(sacdfs::engine::default_threads(), 0).unwrap();
    for scenario in &o.scenarios {
        let mut total = Cost::default();
        for (k, image) in o.images.iter().enumerate() {
            let shape = areas(image);
            for (a, files) in shape.iter().enumerate() {
                let mut cost = Cost::default();
                let n = if scenario == "first" { files.len() as u64 } else { o.seeks };
                for i in 0..n {
                    let (track, off) = if scenario == "first" {
                        (i as usize, HEADER_LEN)
                    } else {
                        position(files, i, o.len)
                    };
                    if scenario == "warm-same" {
                        // Learn from the same seek once, save, and "remount".
                        let f = fresh(image, a, track);
                        read_once(&engine, &f, off, o.len);
                        flush();
                    } else if scenario == "warm-played" {
                        let f = fresh(image, a, track);
                        let reader = Reader::new(f.clone());
                        let mut at = 0;
                        while at < f.size {
                            let (d, _) = reader.read(&engine, at, 1 << 20).unwrap();
                            at += d.len() as u64;
                        }
                        flush();
                    }
                    if o.evict {
                        evict(image);
                    }
                    let f = fresh(image, a, track);
                    cost.add(read_once(&engine, &f, off, o.len));
                }
                println!("{}", cost.line(&format!("{scenario} image{k} area{a}")));
                total.add(cost);
            }
        }
        println!("{}", total.line(&format!("{scenario} ALL")));
    }
}
