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

#[path = "../src/stop_signals.rs"]
#[allow(dead_code)]
mod stop_signals;

use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const NAME: &str = "a_stop_signal_while_the_handlers_go_in_is_not_lost";
/// Set in a trial's environment: the delay, in nanoseconds.
const TRIAL: &str = "SACDFS_STOP_SIGNALS_TRIAL_NS";
const TRIALS: u64 = 2000;
/// Trials at once, per CPU.
const PER_CPU: u64 = 2;
/// The delays tried, spread over 0 to this many nanoseconds.
const SPREAD_NS: u64 = 20_000;
/// Threads in a trial that wake every 50 µs.
const SLEEPERS: usize = 8;
/// A trial's exit status when the handler was in place but the signal never reached it.
const LOST: i32 = 3;

fn mask(how: libc::c_int) {
    // SAFETY: a local set of valid signal numbers; changes this thread's mask only.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in stop_signals::STOP_SIGNALS {
            libc::sigaddset(&mut set, s);
        }
        assert_eq!(libc::pthread_sigmask(how, &set, std::ptr::null_mut()), 0);
    }
}

/// One trial: exits 0 if the signal reached the handler, [`LOST`] if not; SIGTERM ends it
/// if it came before the handler.
fn trial(delay: Duration) -> ! {
    // The threads started here inherit the block.
    mask(libc::SIG_BLOCK);
    let go = Arc::new(AtomicBool::new(false));
    let went = go.clone();
    // SAFETY: getpid cannot fail.
    let pid = unsafe { libc::getpid() };
    let sender = std::thread::spawn(move || {
        while !went.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        let start = Instant::now();
        while start.elapsed() < delay {
            std::hint::spin_loop();
        }
        // SAFETY: signals this process.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    });
    for _ in 0..SLEEPERS {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_micros(50));
            }
        });
    }
    mask(libc::SIG_UNBLOCK);
    go.store(true, Ordering::Release);
    let mut signals = stop_signals::catch().unwrap();
    sender.join().unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        if signals.pending().next().is_some() {
            std::process::exit(0);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    std::process::exit(LOST);
}

fn main() {
    if let Some(ns) = std::env::var_os(TRIAL) {
        trial(Duration::from_nanos(ns.to_str().unwrap().parse().unwrap()));
    }
    // What libtest would do with `--list` and a name filter, and its result line, which
    // scripts/check_skips.py counts.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        println!("{NAME}: test");
        return;
    }
    // The words that are not options or their values are name filters.
    let mut filters = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if ["--test-threads", "--skip", "--format", "--color", "--logfile", "-Z"].contains(&a.as_str()) {
            it.next();
        } else if !a.starts_with('-') {
            filters.push(a.as_str());
        }
    }
    if !filters.is_empty() && !filters.iter().any(|f| NAME.contains(f)) {
        println!("test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out");
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let (mut stopped, mut caught, mut lost) = (0, 0, Vec::new());
    let width = std::thread::available_parallelism().map_or(4, |n| n.get() as u64 * PER_CPU);
    let mut i = 0;
    while i < TRIALS {
        let batch: Vec<_> = (i..(i + width).min(TRIALS))
            .map(|i| {
                let ns = (i * 37) % SPREAD_NS;
                // Under coverage, no profile from each of the 2000 trials.
                let child = Command::new(&exe)
                    .env(TRIAL, ns.to_string())
                    .env("LLVM_PROFILE_FILE", "/dev/null")
                    .spawn();
                (ns, child.unwrap())
            })
            .collect();
        i += batch.len() as u64;
        for (ns, mut child) in batch {
            let st = child.wait().unwrap();
            match (st.code(), st.signal()) {
                (Some(0), _) => caught += 1,
                (Some(LOST), _) => lost.push(ns),
                (_, Some(libc::SIGTERM)) => stopped += 1,
                _ => panic!("trial at {ns} ns: {st}"),
            }
        }
    }
    println!("{TRIALS} trials: {stopped} stopped by the signal, {caught} caught, {} lost", lost.len());
    assert!(lost.is_empty(), "signals lost while the handlers went in, at {lost:?} ns");
    println!("test {NAME} ... ok");
    println!("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out");
}
