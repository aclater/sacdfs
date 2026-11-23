// SPDX-License-Identifier: GPL-3.0-or-later
//! The manual pages `sacdfs(1)` and `sacdfs-nfs(1)`, generated from the command lines (#581),
//! so that they cannot drift from `--help`. Built only with the `man` feature, by
//! `sacdfs --generate-man-pages DIR`; the pages are committed under `man/`, and CI builds them
//! again and compares (`.github/scripts/check-man.sh`).
//!
//! `--help` leaves out the tuning options (#560); a manual lists them, so the options hidden
//! from `--help` are shown here, except the deprecated spellings, which have a section of their
//! own in the text after the generated sections (`man/sections/`).

use std::io;
use std::path::Path;

use clap::Command;

/// Options (by id, their field names) that stay out of the generated OPTIONS section: the deprecated spellings.
const DEPRECATED: &[&str] = &["lookup_name", "lookup_durations", "no_allow_other", "no_index_cache"];

/// The page of `cmd`: name, synopsis, description and options from the command line, then the
/// hand-written sections `extra` (roff).
///
/// The date and version are left out of the header: a page that names the release would change
/// at every release, and the check that it is current would fail on each bump.
pub fn page(cmd: Command, extra: &str) -> io::Result<Vec<u8>> {
    let cmd = cmd
        .mut_args(|a| {
            let deprecated = DEPRECATED.contains(&a.get_id().as_str());
            a.hide(deprecated)
        })
        .after_help(None::<&str>);
    let man = clap_mangen::Man::new(cmd).date("").source("sacdfs").manual("sacdfs");
    let mut out = Vec::new();
    man.render_title(&mut out)?;
    man.render_name_section(&mut out)?;
    man.render_synopsis_section(&mut out)?;
    man.render_description_section(&mut out)?;
    man.render_options_section(&mut out)?;
    out.extend_from_slice(extra.as_bytes());
    Ok(out)
}

/// Writes `sacdfs.1` and, with the `nfs` feature, `sacdfs-nfs.1` into `dir`. `sacdfs` is the
/// command of the Linux mount.
pub fn write_all(sacdfs: Command, dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("sacdfs.1"), page(sacdfs, include_str!("../man/sections/sacdfs.roff"))?)?;
    #[cfg(feature = "nfs")]
    {
        use clap::CommandFactory;
        let nfs = crate::nfs::cli::Args::command().name("sacdfs-nfs");
        std::fs::write(
            dir.join("sacdfs-nfs.1"),
            page(nfs, include_str!("../man/sections/sacdfs-nfs.roff"))?,
        )?;
    }
    Ok(())
}
