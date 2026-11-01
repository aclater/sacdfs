// SPDX-License-Identifier: GPL-3.0-or-later

//! Seeks through scarletbook's own seeking against sacdfs's frame locator (#28), without
//! FUSE: calls to the image, bytes and time for the frames of a 128 KiB DSF read, at the
//! positions of `seek_latency`'s `cold` scenario, each on a freshly opened disc.
//!
//! ```sh
//! cargo run --release --example library_seek -- [OPTIONS] IMAGE...
//! ```
//!
//! - default: the library's cold seek, `SeekPolicy::Speculative` with an empty seek index
//!   kept by the disc (`--probe SECTORS`, `--gap SECTORS` set its options; `--suspect-exact`
//!   reads suspect areas exactly; `--to-end` makes the cursor's range run to the area's end,
//!   as a cursor that read-ahead continues would, instead of the frames of the read);
//! - `--warm`: the library's seek after a "remount": a first disc reads the same range,
//!   its index is saved with its seek hints (`AreaIndex::without_checkpoints`, as a cache
//!   of many discs would keep it), and a second disc loads it (`--full-index` saves the
//!   index as it is, checkpoints included; `--exact-warm` saves a complete exact index built
//!   in one pass first, scarletbook 0.4's only warm path);
//! - `--own`: sacdfs's locator, cold; with `--warm`, after a "remount" that loads the
//!   persistent seek index a first seek at the same position saved;
//! - `--check`: compare the library's frames with sacdfs's; `--seeks N`, `--start I` (the
//!   first position), `--area A` (one area only); `--evict` drops the image from the page
//!   cache before each timed seek.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use sacdfs::dsf::HEADER_LEN;
use sacdfs::track::TrackFile;
use sacdfs::tree::{SourceDir, Tree};
use scarletbook::{
    AreaIndex, IndexOptions, ReadAt, ReadAtReader, SeekPolicy, Slot, Speculation, SpeculativeOptions,
};

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

#[allow(unsafe_code)]
fn evict(path: &Path) {
    use std::os::fd::AsRawFd;
    let f = File::open(path).unwrap();
    // SAFETY: a valid open descriptor; the advice only drops cached pages.
    unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
}

type LibDisc = scarletbook::Disc<ReadAtReader<Counting>>;

/// A freshly opened library disc of `image`, counting its reads, with `index` (or an empty
/// one) kept for `slot` and `policy`.
fn lib_disc(image: &Path, slot: Slot, index: Option<&[u8]>, policy: SeekPolicy) -> (LibDisc, Counting) {
    let file = Arc::new(File::open(image).unwrap());
    let src = Counting { file, calls: Arc::default(), bytes: Arc::default() };
    let mut d = scarletbook::Disc::open(ReadAtReader::new(src.clone())).unwrap();
    match index {
        Some(bytes) => {
            let area = d.toc().area(slot).unwrap();
            let idx =
                AreaIndex::from_bytes(area, d.toc().sectors, bytes, scarletbook::seek::DEFAULT_INLINE_BUDGET)
                    .unwrap();
            d.set_seek_index(slot, idx).unwrap();
        }
        None => d.enable_seek_index(slot, IndexOptions::new()).unwrap(),
    }
    d.set_seek_policy(slot, policy).unwrap();
    (d, src)
}

/// Reads the frames `[f0, f1)` through a cursor of `[f0, end)`: the frames, and whether
/// the reading was provisional and suspect.
fn lib_read(
    d: &mut LibDisc,
    slot: Slot,
    f0: u32,
    f1: u32,
    end: u32,
) -> (Vec<scarletbook::TrackFrame>, bool, bool) {
    let cur = d.range_frames_cursor(slot, f0, end).unwrap();
    let mut it = d.read_frames(cur);
    let got: Vec<_> = it.by_ref().take((f1 - f0) as usize).map(|r| r.unwrap()).collect();
    let provisional = !matches!(it.speculation(), Speculation::Exact | Speculation::Confirmed { .. });
    (got, provisional, it.suspect().is_some())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut seeks = 200u64;
    let mut probe = 8u32;
    let mut images: Vec<PathBuf> = Vec::new();
    let (mut check, mut warm, mut own, mut evict_each) = (false, false, false, false);
    let (mut to_end, mut suspect_exact, mut full_index, mut exact_warm) = (false, false, false, false);
    let mut gap = 2048u64;
    let mut start = 0u64;
    let mut only_area: Option<usize> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--seeks" => seeks = args.next().unwrap().parse().unwrap(),
            "--probe" => probe = args.next().unwrap().parse().unwrap(),
            "--check" => check = true,
            "--warm" => warm = true,
            "--own" => own = true,
            "--evict" => evict_each = true,
            "--to-end" => to_end = true,
            "--suspect-exact" => suspect_exact = true,
            "--full-index" => full_index = true,
            "--exact-warm" => exact_warm = true,
            "--start" => start = args.next().unwrap().parse().unwrap(),
            "--area" => only_area = Some(args.next().unwrap().parse().unwrap()),
            "--gap" => gap = args.next().unwrap().parse().unwrap(),
            _ => images.push(a.into()),
        }
    }
    let policy = SeekPolicy::Speculative(
        SpeculativeOptions::new()
            .with_probe_sectors(probe)
            .with_min_gap_sectors(gap)
            .with_exact_when_suspect(suspect_exact),
    );
    let len = 128u64 << 10;
    let (mut tc, mut tb, mut tn, mut tt) = (0u64, 0u64, 0u64, 0f64);
    let mut per_seek: Vec<(u64, u64, f64)> = Vec::new();
    for (k, image) in images.iter().enumerate() {
        for (a, files) in areas(image).iter().enumerate() {
            if only_area.is_some_and(|x| x != a) {
                continue;
            }
            let (mut c, mut b, mut t, mut spec, mut sus, mut bad) = (0u64, 0u64, 0f64, 0u64, 0u64, 0u64);
            let slot = files[0].disc.areas[files[0].area].slot;
            let exact_saved: Option<Vec<u8>> = (warm && exact_warm && !own).then(|| {
                let (mut d, _) = lib_disc(image, slot, None, SeekPolicy::Exact);
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
                    if evict_each {
                        evict(image);
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
                    let dt = t0.elapsed().as_secs_f64();
                    t += dt;
                    c += d.sector_reads() - c0;
                    b += d.sector_bytes() - b0;
                    per_seek.push((d.sector_reads() - c0, d.sector_bytes() - b0, dt));
                    drop(d);
                    drop(cache);
                    let _ = std::fs::remove_dir_all(&dir);
                    continue;
                }
                let playtime = f.disc.toc.areas[f.area].playtime_frames();
                let f1 = f1.min(playtime);
                let end = if to_end { playtime } else { f1 };
                let saved: Option<Vec<u8>> = match (&exact_saved, warm) {
                    (Some(s), _) => Some(s.clone()),
                    (None, true) => {
                        let (mut d, _) = lib_disc(image, slot, None, policy);
                        lib_read(&mut d, slot, f0, f1, end);
                        let idx = d.seek_index(slot).unwrap();
                        Some(if full_index { idx.to_bytes() } else { idx.without_checkpoints().to_bytes() })
                    }
                    (None, false) => None,
                };
                if evict_each {
                    evict(image);
                }
                let (mut d, src) = lib_disc(image, slot, saved.as_deref(), policy);
                let (c0, b0) = (src.calls.load(Ordering::Relaxed), src.bytes.load(Ordering::Relaxed));
                let t0 = Instant::now();
                let (got, provisional, suspect) = lib_read(&mut d, slot, f0, f1, end);
                let dt = t0.elapsed().as_secs_f64();
                t += dt;
                spec += u64::from(provisional);
                sus += u64::from(suspect);
                let (dc, db) =
                    (src.calls.load(Ordering::Relaxed) - c0, src.bytes.load(Ordering::Relaxed) - b0);
                c += dc;
                b += db;
                per_seek.push((dc, db, dt));
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
                "image{k} area{a}: calls/seek {:.2} KiB/seek {:.1} ms/seek {:.3} provisional {spec}/{seeks} suspect {sus} mismatched frames {bad}",
                c as f64 / n,
                b as f64 / n / 1024.0,
                t * 1000.0 / n,
            );
            tc += c;
            tb += b;
            tn += seeks;
            tt += t;
        }
    }
    let n = tn as f64;
    let pct = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(|a, b| a.total_cmp(b));
        v.get(((v.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(0.0)
    };
    let mut calls: Vec<f64> = per_seek.iter().map(|s| s.0 as f64).collect();
    let mut kib: Vec<f64> = per_seek.iter().map(|s| s.1 as f64 / 1024.0).collect();
    let mut ms: Vec<f64> = per_seek.iter().map(|s| s.2 * 1000.0).collect();
    println!(
        "all: calls/seek {:.2} KiB/seek {:.1} ms/seek {:.3}  calls p50/p99/max {}/{}/{}  KiB p50/p99/max {:.0}/{:.0}/{:.0}  ms p50/p99/max {:.3}/{:.3}/{:.3}",
        tc as f64 / n,
        tb as f64 / n / 1024.0,
        tt * 1000.0 / n,
        pct(&mut calls, 0.5),
        pct(&mut calls, 0.99),
        pct(&mut calls, 1.0),
        pct(&mut kib, 0.5),
        pct(&mut kib, 0.99),
        pct(&mut kib, 1.0),
        pct(&mut ms, 0.5),
        pct(&mut ms, 0.99),
        pct(&mut ms, 1.0),
    );
}
