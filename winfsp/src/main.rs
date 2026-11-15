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
    let args = options::Args::parse();
    let level = match args.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let mut logger = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level));
    if let Some(path) = &args.log_file {
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(f) => {
                logger.target(env_logger::Target::Pipe(Box::new(f)));
            }
            Err(e) => {
                eprintln!("sacdfs-win: --log-file {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        }
    }
    // Names from SOURCE in the log are escaped where lines are written, as on Linux.
    if let Err(e) = sacdfs::printable::install(logger.build()) {
        eprintln!("sacdfs-win: {e}");
    }
    match mount(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sacdfs-win: {e}");
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
