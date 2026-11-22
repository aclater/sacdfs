// SPDX-License-Identifier: GPL-3.0-or-later

//! The NFS server's decoding (#420): what any local program can send to `sacdfs-nfs`. Every
//! input must be answered or refused without a panic, an allocation its size does not
//! justify, or a hang.
//!
//! Input: a byte choosing the path, then the bytes:
//! - 0: one RPC call (a record's content) to the server's dispatcher, over the corpus image
//!   `dsd-stereo-mch-2048.iso`, every caller answered;
//! - 1: a byte stream read as records (record marking), as a connection's are;
//! - 2: a file handle, decoded.

#![no_main]

use std::io::Cursor;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::nfs::proto::context::RPCContext;
use sacdfs::nfs::proto::rpc::auth_unix;
use sacdfs::nfs::proto::rpcwire::{handle_rpc, read_record};
use sacdfs::nfs::proto::transaction_tracker::TransactionTracker;
use sacdfs::nfs::{Callers, NfsFront, handle::Handle};
use sacdfs::tree::{Output, SourceDir, Tree};
use sacdfs::vfs::Volume;
use sacdfs_fuzz::scratch_dir;

struct Server {
    runtime: tokio::runtime::Runtime,
    context: RPCContext,
}

fn server() -> &'static Server {
    static SERVER: OnceLock<Server> = OnceLock::new();
    SERVER.get_or_init(|| {
        let dir = scratch_dir("nfs_rpc");
        let image = include_bytes!("../../tests/corpus/images/dsd-stereo-mch-2048.iso");
        std::fs::write(dir.join("disc.iso"), image).unwrap_or_else(|e| panic!("{e}"));
        let source = SourceDir::scan(&dir).unwrap_or_else(|e| panic!("{e}"));
        let tree = Arc::new(Tree::with_options(source, Tuning::default(), Output::default()));
        let engine = Arc::new(Engine::new(2, 8 << 20).unwrap_or_else(|e| panic!("{e}")));
        let volume = Volume::new(tree, engine, 4, 1000, 1000)
            .and_then(|v| v.with_request_timeout(Some(Duration::from_secs(10))))
            .unwrap_or_else(|e| panic!("{e}"));
        let front = NfsFront::new(Arc::new(volume), 7, Callers::Anyone);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        let context = RPCContext {
            local_port: 2049,
            client_addr: "127.0.0.1:700".to_owned(),
            auth: auth_unix::default(),
            vfs: Arc::new(front),
            mount_signal: None,
            export_name: Arc::new("/".to_owned()),
            // Shared by every input on purpose: it is how the target reaches the retransmission
            // branch and the tracker's MAX_TRACKED housekeeping.
            transaction_tracker: Arc::new(TransactionTracker::new(Duration::from_secs(60))),
        };
        Server { runtime, context }
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&path, rest)) = data.split_first() else { return };
    match path % 3 {
        0 => {
            let s = server();
            let mut out = Vec::new();
            let _ = s.runtime.block_on(handle_rpc(&mut Cursor::new(rest), &mut out, s.context.clone()));
        }
        1 => {
            let s = server();
            s.runtime.block_on(async {
                let mut input = rest;
                let mut buf = Vec::new();
                while let Ok(true) = read_record(&mut input, &mut buf).await {
                    assert!(buf.len() <= sacdfs::nfs::proto::rpcwire::MAX_RECORD);
                }
            });
        }
        _ => {
            if let Some(h) = Handle::decode(rest) {
                assert_eq!(Handle::decode(&h.encode()), Some(h));
            }
        }
    }
});
