// SPDX-License-Identifier: BSD-3-Clause
// From nfsserve 0.11.0 (Copyright (c) 2023, XetData; Copyright (c) 2025, Hugging Face): see
// mod.rs and this folder's LICENSE.
// sacdfs: rewritten from nfsserve's tcp.rs. Each connection reads whole records (bounded,
// `rpcwire::read_record`), answers at most `IN_FLIGHT` calls at a time and writes the replies
// from one task; the listener survives a failed accept (out of descriptors). nfsserve's
// `auto` addresses (127.88.x.x) are gone: sacdfs binds where it is told.

use std::io::{self, Cursor};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use log::{debug, info, warn};
use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};

use super::context::RPCContext;
use super::rpcwire::{handle_rpc, read_record, write_fragment};
use super::transaction_tracker::TransactionTracker;
use super::vfs::NFSFileSystem;

/// Calls answered at once per connection; the next record is read when one is done.
pub const IN_FLIGHT: usize = 64;

/// Connections served at once; more wait to be accepted.
pub const MAX_CONNECTIONS: usize = 64;

/// The NFS server's listening socket.
pub struct NFSTcpListener<T: NFSFileSystem + Send + Sync + 'static> {
    listener: TcpListener,
    port: u16,
    arcfs: Arc<T>,
    export_name: Arc<String>,
    transaction_tracker: Arc<TransactionTracker>,
}

impl<T: NFSFileSystem + Send + Sync + 'static> std::fmt::Debug for NFSTcpListener<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NFSTcpListener").field("listener", &self.listener).finish_non_exhaustive()
    }
}

/// Serves one connection until the client closes it or sends what is not a record.
async fn process_socket(socket: TcpStream, context: RPCContext) {
    let _ = socket.set_nodelay(true);
    let (rd, mut wr) = socket.into_split();
    let mut rd = BufReader::with_capacity(64 * 1024, rd);
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(IN_FLIGHT);
    let writer = tokio::spawn(async move {
        while let Some(reply) = rx.recv().await {
            if let Err(e) = write_fragment(&mut wr, &reply).await {
                debug!("write: {e}");
                break;
            }
        }
    });
    let slots = Arc::new(Semaphore::new(IN_FLIGHT));
    loop {
        let mut record = Vec::new();
        match read_record(&mut rd, &mut record).await {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                debug!("connection from {}: {e}", context.client_addr);
                break;
            }
        }
        let Ok(slot) = slots.clone().acquire_owned().await else { break };
        let (context, tx) = (context.clone(), tx.clone());
        tokio::spawn(async move {
            let mut out = Vec::new();
            match handle_rpc(&mut Cursor::new(record), &mut out, context).await {
                Ok(true) => {
                    let _ = tx.send(out).await;
                }
                Ok(false) => {}
                // A call that does not decode gets no reply, as from nfsserve.
                Err(e) => debug!("call: {e}"),
            }
            drop(slot);
        });
    }
    drop(tx);
    let _ = writer.await;
}

impl<T: NFSFileSystem + Send + Sync + 'static> NFSTcpListener<T> {
    /// Listens on `addr` and serves `fs` there.
    pub async fn bind(addr: SocketAddr, fs: T) -> io::Result<NFSTcpListener<T>> {
        let listener = TcpListener::bind(addr).await?;
        let port = listener.local_addr()?.port();
        Ok(NFSTcpListener {
            listener,
            port,
            arcfs: Arc::new(fs),
            export_name: Arc::from("/".to_string()),
            transaction_tracker: Arc::new(TransactionTracker::new(Duration::from_secs(60))),
        })
    }

    /// The address listened on: the port the system chose for port 0.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves connections until the task is dropped.
    pub async fn handle_forever(&self) -> io::Result<()> {
        let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        loop {
            let Ok(slot) = connections.clone().acquire_owned().await else { return Ok(()) };
            let (socket, peer) = match self.listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    // Out of descriptors, or a connection reset before it was accepted: the
                    // listener stays.
                    warn!("accept: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let context = RPCContext {
                local_port: self.port,
                client_addr: peer.to_string(),
                auth: super::rpc::auth_unix::default(),
                vfs: self.arcfs.clone(),
                mount_signal: None,
                export_name: self.export_name.clone(),
                transaction_tracker: self.transaction_tracker.clone(),
            };
            info!("connection from {}", context.client_addr);
            tokio::spawn(async move {
                process_socket(socket, context).await;
                drop(slot);
            });
        }
    }
}
