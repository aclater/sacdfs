// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs SOURCE MOUNTPOINT`: mount SACD disc images as DSF and WAV files. The subcommands
//! (`verify`, `tags`) run on every system; the mount is Linux FUSE (`linux_main`), and other
//! systems have front ends of their own (#419, #420).

use std::process::ExitCode;

#[cfg(target_os = "linux")]
mod linux_main;
#[cfg(target_os = "linux")]
mod mount_opts;
#[cfg(target_os = "linux")]
mod startup;
#[cfg(target_os = "linux")]
mod stop_signals;

fn main() -> ExitCode {
    // `sacdfs verify ...` (#80) has options of its own: see `sacdfs verify --help`.
    if std::env::args_os().nth(1).is_some_and(|a| a == "verify") {
        return sacdfs::verify::main(std::env::args_os().skip(1));
    }
    // `sacdfs tags ...` (#81): see `sacdfs tags --help`.
    if std::env::args_os().nth(1).is_some_and(|a| a == "tags") {
        return sacdfs::tags_cli::main(std::env::args_os().skip(1));
    }
    mount()
}

#[cfg(target_os = "linux")]
fn mount() -> ExitCode {
    linux_main::main()
}

/// Elsewhere this binary only runs the subcommands, and says its version: the mount is the
/// platform's own front end.
#[cfg(not(target_os = "linux"))]
fn mount() -> ExitCode {
    // As clap's `--version` prints it on Linux.
    if std::env::args_os().nth(1).is_some_and(|a| a == "--version" || a == "-V") {
        println!("sacdfs {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    eprintln!(
        "sacdfs: this build mounts only on Linux; `sacdfs verify` and `sacdfs tags` work here \
         (see `sacdfs verify --help`, `sacdfs tags --help`)"
    );
    ExitCode::from(2)
}
