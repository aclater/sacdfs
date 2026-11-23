// SPDX-License-Identifier: GPL-3.0-or-later

//! The TOC fingerprint of an image on a filesystem whose times can't be relied on (NFS, SMB,
//! FUSE, #588): the bytes its TOC was read from are read again and compared, so that a rewrite
//! the times don't show is found. Its own test binary: it sets the process's open-file check
//! far apart, so that the handle kept is the one the rewrite is seen through.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::Duration;

use sacdfs::disc::{Disc, Tuning};

#[test]
fn a_toc_that_changed_under_the_open_file_is_found() {
    sacdfs::handles::global().set_revalidate_every(Duration::from_secs(3600));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disc.iso");
    std::fs::copy(common::corpus().join("images/no-igl.iso"), &path).unwrap();
    let other = std::fs::read(common::corpus().join("images/no-text.iso")).unwrap();
    let disc = Disc::open(&path, 1, Tuning::default()).unwrap();

    // Unchanged: the fingerprint finds the same disc.
    disc.fingerprint_now();
    assert!(!disc.is_replaced());
    assert!(disc.fresh());

    // Another disc written in place, seen through the handle already open: the file's times
    // are not looked at (the interval is an hour), the TOC bytes are.
    let f = std::fs::File::options().write(true).open(&path).unwrap();
    common::write_all_at(&f, &other, 0).unwrap();
    drop(f);
    assert!(!disc.is_replaced(), "nothing looked yet");
    disc.fingerprint_now();
    assert!(disc.is_replaced());
    assert!(!disc.fresh());
}
