// SPDX-License-Identifier: BSD-3-Clause
// From nfsserve 0.11.0 (Copyright (c) 2023, XetData; Copyright (c) 2025, Hugging Face), with
// sacdfs's changes: see mod.rs and this folder's LICENSE.

use std::fmt;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::transaction_tracker::TransactionTracker;
use super::vfs::NFSFileSystem;

#[derive(Clone)]
pub struct RPCContext {
    pub local_port: u16,
    pub client_addr: String,
    pub auth: super::rpc::auth_unix,
    pub vfs: Arc<dyn NFSFileSystem + Send + Sync>,
    pub mount_signal: Option<mpsc::Sender<bool>>,
    pub export_name: Arc<String>,
    pub transaction_tracker: Arc<TransactionTracker>,
}

impl fmt::Debug for RPCContext {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("RPCContext")
            .field("local_port", &self.local_port)
            .field("client_addr", &self.client_addr)
            .field("auth", &self.auth)
            .finish()
    }
}
