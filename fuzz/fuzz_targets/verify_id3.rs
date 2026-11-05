// SPDX-License-Identifier: GPL-3.0-or-later

//! The `verify` and `tags import` readers of ID3v2 tags, as found in other tools' files.
//!
//! Input: a tag, and optionally a [`SEP`](sacdfs_fuzz::SEP) and a second tag, compared with it.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sacdfs::tags_cli::front_cover;
use sacdfs::verify::id3::{Losses, frames};
use sacdfs_fuzz::fields;

fuzz_target!(|data: &[u8]| {
    let whole = frames(data);
    let _ = front_cover(data);
    let [a, b] = fields::<2>(data);
    let (fa, fb) = (frames(a), frames(b));
    if let Some(image) = front_cover(a) {
        assert!(a.windows(image.len().max(1)).any(|w| w == &image[..]) || image.is_empty());
    }
    for (x, y) in [(&whole, &fa), (&fa, &fb)] {
        for tracks in [0, 1, 9] {
            let l = Losses::between(x, y, tracks);
            let shown = l.render();
            assert_eq!(Losses::parse(&shown).render(), shown);
        }
    }
    let mut l = Losses::default();
    for f in &whole {
        l.add(&f.key);
    }
    assert_eq!(Losses::parse(&l.render()).render(), l.render());
});
