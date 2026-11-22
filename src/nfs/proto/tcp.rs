// SPDX-License-Identifier: BSD-3-Clause
// From nfsserve 0.11.0 (Copyright (c) 2023, XetData; Copyright (c) 2025, Hugging Face): see
// mod.rs and this folder's LICENSE.
// sacdfs: rewritten from nfsserve's tcp.rs. Each connection reads whole records (bounded,
// `rpcwire::read_record`), answers at most `IN_FLIGHT` calls at a time (and all connections
// `ALL_IN_FLIGHT`), and writes the replies from one task, which gives up on a client that
// doesn't read them; a connection that sends nothing is closed; the listener survives a
// failed accept (out of descriptors). nfsserve's `auto` addresses (127.88.x.x) are gone:
// sacdfs binds where it is told.
// sacdfs: a call holds the permit of all connections only while it is answered, and the
// replies of one connection being made or waiting to be written are bounded (`UNWRITTEN`);
// while a new connection waits for a place, a connection with nothing outstanding that stays
// silent for `IDLE_WHEN_FULL` is closed. The bounds can be set smaller (`Limits`,
// `NFSTcpListener::with_limits`) for tests.

use std::io::{self, Cursor};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use log::{debug, info, warn};
use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use super::context::RPCContext;
use super::rpcwire::{handle_rpc, read_record, write_fragment};
use super::transaction_tracker::TransactionTracker;
use super::vfs::NFSFileSystem;

/// Calls read from one connection and not answered yet; the next record is read when one is
/// done. Each holds its record, at most [`super::rpcwire::MAX_RECORD`].
pub const IN_FLIGHT: usize = 16;

/// Calls being answered at once over all connections. A call holds its permit only while the
/// core answers it, never while its reply waits to be written.
pub const ALL_IN_FLIGHT: usize = 128;

/// Calls of one connection being answered, or answered with their replies not written yet. A
/// reply is up to 1 MiB (a READ), so with [`MAX_CONNECTIONS`] this bounds the memory replies
/// that clients don't read can hold: 64 x 4 MiB.
pub const UNWRITTEN: usize = 4;

/// Connections served at once; more wait to be accepted.
pub const MAX_CONNECTIONS: usize = 64;

/// While a new connection waits for a place, a connection with no call outstanding that stays
/// silent this long is closed to make room (else after [`IDLE_CONNECTION`]).
pub const IDLE_WHEN_FULL: Duration = Duration::from_secs(10);

/// How long a new connection may take to send its first call, and an open one may stay
/// silent, before it is closed. NFS clients connect again when they need to.
pub const FIRST_CALL: Duration = Duration::from_secs(10);
pub const IDLE_CONNECTION: Duration = Duration::from_secs(300);

/// How long one reply may take to be written before the connection is closed: a client that
/// doesn't read its replies.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// The bounds a listener serves with: [`Limits::default`] is the constants above; tests set
/// smaller ones ([`NFSTcpListener::with_limits`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// [`IN_FLIGHT`].
    pub in_flight: usize,
    /// [`UNWRITTEN`].
    pub unwritten: usize,
    /// [`ALL_IN_FLIGHT`].
    pub all_in_flight: usize,
    /// [`MAX_CONNECTIONS`].
    pub max_connections: usize,
    /// [`IDLE_WHEN_FULL`].
    pub idle_when_full: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            in_flight: IN_FLIGHT,
            unwritten: UNWRITTEN,
            all_in_flight: ALL_IN_FLIGHT,
            max_connections: MAX_CONNECTIONS,
            idle_when_full: IDLE_WHEN_FULL,
        }
    }
}

/// The NFS server's listening socket.
pub struct NFSTcpListener<T: NFSFileSystem + Send + Sync + 'static> {
    listener: TcpListener,
    port: u16,
    arcfs: Arc<T>,
    export_name: Arc<String>,
    transaction_tracker: Arc<TransactionTracker>,
    limits: Limits,
}

impl<T: NFSFileSystem + Send + Sync + 'static> std::fmt::Debug for NFSTcpListener<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NFSTcpListener").field("listener", &self.listener).finish_non_exhaustive()
    }
}

/// What every connection shares.
#[derive(Clone)]
struct Shared {
    /// Permits of calls being answered, over all connections ([`ALL_IN_FLIGHT`]).
    all: Arc<Semaphore>,
    /// Set while a new connection waits for a place, until one idle connection takes it to
    /// close ([`IDLE_WHEN_FULL`]).
    waiting: Arc<AtomicBool>,
    limits: Limits,
}

/// A reply, and the connection's [`UNWRITTEN`] permit it holds until it is written.
type Reply = (Vec<u8>, OwnedSemaphorePermit);

/// Answers one call, read from the connection as `record`, and queues its reply for the
/// connection's writer. `slot` (one of [`IN_FLIGHT`]) is given back once the call is answered.
/// The call first takes one of the connection's [`UNWRITTEN`] permits, which its reply holds
/// until written, then one of all connections' ([`ALL_IN_FLIGHT`]), given back as soon as the
/// core has answered: a reply waiting for a client that doesn't read it holds only its own
/// connection's permit.
async fn answer(
    record: Vec<u8>,
    context: RPCContext,
    tx: mpsc::Sender<Reply>,
    slot: OwnedSemaphorePermit,
    unwritten: Arc<Semaphore>,
    all: Arc<Semaphore>,
) {
    let Ok(pending) = unwritten.acquire_owned().await else { return };
    if tx.is_closed() {
        // The writer gave up on the client: nothing would be sent.
        return;
    }
    let Ok(global) = all.acquire_owned().await else { return };
    let mut out = Vec::new();
    let answered = handle_rpc(&mut Cursor::new(record), &mut out, context).await;
    drop((global, slot));
    match answered {
        // The channel has room for every permit: this never waits.
        Ok(true) => {
            let _ = tx.send((out, pending)).await;
        }
        Ok(false) => {}
        // A call that does not decode gets no reply, as from nfsserve.
        Err(e) => debug!("call: {e}"),
    }
}

/// Returns when the connection should make room for a new one: a new connection waits for a
/// place, and this one has had no call outstanding for [`Limits::idle_when_full`]. Never
/// returns while the connection is answering or writing.
async fn crowded(shared: &Shared, slots: &Semaphore, unwritten: &Semaphore) {
    let idle = || {
        slots.available_permits() == shared.limits.in_flight
            && unwritten.available_permits() == shared.limits.unwritten
    };
    let mut quiet = Duration::ZERO;
    let step = shared.limits.idle_when_full / 4;
    loop {
        tokio::time::sleep(step).await;
        quiet = if idle() { quiet + step } else { Duration::ZERO };
        // One connection closes per wait: it takes the flag, and the place it frees goes to the
        // waiting connection (only the listener takes places).
        if quiet >= shared.limits.idle_when_full
            && shared.waiting.compare_exchange(true, false, Ordering::Relaxed, Ordering::Relaxed).is_ok()
        {
            return;
        }
    }
}

/// Serves one connection until the client closes it or sends what is not a record.
async fn process_socket(socket: TcpStream, context: RPCContext, shared: Shared) {
    let limits = shared.limits;
    let _ = socket.set_nodelay(true);
    let (rd, mut wr) = socket.into_split();
    let mut rd = BufReader::with_capacity(64 * 1024, rd);
    // Room for every reply the connection's permits allow: a reply never waits to be queued.
    let (tx, mut rx) = mpsc::channel::<Reply>(limits.unwritten);
    let peer = context.client_addr.clone();
    let writer = tokio::spawn(async move {
        while let Some((reply, pending)) = rx.recv().await {
            let written = tokio::time::timeout(WRITE_TIMEOUT, write_fragment(&mut wr, &reply)).await;
            drop((reply, pending));
            match written {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    debug!("write to {peer}: {e}");
                    break;
                }
                Err(_) => {
                    warn!("{peer} does not read its replies: closing the connection");
                    break;
                }
            }
        }
    });
    let slots = Arc::new(Semaphore::new(limits.in_flight));
    let unwritten = Arc::new(Semaphore::new(limits.unwritten));
    let mut wait = FIRST_CALL;
    loop {
        let mut record = Vec::new();
        // The writer gone (a client that doesn't read) closes the connection too.
        let next = tokio::select! {
            r = tokio::time::timeout(wait, read_record(&mut rd, &mut record)) => r,
            _ = tx.closed() => break,
            _ = crowded(&shared, &slots, &unwritten) => {
                debug!("connection from {}: idle while another waits: closed", context.client_addr);
                break;
            }
        };
        match next {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => break,
            Ok(Err(e)) => {
                debug!("connection from {}: {e}", context.client_addr);
                break;
            }
            Err(_) => {
                debug!("connection from {}: silent for {} s: closed", context.client_addr, wait.as_secs());
                break;
            }
        }
        wait = IDLE_CONNECTION;
        let Ok(slot) = slots.clone().acquire_owned().await else { break };
        tokio::spawn(answer(
            record,
            context.clone(),
            tx.clone(),
            slot,
            unwritten.clone(),
            shared.all.clone(),
        ));
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
            limits: Limits::default(),
        })
    }

    /// Serves with `limits` instead of the defaults (each count at least 1, the idle time at
    /// least 4 ms).
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = Limits {
            in_flight: limits.in_flight.max(1),
            unwritten: limits.unwritten.max(1),
            all_in_flight: limits.all_in_flight.max(1),
            max_connections: limits.max_connections.max(1),
            idle_when_full: limits.idle_when_full.max(Duration::from_millis(4)),
        };
        self
    }

    /// The address listened on: the port the system chose for port 0.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves connections until the task is dropped.
    pub async fn handle_forever(&self) -> io::Result<()> {
        let limits = self.limits;
        let connections = Arc::new(Semaphore::new(limits.max_connections));
        let shared = Shared {
            all: Arc::new(Semaphore::new(limits.all_in_flight)),
            waiting: Arc::new(AtomicBool::new(false)),
            limits,
        };
        loop {
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
            // Accepted before it has a place, so that idle connections know one waits; more
            // wait in the listen backlog meanwhile.
            let slot = match connections.clone().try_acquire_owned() {
                Ok(slot) => slot,
                Err(_) => {
                    shared.waiting.store(true, Ordering::Relaxed);
                    let Ok(slot) = connections.clone().acquire_owned().await else { return Ok(()) };
                    shared.waiting.store(false, Ordering::Relaxed);
                    slot
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
            let shared = shared.clone();
            tokio::spawn(async move {
                process_socket(socket, context, shared).await;
                drop(slot);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A context over a file system on an empty folder (`dir` must outlive it).
    fn context(dir: &std::path::Path) -> RPCContext {
        use crate::nfs::{Callers, NfsFront, handle};
        use crate::tree::{Output, SourceDir, Tree};
        let src = dir.canonicalize().unwrap();
        let tree = Arc::new(Tree::with_options(
            SourceDir::scan(&src).unwrap(),
            crate::disc::Tuning::default(),
            Output::default(),
        ));
        let engine = Arc::new(crate::engine::Engine::new(1, 1 << 20).unwrap());
        let volume = Arc::new(crate::vfs::Volume::new(tree, engine, 1, 1000, 1000).unwrap());
        let front = NfsFront::new(volume, handle::volume_id(&src), Callers::User(1000));
        RPCContext {
            local_port: 0,
            client_addr: "127.0.0.1:1".to_string(),
            auth: super::super::rpc::auth_unix::default(),
            vfs: Arc::new(front),
            mount_signal: None,
            export_name: Arc::from("/".to_string()),
            transaction_tracker: Arc::new(TransactionTracker::new(Duration::from_secs(60))),
        }
    }

    /// An NFS NULL call (procedure 0) with AUTH_NONE, as a record's body.
    fn null_call(xid: u32) -> Vec<u8> {
        [xid, 0, 2, 100_003, 3, 0, 0, 0, 0, 0].iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// A reply the writer has not taken holds its connection's permit, never the permit of all
    /// connections; the connection's next call waits for that permit before it is answered,
    /// without taking one of all connections' either.
    #[tokio::test]
    async fn an_unwritten_reply_holds_only_its_connection() {
        let dir = tempfile::tempdir().unwrap();
        let all = Arc::new(Semaphore::new(1));
        let slots = Arc::new(Semaphore::new(2));
        let unwritten = Arc::new(Semaphore::new(1));
        // A writer that takes nothing yet.
        let (tx, mut rx) = mpsc::channel::<Reply>(1);
        let call = |xid| {
            let slot = slots.clone().try_acquire_owned().unwrap();
            tokio::spawn(answer(
                null_call(xid),
                context(dir.path()),
                tx.clone(),
                slot,
                unwritten.clone(),
                all.clone(),
            ))
        };
        let first = call(7);
        tokio::time::timeout(Duration::from_secs(10), first).await.unwrap().unwrap();
        assert_eq!(all.available_permits(), 1, "the reply waits without the permit of all connections");
        assert_eq!(slots.available_permits(), 2, "the call is answered");
        assert_eq!(unwritten.available_permits(), 0, "the reply holds its connection's permit");
        let second = call(8);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!second.is_finished(), "the next call waits for the reply to be written");
        assert_eq!(all.available_permits(), 1, "and does not hold the permit of all connections");
        // The writer takes the first reply and writes it: the second is answered.
        let (reply, pending) = rx.recv().await.unwrap();
        assert_eq!(reply[..4], 7u32.to_be_bytes(), "the reply to the first call");
        drop(pending);
        tokio::time::timeout(Duration::from_secs(10), second).await.unwrap().unwrap();
        let (reply, _pending) = rx.recv().await.unwrap();
        assert_eq!(reply[..4], 8u32.to_be_bytes());
        assert_eq!(unwritten.available_permits(), 0);
        assert_eq!((all.available_permits(), slots.available_permits()), (1, 2));
    }
}
