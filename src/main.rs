// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs SOURCE MOUNTPOINT`: mount SACD disc images as DSF and WAV files. The mount is Linux
//! FUSE (`linux_main`); other systems get a message until their front end exists (#420).

#[cfg(target_os = "linux")]
#[path = "linux_main.rs"]
mod linux_main;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux_main::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("sacdfs: mounting needs Linux FUSE; see #420 for macOS (sacdfs-nfs, a spike)");
    std::process::ExitCode::from(2)
}
