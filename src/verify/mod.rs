// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs verify` (#80): checks extracted DSF and DFF files against what sacdfs serves for
//! the same disc, file by file, before anyone deletes them. It never deletes or writes
//! anything but its report.
//!
//! Each extract is reduced to its canonical DSD stream (see [`container`]) and its SHA-256
//! compared with that of the matching sacdfs DSF file, read in-process through the same
//! code the mount serves files with ([`Tree`] and [`Reader`]), and reduced by the same
//! reader. A match is looked for among the tracks of the images in the extract's folder
//! (or the nearest folder above it that holds images, after `--map`), with the same channel
//! count and sample count; only those candidates are decoded. The tags are compared too, to
//! tell what a deletion would lose.

pub mod container;
pub mod id3;
mod report;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use clap::Parser;

use crate::disc::Tuning;
use crate::engine::{Engine, default_threads};
use crate::track::{Format, Reader, TrackFile};
use crate::tree::{DirKind, Node, Output, SourceDir, Tree};
use container::{FileSource, Problem, Source};
use id3::Losses;
pub use report::{Row, Status};

/// SACD's sampling frequency.
const SACD_RATE: u32 = 2_822_400;

/// Most directory levels walked below an EXTRACTS directory.
const MAX_DEPTH: usize = 64;

/// Check extracted DSF and DFF files against what sacdfs serves, before deleting them.
///
/// For every DSF or DFF file under EXTRACTS (default: SOURCE, for extracts kept next to
/// their images), finds the track of the same disc with the same channel and sample counts
/// among the files sacdfs would show for SOURCE, and compares the audio, ignoring tags,
/// padding, bit order and container. Each file gets a status: IDENTICAL, DIFFERENT,
/// NO-MATCH, UNSUPPORTED, ERROR or (FLAC and WAV) NOT-COMPARABLE. Writes a TSV report and a
/// summary of the bytes that deleting the IDENTICAL files would reclaim, and of the tags
/// they hold that sacdfs's files don't. Nothing is deleted or changed.
///
/// Exit status: 0 if every DSF and DFF file is IDENTICAL, 2 if any is not, 1 on failure.
#[derive(Debug, Parser)]
#[command(name = "sacdfs verify", version)]
pub struct Args {
    /// An SACD image file, or a directory of them (as for mounting).
    pub source: PathBuf,
    /// Extracted files, or directories searched recursively for them (default: SOURCE).
    pub extracts: Vec<PathBuf>,
    /// Extracts under FROM belong to the images under TO (a directory in SOURCE), for
    /// extracts kept in a tree of their own; repeat for more. The longest FROM that applies
    /// wins.
    #[arg(long, num_args = 2, value_names = ["FROM", "TO"], action = clap::ArgAction::Append)]
    pub map: Vec<PathBuf>,
    /// Write the TSV report to FILE instead of standard output.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Keep the rows of an earlier --output report for files whose size and modification
    /// time are unchanged (all but ERROR rows), and check only the rest.
    #[arg(long, requires = "output")]
    pub resume: bool,
    /// Files checked at once, and DST decoder threads (0: one per CPU).
    #[arg(long, default_value_t = 0, value_name = "N")]
    pub threads: usize,
    /// Size of the cache of decoded frames, in MiB.
    #[arg(long, default_value_t = 64, value_name = "N")]
    pub cache_mb: usize,
    /// No progress line on standard error.
    #[arg(short, long)]
    pub quiet: bool,
    /// Log more (repeat for more still); RUST_LOG overrides it.
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

/// `sacdfs verify`: `args` starts with "verify".
pub fn main(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let args = Args::parse_from(args);
    let level = match args.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).try_init();
    match run(&args) {
        Ok(summary) => {
            eprint!("{}", summary.render());
            if summary.all_identical() { ExitCode::SUCCESS } else { ExitCode::from(2) }
        }
        Err(e) => {
            eprintln!("sacdfs verify: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A track sacdfs serves, as a candidate for extracts.
#[derive(Debug)]
struct Candidate {
    file: Arc<TrackFile>,
    /// "Stereo" or "Multichannel".
    area: String,
    digest: OnceLock<Result<[u8; 32], String>>,
    tag: OnceLock<Vec<id3::Frame>>,
}

impl Candidate {
    fn image(&self) -> &Path {
        &self.file.disc.path
    }

    /// The SHA-256 of the served file's canonical audio, read once.
    fn digest(&self, engine: &Engine, read: &AtomicU64) -> Result<[u8; 32], String> {
        self.digest
            .get_or_init(|| {
                let mut src = TrackSource::new(self.file.clone(), engine);
                let audio = container::parse(&mut src).map_err(problem_text)?;
                audio.digest(&mut src, read).map_err(problem_text)
            })
            .clone()
    }

    /// The frames of the served file's tag.
    fn tag(&self, engine: &Engine) -> &[id3::Frame] {
        self.tag.get_or_init(|| {
            let mut src = TrackSource::new(self.file.clone(), engine);
            match container::parse(&mut src).map(|a| a.read_id3(&mut src)) {
                Ok(Ok(Some(t))) => id3::frames(&t),
                _ => Vec::new(),
            }
        })
    }
}

fn problem_text(p: Problem) -> String {
    match p {
        Problem::Unsupported(s) | Problem::Error(s) => s,
    }
}

/// A track file served by sacdfs, read as the mount reads it.
struct TrackSource<'a> {
    reader: Reader,
    engine: &'a Engine,
    len: u64,
}

impl<'a> TrackSource<'a> {
    fn new(file: Arc<TrackFile>, engine: &'a Engine) -> Self {
        let len = file.size;
        TrackSource { reader: Reader::new(file), engine, len }
    }
}

impl Source for TrackSource<'_> {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let at = offset + done as u64;
            let (data, _) =
                self.reader.read(self.engine, at, (buf.len() - done) as u64).map_err(io::Error::other)?;
            if data.is_empty() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "past the end of the file"));
            }
            let n = data.len().min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&data[..n]);
            done += n;
        }
        Ok(())
    }
}

/// What kind of extract a file is, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExtractKind {
    Dsd,
    Pcm(&'static str),
}

fn extract_kind(path: &Path) -> Option<ExtractKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "dsf" | "dff" => Some(ExtractKind::Dsd),
        "flac" => Some(ExtractKind::Pcm("FLAC")),
        "wav" => Some(ExtractKind::Pcm("WAV")),
        _ => None,
    }
}

/// One extract to check.
#[derive(Debug)]
struct Item {
    path: PathBuf,
    kind: ExtractKind,
    bytes: u64,
    mtime: String,
    /// The folder of images it belongs to.
    folder: Option<PathBuf>,
}

/// The file's modification time as the report writes it.
fn mtime_text(m: &fs::Metadata) -> String {
    match m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()) {
        Some(d) => format!("{}.{:09}", d.as_secs(), d.subsec_nanos()),
        None => "0".to_owned(),
    }
}

/// The mount points of sacdfs mounts.
fn sacdfs_mounts() -> Vec<PathBuf> {
    let info = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    info.lines()
        .filter_map(|l| {
            let mp = l.split(' ').nth(4)?;
            let mut tail = l.split(" - ").nth(1)?.split(' ');
            let (fstype, source) = (tail.next()?, tail.next()?);
            let ours = fstype == "fuse.sacdfs" || (fstype.starts_with("fuse") && source == "sacdfs");
            ours.then(|| PathBuf::from(unescape_octal(mp)))
        })
        .collect()
}

/// Undoes mountinfo's octal escapes (`\040` for a space).
fn unescape_octal(s: &str) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = b[i + 1..i + 4].iter().fold(0u32, |a, &c| a * 8 + u32::from(c - b'0'));
            out.push((v & 0xFF) as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    OsString::from_vec(out)
}

/// Every extract under `root`, sorted by path.
fn walk(
    root: &Path,
    mounts: &[PathBuf],
    out: &mut Vec<(PathBuf, ExtractKind, fs::Metadata)>,
) -> io::Result<()> {
    let meta = fs::metadata(root)?;
    if meta.is_file() {
        if let Some(k) = extract_kind(root) {
            out.push((root.to_owned(), k, meta));
        }
        return Ok(());
    }
    let mut stack = vec![(root.to_owned(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                log::warn!("{}: {e}", dir.display());
                continue;
            }
        };
        for e in entries.filter_map(Result::ok) {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() {
                if depth + 1 < MAX_DEPTH && !mounts.contains(&p) {
                    stack.push((p, depth + 1));
                }
            } else if let Some(k) = extract_kind(&p) {
                // Links to files are followed, links to directories are not.
                if let Ok(m) = fs::metadata(&p) {
                    if m.is_file() {
                        out.push((p, k, m));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Folders of `dir` that hold images directly.
fn image_folders(dir: &SourceDir, out: &mut HashSet<PathBuf>) {
    if !dir.images.is_empty() {
        out.insert(dir.path.clone());
    }
    for (_, d) in &dir.dirs {
        image_folders(d, out);
    }
}

/// The folder of images that `path` belongs to: after mapping, the nearest folder above it
/// that holds images, within `root`.
fn folder_of(
    path: &Path,
    maps: &[(PathBuf, PathBuf)],
    root: &Path,
    folders: &HashSet<PathBuf>,
) -> Option<PathBuf> {
    let mapped = maps
        .iter()
        .filter(|(from, _)| path.starts_with(from))
        .max_by_key(|(from, _)| from.components().count())
        .and_then(|(from, to)| path.strip_prefix(from).ok().map(|rest| to.join(rest)))
        .unwrap_or_else(|| path.to_owned());
    for anc in mapped.ancestors().skip(1) {
        if !anc.starts_with(root) {
            break;
        }
        if folders.contains(anc) {
            return Some(anc.to_owned());
        }
    }
    None
}

/// The DSF files sacdfs shows for the images directly in `folder`.
fn folder_candidates(tree: &Tree, root: &Path, folder: &Path) -> Vec<Arc<Candidate>> {
    let mut out = Vec::new();
    let Ok(rel) = folder.strip_prefix(root) else { return out };
    // Down to the folder by name, checking that each step is the source directory meant
    // (de-duplication never renames a source directory: they come first in a listing).
    let mut ino = crate::tree::ROOT;
    let mut at = root.to_owned();
    for part in rel.components() {
        at.push(part);
        let Some(next) = tree.lookup(ino, part.as_os_str()) else { return out };
        match tree.get(next).as_deref() {
            Some(Node::Dir(d)) if matches!(&d.kind, DirKind::Source(s) if s.path == at) => ino = next,
            _ => return out,
        }
    }
    let Some(entries) = tree.children(ino) else { return out };
    for image in entries.iter().filter(|e| e.is_dir) {
        let is_image = matches!(tree.get(image.ino).as_deref(),
            Some(Node::Dir(d)) if matches!(&d.kind, DirKind::Image(disc) if disc.path.parent() == Some(folder)));
        if !is_image {
            continue;
        }
        for area in tree.children(image.ino).iter().flat_map(|c| c.iter()).filter(|e| e.is_dir) {
            for f in tree.children(area.ino).iter().flat_map(|c| c.iter()) {
                if let Some(Node::File(file)) = tree.get(f.ino).as_deref() {
                    if file.format() == Format::Dsf {
                        out.push(Arc::new(Candidate {
                            file: file.clone(),
                            area: area.name.to_string_lossy().into_owned(),
                            digest: OnceLock::new(),
                            tag: OnceLock::new(),
                        }));
                    }
                }
            }
        }
    }
    out
}

/// The track number a file name starts with, if any.
fn leading_number(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let digits: String = name.chars().take_while(char::is_ascii_digit).take(3).collect();
    digits.parse().ok()
}

/// Shared state of a run.
struct Ctx<'a> {
    engine: &'a Engine,
    candidates: &'a HashMap<PathBuf, Vec<Arc<Candidate>>>,
    /// Bytes read so far, from extracts and from sacdfs.
    read: AtomicU64,
}

/// Checks one DSD extract.
fn check(item: &Item, ctx: &Ctx<'_>) -> Row {
    let mut row = Row::new(item.path.clone(), item.bytes, item.mtime.clone());
    let mut src = match FileSource::open(&item.path) {
        Ok(s) => s,
        Err(e) => return row.with(Status::Error, format!("cannot open: {e}")),
    };
    let audio = match container::parse(&mut src) {
        Ok(a) => a,
        Err(Problem::Unsupported(s)) => return row.with(Status::Unsupported, s),
        Err(Problem::Error(s)) => return row.with(Status::Error, s),
    };
    row.channels = Some(audio.channels);
    row.samples = Some(audio.samples);
    if audio.rate != SACD_RATE {
        return row
            .with(Status::NoMatch, format!("sampled at {} Hz; SACD audio is {SACD_RATE} Hz", audio.rate));
    }
    let Some(folder) = &item.folder else {
        return row.with(
            Status::NoMatch,
            "no image in its folder or the folders above it in SOURCE (for extracts kept apart, see --map)"
                .into(),
        );
    };
    let all = ctx.candidates.get(folder).map_or(&[][..], Vec::as_slice);
    if all.is_empty() {
        return row
            .with(Status::NoMatch, format!("sacdfs shows no track for the images in {}", folder.display()));
    }
    let same_ch: Vec<&Arc<Candidate>> =
        all.iter().filter(|c| c.file.layout.channels == audio.channels).collect();
    let mut shape: Vec<&Arc<Candidate>> =
        same_ch.iter().copied().filter(|c| c.file.layout.samples() == audio.samples).collect();
    if shape.is_empty() {
        let why = match same_ch.iter().min_by_key(|c| c.file.layout.samples().abs_diff(audio.samples)) {
            None => format!("no area of the images in {} has {} channels", folder.display(), audio.channels),
            Some(c) => {
                let d = c.file.layout.samples() as f64 - audio.samples as f64;
                format!(
                    "no {}-channel track has {} samples; the nearest, {} track {} of {}, is {:+.3} s {}",
                    audio.channels,
                    audio.samples,
                    c.area,
                    c.file.number,
                    c.image().display(),
                    d.abs() / f64::from(SACD_RATE),
                    if d > 0.0 { "longer" } else { "shorter" },
                )
            }
        };
        return row.with(Status::NoMatch, why);
    }
    // The track the name points at first: usually the only one decoded.
    let n = leading_number(&item.path);
    shape.sort_by_key(|c| (Some(u32::from(c.file.number)) != n, c.area.clone(), c.file.number));
    let digest = match audio.digest(&mut src, &ctx.read) {
        Ok(d) => d,
        Err(p) => return row.with(Status::Error, problem_text(p)),
    };
    row.sha256 = id3::hex(&digest);
    let mut failed = None;
    let mut matched = None;
    for c in &shape {
        match c.digest(ctx.engine, &ctx.read) {
            Ok(d) if d == digest => {
                matched = Some(*c);
                break;
            }
            Ok(_) => {}
            Err(e) => failed = Some((*c, e)),
        }
    }
    let (status, cand, reason) = match (matched, failed) {
        (Some(c), _) => (Status::Identical, c, String::new()),
        (None, Some((c, e))) => (Status::Error, c, format!("sacdfs could not read its track: {e}")),
        (None, None) => {
            let c = shape[0];
            let why = if shape.len() == 1 {
                "the audio differs from the only track of the same channel and sample counts".to_owned()
            } else {
                format!(
                    "the audio differs from all {} tracks of the same channel and sample counts",
                    shape.len()
                )
            };
            (Status::Different, c, why)
        }
    };
    row.image = Some(cand.image().to_owned());
    row.area = cand.area.clone();
    row.track = Some(cand.file.number);
    row.loses = losses(&audio, &mut src, cand.tag(ctx.engine));
    row.with(status, reason)
}

/// What the extract's tags hold that the served file's tag does not.
fn losses(audio: &container::Audio, src: &mut dyn Source, served: &[id3::Frame]) -> Losses {
    let mut l = match audio.read_id3(src) {
        Ok(Some(t)) => Losses::between(&id3::frames(&t), served),
        Ok(None) => Losses::default(),
        Err(size) => Losses { unread: Some(format!("a tag of {size} bytes")), ..Losses::default() },
    };
    let served_text = |key: &str| served.iter().find(|f| f.key == key).map(|f| f.value.as_str());
    let t = &audio.tags;
    if t.dff_title.as_deref().is_some_and(|v| Some(v) != served_text("TIT2")) {
        l.add("DIIN/DITI");
    }
    if t.dff_artist.as_deref().is_some_and(|v| Some(v) != served_text("TPE1")) {
        l.add("DIIN/DIAR");
    }
    for o in &t.dff_other {
        l.add(o);
    }
    l
}

/// The outcome of a run.
#[derive(Debug, Default)]
pub struct Summary {
    /// Every row, sorted by path.
    pub rows: Vec<Row>,
}

impl Summary {
    /// Whether every DSF and DFF file is IDENTICAL.
    pub fn all_identical(&self) -> bool {
        self.rows.iter().all(|r| matches!(r.status, Status::Identical | Status::NotComparable))
    }

    /// The human summary.
    pub fn render(&self) -> String {
        let mut by: BTreeMap<Status, (usize, u64)> = BTreeMap::new();
        for r in &self.rows {
            let e = by.entry(r.status).or_default();
            e.0 += 1;
            e.1 += r.bytes;
        }
        let mut s = String::new();
        let dsd = self.rows.iter().filter(|r| r.status != Status::NotComparable).count();
        s.push_str(&format!("sacdfs verify: {} files, {dsd} of them DSF or DFF\n", self.rows.len()));
        for st in Status::ALL {
            let (n, b) = by.get(&st).copied().unwrap_or_default();
            s.push_str(&format!("  {:<15} {n:>7}  {:>10}\n", st.as_str(), human(b)));
        }
        let ident: Vec<&Row> = self.rows.iter().filter(|r| r.status == Status::Identical).collect();
        let reclaim: u64 = ident.iter().map(|r| r.bytes).sum();
        s.push_str(&format!(
            "Reclaimable: {} in {} IDENTICAL files (sacdfs serves the same audio).\n",
            human(reclaim),
            ident.len()
        ));
        let count = |f: &dyn Fn(&Losses) -> bool| ident.iter().filter(|r| f(&r.loses)).count();
        let (mb, art, rg) = (count(&|l| l.musicbrainz), count(&|l| l.art), count(&|l| l.replaygain));
        let other = count(&|l| !l.other.is_empty() || l.unread.is_some());
        if mb + art + rg + other > 0 {
            s.push_str("Deleting them would lose tags sacdfs does not serve:\n");
            for (what, n) in
                [("MusicBrainz IDs", mb), ("embedded art", art), ("ReplayGain", rg), ("other tags", other)]
            {
                if n > 0 {
                    s.push_str(&format!("  {what:<16} {n:>7} files\n"));
                }
            }
            let mut keys: BTreeMap<&str, usize> = BTreeMap::new();
            for r in &ident {
                for k in &r.loses.other {
                    *keys.entry(k.as_str()).or_default() += 1;
                }
            }
            let mut keys: Vec<(&str, usize)> = keys.into_iter().collect();
            keys.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            if !keys.is_empty() {
                let top: Vec<String> = keys.iter().take(8).map(|(k, n)| format!("{k} ({n})")).collect();
                s.push_str(&format!("  other tags, most common: {}\n", top.join(", ")));
            }
        }
        let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
        for r in self.rows.iter().filter(|r| r.status == Status::Unsupported) {
            *reasons.entry(r.reason.as_str()).or_default() += 1;
        }
        for (why, n) in reasons {
            s.push_str(&format!("UNSUPPORTED ({n}): {why}\n"));
        }
        s.push_str("Nothing was deleted. Keep every file that is not IDENTICAL; see the report for each.\n");
        s
    }
}

fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < U.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
}

/// Runs the check: writes the report and returns every row.
pub fn run(args: &Args) -> io::Result<Summary> {
    let with = |e: io::Error, p: &Path| io::Error::new(e.kind(), format!("{}: {e}", p.display()));
    let source = args.source.canonicalize().map_err(|e| with(e, &args.source))?;
    let mounts = sacdfs_mounts();
    let roots: Vec<PathBuf> = if args.extracts.is_empty() {
        vec![source.clone()]
    } else {
        args.extracts.iter().map(|p| p.canonicalize().map_err(|e| with(e, p))).collect::<io::Result<_>>()?
    };
    for r in &roots {
        if let Some(m) = mounts.iter().find(|m| r.starts_with(m)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is on a sacdfs mount ({}): its files are what verify compares with, not extracts",
                    r.display(),
                    m.display()
                ),
            ));
        }
    }
    let mut maps = Vec::new();
    for pair in args.map.chunks(2) {
        let [from, to] = pair else { continue };
        // FROM may be given as it is written in the extract paths, canonical or not.
        let from = from.canonicalize().unwrap_or_else(|_| from.clone());
        let to = to.canonicalize().map_err(|e| with(e, to))?;
        if !to.starts_with(&source) && !source.starts_with(&to) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("--map: {} is not in SOURCE {}", to.display(), source.display()),
            ));
        }
        maps.push((from, to));
    }
    let scanned = SourceDir::scan(&source).map_err(|e| with(e, &source))?;
    let root = scanned.path.clone();
    let mut folders = HashSet::new();
    image_folders(&scanned, &mut folders);

    let mut found = Vec::new();
    for r in &roots {
        walk(r, &mounts, &mut found).map_err(|e| with(e, r))?;
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.dedup_by(|a, b| a.0 == b.0);

    let earlier = match (&args.output, args.resume) {
        (Some(p), true) if p.exists() => report::read(p)?,
        _ => HashMap::new(),
    };
    let mut kept = Vec::new();
    let mut items = Vec::new();
    for (path, kind, meta) in found {
        let mtime = mtime_text(&meta);
        if let Some(r) = earlier.get(&path) {
            if r.bytes == meta.len() && r.mtime == mtime && r.status != Status::Error {
                kept.push(r.clone());
                continue;
            }
        }
        let folder = folder_of(&path, &maps, &root, &folders);
        items.push(Item { path, kind, bytes: meta.len(), mtime, folder });
    }

    let threads = if args.threads == 0 { default_threads() } else { args.threads };
    let engine = Engine::new(threads, args.cache_mb.saturating_mul(1 << 20))?;
    let tree = Tree::with_options(scanned, Tuning::default(), Output::dsf_only());

    // The images of every folder needed, opened up front, on this thread: a worker never
    // waits for another to open them.
    let needed: Vec<PathBuf> = {
        let mut f: Vec<PathBuf> = items
            .iter()
            .filter(|i| i.kind == ExtractKind::Dsd)
            .filter_map(|i| i.folder.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        f.sort();
        f
    };
    let progress = !args.quiet;
    let mut candidates = HashMap::new();
    for (i, f) in needed.iter().enumerate() {
        if progress && io::stderr().is_terminal() {
            eprint!("\rsacdfs verify: opening images, folder {}/{}", i + 1, needed.len());
        }
        candidates.insert(f.clone(), folder_candidates(&tree, &root, f));
    }
    if progress && io::stderr().is_terminal() && !needed.is_empty() {
        eprintln!();
    }

    let mut out = report::Writer::create(args.output.as_deref())?;
    for r in &kept {
        out.row(r)?;
    }
    let out = Mutex::new(out);
    let ctx = Ctx { engine: &engine, candidates: &candidates, read: AtomicU64::new(0) };
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let rows = Mutex::new(Vec::with_capacity(items.len()));
    let write_err: Mutex<Option<io::Error>> = Mutex::new(None);
    let total = items.len();
    let workers = threads.clamp(1, total.max(1));
    std::thread::scope(|s| {
        if progress {
            s.spawn(|| progress_loop(&done, total, kept.len(), &ctx.read, &stop));
        }
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        let row = match item.kind {
                            ExtractKind::Dsd => check(item, &ctx),
                            ExtractKind::Pcm(what) => {
                                Row::new(item.path.clone(), item.bytes, item.mtime.clone())
                                    .with(Status::NotComparable, pcm_reason(what))
                            }
                        };
                        let mut w = out.lock().unwrap_or_else(|p| p.into_inner());
                        if let Err(e) = w.row(&row) {
                            write_err.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert(e);
                        }
                        drop(w);
                        rows.lock().unwrap_or_else(|p| p.into_inner()).push(row);
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for h in handles {
            let _ = h.join();
        }
        stop.store(true, Ordering::Relaxed);
    });
    if let Some(e) = write_err.into_inner().unwrap_or_else(|p| p.into_inner()) {
        return Err(e);
    }
    let mut rows = rows.into_inner().unwrap_or_else(|p| p.into_inner());
    rows.extend(kept);
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    out.into_inner().unwrap_or_else(|p| p.into_inner()).finish(&rows)?;
    Ok(Summary { rows })
}

fn pcm_reason(what: &str) -> String {
    format!(
        "{what} is PCM: a CD-layer rip is not in the SACD image at all, and a PCM conversion of the \
         SACD layer by another converter never matches sacdfs's WAV bit for bit"
    )
}

/// Prints progress to standard error until `stop`: in place on a terminal, else a line
/// every 30 s.
fn progress_loop(done: &AtomicUsize, total: usize, kept: usize, read: &AtomicU64, stop: &AtomicBool) {
    let tty = io::stderr().is_terminal();
    let every = if tty { Duration::from_secs(1) } else { Duration::from_secs(30) };
    let start = Instant::now();
    let mut last = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
        if last.elapsed() < every || stop.load(Ordering::Relaxed) {
            continue;
        }
        last = Instant::now();
        let b = read.load(Ordering::Relaxed);
        let rate = b as f64 / start.elapsed().as_secs_f64().max(0.001) / (1 << 20) as f64;
        let line = format!(
            "sacdfs verify: {}/{total} files checked{}, {} of audio read ({rate:.0} MiB/s)",
            done.load(Ordering::Relaxed),
            if kept > 0 { format!(" ({kept} kept from the earlier report)") } else { String::new() },
            human(b)
        );
        let mut e = io::stderr().lock();
        let _ = if tty { write!(e, "\r{line}") } else { writeln!(e, "{line}") };
    }
    if tty {
        let _ = writeln!(io::stderr());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_and_maps() {
        let folders: HashSet<PathBuf> = ["/lib/a/x", "/lib/b"].iter().map(PathBuf::from).collect();
        let root = Path::new("/lib");
        let none: Vec<(PathBuf, PathBuf)> = Vec::new();
        assert_eq!(
            folder_of(Path::new("/lib/a/x/Stereo/01.dsf"), &none, root, &folders),
            Some("/lib/a/x".into())
        );
        assert_eq!(folder_of(Path::new("/lib/a/y/01.dsf"), &none, root, &folders), None);
        assert_eq!(folder_of(Path::new("/lib/b/01.dsf"), &none, root, &folders), Some("/lib/b".into()));
        let maps = vec![
            (PathBuf::from("/ext"), PathBuf::from("/lib")),
            (PathBuf::from("/ext/z"), PathBuf::from("/lib/b")),
        ];
        assert_eq!(folder_of(Path::new("/ext/a/x/01.dsf"), &maps, root, &folders), Some("/lib/a/x".into()));
        assert_eq!(folder_of(Path::new("/ext/z/q/01.dsf"), &maps, root, &folders), Some("/lib/b".into()));
        assert_eq!(folder_of(Path::new("/elsewhere/01.dsf"), &maps, root, &folders), None);
    }

    #[test]
    fn mountinfo_escapes() {
        assert_eq!(unescape_octal(r"/a\040b"), OsString::from("/a b"));
        assert_eq!(unescape_octal(r"/a\04"), OsString::from(r"/a\04"));
    }

    #[test]
    fn numbers_from_names() {
        assert_eq!(leading_number(Path::new("/x/07 - a.dsf")), Some(7));
        assert_eq!(leading_number(Path::new("/x/a.dsf")), None);
    }
}
