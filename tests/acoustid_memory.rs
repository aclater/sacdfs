// SPDX-License-Identifier: GPL-3.0-or-later

//! The memory ceiling of `tags fetch --acoustid`'s audio (#296): opening a disc and streaming two
//! minutes of a DST stereo track as fingerprint PCM holds under [`CEILING`] of heap at its peak,
//! and the peak doesn't grow with the length streamed. dhat-rs counts every allocation of this
//! process (`fpcalc` is another process, and not part of the bound). *Why one test in the file:*
//! dhat allows one profiler per process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::{self, Write};

use sacdfs::disc::{Disc, Tuning};
use sacdfs::lookup::fingerprint::{self, MAX_SECONDS, RATE};
use scarletbook::Slot;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// The bound #185 and #296 set for the fingerprint worker.
const CEILING: usize = 32 << 20;

/// Counts what it is given, keeping none of it.
struct Count(u64);

impl Write for Count {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0 += b.len() as u64;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn fingerprint_audio_streams_under_the_ceiling() {
    let _profiler = dhat::Profiler::builder().testing().build();
    let image = common::corpus().join("images/dst-features-2ch-6ch.iso");
    let disc = Disc::open(&image, 0, Tuning::default()).unwrap();
    let area = disc.toc().areas.iter().position(|a| a.slot == Slot::TwoChannel).unwrap();

    // A second, twice (the first read leaves the disc's seek index its hints), then two minutes:
    // the peak is about the same, so nothing grows with the length.
    for _ in 0..2 {
        fingerprint::stream_track(&disc, area, 0, 75, &mut Count(0), &|| false).unwrap();
    }
    let short = dhat::HeapStats::get().max_bytes;
    let mut out = Count(0);
    let fed = fingerprint::stream_track(&disc, area, 0, u32::MAX, &mut out, &|| false).unwrap();
    let stats = dhat::HeapStats::get();
    assert_eq!(fed.frames, MAX_SECONDS * 75);
    assert_eq!(out.0, u64::from(MAX_SECONDS) * u64::from(RATE) * 2);
    eprintln!(
        "peak heap: {} KiB after a second, {} KiB after two minutes",
        short >> 10,
        stats.max_bytes >> 10
    );
    dhat::assert!(stats.max_bytes < CEILING, "peak heap {} bytes", stats.max_bytes);
    dhat::assert!(
        stats.max_bytes <= short + (16 << 10),
        "the peak grew with the length: {} then {}",
        short,
        stats.max_bytes
    );
}
