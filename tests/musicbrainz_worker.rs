// SPDX-License-Identifier: GPL-3.0-or-later

//! The mount's background lookup (`--musicbrainz`, #185), from recorded responses: it asks only
//! about discs that lack text, once per disc across restarts, writes what `sacdfs tags fetch`
//! would, and never holds up the serving side, even while the service doesn't answer. Nothing here
//! touches the network.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sacdfs::disc::{Disc, Tuning};
use sacdfs::lookup::worker::{self, Config, Worker};
use sacdfs::lookup::{Client, DiscFacts, Options, Reply, Transport, wants_lookup};
use sacdfs::tags::{self, Store};
use sacdfs::track::Format;
use sacdfs::tree::{SourceDir, Tree};
use sacdfs::verify::id3;
use scarletbook::Slot;
use serde_json::{Value, json};

const RELEASE: &str = "00000001-0000-4000-8000-000000000001";

fn image(id: &str) -> PathBuf {
    common::corpus().join("images").join(format!("{id}.iso"))
}

/// A release with the corpus's catalogue number and the stereo tracks of `image`.
fn release_for(image: &Path) -> Value {
    let d = Disc::open(image, 0, Tuning::default()).unwrap();
    let tracks: Vec<Value> = d
        .toc()
        .area(Slot::TwoChannel)
        .unwrap()
        .tracks
        .iter()
        .map(|t| json!({"title": format!("Looked-up {}", t.number), "length": u64::from(t.duration_frames) * 1000 / 75}))
        .collect();
    json!({"id": RELEASE, "title": "An Album", "artist-credit": [{"name": "An Artist"}],
           "release-group": {"id": "00000002-0000-4000-8000-000000000002"},
           "label-info": [{"catalog-number": "PYSACD-TEST"}],
           "media": [{"format": "Hybrid SACD", "tracks": tracks}]})
}

/// Answers a catalogue search with the release, the release with `release`, else 404; counts
/// requests. With `gate`, the first request waits for a message: a service that doesn't answer.
#[derive(Clone)]
struct Recorded {
    release: Option<Value>,
    urls: Arc<Mutex<Vec<String>>>,
    gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
}

impl Recorded {
    fn new(release: Option<Value>) -> Recorded {
        Recorded { release, urls: Arc::default(), gate: None }
    }

    fn count(&self) -> usize {
        self.urls.lock().unwrap().len()
    }
}

impl Transport for Recorded {
    fn get(&mut self, url: &str) -> Result<Reply, String> {
        if let Some(g) = self.gate.take() {
            let _ = g.lock().unwrap().recv();
        }
        self.urls.lock().unwrap().push(url.to_owned());
        let ok = |v: Value| Ok(Reply { status: 200, body: v.to_string().into_bytes(), retry_after: None });
        match &self.release {
            Some(r) if url.contains("/release/") => ok(r.clone()),
            Some(_) if url.contains("query=catno") => ok(json!({"releases": [{"id": RELEASE}]})),
            _ => Ok(Reply { status: 404, body: b"{}".to_vec(), retry_after: None }),
        }
    }
}

fn config(tags: &Path, state: Option<PathBuf>) -> Config {
    Config {
        tags: tags.to_owned(),
        state,
        options: Options::default(),
        first_delay: Duration::ZERO,
        pass_every: Duration::from_secs(3600),
    }
}

fn client(r: &Recorded) -> Client<Recorded> {
    Client::paced(r.clone(), Duration::ZERO, Duration::ZERO)
}

/// The two corpus discs the tests use: one without text, one with all of it.
#[test]
fn the_corpus_discs_are_what_the_tests_take_them_for() {
    for (id, wants) in [("no-text", true), ("full-text", false)] {
        let p = image(id);
        let d = Disc::open(&p, 0, Tuning::default()).unwrap();
        assert_eq!(wants_lookup(&DiscFacts::new(d.toc(), &p), None), wants, "{id}");
    }
}

#[test]
fn only_discs_without_text_are_asked_about_and_only_once() {
    let d = tempfile::tempdir().unwrap();
    let tags = d.path().join("tags");
    fs::create_dir_all(&tags).unwrap();
    let state = d.path().join("cache/musicbrainz");
    let images = vec![image("full-text"), image("no-text")];
    let r = Recorded::new(Some(release_for(&image("no-text"))));

    let first = worker::pass_once(&config(&tags, Some(state.clone())), &mut client(&r), &images);
    assert_eq!((first.looked_at, first.complete, first.matched, first.failed), (2, 1, 1, 0), "{first:?}");
    assert!(first.requests > 0);
    let asked = r.count();
    assert!(
        r.urls.lock().unwrap().iter().all(|u| !u.contains("ALB-CAT-0042")),
        "the disc with text isn't asked about"
    );
    let written: Vec<_> = fs::read_dir(&tags).unwrap().collect();
    assert_eq!(written.len(), 1);

    // A restart: nothing is read or asked again.
    let again = worker::pass_once(&config(&tags, Some(state.clone())), &mut client(&r), &images);
    assert_eq!((again.settled, again.looked_at, again.requests), (2, 0, 0), "{again:?}");
    assert_eq!(r.count(), asked);

    // Without the state file the TOCs are read again, but the tags file says the disc was looked
    // up: still nothing is asked.
    let bare = worker::pass_once(&config(&tags, None), &mut client(&r), &images);
    assert_eq!((bare.looked_at, bare.done, bare.complete, bare.requests), (2, 1, 1, 0), "{bare:?}");
}

#[test]
fn a_disc_without_a_match_is_not_asked_about_again_after_a_restart() {
    let d = tempfile::tempdir().unwrap();
    let tags = d.path().join("tags");
    fs::create_dir_all(&tags).unwrap();
    let state = d.path().join("musicbrainz");
    let images = vec![image("no-text")];
    let r = Recorded::new(None);
    let first = worker::pass_once(&config(&tags, Some(state.clone())), &mut client(&r), &images);
    assert_eq!((first.refused, first.matched), (1, 0), "{first:?}");
    assert_eq!(fs::read_dir(&tags).unwrap().count(), 0, "no match writes nothing");
    let asked = r.count();
    let again = worker::pass_once(&config(&tags, Some(state)), &mut client(&r), &images);
    assert_eq!((again.settled, again.requests), (1, 0), "{again:?}");
    assert_eq!(r.count(), asked);
}

/// While the service doesn't answer, the tree lists and reads as fast as without the worker; once
/// it answers, the worker writes the tags file and the served tag carries the looked-up title.
#[test]
fn serving_never_waits_for_the_worker() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    std::os::unix::fs::symlink(image("no-text"), lib.join("disc.iso")).unwrap();
    let tagsdir = d.path().join("tags");
    fs::create_dir_all(&tagsdir).unwrap();
    tags::install(Some(Store::new(&tagsdir)));

    let (open, gate) = mpsc::channel();
    let mut r = Recorded::new(Some(release_for(&image("no-text"))));
    r.gate = Some(Arc::new(Mutex::new(gate)));
    let images = vec![lib.join("disc.iso")];
    let w = Worker::start(config(&tagsdir, None), client(&r), move || images.clone()).unwrap();

    // The worker is now held in its first request.
    let held = Instant::now();
    while r.count() == 0 && held.elapsed() < Duration::from_millis(300) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let tree = Tree::new(SourceDir::scan(&lib).unwrap());
    let t = Instant::now();
    let mut files = 0;
    for (_, f) in tree.all_files() {
        assert!(id3::frames(f.tag()).iter().all(|x| !x.value.starts_with("Looked-up")));
        files += 1;
    }
    let listing = t.elapsed();
    assert!(files > 0);
    assert!(listing < Duration::from_secs(2), "listing took {listing:?} while the worker waited");
    assert_eq!(r.count(), 0, "the worker is still waiting for its answer");

    open.send(()).unwrap();
    let t = Instant::now();
    while fs::read_dir(&tagsdir).unwrap().count() == 0 {
        assert!(t.elapsed() < Duration::from_secs(20), "no tags file written");
        std::thread::sleep(Duration::from_millis(20));
    }
    w.stop_and_join();

    let tree = Tree::new(SourceDir::scan(&lib).unwrap());
    let mut seen = 0;
    for (_, f) in tree.all_files() {
        if f.format() != Format::Dsf {
            continue;
        }
        let frames = id3::frames(f.tag());
        let title = frames.iter().find(|x| x.key == "TIT2").map(|x| x.value.clone());
        assert_eq!(title, Some(format!("Looked-up {}", f.number)));
        assert!(f.tags.is_some(), "a new generation and mtime");
        seen += 1;
    }
    assert!(seen > 0);
}
