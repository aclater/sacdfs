// SPDX-License-Identifier: GPL-3.0-or-later

//! The MusicBrainz web service, as `sacdfs tags fetch` uses it (#185): the requests, at no more
//! than one per second, and its JSON read as hostile input. Whatever is malformed is left out;
//! nothing here panics. An id goes into a URL only after [`is_mbid`] has passed it.

use std::collections::HashMap;
use std::io::Read;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

/// The service. *Why fixed:* a mirror is not asked for, and this keeps what is sent, and to
/// whom, a single documented answer.
pub const API: &str = "https://musicbrainz.org/ws/2";
/// Time between two requests, retries included: MusicBrainz asks for one per second at most, and
/// a request's start is what it counts. As pysacd's client, 10 % over, for a clock's jitter.
pub const MIN_INTERVAL: Duration = Duration::from_millis(1100);
/// Largest response read: as pysacd's client (a big release is about 1 MB).
pub const MAX_RESPONSE: u64 = 8 << 20;
/// What a release is read with.
const RELEASE_INC: &str = "artist-credits+labels+recordings+release-groups+isrcs";
/// Tries of one request.
const ATTEMPTS: u32 = 5;
/// Wait before the second try; it doubles after that.
const BACKOFF: Duration = Duration::from_secs(2);
/// Longest wait a server's `Retry-After` is obeyed for.
const MAX_RETRY_AFTER: u64 = 60;
/// Most candidates read per search.
const SEARCH_LIMIT: usize = 25;

/// The User-Agent: the project and its URL, as MusicBrainz asks, and no personal address.
pub fn user_agent() -> String {
    format!("sacdfs/{} ( {} )", env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_REPOSITORY"))
}

/// A response.
#[derive(Clone, Debug)]
pub struct Reply {
    /// The HTTP status.
    pub status: u16,
    /// The body.
    pub body: Vec<u8>,
    /// `Retry-After`, in seconds, if given as such.
    pub retry_after: Option<u64>,
}

/// What carries a request. *Why a trait:* tests answer from recorded responses, so that CI never
/// touches the network.
pub trait Transport {
    /// GET `url`; an error is a failure to get any response.
    fn get(&mut self, url: &str) -> Result<Reply, String>;
}

/// The real thing: HTTPS only, no redirects, bounded in time and size.
#[derive(Debug)]
pub struct Http {
    agent: ureq::Agent,
}

impl Http {
    /// A new client. No connection is made until a request is.
    pub fn new() -> Http {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(user_agent())
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(60)))
            .build();
        Http { agent: config.into() }
    }
}

impl Default for Http {
    fn default() -> Self {
        Http::new()
    }
}

impl Transport for Http {
    fn get(&mut self, url: &str) -> Result<Reply, String> {
        let resp =
            self.agent.get(url).header("Accept", "application/json").call().map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let mut body = Vec::new();
        resp.into_body()
            .into_reader()
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut body)
            .map_err(|e| e.to_string())?;
        if body.len() as u64 > MAX_RESPONSE {
            return Err(format!("a response larger than {MAX_RESPONSE} bytes"));
        }
        Ok(Reply { status, body, retry_after })
    }
}

/// Why a request gave nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The service answered that there is no such thing.
    NotFound,
    /// No usable answer, after the retries.
    Failed(String),
}

/// A client of the service, paced.
#[derive(Debug)]
pub struct Client<T> {
    transport: T,
    interval: Duration,
    backoff: Duration,
    last: Option<Instant>,
    requests: usize,
    cache: HashMap<String, Value>,
}

impl<T: Transport> Client<T> {
    /// A client over `transport`, at the service's rate.
    pub fn new(transport: T) -> Client<T> {
        Client::paced(transport, MIN_INTERVAL, BACKOFF)
    }

    /// A client with other waits, for tests.
    pub fn paced(transport: T, interval: Duration, backoff: Duration) -> Client<T> {
        Client { transport, interval, backoff, last: None, requests: 0, cache: HashMap::new() }
    }

    /// The requests made so far, retries included.
    pub fn requests(&self) -> usize {
        self.requests
    }

    /// Forgets the answers kept for the current disc: they are only for its two areas.
    pub fn next_disc(&mut self) {
        self.cache.clear();
    }

    /// The transport, for tests.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    fn pace(&mut self) {
        if let Some(wait) = self.last.and_then(|l| self.interval.checked_sub(l.elapsed())) {
            thread::sleep(wait);
        }
        self.last = Some(Instant::now());
        self.requests += 1;
    }

    fn json(&mut self, url: &str) -> Result<Value, Error> {
        if let Some(v) = self.cache.get(url) {
            return Ok(v.clone());
        }
        let mut problem = String::new();
        for attempt in 0..ATTEMPTS {
            self.pace();
            let mut wait = self.backoff.saturating_mul(1 << attempt);
            match self.transport.get(url) {
                Ok(r) if r.status == 404 => return Err(Error::NotFound),
                Ok(r) if r.status == 200 => {
                    let v: Value = serde_json::from_slice(&r.body)
                        .map_err(|e| Error::Failed(format!("the response is not JSON: {e}")))?;
                    if !v.is_object() {
                        return Err(Error::Failed("the response is not a JSON object".to_owned()));
                    }
                    self.cache.insert(url.to_owned(), v.clone());
                    return Ok(v);
                }
                // Busy, or over the rate: as the service asks, back off.
                Ok(r) if r.status == 429 || r.status >= 500 => {
                    problem = format!("HTTP {}", r.status);
                    if let Some(s) = r.retry_after {
                        wait = wait.max(Duration::from_secs(s.min(MAX_RETRY_AFTER)));
                    }
                }
                Ok(r) => return Err(Error::Failed(format!("HTTP {}", r.status))),
                Err(e) => problem = e,
            }
            if attempt + 1 < ATTEMPTS {
                thread::sleep(wait);
            }
        }
        Err(Error::Failed(format!("{problem}, after {ATTEMPTS} tries")))
    }

    /// The releases with this barcode, as the search has them.
    pub fn by_barcode(&mut self, barcode: &str) -> Result<Vec<Hit>, Error> {
        self.search(&format!("barcode:{}", lucene_quote(barcode)))
    }

    /// The releases with this catalogue number.
    pub fn by_catalogue(&mut self, catno: &str) -> Result<Vec<Hit>, Error> {
        self.search(&format!("catno:{}", lucene_quote(catno)))
    }

    /// The releases with this title and artist.
    pub fn by_name(&mut self, album: &str, artist: &str) -> Result<Vec<Hit>, Error> {
        self.search(&format!("release:{} AND artist:{}", lucene_quote(album), lucene_quote(artist)))
    }

    fn search(&mut self, query: &str) -> Result<Vec<Hit>, Error> {
        let url = format!("{API}/release?query={}&limit={SEARCH_LIMIT}&fmt=json", encode(query));
        match self.json(&url) {
            Ok(v) => Ok(release_hits(&v)),
            Err(Error::NotFound) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// The releases the service's fuzzy disc lookup gives for a CD-style `toc`
    /// (`first+last+lead-out+offsets...`). *Why `media-format=all`:* by default it leaves out
    /// formats other than CD, SACD among them.
    pub fn by_toc(&mut self, toc: &str) -> Result<Vec<Hit>, Error> {
        let url = format!("{API}/discid/-?toc={toc}&media-format=all&cdstubs=no&fmt=json");
        match self.json(&url) {
            Ok(v) => Ok(release_hits(&v)),
            Err(Error::NotFound) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// The release `id`, which must have passed [`is_mbid`].
    pub fn release(&mut self, id: &str) -> Result<Release, Error> {
        if !is_mbid(id) {
            return Err(Error::Failed(format!("{id:?} is no release id")));
        }
        let url = format!("{API}/release/{id}?inc={RELEASE_INC}&fmt=json");
        let v = self.json(&url)?;
        parse_release(&v).ok_or_else(|| Error::Failed(format!("release {id}: malformed response")))
    }
}

/// `s` as a quoted Lucene term.
fn lucene_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `s` percent-encoded for a URL's query, byte by byte: all but RFC 3986's unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Whether `s` is a MusicBrainz id: a lower-case UUID.
pub fn is_mbid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

/// A release, as far as a lookup uses it: only well-formed fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Release {
    /// Its id, which passed [`is_mbid`].
    pub id: String,
    /// Its title.
    pub title: Option<String>,
    /// Its artist credit.
    pub credit: Option<String>,
    /// Its date: `YYYY`, `YYYY-MM` or `YYYY-MM-DD`.
    pub date: Option<String>,
    /// Its barcode.
    pub barcode: Option<String>,
    /// Its catalogue numbers.
    pub catalogue: Vec<String>,
    /// Its release group's id.
    pub group: Option<String>,
    /// Its media.
    pub media: Vec<Medium>,
}

/// One medium of a release. MusicBrainz may list each layer of a hybrid disc as its own.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Medium {
    /// Its format: `Hybrid SACD`, `Hybrid SACD (SACD layer, multichannel)`, `CD` ...
    pub format: String,
    /// Its tracks.
    pub tracks: Vec<MbTrack>,
}

/// One track of a medium.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MbTrack {
    /// Its title.
    pub title: Option<String>,
    /// Its length in milliseconds, the recording's if the track has none.
    pub length_ms: Option<u64>,
    /// Its artist credit.
    pub credit: Option<String>,
    /// Its recording's id.
    pub recording: Option<String>,
    /// Its recording's ISRCs.
    pub isrcs: Vec<String>,
}

/// Longest text taken from a response, in bytes; a longer one is no real name, and it ends up in
/// a tag of every file.
const MAX_TEXT: usize = 1024;

/// `v` as text fit for a tag: trimmed, not empty, no control characters, not too long, and no
/// MusicBrainz placeholder.
fn text(v: &Value) -> Option<String> {
    const PLACEHOLDERS: [&str; 3] = ["[unknown]", "[no artist]", "[untitled]"];
    let s = v.as_str()?.trim();
    (!s.is_empty()
        && s.len() <= MAX_TEXT
        && !s.chars().any(char::is_control)
        && !PLACEHOLDERS.contains(&s.to_lowercase().as_str()))
    .then(|| s.to_owned())
}

fn mbid(v: &Value) -> Option<String> {
    v.as_str().filter(|s| is_mbid(s)).map(str::to_owned)
}

/// An artist credit's text: each name and its join phrase. `None` if any name is unusable, since
/// half of a credit is a wrong one.
fn credit(v: &Value) -> Option<String> {
    let parts = v.as_array().filter(|p| !p.is_empty())?;
    let mut out = String::new();
    for c in parts {
        out.push_str(&text(&c["name"])?);
        if let Some(j) = c["joinphrase"].as_str().filter(|j| !j.chars().any(char::is_control)) {
            out.push_str(j);
        }
    }
    let out = out.trim();
    (!out.is_empty() && out.len() <= MAX_TEXT).then(|| out.to_owned())
}

fn date(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let ok = match b.len() {
        4 => digits(0..4),
        7 => digits(0..4) && b[4] == b'-' && digits(5..7),
        10 => digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10),
        _ => false,
    };
    ok.then(|| s.to_owned())
}

/// A release a search gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Its id, which passed [`is_mbid`].
    pub id: String,
    /// The track counts of its media, if the search gave them.
    pub track_counts: Vec<u64>,
}

impl Hit {
    /// Whether a medium of it may have `n` tracks. *Why ask:* each release read is a request at
    /// one per second, and a popular album has dozens of releases.
    pub fn may_have(&self, n: usize) -> bool {
        self.track_counts.is_empty() || self.track_counts.contains(&(n as u64))
    }
}

/// The releases in a search's or a disc lookup's `releases`, in order, without repeats.
pub fn release_hits(v: &Value) -> Vec<Hit> {
    let mut out: Vec<Hit> = Vec::new();
    for r in v["releases"].as_array().map_or(&[][..], Vec::as_slice) {
        if let Some(id) = mbid(&r["id"]).filter(|id| !out.iter().any(|h| h.id == *id)) {
            let track_counts = r["media"]
                .as_array()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .filter_map(|m| m["track-count"].as_u64())
                .collect();
            out.push(Hit { id, track_counts });
        }
        if out.len() == SEARCH_LIMIT {
            break;
        }
    }
    out
}

/// A release from its JSON; `None` without a good id.
pub fn parse_release(v: &Value) -> Option<Release> {
    let mut rel = Release {
        id: mbid(&v["id"])?,
        title: text(&v["title"]),
        credit: credit(&v["artist-credit"]),
        date: date(&v["date"]),
        barcode: text(&v["barcode"]),
        group: mbid(&v["release-group"]["id"]),
        ..Release::default()
    };
    for li in v["label-info"].as_array().map_or(&[][..], Vec::as_slice) {
        rel.catalogue.extend(text(&li["catalog-number"]));
    }
    for m in v["media"].as_array().map_or(&[][..], Vec::as_slice) {
        let mut medium = Medium { format: text(&m["format"]).unwrap_or_default(), tracks: Vec::new() };
        for t in m["tracks"].as_array().map_or(&[][..], Vec::as_slice) {
            let r = &t["recording"];
            let length = |v: &Value| v.as_u64().filter(|&l| l > 0 && l < 1 << 40);
            medium.tracks.push(MbTrack {
                title: text(&t["title"]),
                length_ms: length(&t["length"]).or_else(|| length(&r["length"])),
                credit: credit(&t["artist-credit"]),
                recording: mbid(&r["id"]),
                isrcs: r["isrcs"].as_array().map_or(&[][..], Vec::as_slice).iter().filter_map(text).collect(),
            });
        }
        rel.media.push(medium);
    }
    Some(rel)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::collections::VecDeque;

    use super::*;

    const ID: &str = "cc39cc1a-b2e4-4bb9-b094-718a4d8253e6";

    #[derive(Debug, Default)]
    struct Script {
        replies: VecDeque<Result<Reply, String>>,
        urls: Vec<String>,
        at: Vec<Instant>,
    }

    impl Transport for Script {
        fn get(&mut self, url: &str) -> Result<Reply, String> {
            self.urls.push(url.to_owned());
            self.at.push(Instant::now());
            self.replies.pop_front().unwrap()
        }
    }

    fn reply(status: u16, body: &str) -> Result<Reply, String> {
        Ok(Reply { status, body: body.as_bytes().to_vec(), retry_after: None })
    }

    fn client(replies: Vec<Result<Reply, String>>, interval_ms: u64) -> Client<Script> {
        let s = Script { replies: replies.into(), ..Script::default() };
        Client::paced(s, Duration::from_millis(interval_ms), Duration::from_millis(1))
    }

    #[test]
    fn the_user_agent_names_the_project_and_no_person() {
        let ua = user_agent();
        assert!(ua.starts_with("sacdfs/"), "{ua}");
        assert!(ua.contains("https://github.com/aclater/sacdfs"), "{ua}");
        assert!(!ua.contains('@'), "{ua}");
    }

    #[test]
    fn the_rate_is_at_most_one_request_per_second() {
        assert!(MIN_INTERVAL >= Duration::from_secs(1));
        let mut c = client(vec![reply(200, "{}"), reply(503, ""), reply(200, "{}"), reply(200, "{}")], 40);
        c.by_barcode("1").unwrap();
        c.by_barcode("2").unwrap();
        c.by_barcode("3").unwrap();
        // A retry is a request like any other.
        let at = &c.transport().at;
        assert_eq!(at.len(), 4);
        for w in at.windows(2) {
            assert!(w[1] - w[0] >= Duration::from_millis(40), "{:?}", w[1] - w[0]);
        }
    }

    #[test]
    fn retries_are_bounded_and_answers_kept_for_one_disc() {
        let mut c = client((0..ATTEMPTS).map(|_| reply(503, "busy")).collect(), 0);
        assert_eq!(c.by_barcode("1"), Err(Error::Failed("HTTP 503, after 5 tries".into())));
        assert_eq!(c.requests(), ATTEMPTS as usize);

        let mut c = client(vec![Err("reset".into()), reply(200, "{\"releases\":[]}")], 0);
        assert_eq!(c.by_barcode("1"), Ok(vec![]));
        assert_eq!(c.by_barcode("1"), Ok(vec![]), "from what was kept");
        assert_eq!(c.requests(), 2);

        // Not retried: a request the service refuses.
        let mut c = client(vec![reply(400, "{}")], 0);
        assert_eq!(c.by_barcode("1"), Err(Error::Failed("HTTP 400".into())));
        let mut c = client(vec![reply(200, "<html>")], 0);
        assert!(matches!(c.by_barcode("1"), Err(Error::Failed(_))));
        let mut c = client(vec![reply(200, "[1]")], 0);
        assert!(matches!(c.by_barcode("1"), Err(Error::Failed(_))));
        let mut c = client(vec![reply(404, "")], 0);
        assert_eq!(c.release(ID), Err(Error::NotFound));
    }

    #[test]
    fn what_is_asked_is_encoded() {
        let mut c = client(vec![reply(200, "{}")], 0);
        c.by_name("A \"B\" & C/D", "É\\").unwrap();
        let url = &c.transport().urls[0];
        assert!(url.starts_with("https://musicbrainz.org/ws/2/release?query="), "{url}");
        assert!(url.contains("release%3A%22A%20%5C%22B%5C%22%20%26%20C%2FD%22"), "{url}");
        assert!(url.contains("artist%3A%22%C3%89%5C%5C%22"), "{url}");
        assert!(url.ends_with("&limit=25&fmt=json"), "{url}");
        // An id that is not one never reaches a URL.
        let mut c = client(vec![], 0);
        assert!(c.release("../../etc/passwd").is_err());
        assert_eq!(c.requests(), 0);
    }

    #[test]
    fn hostile_json_gives_nothing_unsafe() {
        let id = format!("\"id\":\"{ID}\"");
        assert_eq!(parse_release(&serde_json::json!({"id": "nope"})), None);
        let v: Value = serde_json::from_str(&format!(
            r#"{{{id},"title":"a\nb","date":"20x1","barcode":5,"artist-credit":[{{"name":5}}],
                "release-group":{{"id":"x"}},"label-info":[null,{{"catalog-number":"C-1"}}],
                "media":[null,{{"tracks":[null,{{"title":"[unknown]","length":-5,
                "recording":{{"id":"../x","length":"9","isrcs":[1,"ISRC"]}}}}]}}]}}"#
        ))
        .unwrap();
        let r = parse_release(&v).unwrap();
        assert_eq!(r.title, None, "a control character");
        assert_eq!(r.date, None);
        assert_eq!(r.barcode, None);
        assert_eq!(r.credit, None);
        assert_eq!(r.group, None);
        assert_eq!(r.catalogue, ["C-1"]);
        assert_eq!(r.media.len(), 2);
        let t = &r.media[1].tracks[1];
        assert_eq!(t.title, None, "a placeholder");
        assert_eq!(t.length_ms, None);
        assert_eq!(t.recording, None);
        assert_eq!(t.isrcs, ["ISRC"]);
        assert_eq!(text(&Value::String("x".repeat(MAX_TEXT + 1))), None);
        let hits = release_hits(&serde_json::json!({"releases": [
            {"id": ID, "media": [{"track-count": 3}, null, {"track-count": "x"}]},
            {"id": ID}, 5, {"id": "x"}]}));
        assert_eq!(hits, [Hit { id: ID.to_owned(), track_counts: vec![3] }]);
        assert!(hits[0].may_have(3) && !hits[0].may_have(4));
        assert!(Hit { id: String::new(), track_counts: vec![] }.may_have(4), "not said: read it");
        for d in ["2001", "2001-02", "2001-02-03"] {
            assert_eq!(date(&Value::String(d.into())).as_deref(), Some(d));
        }
        for d in ["", "01", "2001-2", "2001-02-3", "é001"] {
            assert_eq!(date(&Value::String(d.into())), None);
        }
    }
}
