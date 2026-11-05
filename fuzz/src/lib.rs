// SPDX-License-Identifier: GPL-3.0-or-later

//! What the fuzz targets share: splitting an input into fields, a disc image from a TOC
//! region held in memory, and the checks that every tag and header must pass.
//!
//! A panic here is what the fuzzers look for, so the checks use plain `assert!`.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::OnceLock;

use sacdfs::verify::id3;
use scarletbook::{Disc, MASTER_TOC_LSN, SECTOR_SIZE, Toc};

/// The byte that separates the fields of an input: never part of valid UTF-8, so that it
/// does not get in the way of text fields.
pub const SEP: u8 = 0xFE;

/// The fields of `data`, split at [`SEP`]; missing fields are empty.
pub fn fields<const N: usize>(data: &[u8]) -> [&[u8]; N] {
    let mut out = [&data[..0]; N];
    let mut parts = data.splitn(N, |&b| b == SEP);
    for slot in &mut out {
        *slot = parts.next().unwrap_or(&[]);
    }
    out
}

/// Bytes of an image before its master TOC.
pub const LEAD_IN: u64 = MASTER_TOC_LSN as u64 * SECTOR_SIZE as u64;

/// A 2048-byte-sector image: zeros up to the master TOC, then `tail`. What the targets take
/// as input is `tail`, the part of an image that holds its TOCs and text, without the lead-in
/// that every input would have to repeat.
#[derive(Debug)]
pub struct TailImage<'a> {
    tail: &'a [u8],
    pos: u64,
}

impl<'a> TailImage<'a> {
    /// The image with this tail.
    pub fn new(tail: &'a [u8]) -> Self {
        TailImage { tail, pos: 0 }
    }
}

impl Read for TailImage<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = if self.pos < LEAD_IN {
            let n = buf.len().min((LEAD_IN - self.pos) as usize);
            buf[..n].fill(0);
            n
        } else {
            let at = usize::try_from(self.pos - LEAD_IN).unwrap_or(usize::MAX).min(self.tail.len());
            let src = &self.tail[at..];
            let n = buf.len().min(src.len());
            buf[..n].copy_from_slice(&src[..n]);
            n
        };
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for TailImage<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let len = LEAD_IN + self.tail.len() as u64;
        let target = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = target.ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        Ok(self.pos)
    }
}

/// A valid disc with stereo and multichannel areas, for the targets whose input is not a disc.
pub fn valid_toc() -> &'static Toc {
    static TOC: OnceLock<Toc> = OnceLock::new();
    TOC.get_or_init(|| {
        let image = include_bytes!("../../tests/corpus/images/dsd-stereo-mch-2048.iso");
        match Disc::open(io::Cursor::new(&image[..])) {
            Ok(d) => d.toc().clone(),
            Err(e) => panic!("the corpus disc does not open: {e}"),
        }
    })
}

/// A new, empty directory for this process, under the system's temporary directory.
pub fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sacdfs-fuzz-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        panic!("{}: {e}", dir.display());
    }
    dir
}

/// What every ID3v2.4 tag sacdfs writes must be: empty, or a header with its true size and
/// frames that the `verify` reader reads back.
pub fn check_tag(tag: &[u8]) {
    if tag.is_empty() {
        return;
    }
    assert!(tag.starts_with(b"ID3\x04"), "not an ID3v2.4 header");
    assert!(tag.len() >= 10, "a tag shorter than its header");
    let size = tag[6..10].iter().fold(0u64, |a, &b| {
        assert!(b < 0x80, "a size that is not syncsafe");
        (a << 7) | u64::from(b)
    });
    assert_eq!(size + 10, tag.len() as u64, "the size in the header");
    let _ = id3::frames(tag);
}

/// A RIFF chunk walk: the chunks of `bytes` lie inside it, with their pad bytes.
pub fn check_chunks(mut bytes: &[u8]) {
    while !bytes.is_empty() {
        assert!(bytes.len() >= 8, "a chunk header cut short");
        let size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let total = 8 + size + (size & 1);
        assert!(total <= bytes.len(), "a chunk runs past the end");
        bytes = &bytes[total..];
    }
}

/// What every name in the mount must be: one path component the kernel accepts.
pub fn check_name(name: &[u8]) {
    assert!(!name.is_empty(), "an empty name");
    assert!(name != b"." && name != b"..", "a name that means another directory");
    assert!(!name.contains(&b'/') && !name.contains(&0), "a name with / or NUL");
    assert!(name.len() <= sacdfs::names::NAME_MAX, "a name of {} bytes", name.len());
}
