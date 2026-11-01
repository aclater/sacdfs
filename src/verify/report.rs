// SPDX-License-Identifier: GPL-3.0-or-later

//! The report of `sacdfs verify`: one tab-separated row per file, with a header row.
//!
//! Fields are escaped so that any path fits on one line: `\\`, `\t`, `\n`, `\r`, and
//! `\xNN` for bytes of a path that are not UTF-8. Rows are written as they are made, so
//! an interrupted run leaves the rows it finished (which `--resume` keeps); at the end, the
//! report is rewritten sorted by path.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufWriter, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use super::id3::Losses;

/// The columns, in order.
pub const COLUMNS: [&str; 12] = [
    "status", "path", "bytes", "mtime", "image", "area", "track", "channels", "samples", "sha256", "loses",
    "reason",
];

/// The outcome for one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    /// sacdfs serves the same audio.
    Identical,
    /// sacdfs serves the same audio, and the extract differs only by silent pauses: the
    /// served track inside it, with silence before and after (or a track cut short, where
    /// sacdfs serves only silence beyond the end) (#93).
    IdenticalPause,
    /// A track of the same channel and sample counts exists, with other audio.
    Different,
    /// No track of the disc has the file's channel and sample counts.
    NoMatch,
    /// A file that can't be compared (DST-compressed DFF, ...).
    Unsupported,
    /// A broken or unreadable file, or a track sacdfs could not read.
    Error,
    /// FLAC or WAV.
    NotComparable,
}

impl Status {
    /// Every status, in the order the summary lists them.
    pub const ALL: [Status; 7] = [
        Status::Identical,
        Status::IdenticalPause,
        Status::Different,
        Status::NoMatch,
        Status::Unsupported,
        Status::Error,
        Status::NotComparable,
    ];

    /// The report's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Identical => "IDENTICAL",
            Status::IdenticalPause => "IDENTICAL-PAUSE",
            Status::Different => "DIFFERENT",
            Status::NoMatch => "NO-MATCH",
            Status::Unsupported => "UNSUPPORTED",
            Status::Error => "ERROR",
            Status::NotComparable => "NOT-COMPARABLE",
        }
    }

    /// Whether sacdfs serves the extract's audio (the files whose deletion is safe for audio).
    pub fn is_match(self) -> bool {
        matches!(self, Status::Identical | Status::IdenticalPause)
    }

    fn parse(s: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|st| st.as_str() == s)
    }
}

/// One row of the report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The outcome.
    pub status: Status,
    /// The extract.
    pub path: PathBuf,
    /// Its size.
    pub bytes: u64,
    /// Its modification time, `seconds.nanoseconds`.
    pub mtime: String,
    /// The image of the track it was compared with.
    pub image: Option<PathBuf>,
    /// That track's area folder: "Stereo" or "Multichannel".
    pub area: String,
    /// That track's number.
    pub track: Option<u8>,
    /// The extract's channel count.
    pub channels: Option<u8>,
    /// The extract's samples per channel.
    pub samples: Option<u64>,
    /// The SHA-256 of its canonical audio, in hex (for IDENTICAL-PAUSE, of the served track's).
    pub sha256: String,
    /// What deleting it would lose.
    pub loses: Losses,
    /// Why, for any status but IDENTICAL (for IDENTICAL-PAUSE, what surrounds the track).
    pub reason: String,
}

impl Row {
    /// A row for `path`, to be completed.
    pub fn new(path: PathBuf, bytes: u64, mtime: String) -> Row {
        Row {
            status: Status::Error,
            path,
            bytes,
            mtime,
            image: None,
            area: String::new(),
            track: None,
            channels: None,
            samples: None,
            sha256: String::new(),
            loses: Losses::default(),
            reason: String::new(),
        }
    }

    /// The row with `status` and `reason`.
    pub fn with(mut self, status: Status, reason: String) -> Row {
        self.status = status;
        self.reason = reason;
        self
    }

    fn line(&self) -> String {
        let opt = |v: Option<String>| v.unwrap_or_default();
        let fields = [
            self.status.as_str().to_owned(),
            escape(self.path.as_os_str().as_bytes()),
            self.bytes.to_string(),
            self.mtime.clone(),
            opt(self.image.as_ref().map(|p| escape(p.as_os_str().as_bytes()))),
            escape(self.area.as_bytes()),
            opt(self.track.map(|t| t.to_string())),
            opt(self.channels.map(|c| c.to_string())),
            opt(self.samples.map(|s| s.to_string())),
            self.sha256.clone(),
            escape(self.loses.render().as_bytes()),
            escape(self.reason.as_bytes()),
        ];
        fields.join("\t")
    }

    fn parse(line: &str) -> Option<Row> {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != COLUMNS.len() {
            return None;
        }
        let text = |s: &str| String::from_utf8_lossy(&unescape(s)).into_owned();
        let path = |s: &str| PathBuf::from(OsString::from_vec(unescape(s)));
        Some(Row {
            status: Status::parse(f[0])?,
            path: path(f[1]),
            bytes: f[2].parse().ok()?,
            mtime: f[3].to_owned(),
            image: (!f[4].is_empty()).then(|| path(f[4])),
            area: text(f[5]),
            track: f[6].parse().ok(),
            channels: f[7].parse().ok(),
            samples: f[8].parse().ok(),
            sha256: f[9].to_owned(),
            loses: Losses::parse(&text(f[10])),
            reason: text(f[11]),
        })
    }
}

/// Escapes a field (see the module documentation).
pub fn escape(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    for chunk in b.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c => out.push(c),
            }
        }
        for x in chunk.invalid() {
            out.push_str(&format!("\\x{x:02x}"));
        }
    }
    out
}

/// Undoes [`escape`].
pub fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            match b.get(i + 1) {
                Some(b'\\') => {
                    out.push(b'\\');
                    i += 2;
                    continue;
                }
                Some(b't') => {
                    out.push(b'\t');
                    i += 2;
                    continue;
                }
                Some(b'n') => {
                    out.push(b'\n');
                    i += 2;
                    continue;
                }
                Some(b'r') => {
                    out.push(b'\r');
                    i += 2;
                    continue;
                }
                Some(b'x') => {
                    if let Some(v) = b
                        .get(i + 2..i + 4)
                        .and_then(|h| std::str::from_utf8(h).ok())
                        .and_then(|h| u8::from_str_radix(h, 16).ok())
                    {
                        out.push(v);
                        i += 4;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// The rows of an earlier report, by path. Lines that don't parse are skipped.
pub fn read(path: &Path) -> io::Result<HashMap<PathBuf, Row>> {
    let f = File::open(path)?;
    let mut out = HashMap::new();
    for line in io::BufReader::new(f).lines() {
        let line = line?;
        if let Some(r) = Row::parse(&line) {
            out.insert(r.path.clone(), r);
        }
    }
    Ok(out)
}

/// Where the report goes.
pub struct Writer {
    path: Option<PathBuf>,
    out: Box<dyn Write + Send>,
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writer").field("path", &self.path).finish_non_exhaustive()
    }
}

impl Writer {
    /// A report to `path`, or to standard output; starts with the header row.
    pub fn create(path: Option<&Path>) -> io::Result<Writer> {
        let out: Box<dyn Write + Send> = match path {
            Some(p) => Box::new(BufWriter::new(File::create(p)?)),
            None => Box::new(io::stdout()),
        };
        let mut w = Writer { path: path.map(Path::to_owned), out };
        writeln!(w.out, "{}", COLUMNS.join("\t"))?;
        w.out.flush()?;
        Ok(w)
    }

    /// Writes one row, flushed.
    pub fn row(&mut self, r: &Row) -> io::Result<()> {
        writeln!(self.out, "{}", r.line())?;
        self.out.flush()
    }

    /// Finishes the report: a file is rewritten with `rows`, sorted, replacing it at once.
    pub fn finish(mut self, rows: &[Row]) -> io::Result<()> {
        self.out.flush()?;
        let Some(path) = self.path else { return Ok(()) };
        let mut name = path.file_name().map(OsString::from).unwrap_or_default();
        name.push(".tmp");
        let tmp = path.with_file_name(name);
        {
            let mut w = BufWriter::new(File::create(&tmp)?);
            writeln!(w, "{}", COLUMNS.join("\t"))?;
            for r in rows {
                writeln!(w, "{}", r.line())?;
            }
            w.flush()?;
        }
        fs::rename(&tmp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_trip() {
        let mut r =
            Row::new(PathBuf::from(OsString::from_vec(b"/a\tb\\c\nd\xff.dsf".to_vec())), 12, "1.5".into());
        r = r.with(Status::Different, "x\ty".into());
        r.image = Some("/i.iso".into());
        r.track = Some(3);
        r.loses.art = true;
        r.loses.other.insert("TCON".into());
        let line = r.line();
        assert!(!line.contains('\n'));
        assert_eq!(Row::parse(&line), Some(r));
        assert_eq!(Row::parse(&COLUMNS.join("\t")), None);
    }
}
