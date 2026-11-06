// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs tags fetch` (#185, phase 2): a disc's tags from MusicBrainz, on demand, written to the
//! tags directory (#81). This code runs only in that command: never in a mount, never in the FUSE
//! path, and nothing reaches the network unless the command is run.
//!
//! The method is pysacd's (`pysacd/musicbrainz.py`): find releases by barcode or catalogue number,
//! then confirm a medium against the area's track count, durations and ISRCs. It is stricter
//! where pysacd takes the best candidate, because *a wrong match is worse than none*: see
//! docs/design.md, "Looking tags up". In short:
//!
//! - Search results are only candidates: a release must carry the number searched for, or the
//!   names searched for, and **every** track must fit ([`confirm`]).
//! - The weaker the way a candidate was found, the tighter the durations must fit ([`Step`]).
//! - Candidates left after preferring SACD media must agree on all the text to be written. If
//!   they don't, the match is ambiguous and nothing is written ([`decide`]). Ids and dates only
//!   some of them share are left out.

mod mb;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use scarletbook::output::TagItem;
use scarletbook::{Slot, Toc};

use crate::disc::{Disc, Tuning};
use crate::dsf::{self, MC_ALBUM_SUFFIX};
use crate::fallback;
use crate::tags::{self, Fields, ITEMS, TagsFile};
use crate::tags_cli::{self, AreaFields};

pub use mb::{
    Client, Error, Hit, Http, MbTrack, Medium, Release, Reply, Transport, is_mbid, parse_release,
    release_hits,
};

/// The key of a release's id.
pub const ALBUM_ID: &str = "TXXX:MusicBrainz Album Id";
/// The key of a release group's id. Its presence tells that an area was looked up.
pub const GROUP_ID: &str = "TXXX:MusicBrainz Release Group Id";
/// The key of a recording's id.
pub const RECORDING_ID: &str = "UFID:http://musicbrainz.org";
/// How many discs without an answer in a row end a run. With the waits between them
/// ([`Client::cool_off`]: 1, 2, 4 and 8 minutes) that is a quarter of an hour of an outage.
const MAX_FAILED_IN_A_ROW: usize = 5;

/// How tightly durations must fit, in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// The mean difference over the tracks.
    pub mean: f64,
    /// The largest difference of a track.
    pub worst: f64,
}

/// The ways of finding candidates, in the order they are tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// By the disc's barcode or catalogue number.
    Number,
    /// By album and artist (`--lookup-name`).
    Name,
    /// By the durations alone (`--lookup-durations`).
    Durations,
}

impl Step {
    /// The limits a candidate found this way must meet. *Why these:* a number identifies the
    /// release, so the durations only guard against a wrong number, and pysacd's 3 s mean is
    /// kept, with a limit per track so that one wrong track can't hide in the mean. A name says
    /// less. Durations alone say the least, and a fuzzy disc lookup returns near misses on
    /// purpose, so there only the same master fits: within CD-track rounding and a pregap.
    pub fn limits(self) -> Limits {
        match self {
            Step::Number => Limits { mean: 3.0, worst: 6.0 },
            Step::Name => Limits { mean: 2.0, worst: 4.0 },
            Step::Durations => Limits { mean: 1.0, worst: 2.5 },
        }
    }

    fn label(self) -> &'static str {
        match self {
            Step::Number => "barcode or catalogue number",
            Step::Name => "name",
            Step::Durations => "durations",
        }
    }
}

/// Fewest tracks a lookup by durations alone will trust.
pub const MIN_DURATION_TRACKS: usize = 4;

/// What the lookup needs of a track.
#[derive(Clone, Debug, Default)]
pub struct TrackFacts {
    /// Its number.
    pub number: u8,
    /// Its start, in 1/75 s from the area's start.
    pub start_frame: u32,
    /// Its length, in 1/75 s.
    pub duration_frames: u32,
    /// Its ISRC, if the disc has one.
    pub isrc: Option<String>,
    /// The keys the disc's own text sets: a lookup doesn't replace those.
    pub on_disc: BTreeSet<String>,
}

/// What the lookup needs of an area.
#[derive(Clone, Debug)]
pub struct AreaFacts {
    /// Which area.
    pub slot: Slot,
    /// Its tracks.
    pub tracks: Vec<TrackFacts>,
}

impl AreaFacts {
    fn name(&self) -> &'static str {
        if self.slot == Slot::Multichannel { "multichannel" } else { "stereo" }
    }

    /// The CD-style TOC of its tracks (`1+N+lead-out+offsets`), with the CD's 150-frame lead-in:
    /// SACD timecodes count the same 1/75 s. `None` if a CD could not have it.
    pub fn cd_toc(&self) -> Option<String> {
        const LEAD_IN: u64 = 150;
        let n = self.tracks.len();
        if !(1..=99).contains(&n) {
            return None;
        }
        let mut parts = Vec::with_capacity(n + 3);
        parts.push("1".to_owned());
        parts.push(n.to_string());
        let last = self.tracks.last()?;
        parts.push((LEAD_IN + u64::from(last.start_frame) + u64::from(last.duration_frames)).to_string());
        for w in self.tracks.windows(2) {
            if w[0].start_frame >= w[1].start_frame {
                return None;
            }
        }
        for t in &self.tracks {
            parts.push((LEAD_IN + u64::from(t.start_frame)).to_string());
        }
        Some(parts.join("+"))
    }
}

/// What the lookup needs of a disc.
#[derive(Clone, Debug, Default)]
pub struct DiscFacts {
    /// The catalogue numbers of its album and disc fields.
    pub numbers: Vec<String>,
    /// How many discs its set has, by its own word.
    pub set_size: u16,
    /// Which of them it is.
    pub set_position: u16,
    /// Its areas.
    pub areas: Vec<AreaFacts>,
    /// The album to search for: the disc's text, else from the image's path.
    pub album: Option<String>,
    /// The artist to search for: as `album`.
    pub artist: Option<String>,
    /// The year in the image's path, if any.
    pub year: Option<String>,
}

impl DiscFacts {
    /// The facts of the disc with `toc` in the image `image`.
    pub fn new(toc: &Toc, image: &Path) -> DiscFacts {
        let mut album = None;
        let mut artist = None;
        let areas: Vec<AreaFacts> = toc
            .areas
            .iter()
            .map(|a| {
                let tracks: Vec<TrackFacts> = a
                    .tracks
                    .iter()
                    .map(|t| {
                        let set = dsf::disc_tag_set(toc, a, t);
                        let first = |item| {
                            set.get(item).iter().map(|v| v.trim()).find(|v| !v.is_empty()).map(str::to_owned)
                        };
                        let on_disc: BTreeSet<String> = ITEMS
                            .iter()
                            .filter(|(_, _, item)| first(*item).is_some())
                            .map(|(name, _, _)| (*name).to_owned())
                            .collect();
                        if album.is_none() {
                            album = first(TagItem::Album)
                                .map(|v| v.strip_suffix(MC_ALBUM_SUFFIX).map_or(v.clone(), str::to_owned));
                        }
                        if let Some(v) = first(TagItem::AlbumArtist) {
                            artist = Some(v);
                        } else if artist.is_none() {
                            artist = first(TagItem::Performer);
                        }
                        TrackFacts {
                            number: t.number,
                            start_frame: t.start_frame,
                            duration_frames: t.duration_frames,
                            isrc: t.isrc.clone(),
                            on_disc,
                        }
                    })
                    .collect();
                AreaFacts { slot: a.slot, tracks }
            })
            .collect();
        let guess = fallback::guess(image);
        DiscFacts {
            numbers: numbers(toc),
            set_size: toc.master.album_set_size,
            set_position: toc.master.album_sequence_number,
            areas,
            album: album.or(guess.album),
            artist: artist.or(guess.artist),
            year: fallback::year(image),
        }
    }
}

/// The disc's catalogue numbers worth asking about: the album's and the disc's, without repeats.
/// A field of a few characters, or all zeros, says nothing.
fn numbers(toc: &Toc) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in [&toc.master.album_catalog_number, &toc.master.disc_catalog_number] {
        let n = n.trim();
        let plain = catalogue_form(n);
        if plain.len() >= 3 && !plain.bytes().all(|b| b == b'0') && !out.iter().any(|o| o == n) {
            out.push(n.to_owned());
        }
    }
    out
}

/// A catalogue number for comparing: letters and digits, upper case. Labels write the same number
/// with and without spaces and hyphens.
fn catalogue_form(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_uppercase).collect()
}

/// `s` as a barcode, if it reads as one: 12 to 14 digits, not all zeros (UPC, EAN, GTIN), with any
/// spaces out. *As pysacd:* the album's catalogue field holds one on most SACDs.
pub fn barcode_of(s: &str) -> Option<String> {
    let d: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    ((12..=14).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_digit()) && !d.bytes().all(|b| b == b'0'))
        .then_some(d)
}

/// What confirmed a medium.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Evidence {
    /// The mean difference in durations over the tracks compared, in seconds.
    pub mean: f64,
    /// The largest difference.
    pub worst: f64,
    /// Tracks whose ISRC the recording has.
    pub isrc_hits: usize,
}

/// An ISRC for comparing: some writers include hyphens.
fn isrc_form(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

/// Whether `medium` is what `area` holds: the same track count; every track's duration within
/// `limits`, or, for a track without a length, its ISRC on the recording; and no more ISRCs that
/// disagree than agree. *Stricter than pysacd,* which takes a mean over the tracks that have
/// lengths, so that one track with a length could confirm a whole disc, and which lets ISRC hits
/// excuse durations that don't fit.
pub fn confirm(area: &AreaFacts, medium: &Medium, limits: Limits) -> Result<Evidence, String> {
    if medium.tracks.len() != area.tracks.len() {
        return Err(format!("{} tracks, the area has {}", medium.tracks.len(), area.tracks.len()));
    }
    let (mut sum, mut worst, mut compared, mut hits, mut misses) = (0.0f64, 0.0f64, 0usize, 0usize, 0usize);
    for (mt, at) in medium.tracks.iter().zip(&area.tracks) {
        let isrc = at.isrc.as_deref().map(isrc_form).filter(|i| !i.is_empty());
        let hit = isrc.as_ref().is_some_and(|i| mt.isrcs.iter().any(|m| isrc_form(m) == *i));
        if hit {
            hits += 1;
        } else if isrc.is_some() && !mt.isrcs.is_empty() {
            misses += 1;
        }
        match mt.length_ms {
            Some(ms) => {
                let d = (ms as f64 / 1000.0 - f64::from(at.duration_frames) / 75.0).abs();
                sum += d;
                worst = worst.max(d);
                compared += 1;
            }
            None if hit => {}
            None => return Err(format!("track {} has no length to compare", at.number)),
        }
    }
    if compared == 0 {
        return Err("no track has a length to compare".to_owned());
    }
    let mean = sum / compared as f64;
    if worst > limits.worst {
        return Err(format!("a track is {worst:.1} s off (limit {:.1} s)", limits.worst));
    }
    if mean > limits.mean {
        return Err(format!("tracks are {mean:.1} s off on average (limit {:.1} s)", limits.mean));
    }
    if misses > hits {
        return Err(format!("ISRCs disagree on {misses} tracks, agree on {hits}"));
    }
    Ok(Evidence { mean, worst, isrc_hits: hits })
}

/// The physical disc of each medium: MusicBrainz may list each layer of one disc as its own
/// medium. As pysacd's `physical_discs`.
fn physical_discs(media: &[Medium]) -> Vec<u16> {
    let mut out = Vec::with_capacity(media.len());
    let mut n = 0u16;
    let mut base = String::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for m in media {
        let f = m.format.as_str();
        let b = f.split(" (").next().unwrap_or(f);
        let layered = f.contains('(') && (f.contains("layer") || f.contains("channel"));
        if !layered || b != base || seen.contains(f) || (f.contains("(CD layer") && !seen.is_empty()) {
            n = n.saturating_add(1);
            base = b.to_owned();
            seen.clear();
        }
        seen.insert(f);
        out.push(n);
    }
    out
}

/// How well a medium's format suits an area: its SACD layer for these channels, then an SACD,
/// then a layer of the other channels, then anything else (a CD, a CD layer, a download).
fn format_rank(format: &str, multichannel: bool) -> u8 {
    if !format.contains("SACD") || format.contains("CD layer") {
        return 0;
    }
    if !format.contains("channel") {
        return 2;
    }
    if format.contains("multichannel") == multichannel { 3 } else { 1 }
}

/// A medium that passed [`confirm`].
#[derive(Clone, Debug)]
pub struct Candidate {
    release: Release,
    medium: Medium,
    evidence: Evidence,
    rank: u8,
}

/// Whether `release` carries what it was searched for. A search matches loosely, on analysed
/// words: this is the check that the number or the name is really on it.
fn carries(release: &Release, facts: &DiscFacts, step: Step) -> Result<(), String> {
    match step {
        Step::Number => {
            let digits = |s: &str| s.trim_start_matches('0').to_owned();
            let barcode = release.barcode.as_deref().map(|b| digits(&catalogue_form(b)));
            let cats: Vec<String> = release.catalogue.iter().map(|c| catalogue_form(c)).collect();
            let hit = facts.numbers.iter().any(|n| match barcode_of(n) {
                Some(b) => barcode.as_deref() == Some(digits(&b).as_str()),
                None => cats.contains(&catalogue_form(n)),
            });
            if hit { Ok(()) } else { Err("it doesn't carry the disc's number".to_owned()) }
        }
        Step::Name => {
            let (Some(album), Some(artist)) = (&facts.album, &facts.artist) else {
                return Err("no name to compare".to_owned());
            };
            let title = release.title.as_deref().unwrap_or("");
            if !name_agrees(album, title) {
                return Err("its title differs from the name searched for".to_owned());
            }
            let credit = fallback::normalized(release.credit.as_deref().unwrap_or(""));
            let artist = fallback::normalized(artist);
            if artist.is_empty() || !format!(" {credit} ").contains(&format!(" {artist} ")) {
                return Err("its artist differs from the name searched for".to_owned());
            }
            Ok(())
        }
        Step::Durations => Ok(()),
    }
}

/// Whether two titles are the same, but for case, punctuation, or words added after the end of
/// one (`Album`, `Album: Deluxe`).
fn name_agrees(a: &str, b: &str) -> bool {
    let (a, b) = (fallback::normalized(a), fallback::normalized(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let starts = |x: &str, y: &str| x == y || x.strip_prefix(y).is_some_and(|r| r.starts_with(' '));
    starts(&a, &b) || starts(&b, &a)
}

/// The media of `release` that may be `area`: those that pass [`confirm`], best suited first. Why
/// none does, if none.
fn candidates_of(
    release: &Release,
    area: &AreaFacts,
    facts: &DiscFacts,
    step: Step,
) -> Result<Vec<Candidate>, String> {
    carries(release, facts, step)?;
    let multichannel = area.slot == Slot::Multichannel;
    let discs = physical_discs(&release.media);
    let mut passed: Vec<(u16, Candidate)> = Vec::new();
    let mut why: Vec<String> = Vec::new();
    for (i, m) in release.media.iter().enumerate() {
        match confirm(area, m, step.limits()) {
            Ok(evidence) => passed.push((
                discs.get(i).copied().unwrap_or(0),
                Candidate {
                    release: release.clone(),
                    medium: m.clone(),
                    evidence,
                    rank: format_rank(&m.format, multichannel),
                },
            )),
            Err(e) => why.push(e),
        }
    }
    if passed.is_empty() {
        why.dedup();
        return Err(if why.is_empty() { "it lists no media".to_owned() } else { why.join("; ") });
    }
    if facts.set_size > 1 && passed.iter().any(|(d, _)| *d == facts.set_position) {
        passed.retain(|(d, _)| *d == facts.set_position);
    }
    let best = passed.iter().map(|(_, c)| c.rank).max().unwrap_or(0);
    Ok(passed.into_iter().filter(|(_, c)| c.rank == best).map(|(_, c)| c).collect())
}

/// What one area's text would be: what candidates must agree on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Text {
    album: Option<String>,
    album_artist: Option<String>,
    tracks: Vec<(Option<String>, Option<String>)>,
}

fn text_of(c: &Candidate) -> Text {
    Text {
        album: c.release.title.clone(),
        album_artist: c.release.credit.clone(),
        tracks: c
            .medium
            .tracks
            .iter()
            .map(|t| (t.title.clone(), t.credit.clone().or_else(|| c.release.credit.clone())))
            .collect(),
    }
}

/// The value of `f` if all candidates give the same.
fn agreed(cands: &[Candidate], f: impl Fn(&Candidate) -> Option<String>) -> Option<String> {
    let first = f(cands.first()?)?;
    cands.iter().all(|c| f(c).as_deref() == Some(&first)).then_some(first)
}

/// A confident match of an area: what to write and why.
#[derive(Clone, Debug)]
pub struct Match {
    /// How it was found.
    pub step: Step,
    /// The release it names (the best, if several say the same).
    pub release: String,
    /// The other releases that say the same.
    pub also: Vec<String>,
    /// The medium's format.
    pub format: String,
    /// What confirmed it.
    pub evidence: Evidence,
    /// What a reader should know.
    pub warnings: Vec<String>,
    /// The album-level values.
    pub album: Fields,
    /// The track-level values, by the area's track number.
    pub tracks: AreaFields,
}

/// What became of an area in a step.
#[derive(Clone, Debug)]
pub enum Decision {
    /// A confident match.
    Match(Box<Match>),
    /// More than one thing it could be: stop here, write nothing.
    Ambiguous(String),
    /// Nothing fits.
    None,
}

/// What to do with the candidates of an area: see the module documentation.
pub fn decide(area: &AreaFacts, facts: &DiscFacts, step: Step, cands: Vec<Candidate>) -> Decision {
    let mut left = cands;
    let Some(best) = left.iter().map(|c| c.rank).max() else { return Decision::None };
    // A digital release or CD with the same barcode says less about this disc than the SACD does.
    left.retain(|c| c.rank == best);
    let hits = left.iter().map(|c| c.evidence.isrc_hits).max().unwrap_or(0);
    left.retain(|c| c.evidence.isrc_hits == hits);
    // The year only chooses between candidates, since a folder's year may be a reissue's.
    if let Some(y) = &facts.year {
        let dated = |c: &Candidate| c.release.date.as_deref().is_some_and(|d| d.starts_with(y.as_str()));
        if left.iter().any(dated) {
            left.retain(dated);
        }
    }
    left.sort_by(|a, b| a.evidence.mean.total_cmp(&b.evidence.mean));
    let Some(first) = left.first().cloned() else { return Decision::None };
    let text = text_of(&first);
    if left.iter().any(|c| text_of(c) != text) {
        let mut ids: Vec<&str> = left.iter().map(|c| c.release.id.as_str()).collect();
        ids.dedup();
        return Decision::Ambiguous(format!(
            "{} candidates fit but say different things: {}",
            left.len(),
            ids.join(", ")
        ));
    }
    let mut album = Fields::new();
    let mut put = |key: &str, v: Option<String>| {
        if let Some(v) = v {
            album.insert(key.to_owned(), v);
        }
    };
    let multichannel = area.slot == Slot::Multichannel;
    put("album", text.album.map(|a| if multichannel { format!("{a}{MC_ALBUM_SUFFIX}") } else { a }));
    put("album_artist", text.album_artist);
    put(GROUP_ID, agreed(&left, |c| c.release.group.clone()));
    // Facts of one pressing: a name or durations find the album, not the pressing.
    if step == Step::Number {
        put("date", agreed(&left, |c| c.release.date.clone()));
        put(ALBUM_ID, agreed(&left, |c| Some(c.release.id.clone())));
    }
    let mut tracks = AreaFields::new();
    for (i, at) in area.tracks.iter().enumerate() {
        let (title, artist) = text.tracks.get(i).cloned().unwrap_or((None, None));
        let recording = agreed(&left, |c| c.medium.tracks.get(i).and_then(|t| t.recording.clone()));
        let fields = tracks.entry(at.number).or_default();
        for (key, v) in [("title", title), ("artist", artist), (RECORDING_ID, recording)] {
            if let Some(v) = v {
                fields.insert(key.to_owned(), v);
            }
        }
    }
    let mut warnings = Vec::new();
    if first.rank == 0 {
        warnings.push(format!("the medium's format is {:?}, not an SACD", first.medium.format));
    }
    if step != Step::Number {
        warnings.push(format!(
            "found by {}: the pressing isn't known, so no date or release id is written",
            step.label()
        ));
    }
    let mut also: Vec<String> =
        left.iter().skip(1).map(|c| c.release.id.clone()).filter(|i| *i != first.release.id).collect();
    also.dedup();
    Decision::Match(Box::new(Match {
        step,
        release: first.release.id.clone(),
        also,
        format: first.medium.format.clone(),
        evidence: first.evidence,
        warnings,
        album,
        tracks,
    }))
}

/// The options of a run.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Also search by album and artist.
    pub by_name: bool,
    /// Also ask by durations.
    pub by_durations: bool,
    /// Write nothing, say what would be.
    pub dry_run: bool,
    /// Don't add [`MC_ALBUM_SUFFIX`] to an album written for a multichannel area: for a mount
    /// with `--no-multichannel-album-suffix`.
    pub no_suffix: bool,
}

/// What a run did.
#[derive(Debug, Default)]
pub struct Summary {
    /// Images all of whose areas matched.
    pub matched: usize,
    /// Images with an area that didn't: nothing fit, or more than one thing did.
    pub refused: usize,
    /// Images already looked up: nothing was asked.
    pub done: usize,
    /// Images that failed: no read, no answer, or a tags file that doesn't parse.
    pub failed: usize,
    /// Images not tried, after failures in a row.
    pub not_tried: usize,
    /// Tags files written.
    pub written: usize,
    /// Keys added.
    pub keys: usize,
    /// Requests made.
    pub requests: usize,
}

impl Summary {
    /// The human summary.
    pub fn render(&self) -> String {
        let mut s = format!(
            "tags fetch: {} matched, {} refused, {} already looked up, {} failed; {} tags files written, {} keys added, {} requests\n",
            self.matched, self.refused, self.done, self.failed, self.written, self.keys, self.requests
        );
        if self.not_tried > 0 {
            s.push_str(&format!(
                "not tried, after {MAX_FAILED_IN_A_ROW} failures in a row: {}\n",
                self.not_tried
            ));
        }
        s
    }

    /// Whether every image ended matched or already looked up: exit status 0.
    pub fn all_handled(&self) -> bool {
        self.refused == 0 && self.failed == 0 && self.not_tried == 0
    }
}

/// `sacdfs tags fetch`: looks each of `images` up and writes the results in the tags directory
/// `dir`, telling `say` a line about each area.
pub fn fetch<T: Transport>(
    dir: &Path,
    images: &[PathBuf],
    options: Options,
    client: &mut Client<T>,
    mut say: impl FnMut(String),
) -> Summary {
    let mut sum = Summary::default();
    let mut failed_in_a_row = 0;
    // Discs without an answer get a second try at the end of the run: a large library meets a
    // busy service sooner or later, and one disc's bad moment shouldn't need a second run.
    let mut again: Vec<&PathBuf> = Vec::new();
    let mut retrying = false;
    let mut queue: Vec<&PathBuf> = images.iter().collect();
    let mut i = 0;
    let mut stopped = false;
    loop {
        if !stopped && i == queue.len() && !retrying && !again.is_empty() {
            say(format!("{} images without an answer: trying them once more", again.len()));
            queue = std::mem::take(&mut again);
            i = 0;
            retrying = true;
            failed_in_a_row = 0;
        }
        let Some(image) = queue.get(i).copied() else { break };
        if stopped || failed_in_a_row >= MAX_FAILED_IN_A_ROW {
            sum.not_tried = queue.len() - i;
            break;
        }
        i += 1;
        client.next_disc();
        match fetch_one(dir, image, options, client, &mut say, &mut sum) {
            Ok(()) => failed_in_a_row = 0,
            Err(Failure::Network(e)) => {
                failed_in_a_row += 1;
                say(format!("{}: nothing written: {e}", image.display()));
                if retrying {
                    sum.failed += 1;
                } else {
                    again.push(image);
                }
                let wait = client.cool_off(failed_in_a_row);
                if failed_in_a_row < MAX_FAILED_IN_A_ROW && !wait.is_zero() {
                    say(format!("waiting {} s before the next image", wait.as_secs()));
                    std::thread::sleep(wait);
                }
            }
            Err(Failure::Stop(e)) => {
                sum.failed += 1;
                stopped = true;
                say(format!("{}: nothing written: {e}", image.display()));
            }
            Err(Failure::Local(e)) => {
                sum.failed += 1;
                say(format!("{}: nothing written: {e}", image.display()));
            }
        }
    }
    // Those a run ended before asking again.
    sum.failed += again.len();
    sum.requests = client.requests();
    sum
}

/// Per area, its match, or why there is none.
pub type Outcomes = (Vec<Option<Match>>, Vec<Option<String>>);

/// Why an image failed.
#[derive(Debug)]
pub enum Failure {
    /// No usable answer: others may fail the same way.
    Network(String),
    /// The service said to stay away: nothing more is asked in this run.
    Stop(String),
    /// This image or its tags file.
    Local(String),
}

fn fetch_one<T: Transport>(
    dir: &Path,
    image: &Path,
    options: Options,
    client: &mut Client<T>,
    say: &mut impl FnMut(String),
    sum: &mut Summary,
) -> Result<(), Failure> {
    let disc = Disc::open(image, 0, Tuning::default()).map_err(|e| Failure::Local(e.to_string()))?;
    let toc = disc.toc();
    let key = tags::disc_key(toc);
    let path = dir.join(format!("{key}.toml"));
    // Before anything is asked: a tags file that can't be merged into can't be written.
    let existing = read_tags(&path).map_err(Failure::Local)?;
    let facts = DiscFacts::new(toc, image);
    let shown = image.display();

    // Once per disc: an area that didn't fit the release its sibling matched gets no second try by
    // a weaker way, which could give one disc two albums.
    if existing.as_ref().is_some_and(|f| facts.areas.iter().any(|a| looked_up(f, a))) {
        say(format!(
            "{shown}: already looked up ({key}): nothing asked. To ask again, remove `{GROUP_ID}` from its tags file"
        ));
        sum.done += 1;
        return Ok(());
    }
    let pending: Vec<&AreaFacts> = facts.areas.iter().collect();

    let (found, why) = look_up(&facts, &pending, options, client)?;
    let matched = found.iter().filter(|m| m.is_some()).count();
    for (a, reason) in pending.iter().zip(&why) {
        if let Some(reason) = reason {
            say(format!("{shown}: {}: nothing written: {reason}", a.name()));
        }
    }
    if matched < pending.len() {
        sum.refused += 1;
    } else {
        sum.matched += 1;
    }
    if matched == 0 {
        return Ok(());
    }
    for (a, m) in pending.iter().zip(&found) {
        if let Some(m) = m {
            let e = m.evidence;
            let also =
                if m.also.is_empty() { String::new() } else { format!(" (also {})", m.also.join(", ")) };
            say(format!(
                "{shown}: {}: matched by {}: release {}{also}, {}, {} tracks, durations {:.1} s off on average, {:.1} s at worst, {} ISRCs agree",
                a.name(),
                m.step.label(),
                m.release,
                if m.format.is_empty() { "no format given" } else { m.format.as_str() },
                a.tracks.len(),
                e.mean,
                e.worst,
                e.isrc_hits,
            ));
            for w in &m.warnings {
                say(format!("{shown}: {}: note: {w}", a.name()));
            }
        }
    }

    let mut merged = existing.unwrap_or_default();
    let planned = plan(&mut merged, &pending, &found, options);
    let added = tags_cli::merge(&mut merged, &planned);
    if options.dry_run {
        say(format!(
            "{shown}: dry run: {added} keys would be added to {}:\n{}",
            path.display(),
            planned.to_text()
        ));
        return Ok(());
    }
    if added == 0 {
        say(format!("{shown}: nothing to add: the disc or its tags file already has every value"));
        return Ok(());
    }
    // The file must read back as what was meant: if not, it would be ignored by the mount.
    let text = merged.to_text();
    TagsFile::parse(&text).map_err(|e| Failure::Local(format!("the merged tags file doesn't parse: {e}")))?;
    fs::create_dir_all(dir).map_err(|e| Failure::Local(format!("{}: {e}", dir.display())))?;
    tags_cli::write_atomic(&path, text.as_bytes())
        .map_err(|e| Failure::Local(format!("{}: {e}", path.display())))?;
    say(format!("{shown}: {added} keys added to {}", path.display()));
    sum.written += 1;
    sum.keys += added;
    Ok(())
}

/// The tags file at `path`, if there is one.
fn read_tags(path: &Path) -> Result<Option<TagsFile>, String> {
    match fs::read_to_string(path) {
        Ok(text) => TagsFile::parse(&text).map(Some).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Whether a tags file already holds a lookup for this area: its first track has a release group.
fn looked_up(file: &TagsFile, area: &AreaFacts) -> bool {
    area.tracks.first().is_some_and(|t| file.fields_for(area.slot, t.number).contains_key(GROUP_ID))
}

/// Runs the steps in order on `areas`. The first step with a match or an ambiguity ends it: no
/// weaker way may overrule what a stronger one found, and an area that didn't fit the release its
/// sibling matched doesn't get another release. The match and the reason there is none, per area.
pub fn look_up<T: Transport>(
    facts: &DiscFacts,
    areas: &[&AreaFacts],
    options: Options,
    client: &mut Client<T>,
) -> Result<Outcomes, Failure> {
    let mut tried: Vec<String> = Vec::new();
    let mut steps = vec![Step::Number];
    if options.by_name {
        steps.push(Step::Name);
    }
    if options.by_durations {
        steps.push(Step::Durations);
    }
    let mut releases: HashMap<String, Option<Release>> = HashMap::new();
    let mut notes: Vec<Vec<String>> = vec![Vec::new(); areas.len()];
    for step in steps {
        let mut found: Vec<Option<Match>> = Vec::new();
        let mut why: Vec<Option<String>> = Vec::new();
        let mut any = false;
        for (k, area) in areas.iter().enumerate() {
            let hits = match hits_for(step, facts, area, client) {
                Ok(Ok(hits)) => hits,
                Ok(Err(not_asked)) => {
                    notes[k].push(not_asked);
                    found.push(None);
                    why.push(None);
                    continue;
                }
                Err(HitsError::Network(e)) => return Err(Failure::Network(e)),
                Err(HitsError::Stop(e)) => return Err(Failure::Stop(e)),
            };
            let mut cands = Vec::new();
            if hits.is_empty() {
                notes[k].push(format!("by {}: no release found", step.label()));
            }
            for hit in hits {
                let id = hit.id.clone();
                if !hit.may_have(area.tracks.len()) {
                    notes[k].push(format!(
                        "by {}: release {id}: no medium of {} tracks",
                        step.label(),
                        area.tracks.len()
                    ));
                    continue;
                }
                let release = match releases.get(&id) {
                    Some(r) => r.clone(),
                    None => {
                        let r = match client.release(&id) {
                            Ok(r) => Some(r),
                            Err(Error::NotFound) => None,
                            Err(Error::Failed(e)) => return Err(Failure::Network(e)),
                            Err(Error::Stop(e)) => return Err(Failure::Stop(e)),
                        };
                        releases.insert(id.clone(), r.clone());
                        r
                    }
                };
                let Some(release) = release else { continue };
                match candidates_of(&release, area, facts, step) {
                    Ok(c) => cands.extend(c),
                    Err(e) => notes[k].push(format!("by {}: release {id}: {e}", step.label())),
                }
            }
            match decide(area, facts, step, cands) {
                Decision::Match(m) => {
                    any = true;
                    found.push(Some(*m));
                    why.push(None);
                }
                Decision::Ambiguous(e) => {
                    any = true;
                    found.push(None);
                    why.push(Some(format!("ambiguous by {}: {e}", step.label())));
                }
                Decision::None => {
                    found.push(None);
                    why.push(None);
                }
            }
        }
        tried.push(step.label().to_owned());
        if any {
            // One disc is one album: anything else is no confident match of either area.
            let albums: BTreeSet<String> = found
                .iter()
                .flatten()
                .filter_map(|m: &Match| m.album.get("album"))
                .map(|a| fallback::normalized(a.strip_suffix(MC_ALBUM_SUFFIX).unwrap_or(a)))
                .collect();
            if albums.len() > 1 {
                let why = format!("ambiguous by {}: the areas match different albums", step.label());
                return Ok(((0..areas.len()).map(|_| None).collect(), vec![Some(why); areas.len()]));
            }
            if let Some(a) = why.iter().flatten().next().cloned() {
                let reasons = why.iter().map(|w| Some(w.clone().unwrap_or_else(|| a.clone()))).collect();
                return Ok(((0..areas.len()).map(|_| None).collect(), reasons));
            }
            let why = why
                .into_iter()
                .zip(&found)
                .zip(&notes)
                .map(|((w, m), n): ((Option<String>, _), _)| match (w, m) {
                    (_, Some(_)) => None,
                    (_, None) => Some(format!(
                        "another area matched, this one doesn't fit: {}",
                        if n.is_empty() { "no candidate".to_owned() } else { n.join("; ") }
                    )),
                })
                .collect();
            return Ok((found, why));
        }
    }
    let why = notes
        .iter()
        .map(|n| Some(format!("no confident match (tried {}): {}", tried.join(", "), n.join("; "))))
        .collect();
    Ok(((0..areas.len()).map(|_| None).collect(), why))
}

/// Why a step's search gave nothing to go on.
enum HitsError {
    Network(String),
    Stop(String),
}

/// The releases a step finds for `area`. Inside: `Err` if the step has nothing to ask, and why.
fn hits_for<T: Transport>(
    step: Step,
    facts: &DiscFacts,
    area: &AreaFacts,
    client: &mut Client<T>,
) -> Result<Result<Vec<Hit>, String>, HitsError> {
    let net = |e: Error| match e {
        Error::NotFound => HitsError::Network("not found".to_owned()),
        Error::Failed(e) => HitsError::Network(format!("MusicBrainz: {e}")),
        Error::Stop(e) => HitsError::Stop(format!("MusicBrainz: {e}")),
    };
    match step {
        Step::Number => {
            if facts.numbers.is_empty() {
                return Ok(Err("the disc has no barcode or catalogue number".to_owned()));
            }
            let mut hits: Vec<Hit> = Vec::new();
            for n in &facts.numbers {
                let got = match barcode_of(n) {
                    Some(b) => client.by_barcode(&b),
                    None => client.by_catalogue(n),
                }
                .map_err(net)?;
                for hit in got {
                    if !hits.iter().any(|h| h.id == hit.id) {
                        hits.push(hit);
                    }
                }
            }
            Ok(Ok(hits))
        }
        Step::Name => match (&facts.album, &facts.artist) {
            (Some(album), Some(artist)) => Ok(Ok(client.by_name(album, artist).map_err(net)?)),
            _ => Ok(Err("no album and artist to search by, on the disc or in its path".to_owned())),
        },
        Step::Durations => {
            if area.tracks.len() < MIN_DURATION_TRACKS {
                return Ok(Err(format!(
                    "{} tracks are too few to identify a disc by durations ({MIN_DURATION_TRACKS} at least)",
                    area.tracks.len()
                )));
            }
            match area.cd_toc() {
                Some(toc) => Ok(Ok(client.by_toc(&toc).map_err(net)?)),
                None => Ok(Err("no CD could have this area's tracks".to_owned())),
            }
        }
    }
}

/// What to add to the tags file `existing` for the matched areas. *Who wins:* a value in the tags
/// file, at any level, an empty one included; then the disc's own text; then the lookup. A value
/// goes at the least specific level that says it, so that an edit at a more specific one wins, as
/// the levels do anywhere else.
///
/// `existing` loses any title that is only the generated `Track NN` of its track, if the lookup
/// has one: an import from before this version stored those as if edited by hand.
pub fn plan(
    existing: &mut TagsFile,
    areas: &[&AreaFacts],
    found: &[Option<Match>],
    options: Options,
) -> TagsFile {
    let mut levels: Vec<(Slot, Fields, AreaFields)> = Vec::new();
    for (area, m) in areas.iter().zip(found) {
        let Some(m) = m else { continue };
        let mut tracks = AreaFields::new();
        for t in &area.tracks {
            let Some(values) = m.tracks.get(&t.number) else { continue };
            if values.contains_key("title") && !t.on_disc.contains("title") {
                existing.clear_generated_title(area.slot, t.number);
            }
            let have = existing.fields_for(area.slot, t.number);
            let keep: Fields = values
                .iter()
                .filter(|(k, _)| !t.on_disc.contains(*k) && !have.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            tracks.insert(t.number, keep);
        }
        let all: BTreeSet<u8> = area.tracks.iter().map(|t| t.number).collect();
        let shared = tags_cli::common(&tracks, &all);
        for f in tracks.values_mut() {
            f.retain(|k, _| !shared.contains_key(k));
        }
        let have = existing.album_fields_for(area.slot);
        let disc_has = |k: &str| area.tracks.iter().all(|t| t.on_disc.contains(k));
        let mut album: Fields = m
            .album
            .iter()
            .filter(|(k, _)| !have.contains_key(*k) && !disc_has(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        album.extend(shared);
        if options.no_suffix && area.slot == Slot::Multichannel {
            if let Some(a) = album.get_mut("album") {
                if let Some(s) = a.strip_suffix(MC_ALBUM_SUFFIX) {
                    *a = s.to_owned();
                }
            }
        }
        levels.push((area.slot, album, tracks));
    }
    let mut out = TagsFile::default();
    // Said alike of both areas: the disc's.
    if let [(_, a0, t0), (_, a1, t1)] = levels.as_mut_slice() {
        let same: Vec<String> =
            a0.iter().filter(|(k, v)| a1.get(*k) == Some(*v)).map(|(k, _)| k.clone()).collect();
        for k in same {
            if let (Some(v), Some(_)) = (a0.remove(&k), a1.remove(&k)) {
                out.album.insert(k, v);
            }
        }
        for (n, f0) in t0.iter_mut() {
            let Some(f1) = t1.get_mut(n) else { continue };
            let same: Vec<String> =
                f0.iter().filter(|(k, v)| f1.get(*k) == Some(*v)).map(|(k, _)| k.clone()).collect();
            for k in same {
                if let (Some(v), Some(_)) = (f0.remove(&k), f1.remove(&k)) {
                    out.track.entry(n.to_string()).or_default().insert(k, v);
                }
            }
        }
    }
    for (slot, album, tracks) in levels {
        let level = if slot == Slot::Multichannel { &mut out.multichannel } else { &mut out.stereo };
        level.album = album;
        for (n, f) in tracks {
            if !f.is_empty() {
                level.track.insert(n.to_string(), f);
            }
        }
    }
    out
}
