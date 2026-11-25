# Mutation check of src/deny_list.rs: python3 -I tests/deny_list/mutate.py [N...]
# One edit at a time to the source, the local suite (integration test + unit tests), restore.
# Run from the repository root on a clean tree; results in deny_list_mutants.json in the system temp dir.
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

SRC = "./src/deny_list.rs"
M = [
 ("TSYNC flag dropped", "const SECCOMP_FILTER_FLAG_TSYNC: libc::c_ulong = 1;", "const SECCOMP_FILTER_FLAG_TSYNC: libc::c_ulong = 0;"),
 ("architecture word not checked", "jump(BPF_JEQ_K, arch, 1, 0),\n        stmt(BPF_RET_K, RET_KILL_PROCESS),", "jump(BPF_JEQ_K, arch, 1, 0),\n        stmt(BPF_RET_K, RET_ALLOW),"),
 ("x32 numbers not killed", "f.push(stmt(BPF_RET_K, RET_KILL_PROCESS));\n    }\n    // clone3 passes", "f.push(stmt(BPF_RET_K, RET_ALLOW));\n    }\n    // clone3 passes"),
 ("open_by_handle_at not denied", "        libc::SYS_open_by_handle_at,\n", ""),
 ("mount not denied", "        libc::SYS_mount,\n", ""),
 ("fsmount not denied", "        libc::SYS_fsmount,\n", ""),
 ("ptrace not denied", "        libc::SYS_ptrace,\n", ""),
 ("unshare not denied", "        libc::SYS_unshare,\n", ""),
 ("kexec_file_load not denied", "    v.push(libc::SYS_kexec_file_load);", "    let _ = &v;"),
 ("umount2 never denied", "    if policy.deny_umount {\n        v.push(libc::SYS_umount2);", "    if false {\n        v.push(libc::SYS_umount2);"),
 ("umount2 always denied", "    if policy.deny_umount {\n        v.push(libc::SYS_umount2);", "    if true {\n        v.push(libc::SYS_umount2);"),
 ("errno not in the return value", "    f.push(stmt(BPF_RET_K, RET_ERRNO | u32::from(policy.errno)));\n    for i in 0..list.len()", "    f.push(stmt(BPF_RET_K, RET_ERRNO));\n    for i in 0..list.len()"),
 ("denied calls killed instead of refused", "    f.push(stmt(BPF_RET_K, RET_ERRNO | u32::from(policy.errno)));\n    for i in 0..list.len()", "    f.push(stmt(BPF_RET_K, RET_KILL_PROCESS));\n    for i in 0..list.len()"),
 ("default deny (the list allowed)", "f.push(stmt(BPF_RET_K, RET_ALLOW));\n    let deny", "f.push(stmt(BPF_RET_K, RET_ERRNO | 1));\n    let deny"),
 ("jump target one too far", "f[at].jt = u8::try_from(deny - at - 1).ok()?;", "f[at].jt = u8::try_from(deny - at).ok()?;"),
 ("a failed install reported as success", "        -1 => Err(io::Error::last_os_error()),", "        -1 => Ok(0),"),
 
 ("production errno not EPERM", "Policy { deny_umount, errno: libc::EPERM as u16 }", "Policy { deny_umount, errno: libc::EACCES as u16 }"),
 ("clone3 not refused", "f.push(jump(BPF_JEQ_K, libc::SYS_clone3 as u32, 0, 1));", "f.push(jump(BPF_JEQ_K, 0xffff_ffff, 0, 1));"),
 ("clone3 fails with the policy's errno, not ENOSYS", "RET_ERRNO | ENOSYS", "RET_ERRNO | u32::from(policy.errno)"),
 ("clone with namespace flags not refused", "f.push(jump(BPF_JSET_K, CLONE_NEW_MASK, 0, 1));", "f.push(jump(BPF_JSET_K, 0, 0, 1));"),
 ("CLONE_NEWUSER missing from the mask", "0x0800_0000 | 0x1000_0000 | 0x2000_0000", "0x0800_0000 | 0x2000_0000"),
 ("io_uring_setup not denied", "        libc::SYS_io_uring_setup,\n", ""),
 ("chroot not denied", "        libc::SYS_chroot,\n", ""),
 ("process_vm_writev not denied", "        libc::SYS_process_vm_writev,\n", ""),
 ("iopl and ioperm not denied", "v.extend([libc::SYS_iopl, libc::SYS_ioperm]);", "let _ = &v;"),
 ("count of refused calls wrong", "0 => Ok(refused(policy)),", "0 => Ok(refused(policy) + 1),"),
]
only = sys.argv[1:]
orig = Path(SRC).read_text()
res = []
def run():
    out = ""
    for cmd in (["cargo", "test", "--locked", "--test", "deny_list"], ["cargo", "test", "--locked", "--bin", "sacdfs", "deny_list"]):
        try:
            p = subprocess.run(cmd, check=False, capture_output=True, text=True, timeout=300)
            out += p.stdout + p.stderr
        except subprocess.TimeoutExpired:
            out += "\ntest result: FAILED (hang: killed after 300 s)\n"
    return out
try:
    for i, (name, old, new) in enumerate(M):
        if only and str(i) not in only and only != ["dry"]:
            continue
        if orig.count(old) != 1:
            res.append((i, name, f"PATTERN MISSING/AMBIGUOUS: {orig.count(old)}", []))
            print(res[-1], flush=True)
            continue
        if only == ["dry"]:
            continue
        Path(SRC).write_text(orig.replace(old, new, 1))
        out = run()
        if "error[" in out or "could not compile" in out:
            res.append((i, name, "DID NOT COMPILE", []))
        else:
            failed = sorted(set(re.findall(r"^FAIL (.+?):", out, re.MULTILINE)) | set(re.findall(r"^test (\S+) \.\.\. FAILED", out, re.MULTILINE)))
            m = re.search(r"deny_list: (\d+) passed, (\d+) failed", out)
            killed = bool(failed) or "test result: FAILED" in out or (m is not None and m.group(2) != "0") or m is None
            res.append((i, name, "KILLED" if killed else "SURVIVED", failed))
        print(res[-1], flush=True)
finally:
    Path(SRC).write_text(orig)
Path(os.path.join(tempfile.gettempdir(), "deny_list_mutants.json")).write_text(json.dumps(res, indent=1))
