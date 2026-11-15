// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs-win`: sacdfs's Windows front end on WinFsp (#419). **Spike, not for release.**
//!
//! ```text
//! sacdfs-win SOURCE MOUNTPOINT [--threads N] [--cache-mb N] [--kernel-cache] [--pcm-rate HZ]
//! ```
//!
//! MOUNTPOINT is a drive letter (`X:`) or a folder that does not exist yet. It shows SOURCE's
//! discs as `sacdfs` on Linux does with its defaults (DSF and WAV, WAV at 176.4 kHz), read-only,
//! and runs until it is stopped (Ctrl+C, or the process ends).

// Used by the Windows front end only; its tests run everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
mod names;
#[cfg(windows)]
mod winfs;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    winfs::main()
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("sacdfs-win runs on Windows only");
    std::process::ExitCode::FAILURE
}
