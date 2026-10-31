// SPDX-License-Identifier: GPL-3.0-or-later

//! File and directory names shown in the mount.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};

/// Longest name most Linux filesystems accept, in bytes.
pub const NAME_MAX: usize = 255;

/// Replaces characters that cannot or should not appear in a file name: `/`, NUL and
/// control characters become `_`. Leading and trailing white space is removed.
pub fn sanitize(text: &str) -> String {
    let s: String = text.chars().map(|c| if c == '/' || c.is_control() { '_' } else { c }).collect();
    s.trim().to_owned()
}

/// Cuts `s` to at most `max` bytes at a character boundary.
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The name of a track's file: `NN - <title>.dsf`, or `NN.dsf` without a usable title.
pub fn track_file_name(number: u8, title: Option<&str>) -> String {
    let title = title.map(sanitize).unwrap_or_default();
    if title.is_empty() {
        return format!("{number:02}.dsf");
    }
    let prefix = format!("{number:02} - ");
    // Room for " (NNN)" from de-duplication as well as the extension.
    let room = NAME_MAX - prefix.len() - ".dsf".len() - 6;
    format!("{prefix}{}.dsf", truncate_bytes(&title, room).trim_end())
}

/// Makes names unique, in order: a repeated name gets ` (2)`, ` (3)`, ... before its
/// extension (the part after the last `.`, if `keep_extension`).
#[derive(Debug, Default)]
pub struct Deduper {
    seen: HashSet<OsString>,
}

impl Deduper {
    /// A deduper that already holds `names` (for example `.` and `..`).
    pub fn with_reserved<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        Deduper { seen: names.into_iter().map(OsString::from).collect() }
    }

    /// The unique form of `name`, recorded so that later names avoid it.
    pub fn unique(&mut self, name: &OsStr, keep_extension: bool) -> OsString {
        if self.seen.insert(name.to_owned()) {
            return name.to_owned();
        }
        let lossy = name.to_string_lossy();
        let (stem, ext) = match lossy.rfind('.') {
            Some(i) if keep_extension && i > 0 => (&lossy[..i], &lossy[i..]),
            _ => (&lossy[..], ""),
        };
        let mut n = 2u32;
        loop {
            // The suffix needs at most 13 bytes; cut the stem to keep the name within
            // NAME_MAX.
            let room = NAME_MAX.saturating_sub(ext.len() + 13);
            let candidate = OsString::from(format!("{} ({n}){ext}", truncate_bytes(stem, room)));
            if self.seen.insert(candidate.clone()) {
                return candidate;
            }
            n += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn sanitizes() {
        assert_eq!(sanitize("a/b\0c\u{1}d\u{7f}e\u{9f}"), "a_b_c_d_e_");
        assert_eq!(sanitize("  x  "), "x");
        assert_eq!(sanitize("日本語"), "日本語");
    }

    #[test]
    fn track_names() {
        assert_eq!(track_file_name(1, Some("Intro")), "01 - Intro.dsf");
        assert_eq!(track_file_name(12, None), "12.dsf");
        assert_eq!(track_file_name(3, Some(" / ")), "03 - _.dsf");
        assert_eq!(track_file_name(3, Some("\u{1}")), "03 - _.dsf");
        assert_eq!(track_file_name(3, Some("   ")), "03.dsf");
        assert_eq!(track_file_name(200, Some("x")), "200 - x.dsf");
        let long = "é".repeat(300);
        let name = track_file_name(1, Some(&long));
        assert!(name.len() <= NAME_MAX - 6);
        assert!(name.ends_with(".dsf"));
    }

    #[test]
    fn dedupes() {
        let mut d = Deduper::with_reserved([".", ".."]);
        assert_eq!(d.unique(OsStr::new("a.dsf"), true), "a.dsf");
        assert_eq!(d.unique(OsStr::new("a.dsf"), true), "a (2).dsf");
        assert_eq!(d.unique(OsStr::new("a.dsf"), true), "a (3).dsf");
        assert_eq!(d.unique(OsStr::new("a (2).dsf"), true), "a (2) (2).dsf");
        assert_eq!(d.unique(OsStr::new(".."), false), ".. (2)");
        assert_eq!(d.unique(OsStr::new("x.y"), false), "x.y");
        assert_eq!(d.unique(OsStr::new("x.y"), false), "x.y (2)");
        let long = "a".repeat(255);
        d.unique(OsStr::new(&long), false);
        assert!(d.unique(OsStr::new(&long), false).len() <= NAME_MAX);
    }
}
