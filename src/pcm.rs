// SPDX-License-Identifier: GPL-3.0-or-later
//
// The structure of this converter (a ÷8 stage of byte lookup tables, then ÷2 FIR stages,
// all Kaiser-windowed and linear-phase) is ported from pysacd's `csrc/dsp.c` at commit
// 2113789 (pysacd HEAD 846030d), `SPDX-License-Identifier: GPL-2.0-or-later`, by sacdfs's
// author; see `pcm/filters.rs`. pysacd streams with filter state carried from frame to
// frame; this port is stateless per frame instead, and has new filter designs.
//
// Stage 1's lookup tables of filter partial sums, indexed by whole DSD bytes, are the
// technique credited to Sebastian Gesemann's dsd2pcm (BSD-3-Clause, 2009), which FFmpeg
// and MPD also use.

//! DSD to PCM conversion: DSD64 (2.8224 MHz, 1 bit) to 24-bit PCM at 88.2, 176.4 or
//! 352.8 kHz, one SACD frame at a time.
//!
//! **Stateless per frame.** Frame `f`'s PCM is computed from DSD frames `f - 1`, `f` and
//! `f + 1` only, and equals, bit for bit, what converting the whole area in one pass gives
//! ([`convert_area`]). That is what lets any byte range of a WAV file be produced on its
//! own. Each output sample depends on a fixed window of DSD bits (all stages are FIR) whose
//! half-length (at most about 2 200 DSD samples, 0.8 ms) is far less than a frame (37 632).
//!
//! **Aligned.** PCM sample `n` of a frame is centred on DSD sample `d × n` of the same
//! frame, where `d` is the decimation (32, 16 or 8): the filter delay is compensated
//! exactly, because every stage has an odd length and so a whole-sample delay.
//!
//! **The stages** (designs and why in `docs/pcm-quality.md`):
//!
//! 1. 2.8224 MHz to 352.8 kHz (÷8): 95 taps applied as 12 byte lookup tables, in integers.
//!    Pass to 48 kHz, stop from 304.8 kHz.
//! 2. 352.8 to 176.4 kHz (÷2): a 39-tap half-band. Pass to 48 kHz, stop from 128.4 kHz.
//! 3. The band limit, at 176.4 kHz: pass to 40 kHz, stop from 48 kHz (a 175-tap half-band),
//!    or pass to 30 kHz, stop from 36 kHz (227 taps). Computed at every sample for 176.4 and
//!    352.8 kHz output, at every other sample (÷2) for 88.2 kHz.
//! 4. 352.8 kHz output only: ×2 interpolation by the same half-band as stage 2.
//!
//! **Arithmetic.** Stage 1 adds integer table entries (exact); stages 2 to 4 accumulate in
//! f64. Each output is a fixed sequence of operations (Rust never fuses a multiply and an
//! add, nor reorders float sums), so the result is the same, bit for bit, whatever SIMD
//! width the compiler picked (SSE2 on generic x86-64, AVX2 on x86-64-v3) and on any
//! machine. The FIR kernels are compiled for the baseline, x86-64-v3 and x86-64-v4 and
//! chosen at run time ([`Simd`]); all three give the same bytes.

mod filters;
pub mod sb;
mod simd;

use std::ops::{Add, Mul};

use filters::{Filters, HB_PAIRS, S1_BEFORE, S1_BYTES};
use simd::Kernel;
pub use simd::Simd;

/// DSD bytes per channel in one SACD frame (1/75 s).
pub const FRAME_BYTES: usize = 4704;

/// The DSD silence pattern (01101001), used for the frames before the first and after the
/// last frame of an area. It converts to exact zero.
pub const DSD_SILENCE: u8 = 0x69;

/// Stage 3 output samples (176.4 kHz) per frame.
const S3_PER_FRAME: usize = FRAME_BYTES / 2;

/// A PCM output rate: DSD64 divided by a power of two.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PcmRate {
    /// 88.2 kHz (÷32).
    R88200,
    /// 176.4 kHz (÷16), the default.
    #[default]
    R176400,
    /// 352.8 kHz (÷8).
    R352800,
}

impl PcmRate {
    /// The rate in Hz.
    pub fn hz(self) -> u32 {
        match self {
            PcmRate::R88200 => 88_200,
            PcmRate::R176400 => 176_400,
            PcmRate::R352800 => 352_800,
        }
    }

    /// PCM samples per channel in one SACD frame: 1176, 2352 or 4704.
    pub fn samples_per_frame(self) -> usize {
        FRAME_BYTES * 8 / self.decimation()
    }

    /// DSD samples per PCM sample: 32, 16 or 8.
    pub fn decimation(self) -> usize {
        match self {
            PcmRate::R88200 => 32,
            PcmRate::R176400 => 16,
            PcmRate::R352800 => 8,
        }
    }
}

/// The top of the pass band.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cutoff {
    /// Pass to 40 kHz, fully attenuated (at least 105 dB) from 48 kHz. The default.
    #[default]
    Khz40,
    /// Pass to 30 kHz, fully attenuated from 36 kHz.
    Khz30,
    /// Wide: pass to 80 kHz, fully attenuated from 88.2 kHz (176.4 kHz output) or from
    /// 100 kHz (352.8 kHz output). Not for 88.2 kHz output, whose band ends at 44.1 kHz.
    Wide80,
}

/// What a [`Converter`] produces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PcmConfig {
    /// The output rate.
    pub rate: PcmRate,
    /// The band limit.
    pub cutoff: Cutoff,
    /// Gain in dB; 0 maps 100 % DSD modulation to full scale. Samples beyond full scale are
    /// clamped and counted. A value that is not finite counts as 0.
    pub gain_db: f32,
}

impl Default for PcmConfig {
    fn default() -> Self {
        PcmConfig { rate: PcmRate::default(), cutoff: Cutoff::default(), gain_db: 0.0 }
    }
}

impl PcmConfig {
    /// Checks that the cutoff fits the rate: [`Cutoff::Wide80`] needs 176.4 or 352.8 kHz.
    /// [`Converter::new`] does not fail on an invalid configuration: it uses
    /// [`Cutoff::Khz40`] instead (see [`PcmConfig::effective`]), so check options with this
    /// first.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.cutoff == Cutoff::Wide80 && self.rate == PcmRate::R88200 {
            Err("the wide (80 kHz) filter needs 176.4 or 352.8 kHz output")
        } else {
            Ok(())
        }
    }

    /// The configuration a [`Converter`] uses: this one, or with [`Cutoff::Khz40`] if
    /// [`validate`](Self::validate) refuses it.
    pub fn effective(&self) -> PcmConfig {
        if self.validate().is_ok() { *self } else { PcmConfig { cutoff: Cutoff::Khz40, ..*self } }
    }
}

/// The largest and smallest 24-bit sample.
const MAX24: f64 = 8_388_607.0;
const MIN24: f64 = -8_388_608.0;
/// Adding and subtracting 1.5 × 2^52 rounds an f64 below 2^51 in size to the nearest
/// integer, ties to even, with plain SSE2 arithmetic (`f64::round` would be a libm call
/// on baseline x86-64).
const ROUND_MAGIC: f64 = 6_755_399_441_055_744.0;

/// Margins of each intermediate signal beyond the frames converted, in its own samples.
#[derive(Clone, Copy, Debug)]
struct Extents {
    /// Stage 3 (176.4 kHz) samples needed beyond the output: stage 4's reach.
    e3: usize,
    /// Stage 2 samples: `e3` plus stage 3's half-length.
    e2: usize,
    /// Stage 1 sample pairs (even and odd): `e2` plus the half-band's reach.
    e1: usize,
    /// DSD bytes.
    e0: usize,
}

impl Extents {
    fn new(cfg: &PcmConfig, filters: &Filters) -> Extents {
        if cfg.cutoff == Cutoff::Wide80 {
            // One stage after stage 1, reaching `half` stage 1 samples each way.
            let half = filters.wide.len().saturating_sub(1);
            let e1 = half.div_ceil(2) + 1;
            return Extents { e3: 0, e2: 0, e1, e0: 2 * e1 + filters::S1W_BYTES / 2 };
        }
        let e3 = if cfg.rate == PcmRate::R352800 { HB_PAIRS } else { 0 };
        let e2 = e3 + filters.s3_half;
        let e1 = e2 + HB_PAIRS;
        // Stage 1 output m (at 352.8 kHz) reads bytes m - 6 ..= m + 5.
        let e0 = 2 * e1 + S1_BEFORE;
        Extents { e3, e2, e1, e0 }
    }
}

/// The working buffers and filters for converting `frames` frames of one channel.
#[derive(Debug, Clone)]
struct Pipeline {
    cfg: PcmConfig,
    filters: Filters,
    ext: Extents,
    frames: usize,
    /// DSD bytes of one channel, `e0` before and after the frames.
    bytes: Vec<u8>,
    /// Stage 1 output (fixed point, see `filters::S1_SHIFT`), even and odd samples apart.
    s1e: Vec<i32>,
    s1o: Vec<i32>,
    /// Stage 2 output.
    s2: Vec<f64>,
    /// Stage 2 output split by phase for the decimating stage 3 (88.2 kHz only).
    s2e: Vec<f64>,
    s2o: Vec<f64>,
    /// Stage 3 output (f64: see [`sym_fir`]).
    s3: Vec<f64>,
    /// Stage 4's odd samples (352.8 kHz only).
    s4: Vec<f64>,
    /// Gain as a multiplier of full scale, and of the 24-bit step.
    gain: f64,
    scale24: f64,
    /// Which copy of the kernels runs.
    simd: Simd,
}

impl Pipeline {
    fn new(cfg: &PcmConfig, frames: usize) -> Pipeline {
        let cfg = &cfg.effective();
        let filters = Filters::new(cfg.cutoff, cfg.rate);
        let ext = Extents::new(cfg, &filters);
        let gain_db = if cfg.gain_db.is_finite() { f64::from(cfg.gain_db) } else { 0.0 };
        let gain = 10f64.powf(gain_db / 20.0);
        let mut p = Pipeline {
            cfg: *cfg,
            ext,
            frames: 0,
            bytes: Vec::new(),
            s1e: Vec::new(),
            s1o: Vec::new(),
            s2: Vec::new(),
            s2e: Vec::new(),
            s2o: Vec::new(),
            s3: Vec::new(),
            s4: Vec::new(),
            filters,
            gain,
            scale24: gain * 8_388_608.0,
            simd: Simd::selected(),
        };
        p.set_frames(frames);
        p
    }

    /// Sizes the buffers for converting `frames` frames at once (the filters are kept).
    fn set_frames(&mut self, frames: usize) {
        if frames == self.frames && !self.bytes.is_empty() {
            return;
        }
        let ext = self.ext;
        let n3 = S3_PER_FRAME * frames;
        let wide = self.cfg.cutoff == Cutoff::Wide80;
        let (s2_len, s2_split, s3_len, s4_len) = match (wide, self.cfg.rate) {
            // Wide at 352.8 kHz: s2 holds stage 1 interleaved again, in f64.
            (true, PcmRate::R352800) => (2 * (n3 + 2 * ext.e1), 0, 2 * n3, 0),
            (true, _) => (0, 0, n3, 0),
            (false, PcmRate::R88200) => (n3 + 2 * ext.e2, n3 / 2 + self.filters.s3_half + 2, n3 / 2, 0),
            (false, PcmRate::R176400) => (n3 + 2 * ext.e2, 0, n3, 0),
            (false, PcmRate::R352800) => (n3 + 2 * ext.e2, 0, n3 + 2 * ext.e3, n3),
        };
        self.frames = frames;
        self.bytes.resize(FRAME_BYTES * frames + 2 * ext.e0, DSD_SILENCE);
        self.s1e.resize(n3 + 2 * ext.e1, 0);
        self.s1o.resize(n3 + 2 * ext.e1, 0);
        self.s2.resize(s2_len, 0.0);
        self.s2e.resize(s2_split, 0.0);
        self.s2o.resize(s2_split, 0.0);
        self.s3.resize(s3_len, 0.0);
        self.s4.resize(s4_len, 0.0);
    }

    /// Output samples per channel.
    fn out_len(&self) -> usize {
        self.cfg.rate.samples_per_frame() * self.frames
    }

    /// Runs every stage on `self.bytes`; the result is read with [`Pipeline::sample`].
    fn run(&mut self) {
        if self.cfg.cutoff == Cutoff::Wide80 {
            self.run_wide();
            return;
        }
        if let Some(lut) = self.filters.lut.first_chunk::<S1_BYTES>() {
            stage1::<S1_BYTES>(self.simd, lut, &self.bytes, &mut self.s1e, &mut self.s1o);
        }
        // Stage 2: s2[r] = 0.5 s1[2j] + sum_i h_i (s1[2j + 2i + 1] + s1[2j - 2i - 1]),
        // j = r - e2; in pair indices s1e[r + HB], s1o[r + HB + i], s1o[r + HB - 1 - i].
        let h2 = &self.filters.s2;
        if let Some(taps) = h2[1..].first_chunk::<HB_PAIRS>() {
            let o = &self.s1o[..];
            sym_fir::<i32, f64, HB_PAIRS, 1>(
                self.simd,
                &mut self.s2,
                h2[0],
                taps,
                &self.s1e[HB_PAIRS..],
                &o[HB_PAIRS..],
                o,
            );
        }
        match self.cfg.cutoff {
            Cutoff::Khz40 => self.stage3_half_band::<{ filters::S3_PAIRS_40 }>(),
            Cutoff::Khz30 | Cutoff::Wide80 => self.stage3::<{ filters::S3_HALF_30 }>(),
        }
        if self.cfg.rate == PcmRate::R352800 {
            // Odd outputs between s3[n] and s3[n + 1] (n counted from e3):
            // sum_i h_i (s3[n + 1 + i] + s3[n - i]).
            let s3 = &self.s3[..];
            let e3 = self.ext.e3;
            sym_fir::<f64, f64, HB_PAIRS, 1>(
                self.simd,
                &mut self.s4,
                0.0,
                &self.filters.s4,
                &s3[e3..],
                &s3[e3 + 1..],
                &s3[e3 + 1 - HB_PAIRS..],
            );
        }
    }

    /// The wide filter: stage 1 with 18 tables, then one filter at 352.8 kHz, decimating by
    /// 2 for 176.4 kHz output.
    fn run_wide(&mut self) {
        if let Some(lut) = self.filters.lut.first_chunk::<{ filters::S1W_BYTES }>() {
            stage1::<{ filters::S1W_BYTES }>(self.simd, lut, &self.bytes, &mut self.s1e, &mut self.s1o);
        }
        let e1 = self.ext.e1;
        let h = &self.filters.wide;
        match self.cfg.rate {
            PcmRate::R352800 => {
                const K: usize = filters::W352_HALF;
                let Some(taps) = h.get(1..).and_then(|t| t.first_chunk::<K>()) else {
                    return;
                };
                // Stage 1 at full rate (exact in f64), then out[i] centred on s1[i + 2 e1].
                for (q, pair) in self.s2.chunks_exact_mut(2).enumerate() {
                    pair[0] = f64::from(self.s1e[q]);
                    pair[1] = f64::from(self.s1o[q]);
                }
                let x = &self.s2[2 * e1 - K..];
                sym_fir::<f64, f64, K, 1>(self.simd, &mut self.s3, h[0], taps, &x[K..], &x[K + 1..], x);
            }
            PcmRate::R176400 | PcmRate::R88200 => {
                const K: usize = filters::W176_HALF;
                let Some(taps) = h.get(1..).and_then(|t| t.first_chunk::<K>()) else {
                    return;
                };
                // out[j] centred on s1[2j + 2 e1] = s1e[j + e1]: tap k even reads s1e,
                // odd reads s1o (see dec_lanes).
                let (p, q) = (&self.s1e[e1 - K / 2..], &self.s1o[e1 - K.div_ceil(2)..]);
                dec_fir::<i32, K>(self.simd, &mut self.s3, h[0], taps, p, q);
            }
        }
    }

    /// Stage 3 as a half-band with `N` non-zero tap pairs, at odd offsets up to 2N - 1.
    fn stage3_half_band<const N: usize>(&mut self) {
        let h3 = &self.filters.s3;
        let Some(taps) = h3.get(1..).and_then(|t| t.first_chunk::<N>()) else {
            return;
        };
        let k = 2 * N - 1;
        let s2 = &self.s2[..];
        match self.cfg.rate {
            PcmRate::R176400 | PcmRate::R352800 => {
                // s3[t] = 0.5 s2[t + k] + sum_i h_i (s2[t + k + 1 + 2i] + s2[t + k - 1 - 2i]).
                sym_fir::<f64, f64, N, 2>(self.simd, &mut self.s3, h3[0], taps, &s2[k..], &s2[k + 1..], s2);
            }
            PcmRate::R88200 => {
                // s3[q] centred on s2[2q + k], k odd: every tap pair reads even samples
                // (s2[2(q + N + i)] and s2[2(q + N - 1 - i)]), the centre an odd one.
                for (j, v) in self.s2e.iter_mut().enumerate() {
                    *v = s2.get(2 * j).copied().unwrap_or(0.0);
                }
                for (j, v) in self.s2o.iter_mut().enumerate() {
                    *v = s2.get(2 * j + 1).copied().unwrap_or(0.0);
                }
                let (e, o) = (&self.s2e[..], &self.s2o[..]);
                sym_fir::<f64, f64, N, 1>(self.simd, &mut self.s3, h3[0], taps, &o[N - 1..], &e[N..], e);
            }
        }
    }

    /// Stage 3 with `K` taps on each side of the centre.
    fn stage3<const K: usize>(&mut self) {
        let h3 = &self.filters.s3;
        let Some(taps) = h3.get(1..).and_then(|t| t.first_chunk::<K>()) else {
            return;
        };
        let s2 = &self.s2[..];
        match self.cfg.rate {
            PcmRate::R176400 | PcmRate::R352800 => {
                // s3[t] centred on s2[t + K]: pairs s2[t + K + 1 + i] and s2[t + K - 1 - i].
                sym_fir::<f64, f64, K, 1>(self.simd, &mut self.s3, h3[0], taps, &s2[K..], &s2[K + 1..], s2);
            }
            PcmRate::R88200 => {
                // s3[q] centred on s2[2q + K]. Split s2 into the samples of the centre's
                // parity (P) and the other (Q), so that every tap reads a contiguous run:
                // P[j] = s2[K + 2 (j - K/2)], Q[j] = s2[K + 1 + 2 (j - (K + 1)/2)].
                for (j, v) in self.s2e.iter_mut().enumerate() {
                    *v = s2.get((K + 2 * j).wrapping_sub(2 * (K / 2))).copied().unwrap_or(0.0);
                }
                for (j, v) in self.s2o.iter_mut().enumerate() {
                    *v = s2.get((K + 1 + 2 * j).wrapping_sub(2 * K.div_ceil(2))).copied().unwrap_or(0.0);
                }
                dec_fir::<f64, K>(self.simd, &mut self.s3, h3[0], taps, &self.s2e, &self.s2o);
            }
        }
    }

    /// Output sample `i` of the channel just run, at full scale 1.0, before the gain.
    #[inline]
    fn sample(&self, i: usize) -> f64 {
        if self.cfg.cutoff == Cutoff::Wide80 {
            return self.s3[i];
        }
        match self.cfg.rate {
            PcmRate::R88200 | PcmRate::R176400 => self.s3[i],
            PcmRate::R352800 => {
                if i % 2 == 0 {
                    self.s3[self.ext.e3 + i / 2]
                } else {
                    self.s4[i / 2]
                }
            }
        }
    }

    /// Writes the channel just run into `out` as interleaved 24-bit little-endian samples
    /// (`channels` per sample frame, this one at `c`); returns the clipped count.
    fn write24(&self, out: &mut [u8], channels: usize, c: usize) -> u32 {
        self.write24_from(out, channels, c, 0, self.out_len())
    }

    /// As [`write24`](Self::write24), for the `count` samples from output sample `first`
    /// (written from the start of `out`).
    fn write24_from(&self, out: &mut [u8], channels: usize, c: usize, first: usize, count: usize) -> u32 {
        let mut clips = 0u32;
        let stride = 3 * channels;
        let count = count.min(self.out_len().saturating_sub(first));
        for (i, slot) in out.chunks_exact_mut(stride).take(count).enumerate() {
            let v = self.sample(first + i) * self.scale24;
            let r = (v + ROUND_MAGIC) - ROUND_MAGIC;
            let r = if r > MAX24 {
                clips = clips.saturating_add(1);
                MAX24
            } else if r < MIN24 {
                clips = clips.saturating_add(1);
                MIN24
            } else {
                r
            };
            let b = (r as i32).to_le_bytes();
            slot[3 * c..3 * c + 3].copy_from_slice(&b[..3]);
        }
        clips
    }

    /// Writes the channel just run into `out` as interleaved f32 samples, gain applied.
    fn write_f32(&self, out: &mut [f32], channels: usize, c: usize) {
        for (i, slot) in out.chunks_exact_mut(channels).take(self.out_len()).enumerate() {
            slot[c] = (self.sample(i) * self.gain) as f32;
        }
    }
}

/// Stage 1: `e[q]` and `o[q]` are the outputs centred on the first bit of bytes `2q` and
/// `2q + 1` of the window (counted from its first converted byte, `B/2` bytes in).
/// Each output is `lut[0][w[0]] + lut[1][w[1]] + ... + lut[B-1][w[B-1]]`, in integers, so
/// exact (and the same in any order). The `B` bytes (12, or 18 for the wide filter) are read
/// as words and taken apart with shifts, which halves the loads.
fn stage1<const B: usize>(simd: Simd, lut: &[[i32; 256]; B], bytes: &[u8], e: &mut [i32], o: &mut [i32]) {
    struct Job<'a, const B: usize>(&'a [[i32; 256]; B], &'a [u8], &'a mut [i32], &'a mut [i32]);
    impl<const B: usize> Kernel for Job<'_, B> {
        type Out = ();
        #[inline(always)]
        fn run<const W: usize>(self) {
            stage1_k(self.0, self.1, self.2, self.3);
        }
    }
    simd::dispatch(simd, Job(lut, bytes, e, o));
}

/// [`stage1`]'s body, compiled into each instruction set's copy.
#[inline(always)]
fn stage1_k<const B: usize>(lut: &[[i32; 256]; B], bytes: &[u8], e: &mut [i32], o: &mut [i32]) {
    #[inline(always)]
    fn one<const B: usize>(lut: &[[i32; 256]; B], w: &[u8; B]) -> i32 {
        let mut acc = 0i32;
        let mut j = 0;
        while j + 8 <= B {
            let x = w[j..].first_chunk::<8>().map_or(0, |b| u64::from_le_bytes(*b));
            for k in 0..8 {
                acc = acc.wrapping_add(lut[j + k][usize::from((x >> (8 * k)) as u8)]);
            }
            j += 8;
        }
        if j + 4 <= B {
            let x = w[j..].first_chunk::<4>().map_or(0, |b| u32::from_le_bytes(*b));
            for k in 0..4 {
                acc = acc.wrapping_add(lut[j + k][usize::from((x >> (8 * k)) as u8)]);
            }
            j += 4;
        }
        while j < B {
            acc = acc.wrapping_add(lut[j][usize::from(w[j])]);
            j += 1;
        }
        acc
    }
    for (q, (e, o)) in e.iter_mut().zip(o.iter_mut()).enumerate() {
        let Some(w) = bytes.get(2 * q..2 * q + B + 1) else {
            break;
        };
        let (Some(we), Some(wo)) = (w.first_chunk::<B>(), w[1..].first_chunk::<B>()) else {
            break;
        };
        *e = one(lut, we);
        *o = one(lut, wo);
    }
}

/// Adding the two input samples of a tap pair: wrapping for stage 1's fixed point (which
/// cannot overflow, see `filters::S1_SHIFT`; wrapping keeps the release build's overflow
/// checks out of the inner loop), plain for floats.
trait PairAdd {
    fn pair_add(self, other: Self) -> Self;
}

impl PairAdd for i32 {
    #[inline(always)]
    fn pair_add(self, other: Self) -> Self {
        self.wrapping_add(other)
    }
}

impl PairAdd for f64 {
    #[inline(always)]
    fn pair_add(self, other: Self) -> Self {
        self + other
    }
}

/// Outputs computed together, so that the accumulators stay in registers: the least, used
/// after the wider blocks of the AVX paths (see `simd`).
const BLOCK: usize = 16;

/// A symmetric FIR with `N` tap pairs `S` apart (2 for a half-band, whose other taps are
/// zero), one output per input position:
/// `out[t] = h0 * c[t] + sum_i h[i] * (a[t + S i] + b[t + S (N - 1 - i)])`, the pair added in the
/// input type `I` and widened to the accumulator type `T`, in exactly that order for every
/// `t`: in blocks of [`BLOCK`] outputs (vectorised), then one at a time with the same code.
///
/// Stage 2 reads stage 1's integers (exact pair sums) and accumulates in f64, as stages 3
/// and 4 do. Stage 3 sums 85 to 114 products: in f32 its rounding alone measured 0.34 of a
/// 24-bit step RMS on random DSD, more than the 24-bit quantisation itself (0.29), and an
/// f32 stage 2 added 0.15; in f64 both are negligible (see the arithmetic test).
///
/// Runs the copy for `simd` (see [`Simd`]).
#[inline]
fn sym_fir<I, T, const N: usize, const S: usize>(
    simd: Simd,
    out: &mut [T],
    h0: T,
    h: &[T; N],
    c: &[I],
    a: &[I],
    b: &[I],
) where
    I: Copy + PairAdd,
    T: Copy + Default + From<I> + Mul<Output = T> + Add<Output = T>,
{
    struct Job<'a, I, T, const N: usize, const S: usize>(
        &'a mut [T],
        T,
        &'a [T; N],
        &'a [I],
        &'a [I],
        &'a [I],
    );
    impl<I, T, const N: usize, const S: usize> Kernel for Job<'_, I, T, N, S>
    where
        I: Copy + PairAdd,
        T: Copy + Default + From<I> + Mul<Output = T> + Add<Output = T>,
    {
        type Out = ();
        #[inline(always)]
        fn run<const W: usize>(self) {
            sym_fir_k::<I, T, N, S, W>(self.0, self.1, self.2, self.3, self.4, self.5);
        }
    }
    simd::dispatch(simd, Job::<I, T, N, S>(out, h0, h, c, a, b));
}

/// [`sym_fir`]'s body, compiled into each instruction set's copy.
#[inline(always)]
fn sym_fir_k<I, T, const N: usize, const S: usize, const W: usize>(
    out: &mut [T],
    h0: T,
    h: &[T; N],
    c: &[I],
    a: &[I],
    b: &[I],
) where
    I: Copy + PairAdd,
    T: Copy + Default + From<I> + Mul<Output = T> + Add<Output = T>,
{
    let n = out.len();
    let mut t = 0;
    while W > BLOCK && t + W <= n {
        sym_lanes::<I, T, N, S, W>(
            &mut out[t..],
            h0,
            h,
            &c[t.min(c.len())..],
            &a[t.min(a.len())..],
            &b[t.min(b.len())..],
        );
        t += W;
    }
    while t + BLOCK <= n {
        sym_lanes::<I, T, N, S, BLOCK>(
            &mut out[t..],
            h0,
            h,
            &c[t.min(c.len())..],
            &a[t.min(a.len())..],
            &b[t.min(b.len())..],
        );
        t += BLOCK;
    }
    while t < n {
        sym_lanes::<I, T, N, S, 1>(
            &mut out[t..],
            h0,
            h,
            &c[t.min(c.len())..],
            &a[t.min(a.len())..],
            &b[t.min(b.len())..],
        );
        t += 1;
    }
}

/// `W` outputs of [`sym_fir`], from slices starting at the first output. The windows are
/// cut to their exact lengths first, so the loops need no bounds checks.
#[inline(always)]
fn sym_lanes<I, T, const N: usize, const S: usize, const W: usize>(
    out: &mut [T],
    h0: T,
    h: &[T; N],
    c: &[I],
    a: &[I],
    b: &[I],
) where
    I: Copy + PairAdd,
    T: Copy + Default + From<I> + Mul<Output = T> + Add<Output = T>,
{
    let (Some(out), Some(c), Some(a), Some(b)) = (
        out.first_chunk_mut::<W>(),
        c.first_chunk::<W>(),
        a.get(..S * (N - 1) + W),
        b.get(..S * (N - 1) + W),
    ) else {
        return;
    };
    let mut acc = [T::default(); W];
    for l in 0..W {
        acc[l] = h0 * T::from(c[l]);
    }
    for (i, &hi) in h.iter().enumerate() {
        for l in 0..W {
            acc[l] = acc[l] + hi * T::from(a[S * i + l].pair_add(b[S * (N - 1 - i) + l]));
        }
    }
    *out = acc;
}

/// Stage 3 decimating by 2 (88.2 kHz output), `K` taps each side:
/// `out[q] = h0 s2[2q + K] + sum_k h[k - 1] (s2[2q + K + k] + s2[2q + K - k])`, k = 1..=K
/// in order, from the two phases of s2 (see [`Pipeline::stage3`]): even k read `p`, odd k
/// read `q`.
/// Runs the copy for `simd` (see [`Simd`]).
#[inline]
fn dec_fir<I: Copy + PairAdd, const K: usize>(
    simd: Simd,
    out: &mut [f64],
    h0: f64,
    h: &[f64; K],
    p: &[I],
    q: &[I],
) where
    f64: From<I>,
{
    struct Job<'a, I, const K: usize>(&'a mut [f64], f64, &'a [f64; K], &'a [I], &'a [I]);
    impl<I: Copy + PairAdd, const K: usize> Kernel for Job<'_, I, K>
    where
        f64: From<I>,
    {
        type Out = ();
        #[inline(always)]
        fn run<const W: usize>(self) {
            dec_fir_k::<I, K, W>(self.0, self.1, self.2, self.3, self.4);
        }
    }
    simd::dispatch(simd, Job::<I, K>(out, h0, h, p, q));
}

/// [`dec_fir`]'s body, compiled into each instruction set's copy.
#[inline(always)]
fn dec_fir_k<I: Copy + PairAdd, const K: usize, const W: usize>(
    out: &mut [f64],
    h0: f64,
    h: &[f64; K],
    p: &[I],
    q: &[I],
) where
    f64: From<I>,
{
    let n = out.len();
    let mut t = 0;
    while W > BLOCK && t + W <= n {
        dec_lanes::<I, K, W>(&mut out[t..], h0, h, &p[t.min(p.len())..], &q[t.min(q.len())..]);
        t += W;
    }
    while t + BLOCK <= n {
        dec_lanes::<I, K, BLOCK>(&mut out[t..], h0, h, &p[t.min(p.len())..], &q[t.min(q.len())..]);
        t += BLOCK;
    }
    while t < n {
        dec_lanes::<I, K, 1>(&mut out[t..], h0, h, &p[t.min(p.len())..], &q[t.min(q.len())..]);
        t += 1;
    }
}

/// `W` outputs of [`dec_fir`]. With `rp = K/2` and `rq = (K+1)/2`, output l's centre is
/// `p[rp + l]`; tap k = 2m reads `p[rp + l ± m]`, tap k = 2m + 1 reads `q[rq + l + m]` and
/// `q[rq + l - m - 1]`.
#[inline(always)]
fn dec_lanes<I: Copy + PairAdd, const K: usize, const W: usize>(
    out: &mut [f64],
    h0: f64,
    h: &[f64; K],
    p: &[I],
    q: &[I],
) where
    f64: From<I>,
{
    let rp = K / 2;
    let rq = K.div_ceil(2);
    let (Some(out), Some(p), Some(q)) =
        (out.first_chunk_mut::<W>(), p.get(..2 * rp + W), q.get(..2 * rq + W))
    else {
        return;
    };
    let mut acc = [0.0f64; W];
    for l in 0..W {
        acc[l] = h0 * f64::from(p[rp + l]);
    }
    // Taps in order k = 1, 2, 3, ...: odd k = 2m + 1, then even k = 2m + 2.
    for m in 0..K / 2 {
        let (ho, he) = (h[2 * m], h[2 * m + 1]);
        for l in 0..W {
            acc[l] += ho * f64::from(q[rq + l + m].pair_add(q[rq + l - m - 1]));
        }
        for l in 0..W {
            acc[l] += he * f64::from(p[rp + l + m + 1].pair_add(p[rp + l - m - 1]));
        }
    }
    if K % 2 == 1 {
        let m = K / 2;
        for l in 0..W {
            acc[l] += h[K - 1] * f64::from(q[rq + l + m].pair_add(q[rq + l - m - 1]));
        }
    }
    *out = acc;
}

/// Converts DSD frames to PCM, one frame at a time. Make one per worker thread: it holds
/// its working buffers (about 100 KiB), so converting allocates nothing.
#[derive(Debug, Clone)]
pub struct Converter {
    channels: usize,
    p: Pipeline,
    /// The pipeline of [`convert_run`](Self::convert_run), sized for the last run; made on
    /// first use from `p` (same filters and instruction set).
    run: Option<Box<Pipeline>>,
}

/// Fills one channel's window: the last `e0` bytes of `prev`, the `frames`
/// ([`FRAME_BYTES`] each), the first `e0` bytes of `next`, with DSD silence where a frame is
/// missing or short. `window` is `e0 + frames.len() × FRAME_BYTES + e0` long.
fn fill_window(
    window: &mut [u8],
    e0: usize,
    ch: usize,
    c: usize,
    prev: Option<&[u8]>,
    frames: &[&[u8]],
    next: Option<&[u8]>,
) {
    let at = |frame: Option<&[u8]>, i: usize| -> u8 {
        frame.and_then(|f| f.get(i * ch + c)).copied().unwrap_or(DSD_SILENCE)
    };
    let (head, rest) = window.split_at_mut(e0.min(window.len()));
    for (i, b) in head.iter_mut().enumerate() {
        *b = at(prev, FRAME_BYTES - e0 + i);
    }
    let mut chunks = rest.chunks_mut(FRAME_BYTES);
    for (&cur, body) in frames.iter().zip(chunks.by_ref()) {
        for (i, b) in body.iter_mut().enumerate() {
            *b = at(Some(cur), i);
        }
    }
    // The tail (e0 < FRAME_BYTES, so one chunk).
    for tail in chunks {
        for (i, b) in tail.iter_mut().enumerate() {
            *b = at(next, i);
        }
    }
}

impl Converter {
    /// A converter for frames of `channels` channels (0 counts as 1). It never fails: a
    /// configuration that [`PcmConfig::validate`] refuses (the wide filter at 88.2 kHz) is
    /// converted with [`PcmConfig::effective`] instead, which [`config`](Self::config)
    /// reports. Designing the filters takes well under a millisecond.
    pub fn new(channels: u8, cfg: &PcmConfig) -> Converter {
        Converter::with_simd(channels, cfg, Simd::selected())
    }

    /// As [`new`](Self::new), with the code for `simd` (or the best the CPU has, if that is
    /// less) instead of [`Simd::selected`]. Every choice gives the same output; this is for
    /// tests and measurements.
    pub fn with_simd(channels: u8, cfg: &PcmConfig, simd: Simd) -> Converter {
        let mut p = Pipeline::new(cfg, 1);
        p.simd = simd.min(Simd::detected());
        Converter { channels: usize::from(channels.max(1)), p, run: None }
    }

    /// The instruction set this converter's kernels use.
    pub fn simd(&self) -> Simd {
        self.p.simd
    }

    /// The configuration.
    pub fn config(&self) -> &PcmConfig {
        &self.p.cfg
    }

    /// Bytes of one converted frame: `samples_per_frame × channels × 3`.
    pub fn frame_bytes(&self) -> usize {
        self.p.out_len() * self.channels * 3
    }

    /// Fills the channel `c` window from the three frames: the last `e0` bytes of `prev`,
    /// `cur`, the first `e0` bytes of `next`, with DSD silence where a frame is missing or
    /// short.
    fn fill(&mut self, prev: Option<&[u8]>, cur: &[u8], next: Option<&[u8]>, c: usize) {
        fill_window(&mut self.p.bytes, self.p.ext.e0, self.channels, c, prev, &[cur], next);
    }

    /// Converts DSD frame `cur` (`channels × 4704` interleaved MSB-first bytes, as
    /// scarletbook's `AudioFrame.data`) into interleaved 24-bit little-endian PCM in `out`
    /// ([`frame_bytes`](Self::frame_bytes) long). `prev` and `next` are the neighbouring
    /// timeline frames; `None` means DSD silence (the edges of an area). Missing bytes of a
    /// short frame count as silence, and only whole sample frames that fit in `out` are
    /// written. Returns the number of clipped samples.
    pub fn convert(&mut self, prev: Option<&[u8]>, cur: &[u8], next: Option<&[u8]>, out: &mut [u8]) -> u32 {
        let mut clips = 0u32;
        for c in 0..self.channels {
            self.fill(prev, cur, next, c);
            self.p.run();
            clips = clips.saturating_add(self.p.write24(out, self.channels, c));
        }
        clips
    }

    /// Converts a run of consecutive DSD frames at once (#24): the same bytes as
    /// [`convert`](Self::convert) on each frame in turn (with its neighbours in the run, and
    /// `prev` before the first and `next` after the last), concatenated, but the neighbour
    /// margins each frame needs (up to about 270 bytes each side, some 8 % of a frame's
    /// work) are converted once per run instead of once per frame. `out` is
    /// `frames.len() × frame_bytes()` long; only whole sample frames that fit are written.
    /// Returns the number of clipped samples, and puts each frame's in `clips` (as far as it
    /// is long), as [`convert`](Self::convert) would return them.
    ///
    /// The working buffers grow with the run, about 75 KiB per frame, and are kept for the
    /// next run: bound run lengths (sacdfs uses a few frames).
    pub fn convert_run(
        &mut self,
        prev: Option<&[u8]>,
        frames: &[&[u8]],
        next: Option<&[u8]>,
        out: &mut [u8],
        clips: &mut [u32],
    ) -> u32 {
        clips.iter_mut().for_each(|k| *k = 0);
        if let [cur] = frames {
            let n = self.convert(prev, cur, next, out);
            if let Some(k) = clips.first_mut() {
                *k = n;
            }
            return n;
        }
        let ch = self.channels;
        let p = self.run.get_or_insert_with(|| Box::new(self.p.clone()));
        p.set_frames(frames.len());
        let spf = p.cfg.rate.samples_per_frame();
        let fb = spf * ch * 3;
        let mut total = 0u32;
        for c in 0..ch {
            fill_window(&mut p.bytes, p.ext.e0, ch, c, prev, frames, next);
            p.run();
            for f in 0..frames.len() {
                let Some(out) = out.get_mut(f * fb..) else { break };
                let n = p.write24_from(out, ch, c, f * spf, spf);
                total = total.saturating_add(n);
                if let Some(k) = clips.get_mut(f) {
                    *k = k.saturating_add(n);
                }
            }
        }
        total
    }

    /// As [`convert`](Self::convert), but writes interleaved f32 samples (1.0 is full
    /// scale, gain applied, not clamped): `samples_per_frame × channels` of them.
    pub fn convert_f32(&mut self, prev: Option<&[u8]>, cur: &[u8], next: Option<&[u8]>, out: &mut [f32]) {
        for c in 0..self.channels {
            self.fill(prev, cur, next, c);
            self.p.run();
            self.p.write_f32(out, self.channels, c);
        }
    }
}

/// Converts a whole area in one pass: `dsd` is its frames, each `channels × 4704`
/// interleaved bytes, with DSD silence before the first and after the last. Returns the
/// interleaved 24-bit PCM and the clipped count. Frame-by-frame [`Converter::convert`]
/// gives exactly the same bytes; this is the reference for that, and for tools. It
/// allocates buffers for the whole area (about 50 bytes per DSD byte).
pub fn convert_area(channels: u8, cfg: &PcmConfig, dsd: &[u8]) -> (Vec<u8>, u32) {
    let ch = usize::from(channels.max(1));
    let frames = dsd.len() / (ch * FRAME_BYTES);
    let mut p = Pipeline::new(cfg, frames);
    let mut out = vec![0u8; p.out_len() * ch * 3];
    let mut clips = 0u32;
    let e0 = p.ext.e0;
    for c in 0..ch {
        for (i, b) in p.bytes[e0..e0 + frames * FRAME_BYTES].iter_mut().enumerate() {
            *b = dsd[i * ch + c];
        }
        p.run();
        clips = clips.saturating_add(p.write24(&mut out, ch, c));
    }
    (out, clips)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(PcmRate::R88200.samples_per_frame(), 1176);
        assert_eq!(PcmRate::R176400.samples_per_frame(), 2352);
        assert_eq!(PcmRate::R352800.samples_per_frame(), 4704);
        let c = Converter::new(6, &PcmConfig::default());
        assert_eq!(c.frame_bytes(), 2352 * 6 * 3);
    }

    #[test]
    fn wide_needs_176_or_352() {
        let bad = PcmConfig { rate: PcmRate::R88200, cutoff: Cutoff::Wide80, gain_db: 0.0 };
        assert!(bad.validate().is_err());
        assert_eq!(bad.effective().cutoff, Cutoff::Khz40);
        assert_eq!(Converter::new(2, &bad).config().cutoff, Cutoff::Khz40);
        for rate in [PcmRate::R176400, PcmRate::R352800] {
            let ok = PcmConfig { rate, ..bad };
            assert!(ok.validate().is_ok());
            assert_eq!(Converter::new(2, &ok).config(), &ok);
        }
    }

    #[test]
    fn extents_fit_in_a_neighbour_frame() {
        for rate in [PcmRate::R88200, PcmRate::R176400, PcmRate::R352800] {
            for cutoff in [Cutoff::Khz40, Cutoff::Khz30, Cutoff::Wide80] {
                let cfg = PcmConfig { rate, cutoff, gain_db: 0.0 };
                let p = Pipeline::new(&cfg, 1);
                assert!(p.ext.e0 < FRAME_BYTES, "{cfg:?}: {}", p.ext.e0);
            }
        }
    }

    #[test]
    fn silence_is_zero() {
        for rate in [PcmRate::R88200, PcmRate::R176400, PcmRate::R352800] {
            for cutoff in [Cutoff::Khz40, Cutoff::Khz30, Cutoff::Wide80] {
                let cfg = PcmConfig { rate, cutoff, gain_db: 6.0 };
                let mut c = Converter::new(2, &cfg);
                let mut out = vec![1u8; c.frame_bytes()];
                let clips = c.convert(None, &[DSD_SILENCE; 2 * FRAME_BYTES], None, &mut out);
                assert_eq!(clips, 0);
                assert!(out.iter().all(|&b| b == 0), "{cfg:?}");
            }
        }
    }

    /// The same stages in plain f64 arithmetic, from the same coefficients, straight from
    /// the definitions: what the kernels' arithmetic costs.
    fn reference(p: &Pipeline) -> Vec<f64> {
        let f = &p.filters;
        let n1 = p.s1e.len();
        let s1 = |m: usize| -> f64 {
            (0..f.lut.len()).map(|j| f64::from(f.lut[j][usize::from(p.bytes[m + j])])).sum::<f64>()
                / f64::from(1u32 << filters::S1_SHIFT)
        };
        let s1e: Vec<f64> = (0..n1).map(|q| s1(2 * q)).collect();
        let s1o: Vec<f64> = (0..n1).map(|q| s1(2 * q + 1)).collect();
        if p.cfg.cutoff == Cutoff::Wide80 {
            let one = f64::from(1u32 << filters::S1_SHIFT);
            let h = &f.wide;
            let k = h.len() - 1;
            let at = |m: usize| if m % 2 == 0 { s1e[m / 2] } else { s1o[m / 2] };
            let y = |c: usize| -> f64 {
                one * (h[0] * at(c) + (1..=k).map(|i| h[i] * (at(c + i) + at(c - i))).sum::<f64>())
            };
            let e1 = p.ext.e1;
            let step = if p.cfg.rate == PcmRate::R352800 { 1 } else { 2 };
            return (0..p.out_len()).map(|i| y(step * i + 2 * e1)).collect();
        }
        let s2r: Vec<f64> = (0..p.s2.len())
            .map(|r| {
                0.5 * s1e[r + HB_PAIRS]
                    + (0..HB_PAIRS)
                        .map(|i| {
                            f.s2[i + 1]
                                * f64::from(1u32 << filters::S1_SHIFT)
                                * (s1o[r + HB_PAIRS + i] + s1o[r + HB_PAIRS - 1 - i])
                        })
                        .sum::<f64>()
            })
            .collect();
        let s2 = s2r;
        let k3 = f.s3_half;
        let h3 = f.s3_by_offset();
        let s3at = |c: usize| -> f64 {
            h3[0] * s2[c] + (1..=k3).map(|k| h3[k] * (s2[c + k] + s2[c - k])).sum::<f64>()
        };
        let e3 = p.ext.e3;
        match p.cfg.rate {
            PcmRate::R88200 => (0..p.out_len()).map(|q| s3at(2 * q + k3)).collect(),
            PcmRate::R176400 => (0..p.out_len()).map(|t| s3at(t + k3)).collect(),
            PcmRate::R352800 => {
                let s3: Vec<f64> = (0..p.s3.len()).map(|t| s3at(t + k3)).collect();
                (0..p.out_len())
                    .map(|i| {
                        let n = i / 2;
                        if i % 2 == 0 {
                            s3[e3 + n]
                        } else {
                            (0..HB_PAIRS).map(|j| f.s4[j] * (s3[e3 + n - j] + s3[e3 + n + 1 + j])).sum()
                        }
                    })
                    .collect()
            }
        }
    }

    #[test]
    fn arithmetic_is_far_below_the_24_bit_step() {
        let mut x = 0x1234_5678_9abc_def1u64;
        for rate in [PcmRate::R88200, PcmRate::R176400, PcmRate::R352800] {
            for cutoff in [Cutoff::Khz40, Cutoff::Khz30, Cutoff::Wide80] {
                let cfg = PcmConfig { rate, cutoff, gain_db: 0.0 };
                let mut p = Pipeline::new(&cfg, 1);
                // Random bytes: the hardest case for rounding (large intermediate values).
                for b in p.bytes.iter_mut() {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    *b = (x >> 32) as u8;
                }
                p.run();
                let r = reference(&p);
                let (mut worst, mut sq) = (0.0f64, 0.0f64);
                for (i, &v) in r.iter().enumerate() {
                    let e = p.sample(i) - v;
                    worst = worst.max(e.abs());
                    sq += e * e;
                }
                let rms = (sq / r.len() as f64).sqrt();
                eprintln!(
                    "{cfg:?}: arithmetic error worst {:.5}, rms {:.6} of a 24-bit step",
                    worst * 8_388_608.0,
                    rms * 8_388_608.0
                );
                // One 24-bit step is 2^-23 = 1.19e-7.
                assert!(worst < 0.01 / 8_388_608.0, "{cfg:?}: worst {worst:e}");
                assert!(rms < 0.001 / 8_388_608.0, "{cfg:?}: rms {rms:e}");
            }
        }
    }

    #[test]
    fn short_inputs_do_not_panic() {
        let mut c = Converter::new(2, &PcmConfig::default());
        let mut out = vec![0u8; 10];
        c.convert(Some(&[1, 2, 3]), &[], None, &mut out);
        let (pcm, _) = convert_area(2, &PcmConfig::default(), &[0u8; 100]);
        assert!(pcm.is_empty());
    }
}
