// SPDX-License-Identifier: GPL-3.0-or-later

//! Names for a disc whose TOC has no album or artist text (#185, phase 1): a guess from the
//! image's path, with no network and nothing stored. What it gives only ever goes into tags:
//! it is never part of a path in the mount, so no folder name can make an unsafe file name.
//! A tags file (#81) overrides all of it ([`crate::dsf::tagged`]).
//!
//! *The rule is deliberately narrow:* a wrong artist is worse than none, since a player then
//! files the disc under someone else. So a guess needs a name with a shape that cannot be
//! read another way, or two names that agree. In order, the first that applies:
//!
//! 1. The image's name, or else its folder's, reads `Artist - YYYY - Album [...]`: both.
//! 2. Its folder reads `YYYY - Album [...]`: the album. The artist only if the image's name
//!    agrees: it starts with the name of the folder above (`Artist/YYYY - Album/Artist - Album.iso`,
//!    `Artist/YYYY - Album/Artist.YYYY.Album.iso`), or it reads `Artist - Album` with the same
//!    album.
//! 3. Otherwise the album is the image's file name as it is, and there is no artist. So
//!    `Mozart - Requiem.iso` is an album called that: its two parts could be either way round,
//!    or an album and a disc.
//!
//! A year is 1900 to 2099, and the separator is ` - ` with its spaces. Trailing `[...]` groups
//! (editions, formats) leave the album; anything else stays. The year itself is not used for a
//! date tag: a folder's year may be a reissue's. A name that is not valid UTF-8 or holds control
//! characters gives nothing. Placeholders (`Unknown Artist`) and an artist of digits alone are
//! not names.

use std::ffi::OsStr;
use std::path::Path;

use crate::names;

/// What an image's path says about its disc.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscNames {
    /// The album, if the path gives one.
    pub album: Option<String>,
    /// The artist, if the path gives one.
    pub artist: Option<String>,
}

/// The names the path of the image `image` gives: see the module documentation.
pub fn guess(image: &Path) -> DiscNames {
    let stem = readable(image.file_stem());
    let folder = readable(image.parent().and_then(Path::file_name));
    let above = readable(image.parent().and_then(Path::parent).and_then(Path::file_name));
    for name in [stem, folder].into_iter().flatten() {
        if let Some((artist, album)) = artist_year_album(name) {
            return DiscNames { album: Some(album), artist: Some(artist) };
        }
    }
    if let Some(album) = folder.and_then(year_album) {
        let artist = stem.and_then(|s| agreeing_artist(s, above, &album));
        return DiscNames { album: Some(album), artist };
    }
    let album = image.file_stem().and_then(|n| n.to_str()).map(names::sanitize).filter(|a| is_name(a));
    DiscNames { album, artist: None }
}

/// A path component that can be read as a name: valid UTF-8, no control characters.
fn readable(name: Option<&OsStr>) -> Option<&str> {
    name.and_then(OsStr::to_str).filter(|n| !has_control(n))
}

/// `Artist - YYYY - Album [...]`.
fn artist_year_album(name: &str) -> Option<(String, String)> {
    let (artist, rest) = name.split_once(" - ")?;
    let album = year_album(rest)?;
    let artist = artist.trim();
    is_artist(artist).then(|| (artist.to_owned(), album))
}

/// `YYYY - Album [...]`: the album.
fn year_album(name: &str) -> Option<String> {
    let (year, album) = name.split_once(" - ")?;
    is_year(year.trim()).then(|| album_of(album)).flatten()
}

/// The artist, if the image's name `stem` agrees with its place: see the module documentation.
fn agreeing_artist(stem: &str, above: Option<&str>, album: &str) -> Option<String> {
    let words = normalized(stem);
    if let Some(above) = above.map(str::trim).filter(|a| is_artist(a)) {
        let artist = normalized(above);
        if words == artist || words.strip_prefix(&artist).is_some_and(|r| r.starts_with(' ')) {
            return Some(above.to_owned());
        }
    }
    let (artist, rest) = stem.split_once(" - ")?;
    let artist = artist.trim();
    (is_artist(artist) && album_of(rest).is_some_and(|a| normalized(&a) == normalized(album)))
        .then(|| artist.to_owned())
}

/// `text` without its trailing `[...]` groups, if a name is left.
fn album_of(text: &str) -> Option<String> {
    let mut s = text.trim();
    while let Some(i) = trailing_group(s) {
        s = s[..i].trim_end();
        s = s.strip_suffix(" -").unwrap_or(s).trim_end();
    }
    is_name(s).then(|| s.to_owned())
}

/// Where the `[...]` group that ends `s` starts, nesting included; `None` if `s` doesn't end
/// in one, or its brackets don't balance.
fn trailing_group(s: &str) -> Option<usize> {
    if !s.ends_with(']') {
        return None;
    }
    let mut depth = 0usize;
    for (i, c) in s.char_indices().rev() {
        match c {
            ']' => depth += 1,
            '[' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_year(s: &str) -> bool {
    s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit()) && matches!(s.parse::<u16>(), Ok(1900..=2099))
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Whether `s` can name something: it has a letter or digit, and is no placeholder.
fn is_name(s: &str) -> bool {
    const PLACEHOLDERS: [&str; 6] =
        ["unknown", "unknown album", "unknown artist", "untitled", "no name", "none"];
    s.chars().any(char::is_alphanumeric) && !PLACEHOLDERS.contains(&s.to_lowercase().as_str())
}

/// Whether `s` can name an artist: digits alone are more likely a year or a number.
fn is_artist(s: &str) -> bool {
    is_name(s) && !s.chars().all(|c| c.is_numeric() || c.is_whitespace())
}

/// `s` as lower-case words, for comparing names that differ in case and punctuation
/// (`Some.Artist-2001` starts with `Some Artist`).
fn normalized(s: &str) -> String {
    let lower = s.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    words.join(" ")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::os::unix::ffi::OsStrExt;

    use super::*;

    fn names(path: &str) -> (Option<String>, Option<String>) {
        let n = guess(Path::new(path));
        (n.album, n.artist)
    }

    fn both(album: &str, artist: &str) -> (Option<String>, Option<String>) {
        (Some(album.to_owned()), Some(artist.to_owned()))
    }

    fn album(album: &str) -> (Option<String>, Option<String>) {
        (Some(album.to_owned()), None)
    }

    /// The table in docs/user-guide.md, "Tagging behaviour".
    #[test]
    fn the_user_guides_table() {
        for (path, want) in [
            ("Artist - 2001 - Album [SACD].iso", both("Album", "Artist")),
            ("Artist - 2001 - Album [SACD]/disc.iso", both("Album", "Artist")),
            ("Artist/2001 - Album [SACD]/Artist - Album.iso", both("Album", "Artist")),
            ("Artist/2001 - Album [SACD]/Artist.2001.Album.iso", both("Album", "Artist")),
            ("Genre/2001 - Album [SACD]/Artist - Album.iso", both("Album", "Artist")),
            ("Artist/2001 - Album [SACD]/disc.iso", album("Album")),
            ("Artist/Album/Artist - Album.iso", album("Artist - Album")),
            ("Artist/Album/disc.iso", album("disc")),
        ] {
            assert_eq!(names(path), want, "{path}");
        }
    }

    #[test]
    fn the_full_pattern() {
        assert_eq!(names("/l/x/Some One - 2002 - An Album.iso"), both("An Album", "Some One"));
        assert_eq!(names("/l/Some One - 2002 - An Album [SACD]/disc.iso"), both("An Album", "Some One"));
        assert_eq!(names("/l/A - 1999 - B - C [x, y] [SACD ISO].iso"), both("B - C", "A"));
        assert_eq!(names("/l/A - 1999 - B [x [nested]].iso"), both("B", "A"));
        // Other groups stay; a broken one is part of the name.
        assert_eq!(names("/l/A - 1999 - B (Live) [x].iso"), both("B (Live)", "A"));
        assert_eq!(names("/l/A - 1999 - B [x.iso"), both("B [x", "A"));
        assert_eq!(names("/l/A - 1999 - -B-.iso"), both("-B-", "A"));
        // The image's own name comes first.
        assert_eq!(names("/l/C - 2001 - D/A - 1999 - B.iso"), both("B", "A"));
    }

    #[test]
    fn a_year_folder_needs_the_image_name_to_agree_for_an_artist() {
        let agree = both("An Album", "Some One");
        assert_eq!(names("/l/Some One/2002 - An Album [SACD]/Some One - An Album.iso"), agree);
        assert_eq!(names("/l/Some One/2002 - An Album/Some.One-2002-An.Album.(SACD.ISO).iso"), agree);
        assert_eq!(names("/l/SOME ONE/2002 - An Album/some one.iso").0, agree.0);
        assert_eq!(names("/l/SOME ONE/2002 - An Album/some one.iso").1.as_deref(), Some("SOME ONE"));
        // The folder above says nothing, the name's album agrees.
        assert_eq!(names("/l/SACD/2002 - An Album/Some One - An Album [x].iso"), agree);
        // Nothing agrees: the album alone.
        assert_eq!(names("/l/SACD/2002 - An Album/image.iso"), album("An Album"));
        assert_eq!(names("/l/SACD/2002 - An Album/Some One - Another.iso"), album("An Album"));
        // A name that only starts like the folder above is not it.
        assert_eq!(names("/l/Some/2002 - An Album/Someone.iso"), album("An Album"));
        assert_eq!(names("2002 - An Album/image.iso"), album("An Album"));
    }

    #[test]
    fn otherwise_the_plain_file_name() {
        for (path, want) in [
            ("/l/Artist/Album/disc.iso", "disc"),
            ("disc.iso", "disc"),
            ("/l/Mozart - Requiem.iso", "Mozart - Requiem"),
            ("/l/A - B - 2002 - C.iso", "A - B - 2002 - C"),
            // Not a year: too old, too far ahead, too long, not set off by spaces.
            ("/l/A - 1899 - B.iso", "A - 1899 - B"),
            ("/l/A - 2100 - B.iso", "A - 2100 - B"),
            ("/l/A - 20021 - B.iso", "A - 20021 - B"),
            ("/l/A-2002-B.iso", "A-2002-B"),
            ("/l/A - (2002) - B.iso", "A - (2002) - B"),
            // No artist or album in the pattern's places.
            ("/l/1984 - 1985 - B.iso", "1984 - 1985 - B"),
            ("/l/Unknown Artist - 2002 - B.iso", "Unknown Artist - 2002 - B"),
            ("/l/A - 2002 - [SACD].iso", "A - 2002 - [SACD]"),
        ] {
            assert_eq!(names(path).0.as_deref(), Some(want), "{path}");
            assert_eq!(names(path).1, None, "{path}");
        }
    }

    #[test]
    fn hostile_names_give_nothing_unsafe() {
        // No name, or nothing usable in it.
        for path in ["", "/", "..", "/l/..", "/l/---.iso", "/l/[].iso", "/l/Unknown.iso"] {
            assert_eq!(names(path).1, None, "{path:?}");
            assert_eq!(names(path).0, None, "{path:?}");
        }
        // Not UTF-8: not read.
        let bad = OsStr::from_bytes(b"/l/A - 2002 - \xff.iso");
        assert_eq!(guess(Path::new(bad)), DiscNames::default());
        // Control characters: no pattern, and no such character in the plain name.
        let n = guess(Path::new("/l/A\n - 2002 - B\t.iso"));
        assert_eq!(n, DiscNames { album: Some("A_ - 2002 - B_".into()), artist: None });
        let n = guess(Path::new("/l/A - 2002 - B\n/C.iso"));
        assert_eq!(n, DiscNames { album: Some("C".into()), artist: None });
        // A long name is taken whole; it goes into a tag, not a path.
        let long = format!("A - 2002 - {}.iso", "é".repeat(120));
        assert_eq!(guess(Path::new(&long)).album.unwrap().len(), 240);
    }
}
