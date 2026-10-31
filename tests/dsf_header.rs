// SPDX-License-Identifier: GPL-3.0-or-later

//! DSF header and tag reads (#32): what a tagger costs, read the way the kernel asks for it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use sacdfs::engine::Engine;
use sacdfs::track::{Format, KERNEL_FIRST_WINDOW, Reader};
use sacdfs::tree::{Output, SourceDir, Tree};

/// The kernel's requests for a tagger that reads a DSF file's first 4 KiB, then its tag: the
/// first 128 KiB, the next 128 KiB in the background, and the page that holds the tag.
/// None of them starts read-ahead, so the walk decodes only the frames those bytes hold;
/// and a player's next read does start it.
#[test]
fn dsf_tag_walk_starts_no_read_ahead() {
    let engine = Engine::new(2, 0).unwrap();
    let tree = Tree::with_options(
        SourceDir::scan(&corpus().join("images")).unwrap(),
        sacdfs::disc::Tuning::default(),
        Output::dsf_only(),
    );
    let files = tree.all_files();
    let (mut walked, mut continued) = (0, 0);
    for (path, f) in &files {
        assert_eq!(f.format(), Format::Dsf);
        let reader = Reader::new(f.clone());
        let page = 128 << 10;
        for off in [0, page] {
            let (_, ahead) = reader.read(&engine, off, page).unwrap();
            assert!(ahead.is_none(), "{path:?}: read-ahead after the kernel's read at {off}");
        }
        let tag_page = f.data_end() / 4096 * 4096;
        let (t, ahead) = reader.read(&engine, tag_page, f.size - tag_page).unwrap();
        assert!(ahead.is_none(), "{path:?}: read-ahead after the tag read");
        assert!(t.ends_with(f.tag()), "{path:?}: the tag");
        walked += 1;
        // A player goes on reading, 32 KiB at a time: the first read past the kernel's
        // first window starts read-ahead, none before it does.
        let reader = Reader::new(f.clone());
        let step = 32 << 10;
        let mut off = 0;
        while off < f.data_end() {
            let (_, ahead) = reader.read(&engine, off, step).unwrap();
            if ahead.is_some() {
                assert!(off >= KERNEL_FIRST_WINDOW, "{path:?}: read-ahead at {off}");
                continued += usize::from(off == KERNEL_FIRST_WINDOW);
                break;
            }
            off += step;
        }
    }
    assert!(walked > 100, "{walked} files");
    assert!(continued > 0, "no corpus file is long enough to check a player's read-ahead");
}
