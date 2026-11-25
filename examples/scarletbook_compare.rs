// SPDX-License-Identifier: GPL-3.0-or-later

//! What sacdfs serves for each image, and what it reads from the image to serve it, to compare
//! two builds (two scarletbook versions) line by line, without FUSE:
//!
//! ```sh
//! cargo run --release --example scarletbook_compare -- IMAGE... > out.tsv
//! ```
//!
//! One line per served file (DSF and WAV): image, path, size, SHA-256 of the whole file as the
//! mount serves it, and the image reads (calls and bytes) of two phases on a fresh tree: the
//! first `--head` bytes (a player's first request: #437), then the rest of the file read
//! straight through. A file that cannot be listed or read gets a line with `ERR` instead of
//! a hash. Hashes must be identical between builds; read counts are what is measured.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sacdfs::engine::Engine;
use sacdfs::track::{Reader, TrackFile};
use sacdfs::tree::{SourceDir, Tree};
use sha2::{Digest, Sha256};

fn files(image: &Path) -> Vec<(String, Arc<TrackFile>)> {
    let tree = Tree::new(SourceDir::scan(image).unwrap());
    let mut v: Vec<(String, Arc<TrackFile>)> =
        tree.all_files().into_iter().map(|(p, f)| (p.display().to_string(), f)).collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn main() {
    let mut head = 128 << 10;
    let mut images: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--head" {
            head = args.next().unwrap().parse().unwrap();
        } else {
            images.push(a.into());
        }
    }
    let engine = Engine::new(2, 0).unwrap();
    let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
    for image in &images {
        let listed = std::panic::catch_unwind(|| files(image));
        let Ok(listed) = listed else {
            println!("{}\t-\t-\tERR listing panicked\t-\t-\t-\t-", name(image));
            continue;
        };
        for (path, _) in &listed {
            // A fresh tree for each file: its disc starts with an empty in-memory index.
            let (_, f) = files(image).into_iter().find(|(p, _)| p == path).unwrap();
            let disc = f.disc.clone();
            let (c0, b0) = (disc.sector_reads(), disc.sector_bytes());
            let reader = Reader::new(f.clone());
            let first = reader.read(&engine, 0, head.min(f.size));
            let (c1, b1) = (disc.sector_reads(), disc.sector_bytes());
            let mut h = Sha256::new();
            let mut ok = first.is_ok();
            let mut at = 0;
            if let Ok((d, _)) = &first {
                h.update(d);
                at = d.len() as u64;
            }
            while ok && at < f.size {
                match reader.read(&engine, at, 1 << 20) {
                    Ok((d, _)) if !d.is_empty() => {
                        h.update(&d);
                        at += d.len() as u64;
                    }
                    _ => ok = false,
                }
            }
            let (c2, b2) = (disc.sector_reads(), disc.sector_bytes());
            let digest = if ok {
                h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>()
            } else {
                format!("ERR at {at}")
            };
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                name(image),
                path,
                f.size,
                digest,
                c1 - c0,
                b1 - b0,
                c2 - c1,
                b2 - b1
            );
        }
    }
}
