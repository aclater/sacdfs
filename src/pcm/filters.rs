// SPDX-License-Identifier: GPL-3.0-or-later
//
// The Kaiser-windowed low-pass design (`bessel_i0`, `kaiser_lowpass`) and the stage-1 byte
// lookup tables (`Stage1::new`) are ported from pysacd's `csrc/dsp.c` at commit 2113789
// (the last commit that changed that file; pysacd HEAD 846030d), which carries
// `SPDX-License-Identifier: GPL-2.0-or-later` and no separate copyright line; its author is
// sacdfs's author. Changes from pysacd: odd filter lengths centred on a sample (exact delay
// compensation), equal polyphase sums in stage 1 (DSD silence converts to exact zero),
// half-band stages 2 and 4, and new tap counts for the 40 kHz and 30 kHz bands (see
// docs/pcm-quality.md for the designs and why).

//! Filter design: the coefficients of the four FIR stages, computed when a converter is made.
//!
//! All stages are linear-phase (symmetric) and have an odd number of taps, so each one has a
//! whole-sample delay and the cascade can be centred exactly on a DSD sample.

use std::f64::consts::PI;

use super::{Cutoff, PcmRate};

/// The modified Bessel function of the first kind, order 0 (power series, as pysacd).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0f64, 1.0f64);
    for k in 1..50 {
        let t = x / (2.0 * f64::from(k));
        term *= t * t;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

/// A Kaiser-windowed sinc low-pass of `n` taps (n odd), cutoff `fc` in cycles per sample,
/// designed for `atten_db` of stopband attenuation, normalised to unity gain at DC.
pub(crate) fn kaiser_lowpass(n: usize, fc: f64, atten_db: f64) -> Vec<f64> {
    let beta = 0.1102 * (atten_db - 8.7);
    let m = (n as f64 - 1.0) / 2.0;
    let i0_beta = bessel_i0(beta);
    let mut h: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - m;
            let s = if x == 0.0 { 2.0 * fc } else { (2.0 * PI * fc * x).sin() / (PI * x) };
            let r = x / m;
            s * bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0_beta
        })
        .collect();
    let sum: f64 = h.iter().sum();
    for v in &mut h {
        *v /= sum;
    }
    h
}

/// A half-band low-pass of `n` taps (n = 4k + 3): cutoff at a quarter of the sampling rate,
/// every even offset from the centre exactly zero, the centre exactly 0.5. Returns the taps
/// at odd offsets 1, 3, 5, ... from the centre (one side; the filter is symmetric), scaled
/// so that both sides sum to exactly 0.5 (unity DC gain).
fn half_band(n: usize, atten_db: f64) -> Vec<f64> {
    let h = kaiser_lowpass(n, 0.25, atten_db);
    let m = (n - 1) / 2;
    let mut odd: Vec<f64> = (1..=m).step_by(2).map(|k| h[m + k]).collect();
    let sum: f64 = odd.iter().sum::<f64>() * 2.0;
    for v in &mut odd {
        *v *= 0.5 / sum;
    }
    odd
}

/// Stage 1: DSD64 to 352.8 kHz (÷8) by byte lookup tables.
///
/// Output `m` is centred on the first (most significant) bit of DSD byte `m` and reads
/// bytes `m - 6 ..= m + 5`: 96 bits, the first of which has a zero coefficient, so the
/// 95-tap filter is centred on a sample.
pub(crate) const S1_BYTES: usize = 12;
/// The byte offset of the first byte stage 1 reads, relative to the output's byte.
pub(crate) const S1_BEFORE: usize = 6;
/// Stage 1 design: pass to 48 kHz, stop from 352.8 - 48 = 304.8 kHz (what folds onto
/// 0-48 kHz at 352.8 kHz). Centre 176.4 kHz = fs/16.
const S1_ATTEN: f64 = 120.0;

/// The wide (80 kHz) filter's stage 1: 18 bytes (143 taps), pass to 80 kHz, stop from
/// 352.8 - 100 = 252.8 kHz (what folds onto 0-100 kHz at 352.8 kHz), centre 166.4 kHz.
pub(crate) const S1W_BYTES: usize = 18;
const S1W_ATTEN: f64 = 115.0;
/// The wide filter's second stage at 176.4 kHz output: 337 taps at 352.8 kHz, ÷2, pass to
/// 80 kHz, stop from 88.2 kHz (nothing folds back below the Nyquist frequency).
pub(crate) const W176_HALF: usize = 168;
/// ... and at 352.8 kHz output: 141 taps at 352.8 kHz, pass to 80 kHz, stop from 100 kHz.
pub(crate) const W352_HALF: usize = 70;
const W_ATTEN: f64 = 115.0;

/// Stage 1's fixed point: table entries are multiples of 2^-28. The largest sum of 12
/// entries (all bits matching the signs of the taps) is the sum of the taps' magnitudes,
/// about 1.1, so stage 1 outputs and their pairwise sums in stage 2 stay below 2^31. The
/// rounding of the entries is a fixed error of at most 2^-29 per entry: about -170 dB.
pub(crate) const S1_SHIFT: i32 = 28;
const S1_ONE: f64 = (1u32 << S1_SHIFT) as f64;

/// Half-band stages 2 (352.8 to 176.4 kHz, ÷2) and 4 (176.4 to 352.8 kHz, ×2): pass to
/// 48 kHz, stop from 176.4 - 48 = 128.4 kHz.
const HB_TAPS: usize = 39;
const HB_ATTEN: f64 = 120.0;
/// Pairs of a half-band stage: taps at odd offsets 1, 3, ..., 19.
pub(crate) const HB_PAIRS: usize = (HB_TAPS + 1) / 4;

/// The coefficients of every stage of one configuration, as the kernels use them.
#[derive(Debug, Clone)]
pub(crate) struct Filters {
    /// Stage 1 tables: `lut[j][v]` is the contribution of byte value `v` at position `j` of
    /// the 12-byte window (earliest first); bits most significant first, 1 is +1, 0 is -1;
    /// in units of 2^-[`S1_SHIFT`] (fixed point), so that the 12 lookups add exactly.
    /// 12 tables, or 18 for the wide filter.
    pub lut: Vec<[i32; 256]>,
    /// Stage 2 (half-band ÷2): the centre tap (0.5), then the taps at odd offsets 1, 3, ...;
    /// scaled by 2^-[`S1_SHIFT`] to take stage 1's fixed point back to 1.0 = full scale
    /// (exact: a power of two).
    pub s2: [f64; HB_PAIRS + 1],
    /// Stage 3 (the band-limiting filter at 176.4 kHz): the centre tap, then the taps at
    /// offsets 1, 2, ... (symmetric); for the 40 kHz half-band, the centre (0.5) and the
    /// taps at odd offsets 1, 3, ... only (the even ones are zero). f64, as it accumulates.
    pub s3: Vec<f64>,
    /// Stage 3's half-length: taps on each side of the centre, zeros included.
    pub s3_half: usize,
    /// Stage 4 (half-band ×2, 352.8 kHz output only): taps at odd offsets, doubled for the
    /// interpolation gain. f64, as it accumulates.
    pub s4: [f64; HB_PAIRS],
    /// The wide filter's second (and last) stage, straight from stage 1: the centre tap,
    /// then the taps at offsets 1, 2, ... at 352.8 kHz, scaled by 2^-[`S1_SHIFT`] like
    /// stage 2. Empty for the other cutoffs.
    pub wide: Vec<f64>,
}

/// Stage 1 lookup tables for a 8 x `bytes` - 1 tap Kaiser low-pass centred on the first
/// bit of byte `bytes / 2` of the window.
fn stage1_tables(bytes: usize, fc: f64, atten: f64) -> Vec<[i32; 256]> {
    let mut h1 = kaiser_lowpass(bytes * 8 - 1, fc, atten);
    // Window bit p (0..8 bytes) has coefficient h1[p - 1], bit 0 none. Make the eight
    // polyphase sums equal (1/8 each): the response then has exact zeros at every
    // multiple of 352.8 kHz, so any byte-periodic balanced pattern, DSD silence (0x69)
    // included, converts to zero. Symmetry is kept: bit p and bit 8 bytes - p are in
    // phases p mod 8 and -p mod 8, whose sums are equal by symmetry already.
    let mut phase = [0.0f64; 8];
    for (i, v) in h1.iter().enumerate() {
        phase[(i + 1) % 8] += v;
    }
    for (i, v) in h1.iter_mut().enumerate() {
        *v *= 0.125 / phase[(i + 1) % 8];
    }
    let mut lut = vec![[0i32; 256]; bytes];
    for (j, table) in lut.iter_mut().enumerate() {
        for (v, entry) in table.iter_mut().enumerate() {
            let mut acc = 0.0f64;
            for bit in 0..8 {
                let p = j * 8 + bit;
                if p == 0 {
                    continue;
                }
                let sign = if (v >> (7 - bit)) & 1 == 1 { 1.0 } else { -1.0 };
                acc += h1[p - 1] * sign;
            }
            *entry = (acc * S1_ONE).round() as i32;
        }
    }
    // Exact zero for silence after rounding the entries too: make the last entry for
    // the silence byte cancel the others (a change of at most a few units of 2^-28).
    let silence = usize::from(super::DSD_SILENCE);
    let partial: i32 = lut[..bytes - 1].iter().map(|t| t[silence]).sum();
    lut[bytes - 1][silence] = -partial;
    // The largest sum (every byte at its largest entry) stays far below 2^31, also for
    // the pairwise sums of the next stage.
    debug_assert!(
        lut.iter().map(|t| t.iter().map(|v| v.abs()).max().unwrap_or(0)).sum::<i32>() < i32::MAX / 4
    );
    lut
}

/// Stage 3 for 40 kHz: a half-band of 175 taps (pass to 40.2 kHz, stop from 48 kHz, the
/// transition centred on 44.1 kHz = a quarter of 176.4 kHz), so every other tap is zero
/// and it costs half a general filter of the same length. These are its non-zero pairs.
pub(crate) const S3_PAIRS_40: usize = 44;
const S3_TAPS_40: usize = 4 * S3_PAIRS_40 - 1;
/// Stage 3 for 30 kHz: a general Kaiser low-pass of 227 taps (pass to 30 kHz, stop from
/// 36 kHz), 113 each side of the centre.
pub(crate) const S3_HALF_30: usize = 113;
/// Design attenuation of stage 3, both cutoffs.
const S3_ATTEN: f64 = 115.0;

impl Filters {
    /// Designs every stage for a cutoff. For 40 and 30 kHz the stages do not depend on the
    /// output rate (88.2 and 352.8 kHz output only change which stages run); the wide
    /// filter has one design per rate. `Wide80` at 88.2 kHz is not valid (see
    /// `PcmConfig::validate`); here it gets the 176.4 kHz design.
    pub fn new(cutoff: Cutoff, rate: PcmRate) -> Filters {
        if cutoff == Cutoff::Wide80 {
            let lut = stage1_tables(S1W_BYTES, 166_400.0 / 2_822_400.0, S1W_ATTEN);
            let (half, fc) = if rate == PcmRate::R352800 {
                (W352_HALF, 90_000.0 / 352_800.0)
            } else {
                (W176_HALF, 84_100.0 / 352_800.0)
            };
            let h = kaiser_lowpass(2 * half + 1, fc, W_ATTEN);
            let wide = h[half..].iter().map(|v| v / S1_ONE).collect();
            return Filters {
                lut,
                s2: [0.0; HB_PAIRS + 1],
                s3: Vec::new(),
                s3_half: 0,
                s4: [0.0; HB_PAIRS],
                wide,
            };
        }
        let lut = stage1_tables(S1_BYTES, 1.0 / 16.0, S1_ATTEN);

        let hb = half_band(HB_TAPS, HB_ATTEN);
        let s2: [f64; HB_PAIRS + 1] = std::array::from_fn(|i| if i == 0 { 0.5 } else { hb[i - 1] } / S1_ONE);
        let s4: [f64; HB_PAIRS] = std::array::from_fn(|i| 2.0 * hb[i]);

        let (s3, s3_half) = match cutoff {
            Cutoff::Khz40 => {
                let mut v = vec![0.5];
                v.extend(half_band(S3_TAPS_40, S3_ATTEN));
                (v, 2 * S3_PAIRS_40 - 1)
            }
            Cutoff::Khz30 | Cutoff::Wide80 => {
                let n = 2 * S3_HALF_30 + 1;
                let h3 = kaiser_lowpass(n, 33_000.0 / 176_400.0, S3_ATTEN);
                (h3[S3_HALF_30..].to_vec(), S3_HALF_30)
            }
        };
        Filters { lut, s2, s3, s3_half, s4, wide: Vec::new() }
    }

    /// Stage 3's taps by offset from the centre, 0 to `s3_half`, zeros included.
    #[cfg(test)]
    pub fn s3_by_offset(&self) -> Vec<f64> {
        if self.s3.len() == self.s3_half + 1 {
            return self.s3.clone();
        }
        let mut v = vec![0.0; self.s3_half + 1];
        v[0] = self.s3[0];
        for (i, &h) in self.s3[1..].iter().enumerate() {
            v[2 * i + 1] = h;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_band_shape() {
        let hb = half_band(HB_TAPS, HB_ATTEN);
        assert_eq!(hb.len(), 10);
        let sum: f64 = hb.iter().sum::<f64>() * 2.0 + 0.5;
        assert!((sum - 1.0).abs() < 1e-15);
    }

    #[test]
    fn kaiser_is_symmetric_and_unity() {
        let h = kaiser_lowpass(169, 44.0 / 176.4, 115.0);
        for i in 0..h.len() {
            assert!((h[i] - h[h.len() - 1 - i]).abs() < 1e-15);
        }
        assert!((h.iter().sum::<f64>() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn silence_is_exact_zero_in_stage_1() {
        let f = Filters::new(Cutoff::Khz40, PcmRate::R176400);
        let s: i32 = f.lut.iter().map(|t| t[0x69]).sum();
        assert_eq!(s, 0);
    }

    #[test]
    fn stage_1_dc_gain_is_one() {
        let f = Filters::new(Cutoff::Khz40, PcmRate::R176400);
        let s: i32 = f.lut.iter().map(|t| t[0xff]).sum();
        assert!((f64::from(s) / S1_ONE - 1.0).abs() < 1e-7, "{s}");
    }
}
