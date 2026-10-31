// SPDX-License-Identifier: GPL-3.0-or-later

//! Seeks through scarletbook 0.4's own seeking against sacdfs's frame locator (#28), without
//! FUSE: calls to the image, bytes and time for the frames of a 128 KiB DSF read, at the
//! positions of `seek_latency`'s `cold` scenario, each on a freshly opened disc.
//!
//! ```sh
//! cargo run --release --example library_seek -- [OPTIONS] IMAGE...
//! ```
//!
//! - default: the library's cold seek, `SeekPolicy::Speculative` with an empty seek index
//!   (`--interval K`, `--probe SECTORS`, `--gap SECTORS` set its options);
//! - `--warm`: the library's seek from a complete exact seek index (built in one pass first,
//!   then saved and loaded, as after a remount);
//! - `--own`: sacdfs's locator, cold; with `--warm`, after a "remount" that loads the
//!   persistent seek index a first seek at the same position saved;
//! - `--check`: compare the library's frames with sacdfs's; `--seeks N`, `--start I` (the
//!   first position), `--area A` (one area only).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use sacdfs::dsf::HEADER_LEN;
use sacdfs::track::TrackFile;
use sacdfs::tree::{SourceDir, Tree};
use scarletbook::{IndexOptions, ReadAt, ReadAtReader, SeekPolicy, Speculation, SpeculativeOptions};

#[derive(Clone, Debug)]
struct Counting {
    file: Arc<File>,
    calls: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
}

impl ReadAt for Counting {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(buf.len() as u64, Ordering::Relaxed);
        self.file.read_at(offset, buf)
    }
    fn size(&self) -> io::Result<u64> {
        self.file.size()
    }
}

fn areas(image: &std::path::Path) -> Vec<Vec<Arc<TrackFile>>> {
    let tree = Tree::new(SourceDir::scan(image).unwrap());
    let mut files = tree.all_files();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out: Vec<Vec<Arc<TrackFile>>> = Vec::new();
    for (_, f) in files {
        if f.format() != sacdfs::track::Format::Dsf {
            continue;
        }
        if out.len() <= f.area {
            out.resize(f.area + 1, Vec::new());
        }
        out[f.area].push(f);
    }
    out.retain(|a| !a.is_empty());
    out
}

fn position(files: &[Arc<TrackFile>], i: u64, len: u64) -> (usize, u64) {
    let track = (i as usize * 7 + i as usize / files.len().max(1)) % files.len();
    let f = &files[track];
    let span = f.size.saturating_sub(HEADER_LEN + len);
    let frac = ((i as f64 + 0.5) * 0.618_033_988_75).fract();
    let off = HEADER_LEN + ((span as f64 * frac) as u64 & !4095);
    (track, off)
}

fn tempfile_dir() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "libseek-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut seeks = 200u64;
    let mut interval = 64u32;
    let mut probe = 8u32;
    let mut images: Vec<PathBuf> = Vec::new();
    let mut check = false;
    let mut gap = 2048u64;
    let mut warm = false;
    let mut own = false;
    let mut start = 0u64;
    let mut only_area: Option<usize> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--seeks" => seeks = args.next().unwrap().parse().unwrap(),
            "--interval" => interval = args.next().unwrap().parse().unwrap(),
            "--probe" => probe = args.next().unwrap().parse().unwrap(),
            "--check" => check = true,
            "--warm" => warm = true,
            "--own" => own = true,
            "--start" => start = args.next().unwrap().parse().unwrap(),
            "--area" => only_area = Some(args.next().unwrap().parse().unwrap()),
            "--gap" => gap = args.next().unwrap().parse().unwrap(),
            _ => images.push(a.into()),
        }
    }
    let len = 128u64 << 10;
    let (mut tc, mut tb, mut tn, mut tt) = (0u64, 0u64, 0u64, 0f64);
    for (k, image) in images.iter().enumerate() {
        for (a, files) in areas(image).iter().enumerate() {
            if only_area.is_some_and(|x| x != a) {
                continue;
            }
            let (mut c, mut b, mut t, mut spec, mut bad) = (0u64, 0u64, 0f64, 0u64, 0u64);
            let (mut sc, mut sb, mut pr) = (0u64, 0u64, 0u64);
            let saved: Option<Vec<u8>> = (warm && !own).then(|| {
                let f = &files[0];
                let slot = f.disc.areas[f.area].slot;
                let file = Arc::new(File::open(image).unwrap());
                let mut d = scarletbook::Disc::open(ReadAtReader::new(file)).unwrap();
                d.enable_seek_index(slot, IndexOptions::new().with_interval(interval)).unwrap();
                while !d.extend_seek_index(slot, 1 << 20).unwrap() {}
                d.seek_index(slot).unwrap().to_bytes()
            });
            for i in start..start + seeks {
                let (track, off) = position(files, i, len);
                let f = &files[track];
                let (f0, f1) = f.frames_for(off, off + len);
                if own {
                    let dir = tempfile_dir();
                    let cache = sacdfs::index_cache::IndexCache::open(&dir).unwrap();
                    if warm {
                        let d = sacdfs::disc::Disc::open_with(
                            image,
                            9,
                            sacdfs::disc::Tuning::default(),
                            Some(&cache),
                        )
                        .unwrap();
                        let mut cur = d.locate(f.area, f0).unwrap();
                        for n in f0..f1 {
                            cur.content(&d, n).unwrap();
                        }
                        drop(cur);
                        cache.flush().unwrap();
                        drop(d);
                    }
                    let d = sacdfs::disc::Disc::open_with(
                        image,
                        9,
                        sacdfs::disc::Tuning::default(),
                        Some(&cache),
                    )
                    .unwrap();
                    let (c0, b0) = (d.sector_reads(), d.sector_bytes());
                    let t0 = Instant::now();
                    let mut cur = d.locate(f.area, f0).unwrap();
                    for n in f0..f1 {
                        cur.content(&d, n).unwrap();
                    }
                    t += t0.elapsed().as_secs_f64();
                    c += d.sector_reads() - c0;
                    b += d.sector_bytes() - b0;
                    drop(d);
                    drop(cache);
                    let _ = std::fs::remove_dir_all(&dir);
                    continue;
                }
                let st = &f.disc.areas[f.area];
                let slot = st.slot;
                let file = Arc::new(File::open(image).unwrap());
                let src = Counting { file, calls: Arc::default(), bytes: Arc::default() };
                let mut d = scarletbook::Disc::open(ReadAtReader::new(src.clone())).unwrap();
                match &saved {
                    Some(bytes) => {
                        let area = d.toc().area(slot).unwrap();
                        let idx = scarletbook::AreaIndex::from_bytes(area, d.toc().sectors, bytes, 1 << 20)
                            .unwrap();
                        d.set_seek_index(slot, idx).unwrap();
                    }
                    None => d.enable_seek_index(slot, IndexOptions::new().with_interval(interval)).unwrap(),
                }
                d.set_seek_policy(
                    slot,
                    SeekPolicy::Speculative(
                        SpeculativeOptions::new().with_probe_sectors(probe).with_min_gap_sectors(gap),
                    ),
                )
                .unwrap();
                let playtime = d.toc().area(slot).unwrap().playtime_frames();
                let end = f1.min(playtime);
                let (c0, b0) = (src.calls.load(Ordering::Relaxed), src.bytes.load(Ordering::Relaxed));
                let t0 = Instant::now();
                let cur = d.range_frames_cursor(slot, f0, end).unwrap();
                let mut it = d.read_frames(cur);
                let got: Vec<_> = it.by_ref().map(|r| r.unwrap()).collect();
                t += t0.elapsed().as_secs_f64();
                if !matches!(it.speculation(), Speculation::Exact) {
                    spec += 1;
                    if let scarletbook::ResumedFrom::Speculative { probes, .. } =
                        it.into_cursor().resumed_from()
                    {
                        pr += u64::from(probes);
                    }
                    sc += src.calls.load(Ordering::Relaxed) - c0;
                    sb += src.bytes.load(Ordering::Relaxed) - b0;
                }
                c += src.calls.load(Ordering::Relaxed) - c0;
                b += src.bytes.load(Ordering::Relaxed) - b0;
                if check {
                    let mut cur = f.disc.locate(f.area, f0).unwrap();
                    for g in &got {
                        let want = cur.content(&f.disc, g.number).unwrap();
                        if want != g.content {
                            bad += 1;
                        }
                    }
                }
            }
            let n = seeks as f64;
            println!(
                "image{k} area{a}: calls/seek {:.2} KiB/seek {:.1} ms/seek {:.3} provisional {spec}/{seeks} (calls {:.2} KiB {:.1} probes {:.2}) mismatched frames {bad}",
                c as f64 / n,
                b as f64 / n / 1024.0,
                t * 1000.0 / n,
                sc as f64 / spec.max(1) as f64,
                sb as f64 / spec.max(1) as f64 / 1024.0,
                pr as f64 / spec.max(1) as f64
            );
            tc += c;
            tb += b;
            tn += seeks;
            tt += t;
        }
    }
    let n = tn as f64;
    println!(
        "all: calls/seek {:.2} KiB/seek {:.1} ms/seek {:.3}",
        tc as f64 / n,
        tb as f64 / n / 1024.0,
        tt * 1000.0 / n
    );
}
