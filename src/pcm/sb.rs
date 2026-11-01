// SPDX-License-Identifier: GPL-3.0-or-later

//! TEMPORARY, for the comparison in "PCM: scarletbook-pcm against our converter" (#84):
//! scarletbook-pcm behind the same interface as [`super::Converter`], and a switch between
//! the two. Not a decision: the default stays sacdfs's own converter, and this module goes
//! once the owner has decided.
//!
//! The switch is the environment variable `SACDFS_PCM_ENGINE`: `scarletbook` selects
//! scarletbook-pcm, anything else (or unset) sacdfs's converter. It is read once per
//! process.
//!
//! With 0.5.3 (scarletbook-rs#172) the adapter reads a run's frames in place
//! (`Frames::Separate`), writes 24-bit bytes directly (`PcmOut::S24le`) and takes clip
//! counts per frame from the library. It still pads a short frame with DSD silence (the
//! library refuses one; the decoder never makes one), and `convert_f32` gives the 24-bit
//! samples scaled (no float output, needed only by tools).

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
/// name (40 kHz Default, 30 kHz Low, wide Wide), the same gain.
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
    counts: Vec<u64>,
}

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
            counts: Vec::new(),
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

    /// As [`Converter::convert_run`]; [`Converter::convert`] is a run of one. The frames
    /// are read in place (`Frames::Separate`), the output is written as 24-bit bytes
    /// (`PcmOut::S24le`) and the clip counts come per frame from scarletbook-pcm; only a
    /// short frame (never from the decoder) is copied, padded with DSD silence.
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
        let fb = self.frame_bytes();
        let n = frames.len();
        let Some(out) = out.get_mut(..n * fb) else { return 0 };
        self.counts.clear();
        self.counts.resize(n, 0);
        let [pa, pb] = &mut self.pad;
        let (before, after) = (Self::neighbour(whole, prev, pa), Self::neighbour(whole, next, pb));
        let r = if frames.iter().all(|f| f.len() == whole) {
            let run = sb::Run::new(frames).with_before(before).with_after(after);
            self.conv.convert(run, sb::PcmOut::S24le(out), Some(&mut self.counts))
        } else {
            self.input.clear();
            for f in frames {
                let k = f.len().min(whole);
                self.input.extend_from_slice(&f[..k]);
                self.input.resize(self.input.len() + whole - k, DSD_SILENCE);
            }
            let run = sb::Run::new(self.input.as_slice()).with_before(before).with_after(after);
            self.conv.convert(run, sb::PcmOut::S24le(out), Some(&mut self.counts))
        };
        if r.is_err() {
            return 0;
        }
        let mut total = 0u32;
        for (k, &c) in clips.iter_mut().zip(&self.counts) {
            *k = u32::try_from(c).unwrap_or(u32::MAX);
        }
        for &c in &self.counts {
            total = total.saturating_add(u32::try_from(c).unwrap_or(u32::MAX));
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
