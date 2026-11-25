// SPDX-License-Identifier: GPL-3.0-or-later

//! What every front end shares between its command line and a [`crate::vfs::Volume`] (#419,
//! #420): the options that shape the files and their limits ([`Options`], [`Limits`], each a
//! `clap::Args` to flatten into a front end's own command), and the steps that turn them into
//! a tree and an engine, then start the background threads.
//!
//! The steps are separate functions, not one call, because a front end puts its own steps
//! between them: the Linux mount checks its mount point before the scan and becomes a daemon
//! after it, before any thread exists. Each front end calls them in this order:
//!
//! 1. [`Options::check`]: the tags folder and the PCM options, before anything is read;
//! 2. [`Limits::index_cache_dir`], then [`Options::scan`] and [`Options::sizing`];
//! 3. [`open_index_cache`];
//! 4. [`build`], which starts the engine's threads: the tree and the engine for a `Volume`;
//! 5. [`start_background`] once the front end serves, and [`Background::stop`] at the end.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::disc::Tuning;
use crate::engine::{Engine, default_threads};
use crate::index_cache::{self, IndexCache};
use crate::memory;
use crate::pcm::{Cutoff, Filter, PcmConfig, PcmRate};
use crate::sidecar::Sidecars;
use crate::tree::{Output, SourceDir, Tree};

/// The options that shape the files a front end serves.
#[derive(Debug, Clone, clap::Args)]
pub struct Options {
    /// DST decoder threads (0: one per CPU; under a memory limit, at most one per 64 MiB of it,
    /// and at least 2).
    #[arg(hide = true, long, default_value_t = 0, value_name = "N")]
    pub threads: usize,
    /// Size of the cache of decoded and converted frames, in MiB (default: 32, or 16 under a
    /// memory limit below 256 MiB).
    #[arg(hide = true, long, value_name = "N")]
    pub cache_mb: Option<usize>,
    /// Memory kept for what listed images hold (their TOCs and files), in MiB; past it, those
    /// not used lately are read again when next used. 0: no bound (default: no bound, or 3/8
    /// of the memory limit under one).
    #[arg(hide = true, long, value_name = "N")]
    pub listing_mb: Option<usize>,
    /// How fast players read a track they start, in times real time (#504). Where this machine
    /// decodes a format slower than that (measured as it serves), a starting track is decoded
    /// ahead without waiting for the player's reads. 0: off.
    #[arg(hide = true, long, default_value_t = 3.5, value_name = "X", value_parser = parse_burst)]
    pub start_burst: f64,
    /// Seconds of audio decoded ahead when a track starts playing, within a quarter of the cache.
    #[arg(hide = true, long, default_value_t = 8, value_name = "SECS",
          value_parser = clap::builder::RangedU64ValueParser::<u32>::new().range(1..=60))]
    pub start_burst_secs: u32,
    /// Which files to show: dsf, wav, or both (dsf,wav).
    #[arg(long, default_value = "dsf,wav", value_name = "LIST", value_parser = parse_formats)]
    pub format: Formats,
    /// Pauses: dropped (the default: each file is the track's own range) or kept (each file
    /// is the track with its pause, as extractors that keep pauses write it).
    #[arg(long, default_value = "dropped", value_name = "MODE", value_parser = ["dropped", "kept"])]
    pub pauses: String,
    /// Sample rate of the WAV files, in Hz: 88200, 176400 or 352800.
    #[arg(long, default_value_t = 176_400, value_name = "HZ", value_parser = parse_rate)]
    pub pcm_rate: u32,
    /// Low-pass cutoff of the PCM conversion: 40 or 30 (kHz), or wide (about 80 kHz; needs a
    /// --pcm-rate of 176400 or 352800).
    #[arg(hide = true, long, default_value = "40", value_name = "KHZ", value_parser = parse_cutoff)]
    pub pcm_cutoff: Cutoff,
    /// Decimation filter of the PCM conversion: precise (the default, except on 32-bit x86;
    /// about 20 dB lower distortion and noise, about 1.8 times the CPU time) or fast (the
    /// filter of sacdfs 0.8.0 and earlier; the default on 32-bit x86).
    #[arg(hide = true, long, default_value = Filter::DEFAULT_NAME, value_name = "FILTER", value_parser = parse_filter)]
    pub pcm_filter: Filter,
    /// Gain of the PCM conversion in dB, -60 to +24; samples that would clip are clamped.
    #[arg(hide = true, long, default_value_t = 0.0, value_name = "DB", allow_hyphen_values = true, value_parser = parse_gain)]
    pub pcm_gain: f32,
    /// Do not add " (Multichannel)" to the album tag of the files of the multichannel area
    /// (by default they get it, so players and servers show them as an album of their own).
    #[arg(long, hide = true)]
    pub no_multichannel_album_suffix: bool,
    /// Directory of tags files (#81), one per disc, named by its disc key
    /// (`sacdfs tags key IMAGE`): their tags override and add to the TOC's. Without it, or
    /// for a disc without a file, the files are as the TOC gives them.
    #[arg(long, value_name = "DIR")]
    pub tags: Option<PathBuf>,
    /// Look up, on MusicBrainz, discs without a track title, album or artist, in the
    /// background, and write what matches to the --tags directory (which must be writable),
    /// as `sacdfs tags fetch` does: by barcode or catalogue number, confirmed by track count,
    /// durations and ISRCs. Off by default; nothing else in a mount uses the network. At most
    /// 1 request per second, once per disc; no client request waits for it.
    ///
    /// Takes `=LOOKUPS`, `name`, `durations` or both (`name,durations`), to try more when that
    /// finds nothing: `name` also searches by album and artist, from the disc's text or the
    /// image's path (sends those names); `durations` also asks by the tracks' start times (a
    /// disc of 4 tracks or more; sends the times).
    #[arg(long, requires = "tags", value_name = "LOOKUPS", num_args = 0..=1, require_equals = true,
          default_missing_value = "", value_parser = parse_musicbrainz)]
    pub musicbrainz: Option<Musicbrainz>,
    /// Same as `--musicbrainz=name`.
    #[arg(long, hide = true, requires = "musicbrainz")]
    pub lookup_name: bool,
    /// Same as `--musicbrainz=durations`.
    #[arg(long, hide = true, requires = "musicbrainz")]
    pub lookup_durations: bool,
    /// Collapse each chain of folders below the top level that holds a single image into
    /// its outermost folder, which then holds the image's Stereo/Multichannel folders.
    #[arg(long)]
    pub flatten: bool,
    /// Files passed through from SOURCE, byte for byte, next to the images' folders: a
    /// comma-separated list of extensions and folder names ending in / (whose whole content
    /// passes). `default` is jpg,jpeg,png,webp,m3u,m3u8,Artwork/,Scans/; `-item` removes an
    /// item (default,-webp), `none` passes nothing.
    #[arg(long, default_value = "default", value_name = "LIST", value_parser = Sidecars::parse)]
    pub sidecars: Sidecars,
}

/// The persistent seek index, open files and request time limits. A struct of its own so that
/// a front end can place its own options before these in its help, as the Linux mount does.
#[derive(Debug, Clone, clap::Args)]
pub struct Limits {
    /// Directory of the persistent seek index, which lets seeks after a remount skip most
    /// of their search (default: $XDG_CACHE_HOME/sacdfs, else ~/.cache/sacdfs). `off`: keep
    /// none, so that every mount starts with an empty one.
    #[arg(long, value_name = "DIR|off", value_parser = parse_index_cache)]
    pub index_cache: Option<CacheChoice>,
    /// Same as `--index-cache off`.
    #[arg(long, hide = true, conflicts_with = "index_cache")]
    pub no_index_cache: bool,
    /// Most image files kept open at once (1 to 1024); the least recently used is closed
    /// first, and any closes after 5 minutes unused.
    #[arg(hide = true, long, default_value_t = crate::handles::DEFAULT_MAX_OPEN, value_name = "N",
          value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=1024))]
    pub max_open_images: usize,
    /// Seconds a request (a read, a lookup, a listing) may wait for the source before it
    /// fails with an I/O error and a log line, so that a stalled share never hangs a client;
    /// 0 waits for good.
    #[arg(hide = true, long, default_value_t = crate::watchdog::DEFAULT_TIMEOUT.as_secs(), value_name = "SECS")]
    pub request_timeout: u64,
}

/// The end of `--help`: where the options it leaves out are.
pub const TUNING_HELP: &str = "Tuning options (--threads, --cache-mb, --listing-mb, --start-burst, \
--start-burst-secs, --max-open-images, --request-timeout, --pcm-cutoff, --pcm-filter, --pcm-gain, \
--no-multichannel-album-suffix, and for sacdfs --stop-grace) are not listed here; their defaults \
size themselves. They are described in docs/options.md, under Tuning.";

/// What `--musicbrainz` looks up besides the barcode or catalogue number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Musicbrainz {
    /// Also search by album and artist.
    pub name: bool,
    /// Also ask by the tracks' start times.
    pub durations: bool,
}

/// `--musicbrainz`'s value: empty, `name`, `durations`, or both, comma-separated.
fn parse_musicbrainz(s: &str) -> Result<Musicbrainz, String> {
    let mut m = Musicbrainz::default();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "name" => m.name = true,
            "durations" => m.durations = true,
            other => return Err(format!("unknown lookup {other:?}: use name, durations or name,durations")),
        }
    }
    Ok(m)
}

/// `--index-cache`'s value: a folder, or `off`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CacheChoice {
    /// No persistent seek index.
    Off,
    /// The index lives in this folder.
    Dir(PathBuf),
}

fn parse_index_cache(s: &str) -> Result<CacheChoice, String> {
    match s {
        "" => Err("a folder, or off".to_owned()),
        "off" => Ok(CacheChoice::Off),
        d => Ok(CacheChoice::Dir(PathBuf::from(d))),
    }
}

/// The value of an option that is on or off, as `--allow-other[=yes|no]` takes it.
pub fn parse_yes_no(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "yes" | "true" | "on" | "1" => Ok(true),
        "no" | "false" | "off" | "0" => Ok(false),
        _ => Err("use yes or no".to_owned()),
    }
}

/// The formats shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Formats {
    /// DSF files.
    pub dsf: bool,
    /// WAV files.
    pub wav: bool,
}

fn parse_formats(s: &str) -> Result<Formats, String> {
    let mut f = Formats { dsf: false, wav: false };
    for part in s.split(',') {
        match part.trim().to_ascii_lowercase().as_str() {
            "dsf" => f.dsf = true,
            "wav" => f.wav = true,
            other => return Err(format!("unknown format {other:?}: use dsf, wav or dsf,wav")),
        }
    }
    Ok(f)
}

/// The PCM rate of `hz`, if it is one of the three.
pub fn pcm_rate(hz: u32) -> Option<PcmRate> {
    match hz {
        88_200 => Some(PcmRate::R88200),
        176_400 => Some(PcmRate::R176400),
        352_800 => Some(PcmRate::R352800),
        _ => None,
    }
}

fn parse_rate(s: &str) -> Result<u32, String> {
    s.parse()
        .ok()
        .filter(|&hz| pcm_rate(hz).is_some())
        .ok_or_else(|| "use 88200, 176400 or 352800".to_owned())
}

fn parse_burst(s: &str) -> Result<f64, String> {
    s.trim_end_matches(['x', 'X'])
        .parse::<f64>()
        .ok()
        .filter(|x| x.is_finite() && (*x == 0.0 || (1.0..=32.0).contains(x)))
        .ok_or_else(|| "use 0 (off) or a number from 1 to 32".to_owned())
}

fn parse_cutoff(s: &str) -> Result<Cutoff, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "40" => Ok(Cutoff::Khz40),
        "30" => Ok(Cutoff::Khz30),
        "wide" => Ok(Cutoff::Wide80),
        _ => Err("use 40, 30 or wide".to_owned()),
    }
}

fn parse_filter(s: &str) -> Result<Filter, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "precise" => Ok(Filter::Precise),
        "fast" => Ok(Filter::Fast),
        _ => Err("use precise or fast".to_owned()),
    }
}

fn parse_gain(s: &str) -> Result<f32, String> {
    s.parse::<f32>()
        .ok()
        .filter(|g| g.is_finite() && (-60.0..=24.0).contains(g))
        .ok_or_else(|| "use a number of dB from -60 to 24".to_owned())
}

/// `e` with `path` in front of its message.
pub fn with_path(e: io::Error, path: &Path) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

/// `path` (absolute) with its longest existing ancestor made canonical and the rest kept as
/// given: what `canonicalize` would give for a path that may not exist yet.
pub fn canonical_prefix(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(c) = at.canonicalize() {
            return rest.iter().rev().fold(c, |p, name| p.join(name));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                at = parent;
            }
            _ => return path.to_owned(),
        }
    }
}

/// What the memory limit, if any, makes of the sizes ([`Options::sizing`]).
#[derive(Debug)]
pub struct Sizing {
    /// The process's cgroup, when it has a memory limit to watch.
    pub cgroup: Option<memory::Cgroup>,
    /// DST decoder threads.
    pub threads: usize,
    /// The frame cache, in MiB.
    pub cache_mb: usize,
    /// The listing budget in bytes, or `None` for no bound.
    pub budget: Option<usize>,
}

impl Options {
    /// The PCM rate of the WAV files.
    pub fn rate(&self) -> PcmRate {
        pcm_rate(self.pcm_rate).unwrap_or(PcmRate::R176400)
    }

    /// The checks and settings before anything is read: the album suffix, the tags folder
    /// (which must exist, and be writable for `--musicbrainz`) and the PCM options.
    pub fn check(&self) -> io::Result<PcmConfig> {
        // The wide filter passes up to about 80 kHz, which 88.2 kHz PCM cannot hold (its Nyquist
        // frequency is 44.1 kHz). The converter would quietly use 40 kHz instead: refuse the
        // combination before mounting.
        let rate = self.rate();
        if self.lookup_name || self.lookup_durations {
            log::warn!(
                "--lookup-name and --lookup-durations are deprecated: use --musicbrainz=name, \
                 --musicbrainz=durations or --musicbrainz=name,durations"
            );
        }
        crate::dsf::set_mc_album_suffix(!self.no_multichannel_album_suffix);
        if let Some(dir) = &self.tags {
            let dir = dir.canonicalize().map_err(|e| with_path(e, dir))?;
            if !dir.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!("--tags {}: not a directory", dir.display()),
                ));
            }
            if self.musicbrainz.is_some() {
                crate::platform::writable(&dir).map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!(
                            "--musicbrainz writes to --tags {}, which isn't writable: {e}",
                            dir.display()
                        ),
                    )
                })?;
            }
            crate::tags::install(Some(crate::tags::Store::new(&dir)));
        }
        let pcm =
            PcmConfig { rate, cutoff: self.pcm_cutoff, gain_db: self.pcm_gain, filter: self.pcm_filter };
        if let Err(e) = pcm.validate() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("--pcm-cutoff wide needs --pcm-rate 176400 or 352800 ({e})"),
            ));
        }
        Ok(pcm)
    }

    /// The names of SOURCE (canonical), leaving out `exclude` (the mount point, when it is
    /// inside SOURCE): only names are read, images are opened when their folder is listed.
    pub fn scan(&self, source: &Path, exclude: Option<&Path>) -> io::Result<SourceDir> {
        SourceDir::scan_with(source, &self.sidecars, exclude).map_err(|e| with_path(e, source))
    }

    /// Logs what was found in SOURCE.
    pub fn log_scan(&self, source: &Path, root: &SourceDir) {
        log::info!(
            "{}: {} image files; sidecar files: {}",
            source.display(),
            root.image_count(),
            self.sidecars.describe()
        );
    }

    /// What the memory limit asks of the defaults (#231, #237), logged.
    pub fn sizing(&self, root: &SourceDir) -> Sizing {
        let cgroup = memory::Cgroup::find();
        let limit = cgroup.as_ref().map(memory::Cgroup::limit);
        if limit.is_some() && self.tags.is_some() {
            crate::engine::map_large_allocations();
        }
        let threads =
            if self.threads == 0 { memory::default_threads(default_threads(), limit) } else { self.threads };
        let cache_mb = self.cache_mb.unwrap_or_else(|| memory::default_cache_mb(limit));
        let budget = match self.listing_mb {
            Some(0) => None,
            Some(mb) => Some(mb.saturating_mul(1 << 20)),
            None => memory::listing_budget(limit),
        };
        if let Some(limit) = limit {
            log::info!(
                "memory limit {} MiB: cache {cache_mb} MiB, {threads} decoder threads, listings {}",
                limit >> 20,
                budget.map_or_else(|| "not bounded".to_owned(), |b| format!("{} MiB", b >> 20)),
            );
            let plan = memory::Plan { limit, images: root.image_count(), cache_mb, threads, budget };
            if let Some(warning) = plan.warning() {
                log::warn!("{warning}");
            }
        }
        Sizing { cgroup, threads, cache_mb, budget }
    }

    /// What `--musicbrainz` looks up, if it is on: the value, and the deprecated
    /// `--lookup-name` and `--lookup-durations`.
    pub fn musicbrainz_lookups(&self) -> Option<Musicbrainz> {
        self.musicbrainz.map(|m| Musicbrainz {
            name: m.name || self.lookup_name,
            durations: m.durations || self.lookup_durations,
        })
    }

    /// The files shown.
    pub fn output(&self) -> Output {
        Output {
            dsf: self.format.dsf,
            wav: self.format.wav,
            samples_per_frame: self.rate().samples_per_frame() as u32,
            pauses_kept: self.pauses == "kept",
        }
    }
}

impl Limits {
    /// The folder of the persistent seek index the options ask for, if any.
    ///
    /// One under `mountpoint` would be written through sacdfs's own mount (#56): an
    /// `--index-cache` there is refused; the default folder there (a mount point at or above
    /// the cache folder, such as the home folder) only turns the cache off, since a cache
    /// that cannot be used is only a warning elsewhere too.
    pub fn index_cache_dir(&self, mountpoint: Option<&Path>) -> io::Result<Option<PathBuf>> {
        if self.no_index_cache {
            log::warn!("--no-index-cache is deprecated: use --index-cache off");
        }
        let chosen = match &self.index_cache {
            Some(CacheChoice::Off) => return Ok(None),
            _ if self.no_index_cache => return Ok(None),
            Some(CacheChoice::Dir(d)) => Some(d.clone()),
            None => None,
        };
        let Some(dir) = chosen.clone().or_else(index_cache::default_dir) else {
            log::warn!("no seek index cache: neither XDG_CACHE_HOME nor HOME is set");
            return Ok(None);
        };
        let dir = std::path::absolute(&dir).map_err(|e| with_path(e, &dir))?;
        let Some(mountpoint) = mountpoint.filter(|m| canonical_prefix(&dir).starts_with(m)) else {
            return Ok(Some(dir));
        };
        let why = format!(
            "seek index cache {} is inside the mount point {}: sacdfs would write it through its own mount",
            dir.display(),
            mountpoint.display()
        );
        if chosen.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{why}; choose another --index-cache"),
            ));
        }
        log::warn!("{why}; no seek index cache (use --index-cache DIR)");
        Ok(None)
    }

    /// How long a request may wait for the source.
    pub fn request_timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(self.request_timeout))
    }
}

/// Installs the persistent seek index in `dir`. A cache that cannot be used is only a
/// warning: it costs speed, never the mount.
pub fn open_index_cache(dir: &Path) {
    match IndexCache::open(dir) {
        Ok(cache) => {
            log::debug!("seek index cache in {}", cache.dir().display());
            index_cache::install(Arc::new(cache));
        }
        Err(e) => log::warn!("no seek index cache in {}: {e}", dir.display()),
    }
}

/// The threads a `Volume` answers requests on, for `threads` decoder threads.
pub fn request_threads(threads: usize) -> usize {
    threads.clamp(4, 16)
}

/// What [`build`] needs besides the options.
#[derive(Debug)]
pub struct Parts<'a> {
    /// SOURCE, canonical.
    pub source: &'a Path,
    /// What [`Options::scan`] found.
    pub root: SourceDir,
    /// What [`Options::check`] returned.
    pub pcm: PcmConfig,
    /// What [`Options::sizing`] returned.
    pub sizing: &'a Sizing,
    /// The mount point, canonical: nothing under it is served (it matters when it is inside
    /// SOURCE).
    pub exclude: Option<&'a Path>,
    /// The seek index folder ([`Limits::index_cache_dir`]); the tags files' state is kept
    /// there too.
    pub cache_dir: Option<&'a Path>,
    /// Image files and sidecar files kept open at most.
    pub open_files: (usize, usize),
}

/// The engine and the tree for a `Volume`: sets the open-file bounds, starts the TOC pool and
/// the engine's threads, and watches a SOURCE folder for changes (#101).
pub fn build(options: &Options, parts: Parts<'_>) -> io::Result<(Arc<Tree>, Arc<Engine>)> {
    let (max_open_images, sidecars_open) = parts.open_files;
    crate::handles::global().configure(max_open_images, crate::handles::DEFAULT_IDLE);
    crate::vfs::sidecar_slots::global().set_cap(sidecars_open);
    let threads = parts.sizing.threads;
    // Opening the images of a directory listing runs on rayon's global pool.
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.min(8))
        .thread_name(|i| format!("sacdfs-toc-{i}"))
        .build_global();
    let output = options.output();
    let engine = Arc::new(
        Engine::with_pcm(threads, parts.sizing.cache_mb.saturating_mul(1 << 20), parts.pcm)?
            .with_start_burst(crate::engine::StartBurst {
                rate: options.start_burst,
                secs: options.start_burst_secs,
            }),
    );
    // An idle mount gives its cache and decoders back (#150).
    engine.start_idle_release()?;
    let mut tree = Tree::with_options(parts.root, Tuning::default(), output)
        .with_flatten(options.flatten)
        .with_listing_budget(parts.sizing.budget)
        .with_pcm(&parts.pcm);
    if let Some(at) = parts.exclude {
        tree = tree.excluding(at);
    }
    // The tags files' state is kept with the seek index (#81).
    let tree = tree.with_tag_watch(parts.cache_dir.map(|d| d.join("tag-keys")));
    // A folder SOURCE is re-read when it changes (#101); a single image has no folder.
    let tree = Arc::new(if parts.source.is_dir() { tree.watching(&options.sidecars) } else { tree });
    Ok((tree, engine))
}

/// The background threads of a front end that serves ([`start_background`]).
#[derive(Debug)]
pub struct Background {
    tree: Arc<Tree>,
    lookups: Option<crate::lookup::worker::Worker>,
}

/// How often the persistent seek index is saved, if it changed.
const FLUSH_EVERY: Duration = Duration::from_secs(30);

/// Starts, once the front end serves: the closer of idle image files, the memory watch under a
/// limit, the seek index's saver and, with `--musicbrainz`, the background lookups (#185).
pub fn start_background(
    options: &Options,
    sizing: Sizing,
    tree: &Arc<Tree>,
    engine: &Arc<Engine>,
    cache_dir: Option<&Path>,
) -> io::Result<Background> {
    crate::handles::start_idle_closer()?;
    if let Some(cgroup) = sizing.cgroup {
        let (tree, engine) = (tree.clone(), engine.clone());
        std::thread::Builder::new()
            .name("sacdfs-memory".to_owned())
            .spawn(move || memory::watch(&cgroup, &tree, &engine))?;
    }
    if let Some(cache) = index_cache::global() {
        let cache = cache.clone();
        std::thread::Builder::new().name("sacdfs-index".to_owned()).spawn(move || {
            let mut warned = false;
            loop {
                std::thread::sleep(FLUSH_EVERY);
                if let Err(e) = cache.flush() {
                    if !std::mem::replace(&mut warned, true) {
                        log::warn!("seek index cache {}: {e}", cache.dir().display());
                    }
                }
            }
        })?;
    }
    // The background lookup (#185): after the mount is up, on a thread of its own.
    let lookups = match options.tags.as_deref().zip(options.musicbrainz_lookups()) {
        Some((dir, mb)) => {
            let config = crate::lookup::worker::Config {
                tags: dir.canonicalize().map_err(|e| with_path(e, dir))?,
                state: cache_dir.map(|d| d.join("musicbrainz")),
                options: crate::lookup::Options {
                    by_name: mb.name,
                    by_durations: mb.durations,
                    no_suffix: options.no_multichannel_album_suffix,
                    ..crate::lookup::Options::default()
                },
                first_delay: crate::lookup::worker::FIRST_DELAY,
                pass_every: crate::lookup::worker::PASS_EVERY,
            };
            let client = crate::lookup::Client::new(crate::lookup::Http::new());
            let images = tree.clone();
            log::info!(
                "musicbrainz: background lookups on, by number{}{}, from {} s after the mount",
                if mb.name { ", name" } else { "" },
                if mb.durations { ", durations" } else { "" },
                config.first_delay.as_secs()
            );
            Some(crate::lookup::worker::Worker::start(config, client, move || images.images())?)
        }
        None => None,
    };
    Ok(Background { tree: tree.clone(), lookups })
}

impl Background {
    /// At the end: stops the lookups, saves the tags files' state and the seek index, and
    /// logs the image reads.
    pub fn stop(&mut self) {
        if let Some(w) = self.lookups.as_mut() {
            w.stop();
        }
        self.tree.save_tag_watch();
        if let Some(cache) = index_cache::global() {
            match cache.flush() {
                Ok(n) => log::debug!("seek index cache: {n} entries written"),
                Err(e) => log::warn!("seek index cache {}: {e}", cache.dir().display()),
            }
        }
        let (calls, bytes) = crate::disc::total_reads();
        log::info!("image reads for audio: {calls} calls, {bytes} bytes");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_prefix_of_a_missing_path() {
        let d = tempfile::tempdir().unwrap();
        let c = d.path().canonicalize().unwrap();
        assert_eq!(canonical_prefix(&d.path().join("x/y")), c.join("x/y"));
        assert_eq!(canonical_prefix(d.path()), c);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod option_tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        options: Options,
        #[command(flatten)]
        limits: Limits,
    }

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("sacdfs").chain(args.iter().copied()))
    }

    /// The default filter is `precise`, except on 32-bit x86 (the i686 build), where it is
    /// `fast`; `--pcm-filter` selects either on every target.
    #[test]
    fn pcm_filter_default_by_target() {
        let want = if cfg!(target_arch = "x86") { Filter::Fast } else { Filter::Precise };
        assert_eq!(parse(&[]).unwrap().options.pcm_filter, want);
        assert_eq!(parse(&["--pcm-filter", "fast"]).unwrap().options.pcm_filter, Filter::Fast);
        assert_eq!(parse(&["--pcm-filter", "precise"]).unwrap().options.pcm_filter, Filter::Precise);
        assert!(parse(&["--pcm-filter", "default"]).is_err());
    }

    #[test]
    fn musicbrainz_takes_the_lookups() {
        assert_eq!(parse(&[]).unwrap().options.musicbrainz_lookups(), None);
        let on = |args: &[&str]| parse(args).unwrap().options.musicbrainz_lookups().unwrap();
        assert_eq!(on(&["--tags=/t", "--musicbrainz"]), Musicbrainz::default());
        assert_eq!(on(&["--tags=/t", "--musicbrainz=name"]), Musicbrainz { name: true, durations: false });
        assert_eq!(
            on(&["--tags=/t", "--musicbrainz=name,durations"]),
            Musicbrainz { name: true, durations: true }
        );
        assert!(parse(&["--tags=/t", "--musicbrainz=bogus"]).is_err());
        assert!(parse(&["--musicbrainz"]).is_err(), "needs --tags");
    }

    #[test]
    fn the_deprecated_lookup_flags_still_work() {
        let o =
            parse(&["--tags=/t", "--musicbrainz", "--lookup-name", "--lookup-durations"]).unwrap().options;
        assert_eq!(o.musicbrainz_lookups(), Some(Musicbrainz { name: true, durations: true }));
        assert!(parse(&["--lookup-name"]).is_err(), "still needs --musicbrainz");
    }

    #[test]
    fn index_cache_takes_off() {
        assert_eq!(parse(&["--index-cache", "off"]).unwrap().limits.index_cache, Some(CacheChoice::Off));
        assert_eq!(
            parse(&["--index-cache", "/c"]).unwrap().limits.index_cache,
            Some(CacheChoice::Dir("/c".into()))
        );
        let l = parse(&["--index-cache", "off"]).unwrap().limits;
        assert_eq!(l.index_cache_dir(None).unwrap(), None);
        let l = parse(&["--no-index-cache"]).unwrap().limits;
        assert!(l.no_index_cache);
        assert_eq!(l.index_cache_dir(None).unwrap(), None);
        assert!(parse(&["--index-cache", "/c", "--no-index-cache"]).is_err());
    }

    #[test]
    fn yes_no_values() {
        assert_eq!(parse_yes_no("yes"), Ok(true));
        assert_eq!(parse_yes_no("No"), Ok(false));
        assert!(parse_yes_no("maybe").is_err());
    }
}
