// SPDX-License-Identifier: GPL-3.0-or-later

//! The signals that stop sacdfs (SIGINT, SIGTERM, SIGHUP), caught without losing one that
//! arrives while the handlers go in (#334).
//!
//! signal-hook installs a signal's handler (`sigaction`) before it records where the handler
//! is to send the signal. A signal that arrives in between runs the new handler, which finds
//! nothing to send it to and, as the previous action was the default one, drops it: the
//! signal neither stops the process, as before the handler, nor reaches [`Signals`], as
//! after. A sacdfs stopped right after its start then mounted and ran on. Here the signals
//! are blocked while the handlers go in: one that arrives meanwhile waits, and is delivered
//! to the complete handler when they are unblocked.
//!
//! Blocking applies to the calling thread only, so [`catch`] must run before any other
//! thread exists (or with the signals blocked in all of them): another thread could take the
//! signal in the window. `tests/stop_signals.rs` includes this file and checks it.

use std::io;

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

/// The signals that stop sacdfs.
pub const STOP_SIGNALS: [libc::c_int; 3] = [SIGINT, SIGTERM, SIGHUP];

/// Catches [`STOP_SIGNALS`] from here on: each is then delivered to the returned [`Signals`]
/// instead of ending the process, including one that arrives during this call. To be called
/// while this is the process's only thread.
#[allow(unsafe_code)]
pub fn catch() -> io::Result<Signals> {
    // SAFETY: sigemptyset and sigaddset fill a local set with valid signal numbers;
    // pthread_sigmask changes this thread's mask only and writes the old one to a local.
    let old = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in STOP_SIGNALS {
            libc::sigaddset(&mut set, s);
        }
        let mut old: libc::sigset_t = std::mem::zeroed();
        let r = libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old);
        if r != 0 {
            return Err(io::Error::from_raw_os_error(r));
        }
        old
    };
    let signals = Signals::new(STOP_SIGNALS);
    // The mask as it was: a signal held meanwhile is delivered now, to the handler.
    // SAFETY: `old` is the mask pthread_sigmask returned above.
    let r = unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut()) };
    if r != 0 {
        return Err(io::Error::from_raw_os_error(r));
    }
    signals
}
