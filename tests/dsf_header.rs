// SPDX-License-Identifier: GPL-3.0-or-later

//! DSF header and tag reads (#32): what a tagger costs, read the way the kernel asks for it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use sacdfs::engine::Engine;
use sacdfs::track::{Format, HEAD_WINDOW, Reader};
use sacdfs::tree::{Output, SourceDir, Tree};

/// The kernel's requests for a tagger that reads a DSF file's first 4 KiB, then its tag: the
/// first 128 KiB, the next 128 KiB in the background, and the page that holds the tag.
/// None of them starts read-ahead, so the walk decodes only the frames those bytes hold;
/// and a player's next read past the head window does start it (on the corpus's short files,
/// none starts it; [`real_discs_head_reads`] checks a player's read-ahead).
#[test]
fn dsf_tag_walk_starts_no_read_ahead() {
    let tree = Tree::with_options(
        SourceDir::scan(&corpus().join("images")).unwrap(),
        sacdfs::disc::Tuning::default(),
        Output::dsf_only(),
    );
    let (walked, _) = walk(&tree);
    assert!(walked > 100, "{walked} files");
}

/// [`dsf_tag_walk_starts_no_read_ahead`] on real discs, where a player's read-ahead starts,
/// and a scanner's read of a file's head (Roon: 768 KiB in 128 KiB reads, #78) starts none.
/// `SACDFS_REAL_IMAGES=DIR cargo test --release --test dsf_header -- --ignored`.
#[test]
#[ignore = "needs real disc images: set SACDFS_REAL_IMAGES"]
fn real_discs_head_reads() {
    let Some(dir) = std::env::var_os("SACDFS_REAL_IMAGES") else { return };
    let tree = Tree::with_options(
        SourceDir::scan(std::path::Path::new(&dir)).unwrap(),
        sacdfs::disc::Tuning::default(),
        Output::dsf_only(),
    );
    let (walked, continued) = walk(&tree);
    assert!(walked > 0 && continued > 0, "{walked} files, {continued} read ahead");
    let engine = Engine::new(2, 0).unwrap();
    for (path, f) in tree.all_files() {
        let reader = Reader::new(f.clone());
        let page = 128 << 10;
        for off in (0..768 << 10).step_by(page as usize) {
            let (_, ahead) = reader.read(&engine, off, page).unwrap();
            assert!(ahead.is_none(), "{path:?}: read-ahead after the head read at {off}");
        }
    }
}

/// Walks every file of `tree` as a tagger and then as a player; returns the files walked and
/// how many of them started read-ahead, which must be at [`HEAD_WINDOW`] and not before.
fn walk(tree: &Tree) -> (usize, usize) {
    let engine = Engine::new(2, 0).unwrap();
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
        // A player goes on reading, 32 KiB at a time: the first read past the head window
        // (#78) starts read-ahead, none before it does.
        let reader = Reader::new(f.clone());
        let step = 32 << 10;
        let mut off = 0;
        while off < f.data_end() {
            let (_, ahead) = reader.read(&engine, off, step).unwrap();
            if ahead.is_some() {
                assert!(off >= HEAD_WINDOW, "{path:?}: read-ahead at {off}");
                continued += usize::from(off == HEAD_WINDOW);
                break;
            }
            off += step;
        }
    }
    (walked, continued)
}

/// The bytes of a DSF file from scarletbook's layout without decoded frames: the header and
/// tag are the file's, and audio not given is zeros (as a failed read leaves it), never a
/// panic; a range past the end is empty.
#[test]
fn dsf_bytes_without_frames() {
    let tree = Tree::with_options(
        SourceDir::scan(&corpus().join("images/dsd-stereo-mch-2048.iso")).unwrap(),
        sacdfs::disc::Tuning::default(),
        Output::dsf_only(),
    );
    for (path, f) in tree.all_files() {
        let h = f.header_len();
        let head = f.assemble(0, h + 8192, 0, &[]);
        assert_eq!(&head[..4], b"DSD ", "{path:?}");
        assert_eq!(u64::from_le_bytes(head[12..20].try_into().unwrap()), f.size, "{path:?}: total size");
        assert!(head[h as usize..].iter().all(|&b| b == 0), "{path:?}: audio not given is zeros");
        let tail = f.assemble(f.data_end(), f.size, 0, &[]);
        assert_eq!(&tail[..], f.tag(), "{path:?}: the tag");
        assert!(f.assemble(f.size, 10, 0, &[]).is_empty());
        assert_eq!(f.frames_for(0, h), (0, 0), "{path:?}: the header needs no frame");
    }
}
