// SPDX-License-Identifier: GPL-3.0-or-later

//! Giving up CAP_SYS_ADMIN once the mount exists (hardening step 1, part 2; docs/design.md).
//!
//! sacdfs needs CAP_SYS_ADMIN for one thing: `mount(2)`. Everything after the mount (serving,
//! reading images, writing the index cache) needs at most CAP_DAC_OVERRIDE and CAP_DAC_READ_SEARCH,
//! and the unmount is the helper's ([`crate::unmount_helper`]). So once the helper is armed the
//! daemon cuts itself down to those two ([`drop_mount_caps`]), in EVERY thread: capabilities are a
//! per-thread property on Linux, and threads exist by then (rayon pools, the engine's workers).
//!
//! How every thread is reached: this thread cuts itself first, then each other thread of
//! `/proc/self/task` gets a real-time signal whose handler makes the same raw system calls
//! (the way libcap's `psx` does it), and the result is then READ BACK from every task's
//! `/proc/PID/task/TID/status`, repeated until all agree or the rounds run out. A thread that
//! another thread starts while this runs inherits its parent's set at that moment; the read-back
//! finds it and it gets its signal in the next round. Nothing is claimed that was not read back.
//!
//! What the cut does, per thread, in this order (the numbers are the order in the code: securebits, ambient, bounding set, no_new_privs, capset) (the order matters: dropping from the bounding
//! set and setting the securebits need CAP_SETPCAP in the effective set, which the last step
//! removes with the rest):
//!   1. `PR_SET_SECUREBITS` NOROOT and NOROOT_LOCKED: a later `execve` as uid 0 gains nothing;
//!   2. the ambient set cleared;
//!   3. every capability but the two kept dropped from the bounding set, so that none comes back
//!      through an exec;
//!   4. `PR_SET_NO_NEW_PRIVS` (needs nothing): a setuid program or one with file capabilities gains
//!      nothing in this thread or its children;
//!   5. `capset`: permitted, effective and inheritable cut to the two kept (only those the thread
//!      holds: `capset` cannot add).
//!
//! Where CAP_SETPCAP is missing, steps 1 and 3 fail: the permitted and effective sets are cut
//! anyway and the result says the bounding set and securebits were NOT (a degraded drop, logged).
//!
//! Why CAP_SYS_PTRACE and the rest go too, not only CAP_SYS_ADMIN: the argument "the daemon cannot
//! touch its helper" rests on the helper being non-dumpable AND the daemon not holding
//! CAP_SYS_PTRACE (same uid). A bare `sudo sacdfs` starts with every capability, so dropping only
//! SYS_ADMIN would leave the daemon able to `ptrace` the helper and take its capability back.

use std::io;
use std::sync::atomic::{AtomicU32, Ordering};

pub const CAP_DAC_OVERRIDE: u32 = 1;
pub const CAP_DAC_READ_SEARCH: u32 = 2;
pub const CAP_SYS_ADMIN: u32 = 21;

/// The capabilities the daemon keeps after the mount, given what it holds (`prm`): reading the images
/// and writing the cache and tags folders whatever their owner and mode (`docs/deployment.md`,
/// "Privileges"). `CAP_DAC_OVERRIDE` where it is held, which covers it all; `CAP_DAC_READ_SEARCH` only
/// where `CAP_DAC_OVERRIDE` is NOT held (with both it would only add `open_by_handle_at`, which sacdfs
/// never uses and the "shocker" escape needs).
pub fn keep_for(prm: u64) -> u64 {
    if prm & 1 << CAP_DAC_OVERRIDE != 0 { 1 << CAP_DAC_OVERRIDE } else { prm & 1 << CAP_DAC_READ_SEARCH }
}

/// The signal used to reach the other threads: the real-time signal SIGRTMAX-3 (61 on Linux, where
/// SIGRTMAX is 64), set to ignored again afterwards.
fn broadcast_signal() -> libc::c_int {
    libc::SIGRTMAX() - 3
}

// What the signal handler applies, set before the first signal is sent.
static KEEP_LO: AtomicU32 = AtomicU32::new(0);
static KEEP_HI: AtomicU32 = AtomicU32::new(0);
// Bits OR-ed in by every thread that failed a step, and the number of threads that finished.
static FAILED: AtomicU32 = AtomicU32::new(0);
static DONE: AtomicU32 = AtomicU32::new(0);

const FAIL_SECUREBITS: u32 = 1;
const FAIL_BOUNDING: u32 = 2;
const FAIL_CAPSET: u32 = 4;
const FAIL_NO_NEW_PRIVS: u32 = 8;

const SECBIT_NOROOT: libc::c_ulong = 1 << 0;
const SECBIT_NOROOT_LOCKED: libc::c_ulong = 1 << 1;

#[repr(C)]
struct Header {
    version: u32,
    pid: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Data {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}
const CAP_VERSION_3: u32 = 0x2008_0522;

/// The cut itself, on the calling thread: raw system calls only, no allocation, no locks, so it
/// is safe in a signal handler. Returns the `FAIL_*` bits.
#[allow(unsafe_code)]
fn apply() -> u32 {
    let (lo, hi) = (KEEP_LO.load(Ordering::Relaxed), KEEP_HI.load(Ordering::Relaxed));
    let mut failed = 0;
    // SAFETY: plain prctl and capset calls with stack data.
    unsafe {
        if libc::prctl(libc::PR_SET_SECUREBITS, SECBIT_NOROOT | SECBIT_NOROOT_LOCKED, 0, 0, 0) != 0 {
            failed |= FAIL_SECUREBITS;
        }
        // No program this thread runs, setuid or with file capabilities, gains anything. The daemon
        // runs none after the mount on this path (the setuid `fusermount3` unmount is skipped once the
        // capability is gone; nothing else is started after the mount).
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            failed |= FAIL_NO_NEW_PRIVS;
        }
        // Absent before Linux 4.3, and then nothing to clear.
        libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0);
        let mut cap = 0u32;
        while libc::prctl(libc::PR_CAPBSET_READ, cap, 0, 0, 0) >= 0 {
            let kept = if cap < 32 { lo >> cap & 1 } else { hi >> (cap - 32) & 1 } != 0;
            if !kept && libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) != 0 {
                failed |= FAIL_BOUNDING;
            }
            cap += 1;
        }
        let h = Header { version: CAP_VERSION_3, pid: 0 };
        let d = [
            Data { effective: lo, permitted: lo, inheritable: 0 },
            Data { effective: hi, permitted: hi, inheritable: 0 },
        ];
        if libc::syscall(libc::SYS_capset, &raw const h, d.as_ptr()) != 0 {
            failed |= FAIL_CAPSET;
        }
    }
    failed
}

#[allow(unsafe_code)]
extern "C" fn on_signal(_: libc::c_int) {
    // errno belongs to the interrupted code: keep it.
    let saved = io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let f = apply();
    FAILED.fetch_or(f, Ordering::AcqRel);
    DONE.fetch_add(1, Ordering::AcqRel);
    // SAFETY: the thread's own errno.
    unsafe { *libc::__errno_location() = saved };
}

/// The capabilities of one task, as `/proc/PID/task/TID/status` lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskCaps {
    pub tid: i32,
    pub inh: u64,
    pub prm: u64,
    pub eff: u64,
    pub bnd: u64,
    pub amb: u64,
    /// `NoNewPrivs` is set.
    pub nnp: bool,
}

fn hex(line: &str, key: &str) -> Option<u64> {
    u64::from_str_radix(line.strip_prefix(key)?.trim(), 16).ok()
}

/// The capabilities of every task of this process, read from /proc (what the kernel says, not what
/// this code believes it did).
pub fn task_caps() -> io::Result<Vec<TaskCaps>> {
    let mut out = Vec::new();
    for t in std::fs::read_dir("/proc/self/task")? {
        let t = t?;
        let Ok(tid) = t.file_name().to_string_lossy().parse::<i32>() else { continue };
        // A task that ended between the listing and the read is not an error.
        let Ok(s) = std::fs::read_to_string(t.path().join("status")) else { continue };
        let mut c = TaskCaps { tid, inh: 0, prm: 0, eff: 0, bnd: 0, amb: 0, nnp: false };
        for l in s.lines() {
            if let Some(v) = hex(l, "CapInh:") {
                c.inh = v;
            } else if let Some(v) = hex(l, "CapPrm:") {
                c.prm = v;
            } else if let Some(v) = hex(l, "CapEff:") {
                c.eff = v;
            } else if let Some(v) = hex(l, "CapBnd:") {
                c.bnd = v;
            } else if let Some(v) = hex(l, "CapAmb:") {
                c.amb = v;
            } else if let Some(v) = l.strip_prefix("NoNewPrivs:") {
                c.nnp = v.trim() == "1";
            }
        }
        out.push(c);
    }
    Ok(out)
}

/// What [`drop_mount_caps`] did.
#[derive(Debug)]
pub struct Dropped {
    /// Threads read back with the cut applied.
    pub threads: usize,
    /// The bounding set could not be cut (no CAP_SETPCAP): an `execve` could get a capability
    /// back from a file; the daemon execs nothing after the mount.
    pub bounding_set_kept: bool,
    /// The securebits (NOROOT) could not be set.
    pub securebits_unset: bool,
    /// The effective set of the process now, for the log.
    pub effective: u64,
}

/// Whether this thread holds CAP_SYS_ADMIN, and its permitted set.
#[allow(unsafe_code)]
pub fn own_permitted() -> io::Result<(bool, u64)> {
    let mut h = Header { version: CAP_VERSION_3, pid: 0 };
    let mut d = [Data::default(); 2];
    // SAFETY: capget with a version-3 header and room for its two data words.
    if unsafe { libc::syscall(libc::SYS_capget, &raw mut h, d.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let eff = u64::from(d[1].effective) << 32 | u64::from(d[0].effective);
    let prm = u64::from(d[1].permitted) << 32 | u64::from(d[0].permitted);
    Ok((eff & 1 << CAP_SYS_ADMIN != 0, prm))
}

/// Whether `c` is cut: no capability outside `keep` in the permitted, effective or inheritable set,
/// no ambient one, and `no_new_privs` set.
fn is_cut(c: &TaskCaps, keep: u64) -> bool {
    (c.prm | c.eff | c.inh | c.amb) & !keep == 0 && c.nnp
}

/// Cuts every thread of the process down to what [`keep_for`] says (CAP_DAC_OVERRIDE, or CAP_DAC_READ_SEARCH
/// where CAP_DAC_OVERRIDE is not held).
/// An error means the cut is NOT in force everywhere (a thread could not be reached): the caller
/// keeps behaving as if it had the capability.
#[allow(unsafe_code)]
pub fn drop_mount_caps() -> io::Result<Dropped> {
    let (_, prm) = own_permitted()?;
    let keep = keep_for(prm);
    KEEP_LO.store(keep as u32, Ordering::Relaxed);
    KEEP_HI.store((keep >> 32) as u32, Ordering::Relaxed);
    FAILED.store(0, Ordering::Relaxed);
    DONE.store(0, Ordering::Relaxed);
    // The handler first, then this thread, so that threads started from here on inherit the cut.
    let sig = broadcast_signal();
    // SAFETY: sigaction with a zeroed struct, a handler that only makes raw system calls, and the
    // previous disposition restored (ignored) at the end.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&raw mut sa.sa_mask);
        if libc::sigaction(sig, &raw const sa, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut failed = apply();
    // SAFETY: gettid and getpid have no preconditions.
    let (me, pid) = unsafe { (libc::syscall(libc::SYS_gettid) as i32, libc::getpid()) };
    let mut result = Err(io::Error::other("no round ran"));
    let mut last: Vec<i32> = Vec::new();
    for _round in 0..50 {
        let tasks = task_caps()?;
        let todo: Vec<i32> =
            tasks.iter().filter(|c| c.tid != me && !is_cut(c, keep)).map(|c| c.tid).collect();
        if todo.is_empty() && tasks.iter().all(|c| is_cut(c, keep)) {
            result = Ok(tasks.len());
            break;
        }
        if todo == last {
            // The same threads again after a round: they do not take the signal (it is blocked).
            result = Err(io::Error::other("a thread of the process does not take the signal"));
            break;
        }
        last.clone_from(&todo);
        let before = DONE.load(Ordering::Acquire);
        let mut sent = 0;
        for tid in &todo {
            // SAFETY: tgkill of our own thread; a thread that just ended answers ESRCH.
            if unsafe { libc::syscall(libc::SYS_tgkill, pid, *tid, sig) } == 0 {
                sent += 1;
            }
        }
        // The handlers run at once unless the thread blocks the signal; wait a moment for them.
        for _ in 0..500 {
            if DONE.load(Ordering::Acquire) - before >= sent {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        failed |= FAILED.load(Ordering::Acquire);
    }
    // SAFETY: back to ignoring the signal: its default action would end the process.
    unsafe { libc::signal(sig, libc::SIG_IGN) };
    failed |= FAILED.load(Ordering::Acquire);
    let threads = result.map_err(|_| io::Error::other("a thread of the process could not be reached"))?;
    if failed & FAIL_CAPSET != 0 {
        return Err(io::Error::other("capset failed in a thread"));
    }
    let tasks = task_caps()?;
    let effective = tasks.iter().map(|c| c.eff).fold(0, |a, b| a | b);
    // From /proc, not from the flags: what the kernel says is cut.
    let bounding_set_kept = tasks.iter().any(|c| c.bnd & !keep != 0);
    Ok(Dropped { threads, bounding_set_kept, securebits_unset: failed & FAIL_SECUREBITS != 0, effective })
}
