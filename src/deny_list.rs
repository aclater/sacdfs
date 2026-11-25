// SPDX-License-Identifier: GPL-3.0-or-later

//! A short seccomp deny-list for the mount process (hardening step 1, part 3): the system
//! calls a compromised sacdfs would use to get out of a container or to widen its rights,
//! and that sacdfs itself never makes after it has mounted. They fail with an errno
//! (EPERM), so that nothing dies for a stray call; only what no honest process does is
//! killed: another system call ABI than the native one (a 32-bit `int 0x80` on x86_64, the
//! x32 ABI), which would walk around the list.
//!
//! The list: `open_by_handle_at` and `name_to_handle_at` (the "shocker" escape: with
//! `DAC_READ_SEARCH` they open any file of a file system by its handle, and Docker's default
//! profile allows them with that capability), `mount` and the new mount API that does the same
//! (`fsopen`, `fsconfig`, `fsmount`, `fspick`, `move_mount`, `open_tree`, `mount_setattr`),
//! `umount2` (only with [`Policy::deny_umount`], see below), `ptrace`, `bpf`, `keyctl`,
//! `kexec_load`, `kexec_file_load`, `init_module`, `finit_module`, `pivot_root`, `setns` and
//! `unshare`, and also `clone` WITH a namespace flag (`CLONE_NEW*`: EPERM) and `clone3` (ENOSYS, so that libc falls back to
//! `clone`, whose flags can be looked at: `clone3` passes them in memory, out of a filter's reach), since both can make a
//! user namespace without `unshare`. Threads and forks use `clone` without those flags and are not touched. Then
//! `process_vm_readv` and `process_vm_writev`, `perf_event_open`, `userfaultfd`, `add_key` and `request_key`, the three
//! `io_uring` calls, `chroot`, `swapon` and `swapoff`, `reboot`, `delete_module`, and on x86 `iopl` and `ioperm`: sacdfs
//! uses none of them (checked in the source, and by the lab runs with the list on).
//!
//! `umount2` is denied only when the daemon has given up the right to unmount (the capability drop,
//! `Unmounter::deny_list`). Without that, the daemon's own unmount at a stop would fail: the caller says.
//!
//! Installed after the mount, with `SECCOMP_FILTER_FLAG_TSYNC`: the engine's threads exist
//! by then, and a filter applies per thread. Threads started later inherit it, and so do
//! the programs sacdfs runs, which after the mount are none. It needs CAP_SYS_ADMIN or
//! no_new_privs (the capability drop sets it); this does not set no_new_privs itself, so without either the
//! install fails with EACCES, which the caller logs.
//!
//! The program is written for x86_64 and i686; other architectures, aarch64 included, get no filter until it has
//! been run there.

use std::io;

/// What to deny beyond the fixed list, and with which errno.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// Also deny `umount2` (and `umount` on i686): only where the unmount helper does the unmounting.
    pub deny_umount: bool,
    /// The errno of a denied call; the tests use another than EPERM to tell it from the kernel's own.
    pub errno: u16,
}

impl Policy {
    /// What sacdfs installs.
    pub const fn production(deny_umount: bool) -> Policy {
        Policy { deny_umount, errno: libc::EPERM as u16 }
    }
}

const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_ALLOW: u32 = 0x7fff_0000;
const RET_ERRNO: u32 = 0x0005_0000;
const BPF_LD_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
#[cfg(target_arch = "x86_64")]
const BPF_JGE_K: u16 = 0x35;
const BPF_RET_K: u16 = 0x06;
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
/// The x32 ABI's system call numbers have this bit set.
#[cfg(target_arch = "x86_64")]
const X32_BIT: u32 = 0x4000_0000;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: Option<u32> = Some(0xC000_003E);
#[cfg(target_arch = "x86")]
const AUDIT_ARCH: Option<u32> = Some(0x4000_0003);
// aarch64 has a list in `denied` but NO filter: the program has never run on ARM hardware, and a mismatch
// in a filter kills the process. Like the capability drop, it waits for the Jetson (owner decision
// 2026-11-25). To enable it there, add `Some(0xC000_00B7)` here and run the integration test on it.
#[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
const AUDIT_ARCH: Option<u32> = None;

/// The system call numbers denied under `policy`, for this architecture.
pub fn denied(policy: &Policy) -> Vec<u32> {
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
    // Only x86 has them.
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    v.extend([libc::SYS_iopl, libc::SYS_ioperm]);
    // i686 has no kexec_file_load.
    #[cfg(not(target_arch = "x86"))]
    v.push(libc::SYS_kexec_file_load);
    if policy.deny_umount {
        v.push(libc::SYS_umount2);
        // The old call of the same name, which only i686 still has.
        #[cfg(target_arch = "x86")]
        v.push(libc::SYS_umount);
    }
    v.into_iter().map(|n| n as u32).collect()
}

fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt: 0, jf: 0, k }
}

fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

/// The number of system calls the filter refuses: the list, and `clone` with a namespace flag and `clone3`.
pub fn refused(policy: &Policy) -> usize {
    denied(policy).len() + 2
}

/// The flags of `clone` that make a namespace: CLONE_NEWNS, NEWCGROUP, NEWUTS, NEWIPC, NEWUSER, NEWPID, NEWNET.
/// (CLONE_NEWTIME shares its bit with the exit-signal byte and is not a `clone` flag.)
pub const CLONE_NEW_MASK: u32 =
    0x0002_0000 | 0x0200_0000 | 0x0400_0000 | 0x0800_0000 | 0x1000_0000 | 0x2000_0000 | 0x4000_0000;
const OFF_ARG0: u32 = 16;
const BPF_JSET_K: u16 = 0x45;
const ENOSYS: u32 = 38;

/// The program for this architecture, or `None` where it is not written.
pub fn filter(policy: &Policy) -> Option<Vec<libc::sock_filter>> {
    let arch = AUDIT_ARCH?;
    let list = denied(policy);
    let mut f = vec![
        stmt(BPF_LD_ABS, OFF_ARCH),
        jump(BPF_JEQ_K, arch, 1, 0),
        stmt(BPF_RET_K, RET_KILL_PROCESS),
        stmt(BPF_LD_ABS, OFF_NR),
    ];
    #[cfg(target_arch = "x86_64")]
    {
        // The x32 ABI shares the architecture word: its numbers are told by their bit.
        f.push(jump(BPF_JGE_K, X32_BIT, 0, 1));
        f.push(stmt(BPF_RET_K, RET_KILL_PROCESS));
    }
    // clone3 passes its flags in memory, where a filter cannot look: it fails with ENOSYS and libc
    // falls back to clone. clone with a namespace flag fails with the policy's errno; any other clone
    // (a thread, a fork, a vfork for exec) is allowed.
    f.push(jump(BPF_JEQ_K, libc::SYS_clone3 as u32, 0, 1));
    f.push(stmt(BPF_RET_K, RET_ERRNO | ENOSYS));
    f.push(jump(BPF_JEQ_K, libc::SYS_clone as u32, 0, 4));
    f.push(stmt(BPF_LD_ABS, OFF_ARG0));
    f.push(jump(BPF_JSET_K, CLONE_NEW_MASK, 0, 1));
    f.push(stmt(BPF_RET_K, RET_ERRNO | u32::from(policy.errno)));
    f.push(stmt(BPF_RET_K, RET_ALLOW));
    // (A call that is not clone leaves the accumulator as it is: the number.)
    let first = f.len();
    for n in &list {
        f.push(jump(BPF_JEQ_K, *n, 0, 0));
    }
    f.push(stmt(BPF_RET_K, RET_ALLOW));
    let deny = f.len();
    f.push(stmt(BPF_RET_K, RET_ERRNO | u32::from(policy.errno)));
    for i in 0..list.len() {
        let at = first + i;
        f[at].jt = u8::try_from(deny - at - 1).ok()?;
    }
    Some(f)
}

/// Installs the filter on every thread of the process. Returns how many system calls it
/// refuses; `Ok(0)` where there is no filter for this architecture.
#[allow(unsafe_code)]
pub fn install(policy: &Policy) -> io::Result<usize> {
    let Some(prog) = filter(policy) else { return Ok(0) };
    let fprog = libc::sock_fprog {
        len: u16::try_from(prog.len()).map_err(io::Error::other)?,
        filter: prog.as_ptr().cast_mut(),
    };
    const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;
    const SECCOMP_FILTER_FLAG_TSYNC: libc::c_ulong = 1;
    // SAFETY: the seccomp system call with a program that lives through the call.
    let r = unsafe {
        libc::syscall(libc::SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, &raw const fprog)
    };
    match r {
        0 => Ok(refused(policy)),
        -1 => Err(io::Error::last_os_error()),
        tid => Err(io::Error::other(format!(
            "thread {tid} could not take the filter (it has an incompatible one)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A reference evaluation of the program.
    fn run(f: &[libc::sock_filter], arch: u32, nr: u32) -> u32 {
        run_with(f, arch, nr, 0)
    }

    /// The same, for a call whose first argument (the low word) is `arg0`.
    fn run_with(f: &[libc::sock_filter], arch: u32, nr: u32, arg0: u32) -> u32 {
        let (mut a, mut pc) = (0u32, 0usize);
        loop {
            let i = f[pc];
            pc += 1;
            match i.code {
                BPF_LD_ABS => {
                    a = if i.k == OFF_ARCH {
                        arch
                    } else if i.k == OFF_ARG0 {
                        arg0
                    } else {
                        nr
                    }
                }
                BPF_JSET_K => pc += usize::from(if a & i.k != 0 { i.jt } else { i.jf }),
                BPF_JEQ_K => pc += usize::from(if a == i.k { i.jt } else { i.jf }),
                #[cfg(target_arch = "x86_64")]
                BPF_JGE_K => pc += usize::from(if a >= i.k { i.jt } else { i.jf }),
                BPF_RET_K => return i.k,
                c => unreachable!("opcode {c}"),
            }
        }
    }

    #[test]
    fn denies_the_list_with_the_errno_and_nothing_else() {
        let (Some(arch), p) = (AUDIT_ARCH, Policy::production(true)) else { return };
        let f = filter(&p).unwrap();
        let deny = denied(&p);
        for n in &deny {
            assert_eq!(run(&f, arch, *n), RET_ERRNO | libc::EPERM as u32, "syscall {n}");
        }
        // Every other number below 600 is allowed (the list is a deny-list).
        for n in (0..600).filter(|n| !deny.contains(n) && *n != libc::SYS_clone3 as u32) {
            if cfg!(target_arch = "x86_64") && n >= 0x4000_0000 {
                continue;
            }
            assert_eq!(run(&f, arch, n), RET_ALLOW, "syscall {n}");
        }
        // What sacdfs and its helpers do after the mount stays possible.
        for n in [
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_openat,
            libc::SYS_futex,
            libc::SYS_execve,
            libc::SYS_clone,
            libc::SYS_seccomp,
        ] {
            assert_eq!(run(&f, arch, n as u32), RET_ALLOW, "syscall {n}");
        }
    }

    #[test]
    fn clone_with_a_namespace_flag_and_clone3_are_refused() {
        let (Some(arch), p) = (AUDIT_ARCH, Policy::production(false)) else { return };
        let f = filter(&p).unwrap();
        let clone = libc::SYS_clone as u32;
        // A thread (CLONE_VM|FS|FILES|SIGHAND|THREAD|SYSVSEM|SETTLS|PARENT_SETTID|CHILD_CLEARTID) and a fork
        // (SIGCHLD) and a vfork (CLONE_VM|CLONE_VFORK|SIGCHLD) go through.
        for flags in [0x003d_0f00u32, 17, 0x4111] {
            assert_eq!(run_with(&f, arch, clone, flags), RET_ALLOW, "flags {flags:#x}");
        }
        // Each namespace flag, alone and with a thread's flags, is refused.
        for flag in
            [0x0002_0000u32, 0x0200_0000, 0x0400_0000, 0x0800_0000, 0x1000_0000, 0x2000_0000, 0x4000_0000]
        {
            assert_eq!(
                run_with(&f, arch, clone, flag | 17),
                RET_ERRNO | libc::EPERM as u32,
                "flag {flag:#x}"
            );
            assert_eq!(run_with(&f, arch, clone, flag | 0x003d_0f00), RET_ERRNO | libc::EPERM as u32);
        }
        assert_eq!(CLONE_NEW_MASK.count_ones(), 7);
        // clone3 fails with ENOSYS whatever the policy's errno, so that libc falls back to clone.
        assert_eq!(run(&f, arch, libc::SYS_clone3 as u32), RET_ERRNO | 38);
        // The next call is not mistaken for either (the accumulator is reloaded).
        assert_eq!(run(&f, arch, libc::SYS_mount as u32), RET_ERRNO | libc::EPERM as u32);
        assert_eq!(run(&f, arch, libc::SYS_read as u32), RET_ALLOW);
    }

    #[test]
    fn umount_is_denied_only_when_asked() {
        let Some(arch) = AUDIT_ARCH else { return };
        let off = filter(&Policy::production(false)).unwrap();
        let on = filter(&Policy::production(true)).unwrap();
        assert_eq!(run(&off, arch, libc::SYS_umount2 as u32), RET_ALLOW);
        assert_eq!(run(&on, arch, libc::SYS_umount2 as u32), RET_ERRNO | libc::EPERM as u32);
        assert_eq!(run(&off, arch, libc::SYS_mount as u32), RET_ERRNO | libc::EPERM as u32);
    }

    #[test]
    fn another_abi_is_killed() {
        let Some(arch) = AUDIT_ARCH else { return };
        let f = filter(&Policy::production(true)).unwrap();
        // Another architecture word: killed, whatever the number (an allowed one included).
        assert_eq!(run(&f, arch ^ 0x8000_0000, libc::SYS_read as u32), RET_KILL_PROCESS);
        // The other of the two x86 words (a 32-bit program's, or a 64-bit one's): killed too.
        let other = if arch == 0x4000_0003 { 0xC000_003E } else { 0x4000_0003 };
        assert_eq!(run(&f, other, libc::SYS_read as u32), RET_KILL_PROCESS);
        #[cfg(target_arch = "x86_64")]
        for n in [X32_BIT, X32_BIT | libc::SYS_read as u32, X32_BIT | libc::SYS_mount as u32] {
            assert_eq!(run(&f, arch, n), RET_KILL_PROCESS, "x32 {n:#x}");
        }
    }

    #[test]
    fn program_fits_the_kernel_limit() {
        assert!(filter(&Policy::production(true)).is_none_or(|f| f.len() < 4096));
    }
}
