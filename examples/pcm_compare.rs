// SPDX-License-Identifier: GPL-3.0-or-later

//! TEMPORARY: sacdfs's DSD-to-PCM converter against scarletbook-pcm 0.5.2, for the report
//! "PCM: scarletbook-pcm against our converter (0.5.2)". Both are measured the same way,
//! from their 24-bit output (scarletbook-pcm has no float output).
//!
//! ```sh
//! cargo run --release --example pcm_compare -- quality
//! cargo run --release --example pcm_compare -- disc IMAGE [SECONDS]
//! cargo run --release --example pcm_compare -- speed IMAGE [SECONDS] [THREADS]
//! cargo run --release --example pcm_compare -- determinism
//! ```
//!
//! The analysis (test modulator, FFT, THD+N, fits) is that of `tests/pcm.rs` (#9).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::needless_range_loop)]

use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use sacdfs::disc::{Disc, Tuning};
use sacdfs::pcm::sb::{AnyConverter, Engine as Which, SbConverter, sb_config};
use sacdfs::pcm::{Converter, Cutoff, DSD_SILENCE, FRAME_BYTES, PcmConfig, PcmRate, Simd};
use sacdfs::track::{Format, TrackFile};
use sacdfs::tree::{Output, SourceDir, Tree};
use scarletbook::{DstDecoder, TrackFrame};

const FS: f64 = 2_822_400.0;
const RATES: [PcmRate; 3] = [PcmRate::R88200, PcmRate::R176400, PcmRate::R352800];
const CUTOFFS: [Cutoff; 3] = [Cutoff::Khz40, Cutoff::Khz30, Cutoff::Wide80];
const FULL: f64 = 8_388_608.0;

fn configs() -> Vec<PcmConfig> {
    let mut v = Vec::new();
    for cutoff in CUTOFFS {
        for rate in RATES {
            let c = PcmConfig { rate, cutoff, gain_db: 0.0 };
            if c.validate().is_ok() {
                v.push(c);
            }
        }
    }
    v
}

fn name(c: &PcmConfig) -> String {
    let k = match c.cutoff {
        Cutoff::Khz40 => "40k",
        Cutoff::Khz30 => "30k",
        Cutoff::Wide80 => "wide",
    };
    format!("{:.1}/{k}", f64::from(c.rate.hz()) / 1000.0)
}

/// Pass band edge, sacdfs's stop edge (#5), scarletbook's stop edge.
fn edges(c: &PcmConfig) -> (f64, f64, f64) {
    match c.cutoff {
        Cutoff::Khz40 => (40_000.0, 48_000.0, 48_000.0),
        Cutoff::Khz30 => (30_000.0, 36_000.0, 38_000.0),
        Cutoff::Wide80 if c.rate == PcmRate::R352800 => (80_000.0, 100_000.0, 96_000.0),
        Cutoff::Wide80 => (80_000.0, 88_200.0, 96_000.0),
    }
}

const WHICH: [(Which, &str); 2] = [(Which::Sacdfs, "sacdfs"), (Which::Scarletbook, "scarletbook")];

// ---------------------------------------------------------------------------------------
// Signals and analysis (as tests/pcm.rs)
// ---------------------------------------------------------------------------------------

struct Modulator {
    e: [f64; 5],
    w: [f64; 5],
}

const NTF_N: [f64; 6] = [1.0, -4.997_797_49, 9.993_393_4, -9.993_393_4, 4.997_797_49, -1.0];
const NTF_D: [f64; 6] = [1.0, -4.191_193_74, 7.082_279_04, -6.025_310_23, 2.578_849_66, -0.443_955_18];

impl Modulator {
    fn new() -> Self {
        Modulator { e: [0.0; 5], w: [0.0; 5] }
    }
    fn bit(&mut self, u: f64) -> bool {
        let mut w = 0.0;
        for k in 1..6 {
            w += (NTF_N[k] - NTF_D[k]) * self.e[k - 1] - NTF_D[k] * self.w[k - 1];
        }
        let x = u + w;
        assert!(x.abs() < 10.0, "modulator unstable");
        let v = if x >= 0.0 { 1.0 } else { -1.0 };
        self.e.rotate_right(1);
        self.w.rotate_right(1);
        self.e[0] = v - x;
        self.w[0] = w;
        v > 0.0
    }
    fn run(&mut self, bytes: usize, u: impl Fn(usize) -> f64) -> Vec<u8> {
        (0..bytes)
            .map(|i| {
                let mut b = 0u8;
                for k in 0..8 {
                    b = (b << 1) | u8::from(self.bit(u(i * 8 + k)));
                }
                b
            })
            .collect()
    }
}

fn mono(frames: usize, u: impl Fn(usize) -> f64) -> Vec<u8> {
    Modulator::new().run(frames * FRAME_BYTES, u)
}

fn sine(amp: f64, f: f64, phase: f64) -> impl Fn(usize) -> f64 {
    move |n| amp * (2.0 * PI * f * n as f64 / FS + phase).sin()
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    assert!(n.is_power_of_two());
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        let (ws, wc): (Vec<f64>, Vec<f64>) = (0..len / 2).map(|k| (ang * k as f64).sin_cos()).unzip();
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (ws[k], wc[k]);
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * c - im[b] * s;
                let ti = re[b] * s + im[b] * c;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

fn bh(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let t = 2.0 * PI * i as f64 / n as f64;
            0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos()
        })
        .collect()
}

/// One-sided power spectrum under a Blackman-Harris window; summing a tone's bins gives its power.
fn spectrum(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    let w = bh(n);
    let mut re: Vec<f64> = x.iter().zip(&w).map(|(a, b)| a * b).collect();
    let mut im = vec![0.0; n];
    fft(&mut re, &mut im);
    let norm: f64 = w.iter().map(|v| v * v).sum::<f64>() * n as f64;
    (0..=n / 2)
        .map(|k| {
            let p = (re[k] * re[k] + im[k] * im[k]) / norm;
            if k == 0 || k == n / 2 { p } else { 2.0 * p }
        })
        .collect()
}

/// Welch power-averaged spectrum, 2^16-point segments, 50 % overlap.
fn welch(x: &[f64]) -> Vec<f64> {
    let mut n = 1 << 16;
    while n > 1024 && n * 4 > x.len() {
        n >>= 1;
    }
    let mut acc = vec![0.0; n / 2 + 1];
    let mut k = 0;
    let mut at = 0;
    while at + n <= x.len() {
        for (a, p) in acc.iter_mut().zip(spectrum(&x[at..at + n])) {
            *a += p;
        }
        k += 1;
        at += n / 2;
    }
    acc.iter_mut().for_each(|a| *a /= k.max(1) as f64);
    acc
}

fn band_power(p: &[f64], fs: f64, lo: f64, hi: f64) -> f64 {
    let n = (p.len() - 1) * 2;
    let bin = fs / n as f64;
    p.iter()
        .enumerate()
        .filter(|&(k, _)| {
            let f = k as f64 * bin;
            f >= lo && f <= hi
        })
        .map(|(_, v)| v)
        .sum()
}

fn db(x: f64) -> f64 {
    10.0 * x.max(1e-300).log10()
}

fn thd_n(x: &[f64], fs: f64, f: f64, lo: f64, hi: f64) -> f64 {
    let p = spectrum(x);
    let bin = fs / x.len() as f64;
    let k0 = (f / bin).round() as usize;
    let tone: f64 = p[k0 - 8..=k0 + 8].iter().sum();
    let rest: f64 = p
        .iter()
        .enumerate()
        .filter(|&(k, _)| {
            let fk = k as f64 * bin;
            fk >= lo && fk <= hi && (k + 8 < k0 || k > k0 + 8)
        })
        .map(|(_, v)| v)
        .sum();
    db(rest / tone)
}

fn fit(x: &[f64], w: f64) -> (f64, f64, f64) {
    let (mut m, mut r) = ([[0.0f64; 3]; 3], [0.0f64; 3]);
    for (n, &v) in x.iter().enumerate() {
        let (s, c) = (w * n as f64).sin_cos();
        let basis = [s, c, 1.0];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += basis[i] * basis[j];
            }
            r[i] += basis[i] * v;
        }
    }
    for i in 0..3 {
        for k in i + 1..3 {
            let f = m[k][i] / m[i][i];
            for j in 0..3 {
                m[k][j] -= f * m[i][j];
            }
            r[k] -= f * r[i];
        }
    }
    let mut x = [0.0; 3];
    for i in (0..3).rev() {
        let s: f64 = (i + 1..3).map(|j| m[i][j] * x[j]).sum();
        x[i] = (r[i] - s) / m[i][i];
    }
    ((x[0] * x[0] + x[1] * x[1]).sqrt(), x[1].atan2(x[0]), x[2])
}

// ---------------------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------------------

fn unpack(b: &[u8]) -> impl Iterator<Item = i32> + '_ {
    b.chunks_exact(3).map(|s| i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8)
}

/// Every frame of `dsd` (`ch` interleaved channels), frame by frame with neighbours
/// (`prev`/`next` frames beyond, else silence at the edges): interleaved 24-bit samples.
fn convert_all(
    which: Which,
    cfg: &PcmConfig,
    ch: u8,
    dsd: &[u8],
    before: Option<&[u8]>,
    after: Option<&[u8]>,
) -> (Vec<i32>, u64) {
    let n = usize::from(ch) * FRAME_BYTES;
    let frames: Vec<&[u8]> = dsd.chunks_exact(n).collect();
    let mut conv = AnyConverter::new(which, ch, cfg);
    let mut buf = vec![0u8; conv.frame_bytes()];
    let mut out = Vec::with_capacity(frames.len() * conv.frame_bytes() / 3);
    let mut clips = 0u64;
    for f in 0..frames.len() {
        let prev = if f == 0 { before } else { Some(frames[f - 1]) };
        let next = frames.get(f + 1).copied().or(if f + 1 == frames.len() { after } else { None });
        clips += u64::from(conv.convert(prev, frames[f], next, &mut buf));
        out.extend(unpack(&buf));
    }
    (out, clips)
}

fn channel(x: &[i32], ch: usize, c: usize) -> Vec<f64> {
    x.iter().skip(c).step_by(ch).map(|&v| f64::from(v) / FULL).collect()
}

// ---------------------------------------------------------------------------------------
// quality
// ---------------------------------------------------------------------------------------

/// The composite impulse response at the DSD rate (lag 0 at index `R`), measured from 24-bit
/// output: the difference made by flipping one bit, averaged over `k` random backgrounds,
/// for every phase within an output sample. `exact` uses sacdfs's f32 output on silence
/// instead (the method check).
const R: usize = 4096;
const IMPULSE_GAIN_DB: f64 = 24.0;
fn impulse(which: Which, cfg: &PcmConfig, k: usize, exact: bool) -> Vec<f64> {
    let d = cfg.rate.decimation();
    let per = cfg.rate.samples_per_frame();
    let n0 = per / 2;
    let p0 = n0 * d;
    let mut h = vec![0.0f64; 2 * R + 1];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    // +24 dB (the most both allow) lifts the response 24 dB above the 24-bit rounding; the
    // faint backgrounds stay far from clipping.
    // The wide filter passes the backgrounds' ultrasonic noise, so less there.
    let gain_db = if cfg.cutoff == Cutoff::Wide80 { IMPULSE_GAIN_DB / 2.0 } else { IMPULSE_GAIN_DB };
    let boosted = PcmConfig { gain_db: gain_db as f32, ..*cfg };
    let g = 10f64.powf(gain_db / 20.0);
    let mut conv = AnyConverter::new(which, 1, &boosted);
    let mut ours = Converter::new(1, cfg);
    let mut a = vec![0u8; conv.frame_bytes()];
    let mut b = vec![0u8; conv.frame_bytes()];
    let mut fa = vec![0f32; per];
    let mut fb = vec![0f32; per];
    let reps = if exact { 1 } else { k };
    let pool = backgrounds(k);
    for phase in 0..d {
        let p = p0 + phase;
        let mut acc = vec![0.0f64; per];
        for _ in 0..reps {
            let (prev, mut cur, next) = if exact {
                (vec![DSD_SILENCE; FRAME_BYTES], vec![DSD_SILENCE; FRAME_BYTES], vec![DSD_SILENCE; FRAME_BYTES])
            } else {
                let bg = &pool[rng.next() as usize % pool.len()];
                let f = |i: usize| bg[i * FRAME_BYTES..(i + 1) * FRAME_BYTES].to_vec();
                (f(0), f(1), f(2))
            };
            let mask = 0x80u8 >> (p % 8);
            let was_one = cur[p / 8] & mask != 0;
            let sign = if was_one { -2.0 } else { 2.0 };
            if exact {
                ours.convert_f32(Some(&prev), &cur, Some(&next), &mut fa);
                cur[p / 8] ^= mask;
                ours.convert_f32(Some(&prev), &cur, Some(&next), &mut fb);
                for i in 0..per {
                    acc[i] += f64::from(fb[i] - fa[i]) / sign;
                }
            } else {
                let c1 = conv.convert(Some(&prev), &cur, Some(&next), &mut a);
                cur[p / 8] ^= mask;
                let c2 = conv.convert(Some(&prev), &cur, Some(&next), &mut b);
                assert!(c1 + c2 == 0, "{cfg:?}: the impulse measurement clipped");
                for (i, (x, y)) in unpack(&a).zip(unpack(&b)).enumerate() {
                    acc[i] += f64::from(y - x) / FULL / sign / g;
                }
            }
        }
        for i in 0..per {
            let lag = (d * i) as i64 - p as i64;
            if lag.unsigned_abs() as usize <= R {
                h[(lag + R as i64) as usize] = acc[i] / reps as f64;
            }
        }
    }
    h
}

/// `k` backgrounds of 3 frames each: the modulator's output for a faint random input
/// (white, 1e-3 peak), so that the backgrounds differ, keep their rounding errors
/// independent, and never come near clipping (random bytes would: the wide filter's output
/// of white DSD reaches full scale).
fn backgrounds(k: usize) -> &'static [Vec<u8>] {
    static POOL: std::sync::OnceLock<Vec<Vec<u8>>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        (0..k)
            .map(|i| {
                let mut rng = Rng(0x1234_5678 + i as u64 * 7919);
                let noise: Vec<f64> = (0..3 * FRAME_BYTES * 8).map(|_| (rng.f() - 0.5) * 2e-3).collect();
                Modulator::new().run(3 * FRAME_BYTES, |n| noise[n])
            })
            .collect()
    })
}

/// |H| on a 2^20-point grid at the DSD rate (2.69 Hz bins), bins 0..=2^19.
fn response(h: &[f64]) -> Vec<f64> {
    let n = 1 << 20;
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    // Centre lag 0 at index 0 (circular), so the phase is that of a zero-phase filter.
    for (i, &v) in h.iter().enumerate() {
        let lag = i as i64 - R as i64;
        re[lag.rem_euclid(n as i64) as usize] = v;
    }
    fft(&mut re, &mut im);
    (0..=n / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect()
}

struct Resp {
    r20: f64,
    rpass: f64,
    f01: f64,
    f3: f64,
    sb_own: (f64, f64),
    sb_ours: (f64, f64),
    dc: f64,
    asym: f64,
    centre: f64,
}

fn analyse(h: &[f64], cfg: &PcmConfig, own_stop: f64) -> Resp {
    let m = response(h);
    let bin = FS / (1 << 20) as f64;
    let (pass, ours_stop, _) = edges(cfg);
    let dbm = |k: usize| 20.0 * m[k].max(1e-300).log10();
    let upto = |f: f64| (0..=(f / bin) as usize).map(|k| dbm(k).abs()).fold(0.0, f64::max);
    let first_below = |lvl: f64| {
        (0..m.len()).find(|&k| k as f64 * bin > 1000.0 && dbm(k) < lvl).map(|k| k as f64 * bin).unwrap_or(0.0)
    };
    let stop_from = |f: f64| {
        let k0 = (f / bin).ceil() as usize;
        let (k, v) = (k0..m.len()).map(|k| (k, m[k])).fold((0, 0.0), |a, b| if b.1 > a.1 { b } else { a });
        (-20.0 * v.max(1e-300).log10(), k as f64 * bin)
    };
    let dc: f64 = h.iter().sum();
    let peak = h.iter().fold(0.0f64, |a, v| a.max(v.abs()));
    let asym = (1..R).map(|k| (h[R + k] - h[R - k]).abs()).fold(0.0, f64::max) / peak;
    let centre = h.iter().enumerate().map(|(i, v)| (i as f64 - R as f64) * v).sum::<f64>() / dc;
    Resp {
        r20: upto(20_000.0),
        rpass: upto(pass),
        f01: first_below(-0.1),
        f3: first_below(-3.0),
        sb_own: stop_from(own_stop),
        sb_ours: stop_from(ours_stop),
        dc,
        asym,
        centre,
    }
}

fn quality() {
    let k: usize = std::env::var("K").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    println!("## Response from impulses (24-bit output at +24 dB gain, +12 dB for wide, one flipped bit averaged over {k} faint modulated backgrounds)\n");
    println!(
        "| Config | Converter | Ripple to 20 kHz | Ripple to pass edge | -0.1 dB at | -3 dB at | Stopband from own edge | Stopband from sacdfs's edge (#5) | DC gain | Asymmetry | Centre (DSD samples) |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for cfg in configs() {
        let (_, ours_stop, sb_stop) = edges(&cfg);
        // Method check: sacdfs's exact (f32) response through the same analysis.
        let exact = analyse(&impulse(Which::Sacdfs, &cfg, 1, true), &cfg, ours_stop);
        println!(
            "| {} | sacdfs exact (f32, method check) | ±{:.5} dB | ±{:.5} dB | {:.1} kHz | {:.1} kHz | {:.1} dB @ {:.1} kHz | {:.1} dB @ {:.1} kHz | {:.7} | {:.1e} | {:+.4} |",
            name(&cfg),
            exact.r20,
            exact.rpass,
            exact.f01 / 1e3,
            exact.f3 / 1e3,
            exact.sb_own.0,
            exact.sb_own.1 / 1e3,
            exact.sb_ours.0,
            exact.sb_ours.1 / 1e3,
            exact.dc,
            exact.asym,
            exact.centre
        );
        for (which, label) in WHICH {
            let own = if which == Which::Sacdfs { ours_stop } else { sb_stop };
            let r = analyse(&impulse(which, &cfg, k, false), &cfg, own);
            println!(
                "| {} | {label} | ±{:.5} dB | ±{:.5} dB | {:.1} kHz | {:.1} kHz | {:.1} dB @ {:.1} kHz ({:.1} kHz edge) | {:.1} dB @ {:.1} kHz | {:.7} | {:.1e} | {:+.4} |",
                name(&cfg),
                r.r20,
                r.rpass,
                r.f01 / 1e3,
                r.f3 / 1e3,
                r.sb_own.0,
                r.sb_own.1 / 1e3,
                own / 1e3,
                r.sb_ours.0,
                r.sb_ours.1 / 1e3,
                r.dc,
                r.asym,
                r.centre
            );
        }
    }

    if std::env::var_os("IMPULSE_ONLY").is_some() {
        return;
    }
    // ---- Sweep: modulated sines at -6 dBFS, level and phase per tone, both converters.
    println!("\n## Stepped sweep (modulated sine, -6 dBFS): level error (dB) and alignment (output samples)\n");
    let tones = [
        20.0, 50.0, 100.0, 200.0, 500.0, 1e3, 2e3, 5e3, 10e3, 15e3, 20e3, 25e3, 29e3, 35e3, 39e3, 50e3, 60e3, 70e3,
        79e3,
    ];
    // (config, converter) -> (max |level| to 20k, max |level| to pass edge, max |offset| to 20k, max level diff between converters)
    let mut sweep: BTreeMap<String, Vec<(f64, f64, f64)>> = BTreeMap::new();
    let start_bit = 2 * FRAME_BYTES * 8;
    let bits = 1usize << 20;
    for &f in &tones {
        let phase = 0.3;
        let dsd = mono(32, sine(0.5, f, phase));
        for cfg in configs() {
            let (pass, _, _) = edges(&cfg);
            if f > pass {
                continue;
            }
            let d = cfg.rate.decimation();
            let fs = FS / d as f64;
            for (which, label) in WHICH {
                let (pcm, _) = convert_all(which, &cfg, 1, &dsd, None, None);
                let x = channel(&pcm, 1, 0);
                let seg = &x[start_bit / d..(start_bit + bits) / d];
                let (amp, ph, _) = fit(seg, 2.0 * PI * f / fs);
                let expect = (2.0 * PI * f * start_bit as f64 / FS + phase).rem_euclid(2.0 * PI);
                let mut dph = (ph.rem_euclid(2.0 * PI) - expect).rem_euclid(2.0 * PI);
                if dph > PI {
                    dph -= 2.0 * PI;
                }
                let offset = dph / (2.0 * PI * f / fs);
                let level = 20.0 * (amp / 0.5).log10();
                sweep.entry(format!("{}|{label}", name(&cfg))).or_default().push((f, level, offset));
            }
        }
    }
    println!(
        "| Config | Converter | Max level error to 20 kHz | Max level error to pass edge | Level at top tone | Max offset to 20 kHz |"
    );
    println!("|---|---|---|---|---|---|");
    for cfg in configs() {
        for (_, label) in WHICH {
            let v = &sweep[&format!("{}|{label}", name(&cfg))];
            let m20 = v.iter().filter(|t| t.0 <= 20e3).map(|t| t.1.abs()).fold(0.0, f64::max);
            let mp = v.iter().map(|t| t.1.abs()).fold(0.0, f64::max);
            let top = v.last().unwrap();
            let off = v.iter().filter(|t| t.0 <= 20e3).map(|t| t.2.abs()).fold(0.0, f64::max);
            println!(
                "| {} | {label} | {m20:.5} dB | {mp:.5} dB | {:+.4} dB at {:.0} kHz | {off:.5} |",
                name(&cfg),
                top.1,
                top.0 / 1e3
            );
        }
    }
    println!("\nPer tone, 176.4/40k (sacdfs level, scarletbook level, difference):");
    let a = &sweep["176.4/40k|sacdfs"];
    let b = &sweep["176.4/40k|scarletbook"];
    for (x, y) in a.iter().zip(b) {
        println!("  {:>6.0} Hz: {:+.5} {:+.5} {:+.5} dB", x.0, x.1, y.1, y.1 - x.1);
    }

    // ---- Out-of-band tones.
    println!("\n## Out-of-band tones at -12 dBFS: rejection at the alias (dB); DSD's own content there in brackets\n");
    println!("| Config | Tone | Lands at | sacdfs | scarletbook | DSD itself there |");
    println!("|---|---|---|---|---|---|");
    let amp = 0.25;
    for tone in [60_000.0, 92_000.0, 100_000.0, 120_000.0, 200_000.0] {
        let dsd = mono(32, sine(amp, tone, 0.0));
        let src: Vec<f64> = (start_bit..start_bit + bits)
            .map(|n| if dsd[n / 8] & (0x80 >> (n % 8)) != 0 { 1.0 } else { -1.0 })
            .collect();
        let src_p = spectrum(&src);
        for cfg in configs() {
            let (_, ours_stop, sb_stop) = edges(&cfg);
            if tone < ours_stop.min(sb_stop) + 3_000.0 {
                continue;
            }
            let d = cfg.rate.decimation();
            let fs = FS / d as f64;
            let n = bits / d;
            let mut alias = tone % fs;
            if alias > fs / 2.0 {
                alias = fs - alias;
            }
            let kk = (alias / (fs / n as f64)).round() as usize;
            let mut row = Vec::new();
            for (which, _) in WHICH {
                let (pcm, _) = convert_all(which, &cfg, 1, &dsd, None, None);
                let x = channel(&pcm, 1, 0);
                let p = spectrum(&x[start_bit / d..start_bit / d + n]);
                let (lo, hi) = (kk.saturating_sub(4), (kk + 4).min(p.len() - 1));
                row.push(db(amp * amp / 2.0 / p[lo..=hi].iter().sum::<f64>()));
            }
            let (lo, hi) = (kk.saturating_sub(4), (kk + 4).min(src_p.len() - 1));
            let src_db = db(amp * amp / 2.0 / src_p[lo..=hi].iter().sum::<f64>());
            println!(
                "| {} | {:.0} kHz | {:.1} kHz | {:.1} | {:.1} | {:.1} |",
                name(&cfg),
                tone / 1e3,
                alias / 1e3,
                row[0],
                row[1],
                src_db
            );
        }
    }

    // ---- THD+N, noise floor, DC.
    println!("\n## THD+N in 20 Hz-20 kHz (dB), noise floor (dBFS), DC\n");
    let sigs: Vec<(&str, f64, Vec<u8>)> = vec![
        ("1 kHz -6 dBFS", 1e3, mono(32, sine(0.5, 1e3, 0.3))),
        ("1 kHz -20 dBFS", 1e3, mono(32, sine(0.1, 1e3, 0.3))),
        ("10 kHz -6 dBFS", 10e3, mono(32, sine(0.5, 10e3, 0.3))),
    ];
    let quiet = mono(32, |_| 0.0);
    let src_of = |dsd: &[u8]| -> Vec<f64> {
        (start_bit..start_bit + bits).map(|n| if dsd[n / 8] & (0x80 >> (n % 8)) != 0 { 1.0 } else { -1.0 }).collect()
    };
    print!("| Config | Converter |");
    for s in &sigs {
        print!(" {} |", s.0);
    }
    println!(" Noise 20 Hz-20 kHz (modulated silence) | Noise 20 kHz-pass edge | DC on 1 kHz -6 dBFS | DSD silence 0x69: peak |");
    println!("|---|---|---|---|---|---|---|---|---|");
    print!("| (all) | DSD source |");
    for s in &sigs {
        print!(" {:.2} |", thd_n(&src_of(&s.2), FS, s.1, 20.0, 20_000.0));
    }
    let qs = spectrum(&src_of(&quiet));
    println!(" {:.1} | | | |", db(band_power(&qs, FS, 20.0, 20_000.0)));
    let silence = vec![DSD_SILENCE; 4 * FRAME_BYTES];
    for cfg in configs() {
        let d = cfg.rate.decimation();
        let fs = FS / d as f64;
        let (pass, _, _) = edges(&cfg);
        for (which, label) in WHICH {
            print!("| {} | {label} |", name(&cfg));
            let mut dc = 0.0;
            for (i, s) in sigs.iter().enumerate() {
                let (pcm, _) = convert_all(which, &cfg, 1, &s.2, None, None);
                let x = channel(&pcm, 1, 0);
                let seg = &x[start_bit / d..(start_bit + bits) / d];
                if i == 0 {
                    dc = fit(seg, 2.0 * PI * s.1 / fs).2;
                }
                print!(" {:.2} |", thd_n(seg, fs, s.1, 20.0, 20_000.0));
            }
            let (pcm, _) = convert_all(which, &cfg, 1, &quiet, None, None);
            let x = channel(&pcm, 1, 0);
            let p = spectrum(&x[start_bit / d..(start_bit + bits) / d]);
            let (sil, _) = convert_all(which, &cfg, 1, &silence, None, None);
            let sil_peak = sil.iter().map(|v| v.unsigned_abs()).max().unwrap();
            let sil_mid = sil[sil.len() / 4..3 * sil.len() / 4].iter().map(|v| v.unsigned_abs()).max().unwrap();
            println!(
                " {:.1} | {:.1} | {:.1} dBFS | {sil_peak} LSB (mid-area {sil_mid} LSB) |",
                db(band_power(&p, fs, 20.0, 20_000.0)),
                db(band_power(&p, fs, 20_000.0, pass)),
                20.0 * dc.abs().max(1e-12).log10()
            );
        }
    }

    // ---- Residual between the converters on synthetic signals.
    println!("\n## Residual sacdfs - scarletbook on synthetic DSD (dBFS)\n");
    println!(
        "| Config | Signal | Full band RMS | Full band peak | 20 Hz-20 kHz RMS | pass band RMS | Full band RMS after level match | Level difference | Samples differing | Largest difference |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    let mut rng = Rng(12345);
    // A music-like signal: 48 sines from 20 Hz to 20 kHz, log-spaced, random phases,
    // amplitudes falling 3 dB/octave, about -20 dBFS RMS.
    let comps: Vec<(f64, f64, f64)> = (0..48)
        .map(|i| {
            let f = 20.0 * (1000.0f64).powf(i as f64 / 47.0);
            (f, (1000.0 / f).sqrt().min(3.0), rng.f() * 2.0 * PI)
        })
        .collect();
    let norm: f64 = (comps.iter().map(|c| c.1 * c.1 / 2.0).sum::<f64>()).sqrt();
    let multi = mono(32, move |n| {
        comps.iter().map(|&(f, a, p)| a * (2.0 * PI * f * n as f64 / FS + p).sin()).sum::<f64>() * 0.1 / norm
    });
    let synth: Vec<(&str, &[u8])> = vec![
        ("1 kHz -6 dBFS", &sigs[0].2),
        ("10 kHz -6 dBFS", &sigs[2].2),
        ("48 tones, -20 dBFS RMS", &multi),
        ("modulated silence", &quiet),
    ];
    for cfg in configs() {
        let d = cfg.rate.decimation();
        let fs = FS / d as f64;
        let (pass, _, _) = edges(&cfg);
        for (label, dsd) in &synth {
            let (a, _) = convert_all(Which::Sacdfs, &cfg, 1, dsd, None, None);
            let (b, _) = convert_all(Which::Scarletbook, &cfg, 1, dsd, None, None);
            let r = residual(&a, &b, 1, 0, fs, pass, start_bit / d, bits / d);
            println!(
                "| {} | {label} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:+.5} dB | {:.1} % | {} LSB |",
                name(&cfg),
                r.rms,
                r.peak,
                r.band,
                r.pass,
                r.matched,
                r.gain_db,
                r.differ * 100.0,
                r.max_lsb
            );
        }
    }
}

struct Res {
    rms: f64,
    peak: f64,
    band: f64,
    pass: f64,
    differ: f64,
    max_lsb: i32,
    matched: f64,
    gain_db: f64,
    sig: f64,
}

#[allow(clippy::too_many_arguments)]
fn residual(a: &[i32], b: &[i32], ch: usize, c: usize, fs: f64, pass: f64, from: usize, len: usize) -> Res {
    let x: Vec<i32> = a.iter().skip(c).step_by(ch).copied().collect();
    let y: Vec<i32> = b.iter().skip(c).step_by(ch).copied().collect();
    let diff: Vec<f64> = x.iter().zip(&y).map(|(p, q)| f64::from(p - q) / FULL).collect();
    let ms = diff.iter().map(|v| v * v).sum::<f64>() / diff.len() as f64;
    let peak = diff.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let differ = x.iter().zip(&y).filter(|(p, q)| p != q).count() as f64 / x.len() as f64;
    let max_lsb = x.iter().zip(&y).map(|(p, q)| (p - q).abs()).max().unwrap_or(0);
    // Least-squares scalar level match of y to x, then the residual's power.
    let (xy, yy) = x.iter().zip(&y).fold((0.0, 0.0), |(a, b), (p, q)| {
        (a + f64::from(*p) * f64::from(*q), b + f64::from(*q) * f64::from(*q))
    });
    let g = if yy > 0.0 { xy / yy } else { 1.0 };
    let matched = x.iter().zip(&y).map(|(p, q)| (f64::from(*p) - g * f64::from(*q)) / FULL);
    let mms = matched.map(|v| v * v).sum::<f64>() / x.len() as f64;
    let seg = &diff[from..(from + len).min(diff.len())];
    let p = welch(seg);
    let sigp = welch(&x[from..(from + len).min(x.len())].iter().map(|&v| f64::from(v) / FULL).collect::<Vec<_>>());
    Res {
        rms: db(ms),
        peak: 20.0 * peak.max(1e-15).log10(),
        band: db(band_power(&p, fs, 20.0, 20_000.0)),
        pass: db(band_power(&p, fs, 20.0, pass)),
        differ,
        max_lsb,
        matched: db(mms),
        gain_db: -20.0 * g.log10(),
        sig: db(band_power(&sigp, fs, 20.0, 20_000.0)),
    }
}

// ---------------------------------------------------------------------------------------
// Real discs
// ---------------------------------------------------------------------------------------

/// Decoded DSD frames `[t0 - 1, t0 + n + 1)` of an area (clipped to the area), interleaved.
fn decode(disc: &Disc, area: usize, t0: u32, n: u32, area_end: u32) -> (Vec<Vec<u8>>, u32) {
    let channels = disc.areas[area].channels;
    let first = t0.saturating_sub(1);
    let end = (t0 + n + 1).min(area_end);
    let mut cursor = disc.locate(area, first).unwrap();
    let mut decoder = DstDecoder::new();
    let frames = (first..end)
        .map(|t| {
            let content = cursor.content(disc, t).unwrap();
            scarletbook::audio::decode_frame(&mut decoder, TrackFrame { number: t, content }, channels).unwrap().data
        })
        .collect();
    (frames, first)
}

/// One window per area of every image under `dir`: `secs` seconds from the middle of its
/// timeline.
fn windows(dir: &Path, secs: u32) -> Vec<(String, Arc<Disc>, usize, u32, u32, u32)> {
    let tree = Tree::with_options(SourceDir::scan(dir).unwrap(), Tuning::default(), Output::default());
    let mut areas: BTreeMap<(std::path::PathBuf, usize), Arc<TrackFile>> = BTreeMap::new();
    for (_, f) in tree.all_files() {
        if f.format() == Format::Dsf {
            areas.entry((f.disc.path.clone(), f.area)).or_insert(f);
        }
    }
    areas
        .into_iter()
        .map(|((path, area), f)| {
            let n = secs * 75;
            let t0 = (f.area_end / 2).saturating_sub(n / 2);
            let n = n.min(f.area_end - t0);
            let label = format!(
                "{} area {} ({} ch)",
                path.file_stem().unwrap().to_string_lossy(),
                area,
                f.disc.areas[area].channels
            );
            (label, f.disc.clone(), area, t0, n, f.area_end)
        })
        .collect()
}

fn disc(dir: &Path, secs: u32) {
    println!("## Residual sacdfs - scarletbook on real discs ({secs} s from the middle of each area; dBFS)\n");
    println!(
        "| Area | Config | Channel | Signal 20 Hz-20 kHz | Full band RMS | Full band peak | 20 Hz-20 kHz RMS | Pass band RMS | After level match | Level difference | Samples differing | Largest | Clips (sacdfs/sb) |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for (label, disc, area, t0, n, area_end) in windows(dir, secs) {
        let ch = disc.areas[area].channels;
        let (frames, first) = decode(&disc, area, t0, n, area_end);
        let skip = (t0 - first) as usize;
        let body: Vec<u8> = frames[skip..skip + n as usize].concat();
        let before = (skip == 1).then(|| frames[0].as_slice());
        let after = frames.get(skip + n as usize).map(Vec::as_slice);
        for cfg in configs() {
            let d = cfg.rate.decimation();
            let fs = FS / d as f64;
            let (pass, _, _) = edges(&cfg);
            let (a, ca) = convert_all(Which::Sacdfs, &cfg, ch, &body, before, after);
            let (b, cb) = convert_all(Which::Scarletbook, &cfg, ch, &body, before, after);
            let len = a.len() / usize::from(ch);
            for c in 0..usize::from(ch) {
                let r = residual(&a, &b, usize::from(ch), c, fs, pass, 0, len);
                println!(
                    "| {label} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:+.5} dB | {:.1} % | {} LSB | {ca}/{cb} |",
                    name(&cfg),
                    c + 1,
                    r.sig,
                    r.rms,
                    r.peak,
                    r.band,
                    r.pass,
                    r.matched,
                    r.gain_db,
                    r.differ * 100.0,
                    r.max_lsb
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Speed
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Impl {
    Sacdfs(Simd),
    Scarletbook,
}

fn label(i: Impl) -> String {
    match i {
        Impl::Sacdfs(s) => format!("sacdfs ({})", s.name()),
        Impl::Scarletbook => "scarletbook".into(),
    }
}

/// Converts every frame of `frames` once (runs of `run` frames with neighbours): seconds.
fn time_one(i: Impl, cfg: &PcmConfig, ch: u8, frames: &[&[u8]], run: usize) -> f64 {
    enum C {
        A(Converter),
        B(Box<SbConverter>),
    }
    let mut c = match i {
        Impl::Sacdfs(s) => C::A(Converter::with_simd(ch, cfg, s)),
        Impl::Scarletbook => C::B(Box::new(SbConverter::new(ch, cfg))),
    };
    let fb = cfg.rate.samples_per_frame() * usize::from(ch) * 3;
    let mut out = vec![0u8; fb * run];
    let mut clips = vec![0u32; run];
    let t = Instant::now();
    let mut f = 1;
    while f + 1 < frames.len() {
        let k = run.min(frames.len() - 1 - f);
        let (p, cur, n) = (Some(frames[f - 1]), &frames[f..f + k], Some(frames[f + k]));
        match &mut c {
            C::A(c) => c.convert_run(p, cur, n, &mut out[..fb * k], &mut clips[..k]),
            C::B(c) => c.convert_run(p, cur, n, &mut out[..fb * k], &mut clips[..k]),
        };
        f += k;
    }
    std::hint::black_box(&out);
    t.elapsed().as_secs_f64()
}

fn speed(dir: &Path, secs: u32, threads: usize) {
    // The first stereo area found: `secs` seconds of decoded frames.
    let w = windows(dir, secs);
    let (label_area, disc, area, t0, n, area_end) = w.iter().find(|w| w.1.areas[w.2].channels == 2).unwrap().clone();
    let (frames, _) = decode(&disc, area, t0, n, area_end);
    let refs: Vec<&[u8]> = frames.iter().map(Vec::as_slice).collect();
    let audio = (refs.len() - 2) as f64 / 75.0;
    eprintln!("speed: {label_area}, {audio:.1} s of stereo, {threads} threads for the parallel run");
    let simd = Simd::selected();
    let mut impls = vec![Impl::Sacdfs(simd)];
    if simd != Simd::Baseline {
        impls.push(Impl::Sacdfs(Simd::Baseline));
    }
    impls.push(Impl::Scarletbook);
    println!("## Converter throughput, stereo, × real time ({audio:.0} s of a real area, frames in memory)\n");
    println!("| Config | Converter | 1 thread, frame by frame | 1 thread, runs of 4 | {threads} threads, runs of 4 (total) | per thread |");
    println!("|---|---|---|---|---|---|");
    for cfg in configs() {
        for &i in &impls {
            // Best of 3 on one thread.
            let one = (0..3).map(|_| time_one(i, &cfg, 2, &refs, 1)).fold(f64::MAX, f64::min);
            let four = (0..3).map(|_| time_one(i, &cfg, 2, &refs, 4)).fold(f64::MAX, f64::min);
            let t = Instant::now();
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| time_one(i, &cfg, 2, &refs, 4));
                }
            });
            let all = t.elapsed().as_secs_f64();
            let total = audio * threads as f64 / all;
            println!(
                "| {} | {} | {:.0} | {:.0} | {:.0} | {:.0} |",
                name(&cfg),
                label(i),
                audio / one,
                audio / four,
                total,
                total / threads as f64
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Determinism of scarletbook-pcm behind sacdfs's interface
// ---------------------------------------------------------------------------------------

fn determinism() {
    let mut rng = Rng(777);
    println!("## Determinism (scarletbook-pcm through sacdfs's adapter)\n");
    println!("| Config | Channels | Frame by frame = whole area | Random runs = whole area | Short last frame = padded |");
    println!("|---|---|---|---|---|");
    for cfg in configs() {
        for ch in [1u8, 2, 5, 6] {
            let frames = 12;
            let dsd = rng.bytes(frames * FRAME_BYTES * usize::from(ch));
            // Whole area in one run, through scarletbook-pcm directly.
            let sc = sb_config(ch, &cfg);
            let mut direct = scarletbook_pcm::Converter::new(sc).unwrap();
            let mut whole = vec![0i32; frames * direct.samples_per_frame() * usize::from(ch)];
            direct.convert_run(scarletbook_pcm::FrameRun::new(&dsd), &mut whole).unwrap();
            let mut whole24 = vec![0u8; whole.len() * 3];
            scarletbook_pcm::pack_s24le(&whole, &mut whole24);
            let (per, _) = convert_all(Which::Scarletbook, &cfg, ch, &dsd, None, None);
            let per_ok = per == whole;
            // Random runs of 1 to 5 frames through the adapter.
            let n = usize::from(ch) * FRAME_BYTES;
            let refs: Vec<&[u8]> = dsd.chunks_exact(n).collect();
            let mut conv = SbConverter::new(ch, &cfg);
            let fb = conv.frame_bytes();
            let mut got = vec![0u8; frames * fb];
            let mut f = 0;
            while f < frames {
                let k = (1 + rng.next() as usize % 5).min(frames - f);
                let prev = f.checked_sub(1).map(|p| refs[p]);
                let next = refs.get(f + k).copied();
                let mut clips = vec![0u32; k];
                conv.convert_run(prev, &refs[f..f + k], next, &mut got[f * fb..(f + k) * fb], &mut clips);
                f += k;
            }
            let runs_ok = got == whole24;
            // A short last frame equals the frame padded with DSD silence.
            let cut = dsd.len() - n / 3;
            let mut padded = dsd.clone();
            padded[cut..].fill(DSD_SILENCE);
            let mut a = vec![0u8; fb];
            let mut b = vec![0u8; fb];
            conv.convert(Some(refs[frames - 2]), &dsd[(frames - 1) * n..cut], None, &mut a);
            conv.convert(Some(refs[frames - 2]), &padded[(frames - 1) * n..], None, &mut b);
            println!("| {} | {ch} | {} | {} | {} |", name(&cfg), yes(per_ok), yes(runs_ok), yes(a == b));
        }
    }
}

/// `--pcm-gain` semantics: a -6 dBFS 1 kHz tone at +9 dB clips; both clamp and count.
fn gain_clips() {
    println!("\n## Gain and clipping: 1 kHz at -6 dBFS with +9 dB gain, 4 frames in the middle of 8\n");
    println!("| Config | Converter | Clips counted | Samples at the rails | Samples per channel |");
    println!("|---|---|---|---|---|");
    let dsd = mono(8, sine(0.5, 1e3, 0.3));
    for cfg in configs() {
        let cfg = PcmConfig { gain_db: 9.0, ..cfg };
        for (which, label) in WHICH {
            let (pcm, clips) = convert_all(which, &cfg, 1, &dsd, None, None);
            let spf = cfg.rate.samples_per_frame();
            let mid = &pcm[2 * spf..6 * spf];
            let (pcm4, clips4) = {
                let n = FRAME_BYTES;
                let f: Vec<&[u8]> = dsd.chunks_exact(n).collect();
                let mut c = AnyConverter::new(which, 1, &cfg);
                let mut out = vec![0u8; 4 * c.frame_bytes()];
                let mut k = vec![0u32; 4];
                let total = c.convert_run(Some(f[1]), &f[2..6], Some(f[6]), &mut out, &mut k);
                (unpack(&out).collect::<Vec<_>>(), total)
            };
            assert_eq!(pcm4, mid, "{label}: a run differs from frame by frame");
            let rails = mid.iter().filter(|v| **v == 8_388_607 || **v == -8_388_608).count();
            let _ = clips;
            println!("| {} | {label} | {clips4} | {rails} | {} |", name(&cfg), mid.len());
        }
    }
}

/// Limit cycles of the test modulator (the coordinator's check). The bitstream itself is
/// ideally decimated (its spectrum at 2.8224 MHz, read only in 20 Hz-40 kHz), for inputs
/// where a plain 1-bit loop can lock into periodic patterns: silence, a fine sweep of small
/// DC values (Perez Gonzalez and Reiss, AES 122nd Conv. 2007: the idle tone's fundamental is
/// proportional to the DC input), and tones from -80 to -6 dBFS (noise modulation: Reiss and
/// Sandler, AES 115th Conv. 2003). Each without and with TPDF dither added to the
/// modulator's *input* (not at the quantiser, per Reiss and Sandler), at -120, -100 and
/// -80 dBFS peak. A tone shows as a bin far above the local floor (the median of +-100 bins
/// of a Welch average of 8 segments of 2^19, 5.4 Hz bins); the floor's sum over 20 Hz-20 kHz
/// is the noise without the tones.
fn limit_cycles() {
    println!("## Test modulator: discrete tones in 30 Hz-40 kHz (limit cycles) and noise modulation\n");
    println!(
        "| Input | Dither (TPDF at the input, peak) | DC x fs | Strongest tone | Above floor | Tone level | Tone / (DC x fs) | Bins > 10 dB above floor | Noise floor 20 Hz-20 kHz |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    let seg = 1usize << 19;
    let bits = seg * 9 / 2;
    let frames = bits / (FRAME_BYTES * 8) + 4;
    let mut inputs: Vec<(String, f64, f64)> = vec![("silence".into(), 0.0, 0.0)];
    for i in 0..13 {
        let dc = 1e-5 * 10f64.powf(i as f64 * 3.0 / 12.0);
        inputs.push((format!("DC {dc:.2e}"), dc, 0.0));
    }
    for lvl in [-80.0, -60.0, -40.0, -20.0, -6.0] {
        inputs.push((format!("1 kHz {lvl} dBFS"), 0.0, 10f64.powf(lvl / 20.0)));
    }
    for (label, dc, amp) in &inputs {
        for dither_db in [None, Some(-120.0), Some(-100.0), Some(-80.0)] {
            let peak = dither_db.map_or(0.0, |d: f64| 10f64.powf(d / 20.0));
            let mut rng = Rng(0xD17E_u64);
            let tone = sine(*amp, 1e3, 0.3);
            let d: Vec<f64> = (0..frames * FRAME_BYTES * 8)
                .map(|_| if peak > 0.0 { (rng.f() - 0.5 + rng.f() - 0.5) * peak } else { 0.0 })
                .collect();
            let dsd = Modulator::new().run(frames * FRAME_BYTES, |n| dc + tone(n) + d[n]);
            let start = 2 * FRAME_BYTES * 8;
            let x: Vec<f64> =
                (start..start + bits).map(|n| if dsd[n / 8] & (0x80 >> (n % 8)) != 0 { 1.0 } else { -1.0 }).collect();
            let mut acc = vec![0.0; seg / 2 + 1];
            for k in 0..8 {
                for (a, p) in acc.iter_mut().zip(spectrum(&x[k * seg / 2..k * seg / 2 + seg])) {
                    *a += p / 8.0;
                }
            }
            let bin = FS / seg as f64;
            let (lo, hi) = ((30.0 / bin).ceil() as usize, (40_000.0 / bin) as usize);
            let mut best = (0.0f64, 0.0f64, 0.0f64);
            let mut count = 0;
            let mut floor20 = 0.0;
            let (mut in20, mut used20) = (0usize, 0usize);
            for k in lo..=hi {
                let f = k as f64 * bin;
                let w0 = k.saturating_sub(100).max(lo);
                let mut w: Vec<f64> = acc[w0..=w0 + 200].to_vec();
                w.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let med = w[100] * 1.44; // mean of an averaged noise bin above its median, roughly
                // The test tone and its harmonics, with the window's skirts (a -6 dBFS tone
                // leaks about -100 dBFS 8 bins away): distortion and leakage, not limit cycles.
                let near = *amp > 0.0 && (1..=40).any(|h| (f - 1e3 * h as f64).abs() < 25.0 * bin);
                if f <= 20_000.0 {
                    in20 += 1;
                    if !near {
                        floor20 += med;
                        used20 += 1;
                    }
                }
                if near {
                    continue;
                }
                let r = db(acc[k] / med);
                if r > 10.0 {
                    count += 1;
                }
                if r > best.0 {
                    let p: f64 = acc[k.saturating_sub(4)..=k + 4].iter().sum();
                    best = (r, f, db(p));
                }
            }
            let dcfs = dc * FS;
            println!(
                "| {label} | {} | {} | {:.0} Hz | {:.1} dB | {:.1} dBFS | {} | {count} | {:.1} dBFS |",
                dither_db.map_or("none".into(), |d| format!("{d} dBFS")),
                if *dc > 0.0 { format!("{dcfs:.0} Hz") } else { "".into() },
                best.1,
                best.0,
                best.2,
                if *dc > 0.0 && best.0 > 10.0 { format!("{:.2}", best.1 / dcfs) } else { "".into() },
                db(floor20 * in20 as f64 / used20.max(1) as f64)
            );
        }
    }
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "**NO**" }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("quality") => quality(),
        Some("disc") => disc(Path::new(&args[2]), args.get(3).map_or(60, |s| s.parse().unwrap())),
        Some("speed") => speed(
            Path::new(&args[2]),
            args.get(3).map_or(30, |s| s.parse().unwrap()),
            args.get(4).map_or(sacdfs::engine::default_threads(), |s| s.parse().unwrap()),
        ),
        Some("limitcycles") => limit_cycles(),
        Some("determinism") => {
            determinism();
            gain_clips();
        }
        _ => panic!("usage: pcm_compare quality | disc DIR [SECS] | speed DIR [SECS] [THREADS] | determinism"),
    }
}
