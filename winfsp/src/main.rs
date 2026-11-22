// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs-win`: sacdfs's Windows front end, on WinFsp (#419).
//!
//! ```text
//! sacdfs-win SOURCE MOUNTPOINT [OPTIONS]
//! ```
//!
//! MOUNTPOINT is a drive letter (`X:`) or a folder that does not exist yet. It shows SOURCE's
//! discs as `sacdfs` does on Linux, read-only, until it is stopped: see [`options::Args`].

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

// The modules that need no Windows API build and test everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
mod ahead;
#[cfg_attr(not(windows), allow(dead_code))]
mod names;
#[cfg_attr(not(windows), allow(dead_code))]
mod notice;
#[cfg_attr(not(windows), allow(dead_code))]
mod options;
#[cfg_attr(not(windows), allow(dead_code))]
mod paths;
#[cfg(windows)]
mod run;
#[cfg(windows)]
mod winfs;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let (argv, requoted) = options::args();
    let args = options::Args::parse_from(argv);
    let level = match args.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let mut logger = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level));
    if let Some(path) = &args.log_file {
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(mut f) => {
                // The file is appended to: a line marks where this session starts (#481). A
                // failed write is ignored, as the log's own writes are.
                let _ = sacdfs::errlog::write_session_start(&mut f, "sacdfs-win");
                logger.target(env_logger::Target::Pipe(Box::new(f)));
            }
            Err(e) => {
                eprintln!(
                    "sacdfs-win: --log-file {}: {}",
                    sacdfs::safe_file::shown(path),
                    sacdfs::safe_file::printable(&e.to_string())
                );
                return ExitCode::FAILURE;
            }
        }
    }
    // Names from SOURCE in the log are escaped where lines are written, as on Linux.
    if let Err(e) = sacdfs::printable::install(logger.build()) {
        eprintln!("sacdfs-win: {}", sacdfs::safe_file::printable(&e.to_string()));
    }
    for r in args.renamed() {
        log::warn!("{}", r.message());
    }
    if requoted {
        log::warn!(
            "an argument held a quote, as WinFsp.Launcher makes of a folder ending in a backslash \
             (#466): read the command line again with backslashes kept, as SOURCE {} and MOUNTPOINT {}",
            args.source.display(),
            args.mountpoint
        );
    }
    match mount(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // The error can hold names from SOURCE (#448).
            eprintln!("sacdfs-win: {}", sacdfs::safe_file::printable(&e));
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn mount(args: options::Args) -> Result<(), String> {
    run::main(args)
}

#[cfg(not(windows))]
fn mount(_args: options::Args) -> Result<(), String> {
    Err("sacdfs-win runs on Windows only; on Linux, use sacdfs".to_owned())
}
