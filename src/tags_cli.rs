// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs tags` (#81): the disc key of an image, and `import`, which copies the tags of
//! extracts that `sacdfs verify` matches into the tags directory, so that the extracts can be
//! deleted without losing them (#82). Nothing but the tags directory is written.

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
    /// Print each image's disc key: its tags file is DIR/<key>.toml.
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
}

/// `sacdfs tags`: `args` starts with "tags".
pub fn main(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let cli = Cli::parse_from(args);
    let verbose = match &cli.cmd {
        Cmd::Import { verify, .. } => verify.verbose,
        Cmd::Key { .. } => 0,
    };
    let level = ["warn", "info", "debug"].get(usize::from(verbose)).copied().unwrap_or("trace");
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).try_init();
    let r = match cli.cmd {
        Cmd::Key { tags, images } => key(tags.as_deref(), &images),
        Cmd::Import { tags, verify } => import(&tags, verify).map(|s| eprint!("{}", s.render())),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sacdfs tags: {e}");
            ExitCode::FAILURE
        }
    }
}

fn key(dir: Option<&Path>, images: &[PathBuf]) -> io::Result<()> {
    for image in images {
        let disc = Disc::open(image, 0, Tuning::default())
            .map_err(|e| io::Error::other(format!("{}: {e}", image.display())))?;
        let key = tags::disc_key(&disc.toc);
        match dir {
            Some(d) => {
                let p = d.join(format!("{key}.toml"));
                let state = if p.exists() { "exists" } else { "none" };
                println!("{key}\t{}\t{state}\t{}", p.display(), image.display());
            }
            None => println!("{key}\t{}", image.display()),
        }
    }
    Ok(())
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
    /// Discs whose tags file could not be read or written, and why.
    pub failed: Vec<String>,
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
            let k: Vec<&str> = self.skipped.iter().map(String::as_str).collect();
            s.push_str(&format!("not kept (frames the tag layer cannot write): {}\n", k.join(", ")));
        }
        for f in &self.failed {
            s.push_str(&format!("failed: {f}\n"));
        }
        s
    }
}

/// One area's imported fields, by track number.
type AreaFields = BTreeMap<u8, Fields>;

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
    let mut sum = ImportSummary::default();
    for (image, rows) in by_image {
        if let Err(e) = import_disc(&dir, &image, &rows, &mut sum) {
            sum.failed.push(format!("{}: {e}", image.display()));
        }
    }
    Ok(sum)
}

fn import_disc(dir: &Path, image: &Path, rows: &[&verify::Row], sum: &mut ImportSummary) -> io::Result<()> {
    let disc = Arc::new(Disc::open(image, 0, Tuning::default()).map_err(io::Error::other)?);
    let key = tags::disc_key(&disc.toc);
    let mut areas: [AreaFields; 2] = Default::default();
    let mut art: Option<Vec<u8>> = None;
    for row in rows {
        let slot = if row.area == "Multichannel" { Slot::Multichannel } else { Slot::TwoChannel };
        let Some(number) = row.track else { continue };
        let Some(i) = disc.toc.areas.iter().position(|a| a.slot == slot) else { continue };
        let area = &disc.toc.areas[i];
        let Some(track) = area.tracks.iter().find(|t| t.number == number) else { continue };
        // Compared with the file as sacdfs serves it without a tags file.
        let served = TrackFile::new(disc.clone(), i, track).map(|f| id3::frames(f.tag())).unwrap_or_default();
        let Some(tag) = extract_tag(&row.path) else { continue };
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
                    art = front_cover(&tag);
                }
                continue;
            }
            let binary = f.value.len() == 65 && f.value.starts_with('#');
            match Key::of(&f.key) {
                Some(_) if !binary => {
                    let name =
                        tags::ITEMS.iter().find(|(_, id, _)| *id == f.key).map_or(f.key.as_str(), |i| i.0);
                    fields.entry(name.to_owned()).or_insert_with(|| f.value.clone());
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
            disc.toc.area(*slot).map(|a| a.tracks.iter().map(|t| t.number).collect()).unwrap_or_default();
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
    if let Some(bytes) = &art {
        let ext = if tags::picture_mime(bytes) == Some("image/png") { "png" } else { "jpg" };
        let name = format!("{key}.{ext}");
        let p = dir.join(&name);
        if !p.exists() {
            write_atomic(&p, bytes)?;
            sum.pictures += 1;
        }
        file.album.insert(ART.to_owned(), name);
    }
    if file.is_empty() {
        return Ok(());
    }
    let path = dir.join(format!("{key}.toml"));
    let existing = match fs::read_to_string(&path) {
        Ok(text) => {
            Some(TagsFile::parse(&text).map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let created = existing.is_none();
    let mut merged = existing.unwrap_or_default();
    let added = merge(&mut merged, &file);
    if added == 0 {
        return Ok(());
    }
    write_atomic(&path, merged.to_text().as_bytes())?;
    sum.written += 1;
    sum.created += usize::from(created);
    sum.keys += added;
    Ok(())
}

/// The fields every track of `all` has with the same value, if `tracks` holds them all
/// (two or more): an album-level value. *Why:* a genre on every track is the album's genre,
/// and the tags file stays short.
fn common(tracks: &AreaFields, all: &BTreeSet<u8>) -> Fields {
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
fn merge(into: &mut TagsFile, from: &TagsFile) -> usize {
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

/// Writes `bytes` to `path` through a temporary file in the same directory, renamed over it.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp~");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// The ID3 tag of an extract, read only.
fn extract_tag(path: &Path) -> Option<Vec<u8>> {
    let mut src = container::FileSource::open(path).ok()?;
    let audio = container::parse(&mut src).ok()?;
    audio.read_id3(&mut src).ok().flatten()
}

/// The picture of an ID3v2.3 or v2.4 tag's `APIC` frame, the front cover (type 3) if there
/// is one, else the first; JPEG or PNG only. Frames compressed, encrypted or unsynchronised
/// are skipped.
pub fn front_cover(tag: &[u8]) -> Option<Vec<u8>> {
    if tag.len() < 10 || &tag[0..3] != b"ID3" || !(3..=4).contains(&tag[3]) || tag[5] & 0x80 != 0 {
        return None;
    }
    let major = tag[3];
    let syncsafe = |b: &[u8]| b.iter().fold(0usize, |a, &x| (a << 7) | usize::from(x & 0x7F));
    let be = |b: &[u8]| b.iter().fold(0usize, |a, &x| (a << 8) | usize::from(x));
    let end = (10 + syncsafe(&tag[6..10])).min(tag.len());
    let mut at = 10;
    if tag[5] & 0x40 != 0 {
        let ext = tag.get(10..14)?;
        at += if major == 4 { syncsafe(ext) } else { be(ext) + 4 };
    }
    let mut found: Option<Vec<u8>> = None;
    while at + 10 <= end {
        let h = &tag[at..at + 10];
        if h[0] == 0 {
            break;
        }
        let size = if major == 4 { syncsafe(&h[4..8]) } else { be(&h[4..8]) };
        let body = tag.get(at + 10..(at + 10).checked_add(size)?.min(end))?;
        at += 10 + size;
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
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
}
