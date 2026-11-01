// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading the frames of an ID3v2 tag (versions 2.2, 2.3 and 2.4), to tell what an
//! extracted file's tag holds that sacdfs's own tag for the same track does not.
//!
//! Written from the ID3v2 specifications (id3.org: "ID3 tag version 2", "ID3 tag version
//! 2.3.0", "ID3 tag version 2.4.0 - Main Structure" and "- Native Frames"). Nothing is
//! interpreted beyond what the comparison needs: a frame is a key (its id, plus the
//! description for TXXX, COMM and UFID) and a value (its text, or a hash of its bytes).
//! Every tag is untrusted; sizes are checked against the bytes there are.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

/// One frame: its key and its value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Frame {
    /// The frame id (ID3v2.2 ids mapped to their v2.3 names), with `:description` for TXXX,
    /// COMM and UFID.
    pub key: String,
    /// The text, values joined with "; ", or `#` and the SHA-256 of a binary frame's bytes.
    pub value: String,
}

/// Most frames read from one tag.
const MAX_FRAMES: usize = 4096;

/// The frames of `tag` (the whole tag, from its "ID3" header). Whatever can't be parsed
/// ends the list; nothing fails.
pub fn frames(tag: &[u8]) -> Vec<Frame> {
    let mut out = Vec::new();
    if tag.len() < 10 || &tag[0..3] != b"ID3" {
        return out;
    }
    let (major, flags) = (tag[3], tag[5]);
    if !(2..=4).contains(&major) {
        return out;
    }
    let size = syncsafe(&tag[6..10]) as usize;
    let end = (10 + size).min(tag.len());
    let mut body = tag[10..end].to_vec();
    if flags & 0x80 != 0 && major < 4 {
        body = deunsync(&body);
    }
    let mut at = 0usize;
    if flags & 0x40 != 0 && major >= 3 {
        let ext = match major {
            3 => be(&body, 0, 4).map(|n| n + 4),
            _ => body.get(0..4).map(syncsafe),
        };
        match ext {
            Some(n) => at = n as usize,
            None => return out,
        }
    }
    let (id_len, head_len) = if major == 2 { (3, 6) } else { (4, 10) };
    while out.len() < MAX_FRAMES {
        let Some(h) = body.get(at..at + head_len) else { break };
        if h[0] == 0 {
            break; // padding
        }
        let id = &h[..id_len];
        if !id.iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
            break;
        }
        let size = match major {
            2 => be(h, 3, 3),
            3 => be(h, 4, 4),
            _ => Some(syncsafe(&h[4..8])),
        };
        let Some(size) = size.map(|s| s as usize) else { break };
        let format_flags = if major >= 3 { h[9] } else { 0 };
        let start = at + head_len;
        let Some(data) = start.checked_add(size).and_then(|e| body.get(start..e)) else { break };
        at = start + size;
        let id = std::str::from_utf8(id).unwrap_or("????");
        let id = if major == 2 { v22_id(id) } else { id.to_owned() };
        if let Some(f) = frame(&id, data, major, format_flags, flags & 0x80 != 0) {
            out.push(f);
        }
    }
    out
}

fn frame(id: &str, data: &[u8], major: u8, format_flags: u8, tag_unsync: bool) -> Option<Frame> {
    let mut data = data.to_vec();
    let mut opaque = false;
    match major {
        3 => {
            // Compressed or encrypted: kept as bytes. Grouping adds a byte.
            if format_flags & 0xC0 != 0 {
                opaque = true;
            } else if format_flags & 0x20 != 0 {
                data = data.get(1..)?.to_vec();
            }
        }
        4 => {
            if format_flags & 0x40 != 0 {
                data = data.get(1..)?.to_vec();
            }
            if format_flags & 0x01 != 0 {
                data = data.get(4..)?.to_vec();
            }
            if format_flags & 0x02 != 0 || tag_unsync {
                data = deunsync(&data);
            }
            if format_flags & 0x0C != 0 {
                opaque = true;
            }
        }
        _ => {}
    }
    let hashed = |d: &[u8]| {
        let h: [u8; 32] = Sha256::digest(d).into();
        format!("#{}", hex(&h))
    };
    if opaque {
        return Some(Frame { key: id.to_owned(), value: hashed(&data) });
    }
    let (key, value) = match id {
        "TXXX" => {
            let (&enc, rest) = data.split_first()?;
            let mut v = texts(enc, rest);
            let desc = if v.is_empty() { String::new() } else { v.remove(0) };
            (format!("TXXX:{desc}"), join(v))
        }
        "COMM" | "USLT" => {
            let (&enc, rest) = data.split_first()?;
            let mut v = texts(enc, rest.get(3..).unwrap_or(&[]));
            let desc = if v.is_empty() { String::new() } else { v.remove(0) };
            (format!("{id}:{desc}"), join(v))
        }
        "UFID" => {
            let nul = data.iter().position(|&b| b == 0).unwrap_or(data.len());
            let owner = String::from_utf8_lossy(&data[..nul]).into_owned();
            let ident = data.get(nul + 1..).unwrap_or(&[]);
            (format!("UFID:{owner}"), String::from_utf8_lossy(ident).trim_end_matches('\0').to_owned())
        }
        _ if id.starts_with('T') => {
            let (&enc, rest) = data.split_first()?;
            let key = if id == "TYER" { "TDRC".to_owned() } else { id.to_owned() };
            (key, join(texts(enc, rest)))
        }
        _ => (id.to_owned(), hashed(&data)),
    };
    Some(Frame { key, value })
}

/// The values of a text field in encoding `enc`, split at terminators, trimmed.
fn texts(enc: u8, b: &[u8]) -> Vec<String> {
    let parts: Vec<String> = match enc {
        1 | 2 => {
            let units: Vec<u16> = b
                .chunks_exact(2)
                .map(|c| {
                    if enc == 2 { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }
                })
                .collect();
            units
                .split(|&u| u == 0)
                .map(|s| {
                    // A byte order mark per value (encoding 1); little-endian without one.
                    let (s, swap) = match s.first() {
                        Some(0xFEFF) => (&s[1..], false),
                        Some(0xFFFE) => (&s[1..], true),
                        _ => (s, false),
                    };
                    let s: Vec<u16> =
                        if swap { s.iter().map(|u| u.swap_bytes()).collect() } else { s.to_vec() };
                    String::from_utf16_lossy(&s)
                })
                .collect()
        }
        3 => b.split(|&c| c == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect(),
        _ => b.split(|&c| c == 0).map(|s| s.iter().map(|&c| char::from(c)).collect()).collect(),
    };
    let mut v: Vec<String> = parts.into_iter().map(|s| s.trim().to_owned()).collect();
    // A trailing terminator leaves an empty last part; a description may be empty.
    while v.len() > 1 && v.last().is_some_and(String::is_empty) {
        v.pop();
    }
    v
}

fn join(v: Vec<String>) -> String {
    v.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("; ")
}

fn syncsafe(b: &[u8]) -> u32 {
    b.iter().take(4).fold(0u32, |a, &x| (a << 7) | u32::from(x & 0x7F))
}

fn be(b: &[u8], at: usize, n: usize) -> Option<u32> {
    Some(b.get(at..at + n)?.iter().fold(0u32, |a, &x| (a << 8) | u32::from(x)))
}

/// Undoes unsynchronisation: `FF 00` back to `FF`.
fn deunsync(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut prev_ff = false;
    for &x in b {
        if prev_ff && x == 0 {
            prev_ff = false;
            continue;
        }
        out.push(x);
        prev_ff = x == 0xFF;
    }
    out
}

/// The v2.3 id of a v2.2 frame id, for the frames sacdfs writes or the comparison
/// classifies; others keep their three letters.
fn v22_id(id: &str) -> String {
    let m = match id {
        "TT2" => "TIT2",
        "TP1" => "TPE1",
        "TP2" => "TPE2",
        "TAL" => "TALB",
        "TRK" => "TRCK",
        "TPA" => "TPOS",
        "TCM" => "TCOM",
        "TYE" => "TDRC",
        "TRC" => "TSRC",
        "TCO" => "TCON",
        "TPB" => "TPUB",
        "TCR" => "TCOP",
        "TXX" => "TXXX",
        "COM" => "COMM",
        "PIC" => "APIC",
        "UFI" => "UFID",
        "RVA" => "RVA2",
        other => other,
    };
    m.to_owned()
}

pub(crate) fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// What deleting a file would lose: its tag's frames that sacdfs's tag for the same track
/// lacks, by kind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Losses {
    /// MusicBrainz identifiers (TXXX "MusicBrainz ...", UFID of musicbrainz.org).
    pub musicbrainz: bool,
    /// Embedded pictures (APIC).
    pub art: bool,
    /// ReplayGain values (TXXX "replaygain_...", RVA2, RGAD).
    pub replaygain: bool,
    /// Every other frame key sacdfs's tag lacks or holds another value for.
    pub other: BTreeSet<String>,
    /// A tag that was not read, and why.
    pub unread: Option<String>,
}

impl Losses {
    /// Classifies the frames of `extract` that `served` does not have with the same value.
    pub fn between(extract: &[Frame], served: &[Frame]) -> Losses {
        let have: BTreeSet<(&str, &str)> =
            served.iter().map(|f| (f.key.as_str(), f.value.as_str())).collect();
        let served_value = |key: &str| served.iter().find(|f| f.key == key).map(|f| f.value.as_str());
        // ID3v2.3's date is TYER (mapped to TDRC) plus TDAT (DDMM): one date, as in v2.4.
        let tdat = extract.iter().find(|f| f.key == "TDAT").map(|f| f.value.as_str());
        let mut l = Losses::default();
        for f in extract {
            let value = f.value.as_str();
            if value.is_empty() || have.contains(&(f.key.as_str(), value)) {
                continue;
            }
            // The same information written another way: not lost.
            let same = match f.key.as_str() {
                "TDRC" | "TDAT" => {
                    let year = extract.iter().find(|g| g.key == "TDRC").map(|g| g.value.as_str());
                    let date = match (year, tdat) {
                        (Some(y), Some(d)) if d.len() == 4 && d.bytes().all(|b| b.is_ascii_digit()) => {
                            format!("{y}-{}-{}", &d[2..4], &d[0..2])
                        }
                        (Some(y), _) => y.to_owned(),
                        (None, _) => String::new(),
                    };
                    served_value("TDRC").is_some_and(|s| !date.is_empty() && s.starts_with(&date))
                }
                // A set of one disc, which sacdfs leaves out.
                "TPOS" => value == "1/1" && served_value("TPOS").is_none(),
                "TXXX:PERFORMER" => served_value("TPE1") == Some(value),
                _ => false,
            };
            if !same {
                l.add(&f.key);
            }
        }
        l
    }

    /// Records the loss of a frame with `key`.
    pub fn add(&mut self, key: &str) {
        let lower = key.to_ascii_lowercase();
        if lower.starts_with("txxx:musicbrainz") || lower.starts_with("ufid:http://musicbrainz.org") {
            self.musicbrainz = true;
        } else if key == "APIC" {
            self.art = true;
        } else if lower.starts_with("txxx:replaygain") || key == "RVA2" || key == "RGAD" {
            self.replaygain = true;
        } else {
            self.other.insert(key.to_owned());
        }
    }

    /// Whether nothing would be lost.
    pub fn is_empty(&self) -> bool {
        !self.musicbrainz && !self.art && !self.replaygain && self.other.is_empty() && self.unread.is_none()
    }

    /// The report's form: `musicbrainz,art,replaygain,other=KEY|KEY,unread=why`, or empty.
    pub fn render(&self) -> String {
        let mut parts = Vec::new();
        if self.musicbrainz {
            parts.push("musicbrainz".to_owned());
        }
        if self.art {
            parts.push("art".to_owned());
        }
        if self.replaygain {
            parts.push("replaygain".to_owned());
        }
        if !self.other.is_empty() {
            let keys: Vec<String> = self.other.iter().map(|k| k.replace([',', '|'], "_")).collect();
            parts.push(format!("other={}", keys.join("|")));
        }
        if let Some(u) = &self.unread {
            parts.push(format!("unread={}", u.replace(',', ";")));
        }
        parts.join(",")
    }

    /// Parses [`Losses::render`]'s form (a resumed report).
    pub fn parse(s: &str) -> Losses {
        let mut l = Losses::default();
        for p in s.split(',').filter(|p| !p.is_empty()) {
            match p {
                "musicbrainz" => l.musicbrainz = true,
                "art" => l.art = true,
                "replaygain" => l.replaygain = true,
                _ => {
                    if let Some(k) = p.strip_prefix("other=") {
                        l.other.extend(k.split('|').map(str::to_owned));
                    } else if let Some(u) = p.strip_prefix("unread=") {
                        l.unread = Some(u.to_owned());
                    }
                }
            }
        }
        l
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An ID3v2.4 tag of UTF-8 frames (TXXX as `TXXX:desc`) and raw frames.
    pub(crate) fn tag24(text: &[(&str, &str)], raw: &[(&str, &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        let mut put = |id: &str, data: &[u8]| {
            body.extend_from_slice(id.as_bytes());
            body.extend_from_slice(&crate::dsf::syncsafe(data.len() as u32));
            body.extend_from_slice(&[0, 0]);
            body.extend_from_slice(data);
        };
        for (k, v) in text {
            let mut d = vec![3u8];
            match k.strip_prefix("TXXX:") {
                Some(desc) => {
                    d.extend_from_slice(desc.as_bytes());
                    d.push(0);
                    d.extend_from_slice(v.as_bytes());
                    put("TXXX", &d);
                }
                None => {
                    d.extend_from_slice(v.as_bytes());
                    put(k, &d);
                }
            }
        }
        for (k, d) in raw {
            put(k, d);
        }
        body.extend_from_slice(&[0; 16]); // padding
        let mut t = b"ID3\x04\x00\x00".to_vec();
        t.extend_from_slice(&crate::dsf::syncsafe(body.len() as u32));
        t.extend_from_slice(&body);
        t
    }

    #[test]
    fn reads_what_sacdfs_writes() {
        let mut tags = crate::dsf::Tags::default();
        tags.set(b"TIT2", Some("Title"));
        tags.set(b"TRCK", Some("1/9"));
        let f = frames(&tags.to_id3());
        assert_eq!(
            f,
            vec![
                Frame { key: "TIT2".into(), value: "Title".into() },
                Frame { key: "TRCK".into(), value: "1/9".into() }
            ]
        );
        assert!(Losses::between(&f, &f).is_empty());
    }

    #[test]
    fn classifies_losses() {
        let served = frames(&tag24(&[("TIT2", "Title")], &[]));
        let ext = frames(&tag24(
            &[
                ("TIT2", "Title"),
                ("TXXX:MusicBrainz Album Id", "abc"),
                ("TXXX:REPLAYGAIN_TRACK_GAIN", "-3 dB"),
                ("TCON", "Jazz"),
            ],
            &[("APIC", b"\x00image/jpeg\x00\x03\x00jpegdata")],
        ));
        let l = Losses::between(&ext, &served);
        assert!(l.musicbrainz && l.art && l.replaygain);
        assert_eq!(l.other.iter().collect::<Vec<_>>(), vec!["TCON"]);
        assert_eq!(Losses::parse(&l.render()), l);
    }

    #[test]
    fn same_information_written_another_way_is_not_lost() {
        let served = frames(&tag24(&[("TPE1", "A"), ("TDRC", "2013-04-23")], &[]));
        let ext = frames(&tag24(
            &[("TYER", "2013"), ("TDAT", "2304"), ("TPOS", "1/1"), ("TXXX:PERFORMER", "A"), ("TSRC", "")],
            &[],
        ));
        assert!(Losses::between(&ext, &served).is_empty(), "{:?}", Losses::between(&ext, &served));
        let ext = frames(&tag24(&[("TYER", "2014"), ("TPOS", "1/2"), ("TXXX:PERFORMER", "B")], &[]));
        let l = Losses::between(&ext, &served);
        assert_eq!(l.other.into_iter().collect::<Vec<_>>(), vec!["TDRC", "TPOS", "TXXX:PERFORMER"]);
    }

    #[test]
    fn utf16_and_v23_and_v22() {
        // v2.3, UTF-16 with BOM, plain sizes.
        let mut d = vec![1u8, 0xFF, 0xFE];
        for u in "Title".encode_utf16() {
            d.extend_from_slice(&u.to_le_bytes());
        }
        let mut body = b"TIT2".to_vec();
        body.extend_from_slice(&(d.len() as u32).to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&d);
        let mut t = b"ID3\x03\x00\x00".to_vec();
        t.extend_from_slice(&crate::dsf::syncsafe(body.len() as u32));
        t.extend_from_slice(&body);
        assert_eq!(frames(&t), vec![Frame { key: "TIT2".into(), value: "Title".into() }]);
        // v2.2.
        let mut t = b"ID3\x02\x00\x00\x00\x00\x00\x0a".to_vec();
        t.extend_from_slice(b"TT2\x00\x00\x04\x00abc");
        assert_eq!(frames(&t), vec![Frame { key: "TIT2".into(), value: "abc".into() }]);
    }

    #[test]
    fn hostile_tags_do_not_panic() {
        let base = tag24(&[("TIT2", "Title"), ("TXXX:a", "b")], &[("APIC", b"xx")]);
        for i in 0..base.len() {
            for v in [0u8, 0x7F, 0x80, 0xFF, 1, 2, 3, 4] {
                let mut t = base.clone();
                t[i] = v;
                let _ = frames(&t);
                let _ = frames(&t[..i]);
            }
        }
    }
}
