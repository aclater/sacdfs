// SPDX-License-Identifier: GPL-3.0-or-later

//! The unmount helper (src/unmount_helper.rs, included here) against a REAL FUSE mount: the
//! lab test review asked for. It needs /dev/fuse and root with CAP_SYS_ADMIN, so
//! it cannot run in the development sandbox; there it prints SKIP and ends 0, and the
//! lab must show the final line `unmount_helper_fuse: N passed, 0 failed (real FUSE)`.
//!
//!     sudo cargo test --test unmount_helper_fuse -- --nocapture      # or the built binary, as root
//!
//! The pieces: this process plays the daemon (it forks the helper first, while single-threaded,
//! and arms it), and a forked SERVER process mounts a minimal FUSE file system and serves it
//! (the FUSE connection lives in the server, as it would in a daemon's threads). Stopping or
//! killing the server then does to the helper's world what a stopped or killed daemon does:
//! every request to the mount hangs (stopped) or fails with ENOTCONN (killed). The helper must
//! still unmount, within a stated time ([`LIMIT`]), because nothing on its `U` path asks the
//! daemon. The cases:
//!
//!   arm_on_a_live_mount, u_with_a_healthy_server, u_with_a_stopped_server (the wedge is
//!   proven first: a plain `statfs` of the mount does not return), u_with_a_killed_server,
//!   daemon_killed_helper_detaches (end of file), refused_after_the_path_was_remounted (another
//!   FUSE mount of the same type: only the identity differs), daemon_killed_successor_untouched.
//!
//! No test harness (`harness = false`): the helper forks before any thread.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code, dead_code)]

#[cfg(target_os = "linux")]
#[path = "../src/unmount_helper.rs"]
mod unmount_helper;

#[cfg(target_os = "linux")]
mod lab {
    use std::ffi::CString;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use fuser::{Config, Filesystem, MountOption, Session};

    use crate::unmount_helper::{Refused, UnmountHelper};

    /// How long a `U` (or the end-of-file detach) may take. The helper's work is a few system
    /// calls; a request that reached the stopped server would take until the end of the test.
    const LIMIT: Duration = Duration::from_secs(2);

    type Case = (&'static str, fn(&Path));

    struct Nop;
    impl Filesystem for Nop {}

    pub fn main() {
        if !Path::new("/dev/fuse").exists() {
            println!("SKIP: no /dev/fuse here; run it on a lab host as root");
            return;
        }
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            println!(
                "SKIP: not root; run it on a lab host as root (CAP_SYS_ADMIN, mount(2) as fuser does it)"
            );
            return;
        }
        let dir = std::env::temp_dir().join(format!("sacdfs-unmount-helper-fuse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cases: [Case; 7] = [
            ("arm_on_a_live_mount", arm_live),
            ("u_with_a_healthy_server", u_healthy),
            ("u_with_a_stopped_server", u_stopped),
            ("u_with_a_killed_server", u_killed),
            ("daemon_killed_helper_detaches", daemon_killed),
            ("refused_after_the_path_was_remounted", refused_remount),
            ("daemon_killed_successor_untouched", killed_successor),
        ];
        let mut failed = 0;
        for (i, (name, f)) in cases.iter().enumerate() {
            let d = dir.join(format!("c{i}"));
            std::fs::create_dir_all(&d).unwrap();
            let r = std::panic::catch_unwind(|| f(&d));
            println!("test {name} ... {}", if r.is_ok() { "ok" } else { "FAILED" });
            failed += usize::from(r.is_err());
            // Whatever the case left: detached, so that the next one starts clean.
            let _ = std::process::Command::new("umount").args(["-l"]).arg(d.join("m")).status();
        }
        let _ = std::fs::remove_dir_all(&dir);
        println!("\nunmount_helper_fuse: {} passed, {failed} failed (real FUSE)", cases.len() - failed);
        std::process::exit(i32::from(failed != 0));
    }

    fn cstr(p: &Path) -> CString {
        use std::os::unix::ffi::OsStrExt;
        CString::new(p.as_os_str().as_bytes()).unwrap()
    }

    fn mount_line(m: &Path) -> Option<String> {
        let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
        info.lines().rev().find(|l| l.split(' ').nth(4) == Some(m.to_str().unwrap())).map(str::to_owned)
    }

    fn is_mounted(m: &Path) -> bool {
        mount_line(m).is_some()
    }

    fn wait_until(what: &str, limit: Duration, mut f: impl FnMut() -> bool) -> Duration {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < limit, "{what}: not within {limit:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
        t.elapsed()
    }

    /// Whether `pid` runs: a zombie does not count (a helper orphaned by a killed daemon waits
    /// for an init to reap it, and `kill(pid, 0)` still succeeds on it; the lab rig needs no
    /// `--init` for this).
    fn alive(pid: i32) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
        stat.rsplit_once(") ").is_some_and(|(_, rest)| !rest.starts_with('Z'))
    }

    fn reap(pid: i32) -> bool {
        // SAFETY: waitpid on our own child, without blocking.
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) == pid }
    }

    /// Forks the SERVER: it mounts `m` as a minimal FUSE file system and serves it until the mount
    /// goes. Returns its pid once the mount exists.
    fn start_server(m: &Path) -> i32 {
        let mut p = [0; 2];
        // SAFETY: a pipe into a local array.
        assert_eq!(unsafe { libc::pipe(p.as_mut_ptr()) }, 0);
        // SAFETY: fork while this process has a single thread.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            in_child(|| {
                let mut config = Config::default();
                config.mount_options = vec![
                    MountOption::FSName("sacdfs-lab".to_owned()),
                    MountOption::Subtype("sacdfs".to_owned()),
                ];
                let session = Session::new(Nop, m, &config).expect("mount");
                // SAFETY: one byte to the parent, from the stack.
                unsafe { libc::write(p[1], [1u8].as_ptr().cast(), 1) };
                let _ = session.run();
            });
        }
        // SAFETY: the parent's copy of the write end, closed so that a dead server ends the read.
        unsafe { libc::close(p[1]) };
        let mut b = [0u8; 1];
        // SAFETY: one byte from the server into the stack.
        assert_eq!(unsafe { libc::read(p[0], b.as_mut_ptr().cast(), 1) }, 1, "the server mounted");
        assert!(is_mounted(m));
        pid
    }

    /// In a forked child: runs `f` and never returns into the test loop, whatever happens (a panic
    /// would otherwise run the remaining tests in the child while the parent waits for it).
    fn in_child(f: impl FnOnce()) -> ! {
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_ok();
        // SAFETY: leave without running the parent's atexit handlers.
        unsafe { libc::_exit(if ok { 0 } else { 70 }) }
    }

    fn kill(pid: i32, sig: i32) {
        // SAFETY: a signal to our own child.
        unsafe { libc::kill(pid, sig) };
    }

    /// This process as the daemon: the helper (forked first), then the server, then `M`.
    fn rig(d: &Path) -> (UnmountHelper, i32, PathBuf, u8) {
        let m = d.join("m");
        std::fs::create_dir_all(&m).unwrap();
        let mut h = UnmountHelper::spawn(&m, "sacdfs-lab").unwrap().expect("root with CAP_SYS_ADMIN");
        let server = start_server(&m);
        let level = h.mounted().unwrap();
        (h, server, m, level)
    }

    /// A child that does `statfs` of `m` (a request to the file system's server) and exits 0.
    /// Returns whether it finished within `wait`; it is killed if not.
    fn statfs_returns(m: &Path, wait: Duration) -> bool {
        // SAFETY: fork while this process has a single thread; the child makes one call and exits.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: a statfs into a local buffer, then exit.
            unsafe {
                let mut s: libc::statfs = std::mem::zeroed();
                libc::statfs(cstr(m).as_ptr(), &raw mut s);
                libc::_exit(0)
            }
        }
        let t = Instant::now();
        while t.elapsed() < wait {
            if reap(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        kill(pid, libc::SIGKILL);
        // A bounded wait: a request already in the server's hands is waited out by the kernel, and
        // an unbounded waitpid would then hang the whole run instead of failing it.
        wait_until("the killed prober is reaped", Duration::from_secs(5), || reap(pid));
        false
    }

    fn arm_live(d: &Path) {
        let (mut h, server, m, level) = rig(d);
        println!("  armed at proof level {level}; helper pid {}", h.pid());
        assert!((1..=3).contains(&level));
        let status = |k: &str| {
            let s = std::fs::read_to_string(format!("/proc/{}/status", h.pid())).unwrap();
            s.lines().find_map(|l| l.strip_prefix(k)).unwrap().trim().to_owned()
        };
        assert_eq!(status("CapEff:"), "0000000000200000");
        assert_eq!(status("Seccomp:"), "2");
        // Five descriptors: 0-2 on /dev/null, the socket (3), the mount table (4).
        let fds = std::fs::read_dir(format!("/proc/{}/fd", h.pid())).unwrap().count();
        assert_eq!(fds, 5, "the helper's descriptors");
        println!("  helper degraded mask {} (1 = bounding set not cut: no CAP_SETPCAP)", h.degraded());
        h.stand_down();
        assert!(is_mounted(&m), "stand down leaves the mount");
        kill(server, libc::SIGKILL);
        reap(server);
    }

    fn u_healthy(d: &Path) {
        let (mut h, server, m, level) = rig(d);
        let t = Instant::now();
        h.detach().unwrap();
        println!("  level {level}: U with a healthy server took {:?}", t.elapsed());
        assert!(t.elapsed() < LIMIT);
        assert!(!is_mounted(&m));
        wait_until("the server ends with the mount", Duration::from_secs(5), || reap(server));
    }

    /// Waits until EVERY thread of `pid` is stopped (state T). `kill(SIGSTOP)` returns before the
    /// stop has happened, and a group stop reaches each thread at its own pace (the main thread
    /// can show T while another is still inside its read of /dev/fuse): a thread that is still
    /// running can take a request and then stop with it unanswered, and a request that is in
    /// the server's hands cannot be interrupted by a signal (the caller waits it out). That made
    /// the wedge probe below hang in about one run in ten on the lab kernel, so the probe starts
    /// only after every thread is seen stopped, and the wait for the probe is bounded.
    fn wait_stopped(pid: i32) {
        let all_stopped = || {
            let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else { return false };
            let mut n = 0;
            for t in tasks.flatten() {
                let Ok(stat) = std::fs::read_to_string(t.path().join("stat")) else { return false };
                if !stat.rsplit_once(") ").is_some_and(|(_, rest)| rest.starts_with('T')) {
                    return false;
                }
                n += 1;
            }
            n > 0
        };
        wait_until("every thread of the server is stopped", Duration::from_secs(5), all_stopped);
    }

    fn u_stopped(d: &Path) {
        let (mut h, server, m, level) = rig(d);
        kill(server, libc::SIGSTOP);
        wait_stopped(server);
        // The wedge is real: a request to the mount does not come back.
        assert!(
            !statfs_returns(&m, Duration::from_secs(1)),
            "setup: the mount still answers with its server stopped"
        );
        let t = Instant::now();
        h.detach().unwrap();
        println!("  level {level}: U with a STOPPED server took {:?}", t.elapsed());
        assert!(t.elapsed() < LIMIT, "U waited on the daemon");
        assert!(!is_mounted(&m));
        kill(server, libc::SIGKILL);
        wait_until("the server is reaped", Duration::from_secs(5), || reap(server));
    }

    fn u_killed(d: &Path) {
        let (mut h, server, m, level) = rig(d);
        kill(server, libc::SIGKILL);
        wait_until("the server is gone", Duration::from_secs(5), || reap(server));
        assert!(is_mounted(&m), "a killed server leaves a dead mount");
        // Dead: a request fails at once (ENOTCONN).
        assert!(statfs_returns(&m, Duration::from_secs(1)), "setup: a dead mount answers with an error");
        let t = Instant::now();
        h.detach().unwrap();
        println!("  level {level}: U with a KILLED server took {:?}", t.elapsed());
        assert!(t.elapsed() < LIMIT);
        assert!(!is_mounted(&m));
    }

    /// A forked daemon that arms the helper and waits to be killed; returns (daemon, helper, server).
    fn fork_daemon(d: &Path) -> (i32, i32, i32) {
        let m = d.join("m");
        std::fs::create_dir_all(&m).unwrap();
        let mut p = [0; 2];
        // SAFETY: a pipe into a local array.
        assert_eq!(unsafe { libc::pipe(p.as_mut_ptr()) }, 0);
        // SAFETY: fork while this process has a single thread.
        let daemon = unsafe { libc::fork() };
        if daemon == 0 {
            in_child(|| {
                // The SERVER first, then the helper (rig fix, lab run 2026-11-25, case 5): the
                // server is a fork of this process, and one forked after the helper's socket
                // exists inherits this end of it. Killing the daemon then gave the helper no end
                // of file, because the server still held the socket. A real sacdfs forks nothing
                // after the helper (and the socket is close-on-exec for what it execs).
                let server = start_server(&m);
                let mut h =
                    std::mem::ManuallyDrop::new(UnmountHelper::spawn(&m, "sacdfs-lab").unwrap().unwrap());
                h.mounted().unwrap();
                let mut b = [0u8; 8];
                b[..4].copy_from_slice(&h.pid().to_le_bytes());
                b[4..].copy_from_slice(&server.to_le_bytes());
                // SAFETY: eight bytes to the test, then waiting to be killed.
                unsafe {
                    libc::write(p[1], b.as_ptr().cast(), 8);
                    libc::pause();
                    libc::_exit(0)
                }
            });
        }
        // SAFETY: the parent's copy of the write end, closed so that a dead daemon ends the read.
        unsafe { libc::close(p[1]) };
        let mut b = [0u8; 8];
        // SAFETY: eight bytes into the stack.
        assert_eq!(unsafe { libc::read(p[0], b.as_mut_ptr().cast(), 8) }, 8);
        (
            daemon,
            i32::from_le_bytes(b[..4].try_into().unwrap()),
            i32::from_le_bytes(b[4..].try_into().unwrap()),
        )
    }

    fn daemon_killed(d: &Path) {
        let m = d.join("m");
        let (daemon, helper, server) = fork_daemon(d);
        assert!(is_mounted(&m));
        let t = Instant::now();
        kill(daemon, libc::SIGKILL);
        wait_until("the mount goes", LIMIT, || !is_mounted(&m));
        println!("  mount gone {:?} after the daemon was killed (end of file path)", t.elapsed());
        wait_until("the helper ends", LIMIT, || !alive(helper));
        kill(server, libc::SIGKILL);
        // The daemon is our child; the server is its child (rig fix, case 7): we cannot reap it
        // (the init that adopts it can), so nothing here waits for it.
        // SAFETY: reaping our child.
        unsafe { libc::waitpid(daemon, std::ptr::null_mut(), 0) };
    }

    /// After the path was remounted with something else of the SAME type: refused, with both
    /// identities, the newcomer untouched.
    fn refused_remount(d: &Path) {
        let (mut h, server, m, level) = rig(d);
        // SAFETY: a plain unmount of the first mount, as `umount` would.
        assert_eq!(unsafe { libc::umount2(cstr(&m).as_ptr(), 0) }, 0, "{}", std::io::Error::last_os_error());
        wait_until("the first server ends", Duration::from_secs(5), || reap(server));
        let second = start_server(&m);
        let e = h.detach().unwrap_err();
        let r = e
            .get_ref()
            .and_then(|x| x.downcast_ref::<Refused>())
            .unwrap_or_else(|| panic!("not a refusal: {e}"));
        println!("  level {level}: {r}");
        assert_ne!(r.recorded, r.found);
        assert!(is_mounted(&m), "the newcomer must be left alone");
        kill(second, libc::SIGKILL);
        wait_until("the second server is reaped", Duration::from_secs(5), || reap(second));
    }

    fn killed_successor(d: &Path) {
        let m = d.join("m");
        let (daemon, helper, server) = fork_daemon(d);
        // SAFETY: a plain unmount of the first mount.
        assert_eq!(unsafe { libc::umount2(cstr(&m).as_ptr(), 0) }, 0, "{}", std::io::Error::last_os_error());
        // The first server is the daemon's child, not ours (rig fix, case 7): `waitpid` cannot
        // reap it, so wait for it to end (a zombie counts as ended) instead.
        wait_until("the first server ends", Duration::from_secs(5), || !alive(server));
        let second = start_server(&m);
        kill(daemon, libc::SIGKILL);
        wait_until("the helper ends", LIMIT, || !alive(helper));
        assert!(is_mounted(&m), "the successor must stay");
        kill(second, libc::SIGKILL);
        // SAFETY: reaping our children.
        unsafe {
            libc::waitpid(daemon, std::ptr::null_mut(), 0);
            libc::waitpid(second, std::ptr::null_mut(), 0);
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    lab::main();
}
