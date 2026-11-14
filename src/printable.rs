// SPDX-License-Identifier: GPL-3.0-or-later

//! Log lines that show names as they are, but can't be split, recoloured or reordered by them
//! (#391).
//!
//! The daemon's log lines carry names from SOURCE (image paths, folders, disc text) and the
//! causes of errors, and a file name can hold any byte but `/` and NUL. Written raw, a newline
//! in a name splits a line in two, an escape sequence changes how a terminal shows the rest,
//! and a bidi control (U+202E and the like) makes a line display in another order than it reads.
//! So every line the daemon logs goes through [`EscapingLogger`], which writes these characters
//! escaped, as Rust writes them: `\n`, `\r`, `\t`, or `\u{…}` in hex.
//!
//! - **Escaped:** control characters (general category Cc: C0, DEL and C1), format characters
//!   (Cf, which includes the bidi controls, zero-width characters and the BOM), and the line and
//!   paragraph separators U+2028 and U+2029.
//! - **Not escaped:** everything else, non-ASCII letters included, and the backslash. *Why not
//!   the backslash:* names with one are common on shares written from Windows, and the point is
//!   that a line can't be broken or disguised, not that every line can be decoded back to bytes.
//!
//! *Why at the logger, not at each log call:* the daemon has about a hundred log calls, and the
//! names reach them through error messages too (`io::Error`, scarletbook's errors), which the
//! calls don't build. Escaping once, where lines are written, covers them all, and the lines
//! added later. Only log output changes: the names on the mount are the disc's and SOURCE's as
//! they are.

use std::fmt::{self, Write};

/// The format characters (general category Cf) of Unicode 16.0, as inclusive ranges.
const FORMAT: &[(char, char)] = &[
    ('\u{00AD}', '\u{00AD}'),
    ('\u{0600}', '\u{0605}'),
    ('\u{061C}', '\u{061C}'),
    ('\u{06DD}', '\u{06DD}'),
    ('\u{070F}', '\u{070F}'),
    ('\u{0890}', '\u{0891}'),
    ('\u{08E2}', '\u{08E2}'),
    ('\u{180E}', '\u{180E}'),
    ('\u{200B}', '\u{200F}'),
    ('\u{202A}', '\u{202E}'),
    ('\u{2060}', '\u{2064}'),
    ('\u{2066}', '\u{206F}'),
    ('\u{FEFF}', '\u{FEFF}'),
    ('\u{FFF9}', '\u{FFFB}'),
    ('\u{110BD}', '\u{110BD}'),
    ('\u{110CD}', '\u{110CD}'),
    ('\u{13430}', '\u{1343F}'),
    ('\u{1BCA0}', '\u{1BCA3}'),
    ('\u{1D173}', '\u{1D17A}'),
    ('\u{E0001}', '\u{E0001}'),
    ('\u{E0020}', '\u{E007F}'),
];

/// Whether `c` is a format character (Unicode general category Cf).
pub fn is_format(c: char) -> bool {
    FORMAT.iter().any(|&(lo, hi)| (lo..=hi).contains(&c))
}

/// Whether `c` is written escaped in a log line: a control or format character, or U+2028 or
/// U+2029.
pub fn needs_escape(c: char) -> bool {
    c.is_control() || c == '\u{2028}' || c == '\u{2029}' || (c > '\u{00AC}' && is_format(c))
}

/// Writes `s` to `out` with the characters of [`needs_escape`] escaped.
fn write_escaped(out: &mut impl Write, s: &str) -> fmt::Result {
    let mut rest = s;
    while let Some(i) = rest.find(needs_escape) {
        out.write_str(&rest[..i])?;
        let c = rest[i..].chars().next().unwrap_or_default();
        match c {
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            '\t' => out.write_str("\\t")?,
            c => write!(out, "\\u{{{:x}}}", u32::from(c))?,
        }
        rest = &rest[i + c.len_utf8()..];
    }
    out.write_str(rest)
}

/// A [`fmt::Write`] that escapes what is written to it.
struct EscapeWriter<'a, W: Write>(&'a mut W);

impl<W: Write> Write for EscapeWriter<'_, W> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_escaped(self.0, s)
    }
}

/// Displays a value with the characters of [`needs_escape`] escaped, without allocating.
#[derive(Clone, Copy, Debug)]
pub struct Escaped<T>(pub T);

impl<T: fmt::Display> fmt::Display for Escaped<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(EscapeWriter(f), "{}", self.0)
    }
}

/// A logger that hands each record to `inner` with its message [`Escaped`].
#[derive(Debug)]
pub struct EscapingLogger<L>(pub L);

impl<L: log::Log> log::Log for EscapingLogger<L> {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.0.enabled(metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.0.enabled(record.metadata()) {
            return;
        }
        self.0.log(
            &log::Record::builder()
                .metadata(record.metadata().clone())
                .args(format_args!("{}", Escaped(record.args())))
                .module_path(record.module_path())
                .file(record.file())
                .line(record.line())
                .build(),
        );
    }

    fn flush(&self) {
        self.0.flush();
    }
}

/// Installs `logger`, an env_logger built by the caller, as the process's logger behind
/// [`EscapingLogger`], with its level filter.
///
/// # Errors
///
/// If a logger is installed already.
pub fn install(logger: env_logger::Logger) -> Result<(), log::SetLoggerError> {
    let max = logger.filter();
    log::set_boxed_logger(Box::new(EscapingLogger(logger)))?;
    log::set_max_level(max);
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Mutex;

    use super::*;

    fn esc(s: &str) -> String {
        Escaped(s).to_string()
    }

    #[test]
    fn controls_and_format_characters_are_escaped() {
        assert_eq!(esc("a\nb\r\tc"), "a\\nb\\r\\tc");
        assert_eq!(esc("\u{1b}[31mred\u{7f}\u{85}"), "\\u{1b}[31mred\\u{7f}\\u{85}");
        assert_eq!(esc("abc\u{202e}fed"), "abc\\u{202e}fed");
        assert_eq!(
            esc("\u{2066}x\u{2069}\u{200b}\u{feff}\u{ad}"),
            "\\u{2066}x\\u{2069}\\u{200b}\\u{feff}\\u{ad}"
        );
        assert_eq!(esc("line\u{2028}para\u{2029}"), "line\\u{2028}para\\u{2029}");
        assert_eq!(esc("\u{e0041}\u{1d173}"), "\\u{e0041}\\u{1d173}");
    }

    #[test]
    fn everything_else_is_as_it_is() {
        for s in ["", "plain", "C:\\Music\\a.iso", "日本語 – Ünïcödé ½ 🎵", "\u{fffd}", "\u{a0}\u{ac}\u{ae}"]
        {
            assert_eq!(esc(s), s);
        }
    }

    /// No escaped line has a character that needs escaping, for every character there is.
    #[test]
    fn nothing_unescaped_is_left() {
        let all: String = (0..=0x10FFFFu32).filter_map(char::from_u32).collect();
        assert!(!esc(&all).contains(needs_escape));
    }

    static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    struct Capture;

    impl log::Log for Capture {
        fn enabled(&self, m: &log::Metadata<'_>) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record<'_>) {
            LINES.lock().unwrap().push(format!("{} {} {}", r.level(), r.target(), r.args()));
        }
        fn flush(&self) {}
    }

    #[test]
    fn the_logger_escapes_messages_and_keeps_the_rest() {
        use log::Log;
        let l = EscapingLogger(Capture);
        let name = "evil\n[ERROR] forged\u{202e}";
        l.log(
            &log::Record::builder()
                .level(log::Level::Warn)
                .target("sacdfs::tree")
                .args(format_args!("{name}: not shown"))
                .build(),
        );
        l.log(&log::Record::builder().level(log::Level::Debug).args(format_args!("dropped")).build());
        assert!(l.enabled(&log::Metadata::builder().level(log::Level::Info).build()));
        assert!(!l.enabled(&log::Metadata::builder().level(log::Level::Trace).build()));
        l.flush();
        let lines = LINES.lock().unwrap();
        assert_eq!(*lines, ["WARN sacdfs::tree evil\\n[ERROR] forged\\u{202e}: not shown"]);
    }
}
