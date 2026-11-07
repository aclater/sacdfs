// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs tags fetch` (#185, phase 2), from recorded responses (tests/fixtures/musicbrainz): what
//! is confirmed, what is refused and why, what a tags file ends up holding, and that nothing is
//! asked without need. Nothing here touches the network.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sacdfs::disc::{Disc, Tuning};
use sacdfs::lookup::{
    self, AreaFacts, Client, DiscFacts, Match, Medium, Options, Reply, Step, TrackFacts, Transport, confirm,
};
use sacdfs::tags::{self, TagsFile};
use scarletbook::Slot;
use serde_json::{Value, json};

const HYBRID: &str = "0000001d-0000-4000-8000-00000000001d";
const TWIN: &str = "0000001f-0000-4000-8000-00000000001f";
const BOX: &str = "0000002d-0000-4000-8000-00000000002d";
const GROUP: &str = "0000001e-0000-4000-8000-00000000001e";
const BARCODE: &str = "0000000012345";

fn fixture(name: &str) -> Value {
    let p =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/musicbrainz").join(format!("{name}.json"));
    serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
}

/// Answers from a table: the first entry whose key the URL holds, else 404. Keeps every URL.
#[derive(Debug, Default)]
struct Recorded {
    answers: Vec<(String, Value)>,
    urls: Vec<String>,
}

impl Recorded {
    fn with(mut self, key: &str, v: Value) -> Recorded {
        self.answers.push((key.to_owned(), v));
        self
    }
}

impl Transport for Recorded {
    fn get(&mut self, url: &str) -> Result<Reply, String> {
        self.urls.push(url.to_owned());
        match self.answers.iter().find(|(k, _)| url.contains(k.as_str())) {
            Some((_, v)) => Ok(Reply { status: 200, body: v.to_string().into_bytes(), retry_after: None }),
            None => Ok(Reply { status: 404, body: b"{}".to_vec(), retry_after: None }),
        }
    }
}

fn client(r: Recorded) -> Client<Recorded> {
    Client::paced(r, Duration::ZERO, Duration::ZERO)
}

/// The releases of the recording, by id.
fn releases() -> Recorded {
    Recorded::default()
        .with(&format!("release/{HYBRID}"), fixture("release-hybrid"))
        .with(&format!("release/{TWIN}"), fixture("release-digital-twin"))
        .with(&format!("release/{BOX}"), fixture("release-box"))
}

/// A stereo area with the recorded hybrid's tracks, each `off` seconds longer.
fn recorded_area(off: f64) -> AreaFacts {
    let hybrid = fixture("release-hybrid");
    let mut start = 0;
    let tracks = hybrid["media"][0]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let duration_frames = ((t["length"].as_f64().unwrap() / 1000.0 + off) * 75.0) as u32;
            let f = TrackFacts {
                number: i as u8 + 1,
                start_frame: start,
                duration_frames,
                ..TrackFacts::default()
            };
            start += duration_frames;
            f
        })
        .collect();
    AreaFacts { slot: Slot::TwoChannel, tracks }
}

fn disc(numbers: &[&str]) -> DiscFacts {
    DiscFacts {
        numbers: numbers.iter().map(|n| (*n).to_owned()).collect(),
        set_size: 1,
        set_position: 1,
        areas: vec![recorded_area(0.4)],
        ..DiscFacts::default()
    }
}

/// The outcome of the one area of `facts`.
fn outcome(facts: &DiscFacts, options: Options, r: Recorded) -> (Option<Match>, Option<String>, Vec<String>) {
    let mut c = client(r);
    let areas: Vec<&AreaFacts> = facts.areas.iter().collect();
    let (mut found, mut why) = lookup::look_up(facts, &areas, options, &mut c).unwrap();
    (found.remove(0), why.remove(0), c.transport().urls.clone())
}

#[test]
fn a_barcode_finds_the_sacd_not_its_digital_twin() {
    let r = releases().with("query=barcode", fixture("search-barcode"));
    let (m, why, urls) = outcome(&disc(&[BARCODE]), Options::default(), r);
    let m = m.unwrap_or_else(|| panic!("{why:?}"));
    assert_eq!(m.step, Step::Number);
    assert_eq!(m.release, HYBRID);
    assert!(m.also.is_empty());
    assert_eq!(m.format, "Hybrid SACD");
    assert!(m.evidence.worst < 1.0, "{:?}", m.evidence);
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(m.album["album"], "Title 17");
    assert_eq!(m.album["date"], "2013-09", "the SACD's date, not its twin's");
    assert_eq!(m.album[lookup::ALBUM_ID], HYBRID);
    assert_eq!(m.album[lookup::GROUP_ID], GROUP);
    assert_eq!(m.tracks[&1]["title"], "Title 5");
    assert!(m.tracks[&1].contains_key(lookup::RECORDING_ID));
    assert_eq!(urls.len(), 3, "a search and two releases: {urls:?}");
    assert!(urls[0].contains("barcode%3A%220000000012345%22"), "{}", urls[0]);
}

#[test]
fn a_catalogue_number_finds_it_however_the_label_wrote_it() {
    for number in ["TEST-1", "test 1", "TEST1"] {
        let r = releases().with("query=catno", fixture("search-catno"));
        let (m, why, _) = outcome(&disc(&[number]), Options::default(), r);
        assert_eq!(m.unwrap_or_else(|| panic!("{why:?}")).release, HYBRID, "{number}");
    }
}

#[test]
fn a_search_result_must_carry_the_number() {
    // The search matches on analysed words: the release it gives has another number.
    let r = releases().with("query=catno", fixture("search-catno"));
    let (m, why, _) = outcome(&disc(&["OTHER-9"]), Options::default(), r);
    assert!(m.is_none());
    let why = why.unwrap();
    assert!(why.contains("doesn't carry the disc's number"), "{why}");
    assert!(why.contains(HYBRID), "{why}");

    let r = releases().with("query=barcode", fixture("search-barcode"));
    let (m, why, _) = outcome(&disc(&["0000000099999"]), Options::default(), r);
    assert!(m.is_none());
    assert!(why.unwrap().contains("doesn't carry"));
}

#[test]
fn no_release_writes_nothing_and_says_so() {
    let r = Recorded::default().with("query=barcode", fixture("search-empty"));
    let (m, why, urls) = outcome(&disc(&[BARCODE]), Options::default(), r);
    assert!(m.is_none());
    let why = why.unwrap();
    assert!(why.contains("no confident match"), "{why}");
    assert!(why.contains("no release found"), "{why}");
    assert_eq!(urls.len(), 1);
}

#[test]
fn durations_that_dont_fit_are_refused() {
    let r = || releases().with("query=barcode", fixture("search-barcode"));
    // All of them 8 s off.
    let mut d = disc(&[BARCODE]);
    d.areas = vec![recorded_area(8.0)];
    let (m, why, _) = outcome(&d, Options::default(), r());
    assert!(m.is_none());
    assert!(why.unwrap().contains("s off"));
    // One wrong track, which a mean over 12 would hide (2.5 s).
    let mut d = disc(&[BARCODE]);
    d.areas = vec![recorded_area(0.0)];
    d.areas[0].tracks[4].duration_frames += 30 * 75;
    let (m, why, _) = outcome(&d, Options::default(), r());
    assert!(m.is_none());
    assert!(why.unwrap().contains("a track is 30."));
    // One track fewer.
    let mut d = disc(&[BARCODE]);
    d.areas[0].tracks.pop();
    let (m, why, _) = outcome(&d, Options::default(), r());
    assert!(m.is_none());
    // The search said so: the release isn't even read.
    assert!(why.unwrap().contains("no medium of 11 tracks"));
}

fn medium(lengths: &[Option<u64>], isrcs: &[&[&str]]) -> Medium {
    Medium {
        format: "Hybrid SACD".into(),
        tracks: lengths
            .iter()
            .enumerate()
            .map(|(i, l)| lookup::MbTrack {
                length_ms: *l,
                isrcs: isrcs.get(i).map_or(vec![], |x| x.iter().map(|s| (*s).to_owned()).collect()),
                ..Default::default()
            })
            .collect(),
    }
}

fn area(seconds: &[u32], isrcs: &[Option<&str>]) -> AreaFacts {
    AreaFacts {
        slot: Slot::TwoChannel,
        tracks: seconds
            .iter()
            .enumerate()
            .map(|(i, s)| TrackFacts {
                number: i as u8 + 1,
                duration_frames: s * 75,
                isrc: isrcs.get(i).copied().flatten().map(str::to_owned),
                ..TrackFacts::default()
            })
            .collect(),
    }
}

/// The limits come from real discs: the right release up to 15 s off on a track and 5 s on average
/// by number, and 6 s and 3 s by name, while releases with other tracks were over 60 s off.
#[test]
fn the_limits_measured_on_real_discs() {
    let a = area(&[100; 10], &[]);
    let one_off = |secs: i64| {
        let mut lengths = vec![Some(100_000); 10];
        lengths[3] = Some((100 + secs) as u64 * 1000);
        medium(&lengths, &[])
    };
    let all_off = |ms: u64| medium(&[Some(100_000 + ms); 10], &[]);
    let number = Step::Number.limits();
    assert!(confirm(&a, &one_off(14), number).is_ok(), "a pause counted differently");
    assert!(confirm(&a, &one_off(16), number).is_err());
    assert!(confirm(&a, &all_off(4_500), number).is_ok(), "the same on every track");
    assert!(confirm(&a, &all_off(5_500), number).is_err());
    let name = Step::Name.limits();
    assert!(confirm(&a, &one_off(5), name).is_ok());
    assert!(confirm(&a, &one_off(7), name).is_err());
    assert!(confirm(&a, &all_off(2_500), name).is_ok());
    assert!(confirm(&a, &all_off(3_500), name).is_err());
    let durations = Step::Durations.limits();
    assert!(confirm(&a, &one_off(2), durations).is_ok());
    assert!(confirm(&a, &one_off(3), durations).is_err());
}

#[test]
fn every_track_must_fit() {
    let limits = Step::Number.limits();
    let e = confirm(&area(&[100, 200], &[]), &medium(&[Some(100_000)], &[]), limits);
    assert_eq!(e.unwrap_err(), "1 tracks, the area has 2");
    // Pysacd's method would take the one length as the whole disc's.
    let e = confirm(&area(&[100, 200], &[]), &medium(&[Some(100_000), None], &[]), limits);
    assert_eq!(e.unwrap_err(), "track 2 has no length to compare");
    // No lengths at all, whatever the ISRCs.
    let e = confirm(&area(&[100], &[Some("X")]), &medium(&[None], &[&["X"]]), limits);
    assert_eq!(e.unwrap_err(), "no track has a length to compare");
    // A track without a length, vouched for by its ISRC.
    let e = confirm(
        &area(&[100, 200], &[None, Some("AB-C12")]),
        &medium(&[Some(100_000), None], &[&[], &["abc12"]]),
        limits,
    );
    assert_eq!(e.unwrap().isrc_hits, 1);
    // ISRCs that mostly disagree, on durations that fit.
    let e = confirm(
        &area(&[100, 200, 300], &[Some("A"), Some("B"), Some("C")]),
        &medium(&[Some(100_000), Some(200_000), Some(300_000)], &[&["A"], &["X"], &["Y"]]),
        limits,
    );
    assert_eq!(e.unwrap_err(), "ISRCs disagree on 2 tracks, agree on 1");
    // An ISRC the recording doesn't list because it lists none: no disagreement.
    let e = confirm(&area(&[100], &[Some("A")]), &medium(&[Some(100_000)], &[]), limits);
    assert_eq!(e.unwrap().isrc_hits, 0);
    // ISRC hits don't excuse durations.
    let e = confirm(&area(&[100], &[Some("A")]), &medium(&[Some(150_000)], &[&["A"]]), limits);
    assert!(e.unwrap_err().contains("a track is 50.0 s off"));
    // The steps tighten.
    let m = medium(&[Some(101_500); 4], &[]);
    let a = area(&[100; 4], &[]);
    assert!(confirm(&a, &m, Step::Number.limits()).is_ok());
    assert!(confirm(&a, &m, Step::Name.limits()).is_ok());
    assert!(confirm(&a, &m, Step::Durations.limits()).is_err());
}

#[test]
fn candidates_that_disagree_are_ambiguous() {
    // Two SACD releases with the number, on different albums: the box set holds this very disc.
    let mut boxed = fixture("release-box");
    boxed["barcode"] = json!(BARCODE);
    let search = json!({"releases": [{"id": HYBRID}, {"id": BOX}]});
    let r = Recorded::default()
        .with(&format!("release/{HYBRID}"), fixture("release-hybrid"))
        .with(&format!("release/{BOX}"), boxed)
        .with("query=barcode", search);
    let options = Options { by_name: true, by_durations: true, ..Options::default() };
    let (m, why, urls) = outcome(&disc(&[BARCODE]), options, r);
    assert!(m.is_none());
    let why = why.unwrap();
    assert!(why.starts_with("ambiguous by barcode or catalogue number"), "{why}");
    assert!(why.contains(HYBRID) && why.contains(BOX), "{why}");
    // And no weaker way overrules it.
    assert!(urls.iter().all(|u| !u.contains("discid") && !u.contains("artist%3A")), "{urls:?}");
}

#[test]
fn durations_alone_find_a_disc_only_if_nothing_else_fits() {
    // As recorded: the disc alone, and the box set that holds it, under another album title.
    let r = releases().with("discid/-", fixture("discid-fuzzy"));
    let options = Options { by_durations: true, ..Options::default() };
    let (m, why, urls) = outcome(&disc(&[]), options, r);
    assert!(m.is_none());
    assert!(why.unwrap().starts_with("ambiguous by durations"));
    assert!(urls[0].contains("media-format=all"), "{}", urls[0]);
    let toc = disc(&[]).areas[0].cd_toc().unwrap();
    assert!(urls[0].contains(&format!("toc={toc}&")), "{}", urls[0]);
    assert!(toc.starts_with("1+12+"), "{toc}");

    // Without the box set: a match, without the facts of a pressing.
    let mut fuzzy = fixture("discid-fuzzy");
    fuzzy["releases"].as_array_mut().unwrap().retain(|r| r["id"] == HYBRID || r["id"] == TWIN);
    let r = releases().with("discid/-", fuzzy);
    let (m, why, _) = outcome(&disc(&[]), options, r);
    let m = m.unwrap_or_else(|| panic!("{why:?}"));
    assert_eq!((m.step, m.release.as_str()), (Step::Durations, HYBRID));
    assert_eq!(m.album["album"], "Title 17");
    assert_eq!(m.album[lookup::GROUP_ID], GROUP);
    assert!(!m.album.contains_key("date"));
    assert!(!m.album.contains_key(lookup::ALBUM_ID));
    assert!(m.warnings.iter().any(|w| w.contains("pressing")), "{:?}", m.warnings);

    // Tighter than by number: 2 s off on each track.
    let mut d = disc(&[]);
    d.areas = vec![recorded_area(2.0)];
    let (m, why, _) = outcome(&d, options, releases().with("discid/-", fixture("discid-fuzzy")));
    assert!(m.is_none());
    assert!(why.unwrap().contains("limit 1.0 s"));

    // Too few tracks to mean anything: not even asked.
    let mut d = disc(&[]);
    d.areas[0].tracks.truncate(3);
    let (m, why, urls) = outcome(&d, options, Recorded::default());
    assert!(m.is_none());
    assert!(why.unwrap().contains("too few"));
    assert!(urls.is_empty());
}

#[test]
fn a_name_must_be_the_releases_name() {
    let options = Options { by_name: true, ..Options::default() };
    let named = |album: &str, artist: &str| {
        let mut d = disc(&[]);
        d.album = Some(album.to_owned());
        d.artist = Some(artist.to_owned());
        d
    };
    let search = json!({"releases": [{"id": HYBRID}]});
    let credit = lookup::parse_release(&fixture("release-hybrid")).unwrap().credit.unwrap();
    let r = || releases().with("query=release", search.clone());

    let (m, why, urls) = outcome(&named("title 17", &credit), options, r());
    let m = m.unwrap_or_else(|| panic!("{why:?}"));
    assert_eq!(m.step, Step::Name);
    assert!(!m.album.contains_key("date"));
    assert!(urls[0].contains("release%3A%22title%2017%22%20AND%20artist%3A%22"), "{}", urls[0]);
    // Words added at the end of one.
    assert!(outcome(&named("Title 17: Deluxe", &credit), options, r()).0.is_some());

    for (album, artist, want) in
        [("Title 1", credit.as_str(), "its title"), ("Title 17", "Someone Else", "its artist")]
    {
        let (m, why, _) = outcome(&named(album, artist), options, r());
        assert!(m.is_none());
        assert!(why.unwrap().contains(want));
    }

    // No names: not asked. Asked only with both.
    let (m, why, urls) = outcome(&disc(&[]), options, Recorded::default());
    assert!(m.is_none());
    assert!(why.unwrap().contains("no album and artist"));
    assert!(urls.is_empty());
}

#[test]
fn the_path_year_chooses_between_candidates_only() {
    // Two releases that say the same, of different years.
    let mut other = fixture("release-hybrid");
    other["id"] = json!(TWIN);
    other["date"] = json!("2001");
    let r = || {
        Recorded::default()
            .with(&format!("release/{HYBRID}"), fixture("release-hybrid"))
            .with(&format!("release/{TWIN}"), other.clone())
            .with("query=barcode", fixture("search-barcode"))
    };
    let mut d = disc(&[BARCODE]);
    // No year: they agree on the text, and what only one says is left out.
    let (m, _, _) = outcome(&d, Options::default(), r());
    let m = m.unwrap();
    assert!(!m.album.contains_key("date"));
    assert!(!m.album.contains_key(lookup::ALBUM_ID));
    assert_eq!(m.album[lookup::GROUP_ID], GROUP);
    assert_eq!(m.also.len(), 1);
    // The year chooses.
    d.year = Some("2001".into());
    let (m, _, _) = outcome(&d, Options::default(), r());
    let m = m.unwrap();
    assert_eq!(m.release, TWIN);
    assert_eq!(m.album["date"], "2001");
    // A year nothing has doesn't refuse.
    d.year = Some("1999".into());
    assert!(outcome(&d, Options::default(), r()).0.is_some());
}

// ---- Whole runs, on images of the corpus, with responses shaped to them ----

/// The image of the corpus disc `id`.
fn corpus_image(id: &str) -> PathBuf {
    common::corpus().join("images").join(format!("{id}.iso"))
}

/// A recorded release, with the catalogue number of the corpus's discs and the media `formats`,
/// each with the tracks of that area of `image`.
fn release_for(image: &Path, formats: &[(&str, Slot)]) -> Value {
    let d = Disc::open(image, 0, Tuning::default()).unwrap();
    let mut rel = fixture("release-hybrid");
    let template = rel["media"][0]["tracks"][0].clone();
    let media: Vec<Value> = formats
        .iter()
        .map(|(format, slot)| {
            let area = d.toc().area(*slot).unwrap();
            let tracks: Vec<Value> = area
                .tracks
                .iter()
                .map(|t| {
                    let mut x = template.clone();
                    x["title"] = json!(format!("Looked-up {}", t.number));
                    x["length"] = json!(u64::from(t.duration_frames) * 1000 / 75);
                    x["recording"]["id"] = json!(format!("00000000-0000-4000-8000-{:012x}", t.number));
                    x["recording"]["isrcs"] = json!([]);
                    x
                })
                .collect();
            json!({"format": format, "tracks": tracks})
        })
        .collect();
    rel["media"] = json!(media);
    rel["label-info"] = json!([{"catalog-number": "PYSACD-TEST"}]);
    rel
}

fn search_hit() -> Value {
    json!({"releases": [{"id": HYBRID}]})
}

fn answers(image: &Path, formats: &[(&str, Slot)]) -> Recorded {
    Recorded::default()
        .with(&format!("release/{HYBRID}"), release_for(image, formats))
        .with("query=catno", search_hit())
}

struct Run {
    summary: lookup::Summary,
    lines: Vec<String>,
    urls: Vec<String>,
}

fn run(dir: &Path, image: &Path, options: Options, r: Recorded) -> Run {
    let mut c = client(r);
    let mut lines = Vec::new();
    let summary = lookup::fetch(dir, &[image.to_owned()], options, &mut c, |l| lines.push(l));
    Run { summary, lines, urls: c.transport().urls.clone() }
}

fn key_of(image: &Path) -> String {
    tags::disc_key(Disc::open(image, 0, Tuning::default()).unwrap().toc())
}

fn tags_file(dir: &Path, image: &Path) -> PathBuf {
    dir.join(format!("{}.toml", key_of(image)))
}

const BOTH: [(&str, Slot); 2] = [
    ("Hybrid SACD (SACD layer, 2 channels)", Slot::TwoChannel),
    ("Hybrid SACD (SACD layer, multichannel)", Slot::Multichannel),
];

#[test]
fn a_disc_without_text_gets_a_tags_file_once() {
    let d = tempfile::tempdir().unwrap();
    let tagsdir = d.path().join("new/tags");
    let image = corpus_image("no-text");
    let formats = [("Hybrid SACD", Slot::TwoChannel)];
    let r = run(&tagsdir, &image, Options::default(), answers(&image, &formats));
    assert_eq!(r.summary.matched, 1, "{:?}", r.lines);
    assert_eq!(r.summary.written, 1);
    assert!(r.summary.all_handled());
    assert!(r.lines.iter().any(|l| l.contains("matched by barcode or catalogue number")), "{:?}", r.lines);
    assert_eq!(r.urls.iter().filter(|u| u.contains("query=catno")).count(), 2, "both numbers: {:?}", r.urls);

    let text = fs::read_to_string(tags_file(&tagsdir, &image)).unwrap();
    let file = TagsFile::parse(&text).unwrap();
    assert_eq!(file.stereo.album["album"], "Title 17");
    assert_eq!(file.stereo.album[lookup::GROUP_ID], GROUP);
    assert_eq!(file.stereo.album[lookup::ALBUM_ID], HYBRID);
    assert_eq!(file.stereo.track["1"]["title"], "Looked-up 1");

    // Once per disc: asked again, nothing goes out.
    let again = run(&tagsdir, &image, Options::default(), Recorded::default());
    assert!(again.urls.is_empty());
    assert_eq!(again.summary.done, 1);
    assert!(again.summary.all_handled());
}

#[test]
fn one_area_looked_up_is_the_disc_looked_up() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("dsd-stereo-mch-2048");
    let path = tags_file(d.path(), &image);
    let held = format!("[stereo.album]\n\"{}\" = \"{GROUP}\"\n", lookup::GROUP_ID);
    fs::write(&path, &held).unwrap();
    let options = Options { by_durations: true, ..Options::default() };
    let r = run(d.path(), &image, options, Recorded::default());
    assert!(r.urls.is_empty(), "the other area gets no second try: {:?}", r.urls);
    assert_eq!(r.summary.done, 1);
    assert_eq!(fs::read_to_string(&path).unwrap(), held);
}

#[test]
fn hand_edits_win_at_every_level() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    let path = tags_file(d.path(), &image);
    let formats = [("Hybrid SACD", Slot::TwoChannel)];
    fs::write(
        &path,
        "[album]\nTALB = \"Mine\"\ngenre = \"Jazz\"\n\n[stereo.album]\nalbum_artist = \"\"\n\n\
         [track.1]\nTIT2 = \"My One\"\n\n[stereo.track.2]\ntitle = \"My Two\"\n\n[track.3]\ntitle = \"\"\n",
    )
    .unwrap();
    let r = run(d.path(), &image, Options::default(), answers(&image, &formats));
    assert_eq!(r.summary.written, 1, "{:?}", r.lines);
    let file = TagsFile::parse(&fs::read_to_string(&path).unwrap()).unwrap();
    let f = |n| file.fields_for(Slot::TwoChannel, n);
    assert_eq!(f(1)["album"], "Mine", "another spelling at another level");
    assert_eq!(f(1)["genre"], "Jazz", "not touched");
    assert_eq!(f(1)["album_artist"], "", "an empty value is an edit");
    assert_eq!(f(1)["title"], "My One");
    assert_eq!(f(2)["title"], "My Two");
    assert_eq!(f(3)["title"], "", "an empty value is an edit");
    assert_eq!(f(1)[lookup::GROUP_ID], GROUP, "what they lack is added");
}

#[test]
fn a_generated_title_stored_by_an_older_import_gives_way() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    let path = tags_file(d.path(), &image);
    let formats = [("Hybrid SACD", Slot::TwoChannel)];
    // `Track 2` on track 1 and `Track 2` unpadded on track 2 are no text sacdfs makes for them.
    fs::write(&path, "[track.1]\ntitle = \"Track 01\"\n\n[stereo.track.2]\nTIT2 = \"Track 2\"\n").unwrap();
    let r = run(d.path(), &image, Options::default(), answers(&image, &formats));
    assert_eq!(r.summary.written, 1, "{:?}", r.lines);
    let file = TagsFile::parse(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(file.fields_for(Slot::TwoChannel, 1)["title"], "Looked-up 1");
    assert_eq!(file.fields_for(Slot::TwoChannel, 2)["title"], "Track 2");
}

#[test]
fn the_discs_own_text_stays() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("dsd-stereo-mch-2048");
    let r = run(d.path(), &image, Options::default(), answers(&image, &BOTH));
    assert_eq!(r.summary.matched, 1, "{:?}", r.lines);
    let file = TagsFile::parse(&fs::read_to_string(tags_file(d.path(), &image)).unwrap()).unwrap();
    // Both areas say these alike: the disc's level.
    assert_eq!(file.album[lookup::GROUP_ID], GROUP);
    for level in [&file.album, &file.stereo.album, &file.multichannel.album] {
        assert!(!level.contains_key("album"), "the disc has an album: {level:?}");
        assert!(!level.contains_key("date"), "the disc has a date: {level:?}");
    }
    let tracks =
        file.track.values().chain(file.stereo.track.values()).chain(file.multichannel.track.values());
    for t in tracks {
        assert!(!t.contains_key("title"), "the disc has titles: {t:?}");
    }
}

/// A disc of two areas, the recorded hybrid's tracks in the stereo one.
fn two_areas() -> DiscFacts {
    let mut d = disc(&[BARCODE]);
    let mut mc = recorded_area(0.2);
    mc.slot = Slot::Multichannel;
    d.areas.push(mc);
    d
}

fn hits() -> Recorded {
    releases().with("query=barcode", fixture("search-barcode"))
}

/// A track list the library won't lay out says nothing about the disc's text: found on a live run,
/// where such a disc's album and titles were replaced.
#[test]
fn the_discs_own_text_stays_on_tracks_the_library_refuses() {
    let d = tempfile::tempdir().unwrap();
    let mut bytes = fs::read(corpus_image("dsd-stereo-mch-2048")).unwrap();
    let find = |b: &[u8], tag: &[u8]| -> Vec<usize> {
        b.windows(tag.len()).enumerate().filter(|(_, w)| *w == tag).map(|(i, _)| i).collect()
    };
    // Three-minute tracks, on audio that lasts well under a second.
    let frames = 180 * 75u32;
    let code = |f: u32| [(f / 75 / 60) as u8, (f / 75 % 60) as u8, (f % 75) as u8];
    for at in [find(&bytes, b"TWOCHTOC"), find(&bytes, b"MULCHTOC")].concat() {
        bytes[at + 64..at + 67].copy_from_slice(&code(frames * 6));
    }
    for at in find(&bytes, b"SACDTRL2") {
        for i in 0..3 {
            bytes[at + 8 + 4 * i..at + 11 + 4 * i].copy_from_slice(&code(frames * i as u32));
            bytes[at + 1028 + 4 * i..at + 1031 + 4 * i].copy_from_slice(&code(frames));
        }
    }
    let image = d.path().join("disc.iso");
    fs::write(&image, bytes).unwrap();
    let disc = Disc::open(&image, 0, Tuning::default()).unwrap();
    let toc = disc.toc();
    let refused = toc.areas.iter().any(|a| a.tracks.iter().any(|t| sacdfs::dsf::layout(toc, a, t).is_none()));
    assert!(refused, "the case this test is for");
    let facts = DiscFacts::new(toc, &image);
    assert_eq!(facts.album.as_deref(), Some("Plain DSD stereo and 5.1"));
    for t in facts.areas.iter().flat_map(|a| &a.tracks) {
        assert!(t.on_disc.contains("album"), "track {}: {:?}", t.number, t.on_disc);
    }
}

#[test]
fn a_multichannel_album_keeps_its_suffix_unless_asked() {
    let d = two_areas();
    let areas: Vec<&AreaFacts> = d.areas.iter().collect();
    let mut c = client(hits());
    let (found, why) = lookup::look_up(&d, &areas, Options::default(), &mut c).unwrap();
    assert!(found.iter().all(Option::is_some), "{why:?}");
    assert!(
        c.transport().urls.iter().filter(|u| u.contains("query=")).count() == 1,
        "one search for both areas"
    );
    assert_eq!(c.requests(), 3, "each release once: {:?}", c.transport().urls);

    let file = lookup::plan(&mut TagsFile::default(), &areas, &found, Options::default());
    assert_eq!(file.stereo.album["album"], "Title 17");
    assert_eq!(file.multichannel.album["album"], "Title 17 (Multichannel)");
    assert!(!file.album.contains_key("album"));
    // What both areas say alike goes at the disc's level.
    assert_eq!(file.album[lookup::GROUP_ID], GROUP);
    assert_eq!(file.track["1"]["title"], "Title 5");
    assert!(file.stereo.track.is_empty() && file.multichannel.track.is_empty(), "{file:?}");

    let options = Options { no_suffix: true, ..Options::default() };
    let file = lookup::plan(&mut TagsFile::default(), &areas, &found, options);
    assert_eq!(file.album["album"], "Title 17");
    assert!(!file.multichannel.album.contains_key("album"));
}

#[test]
fn an_area_that_doesnt_fit_is_left_alone_and_said() {
    let mut d = two_areas();
    d.areas[1].tracks.pop();
    let areas: Vec<&AreaFacts> = d.areas.iter().collect();
    let options = Options { by_name: true, by_durations: true, ..Options::default() };
    let mut c = client(hits());
    let (found, why) = lookup::look_up(&d, &areas, options, &mut c).unwrap();
    assert!(found[0].is_some() && why[0].is_none(), "{why:?}");
    assert!(found[1].is_none());
    let w = why[1].as_deref().unwrap();
    assert!(w.contains("doesn't fit") && w.contains("no medium of 11 tracks"), "{w}");
    // The stereo match ends the steps: the other area gets no other release by a weaker way.
    assert!(c.transport().urls.iter().all(|u| !u.contains("discid")), "{:?}", c.transport().urls);

    let file = lookup::plan(&mut TagsFile::default(), &areas, &found, options);
    assert_eq!(file.stereo.track["1"]["title"], "Title 5");
    assert!(
        file.multichannel.album.is_empty() && file.multichannel.track.is_empty() && file.track.is_empty()
    );
}

#[test]
fn a_disc_is_one_album_or_nothing() {
    // Each area fits a different release: stereo the hybrid; multichannel the box's first medium.
    let mut boxed = fixture("release-box");
    boxed["barcode"] = json!(BARCODE);
    boxed["media"].as_array_mut().unwrap().truncate(1);
    let first: Vec<u64> = boxed["media"][0]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["length"].as_u64().unwrap())
        .collect();
    let mut d = disc(&[BARCODE]);
    d.areas.push(AreaFacts {
        slot: Slot::Multichannel,
        tracks: first
            .iter()
            .enumerate()
            .map(|(i, ms)| TrackFacts {
                number: i as u8 + 1,
                duration_frames: (ms * 75 / 1000) as u32,
                ..TrackFacts::default()
            })
            .collect(),
    });
    let r = Recorded::default()
        .with(&format!("release/{HYBRID}"), fixture("release-hybrid"))
        .with(&format!("release/{BOX}"), boxed)
        .with("query=barcode", json!({"releases": [{"id": HYBRID}, {"id": BOX}]}));
    let areas: Vec<&AreaFacts> = d.areas.iter().collect();
    let mut c = client(r);
    let (found, why) = lookup::look_up(&d, &areas, Options::default(), &mut c).unwrap();
    assert!(found.iter().all(Option::is_none), "{why:?}");
    assert!(why[0].as_deref().unwrap().contains("different albums"), "{why:?}");

    // One area ambiguous, and the other fine: nothing either.
    let mut boxed = fixture("release-box");
    boxed["barcode"] = json!(BARCODE);
    let r = Recorded::default()
        .with(&format!("release/{HYBRID}"), fixture("release-hybrid"))
        .with(&format!("release/{BOX}"), boxed)
        .with("query=barcode", json!({"releases": [{"id": HYBRID}, {"id": BOX}]}));
    let mut c = client(r);
    let (found, why) = lookup::look_up(&d, &areas, Options::default(), &mut c).unwrap();
    assert!(found.iter().all(Option::is_none), "{why:?}");
    assert!(why.iter().all(|w| w.as_deref().unwrap().starts_with("ambiguous")), "{why:?}");
}

#[test]
fn a_dry_run_writes_nothing_and_shows_what_would_be() {
    let d = tempfile::tempdir().unwrap();
    let tagsdir = d.path().join("tags");
    let image = corpus_image("no-text");
    let formats = [("Hybrid SACD", Slot::TwoChannel)];
    let options = Options { dry_run: true, ..Options::default() };
    let r = run(&tagsdir, &image, options, answers(&image, &formats));
    assert_eq!(r.summary.matched, 1, "{:?}", r.lines);
    assert_eq!(r.summary.written, 0);
    assert!(!tagsdir.exists(), "not even the directory");
    assert!(r.lines.iter().any(|l| l.contains("dry run") && l.contains("Looked-up 1")), "{:?}", r.lines);
}

#[test]
fn a_refusal_writes_nothing_and_an_unusable_tags_file_stops_it_before_asking() {
    let d = tempfile::tempdir().unwrap();
    let tagsdir = d.path().join("tags");
    let image = corpus_image("no-text");
    let empty = Recorded::default().with("query=catno", fixture("search-empty"));
    let r = run(&tagsdir, &image, Options::default(), empty);
    assert_eq!(r.summary.refused, 1, "{:?}", r.lines);
    assert!(!r.summary.all_handled());
    assert!(!tagsdir.exists());
    assert!(
        r.lines.iter().any(|l| l.contains("nothing written") && l.contains("no release found")),
        "{:?}",
        r.lines
    );

    fs::create_dir_all(&tagsdir).unwrap();
    let path = tags_file(&tagsdir, &image);
    fs::write(&path, "[albm]\nnot = 'a tags file'\n").unwrap();
    let r = run(&tagsdir, &image, Options::default(), Recorded::default());
    assert_eq!(r.summary.failed, 1);
    assert!(r.urls.is_empty(), "{:?}", r.urls);
    assert_eq!(fs::read_to_string(&path).unwrap(), "[albm]\nnot = 'a tags file'\n", "left as it is");
}

/// Answers with `bad` for the first requests, then as `inner` does.
#[derive(Debug)]
struct Busy {
    bad: Vec<Reply>,
    inner: Recorded,
    asked: usize,
}

impl Transport for Busy {
    fn get(&mut self, url: &str) -> Result<Reply, String> {
        self.asked += 1;
        if self.bad.is_empty() { self.inner.get(url) } else { Ok(self.bad.remove(0)) }
    }
}

fn busy() -> Reply {
    Reply { status: 503, body: b"{\"error\": \"busy\"}".to_vec(), retry_after: Some(0) }
}

/// A 503 now and then is what a large library meets: the run carries on and loses no disc.
#[test]
fn a_busy_service_costs_a_retry_not_the_batch() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    let formats = [("Hybrid SACD", Slot::TwoChannel)];
    // Two tries of a request fail: the disc still gets its answer.
    let t = Busy { bad: vec![busy(), busy()], inner: answers(&image, &formats), asked: 0 };
    let mut c = Client::paced(t, Duration::ZERO, Duration::ZERO);
    let mut lines = Vec::new();
    let s =
        lookup::fetch(d.path(), std::slice::from_ref(&image), Options::default(), &mut c, |l| lines.push(l));
    assert_eq!((s.matched, s.failed), (1, 0), "{lines:?}");
    assert!(c.transport().asked > 2);
}

/// A disc whose every try failed is asked about again at the end, and the images after it go on.
#[test]
fn a_disc_without_an_answer_gets_a_second_try_at_the_end() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    let formats = [("Hybrid SACD", Slot::TwoChannel)];
    let t = Busy { bad: (0..5).map(|_| busy()).collect(), inner: answers(&image, &formats), asked: 0 };
    let mut c = Client::paced(t, Duration::ZERO, Duration::ZERO);
    let mut lines = Vec::new();
    let images = vec![image.clone(); 2];
    let s = lookup::fetch(d.path(), &images, Options::default(), &mut c, |l| lines.push(l));
    assert_eq!((s.matched, s.done, s.failed, s.not_tried), (1, 1, 0, 0), "{lines:?}");
    assert!(s.all_handled(), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("trying them once more")), "{lines:?}");

    // Still nothing on the second try: it counts as failed, once.
    let d = tempfile::tempdir().unwrap();
    let t = Busy { bad: (0..10).map(|_| busy()).collect(), inner: Recorded::default(), asked: 0 };
    let mut c = Client::paced(t, Duration::ZERO, Duration::ZERO);
    let s = lookup::fetch(d.path(), &[image], Options::default(), &mut c, |_| {});
    assert_eq!((s.failed, s.not_tried, s.requests), (1, 0, 10));
}

/// Told to stay away, a run asks nothing more: not the next try, not the next disc.
#[test]
fn being_told_to_stay_away_ends_the_run_at_once() {
    let image = corpus_image("no-text");
    for bad in [
        Reply { status: 429, body: b"{}".to_vec(), retry_after: Some(7200) },
        Reply { status: 503, body: b"{}".to_vec(), retry_after: Some(7200) },
        Reply { status: 403, body: b"{}".to_vec(), retry_after: None },
    ] {
        let d = tempfile::tempdir().unwrap();
        let t = Busy { bad: vec![bad], inner: Recorded::default(), asked: 0 };
        let mut c = Client::paced(t, Duration::ZERO, Duration::ZERO);
        let mut lines = Vec::new();
        let s =
            lookup::fetch(d.path(), &vec![image.clone(); 4], Options::default(), &mut c, |l| lines.push(l));
        assert_eq!((s.failed, s.not_tried), (1, 3), "{lines:?}");
        assert_eq!(c.transport().asked, 1, "{lines:?}");
    }
}

#[test]
fn failures_in_a_row_end_the_run() {
    #[derive(Debug, Default)]
    struct Down(usize);
    impl Transport for Down {
        fn get(&mut self, _: &str) -> Result<Reply, String> {
            self.0 += 1;
            Err("connection refused".to_owned())
        }
    }
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    let images = vec![image; 7];
    let mut c = Client::paced(Down::default(), Duration::ZERO, Duration::ZERO);
    let mut lines = Vec::new();
    let s = lookup::fetch(d.path(), &images, Options::default(), &mut c, |l| lines.push(l));
    assert_eq!((s.failed, s.not_tried), (5, 2), "{lines:?}");
    assert!(!s.all_handled());
    assert!(s.render().contains("not tried"), "{}", s.render());
    assert_eq!(c.transport().0, 25, "5 images, 5 tries of the first request of each");
    assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0);
    assert!(lines[0].contains("connection refused"), "{lines:?}");
}

/// The binary: nothing is sent for a disc it can't write to, and its options are these.
#[test]
fn the_command() {
    let bin = env!("CARGO_BIN_EXE_sacdfs");
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    fs::write(tags_file(d.path(), &image), "not toml [").unwrap();
    let out = Command::new(bin)
        .args(["tags", "fetch", "--lookup-name", "--lookup-durations", "--tags"])
        .arg(d.path())
        .arg(&image)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("nothing written"), "{stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("1 failed") && stderr.contains("0 requests"), "{stderr}");

    let help = Command::new(bin).args(["tags", "fetch", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&help.stdout);
    for flag in
        ["--tags", "--lookup-name", "--lookup-durations", "--dry-run", "--no-multichannel-album-suffix"]
    {
        assert!(help.contains(flag), "{flag}: {help}");
    }
    for flag in ["--acoustid", "--lookup-acoustid"] {
        assert!(!help.contains(flag), "{flag}: {help}");
    }
    let none = Command::new(bin).args(["tags", "fetch", "--tags", "x"]).output().unwrap();
    assert_eq!(none.status.code(), Some(2), "an image is required");
}
