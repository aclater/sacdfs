// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs tags fetch --acoustid` (#296), on images of the corpus, with a stand-in for `fpcalc`
//! and answers from tables for AcoustID and MusicBrainz: what is sent, what is matched or refused,
//! the budget, the state kept between runs, and a stop. Nothing here touches the network. One test
//! runs the real `fpcalc` if there is one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sacdfs::disc::{Disc, Tuning};
use sacdfs::lookup::acoustid::{self, Config, Session};
use sacdfs::lookup::fingerprint::{self, MAX_SECONDS, RATE};
use sacdfs::lookup::{self, Client, Options, Reply, Transport};
use sacdfs::tags::{self, TagsFile};
use scarletbook::Slot;
use serde_json::{Value, json};

const HYBRID: &str = "0000001d-0000-4000-8000-00000000001d";
const OTHER: &str = "0000001f-0000-4000-8000-00000000001f";
const KEY: &str = "TestKey123";

fn corpus_image(id: &str) -> PathBuf {
    common::corpus().join("images").join(format!("{id}.iso"))
}

fn fixture(name: &str) -> Value {
    let p =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/musicbrainz").join(format!("{name}.json"));
    serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
}

fn recording(n: u8) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

/// A release whose media have the tracks of these areas of `image`, recording `n` at track `n`.
fn release_for(image: &Path, formats: &[(&str, Slot)]) -> Value {
    let d = Disc::open(image, 0, Tuning::default()).unwrap();
    let mut rel = fixture("release-hybrid");
    let template = rel["media"][0]["tracks"][0].clone();
    let media: Vec<Value> = formats
        .iter()
        .map(|(format, slot)| {
            let tracks: Vec<Value> = d
                .toc()
                .area(*slot)
                .unwrap()
                .tracks
                .iter()
                .map(|t| {
                    let mut x = template.clone();
                    x["title"] = json!(format!("Heard {}", t.number));
                    x["length"] = json!(u64::from(t.duration_frames) * 1000 / 75);
                    x["recording"]["id"] = json!(recording(t.number));
                    x["recording"]["isrcs"] = json!([]);
                    x
                })
                .collect();
            json!({"format": format, "tracks": tracks})
        })
        .collect();
    rel["media"] = json!(media);
    rel
}

/// MusicBrainz: the release, and nothing for any search.
#[derive(Debug, Default)]
struct Mb {
    release: Option<Value>,
    urls: Vec<String>,
}

impl Transport for Mb {
    fn get(&mut self, url: &str) -> Result<Reply, String> {
        self.urls.push(url.to_owned());
        match (&self.release, url.contains(&format!("release/{HYBRID}"))) {
            (Some(v), true) => Ok(Reply { status: 200, body: v.to_string().into_bytes(), retry_after: None }),
            _ => Ok(Reply { status: 404, body: b"{}".to_vec(), retry_after: None }),
        }
    }
}

/// AcoustID: the answer to request `n` is `answers[n]` (the last one after that).
#[derive(Debug, Clone, Default)]
struct Aid {
    answers: Vec<(u16, Value)>,
    urls: Arc<Mutex<Vec<String>>>,
}

impl Transport for Aid {
    fn get(&mut self, url: &str) -> Result<Reply, String> {
        let mut urls = self.urls.lock().unwrap();
        urls.push(url.to_owned());
        let (status, v) = self.answers.get(urls.len() - 1).or(self.answers.last()).cloned().unwrap();
        Ok(Reply { status, body: v.to_string().into_bytes(), retry_after: None })
    }
}

/// An answer naming the recording of track `n`, on these releases.
fn heard(n: u8, releases: &[&str]) -> (u16, Value) {
    let rels: Vec<Value> = releases.iter().map(|r| json!({"id": r})).collect();
    (
        200,
        json!({"status": "ok", "results": [
            {"id": "11111111-0000-4000-8000-000000000000", "score": 0.93,
             "recordings": [{"id": recording(n), "releases": rels}]}
        ]}),
    )
}

/// A stand-in for `fpcalc` in `dir`: it keeps its arguments and the bytes it was given, and gives
/// a fingerprint of its own each time. Written with no child being started: an executable open
/// for writing in a forked child can't be run (ETXTBSY).
fn fake_fpcalc(dir: &Path) -> PathBuf {
    let _no_forks = common::fuse_exclusive();
    let p = dir.join("fpcalc");
    fs::write(
        &p,
        "#!/bin/sh\n\
         [ \"$1\" = -version ] && { echo fake; exit 0; }\n\
         d=$(dirname \"$0\")\n\
         echo \"$*\" >> \"$d/args\"\n\
         n=$(wc -c | tr -d ' ')\n\
         echo \"$n\" >> \"$d/bytes\"\n\
         c=$(wc -l < \"$d/bytes\" | tr -d ' ')\n\
         printf '{\"duration\": 1.0, \"fingerprint\": \"AQAA%s%s\"}\\n' \"$c\" \"$n\"\n",
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn session(tags: &Path, fpcalc: PathBuf, aid: Aid, budget: usize) -> (Session, Arc<AtomicBool>) {
    let stop = Arc::new(AtomicBool::new(false));
    let mut c = Config::new(KEY.to_owned(), fpcalc);
    c.budget = budget;
    c.max_load = None;
    c.interval = Duration::ZERO;
    c.backoff = Duration::ZERO;
    // The corpus's tracks are a fraction of a second.
    c.min_seconds = 0;
    (Session::new(c, Box::new(aid), tags, stop.clone()), stop)
}

struct Run {
    summary: lookup::Summary,
    lines: Vec<String>,
    mb: Vec<String>,
}

fn run(tags: &Path, image: &Path, mb: Mb, s: &mut Session) -> Run {
    let mut client = Client::paced(mb, Duration::ZERO, Duration::ZERO);
    let mut lines = Vec::new();
    let summary =
        lookup::fetch_with(tags, &[image.to_owned()], Options::default(), &mut client, Some(s), |l| {
            lines.push(l)
        });
    Run { summary, lines, mb: client.transport().urls.clone() }
}

fn tags_file(dir: &Path, image: &Path) -> PathBuf {
    dir.join(format!("{}.toml", tags::disc_key(Disc::open(image, 0, Tuning::default()).unwrap().toc())))
}

/// The bytes `fpcalc` was given, per track.
fn bytes_given(dir: &Path) -> Vec<u64> {
    fs::read_to_string(dir.join("bytes")).unwrap_or_default().lines().map(|l| l.parse().unwrap()).collect()
}

#[test]
fn a_disc_nothing_else_finds_is_found_by_its_audio() {
    let d = tempfile::tempdir().unwrap();
    let tagsdir = d.path().join("tags");
    let image = corpus_image("no-text");
    let fpcalc = fake_fpcalc(d.path());
    common::spawns_children!();
    let aid = Aid { answers: vec![heard(1, &[HYBRID, OTHER]), heard(2, &[HYBRID])], ..Aid::default() };
    let urls = aid.urls.clone();
    let (mut s, _) = session(&tagsdir, fpcalc, aid, 20);
    let mb = Mb { release: Some(release_for(&image, &[("Hybrid SACD", Slot::TwoChannel)])), ..Mb::default() };
    let r = run(&tagsdir, &image, mb, &mut s);
    assert_eq!(r.summary.matched, 1, "{:?}", r.lines);
    assert!(r.lines.iter().any(|l| l.contains("matched by audio fingerprint")), "{:?}", r.lines);
    assert!(r.summary.all_handled());

    // What was sent: the key, a fingerprint and the duration per track, and nothing else.
    let urls = urls.lock().unwrap().clone();
    assert_eq!(urls.len(), 2);
    for u in &urls {
        assert!(u.starts_with(acoustid::API), "{u}");
        let keys: Vec<&str> =
            u.split_once('?').unwrap().1.split('&').map(|p| p.split_once('=').unwrap().0).collect();
        assert_eq!(keys, ["client", "meta", "duration", "fingerprint", "format"], "{u}");
        assert!(u.contains(&format!("client={KEY}")));
    }
    // The release holding both recordings is read; the one holding one of them is not.
    assert!(r.mb.iter().any(|u| u.contains(HYBRID)), "{:?}", r.mb);
    assert!(!r.mb.iter().any(|u| u.contains(OTHER)), "{:?}", r.mb);

    // fpcalc was told what it gets: 11,025 Hz mono 16-bit PCM, two minutes at most.
    let args = fs::read_to_string(d.path().join("args")).unwrap();
    assert!(args.contains("-format s16le -rate 11025 -channels 1 -length 120"), "{args}");
    // 147 samples of 2 bytes per SACD frame: the tracks are 1 and 2 frames long.
    assert_eq!(bytes_given(d.path()), [294, 588]);

    let file = TagsFile::parse(&fs::read_to_string(tags_file(&tagsdir, &image)).unwrap()).unwrap();
    assert_eq!(file.stereo.track["1"]["title"], "Heard 1");
    assert!(file.stereo.album.contains_key(lookup::GROUP_ID));
    assert!(!file.stereo.album.contains_key(lookup::ALBUM_ID), "the pressing isn't known");
    assert!(!file.stereo.album.contains_key("date"));
    assert_eq!(s.counters.done, 1);
    assert_eq!(s.counters.tracks, 2);

    // Once per disc.
    let (mut again, _) = session(&tagsdir, d.path().join("none"), Aid::default(), 20);
    let r = run(&tagsdir, &image, Mb::default(), &mut again);
    assert_eq!(r.summary.done, 1);
    assert_eq!(again.counters.requests, 0);
}

#[test]
fn recordings_out_of_order_are_no_match() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    let fpcalc = fake_fpcalc(d.path());
    common::spawns_children!();
    // Track 1 sounds like the release's track 2, and track 2 like its track 1.
    let aid = Aid { answers: vec![heard(2, &[HYBRID]), heard(1, &[HYBRID])], ..Aid::default() };
    let (mut s, _) = session(d.path(), fpcalc.clone(), aid, 20);
    let mb = Mb { release: Some(release_for(&image, &[("Hybrid SACD", Slot::TwoChannel)])), ..Mb::default() };
    let r = run(d.path(), &image, mb, &mut s);
    assert_eq!(r.summary.refused, 1, "{:?}", r.lines);
    assert!(r.lines.iter().any(|l| l.contains("fingerprinted recordings in order")), "{:?}", r.lines);
    assert!(!tags_file(d.path(), &image).exists());

    // A refusal is kept: the next run doesn't fingerprint the disc again.
    let (mut again, _) = session(d.path(), fpcalc, Aid::default(), 20);
    let r = run(d.path(), &image, Mb::default(), &mut again);
    assert!(r.lines.iter().any(|l| l.contains("without a match")), "{:?}", r.lines);
    assert_eq!((again.counters.skipped, again.counters.requests, again.counters.done), (1, 0, 0));
}

#[test]
fn a_disc_unknown_to_acoustid_reads_no_release() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    let fpcalc = fake_fpcalc(d.path());
    common::spawns_children!();
    let aid = Aid { answers: vec![(200, json!({"status": "ok", "results": []}))], ..Aid::default() };
    let (mut s, _) = session(d.path(), fpcalc, aid, 20);
    let r = run(d.path(), &image, Mb::default(), &mut s);
    assert_eq!(r.summary.refused, 1);
    assert!(r.lines.iter().any(|l| l.contains("0 known to AcoustID")), "{:?}", r.lines);
    assert!(!r.mb.iter().any(|u| u.contains("/release/")), "{:?}", r.mb);
}

#[test]
fn tracks_too_short_to_fingerprint_are_left_out() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    let fpcalc = fake_fpcalc(d.path());
    common::spawns_children!();
    let aid = Aid::default();
    let urls = aid.urls.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let mut c = Config::new(KEY.to_owned(), fpcalc);
    c.max_load = None;
    let mut s = Session::new(c, Box::new(aid), d.path(), stop);
    let r = run(d.path(), &image, Mb::default(), &mut s);
    assert_eq!(r.summary.refused, 1, "{:?}", r.lines);
    assert!(r.lines.iter().any(|l| l.contains("2 under 10 s left out")), "{:?}", r.lines);
    assert!(urls.lock().unwrap().is_empty());
    assert!(bytes_given(d.path()).is_empty(), "fpcalc wasn't run");
}

#[test]
fn the_daily_budget_holds() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    let fpcalc = fake_fpcalc(d.path());
    common::spawns_children!();
    let aid = Aid::default();
    let urls = aid.urls.clone();
    let (mut s, _) = session(d.path(), fpcalc, aid, 0);
    let r = run(d.path(), &image, Mb::default(), &mut s);
    assert!(r.lines.iter().any(|l| l.contains("budget of 0 discs is spent")), "{:?}", r.lines);
    assert!(urls.lock().unwrap().is_empty());
    assert!(bytes_given(d.path()).is_empty(), "fpcalc wasn't run");
    assert_eq!((s.counters.queued, s.counters.deferred), (1, 1));
    assert!(!r.summary.all_handled());
}

#[test]
fn a_refused_key_ends_the_run_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    let refused = (400, json!({"status": "error", "error": {"code": 4, "message": "invalid API key"}}));
    let fpcalc = fake_fpcalc(d.path());
    common::spawns_children!();
    let aid = Aid { answers: vec![refused], ..Aid::default() };
    let (mut s, _) = session(d.path(), fpcalc, aid, 20);
    let mut client = Client::paced(Mb::default(), Duration::ZERO, Duration::ZERO);
    let mut lines = Vec::new();
    let images = [image.clone(), corpus_image("full-text")];
    let sum = lookup::fetch_with(d.path(), &images, Options::default(), &mut client, Some(&mut s), |l| {
        lines.push(l)
    });
    assert!(lines.iter().any(|l| l.contains("application key")), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains(KEY)), "the key is never said: {lines:?}");
    assert_eq!((sum.failed, sum.not_tried), (1, 1), "{lines:?}");
    assert!(!tags_file(d.path(), &image).exists());
    // The disc gets its budget back, and no verdict.
    assert_eq!(s.counters.done, 0);
    let state = fs::read_to_string(d.path().join(acoustid::STATE_FILE)).unwrap();
    assert_eq!(state.lines().count(), 2, "{state}");
}

#[test]
fn a_stop_leaves_the_rest_for_the_next_run() {
    let d = tempfile::tempdir().unwrap();
    let (mut s, stop) = session(d.path(), d.path().join("none"), Aid::default(), 20);
    stop.store(true, Ordering::Relaxed);
    let mut client = Client::paced(Mb::default(), Duration::ZERO, Duration::ZERO);
    let images = [corpus_image("no-text"), corpus_image("full-text")];
    let sum = lookup::fetch_with(d.path(), &images, Options::default(), &mut client, Some(&mut s), |_| {});
    assert_eq!(sum.not_tried, 2);
    assert_eq!(client.requests(), 0);
}

#[test]
fn the_multichannel_area_takes_the_release_its_stereo_audio_names() {
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("dsd-stereo-mch-2048");
    let fpcalc = fake_fpcalc(d.path());
    common::spawns_children!();
    let aid = Aid {
        answers: vec![heard(1, &[HYBRID]), heard(2, &[HYBRID]), heard(3, &[HYBRID])],
        ..Aid::default()
    };
    let (mut s, _) = session(d.path(), fpcalc, aid, 20);
    let formats = [
        ("Hybrid SACD (SACD layer, 2 channels)", Slot::TwoChannel),
        ("Hybrid SACD (SACD layer, multichannel)", Slot::Multichannel),
    ];
    let mb = Mb { release: Some(release_for(&image, &formats)), ..Mb::default() };
    let r = run(d.path(), &image, mb, &mut s);
    assert_eq!(r.summary.matched, 1, "{:?}", r.lines);
    // Only the stereo area is fingerprinted.
    assert_eq!(s.counters.tracks, image_tracks(&image, Slot::TwoChannel));
    for area in ["stereo", "multichannel"] {
        let said = format!("{area}: matched by audio fingerprint: release {HYBRID}");
        assert!(r.lines.iter().any(|l| l.contains(&said)), "{area}: {:?}", r.lines);
    }
    // The disc's own text stays; the recordings are added.
    let file = TagsFile::parse(&fs::read_to_string(tags_file(d.path(), &image)).unwrap()).unwrap();
    assert!(file.track["1"].contains_key(lookup::RECORDING_ID), "{file:?}");
}

fn image_tracks(image: &Path, slot: Slot) -> usize {
    Disc::open(image, 0, Tuning::default()).unwrap().toc().area(slot).unwrap().tracks.len()
}

/// Two minutes at most, at 11,025 Hz: a longer track is cut there, whatever its length.
#[test]
fn at_most_two_minutes_of_a_track_are_streamed() {
    let disc = Disc::open(&corpus_image("dst-features-2ch-6ch"), 0, Tuning::default()).unwrap();
    let area = disc.toc().areas.iter().position(|a| a.slot == Slot::TwoChannel).unwrap();
    let mut out = Vec::new();
    let fed = fingerprint::stream_track(&disc, area, 0, u32::MAX, &mut out, &|| false).unwrap();
    assert_eq!(fed.frames, MAX_SECONDS * 75);
    assert_eq!(out.len() as u64, u64::from(MAX_SECONDS) * u64::from(RATE) * 2);
    // A stop between seconds ends it.
    let mut out = Vec::new();
    let e = fingerprint::stream_track(&disc, area, 0, u32::MAX, &mut out, &|| true).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(out.len(), 147 * 2 * 75, "one second");
}

#[test]
fn requests_keep_under_three_a_second() {
    let d = tempfile::tempdir().unwrap();
    assert!(acoustid::MIN_INTERVAL >= Duration::from_millis(334));
    // Paced as the real thing, three requests take two intervals at least.
    let aid = Aid { answers: vec![(200, json!({"status": "ok", "results": []}))], ..Aid::default() };
    let urls = aid.urls.clone();
    let fpcalc = fake_fpcalc(d.path());
    common::spawns_children!();
    let stop = Arc::new(AtomicBool::new(false));
    let mut c = Config::new(KEY.to_owned(), fpcalc);
    c.max_load = None;
    c.min_seconds = 0;
    let mut s = Session::new(c, Box::new(aid), d.path(), stop);
    let disc = Disc::open(&corpus_image("dst-features-2ch-6ch"), 0, Tuning::default()).unwrap();
    let area = disc.toc().areas.iter().position(|a| a.slot == Slot::TwoChannel).unwrap();
    let t = Instant::now();
    s.identify(&disc, area, &mut |_| {}).unwrap();
    assert_eq!(urls.lock().unwrap().len(), 3);
    assert!(t.elapsed() >= acoustid::MIN_INTERVAL * 2, "{:?}", t.elapsed());
}

/// The real `fpcalc` takes what sacdfs gives it. Where Chromaprint isn't installed (UBI has no
/// package), this is skipped.
#[test]
fn the_real_fpcalc_fingerprints_a_track() {
    common::spawns_children!();
    let works =
        std::process::Command::new("fpcalc").arg("-version").output().is_ok_and(|o| o.status.success());
    if !works {
        common::skip!("no fpcalc: install Chromaprint (Fedora: chromaprint-tools)");
    }
    let disc = Disc::open(&corpus_image("dst-features-2ch-6ch"), 0, Tuning::default()).unwrap();
    let area = disc.toc().areas.iter().position(|a| a.slot == Slot::TwoChannel).unwrap();
    // The corpus's tracks are a fraction of a second, too little for a fingerprint: two minutes
    // from the start of the area (its tail is fill) are given instead.
    let (fp, fed) = fingerprint::fpcalc(Path::new("fpcalc"), fingerprint::FPCALC_TIMEOUT, |out| {
        fingerprint::stream_track(&disc, area, 0, u32::MAX, out, &|| false)
    })
    .unwrap();
    assert_eq!(fed.frames, MAX_SECONDS * 75);
    assert!(fp.len() > 100, "{fp}");
}

/// Busy answers are tried again; a failure to connect never says the key.
#[test]
fn busy_answers_are_retried_and_errors_hide_the_key() {
    #[derive(Debug)]
    struct Flaky(Arc<Mutex<usize>>);
    impl Transport for Flaky {
        fn get(&mut self, url: &str) -> Result<Reply, String> {
            let mut n = self.0.lock().unwrap();
            *n += 1;
            match *n {
                1 => Ok(Reply { status: 503, body: Vec::new(), retry_after: None }),
                2 => Err(format!("connection reset for {url}")),
                _ => Ok(Reply {
                    status: 200,
                    body: json!({"status": "ok", "results": []}).to_string().into_bytes(),
                    retry_after: None,
                }),
            }
        }
    }
    let d = tempfile::tempdir().unwrap();
    let fpcalc = fake_fpcalc(d.path());
    common::spawns_children!();
    let count = Arc::new(Mutex::new(0));
    let mut c = Config::new(KEY.to_owned(), fpcalc);
    (c.max_load, c.min_seconds, c.interval, c.backoff) = (Some(f64::MAX), 0, Duration::ZERO, Duration::ZERO);
    let mut s = Session::new(c, Box::new(Flaky(count.clone())), d.path(), Arc::new(AtomicBool::new(false)));
    let disc = Disc::open(&corpus_image("no-text"), 0, Tuning::default()).unwrap();
    let mut lines = Vec::new();
    let p = s.identify(&disc, 0, &mut |l| lines.push(l)).unwrap();
    assert_eq!(p.recordings.len(), 2);
    assert_eq!(*count.lock().unwrap(), 4, "two tries for the first track, one for the second");

    // Always failing: the error names the problem, not the key.
    #[derive(Debug)]
    struct Down;
    impl Transport for Down {
        fn get(&mut self, url: &str) -> Result<Reply, String> {
            Err(format!("cannot reach {url}"))
        }
    }
    let mut c = Config::new(KEY.to_owned(), fake_fpcalc_path(d.path()));
    (c.max_load, c.min_seconds, c.interval, c.backoff) = (None, 0, Duration::ZERO, Duration::ZERO);
    let mut s = Session::new(c, Box::new(Down), d.path(), Arc::new(AtomicBool::new(false)));
    match s.identify(&disc, 0, &mut |_| {}) {
        Err(acoustid::Skip::Network(e)) => {
            assert!(!e.contains(KEY) && e.contains("<key>") && e.contains("after 4 tries"), "{e}")
        }
        other => panic!("{other:?}"),
    }
}

fn fake_fpcalc_path(dir: &Path) -> PathBuf {
    dir.join("fpcalc")
}

/// The command refuses to start without a usable key or fpcalc, and sends nothing.
#[test]
fn the_command_checks_its_key_and_fpcalc_first() {
    common::spawns_children!();
    let bin = env!("CARGO_BIN_EXE_sacdfs");
    let d = tempfile::tempdir().unwrap();
    let image = corpus_image("no-text");
    let key = d.path().join("key");
    for (content, fpcalc, says) in
        [("not a key!", "fpcalc", "letters and digits"), ("Abc123", "/nonexistent/fpcalc", "doesn't run")]
    {
        fs::write(&key, content).unwrap();
        let out = std::process::Command::new(bin)
            .args(["tags", "fetch", "--acoustid", "--acoustid-key-file"])
            .arg(&key)
            .args(["--fpcalc", fpcalc, "--tags"])
            .arg(d.path().join("tags"))
            .arg(&image)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(says) && !err.contains("Abc123"), "{err}");
    }
    let out = std::process::Command::new(bin)
        .args(["tags", "fetch", "--acoustid", "--acoustid-key-file"])
        .arg(d.path().join("missing"))
        .arg("--tags")
        .arg(d.path())
        .arg(&image)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
}

/// With a key from the environment and a working fpcalc, the command starts its session and
/// reports its counters, without the key; an image that doesn't open asks nothing of anyone.
#[test]
fn the_command_reports_its_counters_without_the_key() {
    let d = tempfile::tempdir().unwrap();
    // Before the shared hold below: fake_fpcalc takes the same lock exclusively.
    let fpcalc = fake_fpcalc(d.path());
    common::spawns_children!();
    let bin = env!("CARGO_BIN_EXE_sacdfs");
    let out = std::process::Command::new(bin)
        .env("ACOUSTID_API_KEY", "Abc123")
        .args(["tags", "fetch", "--acoustid", "--acoustid-budget", "3", "--max-load", "64", "--fpcalc"])
        .arg(&fpcalc)
        .arg("--tags")
        .arg(d.path().join("tags"))
        .arg(d.path().join("missing.iso"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code(), Some(0), "{out:?}");
    assert!(err.contains("acoustid: 0 queued, 0 fingerprinted") && !err.contains("Abc123"), "{err}");
    assert!(d.path().join("tags").is_dir(), "{err}");
    assert!(!d.path().join("args").exists(), "fpcalc fingerprinted nothing");
}
