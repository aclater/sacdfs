// SPDX-License-Identifier: GPL-3.0-or-later

//! A tags file written by `sacdfs tags fetch`, served (#185, phase 2): the looked-up title in the
//! tag, and the file's name as it was. In a test binary of its own, since the tag store is
//! process-wide and the lookup's other tests read the disc's own tags.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::time::Duration;

use sacdfs::disc::{Disc, Tuning};
use sacdfs::lookup::{self, Client, Options, Reply, Transport};
use sacdfs::tags::{self, Store};
use sacdfs::track::Format;
use sacdfs::tree::{SourceDir, Tree};
use sacdfs::verify::id3;
use scarletbook::Slot;
use serde_json::json;

const RELEASE: &str = "00000001-0000-4000-8000-000000000001";

#[derive(Debug)]
struct One(serde_json::Value);

impl Transport for One {
    fn get(&mut self, url: &str) -> Result<Reply, String> {
        let body =
            if url.contains("/release/") { self.0.clone() } else { json!({"releases": [{"id": RELEASE}]}) };
        Ok(Reply { status: 200, body: body.to_string().into_bytes(), retry_after: None })
    }
}

#[test]
fn the_title_changes_and_the_name_stays() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    let image = common::corpus().join("images/no-text.iso");
    std::os::unix::fs::symlink(&image, lib.join("disc.iso")).unwrap();

    let disc = Disc::open(&image, 0, Tuning::default()).unwrap();
    let tracks: Vec<_> = disc
        .toc()
        .area(Slot::TwoChannel)
        .unwrap()
        .tracks
        .iter()
        .map(|t| {
            json!({"title": format!("Looked-up {}", t.number),
                   "length": u64::from(t.duration_frames) * 1000 / 75})
        })
        .collect();
    let release = json!({"id": RELEASE, "title": "An Album", "label-info": [{"catalog-number": "PYSACD-TEST"}],
                         "media": [{"format": "Hybrid SACD", "tracks": tracks}]});
    let tagsdir = d.path().join("tags");
    let mut c = Client::paced(One(release), Duration::ZERO, Duration::ZERO);
    let mut lines = Vec::new();
    let s =
        lookup::fetch(&tagsdir, std::slice::from_ref(&image), Options::default(), &mut c, |l| lines.push(l));
    assert_eq!(s.written, 1, "{lines:?}");

    tags::install(Some(Store::new(&tagsdir)));
    let tree = Tree::new(SourceDir::scan(&lib).unwrap());
    let mut seen = 0;
    for (p, f) in tree.all_files() {
        if f.format() != Format::Dsf {
            continue;
        }
        seen += 1;
        let frames = id3::frames(f.tag());
        let value = |key: &str| frames.iter().find(|x| x.key == key).map(|x| x.value.as_str());
        assert_eq!(value("TIT2"), Some(format!("Looked-up {}", f.number).as_str()));
        assert_eq!(value("TALB"), Some("An Album"));
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name, format!("{:02} - Track {:02}.dsf", f.number, f.number));
        assert!(f.tags.is_some(), "a new generation and mtime: {name}");
    }
    assert!(seen > 0);
    tags::install(None);
}
