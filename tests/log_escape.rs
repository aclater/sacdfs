// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon's log escapes control and format characters in names from SOURCE (#391): a name
//! with a newline, an escape sequence or a bidi control can't split, recolour or reorder a line
//! of the log. Needs `/dev/fuse` and `fusermount3`, like `tests/mount.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn is_mounted(mountpoint: &Path) -> bool {
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mp = mountpoint.to_str().unwrap();
    info.lines().any(|l| l.split(' ').nth(4) == Some(mp))
}

#[test]
fn names_in_log_lines_are_escaped() {
    common::skip_without_fuse!();
    let source = tempfile::tempdir().unwrap();
    // Not an image: listed, refused, and logged as not shown, with its path.
    let name = "a\u{1b}[31m\n[2000-01-01T00:00:00Z ERROR sacdfs] forged\u{202e}b.iso";
    std::fs::write(source.path().join(OsStr::from_bytes(name.as_bytes())), vec![0u8; 4096]).unwrap();
    let at = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let log = tempfile::NamedTempFile::new().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .env("XDG_CACHE_HOME", cache.path())
        .env("RUST_LOG", "info")
        .stderr(log.reopen().unwrap())
        .arg("-f")
        .arg(source.path())
        .arg(at.path())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !is_mounted(at.path()) {
        assert!(start.elapsed() < Duration::from_secs(20), "not mounted");
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::read_dir(at.path()).unwrap().count();
    common::unmount_retrying(at.path());
    common::stop_gracefully(&mut child);

    let text = std::fs::read_to_string(log.path()).unwrap();
    let escaped = "a\\u{1b}[31m\\n[2000-01-01T00:00:00Z ERROR sacdfs] forged\\u{202e}b.iso";
    assert!(text.contains(escaped), "no escaped name in the log:\n{text}");
    assert!(!text.contains('\u{1b}') && !text.contains('\u{202e}'), "a raw character in the log:\n{text}");
    // Every line is one of sacdfs's own, none made by the name.
    for line in text.lines() {
        assert!(line.starts_with('['), "a line not made by the logger: {line:?}");
        assert!(!line.starts_with("[2000-01-01"), "a forged line: {line:?}");
    }
}
