// SPDX-License-Identifier: GPL-3.0-or-later

//! The DSF file format: the header chunks, the ID3v2.4 tag at the end, and the mapping from
//! bytes of the data chunk to frames.
//!
//! A DSF file is the DSD chunk (28 bytes), the fmt chunk (52 bytes), the data chunk (a
//! 12-byte header, then the audio) and, here, an ID3v2.4 tag. The audio is stored in
//! 4096-byte blocks, one block per channel in turn ("block group"), with the earliest sample
//! of each byte in the least significant bit. The last block of each channel is padded with
//! zeros.

/// DSD bytes per channel in one SACD frame (1/75 s at 2.8224 MHz).
pub const FRAME_BYTES: u64 = 4704;

/// Bytes per channel in one DSF block.
pub const BLOCK: u64 = 4096;

/// Length of the DSD chunk.
pub const DSD_CHUNK_LEN: u64 = 28;

/// Length of the fmt chunk.
pub const FMT_CHUNK_LEN: u64 = 52;

/// Length of the data chunk header.
pub const DATA_HEADER_LEN: u64 = 12;

/// Bytes before the first audio byte: the DSD chunk, the fmt chunk and the data chunk header.
pub const HEADER_LEN: u64 = DSD_CHUNK_LEN + FMT_CHUNK_LEN + DATA_HEADER_LEN;

/// The sampling frequency of SACD audio, in Hz.
pub const SAMPLE_RATE: u32 = 2_822_400;

/// The audio layout of one DSF file: `frames` SACD frames of `channels` channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Channel count, 1 to 6.
    pub channels: u8,
    /// Number of frames (timeline positions) in the file.
    pub frames: u32,
}

/// One run of bytes of the data chunk that comes from a single source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Piece {
    /// `len` bytes of channel `channel`, starting at byte `byte` of that channel's audio.
    /// The run lies inside one frame: it starts at byte `byte % 4704` of frame
    /// `byte / 4704` and does not cross the frame's end.
    Audio {
        /// The channel.
        channel: u8,
        /// The first byte, counted from the start of the channel's audio.
        byte: u64,
        /// The run length.
        len: usize,
    },
    /// `len` zero bytes of padding at the end of a channel's last block.
    Pad {
        /// The run length.
        len: usize,
    },
}

impl Layout {
    /// Audio bytes per channel, without padding.
    pub fn bytes_per_channel(&self) -> u64 {
        u64::from(self.frames) * FRAME_BYTES
    }

    /// Blocks per channel, the last one padded.
    pub fn blocks_per_channel(&self) -> u64 {
        self.bytes_per_channel().div_ceil(BLOCK)
    }

    /// Bytes in one block group (one block of every channel).
    pub fn group_len(&self) -> u64 {
        BLOCK * u64::from(self.channels)
    }

    /// Length of the audio in the data chunk, with padding.
    pub fn data_len(&self) -> u64 {
        self.blocks_per_channel() * self.group_len()
    }

    /// Samples per channel.
    pub fn samples(&self) -> u64 {
        self.bytes_per_channel() * 8
    }

    /// The DSF channel type for this channel count.
    pub fn channel_type(&self) -> u32 {
        match self.channels {
            1 => 1, // mono
            2 => 2, // stereo
            3 => 3, // 3 channels
            4 => 4, // quad
            5 => 6, // 5 channels
            _ => 7, // 5.1 channels
        }
    }

    /// The source of data byte `d` (counted from the first audio byte) and how many of the
    /// following bytes, at most `max`, come from the same run. `None` past the end of the
    /// data.
    pub fn piece_at(&self, d: u64, max: u64) -> Option<Piece> {
        if d >= self.data_len() || max == 0 || self.channels == 0 {
            return None;
        }
        let group = d / self.group_len();
        let in_group = d % self.group_len();
        let channel = (in_group / BLOCK) as u8;
        let in_block = in_group % BLOCK;
        let byte = group * BLOCK + in_block;
        let block_left = BLOCK - in_block;
        let bpc = self.bytes_per_channel();
        if byte >= bpc {
            // Padding up to the end of the block.
            return Some(Piece::Pad { len: block_left.min(max) as usize });
        }
        let frame_left = FRAME_BYTES - byte % FRAME_BYTES;
        let len = block_left.min(frame_left).min(bpc - byte).min(max);
        Some(Piece::Audio { channel, byte, len: len as usize })
    }

    /// The runs covering data bytes `[start, end)`, clipped to the data length.
    pub fn pieces(&self, start: u64, end: u64) -> Pieces {
        Pieces { layout: *self, pos: start, end: end.min(self.data_len()) }
    }

    /// The frames (indices into the file, `0..frames`) that data bytes `[start, end)` need,
    /// as a half-open range; empty when they need none (padding only, or nothing).
    pub fn frames_for(&self, start: u64, end: u64) -> (u32, u32) {
        let mut lo = u64::MAX;
        let mut hi = 0u64;
        // At most one audio run per channel block, and its frame range is monotone within a
        // group: looking at the first and last group is enough, but walking the runs is
        // simple and a read covers at most a few hundred of them.
        for p in self.pieces(start, end) {
            if let Piece::Audio { byte, len, .. } = p {
                lo = lo.min(byte / FRAME_BYTES);
                hi = hi.max((byte + len as u64 - 1) / FRAME_BYTES + 1);
            }
        }
        if lo == u64::MAX { (0, 0) } else { (lo as u32, hi as u32) }
    }
}

/// Iterator over the runs of a byte range; see [`Layout::pieces`].
#[derive(Debug)]
pub struct Pieces {
    layout: Layout,
    pos: u64,
    end: u64,
}

impl Iterator for Pieces {
    type Item = Piece;

    fn next(&mut self) -> Option<Piece> {
        if self.pos >= self.end {
            return None;
        }
        let p = self.layout.piece_at(self.pos, self.end - self.pos)?;
        self.pos += match p {
            Piece::Audio { len, .. } | Piece::Pad { len } => len as u64,
        };
        Some(p)
    }
}

/// The three header chunks of a file with `layout` and a tag of `tag_len` bytes.
pub fn header(layout: &Layout, tag_len: u64) -> [u8; HEADER_LEN as usize] {
    let data_len = layout.data_len();
    let total = HEADER_LEN + data_len + tag_len;
    let metadata = if tag_len > 0 { HEADER_LEN + data_len } else { 0 };
    let mut h = Vec::with_capacity(HEADER_LEN as usize);
    h.extend_from_slice(b"DSD ");
    h.extend_from_slice(&DSD_CHUNK_LEN.to_le_bytes());
    h.extend_from_slice(&total.to_le_bytes());
    h.extend_from_slice(&metadata.to_le_bytes());
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&FMT_CHUNK_LEN.to_le_bytes());
    h.extend_from_slice(&1u32.to_le_bytes()); // format version
    h.extend_from_slice(&0u32.to_le_bytes()); // format id: DSD raw
    h.extend_from_slice(&layout.channel_type().to_le_bytes());
    h.extend_from_slice(&u32::from(layout.channels).to_le_bytes());
    h.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    h.extend_from_slice(&1u32.to_le_bytes()); // bits per sample: 1, LSB first
    h.extend_from_slice(&layout.samples().to_le_bytes());
    h.extend_from_slice(&(BLOCK as u32).to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // reserved
    h.extend_from_slice(b"data");
    h.extend_from_slice(&(DATA_HEADER_LEN + data_len).to_le_bytes());
    let mut out = [0u8; HEADER_LEN as usize];
    out.copy_from_slice(&h);
    out
}

/// The text frames of an ID3v2.4 tag, by frame id. Empty values are left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tags {
    /// Frames in the order they are written: (frame id, text).
    pub frames: Vec<([u8; 4], String)>,
}

impl Tags {
    /// Adds a text frame if `text` has something to show.
    pub fn set(&mut self, id: &[u8; 4], text: Option<&str>) {
        let Some(text) = text else { return };
        // NUL separates values in ID3v2.4 text frames; one value per frame here.
        let clean: String = text.chars().filter(|&c| c != '\0').collect();
        let clean = clean.trim();
        if !clean.is_empty() {
            self.frames.push((*id, clean.to_owned()));
        }
    }

    /// The tag: an ID3v2.4 header and one UTF-8 text frame per entry. Empty if there are
    /// no frames.
    pub fn to_id3(&self) -> Vec<u8> {
        if self.frames.is_empty() {
            return Vec::new();
        }
        let mut body = Vec::new();
        for (id, text) in &self.frames {
            // Each text is at most a few KiB (SACD text strings are cut at 4096 bytes), far
            // below the 2^28 limit of a syncsafe size.
            let size = 1 + text.len() as u32;
            body.extend_from_slice(id);
            body.extend_from_slice(&syncsafe(size));
            body.extend_from_slice(&[0, 0]); // frame flags
            body.push(3); // UTF-8
            body.extend_from_slice(text.as_bytes());
        }
        let mut tag = Vec::with_capacity(10 + body.len());
        tag.extend_from_slice(b"ID3");
        tag.extend_from_slice(&[4, 0, 0]); // version 2.4.0, no flags
        tag.extend_from_slice(&syncsafe(body.len() as u32));
        tag.extend_from_slice(&body);
        tag
    }
}

/// A 28-bit value as an ID3v2 syncsafe integer.
pub fn syncsafe(v: u32) -> [u8; 4] {
    [((v >> 21) & 0x7F) as u8, ((v >> 14) & 0x7F) as u8, ((v >> 7) & 0x7F) as u8, (v & 0x7F) as u8]
}

/// Bit-reversal table: an SACD byte (earliest sample in the MSB) to a DSF byte (earliest
/// sample in the LSB) and back.
pub static REVERSE: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = (i as u8).reverse_bits();
        i += 1;
    }
    t
};

/// Converts one interleaved SACD frame (`channels x 4704` bytes, byte `i` of channel `c` at
/// `i x channels + c`, earliest sample in the MSB) to DSF order: channel after channel, 4704
/// bytes each, earliest sample in the LSB.
pub fn planarize(interleaved: &[u8], channels: u8, out: &mut [u8]) {
    let c = usize::from(channels);
    let n = FRAME_BYTES as usize;
    if c == 0 || interleaved.len() < c * n || out.len() < c * n {
        return;
    }
    if c == 2 {
        let (l, r) = out.split_at_mut(n);
        for ((pair, a), b) in interleaved.chunks_exact(2).zip(l.iter_mut()).zip(r.iter_mut()) {
            *a = REVERSE[usize::from(pair[0])];
            *b = REVERSE[usize::from(pair[1])];
        }
        return;
    }
    for (i, chunk) in interleaved.chunks_exact(c).take(n).enumerate() {
        for (ch, &v) in chunk.iter().enumerate() {
            out[ch * n + i] = REVERSE[usize::from(v)];
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }
    fn u64_at(b: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
    }

    #[test]
    fn header_fields() {
        let layout = Layout { channels: 2, frames: 75 };
        let tag_len = 100;
        let h = header(&layout, tag_len);
        assert_eq!(&h[0..4], b"DSD ");
        assert_eq!(u64_at(&h, 4), 28);
        let data_len = layout.data_len();
        assert_eq!(u64_at(&h, 12), 92 + data_len + tag_len);
        assert_eq!(u64_at(&h, 20), 92 + data_len);
        assert_eq!(&h[28..32], b"fmt ");
        assert_eq!(u64_at(&h, 32), 52);
        assert_eq!(u32_at(&h, 40), 1);
        assert_eq!(u32_at(&h, 44), 0);
        assert_eq!(u32_at(&h, 48), 2);
        assert_eq!(u32_at(&h, 52), 2);
        assert_eq!(u32_at(&h, 56), 2_822_400);
        assert_eq!(u32_at(&h, 60), 1);
        assert_eq!(u64_at(&h, 64), 75 * 4704 * 8);
        assert_eq!(u32_at(&h, 72), 4096);
        assert_eq!(u32_at(&h, 76), 0);
        assert_eq!(&h[80..84], b"data");
        assert_eq!(u64_at(&h, 84), 12 + data_len);
        // 75 x 4704 = 352800 bytes per channel = 86.13 blocks: 87 blocks.
        assert_eq!(layout.blocks_per_channel(), 87);
        assert_eq!(data_len, 87 * 4096 * 2);
    }

    #[test]
    fn no_tag_no_pointer() {
        let h = header(&Layout { channels: 6, frames: 1 }, 0);
        assert_eq!(u64_at(&h, 20), 0);
        assert_eq!(u32_at(&h, 48), 7);
        assert_eq!(u32_at(&h, 52), 6);
    }

    #[test]
    fn channel_types() {
        let t: Vec<u32> = (1..=6).map(|c| Layout { channels: c, frames: 1 }.channel_type()).collect();
        assert_eq!(t, [1, 2, 3, 4, 6, 7]);
    }

    #[test]
    fn id3_layout() {
        let mut tags = Tags::default();
        tags.set(b"TIT2", Some("Ab\0c"));
        tags.set(b"TPE1", Some("   "));
        tags.set(b"TALB", None);
        tags.set(b"TRCK", Some("1/2"));
        let tag = tags.to_id3();
        assert_eq!(&tag[0..6], b"ID3\x04\x00\x00");
        let size = tag.len() - 10;
        assert_eq!(tag[6..10], syncsafe(size as u32));
        assert_eq!(&tag[10..14], b"TIT2");
        assert_eq!(tag[14..18], syncsafe(4));
        assert_eq!(&tag[18..20], &[0, 0]);
        assert_eq!(&tag[20..24], b"\x03Abc");
        assert_eq!(&tag[24..28], b"TRCK");
        assert_eq!(&tag[34..], b"\x031/2");
        assert!(Tags::default().to_id3().is_empty());
    }

    #[test]
    fn syncsafe_values() {
        assert_eq!(syncsafe(0), [0, 0, 0, 0]);
        assert_eq!(syncsafe(127), [0, 0, 0, 127]);
        assert_eq!(syncsafe(128), [0, 0, 1, 0]);
        assert_eq!(syncsafe(0x0FFF_FFFF), [127, 127, 127, 127]);
    }

    #[test]
    fn mapping_examples() {
        let l = Layout { channels: 2, frames: 2 };
        // First block of channel 0, then of channel 1.
        assert_eq!(l.piece_at(0, u64::MAX), Some(Piece::Audio { channel: 0, byte: 0, len: 4096 }));
        assert_eq!(l.piece_at(4096, u64::MAX), Some(Piece::Audio { channel: 1, byte: 0, len: 4096 }));
        // Second group: channel byte 4096, frame 0 ends at 4704.
        assert_eq!(l.piece_at(8192, u64::MAX), Some(Piece::Audio { channel: 0, byte: 4096, len: 608 }));
        assert_eq!(
            l.piece_at(8192 + 608, u64::MAX),
            Some(Piece::Audio { channel: 0, byte: 4704, len: 3488 })
        );
        // 9408 bytes per channel: third group holds 1216 bytes, then padding.
        assert_eq!(l.piece_at(16384, u64::MAX), Some(Piece::Audio { channel: 0, byte: 8192, len: 1216 }));
        assert_eq!(l.piece_at(16384 + 1216, u64::MAX), Some(Piece::Pad { len: 4096 - 1216 }));
        assert_eq!(l.piece_at(l.data_len(), u64::MAX), None);
        assert_eq!(l.frames_for(0, 4096), (0, 1));
        assert_eq!(l.frames_for(8192, 8192 + 609), (0, 2));
        assert_eq!(l.frames_for(16384 + 1216, 16384 + 4096), (0, 0));
    }

    /// The data chunk of a layout built the slow way: every channel's bytes laid out in
    /// blocks, byte `b` of channel `c` being `tag(c, b)`.
    fn naive_data(l: &Layout, tag: impl Fn(u8, u64) -> u8) -> Vec<u8> {
        let bpc = l.bytes_per_channel();
        let blocks = bpc.div_ceil(BLOCK);
        let mut out = Vec::new();
        for g in 0..blocks {
            for c in 0..l.channels {
                for i in 0..BLOCK {
                    let b = g * BLOCK + i;
                    out.push(if b < bpc { tag(c, b) } else { 0 });
                }
            }
        }
        out
    }

    fn tag(c: u8, b: u64) -> u8 {
        (b.wrapping_mul(31) ^ (u64::from(c) << 5) ^ (b / FRAME_BYTES)) as u8
    }

    /// Bytes `[s, e)` of the data chunk built from the runs.
    fn from_pieces(l: &Layout, s: u64, e: u64) -> Vec<u8> {
        let mut out = Vec::new();
        for p in l.pieces(s, e) {
            match p {
                Piece::Pad { len } => out.extend(std::iter::repeat_n(0, len)),
                Piece::Audio { channel, byte, len } => {
                    // A run stays inside one frame and one block.
                    assert_eq!(byte / FRAME_BYTES, (byte + len as u64 - 1) / FRAME_BYTES);
                    assert_eq!(byte / BLOCK, (byte + len as u64 - 1) / BLOCK);
                    out.extend((byte..byte + len as u64).map(|b| tag(channel, b)));
                }
            }
        }
        out
    }

    proptest::proptest! {
        #[test]
        fn mapping_matches_naive_layout(
            channels in 1u8..=6,
            frames in 1u32..40,
            a in proptest::num::u64::ANY,
            b in proptest::num::u64::ANY,
        ) {
            let l = Layout { channels, frames };
            let data = naive_data(&l, tag);
            proptest::prop_assert_eq!(data.len() as u64, l.data_len());
            let n = l.data_len() + 100;
            let (s, e) = { let (x, y) = (a % n, b % n); (x.min(y), x.max(y)) };
            let got = from_pieces(&l, s, e);
            let want = &data[(s.min(l.data_len())) as usize..(e.min(l.data_len())) as usize];
            proptest::prop_assert_eq!(&got[..], want);
            // The frames a range needs are exactly those its audio bytes come from.
            let (f0, f1) = l.frames_for(s, e);
            let mut want_frames = std::collections::BTreeSet::new();
            for p in l.pieces(s, e) {
                if let Piece::Audio { byte, len, .. } = p {
                    want_frames.insert(byte / FRAME_BYTES);
                    want_frames.insert((byte + len as u64 - 1) / FRAME_BYTES);
                }
            }
            match (want_frames.first(), want_frames.last()) {
                (Some(&lo), Some(&hi)) => proptest::prop_assert_eq!((u64::from(f0), u64::from(f1)), (lo, hi + 1)),
                _ => proptest::prop_assert_eq!(f0, f1),
            }
        }

        #[test]
        fn piece_lengths_respect_max(channels in 1u8..=6, frames in 1u32..2000, d in proptest::num::u64::ANY, max in 1u64..10_000) {
            let l = Layout { channels, frames };
            let d = d % l.data_len();
            match l.piece_at(d, max) {
                Some(Piece::Audio { len, .. }) | Some(Piece::Pad { len }) => {
                    proptest::prop_assert!(len >= 1 && len as u64 <= max && len as u64 <= BLOCK);
                }
                None => proptest::prop_assert!(false, "no piece inside the data"),
            }
        }
    }

    #[test]
    fn planarize_reverses_and_splits() {
        for c in 1..=6u8 {
            let n = 4704 * usize::from(c);
            let inter: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
            let mut out = vec![0u8; n];
            planarize(&inter, c, &mut out);
            for i in 0..4704 {
                for ch in 0..usize::from(c) {
                    assert_eq!(out[ch * 4704 + i], inter[i * usize::from(c) + ch].reverse_bits());
                }
            }
        }
    }
}
