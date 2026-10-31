// SPDX-License-Identifier: GPL-3.0-or-later

//! TEMPORARY STUB of the DSD-to-PCM converter (phase 1 of #11 replaces this file).
//!
//! It has exactly the public API of the real converter, so that the WAV files can be built
//! and tested against it. Its "conversion" is deliberately simple but real enough to test
//! against: each output sample is the mean of the DSD bits of its own bytes plus one byte on
//! either side, taken from the neighbouring frames at the frame edges (DSD silence, 0x69,
//! where there is no neighbour). It is not a usable audio filter.

/// DSD bytes per channel in one SACD frame.
const FRAME_BYTES: usize = 4704;

/// DSD silence (the idle pattern): as many ones as zeros.
const SILENCE: u8 = 0x69;

/// Output sample rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PcmRate {
    /// 88.2 kHz.
    R88200,
    /// 176.4 kHz (the default).
    #[default]
    R176400,
    /// 352.8 kHz.
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

    /// Output samples per channel in one SACD frame.
    pub fn samples_per_frame(self) -> usize {
        match self {
            PcmRate::R88200 => 1176,
            PcmRate::R176400 => 2352,
            PcmRate::R352800 => 4704,
        }
    }
}

/// Passband edge of the low-pass filter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cutoff {
    /// 40 kHz (the default).
    #[default]
    Khz40,
    /// 30 kHz.
    Khz30,
}

/// Conversion settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PcmConfig {
    /// Output rate.
    pub rate: PcmRate,
    /// Filter cutoff.
    pub cutoff: Cutoff,
    /// Gain in dB.
    pub gain_db: f32,
}

impl Default for PcmConfig {
    fn default() -> Self {
        PcmConfig { rate: PcmRate::R176400, cutoff: Cutoff::Khz40, gain_db: 0.0 }
    }
}

/// A converter for one channel count; one per worker thread.
#[derive(Debug)]
pub struct Converter {
    channels: usize,
    spf: usize,
    gain: f64,
}

impl Converter {
    /// A converter of `channels` channels with `cfg`.
    pub fn new(channels: u8, cfg: &PcmConfig) -> Converter {
        let gain = 10f64.powf(f64::from(cfg.gain_db) / 20.0);
        Converter { channels: usize::from(channels.max(1)), spf: cfg.rate.samples_per_frame(), gain }
    }

    /// Output bytes of one frame: samples per frame x channels x 3.
    pub fn frame_bytes(&self) -> usize {
        self.spf * self.channels * 3
    }

    /// Converts frame `cur` (`C x 4704` interleaved MSB-first DSD bytes), with its
    /// neighbouring timeline frames `prev` and `next` (`None`: DSD silence), into
    /// interleaved 24-bit little-endian PCM in `out`. Returns the number of clipped
    /// samples.
    pub fn convert(&mut self, prev: Option<&[u8]>, cur: &[u8], next: Option<&[u8]>, out: &mut [u8]) -> u32 {
        let c = self.channels;
        let per = FRAME_BYTES / self.spf; // DSD bytes per output sample: 4, 2 or 1
        let byte = |ch: usize, i: isize| -> u8 {
            let at = |f: &[u8], j: usize| f.get(j * c + ch).copied().unwrap_or(SILENCE);
            if i < 0 {
                prev.map_or(SILENCE, |p| at(p, (FRAME_BYTES as isize + i) as usize))
            } else if i as usize >= FRAME_BYTES {
                next.map_or(SILENCE, |n| at(n, i as usize - FRAME_BYTES))
            } else {
                at(cur, i as usize)
            }
        };
        let mut clips = 0;
        let full = f64::from(1u32 << 23);
        for s in 0..self.spf {
            for ch in 0..c {
                let lo = (s * per) as isize - 1;
                let hi = ((s + 1) * per) as isize + 1;
                let ones: u32 = (lo..hi).map(|i| byte(ch, i).count_ones()).sum();
                let bits = ((hi - lo) * 8) as f64;
                let v = (2.0 * f64::from(ones) / bits - 1.0) * (full - 1.0) * self.gain;
                let q = v.round();
                let q = if q > full - 1.0 {
                    clips += 1;
                    full - 1.0
                } else if q < -full {
                    clips += 1;
                    -full
                } else {
                    q
                };
                let at = (s * c + ch) * 3;
                if let Some(o) = out.get_mut(at..at + 3) {
                    o.copy_from_slice(&(q as i32).to_le_bytes()[..3]);
                }
            }
        }
        clips
    }
}
