// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading the audio of a DSF or DFF (DSDIFF) file as plain DSD, whatever wrote it.
//!
//! Every file is reduced to one canonical stream: the header's samples per channel and
//! nothing more (tags, padding and other chunks dropped), byte-interleaved across channels
//! (byte `i` of channel `c` at `i x channels + c`) with the earliest sample in the most
//! significant bit, as on the disc and in DFF. A DSF file stores blocks of one channel at a
//! time with the earliest sample in the least significant bit (unless its header says 8 bits
//! per sample), so its bytes are bit-reversed and interleaved on the way.
//!
//! Every file is untrusted: nothing a header says may panic or make memory grow beyond a
//! few MiB per reader. Sizes are checked against the file's length before they are used.
//!
//! The formats: DSF as Sony's "DSF File Format Specification" 1.01; DFF as Philips's
//! "Direct Stream Digital Interchange File Format: DSDIFF" 1.5. Container parsing only:
//! no SACD format code.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

use sha2::{Digest, Sha256};

/// Most channels a file may have to be compared (SACD has at most 6; DSF allows 6, DFF
/// more): a larger count is reported, not read.
pub const MAX_CHANNELS: u32 = 6;

/// Largest DSF block size accepted (the specification fixes 4096).
const MAX_BLOCK: u64 = 1 << 20;

/// Bytes read at a time.
const CHUNK: u64 = 4 << 20;

/// Largest tag read into memory; a larger one is reported as unread.
pub const MAX_TAG: u64 = 16 << 20;

/// Most chunks walked in a DFF file before giving up (a hostile file of tiny chunks).
const MAX_CHUNKS: usize = 4096;

/// Something to read bytes from at an offset: a file, or a track file served by sacdfs.
pub trait Source {
    /// Length in bytes.
    fn len(&self) -> u64;
    /// Whether it holds no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Fills `buf` with the bytes at `offset`; `UnexpectedEof` if the source ends first.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

/// A file on disk.
#[derive(Debug)]
pub struct FileSource {
    file: File,
    len: u64,
}

impl FileSource {
    /// Opens `path`.
    pub fn open(path: &std::path::Path) -> io::Result<FileSource> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(FileSource { file, len })
    }
}

impl Source for FileSource {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.file.read_exact_at(buf, offset)
    }
}

/// An in-memory file (tests).
impl Source for &[u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| eof())?;
        let end = start.checked_add(buf.len()).ok_or_else(eof)?;
        let src = self.get(start..end).ok_or_else(eof)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

fn eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "past the end of the file")
}

/// The container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// DSF.
    Dsf,
    /// DSDIFF.
    Dff,
}

/// Where the audio bytes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Data {
    /// DSF blocks from `start`, `block` bytes per channel per group.
    Dsf { start: u64, block: u64, lsb_first: bool },
    /// DFF interleaved bytes from `start`.
    Dff { start: u64 },
}

/// Where a file's tags are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TagPlace {
    /// An ID3v2 tag: offset and the bytes available from there.
    pub id3: Option<(u64, u64)>,
    /// DFF only: the title (DIIN/DITI) and artist (DIIN/DIAR).
    pub dff_title: Option<String>,
    /// See `dff_title`.
    pub dff_artist: Option<String>,
    /// DFF only: other informational chunks (COMT, MANF, DIIN/EMID, DIIN/MARK).
    pub dff_other: Vec<String>,
}

/// A DSD file whose audio can be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Audio {
    /// The container.
    pub kind: Kind,
    /// Channel count, 1 to [`MAX_CHANNELS`].
    pub channels: u8,
    /// Sampling frequency in Hz.
    pub rate: u32,
    /// Samples per channel.
    pub samples: u64,
    data: Data,
    /// The tags.
    pub tags: TagPlace,
}

/// Why a file can't be compared.
#[derive(Debug)]
pub enum Problem {
    /// A valid file of a kind that can't be compared (DST-compressed DFF, ...).
    Unsupported(String),
    /// A broken or truncated file, or a read error.
    Error(String),
}

impl From<io::Error> for Problem {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Problem::Error("truncated: the file ends inside its headers".to_owned())
        } else {
            Problem::Error(format!("read error: {e}"))
        }
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, Problem> {
    Err(Problem::Error(msg.into()))
}

fn u32le(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64le(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8).and_then(|s| s.try_into().ok()).map_or(0, u64::from_le_bytes)
}

fn u64be(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8).and_then(|s| s.try_into().ok()).map_or(0, u64::from_be_bytes)
}

/// Parses the headers of a DSF or DFF file.
pub fn parse(src: &mut dyn Source) -> Result<Audio, Problem> {
    let mut magic = [0u8; 4];
    src.read_at(0, &mut magic)?;
    match &magic {
        b"DSD " => parse_dsf(src),
        b"FRM8" => parse_dff(src),
        _ => err("neither a DSF nor a DFF file (unknown signature)"),
    }
}

fn parse_dsf(src: &mut dyn Source) -> Result<Audio, Problem> {
    let len = src.len();
    let mut h = [0u8; 28];
    src.read_at(0, &mut h)?;
    let dsd_size = u64le(&h, 4);
    let tag_at = u64le(&h, 20);
    if dsd_size < 28 || dsd_size > len {
        return err(format!("bad DSD chunk size {dsd_size}"));
    }
    let mut f = [0u8; 52];
    src.read_at(dsd_size, &mut f)?;
    if &f[0..4] != b"fmt " {
        return err("no fmt chunk after the DSD chunk");
    }
    let fmt_size = u64le(&f, 4);
    let (version, format_id) = (u32le(&f, 12), u32le(&f, 16));
    let channels = u32le(&f, 24);
    let rate = u32le(&f, 28);
    let bits = u32le(&f, 32);
    let samples = u64le(&f, 36);
    let block = u64::from(u32le(&f, 44));
    if fmt_size < 52 {
        return err(format!("bad fmt chunk size {fmt_size}"));
    }
    if version != 1 {
        return Err(Problem::Unsupported(format!("DSF format version {version}")));
    }
    if format_id != 0 {
        return Err(Problem::Unsupported(format!("DSF format id {format_id} (only 0, raw DSD, is defined)")));
    }
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Problem::Unsupported(format!("{channels} channels")));
    }
    let lsb_first = match bits {
        1 => true,
        8 => false,
        _ => return err(format!("bits per sample {bits} (1 or 8 expected)")),
    };
    if block == 0 || block > MAX_BLOCK {
        return err(format!("block size {block}"));
    }
    let data_at = dsd_size
        .checked_add(fmt_size)
        .filter(|&d| d <= len)
        .ok_or_else(|| Problem::Error("truncated: the file ends inside its headers".to_owned()))?;
    let mut d = [0u8; 12];
    src.read_at(data_at, &mut d)?;
    if &d[0..4] != b"data" {
        return err("no data chunk after the fmt chunk");
    }
    let start = data_at + 12;
    let audio = Audio {
        kind: Kind::Dsf,
        channels: channels as u8,
        rate,
        samples,
        data: Data::Dsf { start, block, lsb_first },
        tags: TagPlace {
            id3: (tag_at != 0 && tag_at < len).then(|| (tag_at, len - tag_at)),
            ..TagPlace::default()
        },
    };
    let need = audio.data_end().ok_or_else(|| Problem::Error(format!("absurd sample count {samples}")))?;
    if need > len {
        return err(format!(
            "truncated: the audio of {samples} samples per channel needs {need} bytes, the file has {len}"
        ));
    }
    Ok(audio)
}

/// One DFF chunk header: id, data offset, data size.
fn dff_chunk(src: &mut dyn Source, at: u64, end: u64) -> Result<([u8; 4], u64, u64), Problem> {
    if end.saturating_sub(at) < 12 {
        return err("truncated: a chunk header runs past its container");
    }
    let mut h = [0u8; 12];
    src.read_at(at, &mut h)?;
    let id = [h[0], h[1], h[2], h[3]];
    let size = u64be(&h, 4);
    let data = at + 12;
    Ok((id, data, size))
}

fn id_text(id: &[u8; 4]) -> String {
    id.iter()
        .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn parse_dff(src: &mut dyn Source) -> Result<Audio, Problem> {
    let len = src.len();
    let (_, form_data, form_size) = dff_chunk(src, 0, len)?;
    let mut ft = [0u8; 4];
    src.read_at(form_data, &mut ft)?;
    if &ft != b"DSD " {
        return err(format!("FRM8 form type {:?} (\"DSD \" expected)", id_text(&ft)));
    }
    // A form that claims more than the file: walk what is there, and let the audio check
    // report the truncation.
    let form_end = form_data.saturating_add(form_size).min(len);
    let mut at = form_data + 4;
    let (mut channels, mut rate, mut compression) = (None, None, None);
    let mut sound: Option<(u64, u64, [u8; 4])> = None;
    let mut tags = TagPlace::default();
    let mut n = 0;
    while form_end.saturating_sub(at) >= 12 {
        n += 1;
        if n > MAX_CHUNKS {
            return err("too many chunks");
        }
        let (id, data, size) = dff_chunk(src, at, form_end)?;
        match &id {
            b"PROP" => {
                let end = data.saturating_add(size).min(form_end);
                let mut pt = [0u8; 4];
                src.read_at(data, &mut pt)?;
                if &pt == b"SND " {
                    let mut p = data + 4;
                    let mut m = 0;
                    while end.saturating_sub(p) >= 12 {
                        m += 1;
                        if m > MAX_CHUNKS {
                            return err("too many chunks");
                        }
                        let (sid, sdata, ssize) = dff_chunk(src, p, end)?;
                        match &sid {
                            b"FS  " if ssize >= 4 => {
                                let mut b = [0u8; 4];
                                src.read_at(sdata, &mut b)?;
                                rate = Some(u32::from_be_bytes(b));
                            }
                            b"CHNL" if ssize >= 2 => {
                                let mut b = [0u8; 2];
                                src.read_at(sdata, &mut b)?;
                                channels = Some(u32::from(u16::from_be_bytes(b)));
                            }
                            b"CMPR" if ssize >= 4 => {
                                let mut b = [0u8; 4];
                                src.read_at(sdata, &mut b)?;
                                compression = Some(b);
                            }
                            _ => {}
                        }
                        p = match next_chunk(sdata, ssize) {
                            Some(p) => p,
                            None => break,
                        };
                    }
                }
            }
            b"DSD " | b"DST " => {
                if sound.is_none() {
                    sound = Some((data, size, id));
                }
            }
            b"ID3 " => tags.id3 = Some((data, size.min(len.saturating_sub(data)))),
            b"DIIN" => {
                let end = data.saturating_add(size).min(form_end);
                let mut p = data;
                let mut m = 0;
                while end.saturating_sub(p) >= 12 {
                    m += 1;
                    if m > MAX_CHUNKS {
                        return err("too many chunks");
                    }
                    let (sid, sdata, ssize) = dff_chunk(src, p, end)?;
                    match &sid {
                        b"DITI" => tags.dff_title = dff_text(src, sdata, ssize, end)?,
                        b"DIAR" => tags.dff_artist = dff_text(src, sdata, ssize, end)?,
                        other => tags.dff_other.push(format!("DIIN/{}", id_text(other))),
                    }
                    p = match next_chunk(sdata, ssize) {
                        Some(p) => p,
                        None => break,
                    };
                }
            }
            b"COMT" | b"MANF" => tags.dff_other.push(id_text(&id)),
            _ => {}
        }
        at = match next_chunk(data, size) {
            Some(p) => p,
            None => break,
        };
    }
    let Some(channels) = channels else { return err("no channel count (PROP/CHNL)") };
    let Some(rate) = rate else { return err("no sampling frequency (PROP/FS)") };
    match compression.as_ref() {
        Some(b"DSD ") | None => {}
        Some(b"DST ") => {
            return Err(Problem::Unsupported(
                "DST-compressed DFF: sacdfs compares plain DSD only; decompress it to a DSD DFF \
                 or a DSF file first, or keep it"
                    .to_owned(),
            ));
        }
        Some(other) => return Err(Problem::Unsupported(format!("DFF compression {:?}", id_text(other)))),
    }
    let Some((start, size, id)) = sound else { return err("no sound data chunk") };
    if &id == b"DST " {
        return Err(Problem::Unsupported("DST-compressed DFF (a DST chunk)".to_owned()));
    }
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(Problem::Unsupported(format!("{channels} channels")));
    }
    if size % u64::from(channels) != 0 {
        return err(format!("sound data of {size} bytes is not a whole number of {channels}-channel bytes"));
    }
    let samples = size / u64::from(channels) * 8;
    let audio =
        Audio { kind: Kind::Dff, channels: channels as u8, rate, samples, data: Data::Dff { start }, tags };
    let need = audio.data_end().ok_or_else(|| Problem::Error("absurd sound data size".to_owned()))?;
    if need > len {
        return err(format!("truncated: the sound data needs {need} bytes, the file has {len}"));
    }
    Ok(audio)
}

/// The offset of the chunk after one whose data starts at `data` (chunks are padded to an
/// even length).
fn next_chunk(data: u64, size: u64) -> Option<u64> {
    data.checked_add(size)?.checked_add(size & 1)
}

/// A DFF text chunk (DITI, DIAR): a 32-bit count, then that many characters.
fn dff_text(src: &mut dyn Source, data: u64, size: u64, end: u64) -> Result<Option<String>, Problem> {
    if size < 4 || data.saturating_add(size) > end {
        return Ok(None);
    }
    let mut c = [0u8; 4];
    src.read_at(data, &mut c)?;
    let n = u64::from(u32::from_be_bytes(c)).min(size - 4).min(64 << 10);
    let mut b = vec![0u8; n as usize];
    src.read_at(data + 4, &mut b)?;
    let s = String::from_utf8_lossy(&b).trim_matches(char::from(0)).trim().to_owned();
    Ok((!s.is_empty()).then_some(s))
}

impl Audio {
    /// Bytes per channel: whole bytes holding the samples.
    pub fn bytes_per_channel(&self) -> u64 {
        self.samples.div_ceil(8)
    }

    /// Length of the canonical stream, if it fits in 64 bits.
    pub fn canonical_len(&self) -> Option<u64> {
        self.bytes_per_channel().checked_mul(u64::from(self.channels))
    }

    /// The offset just past the last audio byte the samples need (not counting a DSF
    /// file's padding after the last channel's last byte).
    fn data_end(&self) -> Option<u64> {
        let bpc = self.bytes_per_channel();
        let ch = u64::from(self.channels);
        match self.data {
            Data::Dff { start } => start.checked_add(bpc.checked_mul(ch)?),
            Data::Dsf { start, block, .. } => {
                if bpc == 0 {
                    return Some(start);
                }
                let groups = bpc.div_ceil(block);
                let last = bpc - (groups - 1) * block;
                let group = block.checked_mul(ch)?;
                start.checked_add((groups - 1).checked_mul(group)?)?.checked_add((ch - 1) * block + last)
            }
        }
    }

    /// The SHA-256 of the canonical stream (see the module documentation). `read` is
    /// increased by the bytes read as they are read.
    pub fn digest(
        &self,
        src: &mut dyn Source,
        read: &std::sync::atomic::AtomicU64,
    ) -> Result<[u8; 32], Problem> {
        let mut hash = Sha256::new();
        self.canonical(src, read, &mut |b| hash.update(b))?;
        Ok(hash.finalize().into())
    }

    /// Feeds the canonical stream to `sink`, a few MiB at a time.
    pub fn canonical(
        &self,
        src: &mut dyn Source,
        read: &std::sync::atomic::AtomicU64,
        sink: &mut dyn FnMut(&[u8]),
    ) -> Result<(), Problem> {
        self.canonical_while(src, read, &mut |b| {
            sink(b);
            true
        })
    }

    /// [`Audio::canonical`], stopping as soon as `sink` returns false.
    pub fn canonical_while(
        &self,
        src: &mut dyn Source,
        read: &std::sync::atomic::AtomicU64,
        sink: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<(), Problem> {
        use std::sync::atomic::Ordering::Relaxed;
        let bpc = self.bytes_per_channel();
        let ch = u64::from(self.channels);
        // The last byte's unused low bits (a sample count that is not a multiple of 8).
        let tail_mask: u8 = match self.samples % 8 {
            0 => 0xFF,
            r => 0xFFu8 << (8 - r),
        };
        let io_err = |e: io::Error| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                Problem::Error("truncated: the file ends inside its audio".to_owned())
            } else {
                Problem::Error(format!("read error: {e}"))
            }
        };
        match self.data {
            Data::Dff { start } => {
                let total = bpc * ch;
                let mut buf = vec![0u8; CHUNK.min(total) as usize];
                let mut done = 0u64;
                while done < total {
                    let n = (total - done).min(CHUNK) as usize;
                    let b = &mut buf[..n];
                    src.read_at(start + done, b).map_err(io_err)?;
                    read.fetch_add(n as u64, Relaxed);
                    done += n as u64;
                    if done == total && tail_mask != 0xFF {
                        // DFF holds whole bytes; this cannot happen, but keep it exact.
                        for x in b.iter_mut().rev().take(ch as usize) {
                            *x &= tail_mask;
                        }
                    }
                    if !sink(b) {
                        return Ok(());
                    }
                }
            }
            Data::Dsf { start, block, lsb_first } => {
                if bpc == 0 {
                    return Ok(());
                }
                let group = block * ch;
                let groups = bpc.div_ceil(block);
                // Whole groups per read, at least one.
                let per_read = (CHUNK / group).max(1);
                let mut raw = vec![0u8; (per_read * group).min(groups * group) as usize];
                let mut out = Vec::with_capacity(raw.len());
                let cu = ch as usize;
                let bu = block as usize;
                let mut g = 0u64;
                while g < groups {
                    let k = (groups - g).min(per_read);
                    let at = start + g * group;
                    // The last group may lack the padding after the last channel's bytes.
                    let want = if g + k == groups {
                        let last = bpc - (groups - 1) * block;
                        (k - 1) * group + (ch - 1) * block + last
                    } else {
                        k * group
                    };
                    let r = &mut raw[..want as usize];
                    src.read_at(at, r).map_err(io_err)?;
                    read.fetch_add(want, Relaxed);
                    out.clear();
                    for j in 0..k {
                        let gi = g + j;
                        let n = if gi + 1 == groups { (bpc - gi * block) as usize } else { bu };
                        let base = (j * group) as usize;
                        let blocks = &r[base..];
                        if lsb_first {
                            match cu {
                                1 => out.extend(blocks[..n].iter().map(|&b| b.reverse_bits())),
                                2 => {
                                    let (l, rr) = (&blocks[..n], &blocks[bu..bu + n]);
                                    for (&a, &b) in l.iter().zip(rr) {
                                        out.push(a.reverse_bits());
                                        out.push(b.reverse_bits());
                                    }
                                }
                                _ => {
                                    for i in 0..n {
                                        for c in 0..cu {
                                            out.push(blocks[c * bu + i].reverse_bits());
                                        }
                                    }
                                }
                            }
                        } else {
                            for i in 0..n {
                                for c in 0..cu {
                                    out.push(blocks[c * bu + i]);
                                }
                            }
                        }
                    }
                    g += k;
                    if g == groups && tail_mask != 0xFF {
                        let at = out.len().saturating_sub(cu);
                        for x in &mut out[at..] {
                            *x &= tail_mask;
                        }
                    }
                    if !sink(&out) {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    /// The bytes of the ID3 tag, if there is one and it fits in [`MAX_TAG`]; `Err` with
    /// its size if it is larger.
    pub fn read_id3(&self, src: &mut dyn Source) -> Result<Option<Vec<u8>>, u64> {
        let Some((at, avail)) = self.tags.id3 else { return Ok(None) };
        let mut h = [0u8; 10];
        if avail < 10 || src.read_at(at, &mut h).is_err() || &h[0..3] != b"ID3" {
            return Ok(None);
        }
        let size = h[6..10].iter().fold(0u64, |a, &b| (a << 7) | u64::from(b & 0x7F));
        // A footer (ID3v2.4 flag 0x10) adds 10 bytes.
        let total = (10 + size + if h[5] & 0x10 != 0 { 10 } else { 0 }).min(avail);
        if total > MAX_TAG {
            return Err(total);
        }
        let mut b = vec![0u8; total as usize];
        if src.read_at(at, &mut b).is_err() {
            return Ok(None);
        }
        Ok(Some(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    /// A DSF file of `ch` channels whose canonical stream is `canon` (MSB-first,
    /// interleaved), with blocks of `block` bytes and `tag` after the audio.
    pub(crate) fn dsf(canon: &[u8], ch: usize, block: usize, lsb: bool, tag: &[u8]) -> Vec<u8> {
        let bpc = canon.len() / ch;
        let groups = bpc.div_ceil(block);
        let mut data = vec![0u8; groups * block * ch];
        for i in 0..bpc {
            for c in 0..ch {
                let b = canon[i * ch + c];
                let (g, o) = (i / block, i % block);
                data[g * block * ch + c * block + o] = if lsb { b.reverse_bits() } else { b };
            }
        }
        let mut f = Vec::new();
        let total = 28 + 52 + 12 + data.len() + tag.len();
        f.extend_from_slice(b"DSD ");
        f.extend_from_slice(&28u64.to_le_bytes());
        f.extend_from_slice(&(total as u64).to_le_bytes());
        let tag_at = if tag.is_empty() { 0 } else { (28 + 52 + 12 + data.len()) as u64 };
        f.extend_from_slice(&tag_at.to_le_bytes());
        f.extend_from_slice(b"fmt ");
        f.extend_from_slice(&52u64.to_le_bytes());
        for v in [1u32, 0, 2, ch as u32, 2_822_400, if lsb { 1 } else { 8 }] {
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&((bpc * 8) as u64).to_le_bytes());
        f.extend_from_slice(&(block as u32).to_le_bytes());
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(b"data");
        f.extend_from_slice(&((12 + data.len()) as u64).to_le_bytes());
        f.extend_from_slice(&data);
        f.extend_from_slice(tag);
        f
    }

    fn canon_of(file: &[u8]) -> Vec<u8> {
        let mut s: &[u8] = file;
        let a = parse(&mut s).unwrap();
        let mut out = Vec::new();
        a.canonical(&mut s, &AtomicU64::new(0), &mut |b| out.extend_from_slice(b)).unwrap();
        out
    }

    fn sample(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + i / 13) as u8).collect()
    }

    #[test]
    fn dsf_round_trips_any_block_and_channels() {
        for ch in 1..=6 {
            for block in [1usize, 7, 4096] {
                let canon = sample(ch * 9001);
                assert_eq!(canon_of(&dsf(&canon, ch, block, true, b"")), canon, "ch {ch} block {block}");
                assert_eq!(canon_of(&dsf(&canon, ch, block, false, b"ID3")), canon);
            }
        }
    }

    #[test]
    fn dsf_without_final_padding_reads() {
        let canon = sample(2 * 5000);
        let mut f = dsf(&canon, 2, 4096, true, b"");
        // 5000 bytes per channel: the right channel's second block holds 904 of them, then
        // 3192 bytes of padding, which some writers leave out at the end of the file.
        f.truncate(f.len() - (4096 - 904));
        assert_eq!(canon_of(&f), canon);
    }

    #[test]
    fn truncated_and_garbage_files_are_errors_not_panics() {
        let canon = sample(2 * 10_000);
        let f = dsf(&canon, 2, 4096, true, b"");
        // The right channel's last byte is the last one needed: what follows is padding.
        let last_needed = 92 + 2 * 8192 + 4096 + (10_000 - 8192);
        for cut in [0, 3, 4, 27, 28, 60, 91, 92, 100, 5000, last_needed - 1] {
            let mut s: &[u8] = &f[..cut];
            assert!(parse(&mut s).is_err(), "cut at {cut}");
        }
        // Every byte of the headers set to every interesting value.
        for at in 0..92 {
            for v in [0u8, 1, 0x7F, 0x80, 0xFF] {
                let mut g = f.clone();
                g[at] = v;
                let mut s: &[u8] = &g;
                if let Ok(a) = parse(&mut s) {
                    let _ = a.digest(&mut s, &AtomicU64::new(0));
                    let _ = a.read_id3(&mut s);
                }
            }
        }
    }

    pub(crate) fn dff(canon: &[u8], ch: u16, cmpr: &[u8; 4], extra: &[u8]) -> Vec<u8> {
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
            chnl.extend_from_slice(&[b'C', b'H', b'0', b'0' + i as u8]);
        }
        prop.extend(chunk(b"CHNL", &chnl));
        let mut c = cmpr.to_vec();
        c.extend_from_slice(b"\x0eNot Compressed\x00");
        prop.extend(chunk(b"CMPR", &c));
        let mut form = b"DSD ".to_vec();
        form.extend(chunk(b"FVER", &0x0105_0000u32.to_be_bytes()));
        form.extend(chunk(b"PROP", &prop));
        form.extend(chunk(if cmpr == b"DST " { b"DST " } else { b"DSD " }, canon));
        form.extend_from_slice(extra);
        chunk(b"FRM8", &form)
    }

    #[test]
    fn dff_reads_and_dst_is_unsupported() {
        let canon = sample(6 * 777);
        assert_eq!(canon_of(&dff(&canon, 6, b"DSD ", b"")), canon);
        let f = dff(&canon, 2, b"DST ", b"");
        let mut s: &[u8] = &f;
        assert!(matches!(parse(&mut s), Err(Problem::Unsupported(_))));
        // Truncated anywhere.
        let f = dff(&canon, 2, b"DSD ", b"");
        for cut in 0..200 {
            let mut s: &[u8] = &f[..cut];
            assert!(parse(&mut s).is_err());
        }
        let mut s: &[u8] = &f[..f.len() - 1];
        assert!(parse(&mut s).is_err());
    }

    #[test]
    fn dff_text_chunks_are_read() {
        let mut diin = Vec::new();
        let mut text = |id: &[u8], s: &str| {
            let mut body = (s.len() as u32).to_be_bytes().to_vec();
            body.extend_from_slice(s.as_bytes());
            diin.extend_from_slice(id);
            diin.extend_from_slice(&(body.len() as u64).to_be_bytes());
            diin.extend_from_slice(&body);
            if body.len() % 2 == 1 {
                diin.push(0);
            }
        };
        text(b"DITI", "Title");
        text(b"DIAR", "Artist");
        text(b"EMID", "x");
        let mut extra = b"DIIN".to_vec();
        extra.extend_from_slice(&(diin.len() as u64).to_be_bytes());
        extra.extend_from_slice(&diin);
        let f = dff(&sample(20), 2, b"DSD ", &extra);
        let mut s: &[u8] = &f;
        let a = parse(&mut s).unwrap();
        assert_eq!(a.tags.dff_title.as_deref(), Some("Title"));
        assert_eq!(a.tags.dff_artist.as_deref(), Some("Artist"));
        assert_eq!(a.tags.dff_other, vec!["DIIN/EMID".to_owned()]);
    }
}
