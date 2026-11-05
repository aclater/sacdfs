// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs tags fetch` (#185): whatever MusicBrainz answers, for a search and for a release, read
//! and matched against a real disc's areas, and the values written to a tags file.
//!
//! Input: the response to every request.

#![no_main]

use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use sacdfs::lookup::{self, AreaFacts, Client, DiscFacts, Options, Reply, TrackFacts, Transport};
use sacdfs::tags::TagsFile;
use sacdfs_fuzz::valid_toc;

struct Same<'a>(&'a [u8]);

impl Transport for Same<'_> {
    fn get(&mut self, _: &str) -> Result<Reply, String> {
        Ok(Reply { status: 200, body: self.0.to_vec(), retry_after: None })
    }
}

fuzz_target!(|data: &[u8]| {
    let areas: Vec<AreaFacts> = valid_toc()
        .areas
        .iter()
        .map(|a| AreaFacts {
            slot: a.slot,
            tracks: a
                .tracks
                .iter()
                .map(|t| TrackFacts {
                    number: t.number,
                    start_frame: t.start_frame,
                    duration_frames: t.duration_frames,
                    ..TrackFacts::default()
                })
                .collect(),
        })
        .collect();
    let facts = DiscFacts {
        numbers: vec!["0000000012345".into()],
        set_size: 1,
        set_position: 1,
        areas,
        album: Some("Album".into()),
        artist: Some("Artist".into()),
        year: Some("2001".into()),
    };
    let refs: Vec<&AreaFacts> = facts.areas.iter().collect();
    let options = Options { by_name: true, by_durations: true, ..Options::default() };
    let mut client = Client::paced(Same(data), Duration::ZERO, Duration::ZERO);
    let Ok((found, _)) = lookup::look_up(&facts, &refs, options, &mut client) else { return };

    // What would be written always reads back as the same tags.
    let planned = lookup::plan(&mut TagsFile::default(), &refs, &found, options);
    let text = planned.to_text();
    match TagsFile::parse(&text) {
        Ok(again) => assert_eq!(again.to_text(), text),
        Err(e) => panic!("{text:?} does not read back: {e}"),
    }
});
