// SPDX-License-Identifier: GPL-3.0-or-later

//! The persistent seek index: each image's learned (frame, LSN) points, kept in a cache
//! directory across mounts so that a seek after a remount need not search again.
//!
//! - One file per image, named by a hash of the image's canonical path (never the path
//!   itself), holding that hash, the image's size and modification time, the shape of
//!   each audio area, and the points, delta-coded. A version number and a checksum guard
//!   it. Any mismatch or doubt discards the file: the cache only ever costs speed.
//! - Loaded points are not trusted. [`crate::disc`] uses them to aim its searches and as
//!   cursor starts that are checked against the first frame read before anything is
//!   returned, exactly as a probe of the search is checked.
//! - Written lazily (by [`IndexCache::flush`]: periodically and at unmount, only for
//!   discs whose points changed) and atomically (a temporary file, then a rename). The
//!   directory is bounded in size; the least recently used files are removed first.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use crate::disc::SharedIndex;

/// First bytes of every index file.
const MAGIC: &[u8; 8] = b"SACDFSIX";

/// Format version; a file of any other version is discarded.
const VERSION: u32 = 1;

/// Largest index file read: two areas of the most points an area keeps, at the longest
/// encoding, fit well within it.
const MAX_FILE: u64 = 4 << 20;

/// Most points per area accepted from a file (the in-memory limit of an area's index).
pub const MAX_POINTS: usize = 1 << 16;

/// Default bound on the whole directory.
pub const DEFAULT_MAX_BYTES: u64 = 32 << 20;

/// Bytes of the checksum at the end of a file (a truncated SHA-256 of what precedes it).
const SUM: usize = 16;

/// File name suffix.
const SUFFIX: &str = ".idx";

/// A frame number and an LSN.
pub type Point = (u32, u64);

/// What identifies one image's entry: its canonical path (hashed), size and mtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageKey {
    path_hash: [u8; 32],
    size: u64,
    mtime: i64,
    mtime_ns: i64,
}

impl ImageKey {
    /// The key of the image at `path` with metadata `meta`.
    pub fn new(path: &Path, meta: &fs::Metadata) -> io::Result<ImageKey> {
        let canonical = path.canonicalize()?;
        Ok(ImageKey::from_parts(
            canonical.as_os_str().as_encoded_bytes(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
        ))
    }

    /// A key from its parts (for tests).
    pub fn from_parts(canonical_path: &[u8], size: u64, mtime: i64, mtime_ns: i64) -> ImageKey {
        let mut h = Sha256::new();
        h.update(b"sacdfs-index-path\0");
        h.update(canonical_path);
        ImageKey { path_hash: h.finalize().into(), size, mtime, mtime_ns }
    }

    /// The file name of this image's entry: a hash, so that no path is visible.
    fn file_name(&self) -> String {
        let mut s = String::with_capacity(32 + SUFFIX.len());
        for b in &self.path_hash[..16] {
            s.push_str(&format!("{b:02x}"));
        }
        s.push_str(SUFFIX);
        s
    }
}

/// What an entry must match besides the key: each audio area's sector range and channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AreaShape {
    /// First audio sector of the area.
    pub start_lsn: u64,
    /// End (exclusive) of the area's audio sectors in the image.
    pub end_lsn: u64,
    /// Channel count.
    pub channels: u8,
}

/// The points of an entry, per area, if `bytes` is a valid entry for `key` and `shapes`.
/// Anything else (another version, a bad checksum, a different image, points out of
/// order or out of their area) is `None`.
pub fn decode(bytes: &[u8], key: &ImageKey, shapes: &[AreaShape]) -> Option<Vec<Vec<Point>>> {
    let body_end = bytes.len().checked_sub(SUM)?;
    let (body, sum) = bytes.split_at(body_end);
    if Sha256::digest(body)[..SUM] != *sum {
        return None;
    }
    let mut r = Cursor { b: body };
    if r.take(8)? != MAGIC || r.u32()? != VERSION {
        return None;
    }
    if r.take(32)? != key.path_hash
        || r.u64()? != key.size
        || r.i64()? != key.mtime
        || r.i64()? != key.mtime_ns
    {
        return None;
    }
    if usize::from(r.u8()?) != shapes.len() {
        return None;
    }
    let mut out = Vec::with_capacity(shapes.len());
    for shape in shapes {
        let (start, end, channels) = (r.u64()?, r.u64()?, r.u8()?);
        if (start, end, channels) != (shape.start_lsn, shape.end_lsn, shape.channels) {
            return None;
        }
        let n = usize::try_from(r.u32()?).ok()?;
        if n > MAX_POINTS {
            return None;
        }
        // Each point takes at least two bytes, so a short file cannot claim many.
        let mut points = Vec::with_capacity(n.min(r.b.len() / 2));
        let mut prev: Option<Point> = None;
        for _ in 0..n {
            let (df, dl) = (r.varint()?, r.varint()?);
            let (f, l) = match prev {
                None => (df, start.checked_add(dl)?),
                Some((pf, pl)) => (u64::from(pf).checked_add(df)?.checked_add(1)?, pl.checked_add(dl)?),
            };
            let f = u32::try_from(f).ok()?;
            if l >= end {
                return None;
            }
            points.push((f, l));
            prev = Some((f, l));
        }
        out.push(points);
    }
    if !r.b.is_empty() {
        return None;
    }
    Some(out)
}

/// An entry for `key` holding `areas` (one list of points per shape, frames strictly and
/// LSNs weakly increasing, LSNs within the area).
pub fn encode(key: &ImageKey, shapes: &[AreaShape], areas: &[Vec<Point>]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&VERSION.to_le_bytes());
    b.extend_from_slice(&key.path_hash);
    b.extend_from_slice(&key.size.to_le_bytes());
    b.extend_from_slice(&key.mtime.to_le_bytes());
    b.extend_from_slice(&key.mtime_ns.to_le_bytes());
    b.push(shapes.len() as u8);
    for (i, shape) in shapes.iter().enumerate() {
        b.extend_from_slice(&shape.start_lsn.to_le_bytes());
        b.extend_from_slice(&shape.end_lsn.to_le_bytes());
        b.push(shape.channels);
        let points: Vec<Point> = areas.get(i).map_or_else(Vec::new, |p| valid_points(p, shape));
        b.extend_from_slice(&(points.len() as u32).to_le_bytes());
        let mut prev: Option<Point> = None;
        for &(f, l) in &points {
            match prev {
                None => {
                    put_varint(&mut b, u64::from(f));
                    put_varint(&mut b, l - shape.start_lsn);
                }
                Some((pf, pl)) => {
                    put_varint(&mut b, u64::from(f - pf - 1));
                    put_varint(&mut b, l - pl);
                }
            }
            prev = Some((f, l));
        }
    }
    let sum = Sha256::digest(&b);
    b.extend_from_slice(&sum[..SUM]);
    b
}

/// The points that can be encoded: in order, inside the area, at most [`MAX_POINTS`].
fn valid_points(points: &[Point], shape: &AreaShape) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(points.len().min(MAX_POINTS));
    for &(f, l) in points {
        if out.len() >= MAX_POINTS {
            break;
        }
        if !(shape.start_lsn..shape.end_lsn).contains(&l) {
            continue;
        }
        if out.last().is_some_and(|&(pf, pl)| f <= pf || l < pl) {
            continue;
        }
        out.push((f, l));
    }
    out
}

fn put_varint(b: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        b.push((v as u8) | 0x80);
        v >>= 7;
    }
    b.push(v as u8);
}

/// A reader of the fields of an entry; every method is `None` past the end.
struct Cursor<'a> {
    b: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.b.len() < n {
            return None;
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Some(h)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes)
    }

    fn i64(&mut self) -> Option<i64> {
        self.take(8).and_then(|b| b.try_into().ok()).map(i64::from_le_bytes)
    }

    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let byte = self.u8()?;
            let part = u64::from(byte & 0x7f);
            if i == 9 && part > 1 {
                return None;
            }
            v |= part << (7 * i);
            if byte & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
}

/// A disc open in this process whose points are saved by [`IndexCache::flush`].
#[derive(Debug)]
struct Live {
    key: ImageKey,
    shapes: Vec<AreaShape>,
    areas: Vec<Arc<SharedIndex>>,
    /// Whether this mount has saved or touched the entry, so that it counts as used.
    touched: bool,
}

/// The persistent seek index of one directory.
#[derive(Debug)]
pub struct IndexCache {
    dir: PathBuf,
    max_bytes: u64,
    live: Mutex<Vec<Live>>,
    /// Entries written so far (for tests and diagnostics).
    writes: AtomicU64,
}

static GLOBAL: OnceLock<Arc<IndexCache>> = OnceLock::new();

/// Makes `cache` the one every [`crate::disc::Disc::open`] of this process uses. Only the
/// first call has an effect; `false` for any later one.
pub fn install(cache: Arc<IndexCache>) -> bool {
    GLOBAL.set(cache).is_ok()
}

/// The cache installed with [`install`], if any.
pub fn global() -> Option<&'static Arc<IndexCache>> {
    GLOBAL.get()
}

/// The default cache directory: `$XDG_CACHE_HOME/sacdfs`, else `~/.cache/sacdfs`.
pub fn default_dir() -> Option<PathBuf> {
    let abs = |v: std::ffi::OsString| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
    std::env::var_os("XDG_CACHE_HOME")
        .and_then(abs)
        .or_else(|| std::env::var_os("HOME").and_then(abs).map(|h| h.join(".cache")))
        .map(|d| d.join("sacdfs"))
}

impl IndexCache {
    /// The cache in `dir`, bounded to [`DEFAULT_MAX_BYTES`]. The directory is made
    /// (private to the user) when the first entry is written.
    pub fn open(dir: &Path) -> io::Result<IndexCache> {
        Self::with_limit(dir, DEFAULT_MAX_BYTES)
    }

    /// The cache in `dir`, bounded to `max_bytes`.
    pub fn with_limit(dir: &Path, max_bytes: u64) -> io::Result<IndexCache> {
        if dir.exists() && !dir.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "not a directory"));
        }
        Ok(IndexCache {
            dir: dir.to_owned(),
            max_bytes,
            live: Mutex::new(Vec::new()),
            writes: AtomicU64::new(0),
        })
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Entries written so far.
    pub fn writes(&self) -> u64 {
        self.writes.load(Ordering::Relaxed)
    }

    /// The saved points of the image, per area, if its entry exists and matches. A file
    /// that exists but does not match is left for the next save to replace.
    pub fn load(&self, key: &ImageKey, shapes: &[AreaShape]) -> Option<Vec<Vec<Point>>> {
        let path = self.dir.join(key.file_name());
        let bytes = read_bounded(&path).ok()?;
        let points = decode(&bytes, key, shapes);
        if points.is_none() {
            log::debug!("seek index cache: discarding a stale or damaged entry");
        }
        points
    }

    /// Saves the points of `areas` by [`IndexCache::flush`] from now on.
    pub(crate) fn register(&self, key: ImageKey, shapes: Vec<AreaShape>, areas: Vec<Arc<SharedIndex>>) {
        let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        live.push(Live { key, shapes, areas, touched: false });
    }

    /// Writes the entry of every registered disc whose points changed since its last
    /// write, marks the others as used, and trims the directory to its bound. The number
    /// of entries written.
    pub fn flush(&self) -> io::Result<usize> {
        let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        let mut written = 0;
        let mut first_err = None;
        for d in live.iter_mut() {
            let path = self.dir.join(d.key.file_name());
            // Every area's flag is cleared, so `fold`, not `any`.
            let changed = d.areas.iter().fold(false, |c, a| crate::disc::take_dirty(a) | c);
            if changed {
                let points: Vec<Vec<Point>> =
                    d.areas.iter().map(|a| crate::disc::points_for_save(a)).collect();
                match self.store(&d.key, &d.shapes, &points) {
                    Ok(()) => written += 1,
                    Err(e) => {
                        // Saved again at the next flush.
                        for a in &d.areas {
                            crate::disc::mark_dirty(a);
                        }
                        first_err.get_or_insert(e);
                    }
                }
                d.touched = true;
            } else if !d.touched {
                // Used in this mount: keep it from being evicted first.
                if let Ok(f) = File::open(&path) {
                    let _ = f.set_modified(SystemTime::now());
                }
                d.touched = true;
            }
        }
        drop(live);
        if written > 0 {
            self.trim()?;
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(written),
        }
    }

    /// Writes one entry atomically: a new temporary file, then a rename over the old one.
    pub fn store(&self, key: &ImageKey, shapes: &[AreaShape], areas: &[Vec<Point>]) -> io::Result<()> {
        let bytes = encode(key, shapes, areas);
        if !self.dir.is_dir() {
            fs::create_dir_all(&self.dir)?;
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        }
        let name = key.file_name();
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let tmp = self.dir.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&tmp)?;
            f.write_all(&bytes)?;
            drop(f);
            fs::rename(&tmp, self.dir.join(&name))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Removes the least recently used entries until the directory is within its bound,
    /// and temporary files left by a crash.
    fn trim(&self) -> io::Result<()> {
        let mut entries = Vec::new();
        let mut total = 0u64;
        let now = SystemTime::now();
        for e in fs::read_dir(&self.dir)? {
            let Ok(e) = e else { continue };
            let name = e.file_name();
            let name = name.to_string_lossy();
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if name.starts_with('.') && name.ends_with(".tmp") {
                if now.duration_since(mtime).is_ok_and(|d| d.as_secs() > 3600) {
                    let _ = fs::remove_file(e.path());
                }
                continue;
            }
            if name.ends_with(SUFFIX) {
                total += meta.len();
                entries.push((mtime, meta.len(), e.path()));
            }
        }
        if total <= self.max_bytes {
            return Ok(());
        }
        entries.sort();
        for (_, len, path) in entries {
            if total <= self.max_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
        Ok(())
    }
}

/// The contents of a regular file of at most [`MAX_FILE`] bytes, not following a
/// symbolic link.
fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let f = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    let meta = f.metadata()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return Err(io::Error::other("not an index file"));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    f.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(io::Error::other("index file too large"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use proptest::prelude::*;

    use super::*;

    fn key() -> ImageKey {
        ImageKey::from_parts(b"/music/a.iso", 1 << 30, 1_700_000_000, 5)
    }

    fn shapes() -> Vec<AreaShape> {
        vec![
            AreaShape { start_lsn: 600, end_lsn: 200_000, channels: 2 },
            AreaShape { start_lsn: 200_000, end_lsn: 500_000, channels: 6 },
        ]
    }

    fn areas() -> Vec<Vec<Point>> {
        vec![vec![(0, 600), (40, 790), (81, 980), (500, 3000)], vec![(7, 200_010), (8, 200_010)]]
    }

    #[test]
    fn round_trip() {
        let b = encode(&key(), &shapes(), &areas());
        assert_eq!(decode(&b, &key(), &shapes()), Some(areas()));
        // 69 bytes of key, 21 per area shape, 16 of checksum, and 16 for the six points.
        assert_eq!(b.len(), 69 + 2 * 21 + 16 + 16);
    }

    #[test]
    fn out_of_order_points_are_not_written() {
        let bad = vec![vec![(5, 700), (4, 800), (6, 650), (7, 600), (9, 900), (10, 300_000)], vec![]];
        let b = encode(&key(), &shapes(), &bad);
        assert_eq!(decode(&b, &key(), &shapes()), Some(vec![vec![(5, 700), (9, 900)], vec![]]));
    }

    #[test]
    fn other_images_do_not_match() {
        let b = encode(&key(), &shapes(), &areas());
        let other_size = ImageKey { size: key().size + 1, ..key() };
        let other_mtime = ImageKey { mtime: key().mtime + 1, ..key() };
        let other_ns = ImageKey { mtime_ns: 6, ..key() };
        let other_path = ImageKey::from_parts(b"/music/b.iso", 1 << 30, 1_700_000_000, 5);
        for k in [other_size, other_mtime, other_ns, other_path] {
            assert_eq!(decode(&b, &k, &shapes()), None);
        }
        let mut s = shapes();
        s[1].channels = 5;
        assert_eq!(decode(&b, &key(), &s), None);
        assert_eq!(decode(&b, &key(), &s[..1]), None);
    }

    #[test]
    fn damage_is_detected() {
        let b = encode(&key(), &shapes(), &areas());
        for i in 0..b.len() {
            let mut c = b.clone();
            c[i] ^= 0x10;
            assert_eq!(decode(&c, &key(), &shapes()), None, "byte {i}");
        }
        for n in 0..b.len() {
            assert_eq!(decode(&b[..n], &key(), &shapes()), None, "truncated to {n}");
        }
        let mut longer = b.clone();
        longer.push(0);
        assert_eq!(decode(&longer, &key(), &shapes()), None);
    }

    /// A file with a valid checksum but hostile content (here: version 2, points past the
    /// area, a huge count) is rejected too.
    #[test]
    fn hostile_content_with_valid_checksum() {
        let resum = |mut b: Vec<u8>| {
            b.truncate(b.len() - SUM);
            let s = Sha256::digest(&b);
            b.extend_from_slice(&s[..SUM]);
            b
        };
        let b = encode(&key(), &shapes(), &areas());
        let mut v2 = b.clone();
        v2[8] = 2;
        assert_eq!(decode(&resum(v2), &key(), &shapes()), None);
        let past_end = encode(&key(), &shapes()[..1], &[vec![(0, 600)]]);
        let mut p = past_end.clone();
        // The first point's LSN delta is its second varint, after the 4-byte count.
        let at = 8 + 4 + 32 + 24 + 1 + 17 + 4 + 1;
        p[at] = 0xff;
        p.insert(at + 1, 0xff);
        p.insert(at + 2, 0x7f);
        assert_eq!(decode(&resum(p), &key(), &shapes()[..1]), None);
        let mut huge = past_end;
        let at = 8 + 4 + 32 + 24 + 1 + 17;
        huge[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&resum(huge), &key(), &shapes()[..1]), None);
    }

    #[test]
    fn store_load_flush_and_trim() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::with_limit(&dir.path().join("c"), 300).unwrap();
        assert!(!cache.dir().exists(), "made only when written to");
        assert_eq!(cache.load(&key(), &shapes()), None);
        cache.store(&key(), &shapes(), &areas()).unwrap();
        assert_eq!(
            fs::metadata(cache.dir()).unwrap().permissions().mode() & 0o777,
            0o700,
            "private directory"
        );
        assert_eq!(cache.load(&key(), &shapes()), Some(areas()));
        let names: Vec<String> = fs::read_dir(cache.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 1);
        assert!(names[0].ends_with(".idx") && !names[0].contains("music"), "{names:?}");
        // A changed image misses.
        assert_eq!(cache.load(&ImageKey { size: 1, ..key() }, &shapes()), None);
        // Over the bound, the oldest entries go.
        for i in 0..10u8 {
            let k = ImageKey::from_parts(&[b'x', i], 1, 2, 3);
            cache.store(&k, &shapes(), &areas()).unwrap();
            let f = File::open(cache.dir().join(k.file_name())).unwrap();
            f.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(u64::from(i) + 10))
                .unwrap();
        }
        cache.trim().unwrap();
        let total: u64 =
            fs::read_dir(cache.dir()).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum();
        assert!(total <= 300, "{total}");
        let newest = ImageKey::from_parts(&[b'x', 9], 1, 2, 3);
        assert!(cache.load(&newest, &shapes()).is_some());
        assert_eq!(cache.load(&ImageKey::from_parts(&[b'x', 0], 1, 2, 3), &shapes()), None);
    }

    #[test]
    fn symlinks_and_oversized_files_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::open(dir.path()).unwrap();
        let target = dir.path().join("elsewhere");
        fs::write(&target, encode(&key(), &shapes(), &areas())).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(key().file_name())).unwrap();
        assert_eq!(cache.load(&key(), &shapes()), None);
        fs::remove_file(dir.path().join(key().file_name())).unwrap();
        let f = File::create(dir.path().join(key().file_name())).unwrap();
        f.set_len(MAX_FILE + 1).unwrap();
        assert_eq!(cache.load(&key(), &shapes()), None);
    }

    proptest! {
        /// Arbitrary bytes, and arbitrary damage to a valid entry, never panic, and
        /// anything accepted lies inside its areas, in order.
        #[test]
        fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            let _ = decode(&bytes, &key(), &shapes());
        }

        /// With `resum`, the checksum is recomputed after the damage, as a hostile writer
        /// would, so that the parser itself is fuzzed.
        #[test]
        fn damaged_entries_are_safe(
            edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8),
            cut in any::<usize>(),
            resum in any::<bool>(),
            pts in proptest::collection::vec((0u32..100_000, 600u64..200_000), 0..50),
        ) {
            let mut sorted = pts.clone();
            sorted.sort_unstable();
            let mut b = encode(&key(), &shapes(), &[sorted, vec![]]);
            for (at, v) in edits {
                let n = b.len();
                b[at % n] = v;
            }
            b.truncate(b.len() - cut % 4);
            if resum && b.len() > SUM {
                b.truncate(b.len() - SUM);
                let s = Sha256::digest(&b);
                b.extend_from_slice(&s[..SUM]);
            }
            if let Some(areas) = decode(&b, &key(), &shapes()) {
                for (a, s) in areas.iter().zip(shapes()) {
                    for w in a.windows(2) {
                        prop_assert!(w[0].0 < w[1].0 && w[0].1 <= w[1].1);
                    }
                    for p in a {
                        prop_assert!((s.start_lsn..s.end_lsn).contains(&p.1));
                    }
                }
            }
        }

        #[test]
        fn encode_decode_round_trip(pts in proptest::collection::vec((0u32..u32::MAX, 600u64..200_000), 0..200)) {
            let mut sorted = pts.clone();
            sorted.sort_unstable();
            let want = valid_points(&sorted, &shapes()[0]);
            let b = encode(&key(), &shapes(), &[sorted, vec![]]);
            prop_assert_eq!(decode(&b, &key(), &shapes()), Some(vec![want, vec![]]));
        }
    }
}
