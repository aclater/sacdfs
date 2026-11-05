// SPDX-License-Identifier: GPL-3.0-or-later

//! The report of `sacdfs verify`: one tab-separated row per file, with a header row.
//!
//! Fields are escaped so that any path fits on one line: `\\`, `\\t`, `\\n`, `\\r`, and
//! `\\xNN` for bytes of a path that are not UTF-8.
//!
//! **A report under its own name is a finished run's.** Rows are written as they are made to
//! `FILE.partial`, so an interrupted run leaves the rows it finished (which `--resume` keeps)
//! under a name that says so. Once a run has its first rows there, an earlier `FILE` moves to
//! `FILE.previous`: not gone, since a run may fail and that report took long to make, and not
//! to be taken for this run's. Only a run that checked every file puts `FILE` back, sorted by
//! path, and removes the other two. *Why:* `sacdfs verify` is what tells a user that extracts
//! can be deleted (#196).
//!
//! A report to a descriptor (`/dev/stdout`, `/dev/fd/N`, whatever it is redirected to) or to
//! something else that isn't a regular file (a named pipe) is a stream: nothing can be renamed
//! there, and only the exit status tells whether it finished.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
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

/// The rows of the report at `path`, by path. Lines that don't parse are skipped.
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

/// `path` with `suffix` after its file name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(OsString::from).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// Where a run writes the report it will finish at `path`.
pub fn partial_path(path: &Path) -> PathBuf {
    beside(path, ".partial")
}

/// Where a run keeps the finished report it found at `path`, until it finishes.
pub fn previous_path(path: &Path) -> PathBuf {
    beside(path, ".previous")
}

/// The open descriptor of this process that `path` names (`/dev/stdout`, `/dev/stderr`,
/// `/dev/fd/N`, `/proc/self/fd/N`, or a link to one), whatever it is open on: the links are
/// followed one at a time, as following them all ends at a redirected-to file.
fn descriptor(path: &Path) -> Option<u32> {
    let fds = Path::new("/proc").join(std::process::id().to_string()).join("fd");
    let mut p = path.to_owned();
    for _ in 0..8 {
        let parent = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_owned();
        if fs::canonicalize(&parent).is_ok_and(|d| d == fds) {
            return p.file_name()?.to_str()?.parse().ok();
        }
        p = parent.join(fs::read_link(&p).ok()?);
    }
    None
}

/// Whether a report to `path` is written straight there: it is a descriptor, or exists and is
/// no regular file.
fn is_stream(path: &Path) -> bool {
    descriptor(path).is_some() || fs::metadata(path).is_ok_and(|m| !m.is_file())
}

/// Opens a report that [`is_stream`]. Standard output and error are written through themselves,
/// so that a file behind them keeps one position and what else goes there overwrites nothing.
/// Anything else is opened anew, and adds to what it holds.
fn open_stream(path: &Path) -> io::Result<Box<dyn Write + Send>> {
    Ok(match descriptor(path) {
        Some(1) => Box::new(io::stdout()),
        Some(2) => Box::new(io::stderr()),
        _ => Box::new(OpenOptions::new().append(true).open(path)?),
    })
}

/// What `--resume` starts from, each over the one before: a finished report that a stopped run
/// moved aside, a finished report, a stopped run's rows. A row is used only if its file's size
/// and modification time are unchanged, so an older one does no harm.
pub fn read_earlier(path: &Path) -> io::Result<HashMap<PathBuf, Row>> {
    let mut out = HashMap::new();
    if is_stream(path) {
        return Ok(out);
    }
    for p in [previous_path(path), path.to_owned(), partial_path(path)] {
        if p.exists() {
            out.extend(read(&p)?);
        }
    }
    Ok(out)
}

/// Removes a file a finished report makes useless. The report is done by then, so failing
/// here is only worth a word.
fn remove_leftover(path: &Path) {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => log::warn!("{}: {e}", path.display()),
        _ => {}
    }
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
    /// A report to `path`, or to standard output: the header row and `kept`, the rows kept
    /// from an earlier report. A regular file is written as [`partial_path`], and the report
    /// that was at `path` moves to [`previous_path`] once those rows are safe.
    pub fn create(path: Option<&Path>, kept: &[Row]) -> io::Result<Writer> {
        let (out, finished): (Box<dyn Write + Send>, _) = match path {
            Some(p) if is_stream(p) => (open_stream(p)?, None),
            Some(p) => (Box::new(BufWriter::new(File::create(partial_path(p))?)), Some(p)),
            None => (Box::new(io::stdout()), None),
        };
        let mut w = Writer { path: finished.map(Path::to_owned), out };
        writeln!(w.out, "{}", COLUMNS.join("\t"))?;
        for r in kept {
            writeln!(w.out, "{}", r.line())?;
        }
        w.out.flush()?;
        if let Some(p) = finished.filter(|p| p.exists()) {
            let previous = previous_path(p);
            fs::rename(p, &previous)?;
            log::warn!(
                "{}: the earlier report is now {}, until this run finishes; this run's rows so far are in {}",
                p.display(),
                previous.display(),
                partial_path(p).display()
            );
        }
        Ok(w)
    }

    /// Writes one row, flushed.
    pub fn row(&mut self, r: &Row) -> io::Result<()> {
        writeln!(self.out, "{}", r.line())?;
        self.out.flush()
    }

    /// Finishes the report, every file checked: a file gets its own name, with `rows`, sorted,
    /// at once, and what stood in for it goes.
    pub fn finish(mut self, rows: &[Row]) -> io::Result<()> {
        self.out.flush()?;
        let Some(path) = self.path else { return Ok(()) };
        let tmp = beside(&path, ".tmp");
        {
            let mut w = BufWriter::new(File::create(&tmp)?);
            writeln!(w, "{}", COLUMNS.join("\t"))?;
            for r in rows {
                writeln!(w, "{}", r.line())?;
            }
            w.flush()?;
        }
        fs::rename(&tmp, &path)?;
        remove_leftover(&partial_path(&path));
        remove_leftover(&previous_path(&path));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_has_its_name_only_when_finished() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.tsv");
        let (partial, previous) = (partial_path(&path), previous_path(&path));
        assert_eq!(partial, dir.path().join("r.tsv.partial"));
        assert_eq!(previous, dir.path().join("r.tsv.previous"));
        let row = |p: &str, st| Row::new(p.into(), 1, "1.5".into()).with(st, String::new());
        let (a, b) = (row("/a", Status::Identical), row("/b", Status::Different));

        // A finished report.
        Writer::create(Some(&path), &[]).unwrap().finish(std::slice::from_ref(&a)).unwrap();
        assert!(path.exists() && !partial.exists() && !previous.exists());
        let finished = fs::read(&path).unwrap();

        // A run that stops: its rows are partial, and the finished report is kept aside.
        let mut w = Writer::create(Some(&path), &[]).unwrap();
        w.row(&b).unwrap();
        drop(w);
        assert!(!path.exists(), "an unfinished run's report under the report's name");
        assert_eq!(fs::read(&previous).unwrap(), finished);
        assert_eq!(read(&partial).unwrap().len(), 1);
        assert_eq!(read_earlier(&path).unwrap().len(), 2, "resuming sees both");

        // Another that stops: there is no report to move, and the one aside stays.
        drop(Writer::create(Some(&path), &[]).unwrap());
        assert_eq!(fs::read(&previous).unwrap(), finished);
        assert_eq!(read(&partial).unwrap().len(), 0);

        // One that finishes leaves its report alone.
        Writer::create(Some(&path), &[]).unwrap().finish(&[a.clone(), b.clone()]).unwrap();
        assert!(path.exists() && !partial.exists() && !previous.exists());
        assert_eq!(read_earlier(&path).unwrap().len(), 2);

        // A later source wins: finished report under a stopped run's rows.
        fs::write(&partial, format!("{}\n", row("/a", Status::Error).line())).unwrap();
        let earlier = read_earlier(&path).unwrap();
        assert_eq!(earlier[Path::new("/a")].status, Status::Error);
        assert_eq!(earlier[Path::new("/b")].status, Status::Different);
    }

    #[test]
    fn a_report_to_a_stream_is_written_there() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR).unwrap();
        assert!(read_earlier(&fifo).unwrap().is_empty(), "reading it would block");
        let reader = {
            let fifo = fifo.clone();
            std::thread::spawn(move || fs::read_to_string(fifo).unwrap())
        };
        let row = Row::new("/a".into(), 1, "1.5".into()).with(Status::Identical, String::new());
        let mut w = Writer::create(Some(&fifo), &[]).unwrap();
        w.row(&row).unwrap();
        w.finish(std::slice::from_ref(&row)).unwrap();
        let text = reader.join().unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("/a"));
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["pipe"], "nothing beside it, and it is still the pipe");
        assert!(is_stream(&fifo));
    }

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
