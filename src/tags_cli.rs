// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs tags` (#81): the disc key of an image; `import`, which copies the tags of extracts
//! that `sacdfs verify` matches into the tags directory, so that the extracts can be deleted
//! without losing them (#82); and `fetch`, which looks a disc up on MusicBrainz, the only part of
//! sacdfs that uses the network ([`crate::lookup`], #185). Nothing but the tags directory is
//! written.

#![warn(clippy::cast_possible_truncation)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use scarletbook::Slot;

use crate::disc::{Disc, Tuning};
use crate::lookup;
use crate::safe_file::{self, shown};
use crate::tags::{self, ART, Fields, Key, TagsFile};
use crate::track::TrackFile;
use crate::verify::{self, container, id3};

/// User tags for the files sacdfs shows: see docs/user-guide.md, "Tags".
#[derive(Debug, Parser)]
#[command(name = "sacdfs tags", version)]
pub struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Print each image's disc key: its tags file is DIR/KEY.toml.
    Key {
        /// The tags directory, to print the tags file's path and whether it exists.
        #[arg(long, value_name = "DIR")]
        tags: Option<PathBuf>,
        /// SACD image files.
        #[arg(required = true)]
        images: Vec<PathBuf>,
    },
    /// Write tags files from the tags of extracts that `sacdfs verify` finds identical:
    /// what deleting them would lose (genres, MusicBrainz IDs, art, corrected names). Takes
    /// verify's arguments (SOURCE, EXTRACTS..., --map, --output, --resume ...). A tags file
    /// that exists keeps its values; only keys it lacks are added. Writes nothing but the
    /// tags directory.
    Import {
        /// The tags directory (created if missing).
        #[arg(long, value_name = "DIR")]
        tags: PathBuf,
        #[command(flatten)]
        verify: verify::Args,
    },
    /// Look discs up on MusicBrainz and write what matches to their tags files. This goes on
    /// the network, at 1 request per second; nothing else in sacdfs does. By barcode or catalogue
    /// number, confirmed by track count, durations and ISRCs. No match, or an ambiguous one,
    /// writes nothing and says why. Values in a tags file and the disc's own text are kept.
    Fetch {
        /// The tags directory (created if missing).
        #[arg(long, value_name = "DIR")]
        tags: PathBuf,
        /// If that finds nothing, search by album and artist, from the disc's text or the
        /// image's path. Sends those names.
        #[arg(long)]
        lookup_name: bool,
        /// If that finds nothing, ask by the tracks' start times (a disc of 4 tracks or more).
        /// Sends the times.
        #[arg(long)]
        lookup_durations: bool,
        /// If that finds nothing, identify the disc by its audio: fingerprint the first two minutes
        /// of each stereo track with Chromaprint's `fpcalc` and look them up in AcoustID. Sends the
        /// fingerprints, the tracks' durations and your AcoustID application key
        /// (ACOUSTID_API_KEY or --acoustid-key-file). Runs at low priority, one disc at a time,
        /// at most --acoustid-budget discs a day.
        #[arg(long, visible_alias = "lookup-acoustid")]
        acoustid: bool,
        /// A file holding the AcoustID application key, instead of ACOUSTID_API_KEY.
        #[arg(long, value_name = "FILE", requires = "acoustid")]
        acoustid_key_file: Option<PathBuf>,
        /// Discs fingerprinted a day, at most; what is left waits for the next run. The count is
        /// kept in the tags directory.
        #[arg(long, value_name = "N", default_value_t = lookup::acoustid::DEFAULT_BUDGET, requires = "acoustid")]
        acoustid_budget: usize,
        /// Wait while the 1-minute load average is over this (default: the number of CPUs).
        #[arg(long, value_name = "LOAD", requires = "acoustid")]
        max_load: Option<f64>,
        /// The `fpcalc` program (Chromaprint's), if not `fpcalc` on PATH.
        #[arg(long, value_name = "PATH", default_value = "fpcalc", requires = "acoustid")]
        fpcalc: PathBuf,
        /// Say what would be written, and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Don't add " (Multichannel)" to the album written for a multichannel area: use this
        /// if your mount has --no-multichannel-album-suffix.
        #[arg(long)]
        no_multichannel_album_suffix: bool,
        /// SACD image files.
        #[arg(required = true)]
        images: Vec<PathBuf>,
    },
}

/// `sacdfs tags`: `args` starts with "tags".
pub fn main(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let cli = Cli::parse_from(args);
    let verbose = match &cli.cmd {
        Cmd::Import { verify, .. } => verify.verbose,
        Cmd::Key { .. } | Cmd::Fetch { .. } => 0,
    };
    let level = ["warn", "info", "debug"].get(usize::from(verbose)).copied().unwrap_or("trace");
    // Escaped where lines are written, as the mount's (#391, #448).
    let _ = crate::printable::install(
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).build(),
    );
    let r = match cli.cmd {
        Cmd::Key { tags, images } => key(tags.as_deref(), &images),
        Cmd::Import { tags, verify } => import(&tags, verify).map(|s| {
            eprint!("{}", safe_file::printable(&s.render()));
            s.all_handled()
        }),
        Cmd::Fetch {
            tags,
            lookup_name,
            lookup_durations,
            acoustid,
            acoustid_key_file,
            acoustid_budget,
            max_load,
            fpcalc,
            dry_run,
            no_multichannel_album_suffix,
            images,
        } => {
            let options = lookup::Options {
                by_name: lookup_name,
                by_durations: lookup_durations,
                dry_run,
                no_suffix: no_multichannel_album_suffix,
                only_wanting: false,
            };
            let mut session = if acoustid {
                match acoustid_session(&tags, acoustid_key_file.as_deref(), acoustid_budget, max_load, fpcalc)
                {
                    Ok(s) => Some(s),
                    Err(e) => {
                        eprintln!("sacdfs tags: {}", safe_file::printable(&e));
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                None
            };
            let mut client = lookup::Client::new(lookup::Http::new());
            let s = lookup::fetch_with(&tags, &images, options, &mut client, session.as_mut(), |line| {
                println!("{}", safe_file::printable(&line));
            });
            eprint!("{}", safe_file::printable(&s.render()));
            if let Some(a) = &session {
                let c = a.counters;
                eprintln!(
                    "acoustid: {} queued, {} fingerprinted, {} skipped (fingerprinted before), {} left for another day; {} tracks, {} requests; {:.1} CPU seconds",
                    c.queued,
                    c.done,
                    c.skipped,
                    c.deferred,
                    c.tracks,
                    c.requests,
                    lookup::acoustid::cpu_seconds()
                );
            }
            Ok(s.all_handled())
        }
    };
    match r {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(e) => {
            eprintln!("sacdfs tags: {}", safe_file::printable(&e.to_string()));
            ExitCode::FAILURE
        }
    }
}

/// The AcoustID side of `tags fetch --acoustid`: the key, a working `fpcalc`, low priority and a
/// stop on SIGINT and SIGTERM.
fn acoustid_session(
    tags: &Path,
    key_file: Option<&Path>,
    budget: usize,
    max_load: Option<f64>,
    fpcalc: PathBuf,
) -> Result<lookup::acoustid::Session, String> {
    use std::sync::atomic::AtomicBool;
    let key = match key_file {
        Some(f) => fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?,
        None => std::env::var("ACOUSTID_API_KEY").map_err(|_| {
            "--acoustid needs an AcoustID application key, in ACOUSTID_API_KEY or --acoustid-key-file (see docs/user-guide.md, \"Looking tags up\")".to_owned()
        })?,
    };
    let key = key.trim().to_owned();
    if key.is_empty() || key.len() > 64 || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("the AcoustID key must be letters and digits only".to_owned());
    }
    let works = std::process::Command::new(&fpcalc)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !works {
        return Err(format!(
            "--acoustid needs Chromaprint's fpcalc, and {} doesn't run: install it (Fedora: chromaprint-tools) or give its path with --fpcalc",
            fpcalc.display()
        ));
    }
    lookup::worker::lower_priority();
    let stop = Arc::new(AtomicBool::new(false));
    // The first stops at the next second of audio or the next disc; a second ends the run now.
    crate::platform::stop_on_signals(&stop).map_err(|e| e.to_string())?;
    let mut config = lookup::acoustid::Config::new(key, fpcalc);
    config.budget = budget;
    if let Some(m) = max_load {
        config.max_load = Some(m);
    }
    fs::create_dir_all(tags).map_err(|e| format!("{}: {e}", tags.display()))?;
    Ok(lookup::acoustid::Session::new(config, Box::new(lookup::Http::new()), tags, stop))
}

fn key(dir: Option<&Path>, images: &[PathBuf]) -> io::Result<bool> {
    for image in images {
        let disc = Disc::open(image, 0, Tuning::default())
            .map_err(|e| io::Error::other(format!("{}: {e}", shown(image))))?;
        let key = tags::disc_key(disc.toc());
        match dir {
            Some(d) => {
                let p = d.join(format!("{key}.toml"));
                let state = if p.exists() { "exists" } else { "none" };
                println!("{key}\t{}\t{state}\t{}", shown(&p), shown(image));
            }
            None => println!("{key}\t{}", shown(image)),
        }
    }
    Ok(true)
}

/// What an import did.
#[derive(Debug, Default)]
pub struct ImportSummary {
    /// Matched extracts whose tags were read.
    pub files: usize,
    /// Tags files written, and how many of them are new.
    pub written: usize,
    /// See `written`.
    pub created: usize,
    /// Keys added to tags files.
    pub keys: usize,
    /// Pictures written.
    pub pictures: usize,
    /// Values and pictures read from matched extracts but not written, because the mount would
    /// not use them ([`MAX_VALUE_BYTES`] and the limits after it, [`tags::MAX_ART_BYTES`]): each
    /// with its extract, disc and reason. Those extracts still
    /// hold something their tags file lacks.
    pub not_kept: Vec<String>,
    /// Discs whose tags file could not be read or written, and matched extracts whose tag could
    /// not be read, and why.
    pub failed: Vec<String>,
    /// Extracts verify did not match or could not check (any status that keeps verify from
    /// exiting 0): nothing was read from them. Their reasons are in `report`.
    pub not_imported: usize,
    /// Where verify's report is.
    pub report: Option<PathBuf>,
    /// Frame keys that were lost but cannot be kept (binary frames).
    pub skipped: BTreeSet<String>,
}

impl ImportSummary {
    /// The human summary.
    pub fn render(&self) -> String {
        let mut s = format!(
            "tags import: {} matched extracts read; {} tags files written ({} new), {} keys added, {} pictures\n",
            self.files, self.written, self.created, self.keys, self.pictures
        );
        if !self.skipped.is_empty() {
            // Each key escaped by itself (a TXXX description can hold any character), and at
            // most 32 of them.
            let mut k: Vec<String> =
                self.skipped.iter().take(32).map(|k| safe_file::one_line(k).into_owned()).collect();
            if self.skipped.len() > 32 {
                k.push(format!("+{} more", self.skipped.len() - 32));
            }
            s.push_str(&format!("not kept (frames the tag layer cannot write): {}\n", k.join(", ")));
        }
        for f in &self.failed {
            s.push_str(&format!("failed: {f}\n"));
        }
        if self.not_imported > 0 {
            let report = self.report.as_deref().map_or(String::new(), |p| format!("; see {}", shown(p)));
            s.push_str(&format!(
                "not imported: {} extracts that verify did not match or could not check{report}\n",
                self.not_imported
            ));
        }
        for n in self.not_kept.iter().take(MAX_LISTED) {
            s.push_str(&format!("not kept: {n}\n"));
        }
        if self.not_kept.len() > MAX_LISTED {
            s.push_str(&format!("not kept: +{} more\n", self.not_kept.len() - MAX_LISTED));
        }
        if !self.not_kept.is_empty() {
            s.push_str("keep those extracts: their tags files lack what is listed as not kept\n");
        }
        s
    }

    /// Whether every extract was handled, which is exit status 0: the same rule as `sacdfs
    /// verify`'s, plus what the import itself could not do (#198).
    pub fn all_handled(&self) -> bool {
        self.failed.is_empty() && self.not_imported == 0 && self.not_kept.is_empty()
    }
}

/// One area's imported fields, by track number.
pub(crate) type AreaFields = BTreeMap<u8, Fields>;

/// The largest value import writes, in bytes.
pub const MAX_VALUE_BYTES: usize = 16 << 10;
/// The most keys import writes for one track (all levels together).
pub const MAX_TRACK_KEYS: usize = 64;
/// The most text import writes for one track, in bytes as `cost` counts them. *Why:* the
/// mount builds each track's tag from these values plus the disc's own text and frames, and
/// refuses a tag over scarletbook's `MAX_TAG_BYTES` (1 MiB) with a picture of up to
/// [`tags::MAX_ART_BYTES`]: that leaves 64 KiB for text, half of it kept for the disc's own.
pub const MAX_TRACK_BYTES: usize = 32 << 10;
/// The most text import adds to one disc's tags file, as `cost` counts it. *Why:* the mount
/// builds every track's tag from that file, so it is kept bounded (under 2 MiB as written).
pub const MAX_DISC_BYTES: usize = (2 << 20) - (64 << 10);
/// The largest tags file import writes, in bytes. *Why:* what import adds merges into the tags
/// file there may already be, which can be large; past this the disc's file is left as it was.
pub const MAX_FILE_BYTES: usize = 3 << 20;
/// The most "not kept" lines the summary lists; the rest are counted.
pub const MAX_LISTED: usize = 32;

/// What `key = value` costs: its bytes, with room for escaping in the tags file and for the
/// frame header in the tag.
fn cost(key: &str, value: &str) -> usize {
    let escaped = value.chars().filter(|c| c.is_control() || matches!(c, '"' | '\\' | '\'')).count();
    key.len() + value.len() + 5 * escaped + 16
}

/// Whether `key = value` may join `fields`, one track's imported values, when the disc's
/// imported values already cost `disc`: `Err` with the reason it is not kept. A value over a
/// limit is left out whole, never cut, and listed, so the extract that has it is kept.
pub(crate) fn fit(fields: &Fields, key: &str, value: &str, disc: usize) -> Result<usize, String> {
    let c = cost(key, value);
    if value.len() > MAX_VALUE_BYTES {
        return Err(format!(
            "{} bytes, over the {} KiB a value can have",
            value.len(),
            MAX_VALUE_BYTES >> 10
        ));
    }
    if fields.len() >= MAX_TRACK_KEYS {
        return Err(format!("over the {MAX_TRACK_KEYS} keys a track can have"));
    }
    let track: usize = fields.iter().map(|(k, v)| cost(k, v)).sum();
    if track + c > MAX_TRACK_BYTES {
        return Err(format!("over the {} KiB of text a track can have", MAX_TRACK_BYTES >> 10));
    }
    if disc + c > MAX_DISC_BYTES {
        return Err(format!("over the {} KiB of text a disc's tags file can get", MAX_DISC_BYTES >> 10));
    }
    Ok(c)
}

/// Runs verify with `args`, then writes the tags of its matched extracts into `dir`.
pub fn import(dir: &Path, mut args: verify::Args) -> io::Result<ImportSummary> {
    fs::create_dir_all(dir)?;
    let dir = dir.canonicalize()?;
    if args.output.is_none() {
        // verify's report, kept next to the tags files as a record of what was imported
        // (hidden, so it is never taken for a tags file).
        args.output = Some(dir.join(".last-import.tsv"));
    }
    let report = verify::run(&args)?;
    let mut by_image: BTreeMap<PathBuf, Vec<&verify::Row>> = BTreeMap::new();
    for r in &report.rows {
        if let (true, Some(image), Some(_)) = (r.status.is_match(), &r.image, r.track) {
            by_image.entry(image.clone()).or_default().push(r);
        }
    }
    let mut sum = ImportSummary {
        not_imported: report
            .rows
            .iter()
            .filter(|r| !r.status.is_match() && r.status != verify::Status::NotComparable)
            .count(),
        report: args.output.clone(),
        ..ImportSummary::default()
    };
    for (image, rows) in by_image {
        if let Err(e) = import_disc(&dir, &image, &rows, &mut sum) {
            sum.failed.push(format!("{}: {e}", shown(&image)));
        }
    }
    Ok(sum)
}

fn import_disc(dir: &Path, image: &Path, rows: &[&verify::Row], sum: &mut ImportSummary) -> io::Result<()> {
    let disc = Arc::new(Disc::open(image, 0, Tuning::default()).map_err(io::Error::other)?);
    let key = tags::disc_key(disc.toc());
    let mut areas: [AreaFields; 2] = Default::default();
    let mut art: Option<Vec<u8>> = None;
    let mut disc_bytes = 0;
    // Where this disc's "not kept" lines start.
    let listed = sum.not_kept.len();
    for row in rows {
        let slot = if row.area == crate::tree::MULTICHANNEL { Slot::Multichannel } else { Slot::TwoChannel };
        let Some(number) = row.track else { continue };
        let Some(i) = disc.toc().areas.iter().position(|a| a.slot == slot) else { continue };
        let area = &disc.toc().areas[i];
        let Some(track) = area.tracks.iter().find(|t| t.number == number) else { continue };
        // Compared with the file as sacdfs serves it without a tags file, and without what
        // comes from the image's path: that is no copy of the extract's tag (#185).
        let served = TrackFile::new(disc.clone(), i, track).map(|f| f.stored_frames()).unwrap_or_default();
        let tag = match extract_tag(&row.path) {
            Ok(Some(tag)) => tag,
            // No tag: nothing to keep.
            Ok(None) => continue,
            Err(e) => {
                sum.failed.push(format!("{}: {e}", shown(&row.path)));
                continue;
            }
        };
        sum.files += 1;
        let frames = id3::frames(&tag);
        let fields = areas[usize::from(slot == Slot::Multichannel)].entry(number).or_default();
        for f in &frames {
            let context: Vec<id3::Frame> = if f.key == "TDRC" || f.key == "TDAT" {
                frames.iter().filter(|g| g.key == "TDRC" || g.key == "TDAT").cloned().collect()
            } else {
                vec![f.clone()]
            };
            if id3::Losses::between(&context, &served, area.tracks.len()).is_empty() {
                continue;
            }
            if f.key == "APIC" {
                if art.is_none() {
                    art = front_cover(&tag).filter(|p| {
                        let fits = p.len() <= tags::MAX_ART_BYTES;
                        if !fits {
                            sum.not_kept.push(format!(
                                "{} (disc {}): picture of {} KiB, over the {} KiB the mount embeds",
                                shown(&row.path),
                                shown(image),
                                p.len() >> 10,
                                tags::MAX_ART_BYTES >> 10
                            ));
                        }
                        fits
                    });
                }
                continue;
            }
            let binary = f.value.len() == 65 && f.value.starts_with('#');
            match Key::of(&f.key) {
                Some(_) if !binary => {
                    let name =
                        tags::ITEMS.iter().find(|(_, id, _)| *id == f.key).map_or(f.key.as_str(), |i| i.0);
                    if !fields.contains_key(name) {
                        match fit(fields, name, &f.value, disc_bytes) {
                            Ok(c) => {
                                disc_bytes += c;
                                fields.insert(name.to_owned(), f.value.clone());
                            }
                            Err(why) => sum.not_kept.push(format!(
                                "{} (disc {}): {}, {why}",
                                shown(&row.path),
                                shown(image),
                                safe_file::one_line(name)
                            )),
                        }
                    }
                }
                _ if f.key == "TDAT" => {}
                _ => {
                    sum.skipped.insert(f.key.clone());
                }
            }
        }
    }
    let mut file = TagsFile::default();
    for (k, tracks) in areas.iter().enumerate() {
        let slot = if k == 0 { &Slot::TwoChannel } else { &Slot::Multichannel };
        if tracks.is_empty() {
            continue;
        }
        let all: BTreeSet<u8> =
            disc.toc().area(*slot).map(|a| a.tracks.iter().map(|t| t.number).collect()).unwrap_or_default();
        let level = match slot {
            Slot::TwoChannel => &mut file.stereo,
            _ => &mut file.multichannel,
        };
        level.album = common(tracks, &all);
        for (n, f) in tracks {
            let rest: Fields = f
                .iter()
                .filter(|(k, _)| !level.album.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !rest.is_empty() {
                level.track.insert(n.to_string(), rest);
            }
        }
    }
    // What both areas share at album level is the album's.
    let shared: Fields = file
        .stereo
        .album
        .iter()
        .filter(|(k, v)| file.multichannel.album.get(*k) == Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for k in shared.keys() {
        file.stereo.album.remove(k);
        file.multichannel.album.remove(k);
    }
    file.album = shared;
    let mut picture = None;
    if let Some(bytes) = &art {
        let ext = if tags::picture_mime(bytes) == Some("image/png") { "png" } else { "jpg" };
        let name = format!("{key}.{ext}");
        picture = Some(dir.join(&name));
        file.album.insert(ART.to_owned(), name);
    }
    if file.is_empty() {
        return Ok(());
    }
    let path = dir.join(format!("{key}.toml"));
    let existing = match safe_file::read_text(&path, safe_file::MAX_TAGS_TEXT) {
        Ok((text, _)) => {
            Some(TagsFile::parse(&text).map_err(|e| io::Error::other(format!("{}: {e}", shown(&path))))?)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", shown(&path)))),
    };
    let created = existing.is_none();
    let mut merged = existing.unwrap_or_default();
    let added = merge(&mut merged, &file);
    // The file as it would be written, with what was there: over its bound, the disc gets
    // nothing, and that is all there is to say about it.
    let text = (added > 0).then(|| merged.to_text());
    if let Some(t) = text.as_ref().filter(|t| t.len() > MAX_FILE_BYTES) {
        sum.not_kept.truncate(listed);
        sum.not_kept.push(format!(
            "disc {}: its tags file {} would be {} KiB, over the {} KiB import writes; nothing added",
            shown(image),
            shown(&path),
            t.len() >> 10,
            MAX_FILE_BYTES >> 10
        ));
        return Ok(());
    }
    if let (Some(p), Some(bytes)) = (&picture, &art) {
        if !p.exists() {
            write_atomic(p, bytes)?;
            sum.pictures += 1;
        }
    }
    let Some(text) = text else { return Ok(()) };
    write_atomic(&path, text.as_bytes())?;
    sum.written += 1;
    sum.created += usize::from(created);
    sum.keys += added;
    Ok(())
}

/// The fields every track of `all` has with the same value, if `tracks` holds them all
/// (two or more): an album-level value. *Why:* a genre on every track is the album's genre,
/// and the tags file stays short.
pub(crate) fn common(tracks: &AreaFields, all: &BTreeSet<u8>) -> Fields {
    let have: BTreeSet<u8> = tracks.keys().copied().collect();
    if all.len() < 2 || !all.is_subset(&have) {
        return Fields::new();
    }
    let mut it = all.iter().filter_map(|n| tracks.get(n));
    let Some(first) = it.next() else { return Fields::new() };
    let mut out = first.clone();
    for f in it {
        out.retain(|k, v| f.get(k) == Some(v));
    }
    // A title or track number is never the album's.
    out.retain(|k, _| !matches!(k.as_str(), "title" | "track" | "isrc"));
    out
}

/// Adds to `into` each key of `from` it lacks, level by level; the number added.
pub(crate) fn merge(into: &mut TagsFile, from: &TagsFile) -> usize {
    fn level(into: &mut Fields, from: &Fields) -> usize {
        let mut n = 0;
        for (k, v) in from {
            if !into.contains_key(k) {
                into.insert(k.clone(), v.clone());
                n += 1;
            }
        }
        n
    }
    fn tracks(into: &mut BTreeMap<String, Fields>, from: &BTreeMap<String, Fields>) -> usize {
        from.iter().map(|(t, f)| level(into.entry(t.clone()).or_default(), f)).sum()
    }
    level(&mut into.album, &from.album)
        + tracks(&mut into.track, &from.track)
        + level(&mut into.stereo.album, &from.stereo.album)
        + tracks(&mut into.stereo.track, &from.stereo.track)
        + level(&mut into.multichannel.album, &from.multichannel.album)
        + tracks(&mut into.multichannel.track, &from.multichannel.track)
}

/// Writes `bytes` to `path` through a new temporary file in the same directory, renamed over it
/// ([`safe_file::write_atomic`]).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    safe_file::write_atomic(path, bytes, safe_file::MODE)
}

/// The ID3 tag of an extract, read only.
fn extract_tag(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut src = container::FileSource::open(path).map_err(|e| format!("cannot open: {e}"))?;
    let audio = container::parse(&mut src).map_err(container::Problem::into_text)?;
    audio.read_id3(&mut src).map_err(|size| format!("a tag of {size} bytes is too large to read"))
}

/// The picture of an ID3v2.3 or v2.4 tag's `APIC` frame, the front cover (type 3) if there
/// is one, else the first; JPEG or PNG only. Frames compressed, encrypted or unsynchronised
/// are skipped. Offsets use checked arithmetic, so that a size near 2^32 can't overflow a 32-bit
/// `usize` (#457): such a tag has no cover.
pub fn front_cover(tag: &[u8]) -> Option<Vec<u8>> {
    if tag.len() < 10 || &tag[0..3] != b"ID3" || !(3..=4).contains(&tag[3]) || tag[5] & 0x80 != 0 {
        return None;
    }
    let major = tag[3];
    let syncsafe = |b: &[u8]| b.iter().fold(0usize, |a, &x| (a << 7) | usize::from(x & 0x7F));
    let be = |b: &[u8]| b.iter().fold(0usize, |a, &x| (a << 8) | usize::from(x));
    let end = (10 + syncsafe(&tag[6..10])).min(tag.len());
    let mut at: usize = 10;
    if tag[5] & 0x40 != 0 {
        let ext = tag.get(10..14)?;
        let ext_size = if major == 4 { syncsafe(ext) } else { be(ext).checked_add(4)? };
        at = at.checked_add(ext_size)?;
    }
    let mut found: Option<Vec<u8>> = None;
    while end.saturating_sub(at) >= 10 {
        let h = &tag[at..at + 10];
        if h[0] == 0 {
            break;
        }
        let size = if major == 4 { syncsafe(&h[4..8]) } else { be(&h[4..8]) };
        // A size past the end of memory: no more frames, but keep a cover already found.
        let Some(body_end) = (at + 10).checked_add(size) else { break };
        let body = tag.get(at + 10..body_end.min(end))?;
        at = body_end;
        let packed = if major == 4 { h[9] & 0x0F != 0 } else { h[9] & 0xE0 != 0 };
        if &h[0..4] != b"APIC" || packed {
            continue;
        }
        let Some((kind, image)) = apic(body) else { continue };
        if kind == 3 {
            return Some(image);
        }
        found.get_or_insert(image);
    }
    found
}

/// An `APIC` body's picture type and image, if the image is a JPEG or PNG.
fn apic(b: &[u8]) -> Option<(u8, Vec<u8>)> {
    let (&enc, rest) = b.split_first()?;
    let mime_end = rest.iter().position(|&c| c == 0)?;
    let rest = &rest[mime_end + 1..];
    let (&kind, rest) = rest.split_first()?;
    // The description: terminated by 0, or by 0 0 at an even offset in UTF-16.
    let desc_end = if enc == 1 || enc == 2 {
        (0..rest.len().saturating_sub(1)).step_by(2).find(|&i| rest[i] == 0 && rest[i + 1] == 0)? + 2
    } else {
        rest.iter().position(|&c| c == 0)? + 1
    };
    let image = &rest[desc_end..];
    tags::picture_mime(image).map(|_| (kind, image.to_vec()))
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, reason = "test data of known small sizes")]
mod tests {
    use super::*;

    fn f(pairs: &[(&str, &str)]) -> Fields {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn album_level_only_when_every_track_agrees() {
        let all: BTreeSet<u8> = [1, 2].into();
        let mut t = AreaFields::new();
        t.insert(1, f(&[("genre", "Jazz"), ("title", "x"), ("TBPM", "1")]));
        t.insert(2, f(&[("genre", "Jazz"), ("title", "x"), ("TBPM", "2")]));
        assert_eq!(common(&t, &all), f(&[("genre", "Jazz")]));
        t.remove(&2);
        assert!(common(&t, &all).is_empty(), "a track missing");
    }

    #[test]
    fn merge_keeps_existing_values() {
        let mut a = TagsFile::parse("[album]\ngenre = 'Mine'\n").unwrap();
        let b =
            TagsFile::parse("[album]\ngenre = 'Theirs'\ndate = '2001'\n[track.2]\ntitle = 't'\n").unwrap();
        assert_eq!(merge(&mut a, &b), 2);
        assert_eq!(a.album["genre"], "Mine");
        assert_eq!(a.album["date"], "2001");
        assert_eq!(merge(&mut a, &b), 0);
    }

    #[test]
    fn fit_skips_what_the_mount_would_not_use() {
        let mut t = Fields::new();
        assert!(fit(&t, "genre", "Jazz", 0).is_ok());
        // A value over its limit is not cut: it is refused, and says why.
        let long = "x".repeat(MAX_VALUE_BYTES + 1);
        assert!(fit(&t, "COMM", &long, 0).unwrap_err().contains("a value can have"));
        // Keys of one track.
        for i in 0..MAX_TRACK_KEYS {
            t.insert(format!("TXXX:k{i}"), "v".into());
        }
        assert!(fit(&t, "genre", "Jazz", 0).unwrap_err().contains("keys a track can have"));
        // A track's text.
        let mut t = Fields::new();
        let v = "y".repeat(12 << 10);
        t.insert("COMM".into(), v.clone());
        t.insert("TXXX:notes".into(), v.clone());
        assert!(fit(&t, "TXXX:more", &v, 0).unwrap_err().contains("of text a track can have"));
        // A disc's text.
        assert!(fit(&Fields::new(), "genre", "Jazz", MAX_DISC_BYTES - 10).unwrap_err().contains("disc"));
        // Escaping counts: a value of quotes costs more than its bytes.
        assert!(cost("k", "\"\"") > cost("k", "ab"));
    }

    #[test]
    fn not_kept_is_listed_and_not_all_handled() {
        let mut s = ImportSummary::default();
        assert!(s.all_handled());
        s.not_kept.push("a.dsf (disc d.iso): COMM, 20000 bytes".into());
        assert!(!s.all_handled());
        let r = s.render();
        assert!(r.contains("not kept: a.dsf (disc d.iso): COMM"), "{r}");
        assert!(r.contains("keep those extracts"), "{r}");
    }

    #[test]
    fn front_cover_from_v23_and_v24() {
        let jpg = b"\xFF\xD8\xFFdata".to_vec();
        for major in [3u8, 4] {
            let mut body = vec![0u8];
            body.extend_from_slice(b"image/jpeg\0");
            body.push(3);
            body.extend_from_slice(b"desc\0");
            body.extend_from_slice(&jpg);
            let mut fr = b"APIC".to_vec();
            let n = body.len() as u32;
            fr.extend_from_slice(&if major == 4 {
                [0, 0, (n >> 7) as u8 & 0x7F, n as u8 & 0x7F]
            } else {
                n.to_be_bytes()
            });
            fr.extend_from_slice(&[0, 0]);
            fr.extend_from_slice(&body);
            let mut tag = vec![b'I', b'D', b'3', major, 0, 0, 0, 0, 0, fr.len() as u8];
            tag.extend_from_slice(&fr);
            assert_eq!(front_cover(&tag), Some(jpg.clone()), "v2.{major}");
        }
        assert_eq!(front_cover(b"ID3\x04\0\0\0\0\0\0"), None);
    }

    /// Sizes near 2^32 in an ID3v2.3 extended header or frame header mean no cover, never an
    /// overflow, on 32-bit targets too (#457).
    #[test]
    fn front_cover_of_the_largest_sizes() {
        for ext in [0xFFFF_FFFFu32, 0xFFFF_FFFC, 0xFFFF_FFF0] {
            let mut tag = b"ID3\x03\x00\x40\x00\x00\x00\x10".to_vec();
            tag.extend_from_slice(&ext.to_be_bytes());
            tag.extend_from_slice(b"APIC\x00\x00\x00\x02\x00\x00\x00a");
            assert_eq!(front_cover(&tag), None, "extended header {ext:#x}");
        }
        for size in [0xFFFF_FFFFu32, 0xFFFF_FFF6, 0xFFFF_FFF5] {
            let mut tag = b"ID3\x03\x00\x00\x00\x00\x00\x20APIC".to_vec();
            tag.extend_from_slice(&size.to_be_bytes());
            tag.extend_from_slice(&[0; 22]);
            assert_eq!(front_cover(&tag), None, "frame size {size:#x}");
        }
        // A cover found before such a frame is kept.
        let jpg = b"\xFF\xD8\xFFdata";
        let mut body = b"\0image/jpeg\0\x00\0".to_vec();
        body.extend_from_slice(jpg);
        let mut tag = b"ID3\x03\x00\x00\x00\x00\x00\x40APIC".to_vec();
        tag.extend_from_slice(&(body.len() as u32).to_be_bytes());
        tag.extend_from_slice(&[0, 0]);
        tag.extend_from_slice(&body);
        tag.extend_from_slice(b"TIT2\xFF\xFF\xFF\xFF\x00\x00");
        tag.resize(10 + 0x40, 0);
        assert_eq!(front_cover(&tag), Some(jpg.to_vec()));
    }

    /// Tags files and covers are written through a new temporary file: a link at the old
    /// temporary name, or at a name made of the process ID and a counter, is never written
    /// through.
    #[cfg(unix)]
    #[test]
    fn writes_through_no_link() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, b"precious").unwrap();
        for name in ["0123.toml", "0123.jpg"] {
            let path = dir.path().join(name);
            let _ = symlink(&victim, path.with_extension("tmp~"));
            for n in 0..64 {
                let guess = dir.path().join(format!(".{name}.{}.{n}.tmp", std::process::id()));
                symlink(&victim, guess).unwrap();
            }
            write_atomic(&path, b"new").unwrap();
            write_atomic(&path, b"newer").unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"newer");
        }
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        // A tags file that is a link is not read.
        let toml = dir.path().join("4567.toml");
        fs::write(&victim, "[album]\ngenre = \"Jazz\"\n").unwrap();
        symlink(&victim, &toml).unwrap();
        assert!(safe_file::read_text(&toml, safe_file::MAX_TAGS_TEXT).is_err());
    }

    /// Keys import could not keep are escaped one by one in its summary.
    #[test]
    fn summary_keys_are_escaped() {
        let keys = ["TXXX:\x1b]0;title\x07\x1b[2J", "TXXX:a\nUNSUPPORTED (1): forged"];
        let sum = ImportSummary {
            skipped: keys.iter().map(|k| (*k).to_owned()).collect(),
            ..ImportSummary::default()
        };
        let out = sum.render();
        assert!(!out.chars().any(|c| c.is_control() && c != '\n'), "{out:?}");
        assert!(!out.lines().any(|l| l.starts_with("UNSUPPORTED")), "{out}");
        assert!(out.contains("\\x1b]0;title\\x07\\x1b[2J") && out.contains("a\\x0aUNSUPPORTED"), "{out}");
        let many = ImportSummary {
            skipped: (0..100).map(|i| format!("K{i:03}")).collect(),
            ..ImportSummary::default()
        };
        assert!(many.render().contains("+68 more"));
    }
}
