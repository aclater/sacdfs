// SPDX-License-Identifier: GPL-3.0-or-later

//! The report `sacdfs verify --resume` reads back: its field escaping and its lines.
//!
//! Input: a line of a report, also taken as the bytes of a field.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sacdfs::verify::Row;
use sacdfs::verify::report::{escape, unescape};

fuzz_target!(|data: &[u8]| {
    // Any bytes in a field come back as they were, and on one line.
    let escaped = escape(data);
    assert!(!escaped.contains(['\t', '\n', '\r']), "{escaped:?}");
    assert_eq!(unescape(&escaped), data);

    let Ok(text) = std::str::from_utf8(data) else { return };
    let _ = unescape(text);
    for line in text.lines() {
        // A line read is written and read back the same.
        if let Some(row) = Row::parse(line) {
            let out = row.line();
            let again = Row::parse(&out);
            assert!(again.is_some(), "{line:?} became {out:?}");
            if let Some(again) = again {
                assert_eq!(again.line(), out);
            }
        }
    }
});
