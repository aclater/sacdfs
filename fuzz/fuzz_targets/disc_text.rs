// SPDX-License-Identifier: GPL-3.0-or-later

//! Hostile disc text, from the TOC parser to what sacdfs builds from it: the file names, the
//! ID3 tag and header of a DSF file, and the header and chunks of a WAV file.
//!
//! Input: an image's bytes from the master TOC on ([`TailImage`]).

#![no_main]

use libfuzzer_sys::fuzz_target;
use sacdfs::names::{Deduper, track_file_name};
use sacdfs::wav::{self, Layout};
use sacdfs::{dsf, tags};
use sacdfs_fuzz::{TailImage, check_chunks, check_name, check_tag};
use scarletbook::Disc;
use scarletbook::output::RegionKind;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

fuzz_target!(|data: &[u8]| {
    let Ok(disc) = Disc::open(TailImage::new(data)) else { return };
    let toc = disc.toc();
    let _ = tags::disc_key(toc);
    for area in &toc.areas {
        let mut dedup = Deduper::with_reserved([".", ".."]);
        for track in &area.tracks {
            for ext in ["dsf", "wav"] {
                let name = track_file_name(track.number, track.text.title.as_deref(), ext);
                check_name(name.as_bytes());
                let unique = dedup.unique(OsStr::new(&name), true);
                check_name(unique.as_bytes());
            }
            let set = dsf::tag_set(toc, area, track, &dsf::options(area, track));
            let id3 = match dsf::tagged(toc, area, track) {
                Some(t) => {
                    let tag = dsf::region(&t.layout, RegionKind::Trailer);
                    check_tag(&tag);
                    assert!(!dsf::region(&t.layout, RegionKind::Header).is_empty());
                    tag
                }
                None => Vec::new(),
            };
            let trailer = wav::trailer(&set, &id3);
            check_chunks(&trailer.bytes);
            assert_eq!(&trailer.bytes[trailer.tag.clone()], &id3[..]);
            let layout = Layout {
                channels: area.channel_count,
                frames: track.duration_frames,
                samples_per_frame: 2352,
                rate: 176_400,
            };
            let header = wav::header_parts(&layout, area.loudspeaker_layout(), trailer.bytes.len() as u64);
            assert_eq!(header.len(), wav::header_len(&layout));
        }
    }
});
