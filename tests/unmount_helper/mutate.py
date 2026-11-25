# Mutation check of src/unmount_helper.rs, as reported in REVIEW-part1.md: python3 -I tests/unmount_helper/mutate.py [N...]
# Edits the source one mutant at a time, runs the local suite, restores it. Run on a clean tree from the repository root; results in /tmp/unmount_helper_mutants.json.
import json
import re
import subprocess
import sys
from pathlib import Path

SRC='./src/unmount_helper.rs'
M=[
("no type check at arming","        if !is_type {","        if false && !is_type {"),
("identity never mismatches","        if !same {","        if false && !same {"),
("second M accepted","CMD_MOUNTED if armed.is_some() => reply(&[E_ARMED]),","CMD_MOUNTED if false => reply(&[E_ARMED]),"),
("plain umount (no MNT_DETACH)","const UMOUNT_FLAGS: libc::c_int = libc::MNT_DETACH | UMOUNT_NOFOLLOW;","const UMOUNT_FLAGS: libc::c_int = UMOUNT_NOFOLLOW;"),
("no UMOUNT_NOFOLLOW","const UMOUNT_FLAGS: libc::c_int = libc::MNT_DETACH | UMOUNT_NOFOLLOW;","const UMOUNT_FLAGS: libc::c_int = libc::MNT_DETACH;"),
("no trailing slash on /proc/self/fd/N","p[at] = b'/';","p[at] = 0;"),
("dumpable left on","libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0","libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0) != 0"),
("no_new_privs not set","libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0","libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 0, 0, 0, 0) != 0"),
("capabilities not cut (no capset)","if libc::syscall(libc::SYS_capset, &raw const h, d.as_ptr()) != 0 {","if false && libc::syscall(libc::SYS_capset, &raw const h, d.as_ptr()) != 0 {"),
("bounding set not cut","if cap != CAP_SYS_ADMIN as i32 && libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) != 0 {","if false && cap != CAP_SYS_ADMIN as i32 && libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) != 0 {"),
("daemon's descriptors kept","libc::syscall(libc::SYS_close_range, 4u32, u32::MAX, 0u32) != 0","libc::syscall(libc::SYS_close_range, 5000u32, u32::MAX, 0u32) != 0 && false"),
("stop signals not ignored","libc::signal(sig, libc::SIG_IGN);","libc::signal(sig, libc::SIG_DFL);"),
("end of file does not unmount","let _ = unmount(target, &a);","let _ = &a;"),
("helper stays after U","// One `U`: refused or done, no retry and no re-arming.\n                    sys_exit(0);","// One `U`: refused or done, no retry and no re-arming.\n                    continue;"),
("recorded identity replaced by the found one in the reply","b[1..9].copy_from_slice(&a.id.mount_id.to_le_bytes());","b[1..9].copy_from_slice(&found.mount_id.to_le_bytes());"),
("arch word not checked (filter)","jump(arch, 1, 0),\n        stmt(BPF_RET_K, RET_KILL_PROCESS),","jump(arch, 1, 0),\n        stmt(BPF_RET_K, RET_ALLOW),"),
("openat flags not pinned (filter)","libc::SYS_statx]","libc::SYS_statx, libc::SYS_openat]"),
("fstatfs allowed again (filter)","libc::SYS_statx]","libc::SYS_statx, libc::SYS_fstatfs]"),
("type prefix matches without the dot","ty[fstype.len()] == b'.'","true"),
("level 3 ignores the device number","mask & libc::STATX_MNT_ID_UNIQUE != 0 && id == a.id.mount_id && dev == a.id.dev","mask & libc::STATX_MNT_ID_UNIQUE != 0 && id == a.id.mount_id"),
("level 2 ignores the device number","                    && id == a.id.mount_id\n                    && dev == a.id.dev\n                    && mountinfo","                    && id == a.id.mount_id\n                    && mountinfo"),
("no_new_privs skipped (pure)","if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {","if false && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {"),
("level 2 skips the type re-check","&& mountinfo(dev, t, None).is_some_and(|m| m.1),","&& true,"),
("source name not compared (arming and U)","&& name == source;","&& true;"),
("openat through a wrapper that adds O_LARGEFILE (musl)","libc::c_long::from(OPEN_FLAGS),","libc::c_long::from(OPEN_FLAGS | 0x8000),"),
("level 1 ignores whether the armed mount line still exists","ty && seen && dev == a.id.dev","ty && dev == a.id.dev"),
]
only=sys.argv[1:] 
orig = Path(SRC).read_text()
res=[]
def run():
    out=''
    for cmd in (["cargo","test","--test","unmount_helper"],["cargo","test","--bin","sacdfs","unmount_helper"]):
        try:
            p=subprocess.run(cmd,check=False, cwd='.',capture_output=True,text=True,timeout=300)
            out+=p.stdout+p.stderr
        except subprocess.TimeoutExpired:
            out+='\ntest result: FAILED (hang: killed after 300 s)\n'
            subprocess.run("for p in $(pgrep -f 'out/unmount_helper-'); do kill -9 $p; done",check=False, shell=True)
    return out
try:
    for i,(name,old,new) in enumerate(M):
        if only and str(i) not in only: continue
        if orig.count(old)!=1:
            res.append((i,name,f"PATTERN MISSING/AMBIGUOUS: {orig.count(old)}",[])); continue
        if only==['dry']: continue
        Path(SRC).write_text(orig.replace(old,new,1))
        out=run()
        if 'error[' in out or 'error: could not compile' in out:
            res.append((i,name,'DID NOT COMPILE',[]))
        else:
            failed=sorted(set(re.findall(r'^test (\S+) \.\.\. FAILED',out,re.MULTILINE)))
            if 'unmount_helper:' in out:
                m=re.search(r'unmount_helper: (\d+) passed, (\d+) failed',out)
            res.append((i,name,'KILLED' if failed or (m and m.group(2)!='0') or 'test result: FAILED' in out else 'SURVIVED',failed))
        print(res[-1],flush=True)
finally:
    Path(SRC).write_text(orig)
Path('/tmp/unmount_helper_mutants.json').write_text(json.dumps(res, indent=1))
