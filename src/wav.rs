// SPDX-License-Identifier: GPL-3.0-or-later

//! The WAV file format: RIFF/WAVE (or RF64 when the file is over 4 GiB) with a
//! `WAVE_FORMAT_EXTENSIBLE` fmt chunk, the data chunk, and the tags after it.
//!
//! ```text
//! RIFF <size> WAVE                  (RF64 0xFFFFFFFF WAVE ds64 <28 bytes> when over 4 GiB)
//! fmt  <40>   WAVE_FORMAT_EXTENSIBLE, 24-bit PCM, channel mask
//! data <size> interleaved 24-bit little-endian samples, frame after frame
//! LIST <size> INFO INAM IART IPRD ITRK ICRD IGNR      (if the track has any of them)
//! id3  <size> the ID3v2.4 tag of the DSF file          (if the track has any text)
//! ```
//!
//! The tags come after the audio so that the header, and with it the offset of every audio
//! byte, does not depend on the text. Every size is known from the TOC alone.

use std::ops::Range;

use crate::dsf::Tags;

/// Bits per sample of the PCM output.
pub const BITS: u16 = 24;

/// Bytes per sample.
pub const SAMPLE_BYTES: u64 = 3;

/// Length of the fmt chunk body (`WAVE_FORMAT_EXTENSIBLE`).
pub const FMT_LEN: u32 = 40;

/// Length of the ds64 chunk body (no table).
pub const DS64_LEN: u32 = 28;

/// Header length of a plain RIFF file: RIFF header, fmt chunk, data chunk header.
pub const RIFF_HEADER_LEN: u64 = 12 + 8 + FMT_LEN as u64 + 8;

/// Header length of an RF64 file: as RIFF, plus the ds64 chunk.
pub const RF64_HEADER_LEN: u64 = RIFF_HEADER_LEN + 8 + DS64_LEN as u64;

/// `WAVE_FORMAT_EXTENSIBLE`.
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// `KSDATAFORMAT_SUBTYPE_PCM`: 00000001-0000-0010-8000-00aa00389b71, in file byte order.
const SUBTYPE_PCM: [u8; 16] =
    [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71];

/// Speaker positions of the channel mask (`SPEAKER_*` of `WAVEFORMATEXTENSIBLE`).
pub mod speaker {
    /// Front left.
    pub const FL: u32 = 0x1;
    /// Front right.
    pub const FR: u32 = 0x2;
    /// Front centre.
    pub const FC: u32 = 0x4;
    /// Low-frequency effects.
    pub const LFE: u32 = 0x8;
    /// Back left.
    pub const BL: u32 = 0x10;
    /// Back right.
    pub const BR: u32 = 0x20;
}

/// The channel mask for a channel count.
///
/// Provisional: derived from the channel count only, in the order of sacdfs's DSF channel
/// types (1 FC; 2 FL FR; 3 FL FR FC; 4 FL FR BL BR; 5 FL FR FC BL BR; 6 FL FR FC LFE BL BR),
/// which is also the order of the mask bits, so the samples need no reordering. scarletbook
/// 0.3.0 exposes the area's loudspeaker configuration only as the raw `area_config` byte,
/// and sacdfs keeps no SACD format knowledge of its own, so a typed channel assignment has
/// been requested upstream.
// TODO(scarletbook#75): take the channel assignment from scarletbook's typed API.
pub fn channel_mask(channels: u8) -> u32 {
    use speaker::*;
    match channels {
        1 => FC,
        2 => FL | FR,
        3 => FL | FR | FC,
        4 => FL | FR | BL | BR,
        5 => FL | FR | FC | BL | BR,
        6 => FL | FR | FC | LFE | BL | BR,
        _ => 0,
    }
}

/// The audio layout of one WAV file: `frames` SACD frames of `channels` channels, each
/// converted to `samples_per_frame` samples per channel at `rate` Hz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Channel count, 1 to 6.
    pub channels: u8,
    /// Number of frames (timeline positions) in the file.
    pub frames: u32,
    /// Output samples per channel per frame.
    pub samples_per_frame: u32,
    /// Output sample rate in Hz.
    pub rate: u32,
}

impl Layout {
    /// Bytes of one sample of every channel.
    pub fn block_align(&self) -> u64 {
        u64::from(self.channels) * SAMPLE_BYTES
    }

    /// Bytes of PCM per frame.
    pub fn frame_bytes(&self) -> u64 {
        u64::from(self.samples_per_frame) * self.block_align()
    }

    /// Length of the data chunk body.
    pub fn data_len(&self) -> u64 {
        u64::from(self.frames) * self.frame_bytes()
    }

    /// Samples per channel.
    pub fn samples(&self) -> u64 {
        u64::from(self.frames) * u64::from(self.samples_per_frame)
    }

    /// The frames (indices into the file) that data bytes `[start, end)` need, as a
    /// half-open range; empty when they need none.
    pub fn frames_for(&self, start: u64, end: u64) -> (u32, u32) {
        let (s, e) = (start.min(self.data_len()), end.min(self.data_len()));
        let fb = self.frame_bytes();
        if s >= e || fb == 0 {
            return (0, 0);
        }
        ((s / fb) as u32, e.div_ceil(fb) as u32)
    }
}

/// Appends a chunk: id, 32-bit size, body, and a pad byte if the body is odd.
fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

/// The chunks after the audio, and where in them the ID3v2.4 tag lies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trailer {
    /// The `LIST`/`INFO` chunk and the `id3 ` chunk (each only if it has something).
    pub bytes: Vec<u8>,
    /// The ID3v2.4 tag inside `bytes` (empty if there is none).
    pub tag: Range<usize>,
}

/// The INFO fields written, and the ID3v2.4 frames they come from.
const INFO: [(&[u8; 4], &[u8; 4]); 6] = [
    (b"INAM", b"TIT2"),
    (b"IART", b"TPE1"),
    (b"IPRD", b"TALB"),
    (b"ITRK", b"TRCK"),
    (b"ICRD", b"TDRC"),
    (b"IGNR", b"TCON"),
];

/// The trailer of a file with `tags`: a `LIST`/`INFO` chunk with INAM, IART, IPRD, ITRK,
/// ICRD and IGNR from TIT2, TPE1, TALB, TRCK, TDRC and TCON
/// (UTF-8, NUL-terminated; ITRK is the track number alone), then an `id3 ` chunk holding
/// `tags` as the same ID3v2.4 tag as the DSF file's.
pub fn trailer(tags: &Tags) -> Trailer {
    let mut info = b"INFO".to_vec();
    for (field, frame) in INFO {
        let Some(text) = tags.get(frame) else { continue };
        let text = if field == b"ITRK" { text.split('/').next().unwrap_or(text) } else { text };
        let mut body = text.as_bytes().to_vec();
        body.push(0);
        chunk(&mut info, field, &body);
    }
    let mut bytes = Vec::new();
    if info.len() > 4 {
        chunk(&mut bytes, b"LIST", &info);
    }
    let id3 = tags.to_id3();
    let mut tag = 0..0;
    if !id3.is_empty() {
        let at = bytes.len() + 8;
        tag = at..at + id3.len();
        chunk(&mut bytes, b"id3 ", &id3);
    }
    Trailer { bytes, tag }
}

/// Whether a file with `layout` and a trailer of `trailer_len` bytes needs RF64: its RIFF
/// size (everything after the first 8 bytes) does not fit in 32 bits.
pub fn needs_rf64(layout: &Layout, trailer_len: u64) -> bool {
    RIFF_HEADER_LEN - 8 + layout.data_len() + trailer_len > u64::from(u32::MAX)
}

/// The header of a file with `layout` and a trailer of `trailer_len` bytes: everything
/// before the first audio byte ([`RIFF_HEADER_LEN`] or [`RF64_HEADER_LEN`] bytes).
pub fn header(layout: &Layout, trailer_len: u64) -> Vec<u8> {
    let rf64 = needs_rf64(layout, trailer_len);
    let header_len = if rf64 { RF64_HEADER_LEN } else { RIFF_HEADER_LEN };
    let data_len = layout.data_len();
    let riff_size = header_len - 8 + data_len + trailer_len;
    let mut h = Vec::with_capacity(header_len as usize);
    if rf64 {
        h.extend_from_slice(b"RF64");
        h.extend_from_slice(&u32::MAX.to_le_bytes());
        h.extend_from_slice(b"WAVE");
        let mut ds64 = Vec::with_capacity(DS64_LEN as usize);
        ds64.extend_from_slice(&riff_size.to_le_bytes());
        ds64.extend_from_slice(&data_len.to_le_bytes());
        ds64.extend_from_slice(&layout.samples().to_le_bytes());
        ds64.extend_from_slice(&0u32.to_le_bytes()); // no table
        chunk(&mut h, b"ds64", &ds64);
    } else {
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&(riff_size as u32).to_le_bytes());
        h.extend_from_slice(b"WAVE");
    }
    let block_align = layout.block_align() as u16;
    let mut fmt = Vec::with_capacity(FMT_LEN as usize);
    fmt.extend_from_slice(&FORMAT_EXTENSIBLE.to_le_bytes());
    fmt.extend_from_slice(&u16::from(layout.channels).to_le_bytes());
    fmt.extend_from_slice(&layout.rate.to_le_bytes());
    fmt.extend_from_slice(&(layout.rate * u32::from(block_align)).to_le_bytes());
    fmt.extend_from_slice(&block_align.to_le_bytes());
    fmt.extend_from_slice(&BITS.to_le_bytes());
    fmt.extend_from_slice(&22u16.to_le_bytes()); // extension size
    fmt.extend_from_slice(&BITS.to_le_bytes()); // valid bits per sample
    fmt.extend_from_slice(&channel_mask(layout.channels).to_le_bytes());
    fmt.extend_from_slice(&SUBTYPE_PCM);
    chunk(&mut h, b"fmt ", &fmt);
    h.extend_from_slice(b"data");
    let data_size = if rf64 { u32::MAX } else { data_len as u32 };
    h.extend_from_slice(&data_size.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn u16_at(b: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
    }
    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }
    fn u64_at(b: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
    }

    fn layout(channels: u8, frames: u32) -> Layout {
        Layout { channels, frames, samples_per_frame: 2352, rate: 176_400 }
    }

    #[test]
    fn riff_header_fields() {
        let l = layout(2, 75);
        let h = header(&l, 100);
        assert_eq!(h.len() as u64, RIFF_HEADER_LEN);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u64::from(u32_at(&h, 4)), 68 - 8 + 75 * 2352 * 6 + 100);
        assert_eq!(&h[8..16], b"WAVEfmt ");
        assert_eq!(u32_at(&h, 16), 40);
        assert_eq!(u16_at(&h, 20), 0xFFFE);
        assert_eq!(u16_at(&h, 22), 2);
        assert_eq!(u32_at(&h, 24), 176_400);
        assert_eq!(u32_at(&h, 28), 176_400 * 6);
        assert_eq!(u16_at(&h, 32), 6);
        assert_eq!(u16_at(&h, 34), 24);
        assert_eq!(u16_at(&h, 36), 22);
        assert_eq!(u16_at(&h, 38), 24);
        assert_eq!(u32_at(&h, 40), 3);
        assert_eq!(h[44..60], SUBTYPE_PCM);
        assert_eq!(&h[60..64], b"data");
        assert_eq!(u64::from(u32_at(&h, 64)), l.data_len());
        assert_eq!(l.data_len(), 75 * 14112);
    }

    #[test]
    fn rf64_only_past_4_gib() {
        // The largest plain RIFF file and the smallest RF64 one, by trailer length.
        let l = layout(6, 20_000); // 20000 x 42336 = 846.7 MB
        assert!(!needs_rf64(&l, 0));
        let big = layout(6, 110_000); // 4.66 GB of data
        assert!(needs_rf64(&big, 0));
        let edge = u64::from(u32::MAX) - (RIFF_HEADER_LEN - 8) - l.data_len();
        assert!(!needs_rf64(&l, edge));
        assert!(needs_rf64(&l, edge + 1));

        let h = header(&big, 10);
        assert_eq!(h.len() as u64, RF64_HEADER_LEN);
        assert_eq!(&h[0..4], b"RF64");
        assert_eq!(u32_at(&h, 4), u32::MAX);
        assert_eq!(&h[8..16], b"WAVEds64");
        assert_eq!(u32_at(&h, 16), 28);
        assert_eq!(u64_at(&h, 20), RF64_HEADER_LEN - 8 + big.data_len() + 10);
        assert_eq!(u64_at(&h, 28), big.data_len());
        assert_eq!(u64_at(&h, 36), 110_000 * 2352);
        assert_eq!(u32_at(&h, 44), 0);
        assert_eq!(&h[48..52], b"fmt ");
        assert_eq!(u16_at(&h, 58), 6);
        assert_eq!(u32_at(&h, 76), 0x3F);
        assert_eq!(&h[96..100], b"data");
        assert_eq!(u32_at(&h, 100), u32::MAX);
    }

    #[test]
    fn channel_masks() {
        let m: Vec<u32> = (1..=6).map(channel_mask).collect();
        assert_eq!(m, [0x4, 0x3, 0x7, 0x33, 0x37, 0x3F]);
        // As many speakers as channels.
        for c in 1..=6u8 {
            assert_eq!(channel_mask(c).count_ones(), u32::from(c));
        }
    }

    #[test]
    fn frame_mapping() {
        let l = layout(2, 3);
        assert_eq!(l.frame_bytes(), 14112);
        assert_eq!(l.frames_for(0, 1), (0, 1));
        assert_eq!(l.frames_for(14111, 14113), (0, 2));
        assert_eq!(l.frames_for(14112, 14112 * 3), (1, 3));
        assert_eq!(l.frames_for(14112 * 3, u64::MAX), (0, 0));
        assert_eq!(l.frames_for(5, 5), (0, 0));
        let l = Layout { channels: 5, frames: 2, samples_per_frame: 1176, rate: 88_200 };
        assert_eq!(l.frame_bytes(), 1176 * 15);
        assert_eq!(l.frames_for(0, u64::MAX), (0, 2));
    }

    #[test]
    fn trailer_chunks() {
        let mut t = Tags::default();
        t.set(b"TIT2", Some("Abc"));
        t.set(b"TPE1", Some("P"));
        t.set(b"TRCK", Some("3/12"));
        t.set(b"TCOM", Some("C"));
        let tr = trailer(&t);
        let b = &tr.bytes;
        assert_eq!(&b[0..4], b"LIST");
        let list_len = u32_at(b, 4) as usize;
        assert_eq!(&b[8..12], b"INFO");
        // INAM "Abc\0", IART "P\0", ITRK "3\0": all even, no padding.
        assert_eq!(&b[12..16], b"INAM");
        assert_eq!(u32_at(b, 16), 4);
        assert_eq!(&b[20..24], b"Abc\0");
        assert_eq!(&b[24..28], b"IART");
        assert_eq!(&b[32..34], b"P\0");
        assert_eq!(&b[34..38], b"ITRK");
        assert_eq!(&b[42..44], b"3\0");
        assert_eq!(list_len, 44 - 8);
        assert_eq!(&b[44..48], b"id3 ");
        let id3 = t.to_id3();
        assert_eq!(u32_at(b, 48) as usize, id3.len());
        assert_eq!(tr.tag, 52..52 + id3.len());
        assert_eq!(&b[tr.tag.clone()], &id3[..]);
        assert_eq!(b.len(), 52 + id3.len() + id3.len() % 2);
        // Odd strings are padded to an even chunk.
        let mut t = Tags::default();
        t.set(b"TCON", Some("Jazz"));
        let tr = trailer(&t);
        assert_eq!(&tr.bytes[12..16], b"IGNR");
        assert_eq!(u32_at(&tr.bytes, 16), 5);
        assert_eq!(&tr.bytes[20..26], b"Jazz\0\0");
        assert_eq!(u32_at(&tr.bytes, 4), 18);
        assert_eq!(tr.bytes.len() % 2, 0);
        // Only text the INFO chunk does not carry: no LIST chunk.
        let mut t = Tags::default();
        t.set(b"TCOM", Some("C"));
        let tr = trailer(&t);
        assert_eq!(&tr.bytes[0..4], b"id3 ");
        // No text at all: no trailer.
        assert_eq!(trailer(&Tags::default()), Trailer::default());
    }
}
