// SPDX-License-Identifier: GPL-3.0-or-later

//! TEMPORARY, for the comparison in "PCM: scarletbook-pcm against our converter (0.5.2)":
//! scarletbook-pcm behind the same interface as [`super::Converter`], and a switch between
//! the two. Not a decision: the default stays sacdfs's own converter, and this module goes
//! once the owner has decided.
//!
//! The switch is the environment variable `SACDFS_PCM_ENGINE`: `scarletbook` selects
//! scarletbook-pcm, anything else (or unset) sacdfs's converter. It is read once per
//! process.
//!
//! What the adapter has to add, because scarletbook-pcm does not offer it (the gaps):
//! - frames of a run given as separate slices: scarletbook-pcm takes one contiguous run,
//!   so they are copied together first;
//! - clip counts per frame of a run: scarletbook-pcm counts per channel per call, so they
//!   are counted here from the output (samples at either rail);
//! - packed 24-bit output: scarletbook-pcm's Rust API writes `i32`, packed afterwards with
//!   its `pack_s24le` (a second pass over the output);
//! - short frames: scarletbook-pcm refuses them; they are padded with DSD silence (0x69)
//!   here, as sacdfs's converter treats them;
//! - f32 output: not offered; `convert_f32` gives the 24-bit samples scaled.

use std::sync::OnceLock;

use scarletbook_pcm as sb;

use super::{Converter, Cutoff, DSD_SILENCE, FRAME_BYTES, PcmConfig, PcmRate};

/// Which converter the engine uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// sacdfs's own (`sacdfs::pcm`), the default.
    Sacdfs,
    /// scarletbook-pcm.
    Scarletbook,
}

/// The converter selected by `SACDFS_PCM_ENGINE` (read once).
pub fn selected() -> Engine {
    static SEL: OnceLock<Engine> = OnceLock::new();
    *SEL.get_or_init(|| match std::env::var("SACDFS_PCM_ENGINE").as_deref() {
        Ok("scarletbook") => {
            log::warn!("PCM: using scarletbook-pcm (SACDFS_PCM_ENGINE=scarletbook), for comparison only");
            Engine::Scarletbook
        }
        _ => Engine::Sacdfs,
    })
}

/// scarletbook-pcm's configuration for sacdfs's: the same rate, the cutoff of the same
/// name (40 kHz Default, 30 kHz Low, wide Wide; note that scarletbook's Low stops at
/// 38 kHz, not 36, and its Wide at 96 kHz, not 88.2/100), the same gain.
pub fn sb_config(channels: u8, cfg: &PcmConfig) -> sb::PcmConfig {
    let cfg = cfg.effective();
    let rate = match cfg.rate {
        PcmRate::R88200 => sb::PcmRate::Hz88200,
        PcmRate::R176400 => sb::PcmRate::Hz176400,
        PcmRate::R352800 => sb::PcmRate::Hz352800,
    };
    let cutoff = match cfg.cutoff {
        Cutoff::Khz40 => sb::Cutoff::Default,
        Cutoff::Khz30 => sb::Cutoff::Low,
        Cutoff::Wide80 => sb::Cutoff::Wide,
    };
    let db = if cfg.gain_db.is_finite() { f64::from(cfg.gain_db) } else { 0.0 };
    let gain = sb::Gain::from_db(db).unwrap_or(sb::Gain::UNITY);
    sb::PcmConfig::new(rate, cutoff, channels.clamp(1, sb::MAX_CHANNELS)).with_gain(gain)
}

/// scarletbook-pcm with [`super::Converter`]'s interface.
#[derive(Debug)]
pub struct SbConverter {
    channels: usize,
    cfg: PcmConfig,
    conv: sb::Converter,
    /// Input frames copied together (and padded), output samples before packing.
    input: Vec<u8>,
    pad: [Vec<u8>; 2],
    pcm: Vec<i32>,
}

/// The largest and smallest 24-bit sample: where clamped samples land.
const RAILS: [i32; 2] = [8_388_607, -8_388_608];

impl SbConverter {
    /// As [`Converter::new`].
    pub fn new(channels: u8, cfg: &PcmConfig) -> SbConverter {
        let sc = sb_config(channels, cfg);
        #[allow(clippy::expect_used)]
        let conv = sb::Converter::new(sc).expect("a valid scarletbook-pcm configuration");
        SbConverter {
            channels: usize::from(sc.channels),
            cfg: cfg.effective(),
            conv,
            input: Vec::new(),
            pad: [Vec::new(), Vec::new()],
            pcm: Vec::new(),
        }
    }

    /// The configuration.
    pub fn config(&self) -> &PcmConfig {
        &self.cfg
    }

    /// Bytes of one converted frame: `samples_per_frame × channels × 3`.
    pub fn frame_bytes(&self) -> usize {
        self.conv.samples_per_frame() * self.channels * 3
    }

    fn whole(&self) -> usize {
        self.channels * FRAME_BYTES
    }

    /// A neighbour of the right length: itself, or padded with silence into `pad[i]`.
    fn neighbour<'a>(whole: usize, f: Option<&'a [u8]>, pad: &'a mut Vec<u8>) -> Option<&'a [u8]> {
        let f = f?;
        if f.len() == whole {
            return Some(f);
        }
        pad.clear();
        pad.extend_from_slice(&f[..f.len().min(whole)]);
        pad.resize(whole, DSD_SILENCE);
        Some(pad)
    }

    /// As [`Converter::convert_run`]; [`Converter::convert`] is a run of one.
    pub fn convert_run(
        &mut self,
        prev: Option<&[u8]>,
        frames: &[&[u8]],
        next: Option<&[u8]>,
        out: &mut [u8],
        clips: &mut [u32],
    ) -> u32 {
        clips.iter_mut().for_each(|k| *k = 0);
        let whole = self.whole();
        self.input.clear();
        for f in frames {
            let n = f.len().min(whole);
            self.input.extend_from_slice(&f[..n]);
            self.input.resize(self.input.len() + whole - n, DSD_SILENCE);
        }
        let spf = self.conv.samples_per_frame() * self.channels;
        self.pcm.resize(frames.len() * spf, 0);
        let [pa, pb] = &mut self.pad;
        let run = sb::FrameRun::new(&self.input)
            .with_before(Self::neighbour(whole, prev, pa))
            .with_after(Self::neighbour(whole, next, pb));
        if self.conv.convert_run(run, &mut self.pcm).is_err() {
            return 0;
        }
        let mut total = 0u32;
        for (f, samples) in self.pcm.chunks_exact(spf).enumerate() {
            let n = samples.iter().filter(|s| RAILS.contains(s)).count() as u32;
            total = total.saturating_add(n);
            if let Some(k) = clips.get_mut(f) {
                *k = n;
            }
            if let Some(o) = out.get_mut(f * spf * 3..) {
                sb::pack_s24le(samples, o);
            }
        }
        total
    }

    /// As [`Converter::convert`].
    pub fn convert(&mut self, prev: Option<&[u8]>, cur: &[u8], next: Option<&[u8]>, out: &mut [u8]) -> u32 {
        let mut k = [0u32];
        self.convert_run(prev, &[cur], next, out, &mut k)
    }

    /// As [`Converter::convert_f32`], but from the 24-bit samples (scarletbook-pcm has no
    /// float output): `sample / 2^23`.
    pub fn convert_f32(&mut self, prev: Option<&[u8]>, cur: &[u8], next: Option<&[u8]>, out: &mut [f32]) {
        let mut bytes = vec![0u8; self.frame_bytes()];
        self.convert(prev, cur, next, &mut bytes);
        for (o, s) in out.iter_mut().zip(bytes.chunks_exact(3)) {
            let v = i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8;
            *o = v as f32 / 8_388_608.0;
        }
    }
}

/// Either converter, as the engine holds it.
#[derive(Debug)]
// One per thread, long-lived: the size difference does not matter.
#[allow(clippy::large_enum_variant)]
pub enum AnyConverter {
    /// sacdfs's.
    Sacdfs(Converter),
    /// scarletbook-pcm's.
    Scarletbook(Box<SbConverter>),
}

impl AnyConverter {
    /// The converter `engine` selects.
    pub fn new(engine: Engine, channels: u8, cfg: &PcmConfig) -> AnyConverter {
        match engine {
            Engine::Sacdfs => AnyConverter::Sacdfs(Converter::new(channels, cfg)),
            Engine::Scarletbook => AnyConverter::Scarletbook(Box::new(SbConverter::new(channels, cfg))),
        }
    }

    /// As [`Converter::frame_bytes`].
    pub fn frame_bytes(&self) -> usize {
        match self {
            AnyConverter::Sacdfs(c) => c.frame_bytes(),
            AnyConverter::Scarletbook(c) => c.frame_bytes(),
        }
    }

    /// As [`Converter::convert`].
    pub fn convert(&mut self, prev: Option<&[u8]>, cur: &[u8], next: Option<&[u8]>, out: &mut [u8]) -> u32 {
        match self {
            AnyConverter::Sacdfs(c) => c.convert(prev, cur, next, out),
            AnyConverter::Scarletbook(c) => c.convert(prev, cur, next, out),
        }
    }

    /// As [`Converter::convert_run`].
    pub fn convert_run(
        &mut self,
        prev: Option<&[u8]>,
        frames: &[&[u8]],
        next: Option<&[u8]>,
        out: &mut [u8],
        clips: &mut [u32],
    ) -> u32 {
        match self {
            AnyConverter::Sacdfs(c) => c.convert_run(prev, frames, next, out, clips),
            AnyConverter::Scarletbook(c) => c.convert_run(prev, frames, next, out, clips),
        }
    }
}
