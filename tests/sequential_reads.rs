// SPDX-License-Identifier: GPL-3.0-or-later

//! Image reads of plain sequential reads of whole files (#456).
//!
//! A copy or `sha256sum` of every file reads each file once, from start to end, in small reads.
//! The image data sacdfs reads for that must stay at about the audio it serves: keeping the
//! frames of files' heads in the cache (#508) made it read about 1.75 times the image data and
//! take twice as long, unseen by every other test. The corpus discs are too small to show it,
//! so the check runs on real discs (`scripts/real-discs.sh`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use common::ask;
use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::tree::{Node, Output, ROOT, SourceDir, Tree};
use sacdfs::vfs::{Kind, Listing, Lookup, Volume};

/// Most image data read per byte of the images read: the images hold each area's audio once, and
/// a sequential read of every DSF file reads it once (0.7.0: 1.00 to 1.01 on real discs; with the
/// heads of #508 kept, 1.76 to 1.82).
const MAX_RATIO: f64 = 1.15;

/// Image bytes the check reads at least, when the directory has that many, with files of two
/// channel counts among them, so that frames of two sizes share the cache.
const MIN_BYTES: u64 = 2 << 30;

/// A file of the volume: its path from the root, inode and size.
type FileAt = (String, u64, u64);

/// Every file below `dir` of the volume.
fn files(v: &Volume, dir: u64, path: &str, out: &mut Vec<FileAt>) {
    let listing: Listing = ask(|r| v.readdir(dir, r)).unwrap();
    let names: Vec<(Kind, std::ffi::OsString)> =
        listing.entries(2).map(|(_, _, kind, name)| (kind, name.to_owned())).collect();
    for (kind, name) in names {
        let Lookup::Found(e) = ask(|r| v.lookup(dir, &name, r)).unwrap() else { panic!("{name:?} missing") };
        let p = format!("{path}/{}", name.to_string_lossy());
        if kind == Kind::Dir {
            files(v, e.attr.ino, &p, out);
        } else {
            out.push((p, e.attr.ino, e.attr.size));
        }
    }
}

/// Real discs: every DSF file of the first images (by path, at least [`MIN_BYTES`] with two
/// channel counts, or all of them) read in path order, as `find | sort | xargs sha256sum` does,
/// in 128 KiB reads (a FUSE mount's) and in 4 KiB reads, through the volume with a mount's
/// default cache: the image data read stays within [`MAX_RATIO`] of the images' size.
#[test]
#[ignore = "needs real disc images: set SACDFS_REAL_IMAGES"]
fn real_discs_sequential_reads() {
    let (dir, images) = common::real_images_or_skip!();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    for chunk in [128u32 << 10, 4 << 10] {
        let tree = Arc::new(Tree::with_options(
            SourceDir::scan(&dir).unwrap(),
            Tuning::default(),
            Output::dsf_only(),
        ));
        let engine = Arc::new(Engine::new(threads, 32 << 20).unwrap());
        let v = Volume::new(tree.clone(), engine, 4, 1000, 1000).unwrap();
        let mut all = Vec::new();
        let listing = std::time::Instant::now();
        files(&v, ROOT, "", &mut all);
        let listing = listing.elapsed();
        all.sort();
        // The files by image, and each image's channel counts.
        let mut by_image: BTreeMap<PathBuf, (Vec<FileAt>, BTreeSet<u8>)> = BTreeMap::new();
        for (path, ino, size) in all {
            let node = tree.get(ino).unwrap();
            let Node::File(f) = &*node else { continue };
            let e = by_image.entry(f.disc.path.clone()).or_default();
            e.1.insert(f.layout.channels);
            e.0.push((path, ino, size));
        }
        common::assert_every_image_served(&images, by_image.keys().cloned());
        let (mut chosen, mut bytes, mut channels, mut n) = (Vec::new(), 0u64, BTreeSet::<u8>::new(), 0);
        for (image, (fs, ch)) in &by_image {
            if bytes >= MIN_BYTES && channels.len() > 1 {
                break;
            }
            bytes += std::fs::metadata(image).unwrap().len();
            channels.extend(ch);
            chosen.extend(fs.iter().cloned());
            n += 1;
        }
        chosen.sort();
        let (calls_before, before) = sacdfs::disc::total_reads();
        let started = std::time::Instant::now();
        let (mut open_total, mut open_max) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
        for (path, ino, size) in &chosen {
            let t = std::time::Instant::now();
            let fh = ask(|r| v.open(*ino, r)).unwrap();
            let t = t.elapsed();
            open_total += t;
            open_max = open_max.max(t);
            let mut off = 0;
            while off < *size {
                let data = ask(|r| v.read(*ino, fh, off, chunk, r)).unwrap();
                assert!(!data.is_empty(), "{path}: empty read at {off}");
                off += data.len() as u64;
            }
            v.release(fh);
        }
        let (calls, read) = sacdfs::disc::total_reads();
        let (calls, read) = (calls - calls_before, read - before);
        let ratio = read as f64 / bytes as f64;
        println!(
            "{chunk}-byte reads: {} files of {n} images ({bytes} bytes, channels {channels:?}): \
             {read} bytes of image read in {calls} calls, {ratio:.2} times, {:.1} s; listing \
             {:.1} ms; opens {:.1} us mean, {:.1} us max",
            chosen.len(),
            started.elapsed().as_secs_f64(),
            listing.as_secs_f64() * 1e3,
            open_total.as_secs_f64() * 1e6 / chosen.len().max(1) as f64,
            open_max.as_secs_f64() * 1e6,
        );
        assert!(
            ratio <= MAX_RATIO,
            "{chunk}-byte sequential reads read {ratio:.2} times the image data (at most {MAX_RATIO})"
        );
    }
}
