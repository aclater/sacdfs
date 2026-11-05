// SPDX-License-Identifier: GPL-3.0-or-later

//! The `verify` readers of DSF and DFF files: their headers, the audio stream they make of
//! them, the tag they find, and the search of one stream in another.
//!
//! Input: a file, and optionally a [`SEP`](sacdfs_fuzz::SEP) and a second file, which is looked
//! for inside the first as a served track is in an extract that kept its pauses.

#![no_main]

use std::sync::atomic::AtomicU64;

use libfuzzer_sys::fuzz_target;
use sacdfs::verify::container::{Audio, MAX_CHANNELS, MAX_TAG, parse};
use sacdfs::verify::id3::{self, Losses};
use sacdfs::verify::pause::{Meter, locate};
use sacdfs_fuzz::fields;

fn read_all(mut file: &[u8]) -> Option<Audio> {
    let audio = parse(&mut file).ok()?;
    assert!(audio.channels > 0 && u32::from(audio.channels) <= MAX_CHANNELS);

    // The stream has the length the header says.
    let read = AtomicU64::new(0);
    let mut meter = Meter::new(audio.channels);
    let mut len = 0u64;
    let stream = audio.canonical(&mut file, &read, &mut |b| {
        len += b.len() as u64;
        meter.feed(b);
    });
    if stream.is_ok() {
        assert_eq!(Some(len), audio.canonical_len());
        let region = meter.finish();
        let _ = (region.describe(), region.is_silence(), region.seconds());
        let digest = audio.digest(&mut file, &read);
        assert!(digest.is_ok());
    }

    match audio.read_id3(&mut file) {
        Ok(Some(tag)) => {
            assert!(tag.len() as u64 <= MAX_TAG);
            let frames = id3::frames(&tag);
            let l = Losses::between(&frames, &[], 1);
            assert_eq!(Losses::parse(&l.render()).render(), l.render());
        }
        Ok(None) => {}
        Err(size) => assert!(size > MAX_TAG),
    }
    Some(audio)
}

fuzz_target!(|data: &[u8]| {
    let [mut first, mut second] = fields::<2>(data);
    let a = read_all(first);
    let b = read_all(second);
    if let (Some(a), Some(b)) = (a, b) {
        let read = AtomicU64::new(0);
        if let Ok(digest) = b.digest(&mut second, &read) {
            let _ = locate((&a, &mut first), (&b, &mut second), &digest, &read);
        }
    }
    // Also the whole input as one file, for a file with the separator byte in it.
    if !second.is_empty() {
        let _ = read_all(data);
    }
});
