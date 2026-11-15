// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs-nfs SOURCE [PORT]` (#420, spike): serves SOURCE's disc images as DSF and WAV files
//! over NFSv3 on 127.0.0.1:PORT (default 11111), for the local NFS client to mount, on macOS:
//!
//! ```text
//! mount_nfs -o vers=3,tcp,port=11111,mountport=11111,nolocks,rdonly localhost:/ MOUNTPOINT
//! ```
//!
//! It runs in the foreground until stopped. Defaults only: no options, no index cache, no
//! tags, no sidecars.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use sacdfs::engine::{Engine, default_threads};
use sacdfs::nfs::{IDLE, NfsFront};
use sacdfs::tree::{SourceDir, Tree};
use sacdfs::vfs::Volume;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut args = std::env::args_os().skip(1);
    let Some(source) = args.next().map(PathBuf::from) else {
        eprintln!("usage: sacdfs-nfs SOURCE [PORT]");
        return ExitCode::from(2);
    };
    let port: u16 = match args.next().map(|p| p.to_string_lossy().parse()) {
        None => 11111,
        Some(Ok(p)) => p,
        Some(Err(e)) => {
            eprintln!("sacdfs-nfs: port: {e}");
            return ExitCode::from(2);
        }
    };
    match serve(source, port) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sacdfs-nfs: {e}");
            ExitCode::FAILURE
        }
    }
}

fn serve(source: PathBuf, port: u16) -> std::io::Result<()> {
    let threads = default_threads();
    let tree = Arc::new(Tree::new(SourceDir::scan(&source)?));
    let engine = Arc::new(Engine::new(threads, 256 << 20)?);
    engine.start_idle_release()?;
    let volume = Arc::new(Volume::new(tree, engine, threads, uid(), gid())?);
    let front = NfsFront::new(volume);
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    runtime.block_on(async move {
        let closer = front.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(IDLE / 4).await;
                closer.close_idle();
            }
        });
        let listener = NFSTcpListener::bind(&format!("127.0.0.1:{port}"), front).await?;
        log::info!("serving {} on 127.0.0.1:{}", source.display(), listener.get_listen_port());
        tokio::select! {
            r = listener.handle_forever() => r,
            _ = tokio::signal::ctrl_c() => Ok(()),
        }
    })?;
    // Give replies in flight a moment.
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}

fn uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

fn gid() -> u32 {
    nix::unistd::getgid().as_raw()
}
