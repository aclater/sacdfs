// SPDX-License-Identifier: GPL-3.0-or-later

//! Audio for an AcoustID fingerprint (#296): up to [`MAX_SECONDS`] of a track of the stereo area as
//! 16-bit mono PCM at [`RATE`] Hz, streamed to Chromaprint's `fpcalc` program on its standard input.
//!
//! **Streaming, bounded.** One frame of DSD is decoded at a time, by a DST decoder of its own,
//! converted to 88.2 kHz PCM, mixed to mono and decimated to 11,025 Hz, and written out a second
//! at a time. Nothing goes through the mount's frame cache, and the memory held does not grow
//! with the track: a few frames of DSD and PCM, the filter, and a second of output (see
//! tests/acoustid.rs for the ceiling's test).
//!
//! **Why `fpcalc` as a program** (docs/design.md, "AcoustID"): Chromaprint is C. Linking it would
//! give the release's binaries a native dependency against their old-glibc baseline; a Rust port
//! of it doesn't exist as a maintained crate. `fpcalc` is packaged by Fedora (`chromaprint-tools`)
//! and the other distributions, and the container image carries it. Without it, only this
//! lookup is unavailable.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use scarletbook::{DstDecoder, TrackFrame};

use crate::disc::Disc;
use crate::pcm::{Converter, Cutoff, PcmConfig, PcmRate};

/// The most audio fingerprinted of a track, in seconds: as `fpcalc`'s own default, and what
/// AcoustID's fingerprints are made of.
pub const MAX_SECONDS: u32 = 120;
/// The rate of the PCM given to `fpcalc`: Chromaprint's own working rate, so it resamples nothing.
pub const RATE: u32 = 11_025;
/// SACD frames per second.
const FRAMES_PER_SECOND: u32 = 75;
/// 88.2 kHz to 11,025 Hz.
const DECIMATION: usize = 8;
/// Taps of the decimation filter.
const TAPS: usize = 127;
/// The filter's cutoff, in Hz: under the 5,512.5 Hz the output can hold. Chromaprint's own
/// analysis stops at about 5 kHz.
const CUTOFF_HZ: f64 = 4_800.0;
/// Longest wait for `fpcalc` with one track.
pub const FPCALC_TIMEOUT: Duration = Duration::from_secs(120);
/// Largest output of `fpcalc` read.
const MAX_FPCALC_OUTPUT: u64 = 1 << 20;
/// Longest fingerprint taken, in characters: two minutes are about 2,500.
const MAX_FINGERPRINT: usize = 64 << 10;

/// A low-pass FIR filter and decimator by 8, on mono samples.
#[derive(Debug)]
struct Decimator {
    taps: Vec<f32>,
    ring: Vec<f32>,
    at: usize,
    phase: usize,
}

impl Decimator {
    fn new() -> Decimator {
        let fs = f64::from(RATE) * DECIMATION as f64;
        let fc = CUTOFF_HZ / fs;
        let m = (TAPS - 1) as f64;
        let mut taps: Vec<f64> = (0..TAPS)
            .map(|i| {
                let x = i as f64 - m / 2.0;
                let sinc = if x == 0.0 {
                    2.0 * fc
                } else {
                    (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
                };
                let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / m).cos()
                    + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / m).cos();
                sinc * w
            })
            .collect();
        let sum: f64 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        Decimator {
            taps: taps.into_iter().map(|t| t as f32).collect(),
            ring: vec![0.0; TAPS],
            at: 0,
            phase: 0,
        }
    }

    /// Takes one input sample; every 8th gives an output sample.
    fn push(&mut self, x: f32) -> Option<f32> {
        self.ring[self.at] = x;
        self.at = (self.at + 1) % TAPS;
        self.phase += 1;
        if self.phase < DECIMATION {
            return None;
        }
        self.phase = 0;
        // The oldest sample is at `at`.
        let (newer, older) = self.ring.split_at(self.at);
        let acc: f32 = older.iter().chain(newer).zip(&self.taps).map(|(s, t)| s * t).sum();
        Some(acc)
    }
}

/// How much was fed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fed {
    /// SACD frames decoded.
    pub frames: u32,
    /// PCM bytes written.
    pub bytes: u64,
}

/// Streams the first [`MAX_SECONDS`] (or less) of the track at `start` (a position of area `area`'s
/// timeline) lasting `duration` frames to `out`, as 16-bit little-endian mono PCM at [`RATE`] Hz.
/// `stop` is asked between seconds; when it says so, the stream ends early with an error of kind
/// `Interrupted`.
pub fn stream_track(
    disc: &Disc,
    area: usize,
    start: u32,
    duration: u32,
    out: &mut dyn Write,
    stop: &dyn Fn() -> bool,
) -> io::Result<Fed> {
    let n = duration.min(MAX_SECONDS * FRAMES_PER_SECOND);
    if n == 0 {
        return Ok(Fed::default());
    }
    let mut cursor = disc.locate_for(area, start, start.saturating_add(n)).map_err(io::Error::other)?;
    let channels = cursor.channels();
    if !(1..=2).contains(&channels) {
        return Err(io::Error::other(format!("{channels} channels: only a stereo area is fingerprinted")));
    }
    let cfg = PcmConfig { rate: PcmRate::R88200, cutoff: Cutoff::Khz30, gain_db: 0.0 };
    let mut conv = Converter::new(channels, &cfg);
    let mut decoder = DstDecoder::default();
    let mut decimator = Decimator::new();
    let mut pcm = vec![0u8; conv.frame_bytes()];
    let per_frame = PcmRate::R88200.samples_per_frame() / DECIMATION;
    let mut second: Vec<u8> = Vec::with_capacity(per_frame * 2 * FRAMES_PER_SECOND as usize);
    let mut fed = Fed::default();
    let mut read = |t: u32| -> io::Result<Vec<u8>> {
        let content = cursor.content(disc, t).map_err(io::Error::other)?;
        let frame = scarletbook::audio::decode_frame(&mut decoder, TrackFrame::new(t, content), channels)
            .map_err(io::Error::other)?;
        Ok(frame.data)
    };
    let mut prev: Option<Vec<u8>> = None;
    let mut cur = read(start)?;
    for k in 0..n {
        let next = if k + 1 < n { Some(read(start + k + 1)?) } else { None };
        conv.try_convert(prev.as_deref(), &cur, next.as_deref(), &mut pcm).map_err(io::Error::other)?;
        let ch = usize::from(channels);
        for s in pcm.chunks_exact(3 * ch) {
            let mut sum = 0i32;
            for c in s.chunks_exact(3) {
                sum += i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8;
            }
            let mono = sum as f32 / (ch as f32 * 8_388_608.0);
            if let Some(y) = decimator.push(mono) {
                let v = (y * 32_768.0).round().clamp(-32_768.0, 32_767.0) as i16;
                second.extend_from_slice(&v.to_le_bytes());
            }
        }
        fed.frames += 1;
        if (k + 1) % FRAMES_PER_SECOND == 0 || k + 1 == n {
            out.write_all(&second)?;
            fed.bytes += second.len() as u64;
            second.clear();
            if k + 1 < n && stop() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
            }
        }
        prev = Some(cur);
        cur = match next {
            Some(f) => f,
            None => break,
        };
    }
    Ok(fed)
}

/// Runs `fpcalc` (at `program`) on what `feed` writes to it: 16-bit mono PCM at [`RATE`] Hz. The
/// fingerprint, in AcoustID's compressed form. `fpcalc` is killed after `timeout`.
pub fn fpcalc(
    program: &Path,
    timeout: Duration,
    feed: impl FnOnce(&mut dyn Write) -> io::Result<Fed>,
) -> Result<(String, Fed), String> {
    let rate = RATE.to_string();
    let length = MAX_SECONDS.to_string();
    let mut child = Command::new(program)
        .args(["-format", "s16le", "-rate", &rate, "-channels", "1", "-length", &length, "-json", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("fpcalc: no pipes".to_owned());
    };
    let child: Arc<Mutex<Child>> = Arc::new(Mutex::new(child));
    let (done, wait) = mpsc::channel::<()>();
    let timer = {
        let child = child.clone();
        thread::Builder::new()
            .name("sacdfs-fpcalc-timer".to_owned())
            .spawn(move || {
                if let Err(mpsc::RecvTimeoutError::Timeout) = wait.recv_timeout(timeout) {
                    let _ = child.lock().unwrap_or_else(|p| p.into_inner()).kill();
                    true
                } else {
                    false
                }
            })
            .map_err(|e| e.to_string())?
    };
    // Its output is read after the input is written: fpcalc writes only at the end, a few KiB.
    let reader = thread::Builder::new()
        .name("sacdfs-fpcalc-out".to_owned())
        .spawn(move || {
            let mut out = Vec::new();
            stdout.take(MAX_FPCALC_OUTPUT).read_to_end(&mut out).map(|_| out)
        })
        .map_err(|e| e.to_string())?;
    let fed = feed(&mut stdin);
    drop(stdin);
    let out = reader.join().unwrap_or_else(|_| Err(io::Error::other("reader panicked")));
    let _ = done.send(());
    let killed = timer.join().unwrap_or(false);
    let status = child.lock().unwrap_or_else(|p| p.into_inner()).wait();
    if killed {
        return Err(format!("fpcalc took over {} s and was stopped", timeout.as_secs()));
    }
    let fed = match fed {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::Interrupted => return Err("stopped".to_owned()),
        Err(e) => return Err(format!("the audio couldn't be given to fpcalc: {e}")),
    };
    let status = status.map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("fpcalc failed ({status})"));
    }
    let out = out.map_err(|e| e.to_string())?;
    Ok((parse_fpcalc(&out)?, fed))
}

/// The fingerprint in `fpcalc -json`'s output, if it is one: base64 characters only.
pub fn parse_fpcalc(out: &[u8]) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_slice(out).map_err(|e| format!("fpcalc's output is not JSON: {e}"))?;
    let fp = v["fingerprint"].as_str().unwrap_or("");
    if fp.is_empty() {
        return Err("fpcalc gave no fingerprint (too little audio?)".to_owned());
    }
    if fp.len() > MAX_FINGERPRINT
        || !fp.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'+' | b'/' | b'='))
    {
        return Err("fpcalc gave a malformed fingerprint".to_owned());
    }
    Ok(fp.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// The filter passes the band Chromaprint uses and stops what would fold into it.
    #[test]
    fn the_decimator_passes_low_tones_and_stops_high_ones() {
        let level = |hz: f64| {
            let mut d = Decimator::new();
            let fs = f64::from(RATE) * DECIMATION as f64;
            let mut peak = 0.0f32;
            for i in 0..(fs as usize / 4) {
                let x = (2.0 * std::f64::consts::PI * hz * i as f64 / fs).sin() as f32;
                if let Some(y) = d.push(x) {
                    if i > TAPS * 2 {
                        peak = peak.max(y.abs());
                    }
                }
            }
            peak
        };
        assert!(level(1_000.0) > 0.95, "{}", level(1_000.0));
        assert!(level(3_000.0) > 0.9, "{}", level(3_000.0));
        assert!(level(8_000.0) < 0.01, "{}", level(8_000.0));
        assert!(level(20_000.0) < 0.001, "{}", level(20_000.0));
    }

    #[test]
    fn a_fingerprint_must_look_like_one() {
        assert_eq!(
            parse_fpcalc(br#"{"duration": 12.0, "fingerprint": "AQAAT0mUaEkSRZEGAA"}"#).unwrap(),
            "AQAAT0mUaEkSRZEGAA"
        );
        assert!(parse_fpcalc(br#"{"duration": 1.0, "fingerprint": ""}"#).is_err());
        assert!(parse_fpcalc(br#"{"fingerprint": "a&b=c"}"#).is_err());
        assert!(parse_fpcalc(b"not json").is_err());
    }
}
