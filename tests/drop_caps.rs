// SPDX-License-Identifier: GPL-3.0-or-later

//! `privs::drop_mount_caps` (src/privs.rs, included here) against the kernel, in every thread.
//!
//! The program runs itself again under `unshare -Urm` (uid 0 with all capabilities inside the
//! namespace, a private mount table for the tmpfs cases); where that is refused it prints SKIP and
//! ends 0, and the lab runs it with real privileges. Each case runs in a forked child (a cut cannot
//! be undone), and no test harness is used (`harness = false`): fork needs a single thread.
//!
//! Cases (no_new_privs is read back with the sets): every thread of a process that has many, and one that is being started while the cut
//! runs, ends with exactly the two kept capabilities in the permitted, effective and inheritable
//! sets and the bounding set (read back from /proc, task by task); a thread started afterwards
//! the same; `mount` and `umount2` then fail with EPERM; a `ptrace` attach to a non-dumpable
//! process of the same uid and a write to its /proc/PID/mem fail, where before the cut the attach
//! worked (the positive control); an `execve` as uid 0 gains nothing (NOROOT); without CAP_SETPCAP
//! the permitted and effective sets are cut anyway and the result says the bounding set and
//! securebits were not; a thread that blocks the signal is reported, not skipped.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code, dead_code)]

#[cfg(target_os = "linux")]
#[path = "../src/privs.rs"]
mod privs;

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::thread;

    use crate::privs::{drop_mount_caps, keep_for, own_permitted, task_caps};

    const CAP_SYS_ADMIN: u64 = 1 << 21;
    const CAP_SETPCAP: u64 = 1 << 8;

    enum End {
        Exit(i32),
        Signal(i32),
    }

    fn child(body: impl FnOnce()) -> End {
        // SAFETY: fork in a process without other threads; the child runs the body and `_exit`s.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
            // SAFETY: leave without running the parent's atexit handlers.
            unsafe { libc::_exit(if r.is_ok() { 0 } else { 1 }) }
        }
        let mut st = 0;
        // SAFETY: waits for the child just forked.
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut st, 0) }, pid);
        if libc::WIFSIGNALED(st) { End::Signal(libc::WTERMSIG(st)) } else { End::Exit(libc::WEXITSTATUS(st)) }
    }

    fn outer() {
        let ok = Command::new("unshare").args(["-Urm", "true"]).status().is_ok_and(|s| s.success());
        if !ok {
            println!(
                "SKIP: cannot make a user and mount namespace here (unshare -Urm failed); run it on a lab host"
            );
            return;
        }
        let st = Command::new("unshare")
            .args(["-Urm", "--"])
            .arg(std::env::current_exe().unwrap())
            .env("SACDFS_TEST_INNER", "1")
            .status()
            .unwrap();
        std::process::exit(st.code().unwrap_or(1));
    }

    /// Every task of this process is cut: exactly `keep` in the permitted, effective and inheritable
    /// sets, nothing ambient; the bounding set too unless `bounding` is false.
    fn assert_cut(keep: u64, bounding: bool) -> usize {
        let tasks = task_caps().unwrap();
        for t in &tasks {
            assert_eq!(
                (t.prm, t.eff, t.inh, t.amb, t.nnp),
                (keep, keep, 0, 0, true),
                "task {}: {t:?}",
                t.tid
            );
            if bounding {
                assert_eq!(t.bnd, keep, "task {} bounding set", t.tid);
            }
        }
        tasks.len()
    }

    /// Threads parked until `go` is dropped.
    fn park(n: usize) -> (Vec<thread::JoinHandle<()>>, mpsc::Sender<()>) {
        let (tx, rx) = mpsc::channel::<()>();
        let rx = Arc::new(std::sync::Mutex::new(rx));
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let hs = (0..n)
            .map(|_| {
                let (rx, ready) = (rx.clone(), ready_tx.clone());
                thread::spawn(move || {
                    ready.send(()).unwrap();
                    let _ = rx.lock().unwrap().recv();
                })
            })
            .collect();
        for _ in 0..n {
            ready_rx.recv().unwrap();
        }
        (hs, tx)
    }

    fn every_thread() {
        let (before, prm) = own_permitted().unwrap();
        assert!(before, "needs CAP_SYS_ADMIN here");
        let keep = keep_for(prm);
        if task_caps().unwrap().iter().any(|c| c.nnp) {
            println!(
                "NOTE: no_new_privs was already set before the cut (inherited): that part of the case proves nothing here"
            );
        }
        let (hs, tx) = park(12);
        // A spawner that keeps starting short-lived threads while the cut runs: they inherit
        // whatever their parent has when they start, and the read-back must find any that was
        // born before the cut reached its parent.
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let spawner = thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                thread::spawn(|| thread::sleep(std::time::Duration::from_millis(20)));
                thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        let d = drop_mount_caps().expect("drop");
        stop.store(true, Ordering::Relaxed);
        spawner.join().unwrap();
        assert!(!d.bounding_set_kept && !d.securebits_unset, "{d:?}");
        let n = assert_cut(keep, true);
        assert!(n >= 14, "only {n} tasks were checked");
        // A thread started now has the cut too.
        thread::spawn(|| {
            assert_cut(keep_for(own_permitted().unwrap().1), true);
        })
        .join()
        .unwrap();
        // NOROOT: a program run as uid 0 gains nothing.
        let out = Command::new("/bin/sh").args(["-c", "grep ^CapEff: /proc/self/status"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "CapEff:\t0000000000000000", "exec as uid 0");
        drop(tx);
        for h in hs {
            h.join().unwrap();
        }
    }

    fn raw_mount(dir: &str) -> io::Result<()> {
        let d = std::ffi::CString::new(dir).unwrap();
        // SAFETY: a plain mount of tmpfs.
        let r = unsafe { libc::mount(c"t".as_ptr(), d.as_ptr(), c"tmpfs".as_ptr(), 0, std::ptr::null()) };
        if r == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
    }

    fn no_mount_no_umount() {
        let dir = std::env::temp_dir().join(format!("sacdfs-drop-caps-{}", std::process::id()));
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        raw_mount(a.to_str().unwrap()).expect("control: mount before the cut");
        let (_hs, _tx) = park(3);
        drop_mount_caps().expect("drop");
        let e = raw_mount(b.to_str().unwrap()).expect_err("mount after the cut");
        assert_eq!(e.raw_os_error(), Some(libc::EPERM), "{e}");
        let p = std::ffi::CString::new(a.to_str().unwrap()).unwrap();
        // SAFETY: umount2 of a path.
        let r = unsafe { libc::umount2(p.as_ptr(), libc::MNT_DETACH) };
        assert_eq!(
            (r, io::Error::last_os_error().raw_os_error()),
            (-1, Some(libc::EPERM)),
            "umount2 after the cut"
        );
        // The same from a thread that existed before the cut.
        let e2 =
            thread::spawn(move || raw_mount(b.to_str().unwrap())).join().unwrap().expect_err("thread mount");
        assert_eq!(e2.raw_os_error(), Some(libc::EPERM));
    }

    /// A sibling process of the same uid that is not dumpable, like the helper: alive until `go`
    /// is written. Returns its pid and the write end.
    fn undumpable_sibling() -> (i32, i32) {
        let (mut ready, mut go) = ([0; 2], [0; 2]);
        // SAFETY: two pipes into local arrays.
        unsafe {
            assert_eq!(libc::pipe(ready.as_mut_ptr()), 0);
            assert_eq!(libc::pipe(go.as_mut_ptr()), 0);
        }
        // SAFETY: fork while this process has a single thread.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: prctl, tell the parent, block until the parent ends, then exit.
            unsafe {
                libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
                libc::close(go[1]);
                libc::write(ready[1], [1u8].as_ptr().cast(), 1);
                let mut b = [0u8; 1];
                libc::read(go[0], b.as_mut_ptr().cast(), 1);
                libc::_exit(0)
            }
        }
        let mut b = [0u8; 1];
        // SAFETY: wait until the sibling is non-dumpable.
        assert_eq!(unsafe { libc::read(ready[0], b.as_mut_ptr().cast(), 1) }, 1);
        (pid, go[1])
    }

    fn ptrace_and_mem() {
        let (pid, _go) = undumpable_sibling();
        let attach = || {
            // SAFETY: attach to a child of ours; detach again at once.
            let r = unsafe { libc::ptrace(libc::PTRACE_ATTACH, pid, 0, 0) };
            let e = io::Error::last_os_error().raw_os_error();
            if r == 0 {
                let mut st = 0;
                // SAFETY: wait for the stop, then detach.
                unsafe {
                    libc::waitpid(pid, &raw mut st, 0);
                    libc::ptrace(libc::PTRACE_DETACH, pid, 0, libc::SIGCONT);
                    libc::kill(pid, libc::SIGCONT);
                }
            }
            (r, e)
        };
        let mem =
            move || std::fs::OpenOptions::new().write(true).open(format!("/proc/{pid}/mem")).map(|_| ());
        assert_eq!(
            attach().0,
            0,
            "control: ptrace attach to a non-dumpable sibling works with CAP_SYS_PTRACE"
        );
        assert!(mem().is_ok(), "control: /proc/PID/mem opens for writing with CAP_SYS_PTRACE");
        let (_hs, _tx) = park(2);
        drop_mount_caps().expect("drop");
        let (r, e) = attach();
        assert_eq!((r, e), (-1, Some(libc::EPERM)), "ptrace attach after the cut");
        let m = mem().expect_err("/proc/PID/mem after the cut");
        assert!(matches!(m.raw_os_error(), Some(libc::EACCES | libc::EPERM)), "{m}");
        // And from a thread of the process that existed before the cut.
        let t = thread::spawn(move || mem().is_err()).join().unwrap();
        assert!(t);
    }

    fn without_setpcap() {
        // Take CAP_SETPCAP away first (as a container without it would), in this thread, before the others exist.
        #[repr(C)]
        struct H {
            v: u32,
            p: i32,
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct D {
            e: u32,
            p: u32,
            i: u32,
        }
        let h = H { v: 0x2008_0522, p: 0 };
        let mut d = [D { e: 0, p: 0, i: 0 }; 2];
        // SAFETY: capget then capset with local buffers.
        unsafe {
            assert_eq!(libc::syscall(libc::SYS_capget, &raw const h, d.as_mut_ptr()), 0);
            d[0].e &= !(CAP_SETPCAP as u32);
            d[0].p &= !(CAP_SETPCAP as u32);
            assert_eq!(libc::syscall(libc::SYS_capset, &raw const h, d.as_ptr()), 0);
        }
        let (_hs, _tx) = park(4);
        let dropped = drop_mount_caps().expect("drop");
        assert!(dropped.bounding_set_kept, "{dropped:?}");
        assert!(dropped.securebits_unset, "{dropped:?}");
        let keep = keep_for(own_permitted().unwrap().1);
        assert_cut(keep, false);
        let t = task_caps().unwrap();
        assert!(
            t.iter().all(|c| c.bnd & CAP_SYS_ADMIN != 0),
            "the bounding set is untouched without CAP_SETPCAP"
        );
    }

    /// Sets this thread's permitted, effective and inheritable sets (low word only).
    fn set_caps(eff: u32, prm: u32, inh: u32) {
        #[repr(C)]
        struct H {
            v: u32,
            p: i32,
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct D {
            e: u32,
            p: u32,
            i: u32,
        }
        let h = H { v: 0x2008_0522, p: 0 };
        let mut d = [D { e: 0, p: 0, i: 0 }; 2];
        // SAFETY: capget then capset with local buffers.
        unsafe {
            assert_eq!(libc::syscall(libc::SYS_capget, &raw const h, d.as_mut_ptr()), 0);
            d[0] = D { e: eff, p: prm, i: inh };
            assert_eq!(libc::syscall(libc::SYS_capset, &raw const h, d.as_ptr()), 0);
        }
    }

    fn holds_only_one_of_the_kept() {
        // As in a container that grants DAC_OVERRIDE but not DAC_READ_SEARCH: only what is held is kept,
        // and the cut must not try to add what is missing.
        let (_, prm) = own_permitted().unwrap();
        let low = prm as u32 & !(1 << 2);
        set_caps(low, low, 0);
        let (_hs, _tx) = park(3);
        let d = drop_mount_caps().expect("drop with one of the two kept capabilities");
        assert!(!d.bounding_set_kept, "{d:?}");
        assert_cut(1 << 1, true);
    }

    fn only_read_search_held() {
        // Without CAP_DAC_OVERRIDE, CAP_DAC_READ_SEARCH is what is kept.
        let (_, prm) = own_permitted().unwrap();
        let low = prm as u32 & !(1 << 1);
        set_caps(low, low, 0);
        let (_hs, _tx) = park(3);
        drop_mount_caps().expect("drop");
        assert_cut(1 << 2, true);
    }

    fn both_held_keeps_only_override() {
        let (_, prm) = own_permitted().unwrap();
        assert!(prm & 6 == 6, "control: both held");
        let (_hs, _tx) = park(3);
        drop_mount_caps().expect("drop");
        assert_cut(1 << 1, true);
    }

    fn ambient_is_cleared() {
        // An ambient capability survives an exec: it must be gone, in every thread.
        let (_, prm) = own_permitted().unwrap();
        let low = prm as u32;
        set_caps(low, low, 1 << 1);
        // SAFETY: raise DAC_OVERRIDE into the ambient set (it is permitted and inheritable now).
        let r = unsafe { libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_RAISE, 1u64, 0, 0) };
        assert_eq!(r, 0, "control: raising an ambient capability: {}", io::Error::last_os_error());
        assert!(task_caps().unwrap().iter().all(|c| c.amb == 1 << 1), "control: ambient set");
        let (_hs, _tx) = park(3);
        drop_mount_caps().expect("drop");
        assert_cut(keep_for(prm), true);
    }

    fn blocked_thread_is_reported() {
        let (tx, rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let h = thread::spawn(move || {
            // SAFETY: block the broadcast signal in this thread only.
            unsafe {
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&raw mut set);
                libc::sigaddset(&raw mut set, libc::SIGRTMAX() - 3);
                libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut());
            }
            ready_tx.send(()).unwrap();
            let _ = rx.recv();
        });
        ready_rx.recv().unwrap();
        let t = std::time::Instant::now();
        let e = drop_mount_caps().expect_err("a thread that blocks the signal must be reported");
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "reported only after {:?}", t.elapsed());
        assert!(e.to_string().contains("signal") || e.to_string().contains("reached"), "{e}");
        // The calling thread was cut; the blocked one was not, and the error says so.
        let caps = task_caps().unwrap();
        assert!(caps.iter().any(|c| c.eff & CAP_SYS_ADMIN != 0), "the blocked thread still holds it");
        drop(tx);
        h.join().unwrap();
    }

    pub fn main() {
        if std::env::var_os("SACDFS_TEST_INNER").is_none() {
            return outer();
        }
        let cases: [(&str, &dyn Fn()); 9] = [
            (
                "every thread is cut, also those born during the cut and after it; exec as uid 0 gains nothing",
                &every_thread,
            ),
            ("mount and umount2 fail with EPERM in every thread", &no_mount_no_umount),
            (
                "ptrace attach and /proc/PID/mem of a non-dumpable sibling fail (control: both worked before)",
                &ptrace_and_mem,
            ),
            (
                "without CAP_SETPCAP: sets cut, bounding set and securebits reported as not cut",
                &without_setpcap,
            ),
            (
                "only the kept capabilities that are held are kept (no DAC_READ_SEARCH here)",
                &holds_only_one_of_the_kept,
            ),
            ("without CAP_DAC_OVERRIDE, CAP_DAC_READ_SEARCH is kept", &only_read_search_held),
            ("with both held only CAP_DAC_OVERRIDE is kept", &both_held_keeps_only_override),
            ("an ambient capability is cleared in every thread", &ambient_is_cleared),
            (
                "a thread that blocks the signal is reported, not skipped, within 5 s",
                &blocked_thread_is_reported,
            ),
        ];
        let mut failed = 0;
        for (name, f) in cases {
            let end = child(f);
            match end {
                End::Exit(0) => println!("ok   {name}"),
                End::Exit(c) => {
                    failed += 1;
                    println!("FAIL {name}: exit {c}");
                }
                End::Signal(s) => {
                    failed += 1;
                    println!("FAIL {name}: signal {s}");
                }
            }
        }
        println!("drop_caps: {} passed, {failed} failed", cases.len() - failed);
        if failed != 0 {
            std::process::exit(1);
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    linux::main();
}
