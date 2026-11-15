// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs-nfs SOURCE [MOUNTPOINT]` (#420): serves SOURCE's disc images as DSF and WAV files
//! over NFSv3, on 127.0.0.1 by default, for the system's own NFS client: on macOS, where it
//! is how sacdfs mounts, and on Linux for NFS clients without FUSE. With MOUNTPOINT it also
//! mounts there, and unmounts when stopped. See [`sacdfs::nfs::cli`] and docs/install/macos.md.

fn main() -> std::process::ExitCode {
    sacdfs::nfs::cli::main(std::env::args_os())
}
