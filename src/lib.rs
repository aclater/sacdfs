// SPDX-License-Identifier: GPL-3.0-or-later

//! sacdfs: a read-only FUSE filesystem that shows the audio of Super Audio CD disc images
//! as DSF files, decoded on demand by [scarletbook].
//!
//! The library holds everything but the command line, so that tests can read the files
//! without mounting: [`tree::Tree`] is the directory tree, [`track::Reader`] reads a byte
//! range of one file with an [`engine::Engine`], and [`fs::SacdFs`] answers FUSE requests.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod disc;
pub mod dsf;
pub mod engine;
pub mod fs;
pub mod index_cache;
pub mod names;
pub mod track;
pub mod tree;
