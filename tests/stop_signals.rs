// SPDX-License-Identifier: GPL-3.0-or-later

//! A stop signal that arrives while sacdfs's handlers go in is not lost (#334): it either
//! ends the process (before the handler) or reaches the handler's `Signals` (after), never
//! neither. Each trial is a fresh copy of this program, as a handler goes in once per
//! process: a thread sends the process SIGTERM 0 to 20 µs after the main thread starts
//! `stop_signals::catch` (src/stop_signals.rs, included here). The other threads have the
//! signals blocked, so that only the main thread can take one, as in sacdfs, where `catch`
//! runs before any other thread exists. A few of them wake every 50 µs, and trials run
//! two per CPU at once: the main thread is then preempted more often, also within the
//! window, as load did in the mount test that found it.
//!
//! Without the blocking in `catch`, about 2 % of the trials lost the signal (39 to 50 of
//! 2000 per run on a 16-core desktop).
//!
//! No test harness (`harness = false` in Cargo.toml): libtest runs each test on a thread of
//! its own while its main thread waits, and that thread could take the signal.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]

// The Linux mount's signal handling: on other systems the test is an empty program.
#[cfg(target_os = "linux")]
#[path = "stop_signals/linux.rs"]
mod linux;

fn main() {
    #[cfg(target_os = "linux")]
    linux::main();
}
