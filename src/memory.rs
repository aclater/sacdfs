// SPDX-License-Identifier: GPL-3.0-or-later

//! The memory limit sacdfs runs under, and the defaults it sets (#231, #237).
//!
//! An out-of-memory kill ends the service for every client, and the kernel acts before an
//! allocator can report a failure, so sacdfs has to stay under the limit by itself. It reads
//! the cgroup limit once at startup (a container's `--memory`, a unit's `MemoryMax=`). *With no
//! limit, nothing here changes anything:* every default is what it was, and listings are not
//! bounded.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::engine::Engine;
use crate::tree::Tree;

const MIB: u64 = 1 << 20;

/// Cache size without a limit, or with a roomy one, in MiB.
pub const CACHE_MB: usize = 32;

/// Cache size under a limit below [`SMALL_LIMIT`], in MiB. *Why not 8:* it slows WAV 2 to 2.6
/// times; 16 showed no cost to a single reader, and saves about 18 MiB.
pub const CACHE_MB_SMALL: usize = 16;

/// Limits below this are small, for the cache's default.
pub const SMALL_LIMIT: u64 = 512 * MIB;

/// Limit per decoder thread: each costs about 3.7 MiB with its frames in flight, and on a host
/// with many CPUs and a small limit they add up. One thread alone gives about 5 times real time
/// on 6-channel WAV.
const LIMIT_PER_THREAD: u64 = 64 * MIB;

/// The share of the limit kept for what listed images hold, in eighths. *Why three:* at 256 MiB,
/// 96 MiB keeps about 1,800 images listed, close to the size that already fitted there, whose second
/// pass over the library would otherwise read every TOC again (4.5 times slower with a quarter, at
/// 1,600 images). What it leaves is for the rest of the process and for Samba and NFS when they share the
/// limit; if they take more, the memory watch shrinks listings.
const LISTING_EIGHTHS: u64 = 3;

/// The least budget for listings, however small the limit.
const LISTING_MIN: u64 = 8 * MIB;

/// Process base: what sacdfs holds with nothing listed, in bytes.
const BASE: u64 = 20 * MIB;

/// A decoder thread with its frames in flight.
const THREAD: u64 = 3_880_000;

/// An image once listed in full, DSF and WAV files alike, when nothing is dropped (#237): the
/// measured slope is 53 to 57 KiB.
const IMAGE_LISTED: u64 = 56 << 10;

/// What an image costs at least, with everything of it that can be dropped gone: the names
/// walked, its directory's node and entry.
const IMAGE_FLOOR: u64 = 7 << 9;

/// The cgroup whose limit this process runs under.
#[derive(Clone, Debug)]
pub struct Cgroup {
    limit: u64,
    dir: PathBuf,
}

impl Cgroup {
    /// The memory limit of this process's cgroup, if any: the smallest along its path.
    pub fn find() -> Option<Cgroup> {
        let own = fs::read_to_string("/proc/self/cgroup").ok()?;
        limit_in(Path::new("/sys/fs/cgroup"), &own)
    }

    /// A cgroup with `limit` bytes, whose `memory.stat` is in `dir`. For a limit [`Cgroup::find`]
    /// doesn't see, and for tests.
    pub fn at(dir: PathBuf, limit: u64) -> Cgroup {
        Cgroup { limit, dir }
    }

    /// The limit, in bytes.
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// The bytes in the cgroup that can't be reclaimed without killing something: its anonymous
    /// and shared memory. *Why not `memory.current`:* that counts the page cache, which the
    /// kernel gives up by itself, and is at the limit on any machine that has been reading.
    pub fn used(&self) -> Option<u64> {
        used_in(&fs::read_to_string(self.dir.join("memory.stat")).ok()?)
    }
}

/// [`Cgroup::used`] from a `memory.stat` of cgroup v2 (`anon`) or v1 (`total_rss`).
fn used_in(stat: &str) -> Option<u64> {
    let field = |name: &str| {
        stat.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.trim().parse::<u64>().ok())
    };
    let anon = field("anon").or_else(|| field("total_rss"))?;
    Some(anon + field("shmem").or_else(|| field("total_shmem")).unwrap_or(0))
}

/// How short of memory the cgroup is, least first (#237, item 3). Each level does what the ones
/// before it do. *Why in this order:* what a listing holds costs a disc read to get back and
/// slows only whoever comes back to it; the cache and decoders cost readers some decoding;
/// refusing fails a request, and an out-of-memory kill would fail every client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    /// Room enough.
    None,
    /// Above 75% of the limit: listings kept to a quarter of their budget.
    Listings,
    /// Above 85%: the cache of frames and the decoders not in use given up too.
    Caches,
    /// Above 93%: nothing new is listed or opened; those requests fail with `ENOMEM`. Files open
    /// already go on being read.
    Refuse,
}

impl Pressure {
    /// The level at `used` bytes of `limit`.
    pub fn of(used: u64, limit: u64) -> Pressure {
        match used.saturating_mul(100) / limit.max(1) {
            93.. => Pressure::Refuse,
            85.. => Pressure::Caches,
            75.. => Pressure::Listings,
            _ => Pressure::None,
        }
    }
}

/// Gives up memory as the cgroup nears its limit (#237, item 3): the kernel's out-of-memory killer
/// acts before an allocation can fail, so sacdfs has to see it coming. Looks four times a second
/// above half the limit and once a second below it, since a listing takes tens of MiB a second:
/// a read of a file in memory that wakes no disk. Returns if the cgroup can't be read any more,
/// with nothing left refused.
pub fn watch(cgroup: &Cgroup, tree: &Tree, engine: &Engine) {
    const WARN_AGAIN: Duration = Duration::from_secs(60);
    const TRIM_AGAIN: Duration = Duration::from_secs(2);
    let limit = cgroup.limit();
    let mut was = Pressure::None;
    let mut warned = None;
    let mut noted = None;
    let mut trimmed = None;
    loop {
        let Some(used) = cgroup.used() else {
            log::debug!("memory use can't be read: not watching it");
            tree.set_pressure(Pressure::None);
            return;
        };
        let now = Pressure::of(used, limit);
        if now != was {
            let line = format!("memory: {} of {} MiB in use: {now:?} pressure", used >> 20, limit >> 20);
            if now == Pressure::Refuse && warned.is_none_or(|t: Instant| t.elapsed() > WARN_AGAIN) {
                log::warn!("{line}: new listings and opens fail with ENOMEM until it passes");
                warned = Some(Instant::now());
            } else if now > was && noted.is_none_or(|t: Instant| t.elapsed() > WARN_AGAIN) {
                // Use can hover at a level's edge: once a minute is enough to say so.
                log::info!("{line}");
                noted = Some(Instant::now());
            } else {
                log::debug!("{line}");
            }
            tree.set_pressure(now);
        } else if now != Pressure::None {
            tree.set_pressure(now);
        }
        if now >= Pressure::Caches {
            engine.release_if_idle(Duration::ZERO);
            // What listings let go of stays in the heap until it is trimmed, with no frames to
            // give back too. Not at every look: a trim holds up every thread that allocates.
            if trimmed.is_none_or(|t: Instant| t.elapsed() > TRIM_AGAIN) {
                crate::engine::trim_heap();
                trimmed = Some(Instant::now());
            }
        }
        was = now;
        let wait = if used > limit / 2 { Duration::from_millis(250) } else { Duration::from_secs(1) };
        std::thread::sleep(wait);
    }
}

/// [`Cgroup::find`] under the cgroup mount `root`, for the content `own` of `/proc/self/cgroup`.
fn limit_in(root: &Path, own: &str) -> Option<Cgroup> {
    let mut least: Option<Cgroup> = None;
    for line in own.lines() {
        let mut parts = line.splitn(3, ':');
        let (_, controllers, path) = (parts.next()?, parts.next()?, parts.next()?);
        let path = path.trim_start_matches('/');
        let (base, files): (_, &[&str]) = match controllers {
            "" => (root.to_owned(), &["memory.max", "memory.high"]),
            c if c.split(',').any(|c| c == "memory") => (root.join("memory"), &["memory.limit_in_bytes"]),
            _ => continue,
        };
        let mut at = base.join(path);
        loop {
            for f in files {
                let found = fs::read_to_string(at.join(f)).ok().and_then(|t| parse_limit(&t));
                if let Some(limit) = found.filter(|&l| least.as_ref().is_none_or(|c| l < c.limit)) {
                    least = Some(Cgroup { limit, dir: at.clone() });
                }
            }
            if at == base || !at.pop() {
                break;
            }
        }
    }
    least
}

/// A limit file's content: a number of bytes, or `max`, or under cgroup v1 a huge number.
fn parse_limit(text: &str) -> Option<u64> {
    text.trim().parse().ok().filter(|&n: &u64| n < 1 << 60)
}

/// The default size of the cache of frames under `limit`, in MiB.
pub fn default_cache_mb(limit: Option<u64>) -> usize {
    match limit {
        Some(l) if l < SMALL_LIMIT => CACHE_MB_SMALL,
        _ => CACHE_MB,
    }
}

/// The default number of decoder threads under `limit`, on a host with `cpus` CPUs: one per CPU,
/// as far as the limit has room for them.
pub fn default_threads(cpus: usize, limit: Option<u64>) -> usize {
    match limit {
        Some(l) => cpus.min(((l / LIMIT_PER_THREAD) as usize).max(2)),
        None => cpus,
    }
}

/// The default budget for what listed images hold, in bytes; `None` for no bound.
pub fn listing_budget(limit: Option<u64>) -> Option<usize> {
    limit.map(|l| (l / 8 * LISTING_EIGHTHS).max(LISTING_MIN) as usize)
}

/// What this configuration asks of the limit (#237, item 1).
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    /// The limit, in bytes.
    pub limit: u64,
    /// Images found by the walk.
    pub images: usize,
    /// The size of the cache of frames, in MiB.
    pub cache_mb: usize,
    /// Decoder threads.
    pub threads: usize,
    /// The budget for listings in bytes; `None` for no bound.
    pub budget: Option<usize>,
}

impl Plan {
    /// Bytes everything but listed images takes.
    fn fixed(&self) -> u64 {
        BASE + self.cache_mb as u64 * MIB + self.threads as u64 * THREAD
    }

    /// Bytes the process takes with the whole library listed and nothing dropped.
    pub fn listed_in_full(&self) -> u64 {
        self.fixed() + self.images as u64 * IMAGE_LISTED
    }

    /// Bytes the process takes at most with listings kept to the budget.
    pub fn bounded(&self) -> Option<u64> {
        let b = self.budget? as u64;
        Some(self.fixed() + b.min(self.images as u64 * IMAGE_LISTED) + self.images as u64 * IMAGE_FLOOR)
    }

    /// The warning for the log at startup, when the library doesn't fit as it is: what will
    /// happen, and what to change. `None` when it fits.
    pub fn warning(&self) -> Option<String> {
        let mb = |b: u64| b.div_ceil(MIB);
        let (limit, full, images) = (mb(self.limit), mb(self.listed_in_full()), self.images);
        if full <= limit {
            return None;
        }
        let Some(bounded) = self.bounded() else {
            return Some(format!(
                "memory limit {limit} MiB: listing all {images} images would take about {full} MiB, \
                 and listings are not bounded (--listing-mb 0): sacdfs can be killed for lack of \
                 memory; raise the limit or remove --listing-mb 0"
            ));
        };
        if bounded > self.limit {
            return Some(format!(
                "memory limit {limit} MiB: {images} images need about {} MiB even with what can be \
                 dropped gone: sacdfs can be killed for lack of memory; raise the limit, or lower \
                 --cache-mb or --threads",
                mb(bounded)
            ));
        }
        let keep = mb(self.budget.unwrap_or(0) as u64);
        Some(format!(
            "memory limit {limit} MiB: listing all {images} images in memory would take about {full} \
             MiB, so {keep} MiB of listings are kept and images not used lately are read again \
             when next used; going through the whole library again will be slower; to avoid that, \
             raise the limit to about {} MiB",
            full + full / 4
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V2: &str = "0::/a.slice/b.scope\n";

    fn cg(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let p = dir.path().join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        dir
    }

    #[test]
    fn limit_of_the_cgroup_itself() {
        let d = cg(&[("a.slice/b.scope/memory.max", "268435456\n")]);
        assert_eq!(limit_in(d.path(), V2).map(|c| c.limit), Some(256 * MIB));
    }

    #[test]
    fn no_limit() {
        let d = cg(&[("a.slice/b.scope/memory.max", "max\n"), ("a.slice/memory.max", "max\n")]);
        assert!(limit_in(d.path(), V2).is_none());
        assert!(limit_in(Path::new("/nonexistent"), V2).is_none());
    }

    #[test]
    fn the_smallest_limit_on_the_way_up_counts() {
        let d = cg(&[("a.slice/b.scope/memory.max", "max\n"), ("a.slice/memory.max", "1000\n")]);
        let c = limit_in(d.path(), V2).unwrap();
        assert_eq!((c.limit, c.dir), (1000, d.path().join("a.slice")));
        let d = cg(&[
            ("a.slice/b.scope/memory.max", "4000\n"),
            ("a.slice/b.scope/memory.high", "3000\n"),
            ("a.slice/memory.max", "5000\n"),
        ]);
        assert_eq!(limit_in(d.path(), V2).map(|c| c.limit), Some(3000));
    }

    #[test]
    fn inside_a_container_the_root_of_the_namespace_has_the_limit() {
        let d = cg(&[("memory.max", "1000\n")]);
        assert_eq!(limit_in(d.path(), "0::/\n").map(|c| c.limit), Some(1000));
    }

    #[test]
    fn cgroup_v1() {
        let own = "4:cpu,cpuacct:/x\n3:memory:/x\n";
        let d = cg(&[("memory/x/memory.limit_in_bytes", "2000\n")]);
        assert_eq!(limit_in(d.path(), own).map(|c| c.limit), Some(2000));
        // What v1 shows for no limit.
        let d = cg(&[("memory/x/memory.limit_in_bytes", "9223372036854771712\n")]);
        assert!(limit_in(d.path(), own).is_none());
    }

    #[test]
    fn used_is_what_cannot_be_reclaimed() {
        assert_eq!(used_in("anon 100\nfile 900\nshmem 20\n"), Some(120));
        assert_eq!(used_in("cache 900\ntotal_rss 100\ntotal_shmem 5\n"), Some(105));
        assert_eq!(used_in("anon_thp 100\n"), None);
    }

    #[test]
    fn pressure_rises_with_use() {
        let at = |pc| Pressure::of(pc, 100);
        assert_eq!(at(74), Pressure::None);
        assert_eq!(at(75), Pressure::Listings);
        assert_eq!(at(85), Pressure::Caches);
        assert_eq!(at(93), Pressure::Refuse);
        assert_eq!(at(150), Pressure::Refuse);
        assert!(Pressure::None < Pressure::Listings && Pressure::Caches < Pressure::Refuse);
    }

    #[test]
    fn no_limit_changes_no_default() {
        assert_eq!(default_cache_mb(None), 32);
        assert_eq!(default_threads(24, None), 24);
        assert_eq!(listing_budget(None), None);
    }

    #[test]
    fn defaults_follow_the_limit() {
        assert_eq!(default_cache_mb(Some(256 * MIB)), 16);
        assert_eq!(default_cache_mb(Some(512 * MIB)), 32);
        assert_eq!(default_threads(24, Some(256 * MIB)), 4);
        assert_eq!(default_threads(2, Some(256 * MIB)), 2);
        assert_eq!(default_threads(24, Some(64 * MIB)), 2);
        assert_eq!(default_threads(8, Some(8192 * MIB)), 8);
        assert_eq!(listing_budget(Some(256 * MIB)), Some(96 << 20));
        assert_eq!(listing_budget(Some(16 * MIB)), Some(8 << 20));
    }

    fn plan(limit_mb: u64, images: usize, budget: Option<usize>) -> Plan {
        Plan { limit: limit_mb * MIB, images, cache_mb: 16, threads: 4, budget }
    }

    #[test]
    fn a_library_that_fits_gets_no_warning() {
        assert!(plan(256, 1000, Some(96 << 20)).warning().is_none());
        assert!(plan(8192, 20_000, Some(2048 << 20)).warning().is_none());
    }

    #[test]
    fn over_the_budget_the_warning_says_what_changes() {
        let w = plan(256, 5000, Some(96 << 20)).warning().unwrap();
        assert!(w.contains("256 MiB") && w.contains("5000 images") && w.contains("raise the limit"), "{w}");
        assert!(w.contains("slower"), "{w}");
    }

    #[test]
    fn unbounded_listings_over_the_limit_can_be_killed() {
        let w = plan(256, 5000, None).warning().unwrap();
        assert!(w.contains("killed") && w.contains("--listing-mb 0"), "{w}");
    }

    #[test]
    fn a_library_too_big_for_the_limit_can_be_killed() {
        let w = plan(256, 200_000, Some(96 << 20)).warning().unwrap();
        assert!(w.contains("killed") && w.contains("raise the limit"), "{w}");
    }
}
