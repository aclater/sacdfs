// SPDX-License-Identifier: GPL-3.0-or-later

//! What a full listing holds on the heap (#237): walks SOURCE as a mount does, lists every
//! directory and makes every file's node, then reports the process's memory.
//!
//! ```text
//! cargo run --release --example listing_mem -- SOURCE [--dsf-only]
//! RUSTFLAGS='--cfg heap_dhat' cargo run --release --example listing_mem -- SOURCE
//! ```
//!
//! Built with `--cfg heap_dhat`, dhat-rs writes every allocation site to `dhat-heap.json`
//! while the tree is still alive, so the "end" figures of each site are what the listing holds.
//! Without it, the example prints glibc's own accounting, so that dhat's requested bytes can be
//! compared with what the allocator holds for them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout, clippy::print_stderr)]
#![allow(unexpected_cfgs, unsafe_code)]

use std::path::PathBuf;

use sacdfs::sidecar::Sidecars;
use sacdfs::tree::{Output, SourceDir, Tree};

#[cfg(heap_dhat)]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn status(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    s.lines()
        .find_map(|l| l.strip_prefix(key)?.trim_start_matches(':').trim().strip_suffix(" kB")?.parse().ok())
        .unwrap_or(0)
}

fn report(what: &str) {
    println!(
        "{what}: anon_kib={} rss_kib={} hwm_kib={}",
        status("RssAnon"),
        status("VmRSS"),
        status("VmHWM")
    );
    #[cfg(target_env = "gnu")]
    {
        // SAFETY: mallinfo takes no arguments and only reads the allocator's counters. Not
        // mallinfo2, which the oldest glibc built on doesn't have; its counters wrap at 2 GiB.
        let m = unsafe { libc::mallinfo() };
        println!(
            "{what}: arena={} mmapped={} in_use={} free={} top={}",
            m.arena, m.hblkhd, m.uordblks, m.fordblks, m.keepcost
        );
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let source = PathBuf::from(args.next().expect("SOURCE"));
    let dsf_only = args.next().is_some_and(|a| a == "--dsf-only");
    let output = if dsf_only { Output::dsf_only() } else { Output::default() };
    sacdfs::engine::limit_malloc_arenas();

    #[cfg(heap_dhat)]
    let profiler = dhat::Profiler::builder().trim_backtraces(Some(24)).build();

    report("start");
    let sel = Sidecars::default();
    let root = SourceDir::scan_with(&source, &sel, None).unwrap();
    let images = root.image_count();
    report("walked");
    let tree = Tree::with_options(root, Default::default(), output).watching(&sel);
    report("indexed");
    let files = tree.all_files().len();
    report("listed");
    println!("images={images} files={files} nodes={}", tree.len());

    #[cfg(heap_dhat)]
    drop(profiler);
    std::mem::forget(tree);
}
