// SPDX-License-Identifier: GPL-3.0-or-later

//! The DSD-to-PCM converter alone: one frame at a time, as the filesystem will call it,
//! for 2 and 6 channels at every output rate and both cutoffs. Throughput is in frames
//! (1/75 s of audio each), so real-time speed is frames per second / 75.
//!
//! `cargo bench --bench pcm`; in CI, `cargo bench --bench pcm -- --test` runs each case
//! once as a smoke test.

#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use sacdfs::pcm::{Converter, Cutoff, FRAME_BYTES, PcmConfig, PcmRate};
use std::hint::black_box;

/// Deterministic noise-like DSD bytes (the cost does not depend on the data).
fn dsd(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("pcm");
    g.throughput(Throughput::Elements(1));
    for channels in [2u8, 6] {
        let n = usize::from(channels) * FRAME_BYTES;
        let frames = [dsd(n, 1), dsd(n, 2), dsd(n, 3)];
        for cutoff in [Cutoff::Khz40, Cutoff::Khz30, Cutoff::Wide80] {
            for rate in [PcmRate::R88200, PcmRate::R176400, PcmRate::R352800] {
                let cfg = PcmConfig { rate, cutoff, gain_db: 0.0 };
                if cfg.validate().is_err() {
                    continue;
                }
                let mut conv = Converter::new(channels, &cfg);
                let mut out = vec![0u8; conv.frame_bytes()];
                let band = match cutoff {
                    Cutoff::Khz40 => "40k",
                    Cutoff::Khz30 => "30k",
                    Cutoff::Wide80 => "wide80k",
                };
                let id = BenchmarkId::new(format!("{channels}ch-{band}"), rate.hz());
                g.bench_function(id, |b| {
                    b.iter(|| {
                        black_box(conv.convert(
                            Some(&frames[0]),
                            black_box(&frames[1]),
                            Some(&frames[2]),
                            &mut out,
                        ))
                    })
                });
            }
        }
    }
    g.finish();

    // Making a converter designs its filters (Kaiser windows, Bessel series, stage 1 tables).
    let mut g = c.benchmark_group("pcm-new");
    for cutoff in [Cutoff::Khz40, Cutoff::Khz30, Cutoff::Wide80] {
        let cfg = PcmConfig { cutoff, ..PcmConfig::default() };
        g.bench_function(format!("{cutoff:?}"), |b| b.iter(|| black_box(Converter::new(2, &cfg))));
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
