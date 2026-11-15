// SPDX-License-Identifier: BSD-3-Clause
// From nfsserve 0.11.0 (Copyright (c) 2023, XetData; Copyright (c) 2025, Hugging Face), with
// sacdfs's changes: see mod.rs and this folder's LICENSE.

use std::collections::{HashMap, hash_map};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// `TransactionTracker` tracks the state of transactions to detect retransmissions.
#[derive(Debug)]
pub struct TransactionTracker {
    retention_period: Duration,
    transactions: Mutex<HashMap<(u32, String), TransactionState>>,
}

impl TransactionTracker {
    pub fn new(retention_period: Duration) -> Self {
        Self { retention_period, transactions: Mutex::new(HashMap::new()) }
    }

    /// Checks if the transaction is a retransmission.
    /// If it's a new transaction, it is marked as `InProgress`.
    ///
    /// Returns `true` if the transaction is a retransmission, `false` otherwise.
    pub fn is_retransmission(&self, xid: u32, client_addr: &str) -> bool {
        let key = (xid, client_addr.to_string());
        let mut transactions = self.transactions.lock().unwrap_or_else(|p| p.into_inner());
        housekeeping(&mut transactions, self.retention_period);
        if let hash_map::Entry::Vacant(e) = transactions.entry(key) {
            e.insert(TransactionState::InProgress);
            false
        } else {
            true
        }
    }

    /// Marks the transaction as processed.
    pub fn mark_processed(&self, xid: u32, client_addr: &str) {
        let key = (xid, client_addr.to_string());
        let completion_time = SystemTime::now();
        let mut transactions = self.transactions.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(tx) = transactions.get_mut(&key) {
            *tx = TransactionState::Completed(completion_time);
        }
    }
}

/// sacdfs: at most this many calls are remembered; past it, the completed ones are forgotten,
/// then all, so that a client sending new xids fast cannot grow the table without bound.
const MAX_TRACKED: usize = 8192;

fn housekeeping(transactions: &mut HashMap<(u32, String), TransactionState>, max_age: Duration) {
    let cutoff = SystemTime::now().checked_sub(max_age).unwrap_or(SystemTime::UNIX_EPOCH);
    transactions.retain(|_, v| match v {
        TransactionState::InProgress => true,
        TransactionState::Completed(completion_time) => *completion_time >= cutoff,
    });
    if transactions.len() >= MAX_TRACKED {
        transactions.retain(|_, v| matches!(v, TransactionState::InProgress));
    }
    if transactions.len() >= MAX_TRACKED {
        transactions.clear();
    }
}

#[derive(Debug)]
pub enum TransactionState {
    InProgress,
    Completed(SystemTime),
}
