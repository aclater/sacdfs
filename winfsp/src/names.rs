// SPDX-License-Identifier: GPL-3.0-or-later

//! Windows names for the core's names (#419): the SFM private-use mapping the SMB share uses
//! (`SMB_NAME_MAPPING=sfm`), bare reserved device names, the 255 UTF-16 unit limit, and the
//! case-insensitive key a lookup folds to.
//!
//! This is done in the front end, folder by folder ([`crate::paths`]): of names that fold alike,
//! the first is shown and the others are left out with a warning. The design (docs/design.md,
//! "Names: case and normalisation") moves it into the tree as `NameRules`, so that names are
//! unique after folding when they are made (#419, item 14).

/// The characters Windows refuses in a name, each with the private-use character it is shown
/// as: Samba's `catia` mapping for SFM, the same table as deploy/entrypoint.sh.
const SFM: [(char, char); 8] = [
    ('"', '\u{f020}'),
    ('*', '\u{f021}'),
    (':', '\u{f022}'),
    ('<', '\u{f023}'),
    ('>', '\u{f024}'),
    ('?', '\u{f025}'),
    ('\\', '\u{f026}'),
    ('|', '\u{f027}'),
];

/// A trailing space and a trailing dot, which Windows strips from a name it opens: SFM's
/// characters for them.
const TRAILING_SPACE: char = '\u{f028}';
const TRAILING_DOT: char = '\u{f029}';

/// Bare names Windows opens as devices (`CON`, `NUL`, `COM1`): a name that is exactly one of
/// these, in any case, gets a trailing `_`. A stem with an extension (`CON.dsf`) opens on
/// Windows 11 (the feasibility spike in #22), so it is left as it is.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The most UTF-16 units in one name; one more and Explorer's listing of the folder fails.
pub const MAX_UNITS: usize = 255;

/// The name `name` (as the core has it) is shown as on Windows.
pub fn to_windows(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '\u{1}'..='\u{1f}' => char::from_u32(0xf000 + c as u32).unwrap_or(c),
            _ => SFM.iter().find(|(from, _)| *from == c).map_or(c, |(_, to)| *to),
        })
        .collect();
    match out.chars().last() {
        Some(' ') => {
            out.pop();
            out.push(TRAILING_SPACE);
        }
        Some('.') if out != "." && out != ".." => {
            out.pop();
            out.push(TRAILING_DOT);
        }
        _ => {}
    }
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&out)) {
        out.push('_');
    }
    out
}

/// Whether `name`, a Windows name, is longer than Windows takes.
pub fn too_long(name: &str) -> bool {
    name.encode_utf16().count() > MAX_UNITS
}

/// The key two names that Windows takes as the same have in common: upper case, by Unicode's
/// simple mapping (close to, not the same as, NTFS's upcase table).
pub fn fold(name: &str) -> String {
    name.chars().flat_map(char::to_uppercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_characters_map_to_private_use() {
        assert_eq!(to_windows("A: B?"), "A\u{f022} B\u{f025}");
        assert_eq!(to_windows("a<b>c|d\"e*f\\g"), "a\u{f023}b\u{f024}c\u{f027}d\u{f020}e\u{f021}f\u{f026}g");
        assert_eq!(to_windows("tab\there"), "tab\u{f009}here");
    }

    #[test]
    fn trailing_dot_and_space_are_kept_visible() {
        assert_eq!(to_windows("Vol. 1."), "Vol. 1\u{f029}");
        assert_eq!(to_windows("Live "), "Live\u{f028}");
        assert_eq!(to_windows(".."), "..");
    }

    #[test]
    fn bare_reserved_names_get_a_suffix() {
        assert_eq!(to_windows("nul"), "nul_");
        assert_eq!(to_windows("COM1"), "COM1_");
        assert_eq!(to_windows("CON.dsf"), "CON.dsf");
        assert_eq!(to_windows("CONCERT"), "CONCERT");
    }

    #[test]
    fn length_is_counted_in_utf16_units() {
        assert!(!too_long(&"a".repeat(255)));
        assert!(too_long(&"a".repeat(256)));
        // 128 characters outside the BMP: 256 units, though 128 characters.
        assert!(too_long(&"\u{1d11e}".repeat(128)));
    }

    #[test]
    fn folding_ignores_case() {
        assert_eq!(fold("Foo.dsf"), fold("FOO.DSF"));
        assert_eq!(fold("Ärger"), fold("ärger"));
    }
}
