// SPDX-License-Identifier: GPL-3.0-or-later

//! The crashes and wrong results the fuzz targets found (`fuzz/`, #161), each as a small
//! input that shows it. These run without a fuzzer, on every pull request.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use sacdfs::names::{Deduper, NAME_MAX};
use sacdfs::tags_cli::front_cover;
use sacdfs::verify::container::{Problem, parse};
use sacdfs::verify::id3::{Losses, frames};

/// An ID3v2.3 tag whose extended header claims 2^32 - 1 bytes: adding the four bytes of its
/// size field overflowed, which panics in a release build too (`overflow-checks`).
#[test]
fn id3v23_extended_header_of_the_largest_size() {
    let mut tag = b"ID3\x03\x00\x40\x00\x00\x00\x10".to_vec();
    tag.extend_from_slice(&[0xFF; 4]);
    tag.extend_from_slice(b"TIT2\x00\x00\x00\x02\x00\x00\x00a");
    assert_eq!(frames(&tag), []);
    assert_eq!(front_cover(&tag), None);
}

/// A repeated name whose extension leaves no room for the ` (2)`: the unique name passed
/// NAME_MAX, which the kernel refuses to look up.
#[test]
fn deduped_name_with_a_long_extension_stays_within_name_max() {
    for (stem, ext_len) in [(&b"a"[..], 253), (b"a", 245), (b"a", 241), (b"a", 240), (&[b'a'; 200], 54)] {
        let mut name = stem.to_vec();
        name.push(b'.');
        name.extend(std::iter::repeat_n(b'b', ext_len));
        assert!(name.len() <= NAME_MAX);
        let mut d = Deduper::default();
        let name = OsStr::from_bytes(&name);
        assert_eq!(d.unique(name, true), name);
        for _ in 0..3 {
            let unique = d.unique(name, true);
            assert!(unique.len() <= NAME_MAX, "{} bytes", unique.len());
            assert_ne!(unique, name);
        }
    }
}

/// The same with bytes that aren't UTF-8, each of which takes three bytes once made text.
#[test]
fn deduped_name_of_invalid_utf8_stays_within_name_max() {
    let mut name = b"a.".to_vec();
    name.extend_from_slice(&[0xFF; 253]);
    let mut d = Deduper::default();
    let name = OsStr::from_bytes(&name);
    d.unique(name, true);
    assert!(d.unique(name, true).len() <= NAME_MAX);
}

/// A DFF file whose sound chunk claims 2^61 bytes of one channel or more: its sample count
/// overflowed on the way to being compared with the file's length.
#[test]
fn dff_sound_chunk_of_an_absurd_size() {
    fn chunk(out: &mut Vec<u8>, id: &[u8; 4], size: u64, body: &[u8]) {
        out.extend_from_slice(id);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(body);
    }
    for size in [1u64 << 61, 1 << 62, u64::MAX - 1] {
        let mut snd = b"SND ".to_vec();
        chunk(&mut snd, b"FS  ", 4, &2_822_400u32.to_be_bytes());
        chunk(&mut snd, b"CHNL", 2, &1u16.to_be_bytes());
        let mut form = b"DSD ".to_vec();
        chunk(&mut form, b"PROP", snd.len() as u64, &snd);
        chunk(&mut form, b"DSD ", size, &[0; 16]);
        let mut file = Vec::new();
        chunk(&mut file, b"FRM8", form.len() as u64, &form);
        assert!(matches!(parse(&mut &file[..]), Err(Problem::Error(_))), "{size}");
    }
}

/// Frame keys with the report's separators in them: written sorted by their original form, they
/// came back in another order, so a resumed report's line differed from the first run's.
#[test]
fn losses_read_back_from_a_report_are_written_the_same() {
    let mut l = Losses::default();
    l.add("UFID:,a");
    l.add("UFID:Ta");
    l.add("UFID:|a");
    let shown = l.render();
    assert_eq!(shown, "other=UFID:Ta|UFID:_a");
    assert_eq!(Losses::parse(&shown).render(), shown);
}
