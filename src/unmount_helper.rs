// SPDX-License-Identifier: GPL-3.0-or-later

//! The unmount helper: a tiny process forked before any thread exists, which keeps
//! CAP_SYS_ADMIN so that the daemon can give it up (hardening step 1, docs/design.md).
//!
//! # What it can do
//!
//! Unmount ONE path: the mount point given at fork, never updated. The daemon is the less
//! trusted side, so nothing it sends names a path or carries an argument: a command is one
//! byte. `M` arms the helper once (a second `M` is refused), `U` detaches the armed mount and
//! ends the helper, `Q` stands it down. End of file on the socket with the helper armed (the
//! daemon died, or let go of its handle without `stand_down`: there is deliberately no
//! `Drop`) is a `U`. Replies are one status byte (0 or an errno); `M` adds the proof level, and
//! a refused `U` adds both identities (recorded and found) so that the daemon can log them.
//!
//! # What it checks, and when
//!
//! At `M` (the mount exists, the session is not served yet), by the helper itself and not on the
//! daemon's word: the file system type of the mount at the path is FUSE, read from
//! `/proc/self/mountinfo` through a descriptor opened at start (the kernel answers without
//! the daemon); and the mount's identity is recorded. At `U` the path is opened `O_PATH` and
//! the identity must be the recorded one; a mismatch is refused, reported with both
//! identities, and the helper ends: no retry, no re-arming.
//!
//! The identity, by the best proof the kernel has (the level is reported to the daemon, which
//! logs it once at arming):
//! - level 3: `STATX_MNT_ID_UNIQUE` (Linux 6.8+), a 64-bit number that is never reused;
//! - level 2: `STATX_MNT_ID` (5.8+) and the device number, and the type again from the mount table.
//!   Both numbers CAN be reused, and were, by the next mount on the same path (measured, Linux
//!   6.12: the same mount ID and device number), so a successor of the same type would pass. It takes the original to be
//!   unmounted from outside and a file system with an anonymous device number to be mounted on
//!   the same path while the daemon is dead or gone, which needs the right to mount, i.e. root;
//! - level 1: `statx` without a mount ID (4.11 to 5.7): the device number, and the mount ID
//!   and type of the `/proc/self/mountinfo` line of that device, which must still exist. While
//!   the original mount exists no other can have its device number, so this is sound unless
//!   the original is gone, as for level 2;
//! - none: no `statx` (before 4.11): no check free of the daemon exists (`fstat` on a FUSE mount
//!   asks the daemon), so the helper declines to arm and the daemon unmounts by itself. Device
//!   number alone is not used.
//!
//! No call on the `U` path reaches the daemon: `openat(O_PATH)` walks to the mount's root
//! without asking the file system, `statx` with `AT_STATX_DONT_SYNC` answers from the kernel's
//! own state (the mount ID is the VFS's, not the file system's), `umount2` and `close` do not
//! either, and no `fstatfs` (a request to the daemon, which hangs when the daemon is wedged and
//! says ENOTCONN when it is dead) is made at all. That claim is checked in the lab with a daemon
//! that is stopped and one that is killed (`tests/unmount_helper_fuse.rs`).
//!
//! # How it unmounts
//!
//! `umount2("/proc/self/fd/N/", MNT_DETACH | UMOUNT_NOFOLLOW)` (and, only after EINVAL, `MNT_DETACH` alone: Linux 4.18
//! refuses `UMOUNT_NOFOLLOW` on this path), N being the descriptor it just
//! checked: the path walk of the unmount ends at the mount that was checked. `MNT_DETACH`, not
//! a plain unmount: a stop with files still open must succeed (#276; sacdfs then serves them
//! for the grace period), and the helper has no way to retry or to tell busy from idle without
//! a second call. Two measured facts (Linux 6.12): without the trailing slash `UMOUNT_NOFOLLOW`
//! on `/proc/self/fd/N` fails with EINVAL (the magic link is not followed); with it, it works.
//! The gap between the check and the unmount is accepted: using it takes the right to mount
//! over that path, which is already root.
//!
//! It never holds a descriptor on the mount between commands: an open `O_PATH` descriptor pins
//! the mount, and `fusermount3 -u` from outside would then fail with EBUSY.
//!
//! # How it is hardened
//!
//! Before it reads anything: every descriptor but the socket (3) and mountinfo (4) closed,
//! `chdir /`, stop signals ignored (a service manager signals the whole group, and the helper
//! has to outlive the signal), `PR_SET_DUMPABLE 0` (checked back) and no_new_privs, permitted and
//! effective sets cut to CAP_SYS_ADMIN, inheritable and ambient cleared, the bounding set cut to
//! CAP_SYS_ADMIN (needs CAP_SETPCAP: without it only this step is reported as degraded), then
//! a seccomp filter that kills the process on any system call but `read`, `write`, `close`,
//! `exit_group`, `statx`, `lseek` (on descriptor 4 only), `openat` (flags pinned to
//! `O_PATH|O_DIRECTORY|O_CLOEXEC|O_NOFOLLOW`) and `umount2` (flags pinned). No timers, no
//! signals, no network, no allocation.
//!
//! Once the daemon has dropped CAP_SYS_ADMIN, the helper's permitted set is NOT a subset of
//! the daemon's, and that is the point of it. Dumpable 0 is what keeps the daemon (same uid)
//! from `ptrace`-ing the helper or writing its memory through `/proc/PID/mem`; the seccomp
//! filter and the one-byte commands are what is left to attack. Do not weaken either without
//! redoing that argument.
//!
//! Fork before any thread: [`UnmountHelper::spawn`] refuses when another thread exists.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

/// The file system type of a sacdfs mount in `/proc/self/mountinfo`: `fuse` or `fuse.SUBTYPE`.
pub const FUSE_TYPE: &str = "fuse";

/// A mount's identity as the helper records and compares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The mount ID (the unique one at level 3).
    pub mount_id: u64,
    /// The device number (`makedev`).
    pub dev: u64,
}

impl std::fmt::Display for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mount id {} device {}:{}", self.mount_id, libc::major(self.dev), libc::minor(self.dev))
    }
}

/// A `U` that was refused because the path is not the mount that was armed.
#[derive(Debug)]
pub struct Refused {
    pub recorded: Identity,
    pub found: Identity,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refused, the mount at the path is not the one armed: armed {}, found {}",
            self.recorded, self.found
        )
    }
}

impl std::error::Error for Refused {}

const E_WRONG_FS: u8 = libc::ENOTSUP as u8;
const E_MISMATCH: u8 = libc::ESTALE as u8;
const E_ARMED: u8 = libc::EALREADY as u8;
const E_NOT_ARMED: u8 = libc::EINVAL as u8;
/// No daemon-free proof exists on this kernel.
const E_NO_PROOF: u8 = libc::ENOSYS as u8;

const CMD_MOUNTED: u8 = b'M';
const CMD_UNMOUNT: u8 = b'U';
const CMD_QUIT: u8 = b'Q';

/// Status bit of the first byte the helper sends: the bounding set could not be cut. 0: fully hardened.
pub const DEGRADED_BOUNDING_SET: u8 = 1;
/// Status bit: the helper could not harden itself and has exited (the low bits tell where).
const FATAL: u8 = 0x80;
const STEP_DUMPABLE: u8 = 1;
const STEP_NNP: u8 = 2;
const STEP_CAPSET: u8 = 3;
const STEP_SECCOMP: u8 = 4;
/// `/proc/self/mountinfo` cannot be opened: the helper could not check anything.
const STEP_MOUNTINFO: u8 = 5;

const CAP_SYS_ADMIN: u32 = 21;
const UMOUNT_NOFOLLOW: libc::c_int = 8;
const OPEN_FLAGS: libc::c_int = libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
const UMOUNT_FLAGS: libc::c_int = libc::MNT_DETACH | UMOUNT_NOFOLLOW;
/// The second form, for kernels where `UMOUNT_NOFOLLOW` on the descriptor's path fails with EINVAL even with the
/// trailing slash (measured: Linux 4.18, RHEL 8.10; it works on 5.14 and 6.12). The path is the magic link to the
/// descriptor the helper just checked, which is the one thing to follow, so the flag only adds a refusal to follow
/// a link at the end of the path; the retry is made only after EINVAL.
const UMOUNT_FLAGS_OLD_KERNEL: libc::c_int = libc::MNT_DETACH;
/// The descriptors of the helper: the socket and `/proc/self/mountinfo`.
const SOCK_FD: libc::c_int = 3;
const MOUNTINFO_FD: libc::c_int = 4;

/// The daemon's handle on its helper.
#[derive(Debug)]
pub struct UnmountHelper {
    sock: OwnedFd,
    pid: libc::pid_t,
    status: u8,
    done: bool,
}

impl UnmountHelper {
    /// Forks the helper for the mount point `mountpoint` (canonical, as the daemon mounts
    /// there). `Ok(None)`: not needed (no CAP_SYS_ADMIN here, as for a rootless mount, whose
    /// unmount is `fusermount3`'s) or not supported on this architecture. `Err`: it could not
    /// be started, or another thread already exists; the caller carries on without it.
    ///
    /// `source` is the name the mount is listed under in the mount table (what the daemon mounts
    /// with as its file system name): checked at arming, and again at `U` where the mount table
    /// is consulted (levels 2 and 1), so that a successor under another name is refused.
    pub fn spawn(mountpoint: &Path, source: &str) -> io::Result<Option<UnmountHelper>> {
        Self::spawn_for(mountpoint, FUSE_TYPE, source, 3)
    }

    /// [`Self::spawn`] with the file system type to expect, and the best proof level the helper
    /// may use (3, 2 or 1): the tests use tmpfs, and each level on a kernel that has all. Not
    /// public: in the shipped binary nothing but [`Self::spawn`] chooses what is checked.
    #[allow(unsafe_code)]
    pub(crate) fn spawn_for(
        mountpoint: &Path,
        fstype: &str,
        source: &str,
        max_level: u8,
    ) -> io::Result<Option<UnmountHelper>> {
        use std::os::unix::ffi::OsStrExt;
        let Some(filter) = filter() else { return Ok(None) };
        if !has_sys_admin()? {
            return Ok(None);
        }
        let threads = std::fs::read_dir("/proc/self/task")?.count();
        if threads != 1 {
            return Err(io::Error::other(format!(
                "{threads} threads exist: the unmount helper forks before any"
            )));
        }
        let path = CString::new(mountpoint.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mount point holds a NUL byte"))?;
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: fds is a valid array of two ints for socketpair to fill.
        if unsafe {
            libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr())
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: socketpair returned two new descriptors that nothing else owns.
        let (mine, theirs) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let target = Target {
            path,
            fstype: fstype.as_bytes().to_vec(),
            source: source.as_bytes().to_vec(),
            max_level,
        };
        // SAFETY: a single thread exists (checked above), so the child may run any code.
        match unsafe { libc::fork() } {
            -1 => Err(io::Error::last_os_error()),
            0 => {
                drop(mine);
                child(theirs, &target, &filter)
            }
            pid => {
                drop(theirs);
                let mut h = UnmountHelper { sock: mine, pid, status: 0, done: false };
                // Before the first read: a helper stuck before its status byte must not hang the start.
                h.set_timeout();
                let mut b = [0u8; 1];
                match h.recv(&mut b) {
                    Ok(1) if b[0] & FATAL == 0 => {
                        h.status = b[0];
                        Ok(Some(h))
                    }
                    Ok(1) => {
                        h.reap();
                        Err(io::Error::other(format!(
                            "the unmount helper could not harden itself (step {})",
                            b[0] & 0x7f
                        )))
                    }
                    _ => {
                        h.reap();
                        Err(io::Error::other("the unmount helper died while starting"))
                    }
                }
            }
        }
    }

    /// A duplicate of the socket to the helper, for a thread that waits for its end (the helper
    /// ended: killed, stopped by nobody, or crashed). The thread only polls it, never reads.
    pub fn death_watch_fd(&self) -> io::Result<OwnedFd> {
        self.sock.try_clone()
    }

    /// The helper's process id.
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// 0 when fully hardened, else a mask of the steps that degraded ([`DEGRADED_BOUNDING_SET`]).
    pub fn degraded(&self) -> u8 {
        self.status
    }

    /// Arms the helper for the mount now at its path: it checks the type and records the
    /// identity. Call it right after the mount, before serving. Returns the proof level (3 the
    /// best, see the module documentation) for the daemon to log once. Errors: `ENOTSUP` the
    /// mount is not of the expected type, `ENOSYS` the kernel has no proof free of the daemon,
    /// `EALREADY` armed before. On an error the helper stays unarmed (and still answers `Q`).
    pub fn mounted(&mut self) -> io::Result<u8> {
        let mut extra = [0u8; 1];
        self.request(CMD_MOUNTED, &mut extra)?;
        Ok(extra[0])
    }

    /// Detaches the mount (`MNT_DETACH`) and ends the helper. The error is the helper's errno,
    /// [`Refused`] (inside the `io::Error`) when the path is no longer the mount that was armed,
    /// or the failure of the socket (the helper is gone, or no answer in 5 s).
    pub fn detach(&mut self) -> io::Result<()> {
        let mut extra = [0u8; 0];
        let r = self.request(CMD_UNMOUNT, &mut extra);
        // The helper ends after every answer to `U` but "not armed" (EINVAL), refusals included.
        let stays = matches!(&r, Err(e) if e.raw_os_error() == Some(libc::EINVAL));
        if !stays {
            self.reap();
        }
        r
    }

    /// The daemon unmounted by itself or is leaving cleanly: the helper has nothing to do.
    pub fn stand_down(&mut self) {
        let _ = self.request(CMD_QUIT, &mut [0u8; 0]);
        self.reap();
    }

    #[allow(unsafe_code)]
    fn send(&self, buf: &[u8]) -> io::Result<()> {
        // SAFETY: a valid buffer of buf.len() bytes and an open socket.
        let n =
            unsafe { libc::send(self.sock.as_raw_fd(), buf.as_ptr().cast(), buf.len(), libc::MSG_NOSIGNAL) };
        if n == buf.len() as isize { Ok(()) } else { Err(io::Error::last_os_error()) }
    }

    #[allow(unsafe_code)]
    fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            // SAFETY: a valid buffer of buf.len() bytes and an open socket.
            let n = unsafe { libc::recv(self.sock.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n >= 0 {
                return Ok(n as usize);
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    /// No answer for 5 s is an error, for every read: the first (status) byte included.
    #[allow(unsafe_code)]
    fn set_timeout(&self) {
        let tv = libc::timeval { tv_sec: 5, tv_usec: 0 };
        // SAFETY: a valid timeval of the size given, on an open socket.
        unsafe {
            libc::setsockopt(
                self.sock.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&raw const tv).cast(),
                size_of::<libc::timeval>() as libc::socklen_t,
            );
        }
    }

    /// Reads exactly `buf.len()` bytes of the helper's reply.
    fn recv_all(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut have = 0;
        while have < buf.len() {
            match self.recv(&mut buf[have..]) {
                Ok(0) => {
                    self.done = true;
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the unmount helper is gone"));
                }
                Ok(n) => have += n,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Sends the one-byte command and reads the reply: the status byte, then what the command
    /// adds on success (`extra`) or, for a refused `U`, both identities.
    #[allow(unsafe_code)]
    fn request(&mut self, cmd: u8, extra: &mut [u8]) -> io::Result<()> {
        if self.done {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "the unmount helper has ended"));
        }
        self.set_timeout();
        if let Err(e) = self.send(&[cmd]) {
            self.done = true;
            return Err(e);
        }
        let mut b = [0u8; 1];
        self.recv_all(&mut b)?;
        match b[0] {
            0 => self.recv_all(extra),
            E_MISMATCH if cmd == CMD_UNMOUNT => {
                let mut ids = [0u8; 32];
                self.recv_all(&mut ids)?;
                let n = |i: usize| u64::from_le_bytes(ids[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
                Err(io::Error::other(Refused {
                    recorded: Identity { mount_id: n(0), dev: n(1) },
                    found: Identity { mount_id: n(2), dev: n(3) },
                }))
            }
            e => Err(io::Error::from_raw_os_error(i32::from(e))),
        }
    }

    /// Reaps the helper if it has ended (never blocks, never waits on a stuck one).
    #[allow(unsafe_code)]
    fn reap(&mut self) {
        for _ in 0..50 {
            // SAFETY: waitpid on our own child, with a null status pointer.
            let r = unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), libc::WNOHANG) };
            if r != 0 {
                self.done = true;
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// Blocks until the other end of `fd` (a duplicate of the helper's socket) is closed: the helper
/// ended. Polls for the hang-up only, and never reads, so it takes nothing from the daemon's own
/// requests on the same socket.
#[allow(unsafe_code)]
pub fn wait_for_hangup(fd: &OwnedFd) {
    let mut p = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLRDHUP, revents: 0 };
    loop {
        // SAFETY: poll on one valid descriptor, no timeout.
        let r = unsafe { libc::poll(&raw mut p, 1, -1) };
        if r >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
    }
}

/// Whether CAP_SYS_ADMIN is in this thread's effective set.
#[allow(unsafe_code)]
fn has_sys_admin() -> io::Result<bool> {
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
    let mut h = Header { version: 0x2008_0522, pid: 0 };
    let mut d = [Data::default(); 2];
    // SAFETY: capget with a version-3 header and room for its two data words.
    let r = unsafe { libc::syscall(libc::SYS_capget, &raw mut h, d.as_mut_ptr()) };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(d[0].effective & (1 << CAP_SYS_ADMIN) != 0)
}

/// The open descriptors from `first` up, as /proc/self/fd lists them (before the filter: it allocates).
fn open_fds_from(first: libc::c_int) -> Vec<libc::c_int> {
    let Ok(dir) = std::fs::read_dir("/proc/self/fd") else { return (first..4096).collect() };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).filter(|&fd| fd >= first).collect()
}

/// What the helper is for, fixed at fork.
struct Target {
    path: CString,
    /// The file system type to expect (`fuse`: `fuse` or `fuse.SUBTYPE`).
    fstype: Vec<u8>,
    /// The mount's name in the mount table (its file system name).
    source: Vec<u8>,
    max_level: u8,
}

/// What the helper recorded at `M`.
#[derive(Clone, Copy)]
struct Armed {
    level: u8,
    id: Identity,
}

// Every system call the filter constrains, and every call made once it is on, is a RAW system
// call, not libc's wrapper: a wrapper may change the arguments the filter pins (musl's `openat`
// ORs in `O_LARGEFILE`, 0x8000 on 32-bit x86, which would kill the helper at its first `openat`),
// and may make other calls than the one named. Errors come back through `errno()`.

#[allow(unsafe_code)]
fn sys_read(fd: libc::c_int, buf: *mut u8, n: usize) -> isize {
    // SAFETY: `buf` is valid for `n` bytes (every caller passes a buffer of its own).
    unsafe { libc::syscall(libc::SYS_read, libc::c_long::from(fd), buf, n) as isize }
}

#[allow(unsafe_code)]
fn sys_write(fd: libc::c_int, buf: &[u8]) -> isize {
    // SAFETY: a valid buffer of `buf.len()` bytes.
    unsafe { libc::syscall(libc::SYS_write, libc::c_long::from(fd), buf.as_ptr(), buf.len()) as isize }
}

#[allow(unsafe_code)]
fn sys_close(fd: libc::c_int) {
    // SAFETY: closing a descriptor of this process.
    unsafe { libc::syscall(libc::SYS_close, libc::c_long::from(fd)) };
}

#[allow(unsafe_code)]
fn sys_openat(path: &CString) -> libc::c_int {
    // SAFETY: a NUL-terminated path; the flags are exactly the ones the filter pins.
    unsafe {
        libc::syscall(
            libc::SYS_openat,
            libc::c_long::from(libc::AT_FDCWD),
            path.as_ptr(),
            libc::c_long::from(OPEN_FLAGS),
            0 as libc::c_long,
        ) as libc::c_int
    }
}

#[allow(unsafe_code)]
fn sys_umount2(path: *const u8, flags: libc::c_int) -> libc::c_int {
    // SAFETY: a NUL-terminated path in a buffer that outlives the call; the flags are one of the two pinned ones.
    unsafe { libc::syscall(libc::SYS_umount2, path, libc::c_long::from(flags)) as libc::c_int }
}

#[allow(unsafe_code)]
fn sys_exit(code: libc::c_int) -> ! {
    loop {
        // SAFETY: ends the process; the loop is for the type only.
        unsafe { libc::syscall(libc::SYS_exit_group, libc::c_long::from(code)) };
    }
}

/// Replies and exits, in the helper.
#[allow(unsafe_code)]
fn reply(bytes: &[u8]) {
    // A failed write ends the loop's next read.
    sys_write(SOCK_FD, bytes);
}

/// The helper process, after fork. Never returns. Nothing in it allocates once the filter is on.
#[allow(unsafe_code, clippy::too_many_lines)]
fn child(sock: OwnedFd, target: &Target, filter: &[libc::sock_filter]) -> ! {
    let sock = sock.as_raw_fd();
    // SAFETY: only raw system calls and plain data from here on; the process has one thread.
    unsafe {
        if sock != SOCK_FD {
            libc::dup2(sock, SOCK_FD);
        }
        // Every descriptor of the daemon closed (the notify pipe to the waiting parent
        // least), then the two the helper keeps, then 0..2 on /dev/null.
        if libc::syscall(libc::SYS_close_range, 4u32, u32::MAX, 0u32) != 0 {
            // No close_range (before Linux 5.9): by what /proc/self/fd lists.
            for fd in open_fds_from(4) {
                libc::close(fd);
            }
        }
        let mi = libc::open(c"/proc/self/mountinfo".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
        if mi < 0 {
            fatal(STEP_MOUNTINFO);
        }
        if mi != MOUNTINFO_FD {
            libc::dup2(mi, MOUNTINFO_FD);
            libc::close(mi);
        }
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if null >= 0 {
            for fd in 0..3 {
                libc::dup2(null, fd);
            }
            if null > MOUNTINFO_FD {
                libc::close(null);
            }
        }
        libc::chdir(c"/".as_ptr());
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGPIPE, libc::SIGQUIT] {
            libc::signal(sig, libc::SIG_IGN);
        }
        let mut status = 0u8;
        fn fatal(step: u8) -> ! {
            sys_write(SOCK_FD, &[FATAL | step]);
            sys_exit(1)
        }
        if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0
            || libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) != 0
        {
            fatal(STEP_DUMPABLE);
        }
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            fatal(STEP_NNP);
        }
        // Ambient set: absent before Linux 4.3, and then nothing to clear.
        libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0);
        // The bounding set first: dropping from it needs CAP_SETPCAP in the effective set,
        // which the capset below removes.
        let mut cap = 0;
        while libc::prctl(libc::PR_CAPBSET_READ, cap, 0, 0, 0) >= 0 {
            if cap != CAP_SYS_ADMIN as i32 && libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) != 0 {
                status |= DEGRADED_BOUNDING_SET;
            }
            cap += 1;
        }
        #[repr(C)]
        struct Header {
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
        let h = Header { version: 0x2008_0522, pid: 0 };
        let mut d =
            [Data { effective: 1 << CAP_SYS_ADMIN, permitted: 1 << CAP_SYS_ADMIN, inheritable: 0 }; 2];
        d[1] = Data { effective: 0, permitted: 0, inheritable: 0 };
        if libc::syscall(libc::SYS_capset, &raw const h, d.as_ptr()) != 0 {
            fatal(STEP_CAPSET);
        }
        let prog = libc::sock_fprog { len: filter.len() as u16, filter: filter.as_ptr().cast_mut() };
        if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &raw const prog) != 0 {
            fatal(STEP_SECCOMP);
        }
        // From here on the filter is on: raw calls, stack data only.
        reply(&[status]);
        let mut armed: Option<Armed> = None;
        loop {
            let mut cmd = [0u8; 1];
            let n = loop {
                let n = sys_read(SOCK_FD, cmd.as_mut_ptr(), 1);
                if n >= 0 || *libc::__errno_location() != libc::EINTR {
                    break n;
                }
            };
            if n <= 0 {
                // The daemon is gone (or died): the same as `U`, with nobody to tell.
                if let Some(a) = armed {
                    let _ = unmount(target, &a);
                }
                sys_exit(0);
            }
            match cmd[0] {
                CMD_MOUNTED if armed.is_some() => reply(&[E_ARMED]),
                CMD_MOUNTED => match arm(target) {
                    Ok(a) => {
                        armed = Some(a);
                        reply(&[0, a.level]);
                    }
                    Err(e) => reply(&[e]),
                },
                CMD_UNMOUNT => {
                    let Some(a) = armed else {
                        reply(&[E_NOT_ARMED]);
                        continue;
                    };
                    match unmount(target, &a) {
                        Ok(()) => reply(&[0]),
                        Err(Fail::Errno(e)) => reply(&[e]),
                        Err(Fail::Mismatch(found)) => {
                            let mut b = [0u8; 33];
                            b[0] = E_MISMATCH;
                            b[1..9].copy_from_slice(&a.id.mount_id.to_le_bytes());
                            b[9..17].copy_from_slice(&a.id.dev.to_le_bytes());
                            b[17..25].copy_from_slice(&found.mount_id.to_le_bytes());
                            b[25..33].copy_from_slice(&found.dev.to_le_bytes());
                            reply(&b);
                        }
                    }
                    // One `U`: refused or done, no retry and no re-arming.
                    sys_exit(0);
                }
                CMD_QUIT => {
                    reply(&[0]);
                    sys_exit(0)
                }
                _ => reply(&[E_NOT_ARMED]),
            }
        }
    }
}

/// Why a unmount did not happen.
enum Fail {
    Errno(u8),
    /// The path is another mount than the armed one; what is there.
    Mismatch(Identity),
}

#[allow(unsafe_code)]
fn errno() -> u8 {
    // SAFETY: __errno_location returns this thread's errno slot.
    let e = unsafe { *libc::__errno_location() };
    u8::try_from(e).unwrap_or(u8::MAX)
}

/// What `statx` says about the mount root open as `fd`, without asking the file system:
/// `(mask, mount id, device)`; `mask` asks for the unique ID, the plain one, or both (a kernel that
/// has the unique ID answers with it alone). `Err(errno)`: no `statx` (ENOSYS before Linux 4.11).
#[allow(unsafe_code)]
fn statx_mount(fd: libc::c_int, mask: u32) -> Result<(u32, u64, u64), u8> {
    // SAFETY: statx with an empty path on an open descriptor and a zeroed result of its own size.
    unsafe {
        let mut stx: libc::statx = std::mem::zeroed();
        let r = libc::syscall(
            libc::SYS_statx,
            fd,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_STATX_DONT_SYNC,
            mask,
            &raw mut stx,
        );
        if r != 0 {
            return Err(errno());
        }
        Ok((stx.stx_mask, stx.stx_mnt_id, libc::makedev(stx.stx_dev_major, stx.stx_dev_minor)))
    }
}

/// What the mount table says of the mounts with device `dev`: the first such line's mount ID
/// and whether its type is `fstype` (or `fstype.SUBTYPE`), and whether a line with ID `want`
/// exists among them. Reads `/proc/self/mountinfo` through descriptor 4 from its start.
#[allow(unsafe_code)]
fn mountinfo(dev: u64, t: &Target, want: Option<u64>) -> Option<(u64, bool, bool)> {
    let mut buf = [0u8; 16384];
    let (mut have, mut first, mut seen_want) = (0usize, None, false);
    // SAFETY: lseek and read on descriptor 4 with a buffer of the length given.
    unsafe {
        if libc::syscall(libc::SYS_lseek, MOUNTINFO_FD, 0, libc::SEEK_SET) < 0 {
            return None;
        }
        loop {
            let n = sys_read(MOUNTINFO_FD, buf[have..].as_mut_ptr(), buf.len() - have);
            if n < 0 {
                return None;
            }
            have += n as usize;
            let mut start = 0;
            while let Some(nl) = buf[start..have].iter().position(|&c| c == b'\n') {
                if let Some((id, d, ty_ok)) = parse_line(&buf[start..start + nl], &t.fstype, &t.source) {
                    if d == dev {
                        first = first.or(Some((id, ty_ok)));
                        seen_want |= want == Some(id);
                    }
                }
                start += nl + 1;
            }
            if n == 0 {
                break;
            }
            // A line that fills the whole buffer cannot be read: give up (the caller refuses).
            if start == 0 && have == buf.len() {
                return None;
            }
            buf.copy_within(start..have, 0);
            have -= start;
        }
    }
    first.map(|(id, ty)| (id, ty, seen_want))
}

/// `(mount id, device, type matches)` of one mountinfo line: `ID PARENT MAJ:MIN ROOT MOUNT ...
/// - TYPE SOURCE OPTS`. Only the numbers and the type are read; spaces in paths are escaped by
/// the kernel, so the first ` - ` is the separator.
fn parse_line(line: &[u8], fstype: &[u8], source: &[u8]) -> Option<(u64, u64, bool)> {
    let mut f = line.split(|&c| c == b' ');
    let num = |s: &[u8]| -> Option<u64> {
        if s.is_empty() || s.len() > 10 {
            return None;
        }
        s.iter().try_fold(0u64, |a, &c| c.is_ascii_digit().then(|| a * 10 + u64::from(c - b'0')))
    };
    let id = num(f.next()?)?;
    f.next()?;
    let (maj, min) = {
        let mm = f.next()?;
        let colon = mm.iter().position(|&c| c == b':')?;
        (num(&mm[..colon])?, num(&mm[colon + 1..])?)
    };
    let sep = line.windows(3).position(|w| w == b" - ")?;
    let mut rest = line[sep + 3..].split(|&c| c == b' ');
    let ty = rest.next()?;
    let name = rest.next()?;
    let ok = (ty == fstype
        || (ty.len() > fstype.len() && ty.starts_with(fstype) && ty[fstype.len()] == b'.'))
        && name == source;
    Some((id, libc::makedev(maj as u32, min as u32), ok))
}

/// `M`: checks the type and records the identity of the mount at the path. Nothing here
/// reaches the daemon (it is not even served yet).
#[allow(unsafe_code)]
fn arm(t: &Target) -> Result<Armed, u8> {
    // SAFETY: a NUL-terminated path; the descriptor is closed before returning.
    let fd = sys_openat(&t.path);
    if fd < 0 {
        return Err(errno());
    }
    let r = (|| {
        // No statx: no way to name the mount without asking the daemon.
        let ask = match t.max_level {
            3.. => libc::STATX_MNT_ID | libc::STATX_MNT_ID_UNIQUE,
            2 => libc::STATX_MNT_ID,
            _ => 0,
        };
        let (mask, id, dev) =
            statx_mount(fd, ask).map_err(|e| if e == libc::ENOSYS as u8 { E_NO_PROOF } else { e })?;
        let (level, mount_id) = if t.max_level >= 3 && mask & libc::STATX_MNT_ID_UNIQUE != 0 {
            (3, Some(id))
        } else if t.max_level >= 2 && mask & libc::STATX_MNT_ID != 0 {
            (2, Some(id))
        } else {
            (1, None)
        };
        // The type, by the mount table: the helper's own reading, not the daemon's word.
        let (table_id, is_type, _) = mountinfo(dev, t, None).ok_or(libc::ENOENT as u8)?;
        if !is_type {
            return Err(E_WRONG_FS);
        }
        Ok(Armed { level, id: Identity { mount_id: mount_id.unwrap_or(table_id), dev } })
    })();
    // SAFETY: closing the descriptor opened above.
    sys_close(fd);
    r
}

/// `U`, and the end of file after `M`: opens the path, checks that it is the armed mount,
/// detaches it.
#[allow(unsafe_code)]
fn unmount(t: &Target, a: &Armed) -> Result<(), Fail> {
    // SAFETY: a NUL-terminated path; the descriptor is closed before returning.
    let fd = sys_openat(&t.path);
    if fd < 0 {
        return Err(Fail::Errno(errno()));
    }
    let r = (|| {
        let ask = match a.level {
            3 => libc::STATX_MNT_ID_UNIQUE,
            2 => libc::STATX_MNT_ID,
            _ => 0,
        };
        let (mask, id, dev) = statx_mount(fd, ask).map_err(Fail::Errno)?;
        let (found_id, same) = match a.level {
            3 => (
                if mask & libc::STATX_MNT_ID_UNIQUE != 0 { id } else { 0 },
                mask & libc::STATX_MNT_ID_UNIQUE != 0 && id == a.id.mount_id && dev == a.id.dev,
            ),
            2 => (
                if mask & libc::STATX_MNT_ID != 0 { id } else { 0 },
                // The plain mount ID and the device number are both reused (measured): the type,
                // read from the mount table (the daemon is not asked), at least tells a successor
                // of another type.
                mask & libc::STATX_MNT_ID != 0
                    && id == a.id.mount_id
                    && dev == a.id.dev
                    && mountinfo(dev, t, None).is_some_and(|m| m.1),
            ),
            _ => match mountinfo(dev, t, Some(a.id.mount_id)) {
                Some((first, ty, seen)) => (first, ty && seen && dev == a.id.dev),
                None => (0, false),
            },
        };
        if !same {
            return Err(Fail::Mismatch(Identity { mount_id: found_id, dev }));
        }
        // `/proc/self/fd/N/`, built by hand: nothing allocates here.
        let mut p = [0u8; 40];
        let head = b"/proc/self/fd/";
        p[..head.len()].copy_from_slice(head);
        let mut at = head.len();
        let mut digits = [0u8; 10];
        let mut n = fd as u32;
        let mut k = 0;
        loop {
            digits[k] = b'0' + (n % 10) as u8;
            n /= 10;
            k += 1;
            if n == 0 {
                break;
            }
        }
        while k > 0 {
            k -= 1;
            p[at] = digits[k];
            at += 1;
        }
        p[at] = b'/';
        // SAFETY: a NUL-terminated path in a buffer that outlives the call.
        // With UMOUNT_NOFOLLOW; on EINVAL once more without it (older kernels, see UMOUNT_FLAGS_OLD_KERNEL).
        if sys_umount2(p.as_ptr(), UMOUNT_FLAGS) == 0
            || (u32::from(errno()) == libc::EINVAL as u32
                && sys_umount2(p.as_ptr(), UMOUNT_FLAGS_OLD_KERNEL) == 0)
        {
            Ok(())
        } else {
            Err(Fail::Errno(errno()))
        }
    })();
    // SAFETY: closing the descriptor opened above.
    sys_close(fd);
    r
}

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: Option<u32> = Some(0xC000_003E);
#[cfg(target_arch = "x86")]
const AUDIT_ARCH: Option<u32> = Some(0x4000_0003);
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: Option<u32> = Some(0xC000_00B7);
#[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
const AUDIT_ARCH: Option<u32> = None;

const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_ALLOW: u32 = 0x7fff_0000;
// BPF opcodes: ld word absolute, jump if equal to a constant, return a constant.
const BPF_LD_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_RET_K: u16 = 0x06;
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
/// The low word of argument `n` in `struct seccomp_data` (little endian on every supported arch).
const fn off_arg(n: u32) -> u32 {
    16 + 8 * n
}

/// The system calls the helper keeps after setup, without argument checks.
fn allowed() -> Vec<u32> {
    [libc::SYS_read, libc::SYS_write, libc::SYS_close, libc::SYS_exit_group, libc::SYS_statx]
        .into_iter()
        .map(|n| n as u32)
        .collect()
}

/// The calls whose first checked argument is pinned: `(number, argument index, the values allowed)`.
fn pinned() -> [(u32, u32, Vec<u32>); 3] {
    [
        (libc::SYS_openat as u32, 2, vec![OPEN_FLAGS as u32]),
        (libc::SYS_umount2 as u32, 1, vec![UMOUNT_FLAGS as u32, UMOUNT_FLAGS_OLD_KERNEL as u32]),
        (libc::SYS_lseek as u32, 0, vec![MOUNTINFO_FD as u32]),
    ]
}

fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt: 0, jf: 0, k }
}

fn jump(k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code: BPF_JEQ_K, jt, jf, k }
}

/// The helper's seccomp program for this architecture, or `None` where it is not written.
/// Any other architecture word, system call, or argument than listed: the process is killed.
pub fn filter() -> Option<Vec<libc::sock_filter>> {
    let arch = AUDIT_ARCH?;
    let simple = allowed();
    let mut f = vec![
        stmt(BPF_LD_ABS, OFF_ARCH),
        jump(arch, 1, 0),
        stmt(BPF_RET_K, RET_KILL_PROCESS),
        stmt(BPF_LD_ABS, OFF_NR),
    ];
    let first_simple = f.len();
    for n in &simple {
        f.push(jump(*n, 0, 0));
    }
    // Calls with a pinned argument: on a match the argument is compared, else the number is
    // loaded again for the next test.
    for (nr, arg, wants) in pinned() {
        let k = u8::try_from(wants.len()).ok()?;
        f.push(jump(nr, 0, k + 3));
        f.push(stmt(BPF_LD_ABS, off_arg(arg)));
        for (j, want) in wants.iter().enumerate() {
            // A match jumps over the other values and the kill, to the allow.
            f.push(jump(*want, k - u8::try_from(j).ok()?, 0));
        }
        f.push(stmt(BPF_RET_K, RET_KILL_PROCESS));
        f.push(stmt(BPF_RET_K, RET_ALLOW));
        f.push(stmt(BPF_LD_ABS, OFF_NR));
    }
    f.push(stmt(BPF_RET_K, RET_KILL_PROCESS));
    let allow = f.len();
    f.push(stmt(BPF_RET_K, RET_ALLOW));
    for i in 0..simple.len() {
        let at = first_simple + i;
        f[at].jt = u8::try_from(allow - at - 1).ok()?;
    }
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference evaluation of the program, to test it without installing it.
    fn run(f: &[libc::sock_filter], arch: u32, nr: u32, args: [u32; 6]) -> u32 {
        let (mut a, mut pc) = (0u32, 0usize);
        loop {
            let i = f[pc];
            pc += 1;
            match i.code {
                BPF_LD_ABS => {
                    a = match i.k {
                        OFF_NR => nr,
                        OFF_ARCH => arch,
                        k => args[((k - 16) / 8) as usize],
                    }
                }
                BPF_JEQ_K => pc += usize::from(if a == i.k { i.jt } else { i.jf }),
                BPF_RET_K => return i.k,
                c => unreachable!("opcode {c}"),
            }
        }
    }

    #[test]
    fn filter_allows_exactly_the_list() {
        let (Some(f), Some(arch)) = (filter(), AUDIT_ARCH) else { return };
        let none = [0; 6];
        for n in allowed() {
            assert_eq!(run(&f, arch, n, none), RET_ALLOW, "syscall {n}");
        }
        for (nr, arg, wants) in pinned() {
            for want in &wants {
                let mut ok = [0; 6];
                ok[arg as usize] = *want;
                assert_eq!(run(&f, arch, nr, ok), RET_ALLOW, "pinned {nr}");
            }
            let mut bad = [0; 6];
            bad[arg as usize] = wants[0] ^ 0x40;
            assert_eq!(run(&f, arch, nr, bad), RET_KILL_PROCESS, "pinned {nr} with another argument");
            // The second value does not open the first up to anything else: 0 and the union are refused.
            bad[arg as usize] = wants.iter().fold(0, |a, w| a | w) | 0x80;
            assert_eq!(run(&f, arch, nr, bad), RET_KILL_PROCESS, "pinned {nr} with a superset");
        }
        // The hazard raw system calls remove: musl's `openat` ORs in O_LARGEFILE (0x8000 on 32-bit
        // x86, 0x20000 on aarch64, where 0x8000 is O_NOFOLLOW and already pinned). A call with that
        // bit is not the pinned one and is killed, so the helper must make the raw call with
        // exactly the pinned flags (`sys_openat`).
        let large: u32 = if cfg!(target_arch = "aarch64") { 0x2_0000 } else { 0x8000 };
        assert_eq!(OPEN_FLAGS as u32 & large, 0);
        let mut wrapped = [0; 6];
        wrapped[2] = OPEN_FLAGS as u32 | large;
        assert_eq!(run(&f, arch, libc::SYS_openat as u32, wrapped), RET_KILL_PROCESS);
        // No other call: the ones the old helper had (fstatfs, nanosleep: a request to the
        // daemon, a timer), and others that matter.
        for n in [
            libc::SYS_fstatfs,
            libc::SYS_nanosleep,
            libc::SYS_mount,
            libc::SYS_ptrace,
            libc::SYS_execve,
            libc::SYS_socket,
            libc::SYS_open_by_handle_at,
            libc::SYS_getppid,
            libc::SYS_getpid,
        ] {
            assert_eq!(run(&f, arch, n as u32, none), RET_KILL_PROCESS, "syscall {n}");
        }
        // Another architecture: killed whatever the number.
        assert_eq!(run(&f, arch ^ 0x8000_0000, libc::SYS_read as u32, none), RET_KILL_PROCESS);
    }

    #[test]
    fn filter_fits_the_kernel_limit() {
        if let Some(f) = filter() {
            assert!(f.len() < 4096);
        }
    }

    #[test]
    fn mountinfo_lines() {
        let l = b"36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext3 /dev/root rw,errors=continue";
        assert_eq!(parse_line(l, b"ext3", b"/dev/root"), Some((36, libc::makedev(98, 0), true)));
        assert_eq!(parse_line(l, b"fuse", b"/dev/root"), Some((36, libc::makedev(98, 0), false)));
        // The right type under another name is another mount.
        assert_eq!(parse_line(l, b"ext3", b"other"), Some((36, libc::makedev(98, 0), false)));
        let f = b"41 29 0:146 / /mnt/with\\040space rw,nosuid shared:5 - fuse.sacdfs sacdfs rw,user_id=0";
        assert_eq!(parse_line(f, b"fuse", b"sacdfs"), Some((41, libc::makedev(0, 146), true)));
        let plain = b"41 29 0:146 / /m rw - fuse sacdfs rw";
        assert_eq!(parse_line(plain, b"fuse", b"sacdfs"), Some((41, libc::makedev(0, 146), true)));
        // fuseblk, and a name that only starts with the type, are other types.
        assert_eq!(
            parse_line(b"7 1 0:5 / /x rw - fuseblk /dev/sda rw", b"fuse", b"/dev/sda").map(|x| x.2),
            Some(false)
        );
        assert_eq!(parse_line(b"7 1 0:5 / /x rw - fused x rw", b"fuse", b"x").map(|x| x.2), Some(false));
        // Not a line.
        assert_eq!(parse_line(b"garbage", b"fuse", b"x"), None);
        assert_eq!(parse_line(b"1 2 3 / /x rw - fuse a b", b"fuse", b"a"), None);
    }
}
