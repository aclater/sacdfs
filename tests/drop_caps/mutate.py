# Mutation check of src/privs.rs: python3 -I tests/drop_caps/mutate.py [N...]
# One edit at a time, the integration test (it includes the module), restore. Run from the repository
# root on a clean tree; the result goes to drop_caps_mutants.json in the system temp dir.
import json, os, re, subprocess, sys, tempfile
SRC = "./src/privs.rs"
M = [
 ("securebits not set", "libc::prctl(libc::PR_SET_SECUREBITS, SECBIT_NOROOT | SECBIT_NOROOT_LOCKED, 0, 0, 0) != 0", "false"),
 ("ambient set not cleared (equivalent: the capset that clears the inheritable set clears the ambient one)", "libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0);", ""),
 ("bounding set not cut", "if !kept && libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) != 0 {", "if false && !kept && libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) != 0 {"),
 ("everything dropped from the bounding set (kept ones too)", "let kept = if cap < 32 { lo >> cap & 1 } else { hi >> (cap - 32) & 1 } != 0;", "let kept = false;"),
 ("capset not called", "if libc::syscall(libc::SYS_capset, &raw const h, d.as_ptr()) != 0 {", "if false && libc::syscall(libc::SYS_capset, &raw const h, d.as_ptr()) != 0 {"),
 ("effective set not cut", "Data { effective: lo, permitted: lo, inheritable: 0 }", "Data { effective: u32::MAX, permitted: lo, inheritable: 0 }"),
 ("inheritable set not cut", "Data { effective: lo, permitted: lo, inheritable: 0 },\n            Data { effective: hi, permitted: hi, inheritable: 0 },", "Data { effective: lo, permitted: lo, inheritable: lo },\n            Data { effective: hi, permitted: hi, inheritable: hi },"),
 ("the high capability word not cut", "Data { effective: hi, permitted: hi, inheritable: 0 },", "Data { effective: u32::MAX, permitted: u32::MAX, inheritable: 0 },"),
 ("CAP_SYS_PTRACE and the rest kept (only SYS_ADMIN dropped)", "pub fn keep_for(prm: u64) -> u64 {\n    if prm", "pub fn keep_for(prm: u64) -> u64 {\n    if true { return prm & !(1 << CAP_SYS_ADMIN); }\n    if prm"),
 ("both kept capabilities kept (READ_SEARCH not dropped when OVERRIDE is held)", "{ 1 << CAP_DAC_OVERRIDE } else {", "{ 1 << CAP_DAC_OVERRIDE | prm & 1 << CAP_DAC_READ_SEARCH } else {"),
 ("no_new_privs not set", "if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {", "if false {"),
 ("no_new_privs not read back", "&& c.nnp\n", "\n"),
 ("other threads not signalled", "if unsafe { libc::syscall(libc::SYS_tgkill, pid, *tid, sig) } == 0 {", "if false && unsafe { libc::syscall(libc::SYS_tgkill, pid, *tid, sig) } == 0 {"),
 ("no read-back: first round is taken as done", "if todo.is_empty() && tasks.iter().all(|c| is_cut(c, keep)) {", "if true {"),
 ("a thread that does not take the signal is not reported", "if todo == last {", "if false && todo == last {"),
 ("this thread not cut first", "let mut failed = apply();", "let mut failed = 0;"),
 ("a failed capset is ignored", "if failed & FAIL_CAPSET != 0 {", "if false && failed & FAIL_CAPSET != 0 {"),
 ("bounding set reported from the flags, not from /proc", "let bounding_set_kept = tasks.iter().any(|c| c.bnd & !keep != 0);", "let bounding_set_kept = failed & FAIL_BOUNDING != 0;"),
 ("keep set not computed from what is held", "let keep = keep_for(prm);", "let keep = 1 << CAP_DAC_OVERRIDE | 1 << CAP_DAC_READ_SEARCH;"),
]
only = sys.argv[1:]
orig = open(SRC).read()
res = []
def run():
    try:
        p = subprocess.run(["cargo", "test", "--locked", "--test", "drop_caps"], capture_output=True, text=True, timeout=240)
        return p.stdout + p.stderr
    except subprocess.TimeoutExpired:
        subprocess.run("pkill -9 -f drop_caps-", shell=True)
        return "\ndrop_caps: HANG, killed after 240 s\n"
try:
    for i, (name, old, new) in enumerate(M):
        if only and str(i) not in only and only != ["dry"]:
            continue
        if orig.count(old) != 1:
            res.append((i, name, "PATTERN MISSING/AMBIGUOUS: %d" % orig.count(old), []))
            print(res[-1], flush=True)
            continue
        if only == ["dry"]:
            continue
        open(SRC, "w").write(orig.replace(old, new, 1))
        out = run()
        if "error[" in out or "could not compile" in out:
            res.append((i, name, "DID NOT COMPILE", []))
        else:
            failed = sorted(set(re.findall(r"^FAIL (.+?):", out, re.M)))
            m = re.search(r"drop_caps: (\d+) passed, (\d+) failed", out)
            killed = bool(failed) or "HANG" in out or m is None or m.group(2) != "0"
            res.append((i, name, "KILLED" if killed else "SURVIVED", [f[:60] for f in failed]))
        print(res[-1], flush=True)
finally:
    open(SRC, "w").write(orig)
json.dump(res, open(os.path.join(tempfile.gettempdir(), "drop_caps_mutants.json"), "w"), indent=1)
