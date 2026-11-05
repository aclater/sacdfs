// SPDX-License-Identifier: GPL-3.0-or-later

//! Names built from disc text and from names in SOURCE (`names.rs`).
//!
//! Input: one byte for the track number, one for the choices, then names separated by
//! [`SEP`](sacdfs_fuzz::SEP).

#![no_main]

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use libfuzzer_sys::fuzz_target;
use sacdfs::names::{Deduper, NAME_MAX, sanitize, track_file_name, truncate_bytes};
use sacdfs_fuzz::{SEP, check_name};

fuzz_target!(|data: &[u8]| {
    let [number, choices, rest @ ..] = data else { return };
    let ext = if choices & 1 == 0 { "dsf" } else { "wav" };
    let keep_extension = choices & 2 == 0;
    let mut dedup = Deduper::with_reserved([".", ".."]);
    let mut seen = HashSet::new();
    for raw in rest.split(|&b| b == SEP) {
        let text = String::from_utf8_lossy(raw);

        let clean = sanitize(&text);
        assert!(!clean.contains('/') && !clean.chars().any(char::is_control), "{clean:?}");
        assert_eq!(clean, clean.trim());

        let max = usize::from(*number) * 2;
        let cut = truncate_bytes(&text, max);
        assert!(cut.len() <= max && text.starts_with(cut));

        let title = (choices & 4 == 0).then_some(&*text);
        let name = track_file_name(*number, title, ext);
        check_name(name.as_bytes());
        assert!(name.ends_with(&format!(".{ext}")), "{name:?}");
        assert!(name.len() <= NAME_MAX - 6, "no room for a de-duplication suffix: {name:?}");

        // Names from a path go into tags (#185): any bytes give clean text or nothing.
        let guess = sacdfs::fallback::guess(std::path::Path::new(OsStr::from_bytes(raw)));
        for text in [&guess.album, &guess.artist].into_iter().flatten() {
            assert!(!text.is_empty() && !text.chars().any(char::is_control), "{text:?}");
        }

        // Names from SOURCE are any bytes, within what the kernel allows.
        for candidate in [name.as_bytes(), raw] {
            if candidate.len() > NAME_MAX {
                continue;
            }
            let unique = dedup.unique(OsStr::from_bytes(candidate), keep_extension);
            assert!(seen.insert(unique.clone()), "{unique:?} twice");
            assert!(unique != "." && unique != "..", "{unique:?}");
            assert!(unique.len() <= NAME_MAX, "{} bytes: {unique:?}", unique.len());
        }
    }
});
