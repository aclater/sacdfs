// SPDX-License-Identifier: GPL-3.0-or-later

//! DSD to PCM conversion: DSD64 (2.8224 MHz, 1 bit) to 24-bit PCM at 88.2, 176.4 or
//! 352.8 kHz, one SACD frame (or a short run of frames) at a time, for the WAV files.
//!
//! The conversion itself is scarletbook-rs's `scarletbook-pcm` (owner decision in #84: it
//! met the targets and the 15 % leeway on 0.5.3, so sacdfs's own converter was deleted).
//! This module keeps sacdfs's settings and interface around it: the options `--pcm-rate`,
//! `--pcm-cutoff` and `--pcm-gain` map to them, and the engine's frame cache feeds it
//! frames in place and takes 24-bit bytes and per-frame clip counts back.
//!
//! **Stateless per frame.** Frame `f`'s PCM is computed from DSD frames `f - 1`, `f` and
//! `f + 1` only, and equals, bit for bit, what converting the whole area in one pass gives
//! ([`convert_area`]). That is what lets any byte range of a WAV file be produced on its
//! own.
//!
//! **Aligned.** PCM sample `n` of a frame is centred on DSD sample `d × n` of the same
//! frame, where `d` is the decimation (32, 16 or 8).
//!
//! **Silence.** DSD silence (every byte 0x69) converts to exact zero, and a missing
//! neighbour (the edge of an area) counts as DSD silence, so an area that starts or ends
//! in silence starts or ends in digital silence.
//!
//! **The same on every machine.** scarletbook-pcm's arithmetic is integer arithmetic, its
//! code chosen at run time (AVX2 where the CPU has it), with the same bytes on every path.
//! The measured quality is in `docs/pcm-quality.md`.

use scarletbook_pcm as sb;

/// DSD bytes per channel in one SACD frame (1/75 s).
pub const FRAME_BYTES: usize = 4704;

/// The DSD silence pattern (01101001), used for the frames before the first and after the
/// last frame of an area. It converts to exact zero.
pub const DSD_SILENCE: u8 = 0x69;

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

/// The top of the pass band. Each is scarletbook-pcm's cutoff of the same band
/// (`Default`, `Low`, `Wide`).
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

/// scarletbook-pcm's configuration for `cfg` (made [`effective`](PcmConfig::effective)) and
/// `channels` (clamped to 1 to 6). A gain that is not finite counts as 0 dB; one outside
/// scarletbook-pcm's -120 to +24 dB (sacdfs's options allow -60 to +24) is clamped.
fn sb_config(channels: u8, cfg: &PcmConfig) -> sb::PcmConfig {
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
    let gain = sb::Gain::from_db(db.clamp(sb::Gain::MIN_DB, sb::Gain::MAX_DB)).unwrap_or(sb::Gain::UNITY);
    sb::PcmConfig::new(rate, cutoff, channels.clamp(1, sb::MAX_CHANNELS)).with_gain(gain)
}

/// Converts DSD frames to PCM, one frame or one short run at a time. Make one per worker
/// thread: it holds scarletbook-pcm's converter and a few small buffers, so converting
/// allocates nothing after the first call.
#[derive(Debug)]
pub struct Converter {
    channels: usize,
    cfg: PcmConfig,
    conv: sb::Converter,
    /// A short frame padded with silence (never for frames from the decoder, which are whole).
    input: Vec<u8>,
    pad: [Vec<u8>; 2],
    /// Output that does not fit the caller's buffer, and per-frame clip counts.
    spill: Vec<u8>,
    counts: Vec<u64>,
}

impl Converter {
    /// A converter for frames of `channels` channels (0 counts as 1). It never fails: a
    /// configuration that [`PcmConfig::validate`] refuses (the wide filter at 88.2 kHz) is
    /// converted with [`PcmConfig::effective`] instead, which [`config`](Self::config)
    /// reports.
    pub fn new(channels: u8, cfg: &PcmConfig) -> Converter {
        let sc = sb_config(channels.max(1), cfg);
        // `sb_config` only makes configurations scarletbook-pcm accepts.
        #[allow(clippy::expect_used)]
        let conv = sb::Converter::new(sc).expect("a valid scarletbook-pcm configuration");
        Converter {
            channels: usize::from(sc.channels),
            cfg: cfg.effective(),
            conv,
            input: Vec::new(),
            pad: [Vec::new(), Vec::new()],
            spill: Vec::new(),
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

    /// A neighbour of exactly one frame: itself, or padded with silence into `pad`.
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

    /// Converts DSD frame `cur` (`channels × 4704` interleaved MSB-first bytes, as
    /// scarletbook's `AudioFrame.data`) into interleaved 24-bit little-endian PCM in `out`
    /// ([`frame_bytes`](Self::frame_bytes) long). `prev` and `next` are the neighbouring
    /// timeline frames; `None` means DSD silence (the edges of an area). Missing bytes of a
    /// short frame count as silence, and only whole sample frames that fit in `out` are
    /// written. Returns the number of clipped samples.
    pub fn convert(&mut self, prev: Option<&[u8]>, cur: &[u8], next: Option<&[u8]>, out: &mut [u8]) -> u32 {
        let mut k = [0u32];
        self.convert_run(prev, &[cur], next, out, &mut k)
    }

    /// Converts a run of consecutive DSD frames at once: the same bytes as
    /// [`convert`](Self::convert) on each frame in turn (with its neighbours in the run, and
    /// `prev` before the first and `next` after the last), concatenated. The frames are read
    /// in place. `out` is `frames.len() × frame_bytes()` long; only whole sample frames that
    /// fit are written. Returns the number of clipped samples, and puts each frame's in
    /// `clips` (as far as it is long), as [`convert`](Self::convert) would return them.
    pub fn convert_run(
        &mut self,
        prev: Option<&[u8]>,
        frames: &[&[u8]],
        next: Option<&[u8]>,
        out: &mut [u8],
        clips: &mut [u32],
    ) -> u32 {
        clips.iter_mut().for_each(|k| *k = 0);
        if frames.is_empty() {
            return 0;
        }
        let whole = self.channels * FRAME_BYTES;
        let need = frames.len() * self.frame_bytes();
        self.counts.clear();
        self.counts.resize(frames.len(), 0);
        let [pa, pb] = &mut self.pad;
        let (before, after) = (Self::neighbour(whole, prev, pa), Self::neighbour(whole, next, pb));
        // Straight into `out` when it holds the whole run, else through `spill`.
        let direct = out.len() >= need;
        if !direct {
            self.spill.clear();
            self.spill.resize(need, 0);
        }
        let dst: &mut [u8] = if direct { &mut out[..need] } else { &mut self.spill };
        let r = if frames.iter().all(|f| f.len() == whole) {
            let run = sb::Run::new(frames).with_before(before).with_after(after);
            self.conv.convert(run, sb::PcmOut::S24le(dst), Some(&mut self.counts))
        } else {
            self.input.clear();
            for f in frames {
                let n = f.len().min(whole);
                self.input.extend_from_slice(&f[..n]);
                self.input.resize(self.input.len() + whole - n, DSD_SILENCE);
            }
            let run = sb::Run::new(self.input.as_slice()).with_before(before).with_after(after);
            self.conv.convert(run, sb::PcmOut::S24le(dst), Some(&mut self.counts))
        };
        // The lengths are right by construction, so this does not fail; if it ever did, the
        // output stays as it was and nothing is counted.
        if r.is_err() {
            return 0;
        }
        if !direct {
            let block = self.channels * 3;
            let fit = out.len() / block * block;
            out[..fit].copy_from_slice(&self.spill[..fit]);
        }
        let mut total = 0u32;
        for (i, &c) in self.counts.iter().enumerate() {
            let c = u32::try_from(c).unwrap_or(u32::MAX);
            total = total.saturating_add(c);
            if let Some(k) = clips.get_mut(i) {
                *k = c;
            }
        }
        total
    }

    /// As [`convert`](Self::convert), but writes the samples as f32 (1.0 is full scale):
    /// the 24-bit samples divided by 2^23, `samples_per_frame × channels` of them. For tools
    /// and tests.
    pub fn convert_f32(&mut self, prev: Option<&[u8]>, cur: &[u8], next: Option<&[u8]>, out: &mut [f32]) {
        let mut bytes = std::mem::take(&mut self.spill);
        bytes.clear();
        bytes.resize(self.frame_bytes(), 0);
        self.convert(prev, cur, next, &mut bytes);
        for (o, s) in out.iter_mut().zip(bytes.chunks_exact(3)) {
            *o = (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0;
        }
        self.spill = bytes;
    }
}

/// Converts a whole area in one pass: `dsd` is its frames, each `channels × 4704`
/// interleaved bytes (a short tail is ignored), with DSD silence before the first and after
/// the last. Returns the interleaved 24-bit PCM and the clipped count. Frame-by-frame
/// [`Converter::convert`] gives exactly the same bytes; this is the reference for that, and
/// for tools.
pub fn convert_area(channels: u8, cfg: &PcmConfig, dsd: &[u8]) -> (Vec<u8>, u32) {
    let mut c = Converter::new(channels, cfg);
    let whole = c.channels * FRAME_BYTES;
    let frames = dsd.len() / whole;
    let mut out = vec![0u8; frames * c.frame_bytes()];
    if frames == 0 {
        return (out, 0);
    }
    let run = sb::Run::new(&dsd[..frames * whole]);
    match c.conv.convert(run, sb::PcmOut::S24le(&mut out), None) {
        Ok(stats) => (out, u32::try_from(stats.clipped_total()).unwrap_or(u32::MAX)),
        Err(_) => (out, 0),
    }
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
    fn options_map_to_scarletbook() {
        // The band of each cutoff (sacdfs's targets, #5): stop from 48 kHz, 36 kHz, and for
        // the wide filter from Nyquist at 176.4 kHz and about 100 kHz at 352.8 kHz.
        let at = |rate, cutoff| {
            let s = sb_config(2, &PcmConfig { rate, cutoff, gain_db: 0.0 });
            (s.cutoff.passband_hz(), s.cutoff.stopband_hz_at(s.rate))
        };
        assert_eq!(at(PcmRate::R176400, Cutoff::Khz40), (40_000, 48_000));
        assert_eq!(at(PcmRate::R176400, Cutoff::Khz30), (30_000, 36_000));
        assert_eq!(at(PcmRate::R176400, Cutoff::Wide80), (80_000, 88_200));
        assert!(at(PcmRate::R352800, Cutoff::Wide80).1 <= 100_000);
        // Gains: not finite counts as 0 dB.
        let g = sb_config(2, &PcmConfig { gain_db: f32::NAN, ..PcmConfig::default() }).gain;
        assert_eq!(g.db(), 0.0);
    }

    #[test]
    fn silence_is_zero() {
        for rate in [PcmRate::R88200, PcmRate::R176400, PcmRate::R352800] {
            for cutoff in [Cutoff::Khz40, Cutoff::Khz30, Cutoff::Wide80] {
                let cfg = PcmConfig { rate, cutoff, gain_db: 6.0 };
                let mut c = Converter::new(2, &cfg);
                let mut out = vec![1u8; c.frame_bytes()];
                // No neighbours: the edges of an area.
                let clips = c.convert(None, &[DSD_SILENCE; 2 * FRAME_BYTES], None, &mut out);
                assert_eq!(clips, 0);
                assert!(out.iter().all(|&b| b == 0), "{cfg:?}");
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

    #[test]
    fn short_output_gets_whole_sample_frames() {
        let cfg = PcmConfig::default();
        let mut c = Converter::new(2, &cfg);
        let cur = [0x5au8; 2 * FRAME_BYTES];
        let mut full = vec![0u8; c.frame_bytes()];
        c.convert(None, &cur, None, &mut full);
        let mut part = vec![7u8; 100];
        c.convert(None, &cur, None, &mut part);
        assert_eq!(part[..96], full[..96]);
        assert_eq!(part[96..], [7; 4]);
    }
}
