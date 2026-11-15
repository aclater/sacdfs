// SPDX-License-Identifier: BSD-3-Clause
//
// The NFSv3 protocol layer: nfsserve 0.11.0 (https://github.com/huggingface/nfsserve),
// Copyright (c) 2023, XetData; Copyright (c) 2025, Hugging Face. BSD-3-Clause: the licence
// is in this folder's LICENSE file, and its notice applies to every file here. sacdfs's own
// changes are under the same licence.

//! The NFSv3, MOUNT and port mapper protocols, from nfsserve 0.11.0 (#420).
//!
//! *Why vendored:* nfsserve answers FSSTAT with a fixed 1 TiB and makes file handles from the
//! server's start time, with no hook for either; a git or path dependency would keep sacdfs
//! off crates.io. See docs/design.md, "The NFS front end".
//!
//! **sacdfs's changes**, each marked `sacdfs:` where it is:
//! - `vfs`: file handles are the file system's (`id_to_fh` and `fh_to_id`, now async and
//!   required, with no `static mut` start time), `serverid` is required, and two methods
//!   are new: `fsstat` (FSSTAT) and `permits` (which credentials are answered).
//! - `rpcwire`: a call whose credentials `permits` refuses gets AUTH_ERROR (procedure 0, the
//!   ping, excepted); records are read with a bound ([`rpcwire::MAX_RECORD`]).
//! - `tcp`: rewritten: bounded calls in flight per connection and connections at once; a
//!   failed accept no longer stops the server.
//! - `xdr`: variable-length data is read as it arrives, never allocated from its declared
//!   length; the macros are not exported from the crate.
//! - `transaction_tracker`: bounded; no panics on a poisoned lock.
//! - `nfs_handlers`: FSSTAT from `fsstat`; PATHCONF says 255-byte names and 1 link;
//!   READDIR(PLUS) limits that underflowed on a small count saturate.
//! - `mount_handlers`: MNT offers AUTH_SYS only.
//! - `tracing` became `log`, `anyhow::Error` became `std::io::Error`, `filetime` was dropped,
//!   and `fs_util` and the `auto` listen addresses were left out.

#![allow(
    unused,
    dead_code,
    non_camel_case_types,
    missing_debug_implementations,
    rust_2018_idioms,
    clippy::all,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

pub mod context;
pub mod mount;
pub mod mount_handlers;
pub mod nfs;
pub mod nfs_handlers;
pub mod portmap;
pub mod portmap_handlers;
pub mod rpc;
pub mod rpcwire;
pub mod tcp;
pub mod transaction_tracker;
pub mod vfs;
pub mod write_counter;
pub mod xdr;
