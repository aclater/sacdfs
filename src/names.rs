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

/// The title of a track whose disc gives none (#185): shown in its tag and its file's name.
pub fn fallback_title(number: u8) -> String {
    format!("Track {number:02}")
}

/// A disc's title as part of a file name: [`sanitize`], with format characters (Unicode
/// category Cf: bidi controls such as U+202E, zero-width characters, the soft hyphen) left out
/// and the line and paragraph separators U+2028 and U+2029 made `_` (#448).
///
/// *Why:* a bidi override in a title could make a name read in another order than it is, so
/// that `.dsf` or `.wav` seems to be some other extension in a file manager, and a separator
/// breaks the name over two lines. Format characters are invisible, so leaving them out changes
/// how no name reads. *Why not in tags:* a tag is shown as a field, not as a name with an
/// extension, and it stays what the disc holds, as extractors write it.
pub fn title_name(text: &str) -> String {
    let kept: String = text
        .chars()
        .filter(|&c| !crate::printable::is_format(c))
        .map(|c| if c == '\u{2028}' || c == '\u{2029}' { '_' } else { c })
        .collect();
    sanitize(&kept)
}

/// The name of a track's file: `NN - <title>.<ext>`, with [`fallback_title`] without a usable
/// title. `ext` is a short extension (`dsf`, `wav`), so that the files of one track differ only
/// in it.
pub fn track_file_name(number: u8, title: Option<&str>, ext: &str) -> String {
    let title = title.map(title_name).filter(|t| !t.is_empty()).unwrap_or_else(|| fallback_title(number));
    let prefix = format!("{number:02} - ");
    // Room for " (NNN)" from de-duplication as well as the extension.
    let room = NAME_MAX.saturating_sub(prefix.len() + 1 + ext.len() + 6);
    format!("{prefix}{}.{ext}", truncate_bytes(&title, room).trim_end())
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
        // An extension that leaves no room for the suffix (#161) is cut like any other text.
        let (stem, ext) = match lossy.rfind('.') {
            Some(i) if keep_extension && i > 0 && lossy.len() - i + 13 < NAME_MAX => {
                (&lossy[..i], &lossy[i..])
            }
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

    /// Bidi controls, zero-width characters and the soft hyphen are left out of names, line
    /// and paragraph separators become `_` (#448); everything else as [`sanitize`].
    #[test]
    fn title_names_have_no_format_characters() {
        assert_eq!(title_name("abc\u{202e}vaw.exe"), "abcvaw.exe");
        assert_eq!(title_name("\u{2066}x\u{2069}\u{200b}y\u{feff}\u{ad}z\u{200f}"), "xyz");
        assert_eq!(title_name("a\u{2028}b\u{2029}c/d\u{1b}"), "a_b_c_d_");
        assert_eq!(title_name(" \u{200b} "), "");
        assert_eq!(title_name("Caf\u{e9} \u{65e5}\u{672c}"), "Caf\u{e9} \u{65e5}\u{672c}");
        assert_eq!(track_file_name(2, Some("\u{202e}"), "dsf"), "02 - Track 02.dsf");
        assert_eq!(track_file_name(2, Some("Song\u{202e}3pm.exe"), "wav"), "02 - Song3pm.exe.wav");
    }

    #[test]
    fn track_names() {
        assert_eq!(track_file_name(1, Some("Intro"), "dsf"), "01 - Intro.dsf");
        assert_eq!(track_file_name(1, Some("Intro"), "wav"), "01 - Intro.wav");
        assert_eq!(track_file_name(12, None, "dsf"), "12 - Track 12.dsf");
        assert_eq!(track_file_name(3, None, "wav"), "03 - Track 03.wav");
        assert_eq!(track_file_name(200, None, "dsf"), "200 - Track 200.dsf");
        assert_eq!(track_file_name(3, Some(" / "), "dsf"), "03 - _.dsf");
        assert_eq!(track_file_name(3, Some("\u{1}"), "dsf"), "03 - _.dsf");
        assert_eq!(track_file_name(3, Some("   "), "wav"), "03 - Track 03.wav");
        assert_eq!(track_file_name(200, Some("x"), "dsf"), "200 - x.dsf");
        let long = "é".repeat(300);
        let name = track_file_name(1, Some(&long), "dsf");
        assert!(name.len() <= NAME_MAX - 6);
        assert!(name.ends_with(".dsf"));
        // The names of one track differ only in the extension, however long the title.
        let wav = track_file_name(1, Some(&long), "wav");
        assert_eq!(wav.strip_suffix(".wav"), name.strip_suffix(".dsf"));
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
