// SPDX-License-Identifier: GPL-3.0-or-later

//! The persistent seek index: each image's scarletbook seek indexes, without their exact
//! checkpoints (seek hints, saved confirmations and the TOC fingerprint:
//! `AreaIndex::without_checkpoints`), kept in a cache directory across mounts so that a
//! seek after a remount near a range read before starts at a hint (#28).
//!
//! - One file per image, named by a hash of the image's canonical path (never the path
//!   itself), holding that hash, the image's size and modification time, the shape of
//!   each audio area, and each area's saved index as scarletbook writes it
//!   (`AreaIndex::to_bytes`). A version number and a checksum guard it. Any mismatch or
//!   doubt discards the file: the cache only ever costs speed.
//! - A loaded index is checked by scarletbook (`AreaIndex::from_bytes` checks every bound
//!   before it allocates; `Disc::set_seek_index` refuses one of a disc with another TOC; a
//!   hint whose sector does not hold its frame record is dropped). *Why the image's size and
//!   mtime as well:* the library's checks cannot tell a re-rip with the same TOC and other
//!   audio, so the key does, as the library asks.
//! - Written lazily (by [`IndexCache::flush`]: periodically and at unmount, only for
//!   discs whose saved form changed) and atomically (a temporary file, then a rename). The
//!   directory is bounded in size; the least recently used files are removed first.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use scarletbook::Slot;

use crate::disc::LibDisc;

/// First bytes of every index file.
const MAGIC: &[u8; 8] = b"SACDFSIX";

/// Format version; a file of any other version is discarded. Version 1 (sacdfs 0.1)
/// held sacdfs's own (frame, LSN) points.
const VERSION: u32 = 2;

/// Largest index file read.
const MAX_FILE: u64 = 4 << 20;

/// Most bytes of one area's saved index (scarletbook's form, about 8 bytes a seek hint
/// and 16 a confirmation, at most 2 048 of each by default).
pub const MAX_AREA_BYTES: usize = 1 << 20;

/// Default bound on the whole directory.
pub const DEFAULT_MAX_BYTES: u64 = 32 << 20;

/// Bytes of the checksum at the end of a file (a truncated SHA-256 of what precedes it).
const SUM: usize = 16;

/// File name suffix.
const SUFFIX: &str = ".idx";

/// One area's saved index, as scarletbook writes it; `None` for none.
pub type Saved = Option<Vec<u8>>;

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

/// The saved indexes of an entry, per area, if `bytes` is a valid entry for `key` and
/// `shapes`. Anything else (another version, a bad checksum, a different image, an area
/// over [`MAX_AREA_BYTES`]) is `None`. The indexes themselves are checked when they are
/// loaded into a disc.
pub fn decode(bytes: &[u8], key: &ImageKey, shapes: &[AreaShape]) -> Option<Vec<Saved>> {
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
        if n > MAX_AREA_BYTES {
            return None;
        }
        out.push((n > 0).then(|| r.take(n).map(<[u8]>::to_vec)).map_or(Some(None), |b| b.map(Some))?);
    }
    if !r.b.is_empty() {
        return None;
    }
    Some(out)
}

/// An entry for `key` holding `areas` (one saved index per shape; one over
/// [`MAX_AREA_BYTES`] is left out).
pub fn encode(key: &ImageKey, shapes: &[AreaShape], areas: &[Saved]) -> Vec<u8> {
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
        let saved =
            areas.get(i).and_then(Option::as_deref).filter(|s| s.len() <= MAX_AREA_BYTES).unwrap_or(&[]);
        b.extend_from_slice(&(saved.len() as u32).to_le_bytes());
        b.extend_from_slice(saved);
    }
    let sum = Sha256::digest(&b);
    b.extend_from_slice(&sum[..SUM]);
    b
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
}

/// A disc open in this process whose seek indexes are saved by [`IndexCache::flush`].
#[derive(Debug)]
struct Live {
    key: ImageKey,
    shapes: Vec<AreaShape>,
    disc: Arc<LibDisc>,
    /// Set by the disc's cursors when they hand their records to its seek index.
    changed: Arc<std::sync::atomic::AtomicBool>,
    /// The slot of each area of `shapes`.
    slots: Vec<Slot>,
    /// What the entry holds now (loaded or last written), per area.
    saved: Vec<Saved>,
    /// Whether this mount has saved or touched the entry, so that it counts as used.
    touched: bool,
}

impl Live {
    /// The saved form of the disc's indexes now.
    fn current(&self) -> Vec<Saved> {
        self.slots
            .iter()
            .map(|&slot| self.disc.seek_index(slot).map(|i| i.without_checkpoints().to_bytes()))
            .collect()
    }
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

    /// The saved indexes of the image, per area, if its entry exists and matches. A file
    /// that exists but does not match is left for the next save to replace.
    pub fn load(&self, key: &ImageKey, shapes: &[AreaShape]) -> Option<Vec<Saved>> {
        let path = self.dir.join(key.file_name());
        let bytes = read_bounded(&path).ok()?;
        let saved = decode(&bytes, key, shapes);
        if saved.is_none() {
            log::debug!("seek index cache: discarding a stale or damaged entry");
        }
        saved
    }

    /// Saves the seek indexes of `disc` (of `slots`, one per shape) by
    /// [`IndexCache::flush`] from now on; `saved` is what the entry holds now (what was
    /// loaded and used, per area).
    pub(crate) fn register(
        &self,
        key: ImageKey,
        shapes: Vec<AreaShape>,
        disc: Arc<LibDisc>,
        changed: Arc<std::sync::atomic::AtomicBool>,
        slots: Vec<Slot>,
        saved: Vec<Saved>,
    ) {
        let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        live.push(Live { key, shapes, disc, changed, slots, saved, touched: false });
    }

    /// Writes the entry of every registered disc whose saved form changed since its last
    /// write, marks the others as used, and trims the directory to its bound. The number
    /// of entries written.
    pub fn flush(&self) -> io::Result<usize> {
        let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        let mut written = 0;
        let mut first_err = None;
        for d in live.iter_mut() {
            let path = self.dir.join(d.key.file_name());
            // Only a disc whose cursors handed records to its index may have changed: the
            // others' saved form is not built again (a copy of the index and its encoding).
            let now = if d.changed.swap(false, Ordering::Relaxed) { Some(d.current()) } else { None };
            if let Some(now) = now.filter(|n| *n != d.saved) {
                match self.store(&d.key, &d.shapes, &now) {
                    Ok(()) => {
                        written += 1;
                        d.saved = now;
                    }
                    // Saved again at the next flush.
                    Err(e) => {
                        d.changed.store(true, Ordering::Relaxed);
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
    pub fn store(&self, key: &ImageKey, shapes: &[AreaShape], areas: &[Saved]) -> io::Result<()> {
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

    fn areas() -> Vec<Saved> {
        vec![Some(b"an index of the stereo area".to_vec()), None]
    }

    #[test]
    fn round_trip() {
        let b = encode(&key(), &shapes(), &areas());
        assert_eq!(decode(&b, &key(), &shapes()), Some(areas()));
        // 69 bytes of key, 21 per area shape, 16 of checksum, and the 27 bytes saved.
        assert_eq!(b.len(), 69 + 2 * 21 + 16 + 27);
        let empty = vec![Some(Vec::new()), None];
        assert_eq!(decode(&encode(&key(), &shapes(), &empty), &key(), &shapes()), Some(vec![None, None]));
    }

    #[test]
    fn oversized_areas_are_not_written() {
        let big = vec![Some(vec![7u8; MAX_AREA_BYTES + 1]), Some(vec![1, 2, 3])];
        let b = encode(&key(), &shapes(), &big);
        assert_eq!(decode(&b, &key(), &shapes()), Some(vec![None, Some(vec![1, 2, 3])]));
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

    /// A file with a valid checksum but hostile content (here: version 1, sacdfs 0.1's;
    /// a length past the end; a huge length) is rejected too.
    #[test]
    fn hostile_content_with_valid_checksum() {
        let resum = |mut b: Vec<u8>| {
            b.truncate(b.len() - SUM);
            let s = Sha256::digest(&b);
            b.extend_from_slice(&s[..SUM]);
            b
        };
        let b = encode(&key(), &shapes(), &areas());
        let mut v1 = b.clone();
        v1[8] = 1;
        assert_eq!(decode(&resum(v1), &key(), &shapes()), None);
        let one = encode(&key(), &shapes()[..1], &[Some(vec![9; 10])]);
        let at = 8 + 4 + 32 + 24 + 1 + 17;
        let mut long = one.clone();
        long[at..at + 4].copy_from_slice(&11u32.to_le_bytes());
        assert_eq!(decode(&resum(long), &key(), &shapes()[..1]), None);
        let mut huge = one;
        huge[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&resum(huge), &key(), &shapes()[..1]), None);
    }

    #[test]
    fn store_load_flush_and_trim() {
        let dir = tempfile::tempdir().unwrap();
        let cache = IndexCache::with_limit(&dir.path().join("c"), 320).unwrap();
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
        assert!(total <= 320, "{total}");
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
        /// Arbitrary bytes, and arbitrary damage to a valid entry, never panic.
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
            saved in proptest::collection::vec(any::<u8>(), 0..300),
        ) {
            let mut b = encode(&key(), &shapes(), &[Some(saved), None]);
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
                prop_assert_eq!(areas.len(), 2);
            }
        }

        #[test]
        fn encode_decode_round_trip(a in proptest::collection::vec(any::<u8>(), 1..500), b in proptest::collection::vec(any::<u8>(), 0..500)) {
            let want = vec![Some(a), (!b.is_empty()).then_some(b)];
            let e = encode(&key(), &shapes(), &want);
            prop_assert_eq!(decode(&e, &key(), &shapes()), Some(want));
        }
    }
}
