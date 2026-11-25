// SPDX-License-Identifier: GPL-3.0-or-later

//! The unmount helper (src/unmount_helper.rs, included here) against the kernel: real mounts
//! (tmpfs) in a private user and mount namespace, so that no root and no lab are needed.
//!
//! The program runs itself again under `unshare -Urm` (uid 0 with all capabilities inside the
//! namespace). Where the namespace cannot be made it prints SKIP and ends 0: the line is in
//! the log, and the lab runs the same program with real privileges.
//!
//! No test harness (`harness = false`): the helper forks before any thread, and libtest has
//! several. What this cannot do: mount FUSE (no /dev/fuse here). The FUSE magic, the dead
//! daemon (ENOTCONN) path and `fusermount3 -u` from outside are lab items (STATUS-harden.md).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code, dead_code)]

#[cfg(target_os = "linux")]
#[path = "../src/unmount_helper.rs"]
mod unmount_helper;

#[cfg(target_os = "linux")]
#[path = "unmount_helper/linux.rs"]
mod linux;

fn main() {
    #[cfg(target_os = "linux")]
    linux::main();
}
