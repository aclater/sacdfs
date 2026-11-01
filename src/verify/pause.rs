// SPDX-License-Identifier: GPL-3.0-or-later

//! Extracts that differ from a served track by pauses (#93): the track with pause audio
//! before or after it (`sacd_extract -p`, `pysacd-extract -b`), or a track cut short.
//!
//! The shorter of the two streams (extract and served track) is looked for inside the longer
//! one: a needle of non-idle audio from the short one is searched for in the first bytes of the
//! long one (the offset can't exceed the difference in length), and each hit is confirmed by the
//! SHA-256 of the long stream's bytes from there, against the short one's. The audio the long
//! stream has outside that span is then measured, region by region.
//!
//! The meter is the one the owner's survey used: the imbalance of ones and zeros of a window of
//! 4096 samples, `20 log10(|ones - zeros| / 4096)` dB, the worst window of the worst channel. One
//! bit of imbalance (two more ones than zeros... the difference is 2) reads -66.2 dB, two bits
//! (difference 4) -60.2 dB. A region is silence if it is the DSD idle pattern exactly, or no
//! window is worse than -60.2 dB: the balanced residue of a noise shaper, which holds no music.

use std::sync::atomic::AtomicU64;

use sha2::{Digest, Sha256};

use super::SACD_RATE;
use super::container::{Audio, Problem, Source};

/// The most audio, in seconds, a stream may have beyond the other for them to be compared.
/// Beyond it the files are different recordings, not a pause: the longest pause seen in a real
/// library is 24 s.
pub const MAX_EXTRA_SECS: u64 = 60;

/// Bytes per channel in a meter window: 4096 samples.
const WINDOW_BYTES: u32 = 512;

/// The window's imbalance (ones minus zeros) at or below which a region counts as silence:
/// two bits, -60.2 dB.
const SILENCE_MAX_IMBALANCE: u32 = 4;

/// A needle's length, in bytes per channel.
const NEEDLE_FRAMES: usize = 1024;

/// A needle must hold at least this many distinct byte values (idle patterns and the like hold
/// one to four).
const NEEDLE_DISTINCT: usize = 8;

/// How much of the short stream is searched for a needle.
const NEEDLE_SCAN: u64 = 8 << 20;

/// Hits kept; more means a periodic stream, and the first ones decide.
const MAX_HITS: usize = 8;

/// The DSD idle pattern 0x69 (01101001) and its rotations, which is what it is when a stream
/// starts a few samples in.
const fn idle_table() -> [bool; 256] {
    let mut t = [false; 256];
    let mut k = 0;
    while k < 8 {
        t[(0x69u8).rotate_left(k) as usize] = true;
        k += 1;
    }
    t
}
static IDLE: [bool; 256] = idle_table();

/// What a region holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Class {
    /// Exactly the DSD idle pattern (or nothing).
    Digital,
    /// Not the idle pattern, but no window worse than -60.2 dB (the level, -inf if balanced).
    Residue(f64),
    /// Audio, at this level in dB.
    Audio(f64),
}

/// A run of audio beyond the other stream's, and what it holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    /// Samples per channel.
    pub samples: u64,
    /// What it holds.
    pub class: Class,
}

impl Region {
    /// Whether it is silence: nothing, the idle pattern, or residue at or below -60.2 dB.
    pub fn is_silence(&self) -> bool {
        !matches!(self.class, Class::Audio(_))
    }

    /// Its length in seconds.
    pub fn seconds(&self) -> f64 {
        self.samples as f64 / f64::from(SACD_RATE)
    }

    /// "2.350 s of digital silence", "0.013 s of residue at -61.2 dB", "21.300 s of audio at
    /// -32.5 dB".
    pub fn describe(&self) -> String {
        let (s, what) = match self.class {
            Class::Digital => (self.seconds(), "digital silence".to_owned()),
            Class::Residue(db) => (self.seconds(), format!("residue at {}", level(db))),
            Class::Audio(db) => (self.seconds(), format!("audio at {}", level(db))),
        };
        format!("{s:.3} s of {what}")
    }
}

fn level(db: f64) -> String {
    if db.is_finite() { format!("{db:.1} dB") } else { "-inf dB".to_owned() }
}

fn db_of(imbalance: u32) -> f64 {
    if imbalance == 0 { f64::NEG_INFINITY } else { 20.0 * (f64::from(imbalance) / 4096.0).log10() }
}

/// Measures a region of the canonical (byte-interleaved) stream, fed in pieces.
#[derive(Debug)]
pub struct Meter {
    ch: usize,
    phase: usize,
    ones: [u32; 6],
    frames_in_window: u32,
    frames: u64,
    worst: u32,
    idle: bool,
}

impl Meter {
    /// A meter for a stream of `channels` channels (at most 6), starting on a whole frame.
    pub fn new(channels: u8) -> Meter {
        Meter {
            ch: usize::from(channels.clamp(1, 6)),
            phase: 0,
            ones: [0; 6],
            frames_in_window: 0,
            frames: 0,
            worst: 0,
            idle: true,
        }
    }

    /// The next bytes.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &x in bytes {
            self.idle &= IDLE[usize::from(x)];
            self.ones[self.phase] += x.count_ones();
            self.phase += 1;
            if self.phase == self.ch {
                self.phase = 0;
                self.frames += 1;
                self.frames_in_window += 1;
                if self.frames_in_window == WINDOW_BYTES {
                    self.close_window();
                }
            }
        }
    }

    /// Closes the window: a short last one is judged by the same absolute imbalance.
    fn close_window(&mut self) {
        let bits = i64::from(self.frames_in_window) * 8;
        for c in 0..self.ch {
            let diff = (2 * i64::from(self.ones[c]) - bits).unsigned_abs();
            self.worst = self.worst.max(u32::try_from(diff).unwrap_or(u32::MAX));
            self.ones[c] = 0;
        }
        self.frames_in_window = 0;
    }

    /// What the region held.
    pub fn finish(mut self) -> Region {
        if self.frames_in_window > 0 {
            self.close_window();
        }
        let class = if self.idle {
            Class::Digital
        } else if self.worst <= SILENCE_MAX_IMBALANCE {
            Class::Residue(db_of(self.worst))
        } else {
            Class::Audio(db_of(self.worst))
        };
        Region { samples: self.frames * 8, class }
    }
}

/// Finds a needle in a stream fed in pieces.
struct Finder {
    needle: Vec<u8>,
    channels: u64,
    tail: Vec<u8>,
    /// The offset of `tail[0]` in the stream.
    base: u64,
    hits: Vec<u64>,
}

impl Finder {
    fn feed(&mut self, chunk: &[u8]) {
        let n = self.needle.len();
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(chunk);
        let first = self.needle[0];
        let mut i = 0;
        while i + n <= buf.len() && self.hits.len() < MAX_HITS {
            let Some(k) = buf[i..=buf.len() - n].iter().position(|&b| b == first) else { break };
            i += k;
            if buf[i..i + n] == self.needle[..] {
                let at = self.base + i as u64;
                if at % self.channels == 0 {
                    self.hits.push(at);
                }
            }
            i += 1;
        }
        let keep = buf.len().min(n - 1);
        self.base += (buf.len() - keep) as u64;
        self.tail = buf[buf.len() - keep..].to_vec();
    }
}

/// A piece of `audio` that is unlikely to be anywhere else: the first window of
/// [`NEEDLE_FRAMES`] frames, at a multiple of its length, with many distinct byte values.
fn pick_needle(
    audio: &Audio,
    src: &mut dyn Source,
    read: &AtomicU64,
) -> Result<Option<(u64, Vec<u8>)>, Problem> {
    let w = NEEDLE_FRAMES * usize::from(audio.channels);
    let mut pending: Vec<u8> = Vec::new();
    let mut base = 0u64;
    let mut found = None;
    audio.canonical_while(src, read, &mut |b| {
        pending.extend_from_slice(b);
        let mut i = 0;
        while i + w <= pending.len() {
            let win = &pending[i..i + w];
            let mut seen = [false; 256];
            win.iter().for_each(|&x| seen[usize::from(x)] = true);
            if seen.iter().filter(|&&s| s).count() >= NEEDLE_DISTINCT {
                found = Some((base + i as u64, win.to_vec()));
                return false;
            }
            i += w;
        }
        pending.drain(..i);
        base += i as u64;
        base < NEEDLE_SCAN
    })?;
    Ok(found)
}

/// Where the short stream lies in the long one, and what the long one has around it.
#[derive(Debug, PartialEq)]
pub struct Found {
    /// The audio of the long stream before the short one.
    pub head: Region,
    /// The audio of the long stream after it.
    pub tail: Region,
    /// Offset of the short stream in the long one, in samples per channel.
    pub offset_samples: u64,
}

/// Looks for `small` (whose canonical stream hashes to `small_digest`) inside `big`, both of the
/// same channel count. `None`: it is not there, or no needle could be picked from it.
pub fn locate(
    big: (&Audio, &mut dyn Source),
    small: (&Audio, &mut dyn Source),
    small_digest: &[u8; 32],
    read: &AtomicU64,
) -> Result<Option<Found>, Problem> {
    let (big, big_src) = big;
    let (small, small_src) = small;
    let ch = u64::from(big.channels);
    let (Some(l), Some(m)) = (big.canonical_len(), small.canonical_len()) else { return Ok(None) };
    if small.channels != big.channels || m == 0 || l < m {
        return Ok(None);
    }
    let d = l - m;
    let Some((p, needle)) = pick_needle(small, small_src, read)? else { return Ok(None) };
    let limit = d + p + needle.len() as u64;
    let mut finder = Finder { needle, channels: ch, tail: Vec::new(), base: 0, hits: Vec::new() };
    let mut seen = 0u64;
    big.canonical_while(big_src, read, &mut |b| {
        finder.feed(b);
        seen += b.len() as u64;
        finder.hits.len() < MAX_HITS && seen < limit
    })?;
    for q in finder.hits {
        let Some(o) = q.checked_sub(p).filter(|&o| o <= d && o % ch == 0) else { continue };
        let (head, mid, tail) = scan(big, big_src, read, o, m)?;
        if &mid == small_digest {
            return Ok(Some(Found { head, tail, offset_samples: o / ch * 8 }));
        }
    }
    Ok(None)
}

/// One pass over `big`: the region before `offset`, the SHA-256 of the next `len` bytes, and the
/// region after them.
fn scan(
    big: &Audio,
    src: &mut dyn Source,
    read: &AtomicU64,
    offset: u64,
    len: u64,
) -> Result<(Region, [u8; 32], Region), Problem> {
    let (mut head, mut tail) = (Meter::new(big.channels), Meter::new(big.channels));
    let mut mid = Sha256::new();
    let mut at = 0u64;
    big.canonical_while(src, read, &mut |mut b| {
        while !b.is_empty() {
            let (n, which) = if at < offset {
                ((offset - at).min(b.len() as u64), 0)
            } else if at < offset + len {
                ((offset + len - at).min(b.len() as u64), 1)
            } else {
                (b.len() as u64, 2)
            };
            let (now, rest) = b.split_at(n as usize);
            match which {
                0 => head.feed(now),
                1 => mid.update(now),
                _ => tail.feed(now),
            }
            at += n;
            b = rest;
        }
        true
    })?;
    Ok((head.finish(), mid.finalize().into(), tail.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dff(canon: &[u8], ch: u16) -> Vec<u8> {
        let chunk = |id: &[u8], body: &[u8]| {
            let mut c = id.to_vec();
            c.extend_from_slice(&(body.len() as u64).to_be_bytes());
            c.extend_from_slice(body);
            if body.len() % 2 == 1 {
                c.push(0);
            }
            c
        };
        let mut prop = b"SND ".to_vec();
        prop.extend(chunk(b"FS  ", &2_822_400u32.to_be_bytes()));
        let mut chnl = ch.to_be_bytes().to_vec();
        for i in 0..ch {
            chnl.extend_from_slice(&[b'C', b'0', b'0', b'0' + i as u8]);
        }
        prop.extend(chunk(b"CHNL", &chnl));
        prop.extend(chunk(b"CMPR", b"DSD \x00"));
        let mut form = b"DSD ".to_vec();
        form.extend(chunk(b"FVER", &0x0105_0000u32.to_be_bytes()));
        form.extend(chunk(b"PROP", &prop));
        form.extend(chunk(b"DSD ", canon));
        chunk(b"FRM8", &form)
    }

    /// Noise-like audio: a xorshift stream.
    fn noise(n: usize, mut x: u32) -> Vec<u8> {
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 7) as u8
            })
            .collect()
    }

    fn run(big: &[u8], small: &[u8], ch: u16) -> Option<Found> {
        let read = AtomicU64::new(0);
        let (fb, fs) = (dff(big, ch), dff(small, ch));
        let (mut sb, mut ss) = (fb.as_slice(), fs.as_slice());
        let ab = super::super::container::parse(&mut sb).unwrap();
        let asm = super::super::container::parse(&mut ss).unwrap();
        let digest = asm.digest(&mut ss, &read).unwrap();
        locate((&ab, &mut sb), (&asm, &mut ss), &digest, &read).unwrap()
    }

    #[test]
    fn meter_classes() {
        let class = |b: &[u8], ch| {
            let mut m = Meter::new(ch);
            m.feed(b);
            m.finish().class
        };
        assert_eq!(class(&[0x69; 4096], 2), Class::Digital);
        assert_eq!(class(&[0x96; 4096], 2), Class::Digital);
        // Balanced but not the idle pattern: residue at -inf.
        assert!(
            matches!(class(&[0x0F, 0xF0, 0x3C, 0xC3].repeat(1024), 2), Class::Residue(d) if d == f64::NEG_INFINITY)
        );
        // Two bits of imbalance in each window: -60.2 dB, still silence; three: audio.
        let mut w = vec![0x0F; 1024];
        w[0] = 0x1F; // one extra one on channel 0: difference 2 per window (-66.2 dB)
        assert!(matches!(class(&w, 2), Class::Residue(d) if (d + 66.2).abs() < 0.1));
        w[2] = 0x1F; // a second, still on channel 0: difference 4 (-60.2 dB)
        assert!(matches!(class(&w, 2), Class::Residue(d) if (d + 60.2).abs() < 0.1));
        w[4] = 0x1F;
        assert!(matches!(class(&w, 2), Class::Audio(d) if (d + 56.7).abs() < 0.2), "{:?}", class(&w, 2));
        // All ones: full-scale DC.
        assert!(matches!(class(&[0xFF; 1024], 2), Class::Audio(d) if d.abs() < 0.1));
        // Empty.
        assert_eq!(Meter::new(2).finish(), Region { samples: 0, class: Class::Digital });
    }

    #[test]
    fn extract_with_pauses_around_the_track() {
        let track = noise(40_000, 1);
        for ch in [1u16, 2, 5] {
            let c = usize::from(ch);
            let t: Vec<u8> = track[..(track.len() / c) * c].to_vec();
            let head = [0x69].repeat(3 * c * 100);
            let tail = [0x0F, 0xF0].repeat(c * 50);
            let big = [head.clone(), t.clone(), tail.clone()].concat();
            let f = run(&big, &t, ch).unwrap_or_else(|| panic!("{ch} channels"));
            assert_eq!(f.head, Region { samples: 2400, class: Class::Digital });
            assert_eq!(f.offset_samples, 2400);
            assert_eq!(f.tail.samples, 800);
            assert!(f.tail.is_silence() && matches!(f.tail.class, Class::Residue(_)));
            // Only a tail, only a head.
            let f = run(&[t.clone(), tail.clone()].concat(), &t, ch).unwrap();
            assert_eq!((f.head.samples, f.offset_samples), (0, 0));
            let f = run(&[head.clone(), t.clone()].concat(), &t, ch).unwrap();
            assert_eq!(f.tail.samples, 0);
        }
    }

    #[test]
    fn extras_that_are_audio_are_measured() {
        let t = noise(40_000, 2);
        let big = [t.clone(), noise(2000, 3)].concat();
        let f = run(&big, &t, 2).unwrap();
        assert_eq!(f.tail.samples, 1000 * 8);
        assert!(!f.tail.is_silence(), "{:?}", f.tail);
        assert!(f.tail.describe().contains("1000") || f.tail.describe().contains("audio"));
    }

    #[test]
    fn extract_inside_the_track_and_prefixes() {
        // The long stream is the served track: the extract is a prefix of it, or inside it.
        let t = noise(40_000, 4);
        let served = [t.clone(), vec![0x69; 2000]].concat();
        let f = run(&served, &t, 2).unwrap();
        assert_eq!((f.head.samples, f.tail.samples), (0, 8000));
        assert_eq!(f.tail.class, Class::Digital);
        let inside = t[2000..30_000].to_vec();
        let f = run(&t, &inside, 2).unwrap();
        assert_eq!((f.head.samples, f.tail.samples, f.offset_samples), (8000, 5000 * 8, 8000));
    }

    #[test]
    fn different_audio_is_not_found() {
        let a = noise(40_000, 5);
        let b = noise(40_000, 6);
        assert!(run(&[vec![0x69; 400], a.clone()].concat(), &b, 2).is_none());
        // The same needle, other audio after it.
        let mut c = a.clone();
        c[30_000] ^= 1;
        assert!(run(&[vec![0x69; 400], c].concat(), &a, 2).is_none());
        // A track that is only the idle pattern has no needle.
        assert!(run(&[0x69; 8000], &[0x69; 4000], 2).is_none());
    }
}
