// SPDX-License-Identifier: GPL-3.0-or-later

//! The ID3, DSF and WAV writers fed any tag text and any file shape: tags and chunks stay
//! well formed, and a header read in pieces is the header read whole.
//!
//! Input: 16 bytes of file shape, then tag values separated by [`SEP`](sacdfs_fuzz::SEP): one
//! per tag item in the order of `ITEMS`, the rest as `TXXX` descriptions and values.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sacdfs::tags::{ITEMS, Key, Overrides, append_frames};
use sacdfs::wav::{self, Layout};
use sacdfs::{dsf, verify::id3};
use sacdfs_fuzz::{SEP, check_chunks, check_tag, valid_toc};
use scarletbook::LoudspeakerLayout;
use scarletbook::output::{Format, Layout as LibLayout, RegionKind, TagSet, Tags};

/// The samples per frame of the PCM rates sacdfs offers.
const SAMPLES_PER_FRAME: [u32; 3] = [1176, 2352, 4704];

fuzz_target!(|data: &[u8]| {
    let Some((shape, values)) = data.split_first_chunk::<16>() else { return };
    let text: Vec<String> =
        values.split(|&b| b == SEP).map(|v| String::from_utf8_lossy(v).into_owned()).collect();

    let mut set = TagSet::new();
    let mut o = Overrides::default();
    for ((_, id, item), value) in ITEMS.iter().zip(&text) {
        if shape[0] & 1 == 0 {
            set.set(*item, value);
        } else {
            set.push(*item, value);
        }
        // As a tags file would change it, and as a frame a tags file appends.
        if let Some(key) = Key::of(id) {
            o.set.push((key, value.clone()));
        }
    }
    for pair in text.get(ITEMS.len()..).unwrap_or(&[]).chunks(2) {
        if let [d, v] = pair {
            set.set_custom(d, v);
            o.set.push((Key::Txxx(d.clone()), v.clone()));
            o.frames.push((*b"COMM", Some(d.clone()), v.clone()));
            o.frames.push((*b"UFID", Some(d.clone()), v.clone()));
            o.frames.push((*b"TBPM", None, v.clone()));
        }
    }
    o.apply(&mut set);

    // A DSF file of that tag, in a real area.
    let toc = valid_toc();
    let mut id3_tag = Vec::new();
    if let Some(area) = toc.areas.first() {
        if let Some(track) = area.tracks.first() {
            let options = dsf::options(area, track);
            let make = |tags: Tags| {
                LibLayout::new(
                    toc,
                    area.slot,
                    track.start_frame,
                    track.end_frame(),
                    Format::Dsf,
                    &options.clone().with_tags(tags),
                )
            };
            if let Ok(l) = make(Tags::Set(set.clone())) {
                id3_tag = dsf::region(&l, RegionKind::Trailer);
                check_tag(&id3_tag);
                assert!(!dsf::region(&l, RegionKind::Header).is_empty());
                if let Some(raw) = append_frames(&id3_tag, &o) {
                    check_tag(&raw);
                    let _ = id3::frames(&raw);
                    if let Ok(l) = make(Tags::Raw(raw.clone())) {
                        assert_eq!(dsf::region(&l, RegionKind::Trailer), raw);
                    }
                }
            }
        }
    }

    // A WAV file of that tag, with any number of frames.
    let trailer = wav::trailer(&set, &id3_tag);
    check_chunks(&trailer.bytes);
    assert_eq!(&trailer.bytes[trailer.tag.clone()], &id3_tag[..]);
    let u32_at = |at: usize| u32::from_le_bytes([shape[at], shape[at + 1], shape[at + 2], shape[at + 3]]);
    let samples_per_frame = SAMPLES_PER_FRAME[usize::from(shape[1] % 3)];
    let layout = Layout {
        channels: shape[2] % 6 + 1,
        frames: u32_at(4),
        samples_per_frame,
        rate: samples_per_frame * 75,
    };
    let speakers = match layout.channels {
        2 => LoudspeakerLayout::Stereo,
        5 => LoudspeakerLayout::ThreeTwo,
        6 => LoudspeakerLayout::FivePointOne,
        channels => LoudspeakerLayout::Unspecified { channels, config: shape[3] },
    };
    let header = wav::header_parts(&layout, speakers, trailer.bytes.len() as u64);
    assert_eq!(header.len(), wav::header_len(&layout));
    assert!(header.len() >= wav::AUDIO_START_MIN);
    assert_eq!((header.len() + layout.data_len()) % wav::PAGE, 0);
    let whole = header.to_vec();
    assert_eq!(whole.len() as u64, header.len());
    assert_eq!(wav::needs_rf64(&layout, trailer.bytes.len() as u64), whole.starts_with(b"RF64"));

    // The header in pieces.
    let (a, b) = (u64::from(u32_at(8)) % (header.len() + 1), u64::from(u32_at(12)) % (header.len() + 1));
    let (start, end) = (a.min(b), a.max(b));
    let mut piece = Vec::new();
    header.extend_into(start, end, &mut piece);
    assert_eq!(piece, &whole[start as usize..end as usize]);

    // Frames for a byte range stay inside the file.
    let (first, last) = layout.frames_for(start, end.saturating_mul(u64::from(u32_at(8))));
    assert!(first <= last && last <= layout.frames, "{first}..{last} of {}", layout.frames);
});
