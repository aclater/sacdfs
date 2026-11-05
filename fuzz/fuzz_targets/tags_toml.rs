// SPDX-License-Identifier: GPL-3.0-or-later

//! Tags files (`--tags`, `sacdfs tags`): the TOML parser's result, its text form, and a file
//! read through the tag store and applied to a real disc's tracks, as the mount does.
//!
//! Input: the file's text.

#![no_main]

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::SystemTime;

use libfuzzer_sys::fuzz_target;
use sacdfs::dsf;
use sacdfs::tags::{self, DiscTags, Key, Store, TagsFile, append_frames, disc_key};
use sacdfs_fuzz::{check_tag, scratch_dir, valid_toc};
use scarletbook::Slot;
use scarletbook::output::RegionKind;

/// The tags directory, with a picture and a file that is no picture, for `art`.
fn dir() -> &'static PathBuf {
    static D: OnceLock<PathBuf> = OnceLock::new();
    D.get_or_init(|| {
        let d = scratch_dir("tags");
        let _ = std::fs::write(d.join("cover.jpg"), b"\xFF\xD8\xFF\xE0 a tiny not-quite JPEG");
        let _ = std::fs::write(d.join("cover.png"), b"\x89PNG\r\n\x1a\n a tiny not-quite PNG");
        let _ = std::fs::write(d.join("note.txt"), b"not a picture");
        d
    })
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    for key in text.lines() {
        let _ = Key::of(key);
    }
    let Ok(file) = TagsFile::parse(text) else { return };

    // Its text form, which `sacdfs tags` writes, reads back as the same tags.
    let out = file.to_text();
    let again = match TagsFile::parse(&out) {
        Ok(a) => a,
        Err(e) => panic!("{out:?} does not read back: {e}"),
    };
    assert_eq!(again.to_text(), out);
    let slots = [Slot::TwoChannel, Slot::Multichannel];
    let numbers: Vec<u8> = file.track.keys().filter_map(|t| t.parse().ok()).chain([1, 2, 255]).collect();
    for slot in slots {
        for &n in &numbers {
            assert_eq!(file.fields_for(slot, n), again.fields_for(slot, n), "{slot:?} track {n}");
        }
    }

    // Applied alone: the frames and the picture appended to a tag.
    let d = DiscTags::new(file, SystemTime::UNIX_EPOCH, dir());
    for slot in slots {
        for &n in &numbers {
            if let Some(o) = d.overrides(slot, n) {
                for tag in [&b""[..], &b"ID3\x04\0\0\0\0\0\0"[..]] {
                    if let Some(t) = append_frames(tag, &o) {
                        check_tag(&t);
                    }
                }
            }
        }
    }

    // Applied as the mount does: through the store, to a disc's tracks.
    let toc = valid_toc();
    let store = Store::new(dir());
    if std::fs::write(store.path(&disc_key(toc)), text).is_err() {
        return;
    }
    tags::install(Some(store));
    for area in &toc.areas {
        for track in &area.tracks {
            if let Some(t) = dsf::tagged(toc, area, track, None) {
                check_tag(&dsf::region(&t.layout, RegionKind::Trailer));
                assert!(!dsf::region(&t.layout, RegionKind::Header).is_empty());
            }
        }
    }
    tags::install(None);
});
