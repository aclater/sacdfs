// SPDX-License-Identifier: GPL-3.0-or-later

//! The core's API, as a front end calls it, without a mount (#22): every file read through
//! [`Volume`] is byte-identical to the same file read with its [`Reader`], and each request
//! answers what the FUSE front end passes on to the kernel. A Windows or macOS front end runs
//! on the same calls, so these tests cover it too.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::ffi::{OsStr, OsString};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::track::Reader;
use sacdfs::tree::{Node, Output, ROOT, SourceDir, Tree};
use sacdfs::vfs::{
    Access, Attr, DataReply, DirOpen, Error, Kind, Listing, ListingPlus, Lookup, Reply, StatFs, TTL, Volume,
};
use sacdfs::watchdog::Fail;

const IMAGE: &str = "images/dsd-stereo-mch-2048.iso";
/// DST-coded, stereo and multichannel.
const DST_IMAGE: &str = "images/dst-features-2ch-6ch.iso";

/// A reply that hands its answer to the test, as a front end that must answer before its
/// callback returns would wait for it.
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

fn volume(image: &str, output: Output) -> (Volume, Arc<Tree>, Arc<Engine>) {
    let tree = Arc::new(Tree::with_options(
        SourceDir::scan(&common::corpus().join(image)).unwrap(),
        Tuning::default(),
        output,
    ));
    let engine = Arc::new(Engine::new(2, 32 << 20).unwrap());
    let volume = Volume::new(tree.clone(), engine.clone(), 4, 1000, 1000).unwrap();
    (volume, tree, engine)
}

/// Every file and folder below `dir`, by walking the volume as a front end does: listings,
/// then a lookup of each name.
fn walk(v: &Volume, dir: u64, out: &mut Vec<(u64, OsString, Attr)>) {
    let listing: Listing = ask(|r| v.readdir(dir, r)).unwrap();
    let names: Vec<(u64, Kind, OsString)> =
        listing.entries(2).map(|(_, ino, kind, name)| (ino, kind, name.to_owned())).collect();
    let positions: Vec<u64> = listing.entries(0).map(|(next, ..)| next).collect();
    assert_eq!(positions, (1..=names.len() as u64 + 2).collect::<Vec<_>>(), "positions");
    for (ino, kind, name) in names {
        let Lookup::Found(e) = ask(|r| v.lookup(dir, &name, r)).unwrap() else { panic!("{name:?} missing") };
        assert_eq!((e.attr.ino, e.attr.kind), (ino, kind), "{name:?}");
        out.push((ino, name, e.attr));
        if kind == Kind::Dir {
            walk(v, ino, out);
        }
    }
}

#[test]
fn reads_through_the_volume_match_the_reader() {
    for image in [IMAGE, DST_IMAGE] {
        reads_match(image);
    }
}

fn reads_match(image: &str) {
    let (v, tree, engine) = volume(image, Output::default());
    let mut all = Vec::new();
    walk(&v, ROOT, &mut all);
    let files: Vec<_> = all.iter().filter(|(_, _, a)| a.kind == Kind::File).collect();
    assert!(files.iter().any(|(_, n, _)| n.to_string_lossy().ends_with(".dsf")), "no DSF file");
    assert!(files.iter().any(|(_, n, _)| n.to_string_lossy().ends_with(".wav")), "no WAV file");
    for (ino, name, attr) in files {
        let fh = ask(|r| v.open(*ino, r)).unwrap();
        // In the sizes a kernel asks for, and one odd size, to the end and past it.
        let mut got = Vec::new();
        let mut off = 0;
        while off < attr.size {
            let chunk = ask(|r| v.read(*ino, fh, off, 131_072 - 7, r)).unwrap();
            assert!(!chunk.is_empty(), "{name:?}: empty read at {off}");
            off += chunk.len() as u64;
            got.extend_from_slice(&chunk);
        }
        assert_eq!(
            ask(|r| v.read(*ino, fh, attr.size, 4096, r)).unwrap(),
            Vec::<u8>::new(),
            "{name:?}: past the end"
        );
        let node = tree.get(*ino).unwrap();
        let Node::File(file) = &*node else { panic!("{name:?}: not a file") };
        let (want, _) = Reader::new(file.clone()).read(&engine, 0, attr.size).unwrap();
        assert_eq!(got.len(), want.len(), "{name:?}");
        assert!(got == want, "{name:?}: the bytes differ from the reader's");
        v.release(fh);
        assert_eq!(ask(|r| v.read(*ino, fh, 0, 4096, r)), Err(Error::BadHandle), "{name:?}: released");
    }
}

#[test]
fn requests_answer_as_the_fuse_front_end_expects() {
    let (v, _tree, _) = volume(IMAGE, Output::dsf_only());
    // The root: a folder, kept for the TTL.
    let (ttl, root) = ask(|r| v.getattr(ROOT, None, r)).unwrap();
    assert_eq!((root.ino, root.kind, root.perm, root.nlink), (ROOT, Kind::Dir, 0o555, 2));
    assert_eq!((root.uid, root.gid), (1000, 1000));
    assert_eq!(ttl, TTL);
    // A missing name is a negative entry; `..` of the root is the root.
    assert_eq!(ask(|r| v.lookup(ROOT, OsStr::new("no such name"), r)), Ok(Lookup::Missing { ttl: TTL }));
    let Lookup::Found(dots) = ask(|r| v.lookup(ROOT, OsStr::new(".."), r)).unwrap() else { panic!() };
    assert_eq!(dots.attr.ino, ROOT);
    // An unknown number.
    assert_eq!(ask(|r: To<(Duration, Attr)>| v.getattr(u64::MAX - 1, None, r)), Err(Error::NotFound));
    // A folder and a file.
    let mut all = Vec::new();
    walk(&v, ROOT, &mut all);
    let (dir, _, _) = all.iter().find(|(_, _, a)| a.kind == Kind::Dir).unwrap();
    let (file, _, fattr) = all.iter().find(|(_, _, a)| a.kind == Kind::File).unwrap();
    assert_eq!((fattr.perm, fattr.nlink, fattr.blocks), (0o444, 1, fattr.size.div_ceil(512)));
    assert_eq!(ask(|r| v.open(*dir, r)), Err(Error::IsDir));
    assert!(matches!(ask(|r| v.opendir(*dir, r)), Ok(DirOpen { .. })));
    assert_eq!(ask(|r: To<DirOpen>| v.opendir(*file, r)), Err(Error::NotDir));
    assert_eq!(ask(|r: To<Listing>| v.readdir(*file, r)).err(), Some(Error::NotDir));
    assert_eq!(ask(|r: To<ListingPlus>| v.readdir_plus(*file, 0, r)).err(), Some(Error::NotDir));
    assert_eq!(ask(|r| v.lookup(*file, OsStr::new(".."), r)), Err(Error::NotDir));
    let Lookup::Found(up) = ask(|r| v.lookup(*dir, OsStr::new(".."), r)).unwrap() else { panic!() };
    assert_eq!(up.attr.kind, Kind::Dir);
    // Access: read everywhere, write nowhere, execute folders only.
    assert_eq!(ask(|r| v.access(*file, Access::default(), r)), Ok(()));
    assert_eq!(ask(|r| v.access(*dir, Access { write: true, execute: false }, r)), Err(Error::ReadOnly));
    assert_eq!(ask(|r| v.access(*file, Access { write: false, execute: true }, r)), Err(Error::Access));
    assert_eq!(ask(|r| v.access(*dir, Access { write: false, execute: true }, r)), Ok(()));
    // readdirplus gives what each lookup gives, from the offset asked.
    let plus = ask(|r| v.readdir_plus(ROOT, 2, r)).unwrap();
    assert!(!plus.is_empty());
    for (next, name, e) in &plus {
        assert!(*next >= 3);
        assert_eq!(ask(|r| v.lookup(ROOT, name, r)), Ok(Lookup::Found(*e)), "{name:?}");
    }
    // The size of the image, all used, and every node counted.
    let StatFs { blocks, frsize, bsize, files, namelen } = v.statfs();
    let bytes = std::fs::metadata(common::corpus().join(IMAGE)).unwrap().len();
    assert_eq!((frsize, bsize, namelen), (1 << 16, 1 << 16, 255));
    assert_eq!(blocks, bytes.div_ceil(1 << 16));
    assert!(files >= all.len() as u64);
}
