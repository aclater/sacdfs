// SPDX-License-Identifier: GPL-3.0-or-later

//! The log line of every request sacdfs fails with an I/O error, rate-limited (#339).
//!
//! Every path that answers a request with `EIO` (or with the errno of a failed source
//! read) calls [`failed`] with what failed (an image and track, a sidecar, an inode), the
//! offset where there is one, and the cause. So an `EIO` a client sees with no line in the
//! log came from the kernel or from fuser (which logs its own at `warn`), not from sacdfs.
//!
//! *Why rate-limited:* a damaged image, or a hung share, fails every read of it, and a client
//! or a scan retries: a line per read would flood the log and bury the first one. Lines with
//! the same key (the image, or the inode) are logged at most once per [`WINDOW`]; the next
//! one after that says how many were left out. *Why per key and not one limit for all:* the
//! line that matters is often the one failure of a healthy image while another image fails
//! many times (#339), and one shared limit would have dropped it. A cap on all keys together,
//! [`MAX_LINES_PER_WINDOW`], keeps many failing images (a whole share gone) from flooding the
//! log too: past it, one line per window says how many were left out.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// Lines with one key are logged at most once per this.
pub const WINDOW: Duration = Duration::from_secs(60);

/// At most this many lines per [`WINDOW`], all keys together.
pub const MAX_LINES_PER_WINDOW: u32 = 50;

/// Keys remembered at most; older ones are forgotten first (a key forgotten logs again).
const MAX_KEYS: usize = 4096;

#[derive(Debug)]
struct Key {
    /// When its last line was logged; `None` if none was (the cap left them all out).
    last: Option<Instant>,
    left_out: u64,
}

#[derive(Debug)]
struct State {
    keys: HashMap<String, Key>,
    window_start: Instant,
    lines: u32,
    left_out: u64,
}

/// A rate limit for the lines of failed requests: see the [module](self).
#[derive(Debug)]
pub struct Limiter {
    window: Duration,
    max_lines: u32,
    state: Mutex<State>,
    logged: AtomicU64,
}

impl Limiter {
    /// A limit of one line per key per `window`, and `max_lines` per window in all.
    pub fn new(window: Duration, max_lines: u32) -> Limiter {
        Limiter {
            window,
            max_lines,
            state: Mutex::new(State {
                keys: HashMap::new(),
                window_start: Instant::now(),
                lines: 0,
                left_out: 0,
            }),
            logged: AtomicU64::new(0),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether a line with `key` is to be logged now: `Some(n)` with the lines of that key
    /// left out since its last one, and of all keys if the cap left any out; `None` to leave
    /// it out.
    fn admit(&self, key: &str) -> Option<(u64, u64)> {
        let now = Instant::now();
        let mut st = self.state();
        let mut all_left_out = 0;
        if now.duration_since(st.window_start) >= self.window {
            st.window_start = now;
            st.lines = 0;
            all_left_out = std::mem::take(&mut st.left_out);
        }
        let window = self.window;
        let due = st.keys.get(key).and_then(|k| k.last).is_none_or(|last| now.duration_since(last) >= window);
        if !due || st.lines >= self.max_lines {
            st.keys.entry(key.to_owned()).or_insert(Key { last: None, left_out: 0 }).left_out += 1;
            // Those of the last window are told on the next line logged.
            st.left_out += 1 + all_left_out;
            return None;
        }
        st.lines += 1;
        if st.keys.len() >= MAX_KEYS && !st.keys.contains_key(key) {
            st.keys.retain(|_, k| k.last.is_some_and(|last| now.duration_since(last) < window));
            if st.keys.len() >= MAX_KEYS {
                st.keys.clear();
            }
        }
        let k = st.keys.entry(key.to_owned()).or_insert(Key { last: None, left_out: 0 });
        k.last = Some(now);
        let mine = std::mem::take(&mut k.left_out);
        // This key's lines left out are told here, not again in the count of all keys: those
        // still in this window's count first, then those of the last one.
        let here = mine.min(st.left_out);
        st.left_out -= here;
        Some((mine, all_left_out.saturating_sub(mine - here)))
    }

    /// Logs at `warn` that a request about `key` failed, unless lines with that key, or too
    /// many lines, were logged lately; whether it logged. `msg` says what failed, where and
    /// why, and the errno the client gets.
    pub fn failed(&self, key: &str, msg: fmt::Arguments<'_>) -> bool {
        let Some((mine, others)) = self.admit(key) else { return false };
        let line = match (mine, others) {
            (0, 0) => format!("{msg}"),
            (m, 0) => format!("{msg} (and {m} more like it in the last {} s)", self.window.as_secs()),
            (0, o) => format!(
                "{msg} ({o} other failed requests were not logged in the last {} s)",
                self.window.as_secs()
            ),
            (m, o) => format!(
                "{msg} (and {m} more like it, and {o} other failed requests, not logged in the \
                 last {} s)",
                self.window.as_secs()
            ),
        };
        log::warn!("{line}");
        self.logged.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        tests::note(&line);
        true
    }

    /// Lines logged so far.
    pub fn logged(&self) -> u64 {
        self.logged.load(Ordering::Relaxed)
    }
}

/// The process's limiter.
pub fn limiter() -> &'static Limiter {
    static LIMITER: OnceLock<Limiter> = OnceLock::new();
    // Unit tests share the process: each checks its own key, and the cap on all keys has a
    // test of its own.
    let cap = if cfg!(test) { u32::MAX } else { MAX_LINES_PER_WINDOW };
    LIMITER.get_or_init(|| Limiter::new(WINDOW, cap))
}

/// Logs a failed request about `key` with the process's [`limiter`]: see [`Limiter::failed`].
pub fn failed(key: &str, msg: fmt::Arguments<'_>) -> bool {
    limiter().failed(key, msg)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    pub(crate) fn note(line: &str) {
        LINES.lock().unwrap_or_else(|p| p.into_inner()).push(line.to_owned());
    }

    /// The lines logged so far in this test process that contain `text`.
    pub(crate) fn lines_with(text: &str) -> Vec<String> {
        LINES.lock().unwrap_or_else(|p| p.into_inner()).iter().filter(|l| l.contains(text)).cloned().collect()
    }

    #[test]
    fn one_line_per_key_per_window_and_the_count_left_out() {
        let l = Limiter::new(Duration::from_millis(300), 100);
        assert!(l.failed("a.iso", format_args!("limit-test a.iso: read at 0: EIO")));
        for _ in 0..10 {
            assert!(!l.failed("a.iso", format_args!("limit-test a.iso: again")));
        }
        // Another key is not held back by the first one's lines (#339).
        assert!(l.failed("b.iso", format_args!("limit-test b.iso: read at 4096: EIO")));
        std::thread::sleep(Duration::from_millis(350));
        assert!(l.failed("a.iso", format_args!("limit-test a.iso: later")));
        assert_eq!(l.logged(), 3);
        let later = lines_with("limit-test a.iso: later");
        assert_eq!(later.len(), 1);
        assert!(later[0].contains("and 10 more like it"), "{later:?}");
    }

    #[test]
    fn a_cap_on_all_keys_together() {
        let l = Limiter::new(Duration::from_millis(300), 3);
        let logged = (0..20).filter(|i| l.failed(&format!("cap-{i}"), format_args!("cap-test {i}"))).count();
        assert_eq!(logged, 3);
        std::thread::sleep(Duration::from_millis(350));
        assert!(l.failed("cap-new", format_args!("cap-test after")));
        let after = lines_with("cap-test after");
        assert!(after[0].contains("17 other failed requests"), "{after:?}");
    }
}
