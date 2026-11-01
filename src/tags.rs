// SPDX-License-Identifier: GPL-3.0-or-later

//! The tag layer (#81): user tags kept in a directory of their own, one TOML file per disc,
//! named by the disc's TOC identity ([`disc_key`]), which override and add to the tags sacdfs
//! makes from the TOC. Edits are made out of band (`sacdfs tags import`, beets-pysacd, a text
//! editor); the mount stays read-only. With no tags file for a disc, its files are byte for
//! byte what they are without the tag layer.
//!
//! ```toml
//! [album]                       # every file of the disc
//! genre = "Jazz"
//! art = "cover.jpg"             # a JPEG or PNG in the tags directory, embedded as APIC
//! "TXXX:MusicBrainz Album Id" = "..."
//!
//! [track.3]                     # track 3 of both areas
//! title = "Corrected title"
//! "UFID:http://musicbrainz.org" = "..."
//!
//! [multichannel.album]          # one area only; overrides the levels above
//! album = "Album (Multichannel)"
//!
//! [stereo.track.3]
//! "TXXX:replaygain_track_gain" = "-6.20 dB"
//! ```
//!
//! Keys are the names below ([`ITEMS`]) or ID3 frames written as `sacdfs verify` reports
//! them: a text frame id (`TBPM`), `TXXX:description`, `COMM:description` or `UFID:owner`.
//! An empty value removes the item. Levels apply in the order album, area album, track, area
//! track; a later one wins.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use scarletbook::output::{MAX_TAG_BYTES, TagItem, TagSet};
use scarletbook::{Slot, Toc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The names of the TagSet items a tags file can set, with the ID3 frame each is written as.
pub const ITEMS: [(&str, &str, TagItem); 12] = [
    ("title", "TIT2", TagItem::TrackTitle),
    ("artist", "TPE1", TagItem::Performer),
    ("album", "TALB", TagItem::Album),
    ("album_artist", "TPE2", TagItem::AlbumArtist),
    ("composer", "TCOM", TagItem::Composer),
    ("genre", "TCON", TagItem::Genre),
    ("date", "TDRC", TagItem::Date),
    ("track", "TRCK", TagItem::TrackNumber),
    ("disc", "TPOS", TagItem::DiscNumber),
    ("publisher", "TPUB", TagItem::Publisher),
    ("copyright", "TCOP", TagItem::Copyright),
    ("isrc", "TSRC", TagItem::Isrc),
];

/// The key naming the picture to embed.
pub const ART: &str = "art";

/// Largest picture embedded: what fits in scarletbook's tag limit with room for the text.
pub const MAX_ART_BYTES: usize = MAX_TAG_BYTES - (64 << 10);

/// One level of a tags file: key to value.
pub type Fields = BTreeMap<String, String>;

/// The album and track levels of one area, or of the whole disc.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Levels {
    /// Every file.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub album: Fields,
    /// Per track number.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub track: BTreeMap<String, Fields>,
}

impl Levels {
    fn is_empty(&self) -> bool {
        self.album.is_empty() && self.track.values().all(BTreeMap::is_empty)
    }
}

/// A picture: its MIME type and bytes.
pub type Picture = (&'static str, Arc<Vec<u8>>);

/// A tags file as cached: its mtime and size when read, and its tags.
type Cached = (SystemTime, u64, Option<Arc<DiscTags>>);

/// A tags file as written.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagsFile {
    /// Every file of the disc.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub album: Fields,
    /// Per track number, both areas.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub track: BTreeMap<String, Fields>,
    /// The stereo area only.
    #[serde(default, skip_serializing_if = "Levels::is_empty")]
    pub stereo: Levels,
    /// The multichannel area only.
    #[serde(default, skip_serializing_if = "Levels::is_empty")]
    pub multichannel: Levels,
}

impl TagsFile {
    /// Parses a tags file's text.
    pub fn parse(text: &str) -> Result<TagsFile, String> {
        let f: TagsFile = toml::from_str(text).map_err(|e| e.to_string())?;
        for (_, fields) in f.levels() {
            for key in fields.keys() {
                if key != ART && Key::of(key).is_none() {
                    return Err(format!("unknown key {key:?}"));
                }
            }
        }
        for t in f.track.keys().chain(f.stereo.track.keys()).chain(f.multichannel.track.keys()) {
            if t.parse::<u8>().ok().filter(|&n| n > 0).is_none() {
                return Err(format!("track {t:?} is not a number from 1 to 255"));
            }
        }
        Ok(f)
    }

    /// The text written to a tags file.
    pub fn to_text(&self) -> String {
        let body = toml::to_string(self).unwrap_or_default();
        format!("# sacdfs tags for one disc (#81); see docs/user-guide.md, \"Tags\".\n{body}")
    }

    /// Whether it sets nothing.
    pub fn is_empty(&self) -> bool {
        self.album.is_empty()
            && self.track.values().all(BTreeMap::is_empty)
            && self.stereo.is_empty()
            && self.multichannel.is_empty()
    }

    fn levels(&self) -> Vec<(String, &Fields)> {
        let mut v = vec![("album".to_owned(), &self.album)];
        for (n, f) in &self.track {
            v.push((format!("track.{n}"), f));
        }
        for (name, l) in [("stereo", &self.stereo), ("multichannel", &self.multichannel)] {
            v.push((format!("{name}.album"), &l.album));
            for (n, f) in &l.track {
                v.push((format!("{name}.track.{n}"), f));
            }
        }
        v
    }

    /// The fields of track `number` of the area in `slot`, every level merged.
    pub fn fields_for(&self, slot: Slot, number: u8) -> Fields {
        let area = match slot {
            Slot::TwoChannel => &self.stereo,
            _ => &self.multichannel,
        };
        let n = number.to_string();
        let mut out = Fields::new();
        for level in [Some(&self.album), Some(&area.album), self.track.get(&n), area.track.get(&n)]
            .into_iter()
            .flatten()
        {
            for (k, v) in level {
                out.insert(canonical(k), v.clone());
            }
        }
        out
    }
}

/// A key's one spelling, so that `genre` and `TCON` on two levels are the same key.
fn canonical(key: &str) -> String {
    match ITEMS.iter().find(|(_, id, _)| *id == key) {
        Some((name, _, _)) => (*name).to_owned(),
        None => key.to_owned(),
    }
}

/// What a key sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// A TagSet item.
    Item(TagItem),
    /// A `TXXX` frame with this description.
    Txxx(String),
    /// A frame sacdfs appends to the tag: a text frame id, `COMM` or `UFID`, with its
    /// description or owner.
    Frame([u8; 4], Option<String>),
}

impl Key {
    /// The key `key` names, if any.
    pub fn of(key: &str) -> Option<Key> {
        if let Some((_, _, item)) = ITEMS.iter().find(|(n, id, _)| *n == key || *id == key) {
            return Some(Key::Item(*item));
        }
        let (id, rest) = match key.split_once(':') {
            Some((id, rest)) => (id, Some(rest.to_owned())),
            None => (key, None),
        };
        let b: [u8; 4] = id.as_bytes().try_into().ok()?;
        if !b.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
            return None;
        }
        match (id, rest) {
            ("TXXX", Some(d)) => Some(Key::Txxx(d)),
            ("COMM", d) => Some(Key::Frame(b, Some(d.unwrap_or_default()))),
            ("UFID", Some(o)) if !o.is_empty() => Some(Key::Frame(b, Some(o))),
            // TDAT, TYER and the like are ID3v2.3's date: `date` is the one to use.
            ("TXXX" | "UFID" | "TYER" | "TDAT" | "TIME" | "TRDA", _) => None,
            (_, None) if b[0] == b'T' => Some(Key::Frame(b, None)),
            _ => None,
        }
    }
}

/// The overrides of one file: what [`TagsFile::fields_for`] gives, sorted out.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    /// TagSet items and `TXXX` frames to set (or, with an empty value, remove).
    pub set: Vec<(Key, String)>,
    /// Frames appended to the tag, in order.
    pub frames: Vec<([u8; 4], Option<String>, String)>,
    /// The picture to embed: its MIME type and bytes.
    pub art: Option<Picture>,
    /// Whether it names the album: then the multichannel suffix (#97) is not added.
    pub names_album: bool,
}

impl Overrides {
    /// Applies the items to `set`.
    pub fn apply(&self, set: &mut TagSet) {
        for (key, value) in &self.set {
            match key {
                Key::Item(item) => apply_item(set, *item, value),
                Key::Txxx(d) => {
                    if value.is_empty() {
                        // TagSet has no way to remove one user-defined item: copy the rest.
                        let keep: Vec<(String, String)> =
                            set.custom().iter().filter(|(c, _)| c != d).cloned().collect();
                        *set = without_custom(set, &keep);
                    } else {
                        set.set_custom(d, value);
                    }
                }
                Key::Frame(..) => {}
            }
        }
    }

    /// Whether it changes nothing beyond the TagSet items.
    pub fn only_items(&self) -> bool {
        self.frames.is_empty() && self.art.is_none()
    }
}

/// `set`'s items with `custom` as its user-defined items.
fn without_custom(set: &TagSet, custom: &[(String, String)]) -> TagSet {
    let mut out = TagSet::new();
    for item in ALL_ITEMS {
        for v in set.get(item) {
            out.push(item, v);
        }
    }
    for (d, v) in custom {
        out.set_custom(d, v);
    }
    out
}

/// Every TagSet item, for copying a set.
const ALL_ITEMS: [TagItem; 20] = [
    TagItem::TrackTitle,
    TagItem::Performer,
    TagItem::Songwriter,
    TagItem::Composer,
    TagItem::Arranger,
    TagItem::Message,
    TagItem::ExtraMessage,
    TagItem::TitlePhonetic,
    TagItem::PerformerPhonetic,
    TagItem::TrackNumber,
    TagItem::TrackCount,
    TagItem::Isrc,
    TagItem::Genre,
    TagItem::Album,
    TagItem::AlbumArtist,
    TagItem::Publisher,
    TagItem::Copyright,
    TagItem::DiscNumber,
    TagItem::DiscCount,
    TagItem::Date,
];

/// Sets `item` to `value`: `n/N` for the track and disc numbers sets their counts too; an
/// empty value removes it.
fn apply_item(set: &mut TagSet, item: TagItem, value: &str) {
    let value = value.trim();
    let count = match item {
        TagItem::TrackNumber => Some(TagItem::TrackCount),
        TagItem::DiscNumber => Some(TagItem::DiscCount),
        _ => None,
    };
    if value.is_empty() {
        set.remove(item);
        if let Some(c) = count {
            set.remove(c);
        }
        return;
    }
    match (count, value.split_once('/')) {
        (Some(c), Some((n, total))) => {
            set.set(item, n.trim());
            set.set(c, total.trim());
        }
        _ => set.set(item, value),
    }
}

/// A disc's tags file, read.
#[derive(Debug)]
pub struct DiscTags {
    /// The file.
    pub file: TagsFile,
    /// Its modification time.
    pub mtime: SystemTime,
    /// Pictures named by it, by name, read once: `None` if unusable (logged).
    art: Mutex<HashMap<String, Option<Picture>>>,
    dir: PathBuf,
}

impl DiscTags {
    /// The tags of `file` in the tags directory `dir`.
    pub fn new(file: TagsFile, mtime: SystemTime, dir: &Path) -> DiscTags {
        DiscTags { file, mtime, art: Mutex::new(HashMap::new()), dir: dir.to_owned() }
    }

    /// The overrides of track `number` of the area in `slot`; `None` if the file sets
    /// nothing for it.
    pub fn overrides(&self, slot: Slot, number: u8) -> Option<Overrides> {
        let fields = self.file.fields_for(slot, number);
        if fields.is_empty() {
            return None;
        }
        let mut o = Overrides::default();
        for (k, v) in &fields {
            if k == ART {
                o.art = if v.is_empty() { None } else { self.picture(v) };
                continue;
            }
            match Key::of(k) {
                Some(Key::Frame(id, d)) => o.frames.push((id, d, v.clone())),
                Some(key) => {
                    if key == Key::Item(TagItem::Album) {
                        o.names_album = true;
                    }
                    o.set.push((key, v.clone()));
                }
                None => {}
            }
        }
        Some(o)
    }

    fn picture(&self, name: &str) -> Option<Picture> {
        let mut m = self.art.lock().unwrap_or_else(|p| p.into_inner());
        m.entry(name.to_owned())
            .or_insert_with(|| {
                read_picture(&self.dir, name)
                    .inspect_err(|e| log::warn!("tags: art {name:?}: {e}; not embedded"))
                    .ok()
            })
            .clone()
    }
}

/// Reads the picture `name` (a file name in `dir`, not a path; no link): its MIME type, from
/// its first bytes, and its bytes.
pub fn read_picture(dir: &Path, name: &str) -> io::Result<Picture> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidInput, m.to_owned());
    if name.contains('/') || name.starts_with('.') || name.contains('\0') {
        return Err(bad("not a file name in the tags directory"));
    }
    let path = dir.join(name);
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_file() {
        return Err(bad("not a regular file"));
    }
    if meta.len() > MAX_ART_BYTES as u64 {
        return Err(bad(&format!("larger than {} KiB", MAX_ART_BYTES >> 10)));
    }
    let bytes = fs::read(&path)?;
    let mime = picture_mime(&bytes).ok_or_else(|| bad("not a JPEG or PNG image"))?;
    Ok((mime, Arc::new(bytes)))
}

/// The MIME type of a JPEG or PNG image, from its first bytes.
pub fn picture_mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else {
        None
    }
}

/// The identity of a disc, from its TOC only: 32 hex digits of a SHA-256 of the master and
/// area TOCs' numbers (catalogue numbers, set position, date, flags, area and track
/// positions and lengths). *Why the TOC and not the path:* edits must survive renaming and
/// moving the image, and two copies of the same disc share them (#81). *Why numbers, not
/// the TOC sectors' bytes:* the library hands over the parsed TOC; these fields are read
/// from fixed offsets of the specification, so a newer library gives the same key.
pub fn disc_key(toc: &Toc) -> String {
    let m = &toc.master;
    let mut h = Sha256::new();
    h.update(b"sacdfs disc v1\0");
    h.update(m.album_set_size.to_be_bytes());
    h.update(m.album_sequence_number.to_be_bytes());
    for b in [&m.album_catalog_number_stored, &m.disc_catalog_number_stored] {
        h.update((b.len() as u32).to_be_bytes());
        h.update(b);
    }
    h.update(m.date.year.to_be_bytes());
    h.update([m.date.month, m.date.day, m.disc_flags]);
    h.update((toc.areas.len() as u32).to_be_bytes());
    for a in &toc.areas {
        h.update([a.slot as u8, a.channel_count]);
        h.update(a.toc_lsn.to_be_bytes());
        h.update(a.audio_start_lsn.to_be_bytes());
        h.update(a.audio_end_lsn.to_be_bytes());
        h.update((a.tracks.len() as u32).to_be_bytes());
        for t in &a.tracks {
            h.update([t.number]);
            h.update(t.start_lsn.to_be_bytes());
            h.update(t.length_sectors.to_be_bytes());
            h.update(t.start_frame.to_be_bytes());
            h.update(t.duration_frames.to_be_bytes());
        }
    }
    let d = h.finalize();
    crate::verify::id3::hex(&d[..16])
}

/// The tags directory: the files read, re-read when their mtime changes.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    cache: Mutex<HashMap<String, Cached>>,
}

impl Store {
    /// The store of the tags directory `dir`.
    pub fn new(dir: &Path) -> Store {
        Store { dir: dir.to_owned(), cache: Mutex::new(HashMap::new()) }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The path of the tags file of the disc with key `key`.
    pub fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.toml"))
    }

    /// The tags of the disc with `toc`, if it has a usable tags file. A file that does not
    /// parse is logged and ignored: the disc's files are then as without one.
    pub fn get(&self, toc: &Toc) -> Option<Arc<DiscTags>> {
        let key = disc_key(toc);
        let path = self.path(&key);
        let meta = fs::symlink_metadata(&path).ok().filter(fs::Metadata::is_file)?;
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let mut c = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((t, len, tags)) = c.get(&key) {
            if *t == mtime && *len == meta.len() {
                return tags.clone();
            }
        }
        let tags =
            match fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| TagsFile::parse(&s)) {
                Ok(f) if !f.is_empty() => Some(Arc::new(DiscTags::new(f, mtime, &self.dir))),
                Ok(_) => None,
                Err(e) => {
                    log::warn!("tags: {}: {e}; ignored", path.display());
                    None
                }
            };
        c.insert(key, (mtime, meta.len(), tags.clone()));
        tags
    }
}

/// The process's tags directory, set once at startup (`--tags`). *Why process-wide:* as the
/// multichannel suffix switch, it keeps the option out of the tree's and the track files'
/// signatures; without it nothing is looked up.
static STORE: RwLock<Option<Arc<Store>>> = RwLock::new(None);

/// Sets the process's tags directory (`None`: no tag layer).
pub fn install(store: Option<Store>) {
    *STORE.write().unwrap_or_else(|p| p.into_inner()) = store.map(Arc::new);
}

/// The tags of the disc with `toc` in the process's tags directory, if any.
pub fn lookup(toc: &Toc) -> Option<Arc<DiscTags>> {
    let store = STORE.read().unwrap_or_else(|p| p.into_inner()).clone()?;
    store.get(toc)
}

/// An ID3v2.4 frame: `id`, a body of `body`.
fn frame(out: &mut Vec<u8>, id: [u8; 4], body: &[u8]) {
    out.extend_from_slice(&id);
    out.extend_from_slice(&syncsafe(body.len() as u32));
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(body);
}

fn syncsafe(v: u32) -> [u8; 4] {
    [(v >> 21) as u8 & 0x7F, (v >> 14) as u8 & 0x7F, (v >> 7) as u8 & 0x7F, v as u8 & 0x7F]
}

/// `tag` (an ID3v2.4 tag without footer, or empty) with the frames and the picture of `o`
/// appended; `None` if the result would pass scarletbook's limit.
pub fn append_frames(tag: &[u8], o: &Overrides) -> Option<Vec<u8>> {
    let mut out = if tag.len() >= 10 && tag.starts_with(b"ID3\x04") {
        tag.to_vec()
    } else {
        b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec()
    };
    for (id, d, value) in &o.frames {
        if value.is_empty() {
            continue;
        }
        let mut body = Vec::new();
        match id {
            b"UFID" => {
                body.extend_from_slice(d.as_deref().unwrap_or("").as_bytes());
                body.push(0);
                body.extend_from_slice(value.as_bytes());
            }
            b"COMM" => {
                body.push(3);
                body.extend_from_slice(b"XXX");
                body.extend_from_slice(d.as_deref().unwrap_or("").as_bytes());
                body.push(0);
                body.extend_from_slice(value.as_bytes());
            }
            _ => {
                body.push(3);
                body.extend_from_slice(value.as_bytes());
            }
        }
        frame(&mut out, *id, &body);
    }
    if let Some((mime, data)) = &o.art {
        let mut body = vec![3];
        body.extend_from_slice(mime.as_bytes());
        body.extend_from_slice(&[0, 3, 0]); // front cover, empty description
        body.extend_from_slice(data);
        frame(&mut out, *b"APIC", &body);
    }
    if out.len() > MAX_TAG_BYTES {
        return None;
    }
    let size = syncsafe((out.len() - 10) as u32);
    out[6..10].copy_from_slice(&size);
    out[5] &= !0x10; // no footer
    Some(out)
}

/// What the tag layer changed in a file: shown in its mtime and generation (#64).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// The tags file's modification time.
    pub mtime: SystemTime,
    /// A hash of the file's tag.
    pub hash: u64,
}

impl Stamp {
    /// The stamp of a tag `tag` from a tags file modified at `mtime`.
    pub fn new(mtime: SystemTime, tag: &[u8]) -> Stamp {
        let d = Sha256::digest(tag);
        let mut b = [0u8; 8];
        b.copy_from_slice(&d[..8]);
        Stamp { mtime, hash: u64::from_be_bytes(b) }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    const EXAMPLE: &str = r#"
[album]
genre = "Jazz"
"TXXX:MusicBrainz Album Id" = "abc"

[track.3]
title = "Three"
"UFID:http://musicbrainz.org" = "rec-3"

[multichannel.album]
album = "Name (Multichannel)"

[stereo.track.3]
TIT2 = "Three (stereo)"
"#;

    #[test]
    fn levels_merge_in_order() {
        let f = TagsFile::parse(EXAMPLE).unwrap();
        let s = f.fields_for(Slot::TwoChannel, 3);
        assert_eq!(s["title"], "Three (stereo)", "area track wins, TIT2 is title");
        assert_eq!(s["genre"], "Jazz");
        assert!(!s.contains_key("album"));
        let m = f.fields_for(Slot::Multichannel, 3);
        assert_eq!(m["title"], "Three");
        assert_eq!(m["album"], "Name (Multichannel)");
        assert_eq!(f.fields_for(Slot::TwoChannel, 1).len(), 2);
        // Written and read again: the same.
        assert_eq!(TagsFile::parse(&f.to_text()).unwrap(), f);
    }

    #[test]
    fn bad_files_are_refused() {
        for bad in ["[albm]\ngenre='x'", "[album]\nfoo = 'x'", "[track.0]\ntitle='x'", "[track.x]\ntitle='x'"]
        {
            assert!(TagsFile::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn keys() {
        assert_eq!(Key::of("genre"), Some(Key::Item(TagItem::Genre)));
        assert_eq!(Key::of("TCON"), Some(Key::Item(TagItem::Genre)));
        assert_eq!(Key::of("TXXX:x"), Some(Key::Txxx("x".into())));
        assert_eq!(Key::of("TBPM"), Some(Key::Frame(*b"TBPM", None)));
        assert_eq!(Key::of("UFID:o"), Some(Key::Frame(*b"UFID", Some("o".into()))));
        for k in ["APIC", "TXXX", "tbpm", "TDAT", "UFID", "TBPM:x", "PRIV:x"] {
            assert_eq!(Key::of(k), None, "{k}");
        }
    }

    #[test]
    fn items_and_counts() {
        let mut s = TagSet::new();
        s.set(TagItem::Album, "A");
        s.set_custom("keep", "1");
        s.set_custom("drop", "2");
        let o = Overrides {
            set: vec![
                (Key::Item(TagItem::TrackNumber), "3/12".into()),
                (Key::Item(TagItem::Album), String::new()),
                (Key::Txxx("drop".into()), String::new()),
                (Key::Txxx("new".into()), "v".into()),
            ],
            ..Overrides::default()
        };
        o.apply(&mut s);
        assert_eq!(s.get(TagItem::TrackNumber), ["3"]);
        assert_eq!(s.get(TagItem::TrackCount), ["12"]);
        assert!(s.get(TagItem::Album).is_empty());
        let c: Vec<&str> = s.custom().iter().map(|(d, _)| d.as_str()).collect();
        assert_eq!(c, ["keep", "new"]);
    }

    #[test]
    fn appended_frames_parse() {
        let o = Overrides {
            frames: vec![
                (*b"UFID", Some("http://musicbrainz.org".into()), "id".into()),
                (*b"TBPM", None, "120".into()),
            ],
            art: Some(("image/png", Arc::new(b"\x89PNG\r\n\x1a\nxx".to_vec()))),
            ..Overrides::default()
        };
        let t = append_frames(&[], &o).unwrap();
        let frames = crate::verify::id3::frames(&t);
        let keys: Vec<&str> = frames.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["UFID:http://musicbrainz.org", "TBPM", "APIC"]);
        assert_eq!(frames[0].value, "id");
        assert_eq!(frames[1].value, "120");
    }

    #[test]
    fn pictures() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.jpg"), b"\xFF\xD8\xFFrest").unwrap();
        fs::write(d.path().join("b.gif"), b"GIF89a").unwrap();
        assert_eq!(read_picture(d.path(), "a.jpg").unwrap().0, "image/jpeg");
        assert!(read_picture(d.path(), "b.gif").is_err());
        assert!(read_picture(d.path(), "../a.jpg").is_err());
        std::os::unix::fs::symlink(d.path().join("a.jpg"), d.path().join("l.jpg")).unwrap();
        assert!(read_picture(d.path(), "l.jpg").is_err());
    }
}
