// SPDX-License-Identifier: GPL-3.0-or-later

//! A read that fails with `EIO` logs what failed, where and why, and a failing image logs
//! once per window, not once per read (#339). A test binary of its own: it installs the
//! process's logger.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs::OpenOptions;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::tree::{Output, ROOT, SourceDir, Tree};
use sacdfs::vfs::{Attr, DataReply, Error, Kind, Listing, Lookup, Reply, Volume};
use sacdfs::watchdog::Fail;

/// DST-coded, with a 6-channel area: the kind of file #339 failed on.
const IMAGE: &str = "images/dst-features-2ch-6ch.iso";

static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Capture;

impl log::Log for Capture {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        m.level() <= log::Level::Warn
    }
    fn log(&self, r: &log::Record<'_>) {
        if self.enabled(r.metadata()) {
            LINES.lock().unwrap().push(format!("{}", r.args()));
        }
    }
    fn flush(&self) {}
}

static CAPTURE: Capture = Capture;

struct To<T>(Sender<Result<T, Error>>);

impl<T: Send + 'static> Fail for To<T> {
    fn fail(self, err: Error) {
        let _ = self.0.send(Err(err));
    }
}

impl<T: Send + 'static> Reply<T> for To<T> {
    fn reply(self, r: Result<T, Error>) {
        let _ = self.0.send(r);
    }
}

impl DataReply for To<Vec<u8>> {
    fn data(self, data: &[u8]) {
        let _ = self.0.send(Ok(data.to_vec()));
    }
}

fn ask<T: Send + 'static>(f: impl FnOnce(To<T>)) -> Result<T, Error> {
    let (tx, rx) = mpsc::channel::<Result<T, Error>>();
    f(To(tx));
    rx.recv_timeout(Duration::from_secs(60)).expect("no answer")
}

/// Every file below `dir`, with its attributes.
fn files(v: &Volume, dir: u64, out: &mut Vec<(u64, String, Attr)>) {
    let listing: Listing = ask(|r| v.readdir(dir, r)).unwrap();
    let names: Vec<_> = listing.entries(2).map(|(_, _, kind, name)| (kind, name.to_owned())).collect();
    for (kind, name) in names {
        let Lookup::Found(e) = ask(|r| v.lookup(dir, &name, r)).unwrap() else { panic!("{name:?} missing") };
        match kind {
            Kind::Dir => files(v, e.attr.ino, out),
            Kind::File => out.push((e.attr.ino, name.to_string_lossy().into_owned(), e.attr)),
        }
    }
}

/// An image cut short after it was listed: reads of its 6-channel WAV past the cut fail with
/// `EIO`, and the log says which image, track, inode and offset, and why, once.
#[test]
fn a_failed_read_logs_its_image_inode_offset_and_cause_once() {
    log::set_logger(&CAPTURE).unwrap();
    log::set_max_level(log::LevelFilter::Warn);

    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("cut-short.iso");
    std::fs::copy(common::corpus().join(IMAGE), &image).unwrap();
    let tree =
        Arc::new(Tree::with_options(SourceDir::scan(&image).unwrap(), Tuning::default(), Output::default()));
    let engine = Arc::new(Engine::new(2, 32 << 20).unwrap());
    let v = Volume::new(tree, engine, 4, 1000, 1000).unwrap();

    let mut all = Vec::new();
    files(&v, ROOT, &mut all);
    // The largest WAV: the 6-channel area's.
    let (ino, name, attr) = all
        .into_iter()
        .filter(|(_, n, _)| n.ends_with(".wav"))
        .max_by_key(|(_, _, a)| a.size)
        .expect("no WAV");
    let fh = ask(|r| v.open(ino, r)).unwrap();

    // Cut the image down to its first 1 MiB: the TOC is read already, the audio is gone.
    OpenOptions::new().write(true).open(&image).unwrap().set_len(1 << 20).unwrap();

    let at = attr.size / 2;
    for i in 0..5 {
        let r = ask(|r| v.read(ino, fh, at + i * 4096, 65536, r));
        assert_eq!(r, Err(Error::Io), "{name}: read {i} of the cut image");
    }
    let lines: Vec<String> =
        LINES.lock().unwrap().iter().filter(|l| l.contains("cut-short.iso")).cloned().collect();
    assert_eq!(lines.len(), 1, "one line for the image's failed reads: {lines:?}");
    let line = &lines[0];
    for want in [format!("inode {ino}"), format!("read at {at}"), "EIO".to_owned()] {
        assert!(line.contains(&want), "{want:?} missing from {line:?}");
    }
    v.release(fh);
}
