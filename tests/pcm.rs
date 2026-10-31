// SPDX-License-Identifier: GPL-3.0-or-later

//! The DSD-to-PCM converter, measured: frequency response, stopband, level, DC, THD+N, time
//! alignment and clipping, on DSD made by a test sigma-delta modulator; and determinism:
//! every frame converted alone equals the whole area converted in one pass.
//!
//! The figures are printed (`cargo test --test pcm -- --nocapture`) and go into
//! `docs/pcm-quality.md`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use proptest::prelude::*;
use sacdfs::pcm::{Converter, Cutoff, DSD_SILENCE, FRAME_BYTES, PcmConfig, PcmRate, Simd, convert_area};
use std::f64::consts::PI;

const FS: f64 = 2_822_400.0;
const RATES: [PcmRate; 3] = [PcmRate::R88200, PcmRate::R176400, PcmRate::R352800];
const CUTOFFS: [Cutoff; 3] = [Cutoff::Khz40, Cutoff::Khz30, Cutoff::Wide80];

/// Whether a rate and cutoff go together (the wide filter needs 176.4 or 352.8 kHz).
fn valid(rate: PcmRate, cutoff: Cutoff) -> bool {
    PcmConfig { rate, cutoff, gain_db: 0.0 }.validate().is_ok()
}

fn cfg(rate: PcmRate, cutoff: Cutoff) -> PcmConfig {
    PcmConfig { rate, cutoff, gain_db: 0.0 }
}

/// Pass band edge and stop band start of a cutoff, in Hz.
fn band(rate: PcmRate, cutoff: Cutoff) -> (f64, f64) {
    match cutoff {
        Cutoff::Khz40 => (40_000.0, 48_000.0),
        Cutoff::Khz30 => (30_000.0, 36_000.0),
        Cutoff::Wide80 if rate == PcmRate::R352800 => (80_000.0, 100_000.0),
        Cutoff::Wide80 => (80_000.0, 88_200.0),
    }
}

// ---------------------------------------------------------------------------------------
// The test modulator
// ---------------------------------------------------------------------------------------

/// A 5th-order, 1-bit sigma-delta modulator in error-feedback form.
///
/// Why this one: it is the class of modulator SACD is made with. E. Janssen's thesis (*Look-
/// ahead sigma-delta modulation and its application to Super Audio CD*, TU Eindhoven, 2010,
/// doi:10.6100/IR691188; chapter 2 and appendix B) uses as its standard example a 5th-order
/// loop filter from a Butterworth prototype with a corner near 100 kHz at 2.8224 MHz and
/// resonators at 12 and 20 kHz. This one has the same order and the same ingredients, designed
/// directly as a noise transfer function: NTF(z) = N(z)/D(z), N with one zero at DC and two
/// complex pairs at 0.5385 and 0.9062 of 20 kHz (10.8 and 18.1 kHz: the optimal spread of
/// zeros for order 5, doing the resonators' job), D with the poles of a 5th-order Butterworth
/// high-pass whose cutoff (112 kHz) was chosen so that the largest gain of the NTF is 1.5
/// (Lee's rule for a stable 1-bit loop). Designed with scipy (`signal.butter(5, wc, 'high')`,
/// wc found by bisection, wc = 0.0795 of Nyquist). The error-feedback form makes the signal
/// transfer function exactly 1, so the DSD carries the input plus shaped noise, with no
/// filtering of the signal to account for in the measurements. It is plain, not look-ahead
/// (the thesis's subject), so its in-band noise and distortion are higher than a mastering
/// modulator's: THD+N of a -6 dBFS 1 kHz tone, measured in 20 Hz-20 kHz, is -91.5 dB. That
/// is what makes "limited by the source" testable: the converter's THD+N must equal it.
/// Stable at 0.5 (-6 dBFS); 0.6 is not.
///
/// The loop: `x = u + w`, the bit `v = sign(x)`, the error `e = v - x`, and
/// `w = ((N - D)/D) e` (strictly causal), so that `V = U + NTF E`.
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

    /// `bytes` DSD bytes (MSB first) of the signal `u(n)`, n the DSD sample index.
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

/// A sine of amplitude `amp` (1.0 = 100 % modulation) at `f` Hz, phase `phase` at DSD sample 0.
fn sine(amp: f64, f: f64, phase: f64) -> impl Fn(usize) -> f64 {
    move |n| amp * (2.0 * PI * f * n as f64 / FS + phase).sin()
}

/// Interleaves per-channel DSD into frames of `channels × 4704` bytes.
fn interleave(chans: &[Vec<u8>]) -> Vec<u8> {
    let ch = chans.len();
    let len = chans[0].len();
    let mut out = vec![0u8; len * ch];
    for (c, data) in chans.iter().enumerate() {
        for (i, &b) in data.iter().enumerate() {
            out[i * ch + c] = b;
        }
    }
    out
}

/// Converts every frame of `dsd` alone, with its neighbours, to f32 (one channel out of `ch`).
fn frames_f32(cfg: &PcmConfig, ch: u8, dsd: &[u8], channel: usize) -> Vec<f64> {
    let n = usize::from(ch) * FRAME_BYTES;
    let frames: Vec<&[u8]> = dsd.chunks_exact(n).collect();
    let mut conv = Converter::new(ch, cfg);
    let per = cfg.rate.samples_per_frame() * usize::from(ch);
    let mut buf = vec![0f32; per];
    let mut out = Vec::new();
    for f in 0..frames.len() {
        let prev = f.checked_sub(1).map(|p| frames[p]);
        conv.convert_f32(prev, frames[f], frames.get(f + 1).copied(), &mut buf);
        out.extend(buf.chunks(usize::from(ch)).map(|s| f64::from(s[channel])));
    }
    out
}

/// Mono DSD of `frames` frames of the signal `u`.
fn mono(frames: usize, u: impl Fn(usize) -> f64) -> Vec<u8> {
    Modulator::new().run(frames * FRAME_BYTES, u)
}

// ---------------------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------------------

/// In-place radix-2 FFT of (re, im).
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
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (ang * k as f64).sin_cos();
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

/// Power spectrum (one-sided, bins 0..n/2) of `x` under a 4-term Blackman-Harris window,
/// normalised so that a full-scale sine's peak bin sums (over its main lobe) to 0.5.
fn spectrum(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    let w: Vec<f64> = (0..n)
        .map(|i| {
            let t = 2.0 * PI * i as f64 / n as f64;
            0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos()
        })
        .collect();
    let mut re: Vec<f64> = x.iter().zip(&w).map(|(a, b)| a * b).collect();
    let mut im = vec![0.0; n];
    fft(&mut re, &mut im);
    // Sum of w^2 normalises the power so that summing the bins of a tone gives its power.
    let norm: f64 = w.iter().map(|v| v * v).sum::<f64>() * n as f64;
    (0..=n / 2)
        .map(|k| {
            let p = (re[k] * re[k] + im[k] * im[k]) / norm;
            if k == 0 || k == n / 2 { p } else { 2.0 * p }
        })
        .collect()
}

fn db(x: f64) -> f64 {
    10.0 * x.max(1e-300).log10()
}

/// THD+N in dB of a tone at `f` within `lo..hi` Hz: everything in the band but the tone's
/// main lobe, against the tone. `fs` is the sampling rate of `x`.
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

/// Least-squares fit of `a sin(wn) + b cos(wn) + c` at angular frequency `w` (rad/sample):
/// returns (amplitude, phase, dc).
#[allow(clippy::needless_range_loop)]
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
    // Gaussian elimination.
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

/// The converter's composite impulse response at the DSD rate, measured: one bit of a silent
/// middle frame flipped at a time, for every phase within one output sample.
fn impulse_response(cfg: &PcmConfig) -> Vec<f64> {
    let d = cfg.rate.decimation();
    let per = cfg.rate.samples_per_frame();
    let silence = vec![DSD_SILENCE; FRAME_BYTES];
    let mut conv = Converter::new(1, cfg);
    let mut out = vec![0f32; per];
    // Flip bits around DSD sample p0 = the middle of the frame (an output sample position).
    let n0 = per / 2;
    let p0 = n0 * d;
    let reach = FRAME_BYTES * 8; // every lag a frame can hold
    let mut h = vec![0.0f64; 2 * reach];
    for phase in 0..d {
        let p = p0 + phase;
        let mut frame = silence.clone();
        let byte = p / 8;
        let mask = 0x80u8 >> (p % 8);
        let was_one = frame[byte] & mask != 0;
        frame[byte] ^= mask;
        conv.convert_f32(None, &frame, None, &mut out);
        // Flipping 1 -> 0 changes the input by -2, 0 -> 1 by +2.
        let sign = if was_one { -2.0 } else { 2.0 };
        for (i, &y) in out.iter().enumerate() {
            // Output i is centred on DSD sample d*i; the response at lag d*i - p.
            let lag = (d * i) as i64 - p as i64;
            h[(lag + reach as i64) as usize] = f64::from(y) / sign;
        }
    }
    h
}

/// |H(f)| of an impulse response at the DSD rate (centred: index `len/2` is lag 0).
fn magnitude(h: &[f64], f: f64) -> f64 {
    let w = 2.0 * PI * f / FS;
    let c = (h.len() / 2) as f64;
    let (mut re, mut im) = (0.0, 0.0);
    for (k, &v) in h.iter().enumerate() {
        if v != 0.0 {
            let (s, co) = (w * (k as f64 - c)).sin_cos();
            re += v * co;
            im -= v * s;
        }
    }
    (re * re + im * im).sqrt()
}

// ---------------------------------------------------------------------------------------
// Frequency response, measured from the impulse response
// ---------------------------------------------------------------------------------------

#[test]
fn response_ripple_and_stopband() {
    for cutoff in CUTOFFS {
        for rate in RATES {
            if !valid(rate, cutoff) {
                continue;
            }
            let (pass, stop) = band(rate, cutoff);
            let cfg = cfg(rate, cutoff);
            let h = impulse_response(&cfg);
            let sum: f64 = h.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "{cfg:?}: DC gain {sum}");
            // Linear phase: the response is symmetric about lag 0.
            let c = h.len() / 2;
            let asym = (1..c).map(|k| (h[c + k] - h[c - k]).abs()).fold(0.0, f64::max);
            assert!(asym < 1e-7, "{cfg:?}: asymmetry {asym}");
            let ripple = |hi: f64| {
                (0..=(hi / 25.0) as usize)
                    .map(|i| 20.0 * magnitude(&h, i as f64 * 25.0).log10())
                    .fold(0.0f64, |m, v| m.max(v.abs()))
            };
            let (r20, rc) = (ripple(20_000.0), ripple(pass));
            // Stopband: from the stop frequency to half the DSD rate, every 50 Hz.
            let mut worst = (0.0f64, 0.0f64);
            let mut f = stop;
            while f <= FS / 2.0 {
                let m = magnitude(&h, f);
                if m > worst.0 {
                    worst = (m, f);
                }
                f += 50.0;
            }
            let sb = -20.0 * worst.0.log10();
            let at = |f: f64| 20.0 * magnitude(&h, f).log10();
            println!(
                "{:>6} Hz {:?}: ripple ±{r20:.5} dB to 20 kHz, ±{rc:.5} dB to {} kHz; \
                 {:.2} dB at {} kHz (mid-transition); stopband {sb:.1} dB (worst at {:.1} kHz); \
                 taps {}",
                rate.hz(),
                cutoff,
                pass / 1000.0,
                at((pass + stop) / 2.0),
                (pass + stop) / 2000.0,
                worst.1 / 1000.0,
                h.iter().filter(|v| **v != 0.0).count(),
            );
            assert!(r20 <= 0.01, "{cfg:?}: ripple {r20} dB to 20 kHz");
            assert!(rc <= 0.1, "{cfg:?}: ripple {rc} dB to the cutoff");
            assert!(sb >= 105.0, "{cfg:?}: stopband {sb} dB at {} Hz", worst.1);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Measurements on modulated DSD
// ---------------------------------------------------------------------------------------

/// 1 kHz at -6 dBFS: level, DC, THD+N (against the DSD source's own) and time alignment.
#[test]
fn tone_1k_level_dc_thdn_alignment() {
    let f = 1000.0;
    let phase = 0.3;
    // 32 frames; analyse 2^20 DSD samples from frame 2 on (away from the area edges).
    let dsd = mono(32, sine(0.5, f, phase));
    let start_bit = 2 * FRAME_BYTES * 8;
    let bits = 1usize << 20;
    // The source: the DSD bitstream itself, as +-1.
    let src: Vec<f64> = (start_bit..start_bit + bits)
        .map(|n| if dsd[n / 8] & (0x80 >> (n % 8)) != 0 { 1.0 } else { -1.0 })
        .collect();
    let src_thdn = thd_n(&src, FS, f, 20.0, 20_000.0);
    for cutoff in CUTOFFS {
        for rate in RATES {
            if !valid(rate, cutoff) {
                continue;
            }
            let cfg = cfg(rate, cutoff);
            let d = rate.decimation();
            let pcm = frames_f32(&cfg, 1, &dsd, 0);
            let seg = &pcm[start_bit / d..(start_bit + bits) / d];
            let fs = FS / d as f64;
            let (amp, ph, dc) = fit(seg, 2.0 * PI * f / fs);
            // Expected phase at output sample start_bit/d: DSD sample start_bit.
            let expect = (2.0 * PI * f * start_bit as f64 / FS + phase).rem_euclid(2.0 * PI);
            let mut dph = (ph.rem_euclid(2.0 * PI) - expect).rem_euclid(2.0 * PI);
            if dph > PI {
                dph -= 2.0 * PI;
            }
            let offset = dph / (2.0 * PI * f / fs);
            let level = 20.0 * (amp / 0.5).log10();
            let thdn = thd_n(seg, fs, f, 20.0, 20_000.0);
            println!(
                "{:>6} Hz {cutoff:?}: level {level:+.5} dB re -6 dBFS, DC {:.1} dBFS, \
                 THD+N {thdn:.2} dB (DSD source {src_thdn:.2} dB), offset {offset:+.5} samples",
                rate.hz(),
                20.0 * dc.abs().max(1e-12).log10(),
            );
            assert!(level.abs() < 0.001, "{cfg:?}: level {level} dB");
            assert!(dc.abs() < 1e-6, "{cfg:?}: DC {dc}");
            assert!(offset.abs() < 0.001, "{cfg:?}: offset {offset} samples");
            assert!((thdn - src_thdn).abs() < 0.5, "{cfg:?}: THD+N {thdn} vs source {src_thdn}");
        }
    }
}

/// Tones above the band, at -12 dBFS: what reaches the PCM (directly, or aliased below
/// the output's Nyquist frequency) is at least 100 dB down, measured against the
/// modulator's own noise in the same bins (printed).
#[test]
fn out_of_band_tones_are_rejected() {
    let amp = 0.25;
    // The modulator's noise with no tone, in the same analysis.
    let quiet = mono(32, |_| 0.0);
    let floor: Vec<Vec<Vec<f64>>> = RATES
        .iter()
        .map(|&rate| {
            CUTOFFS
                .iter()
                .map(|&cutoff| {
                    let d = rate.decimation();
                    let pcm = frames_f32(&cfg(rate, cutoff), 1, &quiet, 0);
                    let start = 2 * FRAME_BYTES * 8 / d;
                    spectrum(&pcm[start..start + (1usize << 20) / d])
                })
                .collect()
        })
        .collect();
    for tone in [60_000.0, 100_000.0, 120_000.0, 200_000.0] {
        let dsd = mono(32, sine(amp, tone, 0.0));
        // The DSD itself, as +-1 at 2.8224 MHz, over the same 2^20 samples: what the
        // modulator really put at each frequency (its own spurs included).
        let start_bit = 2 * FRAME_BYTES * 8;
        let src: Vec<f64> = (start_bit..start_bit + (1 << 20))
            .map(|n| if dsd[n / 8] & (0x80 >> (n % 8)) != 0 { 1.0 } else { -1.0 })
            .collect();
        let src_p = spectrum(&src);
        for cutoff in CUTOFFS {
            for rate in RATES {
                if !valid(rate, cutoff) {
                    continue;
                }
                if tone < band(rate, cutoff).1 + 5_000.0 {
                    continue; // in or next to this filter's pass band
                }
                let cfg = cfg(rate, cutoff);
                let d = rate.decimation();
                let fs = FS / d as f64;
                let pcm = frames_f32(&cfg, 1, &dsd, 0);
                let n = (1usize << 20) / d;
                let start = 2 * FRAME_BYTES * 8 / d;
                let p = spectrum(&pcm[start..start + n]);
                // Where the tone lands at this rate.
                let mut alias = tone % fs;
                if alias > fs / 2.0 {
                    alias = fs - alias;
                }
                let k = (alias / (fs / n as f64)).round() as usize;
                let lo = k.saturating_sub(4);
                let hi = (k + 4).min(p.len() - 1);
                let got: f64 = p[lo..=hi].iter().sum();
                let rejection = db(amp * amp / 2.0 / got);
                let fl: f64 = floor[rate as usize][cutoff as usize][lo..=hi].iter().sum();
                let floor_db = db(amp * amp / 2.0 / fl);
                // The source's own content where the alias lands (same bin width: same
                // duration).
                let src_db = db(amp * amp / 2.0 / src_p[lo..=hi].iter().sum::<f64>());
                println!(
                    "{:>6} Hz {cutoff:?}: {:.0} kHz tone (at {:.1} kHz): {rejection:.1} dB down \
                     (modulator noise alone in those bins: {floor_db:.1} dB down; the DSD itself \
                     there: {src_db:.1} dB down)",
                    rate.hz(),
                    tone / 1000.0,
                    alias / 1000.0
                );
                if floor_db < 110.0 {
                    // The modulator's own noise in those bins (DSD's shaped noise rises
                    // steeply above 40 kHz, and the wide filter passes it) hides anything
                    // below it: nothing to measure here. The exact stopband is asserted in
                    // `response_ripple_and_stopband`.
                    continue;
                }
                // The exact stopband is asserted in `response_ripple_and_stopband`; here the bins
                // also hold the modulator's own noise and spurs, so allow for them: what the
                // DSD itself carries at the alias frequency passes, as it should.
                assert!(
                    rejection >= 105.0 || rejection >= src_db - 1.0,
                    "{cfg:?} {tone}: {rejection} dB, source {src_db} dB"
                );
            }
        }
    }
}

/// A full-scale step: all zeros (-1.0), then all ones (+1.0, 100 % modulation), with no
/// modulator noise. It settles at exactly -1.0 and 1.0, its ringing is symmetric about the step's
/// first sample (linear phase, aligned), and in 24 bits the overshoot clips and is counted. Prints the
/// overshoot, the headroom a full-scale step needs.
#[test]
fn full_scale_step() {
    for cutoff in CUTOFFS {
        for rate in RATES {
            if !valid(rate, cutoff) {
                continue;
            }
            let cfg = cfg(rate, cutoff);
            let d = rate.decimation();
            let mut dsd = vec![0x00u8; 2 * FRAME_BYTES];
            dsd.extend(vec![0xffu8; 3 * FRAME_BYTES]);
            let pcm = frames_f32(&cfg, 1, &dsd, 0);
            let n0 = 2 * FRAME_BYTES * 8 / d;
            let per_ms = 2_822_400 / d / 1000;
            for &v in &pcm[n0 + 6 * per_ms..n0 + 10 * per_ms] {
                assert!((v - 1.0).abs() < 1e-6, "{cfg:?}: settled at {v}");
            }
            for &v in &pcm[n0 - 10 * per_ms..n0 - 6 * per_ms] {
                assert!((v + 1.0).abs() < 1e-6, "{cfg:?}: settled at {v} before the step");
            }
            // Linear phase, centred on the step's first sample: with h the impulse response,
            // a unit step gives y[n0 + k] = sum of h over lags <= d k and y[n0 - k] = sum over
            // lags >= d k, so for this step of 2, y[n0 + k] + y[n0 - k] = 2 h(d k). (At 352.8 kHz the converter is
            // periodic over two output samples, so check the even ones there.)
            let h = impulse_response(&cfg);
            let c = h.len() / 2;
            let step_k = if rate == PcmRate::R352800 { 2 } else { 1 };
            let mut worst = 0.0f64;
            for k in (step_k..400).step_by(step_k) {
                let e = pcm[n0 + k] + pcm[n0 - k] - 2.0 * h[c + d * k];
                worst = worst.max(e.abs());
            }
            let overshoot = (pcm[n0..n0 + 2 * per_ms].iter().copied().fold(0.0, f64::max) - 1.0) / 2.0;
            println!(
                "{:>6} Hz {cutoff:?}: -1 to +1 step: overshoot {:.2} % of the step (peak {:.3} \
                 of full scale); symmetry error {worst:.1e}",
                rate.hz(),
                overshoot * 100.0,
                1.0 + 2.0 * overshoot,
            );
            assert!(worst < 1e-6, "{cfg:?}: step not symmetric: {worst}");
            let (_, clips) = convert_area(1, &cfg, &dsd);
            assert!(clips > 0, "{cfg:?}: the overshoot must clip");
        }
    }
}

/// Gain and clipping: +9 dB multiplies the level by 2.818; samples past full scale are clamped and
/// counted; 100 % modulation (all ones) is one step above the largest 24-bit code.
#[test]
fn gain_and_clipping() {
    let dsd = mono(6, sine(0.5, 1000.0, 0.0));
    let frames: Vec<&[u8]> = dsd.chunks_exact(FRAME_BYTES).collect();
    let run = |gain_db: f32| {
        let cfg = PcmConfig { gain_db, ..PcmConfig::default() };
        let mut c = Converter::new(1, &cfg);
        let mut out = vec![0u8; c.frame_bytes()];
        let mut pcm = Vec::new();
        let mut clips = 0;
        for f in 1..5 {
            clips += c.convert(Some(frames[f - 1]), frames[f], Some(frames[f + 1]), &mut out);
            pcm.extend(out.chunks(3).map(|s| i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8));
        }
        (pcm, clips)
    };
    let (a, ca) = run(0.0);
    let (b, cb) = run(9.0);
    assert_eq!(ca, 0);
    let peak = |v: &[i32]| v.iter().map(|x| x.abs()).max().unwrap();
    assert!((f64::from(peak(&a)) / 8_388_608.0 - 0.5).abs() < 0.01);
    // 0.5 * 2.818 = 1.41 of full scale: clipped samples sit at the rails and are counted.
    let rails = b.iter().filter(|&&x| x == 8_388_607 || x == -8_388_608).count();
    assert!(cb > 0 && cb as usize == rails, "clips {cb}, at the rails {rails}");
    for (x, y) in a.iter().zip(&b) {
        if y.abs() < 8_000_000 {
            let ratio = f64::from(*y) / f64::from(*x);
            assert!(x.abs() < 10_000 || (ratio - 2.818_383).abs() < 1e-3, "{x} {y}");
        }
    }
    println!("+9 dB on a -6 dBFS sine: {cb} of {} samples clipped", b.len());

    // All ones: +1.0 exactly, one step above 8388607.
    let ones = vec![0xffu8; FRAME_BYTES];
    let mut c = Converter::new(1, &PcmConfig::default());
    let mut out = vec![0u8; c.frame_bytes()];
    let clips = c.convert(Some(&ones), &ones, Some(&ones), &mut out);
    assert_eq!(clips as usize, PcmRate::R176400.samples_per_frame());
    assert!(out.chunks(3).all(|s| s == [0xff, 0xff, 0x7f]));
}

// ---------------------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------------------

/// Pseudo-random DSD bytes.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect()
}

/// Each frame converted alone (with its neighbours) equals its slice of the whole area.
fn check_frames(cfg: &PcmConfig, ch: u8, dsd: &[u8]) {
    let (area, area_clips) = convert_area(ch, cfg, dsd);
    let n = usize::from(ch) * FRAME_BYTES;
    let frames: Vec<&[u8]> = dsd.chunks_exact(n).collect();
    let mut conv = Converter::new(ch, cfg);
    let fb = conv.frame_bytes();
    assert_eq!(area.len(), fb * frames.len());
    let mut out = vec![0u8; fb];
    let mut clips = 0;
    for f in 0..frames.len() {
        let prev = f.checked_sub(1).map(|p| frames[p]);
        clips += conv.convert(prev, frames[f], frames.get(f + 1).copied(), &mut out);
        assert!(out == area[f * fb..(f + 1) * fb], "{cfg:?} {ch} ch: frame {f} differs");
    }
    assert_eq!(clips, area_clips);
}

#[test]
fn every_frame_alone_equals_the_area() {
    for ch in 1..=6u8 {
        // Music-like DSD in each channel (different tones), so that the signal is real.
        let chans: Vec<Vec<u8>> =
            (0..ch).map(|c| mono(4, sine(0.4, 440.0 * f64::from(c + 1), f64::from(c)))).collect();
        let dsd = interleave(&chans);
        for cutoff in CUTOFFS {
            for rate in RATES {
                if !valid(rate, cutoff) {
                    continue;
                }
                for gain_db in [0.0, 9.0] {
                    check_frames(&PcmConfig { rate, cutoff, gain_db }, ch, &dsd);
                }
            }
        }
    }
}

#[test]
fn area_edges_are_silence() {
    // A frame converted with no neighbours equals one with silent neighbours.
    let cfg = PcmConfig::default();
    let dsd = noise(2 * FRAME_BYTES, 7);
    let silence = vec![DSD_SILENCE; 2 * FRAME_BYTES];
    let mut c = Converter::new(2, &cfg);
    let mut a = vec![0u8; c.frame_bytes()];
    let mut b = a.clone();
    c.convert(None, &dsd, None, &mut a);
    c.convert(Some(&silence), &dsd, Some(&silence), &mut b);
    assert_eq!(a, b);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Random areas, configurations and frames: the frame alone equals the area's slice.
    #[test]
    fn random_frame_windows(
        frames in 1usize..6,
        ch in 1u8..=6,
        rate in 0usize..3,
        cutoff in 0usize..3,
        gain in -6.0f32..12.0,
        seed in any::<u64>(),
        pick in any::<usize>(),
    ) {
        let cfg = PcmConfig { rate: RATES[rate], cutoff: CUTOFFS[cutoff], gain_db: gain };
        let n = usize::from(ch) * FRAME_BYTES;
        let dsd = noise(frames * n, seed);
        let (area, _) = convert_area(ch, &cfg, &dsd);
        let f = pick % frames;
        let mut conv = Converter::new(ch, &cfg);
        let fb = conv.frame_bytes();
        let mut out = vec![0u8; fb];
        let prev = f.checked_sub(1).map(|p| &dsd[p * n..(p + 1) * n]);
        let next = (f + 1 < frames).then(|| &dsd[(f + 1) * n..(f + 2) * n]);
        conv.convert(prev, &dsd[f * n..(f + 1) * n], next, &mut out);
        prop_assert!(out == area[f * fb..(f + 1) * fb]);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Random runs of frames (#24): `convert_run` equals the area's slice, and so per-frame
    /// conversion, byte for byte; one converter does runs of changing lengths, short last
    /// frames included, on a random instruction set's code.
    #[test]
    fn random_runs(
        frames in 1usize..9,
        ch in 1u8..=6,
        rate in 0usize..3,
        cutoff in 0usize..3,
        gain in -6.0f32..12.0,
        seed in any::<u64>(),
        runs in proptest::collection::vec((any::<usize>(), 1usize..6), 1..4),
        simd in 0usize..3,
        short in 0usize..300,
    ) {
        let cfg = PcmConfig { rate: RATES[rate], cutoff: CUTOFFS[cutoff], gain_db: gain };
        let n = usize::from(ch) * FRAME_BYTES;
        let mut dsd = noise(frames * n, seed);
        // A short last frame: its missing bytes are silence in both.
        let cut = frames * n - short.min(n - 1);
        for b in &mut dsd[cut..] {
            *b = DSD_SILENCE;
        }
        let (area, clips_area) = convert_area(ch, &cfg, &dsd);
        let simd = [Simd::Baseline, Simd::V3, Simd::V4][simd];
        let mut conv = Converter::with_simd(ch, &cfg, simd);
        let fb = conv.frame_bytes();
        let slices: Vec<&[u8]> = (0..frames)
            .map(|f| if f + 1 == frames { &dsd[f * n..cut] } else { &dsd[f * n..(f + 1) * n] })
            .collect();
        for (start, len) in runs {
            let f0 = start % frames;
            let f1 = (f0 + len).min(frames);
            let mut out = vec![0u8; (f1 - f0) * fb];
            let prev = f0.checked_sub(1).map(|p| slices[p]);
            let next = slices.get(f1).copied();
            let mut clips = vec![u32::MAX; f1 - f0];
            let total = conv.convert_run(prev, &slices[f0..f1], next, &mut out, &mut clips);
            prop_assert!(out == area[f0 * fb..f1 * fb], "frames {f0}..{f1} of {frames}");
            prop_assert_eq!(total, clips.iter().sum::<u32>());
            if f0 == 0 && f1 == frames {
                prop_assert_eq!(total, clips_area);
            }
            // Each frame's clip count is what converting it alone reports.
            let mut one = Converter::with_simd(ch, &cfg, simd);
            let mut buf = vec![0u8; fb];
            for (j, f) in (f0..f1).enumerate() {
                let p = f.checked_sub(1).map(|p| slices[p]);
                prop_assert_eq!(clips[j], one.convert(p, slices[f], slices.get(f + 1).copied(), &mut buf));
            }
        }
    }
}

/// The output for a fixed input, pinned: any change to the filters, the arithmetic or the
/// platform's maths library (the designs use `sin` and `exp`-like series at start-up) shows
/// up here. A deliberate change to the filters updates these hashes.
#[test]
fn golden_output() {
    use sha2::{Digest, Sha256};
    let dsd = noise(3 * 2 * FRAME_BYTES, 11);
    let mut got = Vec::new();
    for cutoff in CUTOFFS {
        for rate in RATES {
            if !valid(rate, cutoff) {
                continue;
            }
            let (pcm, _) = convert_area(2, &cfg(rate, cutoff), &dsd);
            let h = Sha256::digest(&pcm);
            let hex: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
            println!("{} {cutoff:?}: {hex}", rate.hz());
            got.push(hex);
        }
    }
    let want = GOLDEN;
    assert_eq!(got, want);
}

/// Every instruction set's code (#23) gives the golden output, frame by frame, at 24 bits and
/// in f32: the baseline, x86-64-v3 and x86-64-v4 paths, as far as this CPU has them (the
/// others fall back and are reported). `SACDFS_SIMD` additionally forces the path of every
/// other test in a run.
#[test]
fn every_simd_path_gives_the_golden_output() {
    use sha2::{Digest, Sha256};
    let n = 2 * FRAME_BYTES;
    let dsd = noise(3 * n, 11);
    let frame = |f: usize| (f < 3).then(|| &dsd[f * n..(f + 1) * n]);
    eprintln!("CPU supports {}", Simd::detected().name());
    let mut f32_ref: Vec<Vec<u32>> = Vec::new();
    for simd in [Simd::Baseline, Simd::V3, Simd::V4] {
        let mut got = Vec::new();
        let mut f32s = Vec::new();
        for cutoff in CUTOFFS {
            for rate in RATES {
                if !valid(rate, cutoff) {
                    continue;
                }
                let mut conv = Converter::with_simd(2, &cfg(rate, cutoff), simd);
                if conv.simd() != simd {
                    eprintln!("{} not available here: testing {}", simd.name(), conv.simd().name());
                }
                let mut pcm = vec![0u8; 3 * conv.frame_bytes()];
                let mut flt = vec![0f32; 3 * conv.frame_bytes() / 3];
                let fb = conv.frame_bytes();
                for f in 0..3usize {
                    conv.convert(
                        f.checked_sub(1).and_then(frame),
                        &dsd[f * n..(f + 1) * n],
                        frame(f + 1),
                        &mut pcm[f * fb..(f + 1) * fb],
                    );
                    conv.convert_f32(
                        f.checked_sub(1).and_then(frame),
                        &dsd[f * n..(f + 1) * n],
                        frame(f + 1),
                        &mut flt[f * fb / 3..(f + 1) * fb / 3],
                    );
                }
                let h = Sha256::digest(&pcm);
                got.push(h.iter().take(8).map(|b| format!("{b:02x}")).collect::<String>());
                f32s.push(flt.iter().map(|v| v.to_bits()).collect::<Vec<u32>>());
            }
        }
        assert_eq!(got, GOLDEN, "{}", simd.name());
        if f32_ref.is_empty() {
            f32_ref = f32s;
        } else {
            assert!(f32_ref == f32s, "{}: f32 output differs from the baseline", simd.name());
        }
    }
}

/// First 8 bytes of the SHA-256 of each configuration's output, in the order of the loops:
/// 40 kHz at 88.2, 176.4, 352.8 kHz, then 30 kHz, then the wide filter at 176.4 and 352.8.
const GOLDEN: [&str; 8] = [
    "d6e861426fc1aa34",
    "ea76e7ad5d5b882c",
    "b3c2ed2efe125e9b",
    "340644957016eca3",
    "5e895cf126369dc8",
    "1ba48df1680595c8",
    "925945e4adf87fe2",
    "8107d10a0c565065",
];
