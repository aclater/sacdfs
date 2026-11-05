// SPDX-License-Identifier: GPL-3.0-or-later

//! The sidecar allow-list (`--sidecars`), and the paths it serves: whatever a symbolic link in
//! SOURCE or a relative path says, nothing outside SOURCE or under MOUNTPOINT is opened.
//!
//! Input, separated by [`SEP`](sacdfs_fuzz::SEP): the list; a name; the target of a link in
//! SOURCE (`@ROOT@` stands for SOURCE's absolute path, `@OUT@` for its parent's); a relative
//! path.

#![no_main]

use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use sacdfs::sidecar::{self, Sidecars, hidden, is_art_name};
use sacdfs_fuzz::{fields, scratch_dir};

const OUTSIDE: &[u8] = b"outside SOURCE";
const MOUNTED: &[u8] = b"under MOUNTPOINT";
const INSIDE: &[u8] = b"inside";

struct Fixture {
    outer: PathBuf,
    source: PathBuf,
    root: OwnedFd,
}

/// `outer/secret.jpg`, and SOURCE at `outer/src` with `d/in.jpg`, `mnt/m.jpg` and links out.
fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let outer = scratch_dir("sidecars");
        let source = outer.join("src");
        let put = |p: &Path, b: &[u8]| {
            if let Err(e) =
                std::fs::create_dir_all(p.parent().unwrap_or(p)).and_then(|()| std::fs::write(p, b))
            {
                panic!("{}: {e}", p.display());
            }
        };
        put(&outer.join("secret.jpg"), OUTSIDE);
        put(&source.join("d/in.jpg"), INSIDE);
        put(&source.join("mnt/m.jpg"), MOUNTED);
        for (link, to) in [("d/out.jpg", "../../secret.jpg"), ("d/up", "../.."), ("d/mntlink", "../mnt")] {
            if let Err(e) = std::os::unix::fs::symlink(to, source.join(link)) {
                panic!("{link}: {e}");
            }
        }
        let root = match sidecar::open_root(&source) {
            Ok(r) => r,
            Err(e) => panic!("{}: {e}", source.display()),
        };
        Fixture { outer, source, root }
    })
}

/// Opening `rel` shows only what lies inside SOURCE and outside MOUNTPOINT.
fn check_open(f: &Fixture, rel: &Path) {
    let _ = sidecar::stat_file(&f.root, rel);
    let _ = sidecar::stat_dir(&f.root, rel);
    if let Ok((mut file, _)) = sidecar::open_file(&f.root, rel) {
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_ok() {
            assert_ne!(bytes, OUTSIDE, "{rel:?} read a file outside SOURCE");
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let [list, name, link, rel] = fields::<4>(data);
    let name = OsStr::from_bytes(name);

    if let Ok(list) = std::str::from_utf8(list) {
        if let Ok(s) = Sidecars::parse(list) {
            let _ = s.describe();
            if hidden(name) {
                assert!(!s.file(name) && !s.folder(name), "hidden {name:?} passed through");
            }
            if name.as_bytes().to_ascii_lowercase().ends_with(b".iso") {
                assert!(!s.file(name), "image {name:?} passed through");
            }
        }
    }
    let _ = is_art_name(name);

    let f = fixture();
    if !link.contains(&0) && !link.is_empty() && link.len() < 4096 {
        let text = String::from_utf8_lossy(link)
            .replace("@ROOT@", &f.source.to_string_lossy())
            .replace("@OUT@", &f.outer.to_string_lossy());
        // Valid UTF-8 inputs keep their bytes; the rest are made so, which the kernel accepts
        // the same.
        let here = f.source.join("d/fuzz.jpg");
        let _ = std::fs::remove_file(&here);
        if std::os::unix::fs::symlink(&text, &here).is_ok() {
            let target =
                sidecar::target(&f.root, &f.source, Path::new("d/fuzz.jpg"), true, Some(Path::new("mnt")));
            if let Ok(to) = target {
                assert!(
                    to.components().all(|c| matches!(c, Component::Normal(_))),
                    "{text:?} resolved to {to:?}"
                );
                assert!(!to.starts_with("mnt"), "{text:?} resolved under MOUNTPOINT: {to:?}");
                check_open(f, &to);
            }
        }
    }

    if !rel.contains(&0) {
        let rel = Path::new(OsStr::from_bytes(rel));
        check_open(f, rel);
        if let Ok(to) = sidecar::target(&f.root, &f.source, rel, false, Some(Path::new("mnt"))) {
            assert!(to.components().all(|c| matches!(c, Component::Normal(_))), "{rel:?} to {to:?}");
            assert!(!to.starts_with("mnt"), "{rel:?} under MOUNTPOINT: {to:?}");
        }
    }
});
