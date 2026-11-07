// SPDX-License-Identifier: GPL-3.0-or-later

//! The mount's background lookup (`--musicbrainz`, #185, phase 4 for MusicBrainz): one thread
//! that goes through the mount's images and looks up, by [`super::fetch_image`], each disc that
//! lacks a track title, its album or its artist. It writes to the tags directory as `sacdfs tags
//! fetch` does, and the tag layer serves what it writes: the files' tags change and their mtimes
//! move, as for any edit of a tags file (#81).
//!
//! **Never in the FUSE path.** No request of a client waits for this thread or the network: it
//! shares nothing with the serving side but the tags directory, read as for a hand edit, and the
//! open image handles, which every image already shares. Its image reads are TOCs only, at idle
//! I/O priority and nice 19.
//!
//! **Once per disc.** A match is kept in the tags file, so it survives restarts and offline use
//! and is never asked again. What else became of an image (nothing missing, no confident match)
//! is kept in the seek index cache directory, so that a restart doesn't ask again either; an
//! image whose size or mtime changes is looked at anew, and a disc without a match is asked again
//! after [`RETRY_REFUSED`], since MusicBrainz grows. Without a cache directory that is only kept
//! for the mount's lifetime.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use super::{Client, Failure, MAX_FAILED_IN_A_ROW, Options, Outcome, Transport, WRITING, fetch_image};

/// Wait after the mount before the first pass, so that a scanner's first look at a new mount
/// doesn't share the source with it.
pub const FIRST_DELAY: Duration = Duration::from_secs(30);
/// Wait between passes. A pass only reads TOCs of images it hasn't settled, so most are free.
pub const PASS_EVERY: Duration = Duration::from_secs(3600);
/// Wait after the service refused this client, or asked for a wait longer than a run makes.
pub const AFTER_STOP: Duration = Duration::from_secs(24 * 3600);
/// A disc without a confident match is asked again after this long.
pub const RETRY_REFUSED: Duration = Duration::from_secs(30 * 24 * 3600);
/// The first line of the state file.
const STATE_HEADER: &str = "sacdfs musicbrainz 1";

/// What the worker needs.
#[derive(Clone, Debug)]
pub struct Config {
    /// The tags directory: where matches go.
    pub tags: PathBuf,
    /// Where what became of each image is kept across mounts; `None`: kept for this mount only.
    pub state: Option<PathBuf>,
    /// The steps and the album suffix, as `sacdfs tags fetch` takes them.
    pub options: Options,
    /// Wait before the first pass.
    pub first_delay: Duration,
    /// Wait between passes.
    pub pass_every: Duration,
}

/// What a pass did, for the log and for tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassSummary {
    /// Images looked at (their TOC read).
    pub looked_at: usize,
    /// Images left alone: settled by an earlier pass or mount.
    pub settled: usize,
    /// Images with nothing missing.
    pub complete: usize,
    /// Images matched.
    pub matched: usize,
    /// Images without a confident match.
    pub refused: usize,
    /// Images already looked up (a tags file says so).
    pub done: usize,
    /// Images that failed: unreadable, no answer, or a tags file that doesn't parse.
    pub failed: usize,
    /// Requests made.
    pub requests: usize,
    /// Whether the pass ended early: stopped, failures in a row, or the service said stop.
    pub cut_short: bool,
}

/// What became of an image, as kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Settled {
    Complete,
    Matched,
    Refused,
}

impl Settled {
    fn word(self) -> &'static str {
        match self {
            Settled::Complete => "complete",
            Settled::Matched => "matched",
            Settled::Refused => "refused",
        }
    }

    fn of(word: &str) -> Option<Settled> {
        match word {
            "complete" => Some(Settled::Complete),
            "matched" => Some(Settled::Matched),
            "refused" => Some(Settled::Refused),
            _ => None,
        }
    }
}

/// One image's entry: the image's size and mtime (ns) when settled, how, and when (s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    len: u64,
    mtime: u64,
    how: Settled,
    at: u64,
}

/// The images settled, by a hash of their path (no path is kept in the cache directory).
#[derive(Debug, Default)]
struct State {
    entries: HashMap<String, Entry>,
    file: Option<PathBuf>,
    dirty: bool,
}

fn path_hash(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    crate::verify::id3::hex(&Sha256::digest(p.as_os_str().as_bytes())[..16])
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// An image's size and mtime in ns, if it can be read.
fn stamp(image: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(image).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((meta.len(), u64::try_from(mtime.as_nanos()).unwrap_or(u64::MAX)))
}

impl State {
    fn load(file: Option<PathBuf>) -> State {
        let mut entries = HashMap::new();
        if let Some(text) = file.as_deref().and_then(|f| fs::read_to_string(f).ok()) {
            let mut lines = text.lines();
            if lines.next() == Some(STATE_HEADER) {
                for line in lines {
                    let f: Vec<&str> = line.split('\t').collect();
                    let [hash, len, mtime, how, at] = f[..] else { continue };
                    let (Ok(len), Ok(mtime), Some(how), Ok(at)) =
                        (len.parse(), mtime.parse(), Settled::of(how), at.parse())
                    else {
                        continue;
                    };
                    entries.insert(hash.to_owned(), Entry { len, mtime, how, at });
                }
            }
        }
        State { entries, file, dirty: false }
    }

    /// Whether `image`, now at `stamp`, needs no look: settled at this size and mtime, and not a
    /// refusal old enough to ask again.
    fn settled(&self, image: &Path, stamp: (u64, u64), retry: Duration) -> bool {
        self.entries.get(&path_hash(image)).is_some_and(|e| {
            (e.len, e.mtime) == stamp
                && (e.how != Settled::Refused || now_secs().saturating_sub(e.at) < retry.as_secs())
        })
    }

    fn note(&mut self, image: &Path, stamp: (u64, u64), how: Settled) {
        let e = Entry { len: stamp.0, mtime: stamp.1, how, at: now_secs() };
        self.entries.insert(path_hash(image), e);
        self.dirty = true;
    }

    /// Writes the file, if any and if changed. A failure is logged: it only costs a second look.
    fn save(&mut self) {
        let Some(file) = self.file.as_deref().filter(|_| self.dirty) else { return };
        let mut text = format!("{STATE_HEADER}\n");
        let mut rows: Vec<_> = self.entries.iter().collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        for (h, e) in rows {
            text.push_str(&format!("{h}\t{}\t{}\t{}\t{}\n", e.len, e.mtime, e.how.word(), e.at));
        }
        let r = file
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| crate::tags_cli::write_atomic(file, text.as_bytes()));
        match r {
            Ok(()) => self.dirty = false,
            Err(e) => log::warn!("musicbrainz: {}: {e}", file.display()),
        }
    }
}

/// A stop that wakes the worker from its waits between discs and passes.
#[derive(Debug, Default)]
struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
    flag: AtomicBool,
}

impl Stop {
    /// Waits `d`, or less if stopped. Whether stopped.
    fn wait(&self, d: Duration) -> bool {
        let g = self.stopped.lock().unwrap_or_else(|p| p.into_inner());
        let (g, _) = self.wake.wait_timeout_while(g, d, |s| !*s).unwrap_or_else(|p| p.into_inner());
        *g
    }

    fn is_set(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    fn set(&self) {
        self.flag.store(true, Ordering::Relaxed);
        *self.stopped.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.wake.notify_all();
    }
}

/// The running worker.
#[derive(Debug)]
pub struct Worker {
    stop: Arc<Stop>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    /// Starts the worker on its own thread: it goes through `images()` (called at each pass, so
    /// that it sees what the mount sees) with `client`.
    pub fn start<T, F>(config: Config, client: Client<T>, images: F) -> std::io::Result<Worker>
    where
        T: Transport + Send + 'static,
        F: Fn() -> Vec<PathBuf> + Send + 'static,
    {
        let stop = Arc::new(Stop::default());
        let s = stop.clone();
        let thread = thread::Builder::new().name("sacdfs-lookup".to_owned()).spawn(move || {
            lower_priority();
            run(&config, client, images, &s);
        })?;
        Ok(Worker { stop, thread: Some(thread) })
    }

    /// Stops the worker at its next wait or disc, after a tags file write under way: nothing half
    /// written is left. A request under way isn't waited for (it may take a minute); the thread
    /// ends with the process.
    pub fn stop(&mut self) {
        self.stop.set();
        let _writing = WRITING.lock().unwrap_or_else(|p| p.into_inner());
        super::STOPPING.store(true, Ordering::Relaxed);
        drop(_writing);
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    /// Stops the worker and waits for it: for tests, whose transport never waits.
    pub fn stop_and_join(mut self) {
        self.stop.set();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Nice 19 and the idle I/O class for this thread, where the system allows: the worker's TOC
/// reads and its CPU come after everything a client asks for. Best effort.
#[allow(unsafe_code)]
fn lower_priority() {
    // SAFETY: plain system calls on the calling thread, with no pointers.
    unsafe {
        let tid = libc::gettid();
        if libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, 19) != 0 {
            log::debug!("musicbrainz: nice: {}", std::io::Error::last_os_error());
        }
        // ioprio_set(IOPRIO_WHO_PROCESS, tid, IOPRIO_PRIO_VALUE(IOPRIO_CLASS_IDLE, 0)).
        const IOPRIO_WHO_PROCESS: libc::c_int = 1;
        const IOPRIO_CLASS_IDLE: libc::c_int = 3;
        if libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, tid, IOPRIO_CLASS_IDLE << 13) != 0 {
            log::debug!("musicbrainz: ioprio: {}", std::io::Error::last_os_error());
        }
    }
}

fn run<T: Transport>(config: &Config, mut client: Client<T>, images: impl Fn() -> Vec<PathBuf>, stop: &Stop) {
    let mut state = State::load(config.state.clone());
    if stop.wait(config.first_delay) {
        return;
    }
    loop {
        let (sum, stopped_by_service) = pass(config, &mut client, &images(), &mut state, stop);
        state.save();
        log::info!(
            "musicbrainz: pass done: {} images looked at, {} settled before; {} complete, {} matched, {} without a match, {} looked up before, {} failed; {} requests{}",
            sum.looked_at,
            sum.settled,
            sum.complete,
            sum.matched,
            sum.refused,
            sum.done,
            sum.failed,
            sum.requests,
            if sum.cut_short { "; cut short" } else { "" },
        );
        let wait = if stopped_by_service { AFTER_STOP } else { config.pass_every };
        if stop.wait(wait) {
            return;
        }
    }
}

/// One pass over `images`. Whether the service said to stop.
fn pass<T: Transport>(
    config: &Config,
    client: &mut Client<T>,
    images: &[PathBuf],
    state: &mut State,
    stop: &Stop,
) -> (PassSummary, bool) {
    let mut sum = PassSummary::default();
    let before = client.requests();
    let mut failed_in_a_row = 0;
    let options = Options { only_wanting: true, dry_run: false, ..config.options };
    let mut stopped_by_service = false;
    for image in images {
        if stop.is_set() {
            sum.cut_short = true;
            break;
        }
        let Some(st) = stamp(image) else {
            sum.failed += 1;
            continue;
        };
        if state.settled(image, st, RETRY_REFUSED) {
            sum.settled += 1;
            continue;
        }
        sum.looked_at += 1;
        let asked = client.requests();
        let r = fetch_image(&config.tags, image, options, client, |line| log::info!("musicbrainz: {line}"));
        match r {
            Ok(o) => {
                failed_in_a_row = 0;
                let how = match o {
                    Outcome::Complete => {
                        sum.complete += 1;
                        Settled::Complete
                    }
                    Outcome::Done => {
                        sum.done += 1;
                        Settled::Matched
                    }
                    Outcome::Matched => {
                        sum.matched += 1;
                        Settled::Matched
                    }
                    Outcome::Refused => {
                        sum.refused += 1;
                        Settled::Refused
                    }
                };
                state.note(image, st, how);
                // Kept as it goes: a disc that cost requests shouldn't cost them again after a
                // restart. Discs that cost none are kept at the end of the pass.
                if client.requests() > asked {
                    state.save();
                }
            }
            Err(Failure::Local(e)) => {
                sum.failed += 1;
                log::info!("musicbrainz: {}: nothing written: {e}", image.display());
            }
            Err(Failure::Network(e)) => {
                sum.failed += 1;
                failed_in_a_row += 1;
                log::warn!("musicbrainz: {}: nothing written: {e}", image.display());
                if failed_in_a_row >= MAX_FAILED_IN_A_ROW {
                    log::warn!(
                        "musicbrainz: {failed_in_a_row} discs without an answer in a row: trying again in {} min",
                        config.pass_every.as_secs() / 60
                    );
                    sum.cut_short = true;
                    break;
                }
                if stop.wait(client.cool_off(failed_in_a_row)) {
                    sum.cut_short = true;
                    break;
                }
            }
            Err(Failure::Stop(e)) => {
                sum.failed += 1;
                log::warn!(
                    "musicbrainz: {}: {e}; no more lookups for {} h",
                    image.display(),
                    AFTER_STOP.as_secs() / 3600
                );
                sum.cut_short = true;
                stopped_by_service = true;
                break;
            }
        }
    }
    sum.requests = client.requests() - before;
    (sum, stopped_by_service)
}

/// Runs one pass in the calling thread, for tests: what [`Worker`] does at each pass.
pub fn pass_once<T: Transport>(config: &Config, client: &mut Client<T>, images: &[PathBuf]) -> PassSummary {
    let mut state = State::load(config.state.clone());
    let (sum, _) = pass(config, client, images, &mut state, &Stop::default());
    state.save();
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_settles_by_stamp() {
        let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let file = d.path().join("musicbrainz");
        let image = Path::new("/images/a.iso");
        let mut s = State::load(Some(file.clone()));
        s.note(image, (10, 20), Settled::Complete);
        s.note(Path::new("/images/b.iso"), (1, 2), Settled::Refused);
        s.save();
        let s = State::load(Some(file));
        assert!(s.settled(image, (10, 20), RETRY_REFUSED));
        assert!(!s.settled(image, (10, 21), RETRY_REFUSED), "a changed image is looked at again");
        assert!(s.settled(Path::new("/images/b.iso"), (1, 2), RETRY_REFUSED));
        assert!(!s.settled(Path::new("/images/b.iso"), (1, 2), Duration::ZERO), "a refusal is retried");
        assert!(!s.settled(Path::new("/images/c.iso"), (1, 2), RETRY_REFUSED));
    }

    #[test]
    fn a_stop_wakes_a_wait() {
        let stop = Arc::new(Stop::default());
        let s = stop.clone();
        let t = thread::spawn(move || s.wait(Duration::from_secs(600)));
        thread::sleep(Duration::from_millis(50));
        stop.set();
        assert!(t.join().unwrap_or(false));
    }
}
