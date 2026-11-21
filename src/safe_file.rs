// SPDX-License-Identifier: GPL-3.0-or-later

//! Files sacdfs writes or reads in folders that other users may write to (the tags directory,
//! the cache, a report's folder), and text from such folders shown on a terminal.
//!
//! - A file is written through a new temporary file in the same folder: made with `O_EXCL` and
//!   `O_NOFOLLOW`, under a name nobody can guess, with an explicit mode, then synced and renamed
//!   over the target. Nothing that was planted under any name is ever opened for writing.
//! - A file is read through one descriptor opened with `O_NOFOLLOW`, checked with `fstat` to be
//!   a regular file (and not too large), and read from that descriptor: no check by name that
//!   the file could change after.
//! - Paths and messages shown on a terminal or in a log have their control characters escaped.

use std::borrow::Cow;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The mode of tags files, covers, reports and the state files, before the umask: as
/// `fs::write` made them, so that a folder shared by a group (umask 002) stays editable by it.
pub const MODE: u32 = 0o666;

/// The largest tags file read. *Why a bound:* the mount reads tags files while it answers
/// requests, and a huge (or sparse) file must not take its memory. Real ones are a few KiB.
pub const MAX_TAGS_TEXT: u64 = 4 << 20;

/// The largest state file read (the tag watch, the lookup state): about 100 bytes per image,
/// so room for a few hundred thousand images.
pub const MAX_STATE_TEXT: u64 = 32 << 20;

/// A temporary file's name beside `path`: hidden, with the process's ID, a counter and 64
/// random bits, so that it can't be guessed ahead.
fn temp_name(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    // Seeded from the OS's random source, once per thread, then varied per call.
    let mut h = RandomState::new().build_hasher();
    h.write_u64(seq);
    h.write_u128(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()),
    );
    let name = path.file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy());
    path.with_file_name(format!(".{name}.{}.{seq}.{:016x}.tmp", std::process::id(), h.finish()))
}

/// Creates the new file `path` for writing, with `mode`: it must not exist, not even as a
/// symbolic link (`O_EXCL`, `O_NOFOLLOW`).
pub fn create_new(path: &Path, mode: u32) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    crate::platform::set_create_mode(&mut o, mode);
    crate::platform::set_open_flags(&mut o, crate::platform::OpenFlags::NO_FOLLOW).open(path)
}

/// A new temporary file in the folder of `path`, with `mode`, and its path.
pub fn create_temp(path: &Path, mode: u32) -> io::Result<(File, PathBuf)> {
    let mut tries = 0;
    loop {
        let tmp = temp_name(path);
        match create_new(&tmp, mode) {
            Ok(f) => return Ok((f, tmp)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && tries < 8 => tries += 1,
            Err(e) => return Err(e),
        }
    }
}

/// Writes `bytes` to `path` atomically: a new temporary file in the same folder (see
/// [`create_temp`]), written, synced, then renamed over `path`. The temporary file is removed on
/// any error.
///
/// If `path` is a symbolic link, the rename replaces the link itself; its target is untouched.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_atomic_with(path, mode, |f| f.write_all(bytes))
}

/// [`write_atomic`], with what `write` writes to the file.
pub fn write_atomic_with(
    path: &Path,
    mode: u32,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let (mut f, tmp) = create_temp(path, mode)?;
    let result = write(&mut f).and_then(|()| f.sync_all()).and_then(|()| {
        drop(f);
        // rename(2) replaces whatever is at `path`, a symbolic link included, and never
        // follows it.
        fs::rename(&tmp, path)
    });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The bytes of the regular file `path`, of at most `max` bytes, read through one descriptor
/// that doesn't follow a symbolic link, and its metadata from that descriptor.
pub fn read_regular(path: &Path, max: u64) -> io::Result<(Vec<u8>, fs::Metadata)> {
    use crate::platform::OpenFlags;
    let f = crate::platform::set_open_flags(
        OpenOptions::new().read(true),
        OpenFlags::NO_FOLLOW | OpenFlags::NON_BLOCK | OpenFlags::NO_CTTY,
    )
    .open(path)
    .map_err(|e| {
        if crate::platform::is_link_refused(&e) {
            io::Error::new(io::ErrorKind::InvalidInput, "a symbolic link: not read")
        } else {
            e
        }
    })?;
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    let too_large = || {
        let limit = if max % (1 << 20) == 0 { format!("{} MiB", max >> 20) } else { format!("{max} bytes") };
        io::Error::new(io::ErrorKind::InvalidInput, format!("larger than {limit}: not read"))
    };
    if meta.len() > max {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().min(max)).unwrap_or(0));
    f.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(too_large());
    }
    Ok((bytes, meta))
}

/// [`read_regular`] of a UTF-8 text file of at most `max` bytes ([`MAX_TAGS_TEXT`],
/// [`MAX_STATE_TEXT`]).
pub fn read_text(path: &Path, max: u64) -> io::Result<(String, fs::Metadata)> {
    let (bytes, meta) = read_regular(path, max)?;
    let text = String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((text, meta))
}

/// `s` with its control characters escaped (`\x1b`, `\u{9b}`, ...), and its format characters
/// (bidi controls and the like) and line and paragraph separators as `\u{202e}` (#448), the
/// characters [`crate::printable::needs_escape`] names; line ends kept when `lines`: for a
/// terminal or a log.
fn escape_controls(s: &str, lines: bool) -> Cow<'_, str> {
    use fmt::Write as _;
    let escaped = |c: char| crate::printable::needs_escape(c) && !(lines && c == '\n');
    if !s.chars().any(escaped) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        // Writing to a `String` can't fail.
        let _ = match c {
            c if !escaped(c) => out.write_char(c),
            c if (c as u32) < 0x100 && c.is_control() => write!(out, "\\x{:02x}", c as u32),
            c => write!(out, "\\u{{{:x}}}", c as u32),
        };
    }
    Cow::Owned(out)
}

/// `s` (lines of text) with its control characters but line ends escaped.
pub fn printable(s: &str) -> Cow<'_, str> {
    escape_controls(s, true)
}

/// `s` on one line: every control character escaped, line ends included (a key, a name).
pub fn one_line(s: &str) -> Cow<'_, str> {
    escape_controls(s, false)
}

/// A path as shown on a terminal or in a log: as [`Path::display`], with every control
/// character escaped.
#[derive(Debug)]
pub struct Shown<'a>(&'a Path);

/// See [`Shown`].
pub fn shown(path: &Path) -> Shown<'_> {
    Shown(path)
}

impl fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&escape_controls(&self.0.to_string_lossy(), false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Links and modes: Unix only.
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[cfg(unix)]
    #[test]
    fn write_atomic_replaces_a_link_and_leaves_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, b"precious").unwrap();
        let target = dir.path().join("k.toml");
        symlink(&victim, &target).unwrap();
        write_atomic(&target, b"new", MODE).unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        assert!(!fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&target).unwrap(), b"new");
        // The mode `fs::write` gives under this process's umask.
        let plain = dir.path().join("plain");
        fs::write(&plain, b"x").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(&target), mode(&plain));
        let left: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left.len(), 3, "no temporary file left: {left:?}");
    }

    #[test]
    fn temp_names_differ() {
        let p = Path::new("/x/a.toml");
        let (a, b) = (temp_name(p), temp_name(p));
        assert_ne!(a, b);
        assert!(a.file_name().unwrap().to_str().unwrap().starts_with(".a.toml."));
    }

    #[cfg(unix)]
    #[test]
    fn create_new_refuses_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, b"precious").unwrap();
        let link = dir.path().join("l");
        symlink(&victim, &link).unwrap();
        assert!(create_new(&link, MODE).is_err());
        symlink(dir.path().join("none"), dir.path().join("dangling")).unwrap();
        assert!(create_new(&dir.path().join("dangling"), MODE).is_err());
        assert!(!dir.path().join("none").exists());
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
    }

    #[cfg(unix)]
    #[test]
    fn read_regular_refuses_links_and_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        fs::write(&f, b"0123456789").unwrap();
        let link = dir.path().join("l");
        symlink(&f, &link).unwrap();
        assert_eq!(read_regular(&f, 10).unwrap().0, b"0123456789");
        assert!(read_regular(&f, 9).is_err());
        assert!(read_regular(&link, 100).is_err());
        assert!(read_regular(dir.path(), 100).is_err());
    }

    /// A sparse file larger than the bound is refused before anything is allocated for it.
    /// Unix only: the test makes a file of 1 TiB, which is sparse there without asking.
    #[cfg(unix)]
    #[test]
    fn read_text_refuses_a_file_over_its_bound() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("k.toml");
        File::create(&f).unwrap().set_len(MAX_TAGS_TEXT + 1).unwrap();
        let e = read_text(&f, MAX_TAGS_TEXT).unwrap_err();
        assert_eq!(e.to_string(), "larger than 4 MiB: not read");
        File::create(&f).unwrap().set_len(1 << 40).unwrap();
        assert!(read_text(&f, MAX_TAGS_TEXT).is_err());
        fs::write(&f, "[album]\n").unwrap();
        assert_eq!(read_text(&f, MAX_TAGS_TEXT).unwrap().0, "[album]\n");
    }

    #[test]
    fn control_characters_are_escaped() {
        let p = Path::new("/a/\x1b[31mred\u{9b}\n.iso");
        assert_eq!(shown(p).to_string(), "/a/\\x1b[31mred\\x9b\\x0a.iso");
        assert_eq!(printable("a\x1bb\nc\t"), "a\\x1bb\nc\\x09");
        assert!(matches!(printable("plain\n"), Cow::Borrowed(_)));
        assert_eq!(one_line("TXXX:a\nb\x1b"), "TXXX:a\\x0ab\\x1b");
        // Bidi controls, zero-width characters and line separators too (#448).
        assert_eq!(printable("a\u{202e}b\u{200b}c\u{2028}d\n"), "a\\u{202e}b\\u{200b}c\\u{2028}d\n");
        assert_eq!(one_line("\u{2066}x\u{9b}"), "\\u{2066}x\\x9b");
        assert_eq!(shown(Path::new("a\u{202e}b")).to_string(), "a\\u{202e}b");
        assert!(matches!(printable("Caf\u{e9} \u{65e5}\n"), Cow::Borrowed(_)));
    }
}
