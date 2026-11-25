use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::unmount_helper::{FUSE_TYPE, Refused, UnmountHelper, filter};

const CAP_SYS_ADMIN_MASK: &str = "0000000000200000";
/// The proof levels the kernel here (6.8+ for level 3) lets each test run at.
const LEVELS: [u8; 3] = [3, 2, 1];
/// The source name the tests mount tmpfs under (what a daemon's file system name is in production).
const SRC: &str = "t";

type Test = (&'static str, fn(&Path));

pub fn main() {
    if std::env::var_os("SACDFS_TEST_INNER").is_none() {
        return outer();
    }
    let dir = PathBuf::from(std::env::var_os("SACDFS_TEST_DIR").unwrap());
    let mut failed = 0;
    let tests: [Test; 19] = [
        ("hardened_helper_status", hardened),
        ("unmount_on_command_at_every_level", unmount_on_command),
        ("busy_mount_is_detached", busy_detached),
        ("u_before_m_refused", unarmed_refused),
        ("wrong_type_refused_at_arming", wrong_type),
        ("second_m_refused", second_arming),
        ("stacked_mount_refused_at_every_level", stacked_refused),
        ("recycled_path_refused", recycled_refused),
        ("successor_of_another_type_refused", other_type_successor),
        ("successor_under_another_name_refused", other_name_successor),
        ("wrong_source_name_refused_at_arming", wrong_source),
        ("stand_down_leaves_the_mount", stand_down),
        ("survives_stop_signals", signals),
        ("holds_no_descriptor_between_commands", no_pin),
        ("external_unmount_then_u_refused", external_then_command),
        ("daemon_killed_detaches_its_mount", killed_detaches),
        ("daemon_killed_leaves_a_successor_alone", killed_successor),
        ("filter_kills_outside_the_list", filter_kills),
        ("refuses_with_another_thread", threads),
    ];
    for (i, (name, f)) in tests.iter().enumerate() {
        let d = dir.join(format!("t{i}"));
        std::fs::create_dir_all(d.join("m")).unwrap();
        let r = std::panic::catch_unwind(|| f(&d));
        println!("test {name} ... {}", if r.is_ok() { "ok" } else { "FAILED" });
        failed += usize::from(r.is_err());
    }
    println!("\nunmount_helper: {} passed, {failed} failed", tests.len() - failed);
    std::process::exit(i32::from(failed != 0));
}

/// Runs this program again inside `unshare -Urm`.
fn outer() {
    let ok = Command::new("unshare").args(["-Urm", "true"]).status().is_ok_and(|s| s.success());
    if !ok {
        println!(
            "SKIP: cannot make a user and mount namespace here (unshare -Urm failed); run it on a lab host"
        );
        return;
    }
    let dir = std::env::temp_dir().join(format!("sacdfs-unmount-helper-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let st = Command::new("unshare")
        .args(["-Urm", "--"])
        .arg(std::env::current_exe().unwrap())
        .env("SACDFS_TEST_INNER", "1")
        .env("SACDFS_TEST_DIR", &dir)
        .status()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    std::process::exit(st.code().unwrap_or(1));
}

fn cstr(p: &Path) -> CString {
    use std::os::unix::ffi::OsStrExt;
    CString::new(p.as_os_str().as_bytes()).unwrap()
}

fn mount_tmpfs(m: &Path) {
    mount_tmpfs_as(m, c"t");
}

fn mount_tmpfs_as(m: &Path, source: &std::ffi::CStr) {
    // SAFETY: a raw libc call on local values, in a process with a single thread.
    let r = unsafe { libc::mount(source.as_ptr(), cstr(m).as_ptr(), c"tmpfs".as_ptr(), 0, std::ptr::null()) };
    assert_eq!(r, 0, "mount: {}", std::io::Error::last_os_error());
}

fn umount(m: &Path, flags: libc::c_int) -> i32 {
    // SAFETY: a raw libc call on local values.
    unsafe { libc::umount2(cstr(m).as_ptr(), flags) }
}

fn is_mounted(m: &Path) -> bool {
    mounts_at(m) > 0
}

fn mounts_at(m: &Path) -> usize {
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    info.lines().filter(|l| l.split(' ').nth(4) == Some(m.to_str().unwrap())).count()
}

/// Whether `pid` runs: a zombie does not count.
fn alive(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
    stat.rsplit_once(") ").is_some_and(|(_, rest)| !rest.starts_with('Z'))
}

fn errno_of<T: std::fmt::Debug>(r: std::io::Result<T>) -> i32 {
    r.unwrap_err().raw_os_error().unwrap()
}

fn refused_of<T: std::fmt::Debug>(r: std::io::Result<T>) -> Refused {
    let e = r.unwrap_err();
    let Some(r) = e.get_ref().and_then(|x| x.downcast_ref::<Refused>()) else { panic!("not a refusal: {e}") };
    Refused { recorded: r.recorded, found: r.found }
}

/// A helper for `m` expecting tmpfs, at most at `level`, and the mount made and armed.
fn armed(d: &Path, level: u8) -> (UnmountHelper, PathBuf) {
    let m = d.join("m");
    let mut h =
        UnmountHelper::spawn_for(&m, "tmpfs", SRC, level).unwrap().expect("CAP_SYS_ADMIN in the namespace");
    mount_tmpfs(&m);
    assert_eq!(h.mounted().unwrap(), level, "proof level");
    (h, m)
}

fn status_field(pid: i32, key: &str) -> String {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    s.lines().find_map(|l| l.strip_prefix(key)).unwrap().trim().to_owned()
}

fn wait_until(what: &str, limit: Duration, mut f: impl FnMut() -> bool) -> Duration {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < limit, "{what}: not within {limit:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    t.elapsed()
}

fn hardened(d: &Path) {
    // A descriptor of the "daemon" that the helper must not inherit.
    let _leak = std::fs::File::open("/etc/hostname").unwrap();
    let (mut h, m) = armed(d, 3);
    assert_eq!(h.degraded(), 0, "bounding set cut");
    let pid = h.pid();
    // NoNewPrivs is inherited: where this process already has it (a sandbox may do), the
    // helper's value proves nothing about the helper setting it. Say so in the log.
    let mine = std::fs::read_to_string("/proc/self/status").unwrap();
    if mine.lines().any(|l| l.trim() == "NoNewPrivs:\t1") {
        println!(
            "  note: NoNewPrivs is inherited here, so the helper's NoNewPrivs check is vacuous on this host (meaningful in the lab)"
        );
    }
    for (k, want) in [
        ("CapEff:", CAP_SYS_ADMIN_MASK),
        ("CapPrm:", CAP_SYS_ADMIN_MASK),
        ("CapBnd:", CAP_SYS_ADMIN_MASK),
        ("CapInh:", "0000000000000000"),
        ("CapAmb:", "0000000000000000"),
        ("NoNewPrivs:", "1"),
        ("Seccomp:", "2"),
        ("Threads:", "1"),
    ] {
        assert_eq!(status_field(pid, k), want, "{k}");
    }
    // The socket, mountinfo, and /dev/null on 0-2.
    let mut fds: Vec<String> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    fds.sort();
    assert_eq!(fds, ["0", "1", "2", "3", "4"], "open descriptors");
    assert_eq!(
        std::fs::read_link(format!("/proc/{pid}/fd/4")).unwrap(),
        Path::new(&format!("/proc/{pid}/mountinfo"))
    );
    assert_eq!(std::fs::read_link(format!("/proc/{pid}/cwd")).unwrap(), Path::new("/"));
    h.detach().unwrap();
    assert!(!is_mounted(&m));
}

fn unmount_on_command(d: &Path) {
    for level in LEVELS {
        let (mut h, m) = armed(d, level);
        assert!(is_mounted(&m));
        let t = Instant::now();
        h.detach().unwrap();
        println!("  level {level}: U took {:?}", t.elapsed());
        assert!(!is_mounted(&m), "level {level}");
        assert!(!alive(h.pid()), "the helper ends and is reaped");
        assert!(h.detach().is_err());
    }
}

/// A file open on the mount: the unmount is a detach (`MNT_DETACH`), not a refused plain one (#276).
fn busy_detached(d: &Path) {
    let (mut h, m) = armed(d, 3);
    std::fs::write(m.join("held"), b"x").unwrap();
    let held = std::fs::File::open(m.join("held")).unwrap();
    assert_ne!(umount(&m, 0), 0, "setup: a plain unmount of a busy mount is refused");
    h.detach().unwrap();
    assert!(!is_mounted(&m));
    // The file is still readable: sacdfs serves open files for the grace period.
    assert_eq!(std::io::read_to_string(&held).unwrap(), "x");
}

fn unarmed_refused(d: &Path) {
    let m = d.join("m");
    let mut h = UnmountHelper::spawn_for(&m, "tmpfs", SRC, 3).unwrap().unwrap();
    mount_tmpfs(&m);
    assert_eq!(errno_of(h.detach()), libc::EINVAL);
    assert!(is_mounted(&m));
    let _ = umount(&m, libc::MNT_DETACH);
}

fn wrong_type(d: &Path) {
    let m = d.join("m");
    // The production type on a tmpfs: arming is refused, nothing is armed, the mount stays.
    let mut h = UnmountHelper::spawn_for(&m, FUSE_TYPE, SRC, 3).unwrap().unwrap();
    mount_tmpfs(&m);
    assert_eq!(errno_of(h.mounted()), libc::ENOTSUP);
    assert_eq!(errno_of(h.detach()), libc::EINVAL);
    assert!(is_mounted(&m));
    h.stand_down();
    let _ = umount(&m, libc::MNT_DETACH);
}

fn second_arming(d: &Path) {
    let (mut h, m) = armed(d, 3);
    assert_eq!(errno_of(h.mounted()), libc::EALREADY);
    h.detach().unwrap();
    assert!(!is_mounted(&m));
}

/// Another mount on top of the armed one (so a different device number, at every level): refused,
/// with both identities, nothing unmounted, and the helper ends.
fn stacked_refused(d: &Path) {
    for level in LEVELS {
        let (mut h, m) = armed(d, level);
        let armed_dev = std::fs::metadata(&m).unwrap().dev();
        mount_tmpfs(&m);
        assert_eq!(mounts_at(&m), 2);
        let r = refused_of(h.detach());
        assert_ne!(r.recorded, r.found, "level {level}");
        assert_eq!(r.recorded.dev, armed_dev, "level {level}: the armed identity comes first");
        assert_ne!(r.found.dev, armed_dev, "level {level}: what was found second");
        assert_ne!(r.recorded.dev, r.found.dev, "level {level}: a second tmpfs has its own device number");
        println!("  level {level}: {}", Refused { recorded: r.recorded, found: r.found });
        assert_eq!(mounts_at(&m), 2, "nothing unmounted");
        wait_until("the helper ends after a refusal", Duration::from_secs(2), || !alive(h.pid()));
        assert!(h.detach().is_err(), "no retry");
        let _ = umount(&m, libc::MNT_DETACH);
        let _ = umount(&m, libc::MNT_DETACH);
    }
}

/// The mount ID of the (topmost) mount at `m`, from the mount table.
fn table_id(m: &Path) -> u64 {
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    let l = info.lines().rev().find(|l| l.split(' ').nth(4) == Some(m.to_str().unwrap())).unwrap();
    l.split(' ').next().unwrap().parse().unwrap()
}

/// The path reused by another mount after the armed one went. The device number is recycled
/// (measured: the same before and after); the unique mount ID (level 3) never is. The plain
/// mount ID of levels 2 and 1 can be: when the kernel gave the newcomer the old one, those
/// weaker levels cannot tell the two apart (documented), and the test says so instead of
/// failing; when it gave another, they must refuse.
fn recycled_refused(d: &Path) {
    for level in LEVELS {
        let (mut h, m) = armed(d, level);
        let id_before = table_id(&m);
        let dev_before = std::fs::metadata(&m).unwrap().dev();
        assert_eq!(umount(&m, 0), 0);
        mount_tmpfs(&m);
        let (id_after, dev_after) = (table_id(&m), std::fs::metadata(&m).unwrap().dev());
        println!(
            "  level {level}: device {dev_before:#x} -> {dev_after:#x}, table mount id {id_before} -> {id_after}"
        );
        if level == 3 || id_before != id_after {
            let r = refused_of(h.detach());
            assert_ne!(r.recorded.mount_id, r.found.mount_id, "level {level}: the mount ID differs");
            assert!(is_mounted(&m), "the new mount is left alone");
        } else {
            println!(
                "  level {level}: the kernel reused the mount ID: this level cannot refuse (documented)"
            );
            h.stand_down();
        }
        let _ = umount(&m, libc::MNT_DETACH);
    }
}

/// The path reused by a mount of ANOTHER type (ramfs, also an anonymous device number): the weak
/// levels, which cannot always tell a recycled mount ID, refuse it by the type.
fn other_type_successor(d: &Path) {
    for level in [2, 1] {
        let (mut h, m) = armed(d, level);
        assert_eq!(umount(&m, 0), 0);
        // SAFETY: a raw libc call on local values, in a process with a single thread.
        let r =
            unsafe { libc::mount(c"t".as_ptr(), cstr(&m).as_ptr(), c"ramfs".as_ptr(), 0, std::ptr::null()) };
        assert_eq!(r, 0, "mount ramfs: {}", std::io::Error::last_os_error());
        let _ = refused_of(h.detach());
        assert!(is_mounted(&m), "level {level}: the ramfs is left alone");
        let _ = umount(&m, libc::MNT_DETACH);
    }
}

/// The same type under another name (the daemon's file system name): refused at the weak levels.
fn other_name_successor(d: &Path) {
    for level in [2, 1] {
        let (mut h, m) = armed(d, level);
        assert_eq!(umount(&m, 0), 0);
        mount_tmpfs_as(&m, c"other");
        let _ = refused_of(h.detach());
        assert!(is_mounted(&m), "level {level}: the newcomer is left alone");
        let _ = umount(&m, libc::MNT_DETACH);
    }
}

/// A mount under another name than the daemon's: arming is refused.
fn wrong_source(d: &Path) {
    let m = d.join("m");
    let mut h = UnmountHelper::spawn_for(&m, "tmpfs", "not-the-name", 3).unwrap().unwrap();
    mount_tmpfs(&m);
    assert_eq!(errno_of(h.mounted()), libc::ENOTSUP);
    h.stand_down();
    assert!(is_mounted(&m));
    let _ = umount(&m, libc::MNT_DETACH);
}

use std::os::unix::fs::MetadataExt;

fn stand_down(d: &Path) {
    let (mut h, m) = armed(d, 3);
    let pid = h.pid();
    h.stand_down();
    assert!(is_mounted(&m), "stand down must not unmount");
    assert!(!alive(pid));
    let _ = umount(&m, libc::MNT_DETACH);
}

fn signals(d: &Path) {
    let (mut h, m) = armed(d, 3);
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT, libc::SIGPIPE] {
        // SAFETY: a signal to the helper, our child.
        assert_eq!(unsafe { libc::kill(h.pid(), sig) }, 0);
    }
    std::thread::sleep(Duration::from_millis(100));
    h.detach().unwrap();
    assert!(!is_mounted(&m));
}

/// No descriptor on the mount between commands: a plain umount (what `fusermount3 -u` does) works.
fn no_pin(d: &Path) {
    let (mut h, m) = armed(d, 3);
    assert_eq!(umount(&m, 0), 0, "{}", std::io::Error::last_os_error());
    assert!(!is_mounted(&m));
    h.stand_down();
}

/// After an unmount from outside, the helper must not touch what is under the path.
fn external_then_command(d: &Path) {
    for level in LEVELS {
        let (mut h, m) = armed(d, level);
        assert_eq!(umount(&m, 0), 0);
        let r = refused_of(h.detach());
        assert_ne!(r.recorded, r.found, "level {level}");
        assert!(
            !alive(h.pid())
                || wait_until("ends", Duration::from_secs(2), || !alive(h.pid())) > Duration::ZERO
        );
    }
}

/// In a forked child: runs `f` and never returns into the test loop, whatever happens (a panic
/// would otherwise run the remaining tests in the child while the parent waits for it).
fn in_child(f: impl FnOnce()) -> ! {
    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_ok();
    // SAFETY: leave without running the parent's atexit handlers.
    unsafe { libc::_exit(if ok { 0 } else { 70 }) }
}

/// Forks a "daemon" that arms a helper on a tmpfs mount of `m` and waits; returns its pid and the helper's.
fn fork_daemon(m: &Path, level: u8) -> (i32, i32) {
    let mut pipe = [0; 2];
    // SAFETY: a raw libc call on local values, in a process with a single thread.
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    // SAFETY: fork in a process with a single thread.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        in_child(|| {
            let h = UnmountHelper::spawn_for(m, "tmpfs", SRC, level).unwrap().unwrap();
            let mut h = std::mem::ManuallyDrop::new(h);
            mount_tmpfs(m);
            h.mounted().unwrap();
            let hp = h.pid().to_le_bytes();
            // SAFETY: a write of four bytes from the stack, then waiting to be killed.
            unsafe {
                libc::write(pipe[1], hp.as_ptr().cast(), 4);
                libc::pause();
                libc::_exit(0)
            }
        })
    }
    // SAFETY: the parent's copy of the write end, closed so that a dead child ends the read.
    unsafe { libc::close(pipe[1]) };
    let mut hp = [0u8; 4];
    // SAFETY: a read of four bytes into the stack.
    assert_eq!(unsafe { libc::read(pipe[0], hp.as_mut_ptr().cast(), 4) }, 4);
    (pid, i32::from_le_bytes(hp))
}

fn kill_and_reap(pid: i32) {
    // SAFETY: SIGKILL to our own child, then waiting for it.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
    }
}

/// The daemon dies without a word: the helper sees end of file and detaches ITS mount, with no
/// wait and no call to the daemon.
fn killed_detaches(d: &Path) {
    for level in LEVELS {
        let m = d.join("m");
        let (daemon, helper) = fork_daemon(&m, level);
        assert!(is_mounted(&m));
        let t = Instant::now();
        kill_and_reap(daemon);
        wait_until("the mount goes", Duration::from_secs(2), || !is_mounted(&m));
        println!("  level {level}: mount gone {:?} after the kill", t.elapsed());
        wait_until("the helper ends", Duration::from_secs(2), || !alive(helper));
    }
}

/// The armed mount went, another took its path, and then the daemon died: the successor stays.
fn killed_successor(d: &Path) {
    let m = d.join("m");
    let (daemon, helper) = fork_daemon(&m, 3);
    assert_eq!(umount(&m, 0), 0);
    mount_tmpfs(&m);
    kill_and_reap(daemon);
    wait_until("the helper ends", Duration::from_secs(2), || !alive(helper));
    assert!(is_mounted(&m), "the successor must stay");
    let _ = umount(&m, libc::MNT_DETACH);
}

/// The installed program, on the real kernel: allowed calls pass, any other kills with SIGSYS.
fn filter_kills(_: &Path) {
    let prog = filter().expect("filter on this architecture");
    for allowed in [false, true] {
        // SAFETY: fork in a process with a single thread.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: raw calls on local values in the fresh child.
            unsafe {
                libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
                let fp = libc::sock_fprog { len: prog.len() as u16, filter: prog.as_ptr().cast_mut() };
                if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &raw const fp) != 0 {
                    libc::_exit(3);
                }
                if allowed {
                    libc::write(1, b"".as_ptr().cast(), 0);
                    libc::_exit(0);
                }
                // fstatfs: what the first design asked the daemon with.
                libc::fstatfs(0, std::ptr::null_mut());
                libc::_exit(0);
            }
        }
        let mut st = 0;
        // SAFETY: waiting for our child.
        unsafe { libc::waitpid(pid, &raw mut st, 0) };
        if allowed {
            assert!(libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 0, "allowed calls: status {st}");
        } else {
            assert!(libc::WIFSIGNALED(st) && libc::WTERMSIG(st) == libc::SIGSYS, "denied call: status {st}");
        }
    }
}

/// Another thread exists: no helper (it must fork before any).
fn threads(d: &Path) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let t = std::thread::spawn(move || {
        let _ = rx.recv();
    });
    let e = UnmountHelper::spawn_for(&d.join("m"), "tmpfs", SRC, 3).unwrap_err();
    assert!(e.to_string().contains("threads"), "{e}");
    tx.send(()).unwrap();
    t.join().unwrap();
}
