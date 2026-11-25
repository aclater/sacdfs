// SPDX-License-Identifier: GPL-3.0-or-later

//! The seccomp deny-list (src/deny_list.rs, included here) on the real kernel. The unit tests
//! evaluate the program with a reference interpreter; this installs it and makes the calls.
//!
//! Every case runs in a forked child, because a filter cannot be taken off and some cases end
//! in SIGSYS on purpose. The parent reads how the child ended. No test harness
//! (`harness = false`): fork is safe only before any thread exists.
//!
//! No root is needed: a child sets `no_new_privs` itself where the case is not about that. Cases:
//!
//!   - every denied call returns -1 with the test's errno (3999, not one the kernel would pick),
//!     on the thread that installed the filter AND on a thread that already existed (TSYNC), AND
//!     on one started afterwards;
//!   - umount2 is refused only with `deny_umount`;
//!   - what sacdfs does after the mount stays possible (read, write, openat, a thread, fork,
//!     exec of another program, `seccomp` itself);
//!   - another ABI is killed with SIGSYS: `int 0x80` and the x32 number bit on x86_64;
//!   - without CAP_SYS_ADMIN and without `no_new_privs` the install fails with EACCES (and the
//!     process is then unfiltered), the message the daemon logs;
//!   - the production errno is EPERM and `install` returns the number of denied calls.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code, dead_code)]

// The filter exists for x86_64 and i686 only (none on ARM until it has run there): elsewhere this test says so
// and passes, so that a hosted ARM runner does not run a test of a filter that is not installed.
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "x86")))]
#[path = "../src/deny_list.rs"]
mod deny_list;

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "x86")))]
mod linux {
    use std::io;
    use std::process::Command;
    use std::sync::mpsc;
    use std::thread;

    use crate::deny_list::{Policy, denied, install, refused};

    /// An errno no denied call returns by itself (the kernel clamps a filter's errno to 4095, so not a large one).
    const TEST_ERRNO: u16 = 3999;

    fn policy(deny_umount: bool) -> Policy {
        Policy { deny_umount, errno: TEST_ERRNO }
    }

    /// How a case ended.
    enum End {
        Exit(i32),
        Signal(i32),
    }

    /// Runs `body` in a forked child; the child exits 0 if it returns, `FAIL` text on stderr and exit 1
    /// if it panics (a panic exits 101 here: the hook below prints and `_exit`s).
    fn child(body: impl FnOnce()) -> End {
        // SAFETY: fork in a process without other threads; the child runs the body and `_exit`s.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork: {}", io::Error::last_os_error());
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

    fn nnp() {
        // SAFETY: plain prctl.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) }, 0);
    }

    /// The raw call, so that no libc wrapper changes the arguments or the errno handling.
    fn raw(nr: u32, a: [usize; 3]) -> (isize, i32) {
        // SAFETY: the arguments are null or dummy; every denied call is refused before it looks at
        // them, and the calls checked for "not denied" are given arguments that make them fail.
        let r = unsafe { libc::syscall(nr as libc::c_long, a[0], a[1], a[2]) };
        (r as isize, io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }

    /// The list the review approved, written out here on purpose and NOT taken from `denied`: a call
    /// dropped from the module's list must fail this test.
    fn approved() -> Vec<u32> {
        #[allow(unused_mut)]
        let mut v = vec![
            libc::SYS_open_by_handle_at,
            libc::SYS_name_to_handle_at,
            libc::SYS_mount,
            libc::SYS_fsopen,
            libc::SYS_fsconfig,
            libc::SYS_fsmount,
            libc::SYS_fspick,
            libc::SYS_move_mount,
            libc::SYS_open_tree,
            libc::SYS_mount_setattr,
            libc::SYS_ptrace,
            libc::SYS_bpf,
            libc::SYS_keyctl,
            libc::SYS_kexec_load,
            libc::SYS_init_module,
            libc::SYS_finit_module,
            libc::SYS_pivot_root,
            libc::SYS_setns,
            libc::SYS_unshare,
            libc::SYS_process_vm_readv,
            libc::SYS_process_vm_writev,
            libc::SYS_perf_event_open,
            libc::SYS_userfaultfd,
            libc::SYS_add_key,
            libc::SYS_request_key,
            libc::SYS_io_uring_setup,
            libc::SYS_io_uring_enter,
            libc::SYS_io_uring_register,
            libc::SYS_chroot,
            libc::SYS_swapon,
            libc::SYS_swapoff,
            libc::SYS_reboot,
            libc::SYS_delete_module,
        ];
        #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
        v.extend([libc::SYS_iopl, libc::SYS_ioperm]);
        #[cfg(not(target_arch = "x86"))]
        v.push(libc::SYS_kexec_file_load);
        v.into_iter().map(|n| n as u32).collect()
    }

    /// Every denied call, from this thread: -1 and the test errno.
    fn all_denied(deny_umount: bool) {
        let mut d = approved();
        if deny_umount {
            d.push(libc::SYS_umount2 as u32);
        }
        assert_eq!(
            d.len(),
            denied(&policy(deny_umount)).len(),
            "the module's list and the approved list differ in length"
        );
        for nr in d {
            let (r, e) = raw(nr, [0, 0, 0]);
            assert_eq!(
                (r, e),
                (-1, i32::from(TEST_ERRNO)),
                "syscall {nr} was not denied (ret {r}, errno {e})"
            );
        }
    }

    fn denied_everywhere() {
        // A thread that exists BEFORE the filter: only TSYNC puts the filter on it.
        let (tx_ready, rx_ready) = mpsc::channel::<()>();
        let (tx_go, rx_go) = mpsc::channel::<()>();
        let (tx_done, rx_done) = mpsc::channel::<Result<(), String>>();
        let early = thread::spawn(move || {
            tx_ready.send(()).unwrap();
            rx_go.recv().unwrap();
            tx_done
                .send(std::panic::catch_unwind(|| all_denied(false)).map_err(|_| "early thread".to_string()))
                .unwrap();
        });
        rx_ready.recv().unwrap();
        nnp();
        let n = install(&policy(false)).expect("install");
        assert_eq!(n, refused(&policy(false)));
        all_denied(false);
        tx_go.send(()).unwrap();
        rx_done.recv().unwrap().expect("a thread that existed before the filter was not filtered (TSYNC)");
        early.join().unwrap();
        // A thread started afterwards inherits it.
        thread::spawn(|| all_denied(false)).join().expect("a later thread was not filtered");
    }

    fn umount_only_when_asked(deny: bool) {
        nnp();
        install(&policy(deny)).expect("install");
        let path = b"/nonexistent-deny-list-test\0";
        let (r, e) = raw(libc::SYS_umount2 as u32, [path.as_ptr() as usize, 0, 0]);
        if deny {
            assert_eq!((r, e), (-1, i32::from(TEST_ERRNO)), "umount2 allowed with deny_umount");
        } else {
            assert_eq!((r, e), (-1, libc::ENOENT), "umount2 refused without deny_umount (errno {e})");
        }
    }

    fn what_stays_possible() {
        nnp();
        install(&policy(true)).expect("install");
        // Files, pipes, time, memory.
        let mut fds = [0; 2];
        // SAFETY: pipe into a two-element array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let b = b"x";
        // SAFETY: write and read one byte on the pipe just made.
        assert_eq!(unsafe { libc::write(fds[1], b.as_ptr().cast(), 1) }, 1);
        let mut c = [0u8; 1];
        // SAFETY: as above.
        assert_eq!(unsafe { libc::read(fds[0], c.as_mut_ptr().cast(), 1) }, 1);
        assert!(std::fs::read_to_string("/proc/self/status").unwrap().contains("Seccomp:\t2"));
        // Threads (clone3), fork, exec of another program (the `fusermount3 -u` case).
        assert_eq!(thread::spawn(|| 7).join().unwrap(), 7);
        let st = Command::new("/bin/sh").args(["-c", "exit 0"]).status().expect("exec after the filter");
        #[cfg(target_arch = "x86")]
        {
            // A 32-bit program on a 64-bit kernel starts /bin/sh as a 64-bit program: another system call
            // ABI than the filter's, so it is killed (documented). On a 32-bit kernel it runs.
            // SAFETY: uname into a zeroed struct.
            let machine = unsafe {
                let mut u: libc::utsname = std::mem::zeroed();
                libc::uname(&raw mut u);
                std::ffi::CStr::from_ptr(u.machine.as_ptr()).to_string_lossy().into_owned()
            };
            if machine == "x86_64" {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(st.signal(), Some(libc::SIGSYS), "a 64-bit child of a 32-bit process: {st:?}");
            } else {
                assert!(st.success());
            }
        }
        #[cfg(not(target_arch = "x86"))]
        assert!(st.success());
        // A second filter on top (the container runtime's profile is one already).
        install(&policy(true)).expect("a second install");
    }

    fn namespaces_by_clone() {
        // Before the filter: a clone with CLONE_NEWUSER may work (a container's own profile can forbid it:
        // then the control says so and the case still checks the filter's own errno below).
        let flags = 0x1000_0000usize | libc::SIGCHLD as usize;
        // SAFETY: a raw clone with a new user namespace and no stack: a fork.
        let pid = unsafe { libc::syscall(libc::SYS_clone, flags, 0usize, 0usize, 0usize, 0usize) };
        if pid == 0 {
            // SAFETY: the child of the control clone leaves at once.
            unsafe { libc::_exit(0) };
        }
        println!("  control: clone(CLONE_NEWUSER) before the filter returned {pid}");
        if pid > 0 {
            // SAFETY: reap the control child.
            unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), 0) };
        }
        nnp();
        install(&policy(false)).expect("install");
        // After: the filter's own errno, with no child made.
        for flag in
            [0x0002_0000usize, 0x0200_0000, 0x0400_0000, 0x0800_0000, 0x1000_0000, 0x2000_0000, 0x4000_0000]
        {
            let r = raw_clone(flag | libc::SIGCHLD as usize);
            assert_eq!(r, (-1, i32::from(TEST_ERRNO)), "clone with {flag:#x}");
        }
        // clone3 (any arguments) is ENOSYS, whatever the policy's errno: libc falls back to clone.
        assert_eq!(raw(libc::SYS_clone3 as u32, [0, 0, 0]), (-1, libc::ENOSYS));
        // unshare of a user namespace is on the list too.
        assert_eq!(raw(libc::SYS_unshare as u32, [0x1000_0000, 0, 0]), (-1, i32::from(TEST_ERRNO)));
        // Threads and forks, which libc makes with clone without those flags (and clone3 refused), still work.
        assert_eq!(thread::spawn(|| 7).join().unwrap(), 7);
        let st = Command::new("/bin/true").status();
        assert!(st.is_ok(), "{st:?}");
        // SAFETY: a plain fork whose child exits at once.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            // SAFETY: leave the child.
            unsafe { libc::_exit(0) };
        }
        let mut stt = 0;
        // SAFETY: reap it.
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut stt, 0) }, pid);
        assert_eq!(stt, 0);
    }

    fn raw_clone(flags: usize) -> (isize, i32) {
        // SAFETY: a raw clone that the filter refuses; with no filter it would be a fork.
        let r = unsafe { libc::syscall(libc::SYS_clone, flags, 0usize, 0usize, 0usize, 0usize) };
        (r as isize, io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }

    fn eacces_without_privilege() {
        // No no_new_privs, and no capabilities at all: the install is refused.
        #[repr(C)]
        struct Hdr {
            version: u32,
            pid: i32,
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Data {
            effective: u32,
            permitted: u32,
            inheritable: u32,
        }
        let h = Hdr { version: 0x2008_0522, pid: 0 };
        let d = [Data { effective: 0, permitted: 0, inheritable: 0 }; 2];
        // SAFETY: capset with a header and two data words that live through the call.
        assert_eq!(unsafe { libc::syscall(libc::SYS_capset, &raw const h, d.as_ptr()) }, 0, "capset");
        let e = install(&policy(false)).expect_err("the install must fail");
        assert_eq!(e.raw_os_error(), Some(libc::EACCES), "{e}");
        // Unfiltered: the process goes on as before.
        let (r, e) = raw(libc::SYS_unshare as u32, [0, 0, 0]);
        assert_ne!(e, i32::from(TEST_ERRNO), "a failed install must leave no filter (ret {r})");
    }

    fn production_errno() {
        nnp();
        let n = install(&Policy::production(false)).expect("install");
        assert_eq!(n, refused(&Policy::production(false)));
        assert_eq!(raw(libc::SYS_bpf as u32, [0, 0, 0]), (-1, libc::EPERM));
    }

    #[cfg(target_arch = "x86_64")]
    fn int80() {
        nnp();
        install(&policy(false)).expect("install");
        // The 32-bit ABI: the architecture word differs, so it would walk around the list.
        // SAFETY: int 0x80 getpid (nr 20 in the i386 table); the filter kills the process first.
        unsafe { std::arch::asm!("int 0x80", inout("eax") 20 => _, options(nostack)) };
        panic!("int 0x80 returned");
    }

    #[cfg(target_arch = "x86_64")]
    fn x32() {
        nnp();
        install(&policy(false)).expect("install");
        // The x32 ABI has the native architecture word and a number bit. Not denied by name, killed by bit.
        let _ = raw(0x4000_0000 | libc::SYS_getpid as u32, [0, 0, 0]);
        panic!("an x32 call returned");
    }

    pub fn main() {
        let mut failed = 0;
        let mut total = 0;
        let mut case = |name: &str, want: &dyn Fn(&End) -> bool, body: &dyn Fn()| {
            total += 1;
            let end = child(body);
            if want(&end) {
                println!("ok   {name}");
            } else {
                failed += 1;
                match end {
                    End::Exit(c) => println!("FAIL {name}: exit {c}"),
                    End::Signal(s) => println!("FAIL {name}: signal {s}"),
                }
            }
        };
        let ok = |e: &End| matches!(e, End::Exit(0));
        let sigsys = |e: &End| matches!(e, End::Signal(s) if *s == libc::SIGSYS);
        case(
            "every denied call returns the test errno: installing thread, existing thread (TSYNC), later thread",
            &ok,
            &denied_everywhere,
        );
        case("umount2 allowed without deny_umount", &ok, &|| umount_only_when_asked(false));
        case("umount2 refused with deny_umount", &ok, &|| umount_only_when_asked(true));
        case(
            "what sacdfs does after the mount stays possible (io, thread, fork, exec, a second filter)",
            &ok,
            &what_stays_possible,
        );
        case(
            "clone with a namespace flag and clone3 are refused, threads and forks still work",
            &ok,
            &namespaces_by_clone,
        );
        case(
            "no privilege and no no_new_privs: install fails with EACCES, process unfiltered",
            &ok,
            &eacces_without_privilege,
        );
        case(
            "the production errno is EPERM; install returns the number of denied calls",
            &ok,
            &production_errno,
        );
        #[cfg(target_arch = "x86_64")]
        {
            case("int 0x80 (the 32-bit ABI) is killed with SIGSYS", &sigsys, &int80);
            case("the x32 number bit is killed with SIGSYS", &sigsys, &x32);
        }
        println!("deny_list: {} passed, {failed} failed", total - failed);
        if failed != 0 {
            std::process::exit(1);
        }
    }
}

fn main() {
    #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "x86")))]
    linux::main();
    #[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "x86"))))]
    println!("SKIP: the deny-list filter is written for x86_64 and i686 and is not installed here");
}
