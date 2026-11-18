// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs tags fetch --acoustid` (#296): the last resort for a disc that nothing else identifies.
//! Each track of the stereo area is fingerprinted ([`super::fingerprint`]) and looked up in
//! AcoustID, whose answers name MusicBrainz recordings and the releases that hold them. The
//! releases that hold the most of the disc's tracks go to the same checks as every other lookup
//! (track count and durations, then agreement), with one more: the release's tracks must be the
//! fingerprinted recordings, in order ([`super::Step::Fingerprint`]).
//!
//! The bounds, from #185 and #296: one disc at a time on one thread, at nice 19 and the idle I/O
//! class (set by the command); a pause while the load average is over a limit; a daily budget of
//! discs, kept with what became of each disc in the tags directory, so that a restart or the next
//! day's run goes on where it was; at most 3 requests a second to AcoustID; timeouts on every
//! request and on `fpcalc`; bounded retries; a stop (Ctrl-C, SIGTERM) between seconds of audio
//! that writes nothing half done and keeps the state.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::fingerprint::{self, FPCALC_TIMEOUT, Fed};
use super::mb::{self, Hit, Transport, encode, is_mbid};
use crate::disc::Disc;

/// The service. Fixed, as MusicBrainz's is: what is sent, and to whom, has one documented answer.
pub const API: &str = "https://api.acoustid.org/v2/lookup";
/// Time between two requests, retries included. AcoustID allows 3 a second; this keeps under it
/// with room for a clock's jitter.
pub const MIN_INTERVAL: Duration = Duration::from_millis(400);
/// Discs fingerprinted a day, unless told otherwise.
pub const DEFAULT_BUDGET: usize = 20;
/// A disc fingerprinted without a match is fingerprinted again after this long: AcoustID grows.
pub const RETRY_REFUSED: Duration = Duration::from_secs(30 * 24 * 3600);
/// Shortest track fingerprinted, in seconds: Chromaprint gives nothing for under about 3 s, and
/// little a lookup can trust for under 10.
pub const MIN_SECONDS: u32 = 10;
/// Least score of an AcoustID result taken.
pub const MIN_SCORE: f64 = 0.5;
/// Most releases read from MusicBrainz for one disc: those holding the most of its tracks.
pub const MAX_RELEASES: usize = 5;
/// What is asked of AcoustID: the recordings of each result, with the ids of their releases.
const META: &str = "recordings+releaseids";
/// Tries of one request.
const ATTEMPTS: u32 = 4;
/// Wait before the second try; it doubles after that.
const BACKOFF: Duration = Duration::from_secs(2);
/// Wait between looks at the load average while it is too high.
const LOAD_POLL: Duration = Duration::from_secs(15);
/// Most recordings kept of one track's answer, and most releases of one recording.
const MAX_RECORDINGS: usize = 32;
const MAX_RELEASES_PER_RECORDING: usize = 256;
/// The first line of the state file.
const STATE_HEADER: &str = "sacdfs acoustid 1";
/// The state file, in the tags directory (hidden, so never taken for a tags file).
pub const STATE_FILE: &str = ".acoustid-state";

/// Why a request gave nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// No usable answer, after the retries.
    Failed(String),
    /// The service refused this client or its key: nothing more is asked in this run.
    Stop(String),
}

/// What the run is given.
#[derive(Clone, Debug)]
pub struct Config {
    /// The AcoustID application key: each user's own. Never logged.
    pub key: String,
    /// The `fpcalc` program.
    pub fpcalc: PathBuf,
    /// Discs fingerprinted a day.
    pub budget: usize,
    /// The 1-minute load average over which fingerprinting waits; `None`: never waits.
    pub max_load: Option<f64>,
    /// Wait between requests.
    pub interval: Duration,
    /// Wait before the second try of a request.
    pub backoff: Duration,
    /// Tracks shorter than this, in seconds, are not fingerprinted.
    pub min_seconds: u32,
}

impl Config {
    /// A configuration with the service's pace and the default budget.
    pub fn new(key: String, fpcalc: PathBuf) -> Config {
        let cpus = thread::available_parallelism().map_or(1, |n| n.get());
        Config {
            key,
            fpcalc,
            budget: DEFAULT_BUDGET,
            max_load: Some(cpus as f64),
            interval: MIN_INTERVAL,
            backoff: BACKOFF,
            min_seconds: MIN_SECONDS,
        }
    }
}

/// What a run did, for its summary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Discs that would have been fingerprinted.
    pub queued: usize,
    /// Discs fingerprinted.
    pub done: usize,
    /// Discs not fingerprinted: done before without a match, recently.
    pub skipped: usize,
    /// Discs left for another day: the budget was spent.
    pub deferred: usize,
    /// Tracks fingerprinted.
    pub tracks: usize,
    /// Requests to AcoustID.
    pub requests: usize,
}

/// One disc's fingerprints, as AcoustID answered them.
#[derive(Clone, Debug, Default)]
pub struct Prints {
    /// Per track of the stereo area, in order: the recordings AcoustID gave.
    pub recordings: Vec<BTreeSet<String>>,
    /// The releases holding the most of those tracks, most first.
    pub releases: Vec<Hit>,
}

/// What became of a disc fingerprinted, as kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// Matched: its tags file says so as well.
    Matched,
    /// No confident match.
    Refused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    len: u64,
    mtime: u64,
    how: Settled,
    at: u64,
}

/// The discs fingerprinted, by a hash of their path, and today's count.
#[derive(Debug, Default)]
struct State {
    file: Option<PathBuf>,
    day: u64,
    count: usize,
    entries: HashMap<String, Entry>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn path_hash(p: &Path) -> String {
    crate::verify::id3::hex(&Sha256::digest(p.as_os_str().as_encoded_bytes())[..16])
}

fn stamp(image: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(image).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((meta.len(), u64::try_from(mtime.as_nanos()).unwrap_or(u64::MAX)))
}

impl State {
    fn load(file: Option<PathBuf>) -> State {
        let mut s = State { file, ..State::default() };
        let Some(text) = s.file.as_deref().and_then(|f| fs::read_to_string(f).ok()) else { return s };
        let mut lines = text.lines();
        if lines.next() != Some(STATE_HEADER) {
            return s;
        }
        for line in lines {
            let f: Vec<&str> = line.split('\t').collect();
            match f[..] {
                ["day", day, count] => {
                    if let (Ok(d), Ok(c)) = (day.parse(), count.parse()) {
                        (s.day, s.count) = (d, c);
                    }
                }
                [hash, len, mtime, how, at] => {
                    let how = match how {
                        "matched" => Settled::Matched,
                        "refused" => Settled::Refused,
                        _ => continue,
                    };
                    if let (Ok(len), Ok(mtime), Ok(at)) = (len.parse(), mtime.parse(), at.parse()) {
                        s.entries.insert(hash.to_owned(), Entry { len, mtime, how, at });
                    }
                }
                _ => {}
            }
        }
        s
    }

    fn today(&mut self) -> usize {
        let d = now_secs() / 86_400;
        if d != self.day {
            (self.day, self.count) = (d, 0);
        }
        self.count
    }

    fn settled(&self, image: &Path) -> Option<Entry> {
        let st = stamp(image)?;
        self.entries.get(&path_hash(image)).copied().filter(|e| {
            (e.len, e.mtime) == st
                && (e.how == Settled::Matched || now_secs().saturating_sub(e.at) < RETRY_REFUSED.as_secs())
        })
    }

    fn note(&mut self, image: &Path, how: Settled) {
        if let Some((len, mtime)) = stamp(image) {
            self.entries.insert(path_hash(image), Entry { len, mtime, how, at: now_secs() });
        }
    }

    fn save(&self) -> Result<(), String> {
        let Some(file) = self.file.as_deref() else { return Ok(()) };
        let mut text = format!("{STATE_HEADER}\nday\t{}\t{}\n", self.day, self.count);
        let mut rows: Vec<_> = self.entries.iter().collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        for (h, e) in rows {
            let how = if e.how == Settled::Matched { "matched" } else { "refused" };
            text.push_str(&format!("{h}\t{}\t{}\t{how}\t{}\n", e.len, e.mtime, e.at));
        }
        file.parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| crate::tags_cli::write_atomic(file, text.as_bytes()))
            .map_err(|e| format!("{}: {e}", file.display()))
    }
}

/// Why a disc wasn't fingerprinted, or its fingerprinting failed.
#[derive(Debug)]
pub enum Skip {
    /// Fingerprinted before; said why.
    Settled(String),
    /// The day's budget is spent.
    Budget(usize),
    /// Stopped by a signal: nothing kept of this disc.
    Stopped,
    /// The service said to stay away.
    Stop(String),
    /// No answer from the service.
    Network(String),
    /// This disc: no stereo area, or `fpcalc` failed.
    Local(String),
}

/// A run's AcoustID side: the client, `fpcalc`, the budget and the state.
pub struct Session {
    config: Config,
    transport: Box<dyn Transport + Send>,
    last: Option<Instant>,
    cache: HashMap<String, Vec<(String, Vec<String>)>>,
    state: State,
    stop: Arc<AtomicBool>,
    /// What the run did.
    pub counters: Counters,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key.
        f.debug_struct("Session").field("counters", &self.counters).finish_non_exhaustive()
    }
}

impl Session {
    /// A session for the tags directory `tags` (where its state is kept), stopped when `stop`
    /// is set.
    pub fn new(
        config: Config,
        transport: Box<dyn Transport + Send>,
        tags: &Path,
        stop: Arc<AtomicBool>,
    ) -> Session {
        Session {
            config,
            transport,
            last: None,
            cache: HashMap::new(),
            state: State::load(Some(tags.join(STATE_FILE))),
            stop,
            counters: Counters::default(),
        }
    }

    /// Whether the run was told to stop.
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Notes what became of `image`, fingerprinted.
    pub fn settle(&mut self, image: &Path, how: Settled) -> Result<(), String> {
        self.state.note(image, how);
        self.state.save()
    }

    /// Whether `image` may be fingerprinted now: not settled, and within the day's budget.
    /// Counts it against the budget if so.
    pub fn begin(&mut self, image: &Path) -> Result<(), Skip> {
        self.counters.queued += 1;
        if let Some(e) = self.state.settled(image) {
            self.counters.skipped += 1;
            let days = now_secs().saturating_sub(e.at) / 86_400;
            return Err(Skip::Settled(match e.how {
                Settled::Matched => format!("fingerprinted and matched {days} days ago"),
                Settled::Refused => format!(
                    "fingerprinted {days} days ago without a match; asked again after {} days",
                    RETRY_REFUSED.as_secs() / 86_400
                ),
            }));
        }
        if self.state.today() >= self.config.budget {
            self.counters.deferred += 1;
            return Err(Skip::Budget(self.config.budget));
        }
        self.state.count += 1;
        self.state.save().map_err(Skip::Local)?;
        self.counters.done += 1;
        Ok(())
    }

    /// Gives back the budget of a disc [`begin`](Self::begin) counted but that was not fingerprinted
    /// to the end: stopped, no answer, or `fpcalc` failed.
    pub fn abandon(&mut self) {
        self.state.count = self.state.count.saturating_sub(1);
        self.counters.done = self.counters.done.saturating_sub(1);
        if let Err(e) = self.state.save() {
            log::warn!("acoustid: {e}");
        }
    }

    /// Waits while the load average is over the limit; whether stopped meanwhile.
    fn wait_for_load(&self, say: &mut dyn FnMut(String)) -> bool {
        let Some(max) = self.config.max_load else { return self.stopped() };
        let mut said = false;
        loop {
            if self.stopped() {
                return true;
            }
            let load = crate::platform::load_average().unwrap_or(0.0);
            if load <= max {
                return false;
            }
            if !said {
                say(format!("load average {load:.1} is over {max:.1}: waiting"));
                said = true;
            }
            let until = Instant::now() + LOAD_POLL;
            while Instant::now() < until {
                if self.stopped() {
                    return true;
                }
                thread::sleep(Duration::from_millis(200));
            }
        }
    }

    /// Fingerprints the tracks of area `area` of `disc` (its stereo area) and looks them up.
    pub fn identify(
        &mut self,
        disc: &Disc,
        area: usize,
        say: &mut dyn FnMut(String),
    ) -> Result<Prints, Skip> {
        let tracks: Vec<(u32, u32)> = disc
            .toc()
            .areas
            .get(area)
            .map(|a| a.tracks.iter().map(|t| (t.start_frame, t.duration_frames)).collect())
            .unwrap_or_default();
        let mut recordings = Vec::with_capacity(tracks.len());
        let mut by_release: BTreeMap<String, usize> = BTreeMap::new();
        let (mut audio, mut answered) = (0u64, 0usize);
        let mut short = 0;
        for (start, duration) in tracks {
            if duration < self.config.min_seconds.saturating_mul(75) {
                short += 1;
                recordings.push(BTreeSet::new());
                continue;
            }
            if self.wait_for_load(say) {
                return Err(Skip::Stopped);
            }
            let stop = self.stop.clone();
            let stopped = move || stop.load(Ordering::Relaxed);
            let r = fingerprint::fpcalc(&self.config.fpcalc, FPCALC_TIMEOUT, |out| {
                fingerprint::stream_track(disc, area, start, duration, out, &stopped)
            });
            let (fp, fed): (String, Fed) = match r {
                Ok(x) => x,
                Err(_) if self.stopped() => return Err(Skip::Stopped),
                Err(e) => return Err(Skip::Local(e)),
            };
            self.counters.tracks += 1;
            audio += u64::from(fed.frames);
            let seconds = (u64::from(duration) + 37) / 75;
            let found = self.lookup(&fp, seconds.max(1)).map_err(|e| match e {
                Error::Failed(e) => Skip::Network(format!("AcoustID: {e}")),
                Error::Stop(e) => Skip::Stop(format!("AcoustID: {e}")),
            })?;
            let mut recs = BTreeSet::new();
            let mut rels = BTreeSet::new();
            for (rec, releases) in found {
                recs.insert(rec);
                rels.extend(releases);
            }
            answered += usize::from(!recs.is_empty());
            for r in rels {
                *by_release.entry(r).or_default() += 1;
            }
            recordings.push(recs);
        }
        let mut releases: Vec<(String, usize)> = by_release.into_iter().collect();
        releases.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let need = required_hits(answered);
        let releases: Vec<Hit> = releases
            .into_iter()
            .filter(|(_, n)| *n >= need)
            .take(MAX_RELEASES)
            .map(|(id, _)| Hit { id, track_counts: Vec::new() })
            .collect();
        let skipped = if short > 0 {
            format!(", {short} under {} s left out", self.config.min_seconds)
        } else {
            String::new()
        };
        say(format!(
            "fingerprinted {} tracks ({} s of audio{skipped}): {answered} known to AcoustID, {} releases to read",
            recordings.len() - short,
            audio / 75,
            releases.len()
        ));
        Ok(Prints { recordings, releases })
    }

    fn pace(&mut self) {
        if let Some(wait) = self.last.and_then(|l| self.config.interval.checked_sub(l.elapsed())) {
            thread::sleep(wait);
        }
        self.counters.requests += 1;
    }

    /// The recordings AcoustID gives for a fingerprint, with their releases' ids.
    fn lookup(&mut self, fp: &str, seconds: u64) -> Result<Vec<(String, Vec<String>)>, Error> {
        let cache_key = format!("{seconds}:{fp}");
        if let Some(v) = self.cache.get(&cache_key) {
            return Ok(v.clone());
        }
        let url = format!(
            "{API}?client={}&meta={META}&duration={seconds}&fingerprint={}&format=json",
            encode(&self.config.key),
            encode(fp)
        );
        let hide = |s: String, key: &str| if key.is_empty() { s } else { s.replace(key, "<key>") };
        let mut problem = String::new();
        for attempt in 0..ATTEMPTS {
            self.pace();
            let answer = self.transport.get(&url);
            self.last = Some(Instant::now());
            let mut wait = self.config.backoff.saturating_mul(1 << attempt);
            match answer {
                Ok(r) if r.status == 200 => {
                    let v: Value = serde_json::from_slice(&r.body)
                        .map_err(|e| Error::Failed(format!("the response is not JSON: {e}")))?;
                    if v["status"].as_str() != Some("ok") {
                        return Err(service_error(&v, r.status));
                    }
                    let found = parse_lookup(&v);
                    self.cache.insert(cache_key, found.clone());
                    return Ok(found);
                }
                Ok(r) if r.status == 429 || r.status >= 500 => {
                    problem = format!("HTTP {}", r.status);
                    wait = mb::wait_after(r.status, r.retry_after, attempt, self.config.backoff).ok_or_else(
                        || Error::Stop(format!("{problem}: the service asks to wait; run this again later")),
                    )?;
                }
                Ok(r) => {
                    let v: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
                    return Err(service_error(&v, r.status));
                }
                Err(e) => problem = hide(e, &self.config.key),
            }
            if attempt + 1 < ATTEMPTS {
                thread::sleep(wait);
            }
        }
        Err(Error::Failed(format!("{problem}, after {ATTEMPTS} tries")))
    }
}

/// An error answer: a refused key or client stops the run; anything else fails this disc.
fn service_error(v: &Value, status: u16) -> Error {
    let code = v["error"]["code"].as_i64();
    let message: String =
        v["error"]["message"].as_str().unwrap_or("").chars().filter(|c| !c.is_control()).take(200).collect();
    match code {
        // 4: invalid API key; 5: (in old versions) invalid user key; 6: client not allowed.
        Some(4..=6) => Error::Stop(format!(
            "the service refuses the key ({message}): it must be an application key, from acoustid.org's \"New application\""
        )),
        _ => Error::Failed(format!("HTTP {status}, error {}: {message}", code.unwrap_or(-1))),
    }
}

/// The recordings of an answer, each with its releases' ids, from results with a score of at least
/// [`MIN_SCORE`]. Malformed ids are left out.
pub fn parse_lookup(v: &Value) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for r in v["results"].as_array().map_or(&[][..], Vec::as_slice) {
        if r["score"].as_f64().is_none_or(|s| s < MIN_SCORE) {
            continue;
        }
        for rec in r["recordings"].as_array().map_or(&[][..], Vec::as_slice) {
            let Some(id) = rec["id"].as_str().filter(|s| is_mbid(s)) else { continue };
            let releases: Vec<String> = rec["releases"]
                .as_array()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .filter_map(|r| r["id"].as_str().filter(|s| is_mbid(s)).map(str::to_owned))
                .take(MAX_RELEASES_PER_RECORDING)
                .collect();
            if let Some(k) = out.iter().position(|(i, _)| i == id) {
                let rels = &mut out[k].1;
                for r in releases {
                    if !rels.contains(&r) && rels.len() < MAX_RELEASES_PER_RECORDING {
                        rels.push(r);
                    }
                }
            } else if out.len() < MAX_RECORDINGS {
                out.push((id.to_owned(), releases));
            }
        }
    }
    out
}

/// Tracks of a release that must be the fingerprinted recordings, in order, out of `answered`
/// tracks AcoustID knows: more than half, and at least one.
pub fn required_hits(answered: usize) -> usize {
    (answered / 2 + 1).max(1)
}

/// CPU seconds of this process and the programs it ran (`fpcalc`) that ended.
pub fn cpu_seconds() -> f64 {
    crate::platform::cpu_seconds()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    const REC: &str = "00000000-0000-4000-8000-000000000001";
    const REL: &str = "00000000-0000-4000-8000-0000000000aa";

    #[test]
    fn an_answer_gives_recordings_and_releases_of_good_results() {
        let v = json!({"status": "ok", "results": [
            {"id": "x", "score": 0.9, "recordings": [
                {"id": REC, "releases": [{"id": REL}, {"id": "bad"}]},
                {"id": "not-an-id"}
            ]},
            {"id": "y", "score": 0.2, "recordings": [{"id": "00000000-0000-4000-8000-000000000002"}]}
        ]});
        assert_eq!(parse_lookup(&v), vec![(REC.to_owned(), vec![REL.to_owned()])]);
        assert!(parse_lookup(&json!({"status": "ok"})).is_empty());
    }

    #[test]
    fn a_refused_key_stops_the_run() {
        let v = json!({"status": "error", "error": {"code": 4, "message": "invalid API key"}});
        assert!(matches!(service_error(&v, 400), Error::Stop(m) if m.contains("application key")));
        let v = json!({"status": "error", "error": {"code": 3, "message": "invalid fingerprint"}});
        assert!(matches!(service_error(&v, 400), Error::Failed(_)));
    }

    #[test]
    fn more_than_half_must_be_in_order() {
        assert_eq!(required_hits(0), 1);
        assert_eq!(required_hits(1), 1);
        assert_eq!(required_hits(4), 3);
        assert_eq!(required_hits(5), 3);
    }

    #[test]
    fn the_state_keeps_the_days_count_and_the_discs() {
        let d = tempfile::tempdir().unwrap();
        let image = d.path().join("a.iso");
        fs::write(&image, b"x").unwrap();
        let file = d.path().join(STATE_FILE);
        let mut s = State::load(Some(file.clone()));
        assert_eq!(s.today(), 0);
        s.count = 3;
        s.note(&image, Settled::Refused);
        s.save().unwrap();
        let mut s = State::load(Some(file));
        assert_eq!(s.today(), 3);
        assert_eq!(s.settled(&image).map(|e| e.how), Some(Settled::Refused));
        fs::write(&image, b"xy").unwrap();
        assert!(s.settled(&image).is_none(), "a changed image is fingerprinted again");
    }
}
