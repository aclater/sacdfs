// SPDX-License-Identifier: GPL-3.0-or-later

//! Folder art replaced after it was looked up (#628, #631), through the core's own calls: the
//! kernel is told one generation for a node number however often the file is replaced, and an
//! open of art found changed is served as the file is now, while the network file server's
//! open still gets `ESTALE`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;

use common::{ask, corpus};
use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::sidecar::Sidecars;
use sacdfs::tree::{Output, ROOT, SourceDir, Tree};
use sacdfs::vfs::{ART_KERNEL_GENERATION, Caching, EntryAttr, Error, Lookup, Opened, Volume};

fn found(v: &Volume, parent: u64, name: &str) -> EntryAttr {
    match ask(|r| v.lookup(parent, OsStr::new(name), r)).unwrap() {
        Lookup::Found(e) => e,
        Lookup::Missing { .. } => panic!("{name} missing"),
    }
}

fn read_all(v: &Volume, ino: u64, fh: u64) -> Vec<u8> {
    ask(|r| v.read(ino, fh, 0, 65536, r)).unwrap().to_vec()
}

fn volume(src: &Path) -> Volume {
    let sel = Sidecars::standard();
    let tree = Tree::with_options(
        SourceDir::scan_with(src, &sel, None).unwrap(),
        Tuning::default(),
        Output::default(),
    )
    .watching(&sel);
    Volume::new(Arc::new(tree), Arc::new(Engine::new(2, 32 << 20).unwrap()), 4, 1000, 1000).unwrap()
}

#[test]
fn replaced_art_keeps_the_kernels_generation_and_opens_as_it_is_now() {
    let dir = tempfile::tempdir().unwrap();
    let album = dir.path().join("Artist/Album");
    std::fs::create_dir_all(&album).unwrap();
    std::os::unix::fs::symlink(corpus().join("images/dsd-stereo-mch-2048.iso"), album.join("disc.iso"))
        .unwrap();
    std::fs::write(album.join("cover-0.jpg"), b"art 0").unwrap();
    let v = volume(dir.path());
    let a = found(&v, ROOT, "Artist");
    let b = found(&v, a.attr.ino, "Album");
    let d = found(&v, b.attr.ino, "disc");
    let area = found(&v, d.attr.ino, "Stereo");
    let before = found(&v, area.attr.ino, "cover.jpg");
    assert_eq!(before.kernel_generation, ART_KERNEL_GENERATION);
    let ino = before.attr.ino;

    // Replaced after the lookup, as a reader's open comes in between.
    std::fs::write(album.join("cover-0.jpg"), b"a new and longer art").unwrap();
    // The network file server's open: the handle it holds is of the old file.
    assert_eq!(ask(|r| v.open(ino, r)), Err(Error::Stale));
    // The kernel's: the file as it is now, read past the size the kernel knows.
    let Opened { fh, caching } = ask(|r| v.open_by(ino, 0, r)).unwrap();
    assert_eq!(caching, Caching::Direct, "art found changed is read directly");
    assert_eq!(read_all(&v, ino, fh), b"a new and longer art");
    v.release(fh);

    // The next lookup: the same number, a new generation for a handle, the same for the kernel.
    let after = found(&v, area.attr.ino, "cover.jpg");
    assert_eq!(after.attr.ino, ino);
    assert_ne!(after.generation, before.generation);
    assert_eq!(after.kernel_generation, before.kernel_generation);
    let Opened { fh, caching } = ask(|r| v.open_by(ino, 0, r)).unwrap();
    assert_eq!(caching, Caching::Drop, "art as listed drops the kernel's pages at each open");
    v.release(fh);
}
