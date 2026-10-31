// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent inode and generation numbers (issue #18).
//!
//! An inode number says what an entry *is*, so that the same source tree gives the same
//! numbers on every mount, and an NFS client's file handles still work after sacdfs restarts.
//!
//! ```text
//!  63                                   16 15 14 13 12         5  4  3      0
//! +---------------------------------------+-----+--+------------+--+--------+
//! | key: 48 bits of a hash of the path    |kind |sl| track no.  |wv| flavour|
//! +---------------------------------------+-----+--+------------+--+--------+
//! ```
//!
//! - The **key** names a source directory (by its path relative to SOURCE) or an image (by
//!   its relative path): the top 48 bits of the SHA-256 of a fixed prefix, a domain byte
//!   (directory or image) and the path's bytes.
//! - The low 16 bits name an entry inside an image: kind 0 is the directory itself (a source
//!   directory or an image's directory), kind 1 an area (`sl`: the slot, 0 for 2-channel, 1
//!   for multichannel), kind 2 a track file (slot, track number 1 to 255, `wv` set for WAV,
//!   and 4 bits of the PCM flavour of a WAV file, 0 for DSF).
//! - The root is inode 1, as FUSE requires; key 0 is never handed out, so no other entry can
//!   be 1.
//!
//! **Collisions.** Every key is computed at mount from the names already walked, and two
//! paths with the same key are detected there. The path that sorts first (by domain, then by
//! its bytes) keeps the key; each other one takes the first free key of the same hash over
//! its path followed by an attempt number 1, 2, ..., free meaning no path's first key and not
//! already handed out. Such a fallback key can change on a later mount if a new path's first
//! key takes it; with 48 bits, a collision needs about 16 million paths to be likely at all.
//!
//! **Generations.** A track file's generation is a hash of its image's sector count and
//! modification time, the file's format, size and output version, and for WAV the full PCM
//! flavour: a handle to a file whose bytes may differ fails with `ESTALE` instead of serving
//! other data. Directories have generation 0: they are named containers, and a handle to one
//! whose content changed still finds it.

use std::collections::{HashMap, HashSet};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::SystemTime;

use scarletbook::Slot;
use sha2::{Digest, Sha256};

use crate::pcm::{Cutoff, PcmConfig};
use crate::track::{Format, TrackFile};
use crate::tree::{ROOT, SourceDir};

/// Bits of an inode number below the key.
const ENTRY_BITS: u32 = 16;

/// The bits of an inode number that hold the key.
const KEY_MASK: u64 = !((1 << ENTRY_BITS) - 1);

const KIND_AREA: u64 = 1 << 14;
const KIND_FILE: u64 = 2 << 14;

/// Version of the bytes of DSF files: bump when a change makes the same track's DSF file
/// differ (header, tag, padding), so that handles to the old bytes go stale.
const DSF_VERSION: u32 = 1;

/// Version of the bytes of WAV files: bump when a change makes the same track's WAV file
/// differ (converter coefficients, rounding, header, tags).
const WAV_VERSION: u32 = 1;

/// What a key names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Domain {
    Dir = 1,
    Image = 2,
}

/// The first 64 bits of a SHA-256 of `parts`.
fn sha64(parts: &[&[u8]]) -> u64 {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    let d = h.finalize();
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[..8]);
    u64::from_be_bytes(b)
}

/// The inode number of a path's key at `attempt` (0: its first key).
fn key_ino(domain: Domain, rel: &[u8], attempt: u32) -> u64 {
    let n = attempt.to_be_bytes();
    let h = if attempt == 0 {
        sha64(&[b"sacdfs ino v1\0", &[domain as u8], rel])
    } else {
        // A relative path holds no NUL byte, so this never equals another path's first key
        // input.
        sha64(&[b"sacdfs ino v1\0", &[domain as u8], rel, b"\0", &n])
    };
    h & KEY_MASK
}

/// The PCM options that change the bytes of a WAV file (the rate is in the output's samples
/// per frame), as a hash.
pub fn pcm_flavour(pcm: &PcmConfig) -> u64 {
    let cutoff: u8 = match pcm.cutoff {
        Cutoff::Khz40 => 1,
        Cutoff::Khz30 => 2,
        Cutoff::Wide80 => 3,
    };
    // As the converter: a gain that is not finite counts as 0 (and -0 as 0).
    let gain = if pcm.gain_db.is_finite() && pcm.gain_db != 0.0 { pcm.gain_db.to_bits() } else { 0 };
    sha64(&[b"sacdfs pcm v1\0", &[cutoff], &gain.to_be_bytes()])
}

/// One source directory or image of the index.
#[derive(Debug, Clone, Copy)]
struct Indexed {
    /// Inode number of the source directory holding it (the root for the top level).
    parent: u64,
}

/// The inode numbers of every source directory and image, made at mount from the names
/// walked, and the way back from a number to its place in the tree.
#[derive(Debug)]
pub struct Ids {
    /// The source root, which relative paths are relative to.
    root: std::path::PathBuf,
    /// Every directory and image key's inode number, with its parent.
    by_ino: HashMap<u64, Indexed>,
    /// Keys of paths that lost a collision. Almost always empty.
    fallback: HashMap<(Domain, Vec<u8>), u64>,
}

impl Ids {
    /// The numbers of `root`'s directories and images. Hashes every name once; opens no
    /// file.
    pub fn new(root: &SourceDir) -> Ids {
        Self::with_keys(root, key_ino)
    }

    /// [`Ids::new`] with another key function (tests force collisions with it).
    fn with_keys(root: &SourceDir, key_ino: impl Fn(Domain, &[u8], u32) -> u64) -> Ids {
        // (domain, relative path, parent item index or None for the root)
        let mut items: Vec<(Domain, Vec<u8>, Option<usize>)> = Vec::new();
        let base = root.path.clone();
        let rel = |p: &Path| p.strip_prefix(&base).unwrap_or(p).as_os_str().as_bytes().to_vec();
        let mut stack: Vec<(&SourceDir, Option<usize>)> = vec![(root, None)];
        while let Some((dir, me)) = stack.pop() {
            for img in &dir.images {
                items.push((Domain::Image, rel(img), me));
            }
            for (_, sub) in &dir.dirs {
                items.push((Domain::Dir, rel(&sub.path), me));
                stack.push((sub, Some(items.len() - 1)));
            }
        }
        let first: Vec<u64> = items.iter().map(|(d, r, _)| key_ino(*d, r, 0)).collect();
        // Detect collisions: every key held by more than one path, or the reserved key 0.
        let mut owner: HashMap<u64, usize> = HashMap::with_capacity(items.len());
        let mut losers: Vec<usize> = Vec::new();
        for (i, &k) in first.iter().enumerate() {
            if k == 0 {
                losers.push(i);
                continue;
            }
            if let Some(o) = owner.get_mut(&k) {
                // The path that sorts first keeps the key.
                let (a, b) = (&items[*o], &items[i]);
                if (b.0, &b.1) < (a.0, &a.1) {
                    losers.push(std::mem::replace(o, i));
                } else {
                    losers.push(i);
                }
            } else {
                owner.insert(k, i);
            }
        }
        let mut ino = first;
        let mut fallback = HashMap::new();
        if !losers.is_empty() {
            losers.sort_by(|&a, &b| (items[a].0, &items[a].1).cmp(&(items[b].0, &items[b].1)));
            let mut taken: HashSet<u64> = owner.keys().copied().collect();
            taken.insert(0);
            for i in losers {
                let (d, r, _) = &items[i];
                let k = (1..).map(|n| key_ino(*d, r, n)).find(|k| !taken.contains(k)).unwrap_or(0);
                log::debug!("inode key collision: {} takes a fallback key", String::from_utf8_lossy(r));
                taken.insert(k);
                ino[i] = k;
                fallback.insert((*d, r.clone()), k);
            }
        }
        let by_ino = items
            .iter()
            .zip(&ino)
            .map(|((_, _, parent), &k)| (k, Indexed { parent: parent.map_or(ROOT, |p| ino[p]) }))
            .collect();
        Ids { root: base, by_ino, fallback }
    }

    fn of(&self, domain: Domain, path: &Path) -> u64 {
        let rel = path.strip_prefix(&self.root).unwrap_or(path).as_os_str().as_bytes();
        if !self.fallback.is_empty() {
            if let Some(&k) = self.fallback.get(&(domain, rel.to_vec())) {
                return k;
            }
        }
        key_ino(domain, rel, 0)
    }

    #[cfg(test)]
    fn of_with(&self, domain: Domain, path: &Path, key: impl Fn(Domain, &[u8], u32) -> u64) -> u64 {
        let rel = path.strip_prefix(&self.root).unwrap_or(path).as_os_str().as_bytes();
        self.fallback.get(&(domain, rel.to_vec())).copied().unwrap_or_else(|| key(domain, rel, 0))
    }

    /// The inode number of the source directory at `path`.
    pub fn dir(&self, path: &Path) -> u64 {
        self.of(Domain::Dir, path)
    }

    /// The inode number of the image at `path` (its directory, or the collapsed directory
    /// that shows it with `--flatten`).
    pub fn image(&self, path: &Path) -> u64 {
        self.of(Domain::Image, path)
    }

    /// Number of directories and images indexed.
    pub fn len(&self) -> usize {
        self.by_ino.len()
    }

    /// Whether nothing is indexed.
    pub fn is_empty(&self) -> bool {
        self.by_ino.is_empty()
    }

    /// The inode numbers from the top level down to the directory or image `ino` belongs to
    /// (its own key), or `None` if no indexed path has that key. The root is not included.
    pub fn chain(&self, ino: u64) -> Option<Vec<u64>> {
        let mut k = ino & KEY_MASK;
        let mut out = Vec::new();
        while k != ROOT {
            out.push(k);
            // A cycle is impossible (parents are made before their children), but bound the
            // walk anyway.
            if out.len() > 256 {
                return None;
            }
            k = self.by_ino.get(&k)?.parent;
        }
        out.reverse();
        Some(out)
    }
}

/// The entry part of `ino`: 0 for a source directory or an image's directory, else an
/// area or a file inside the image.
pub fn entry_of(ino: u64) -> u64 {
    ino & !KEY_MASK
}

/// The key part of `ino`.
pub fn key_of(ino: u64) -> u64 {
    ino & KEY_MASK
}

fn slot_bit(slot: Slot) -> u64 {
    match slot {
        Slot::TwoChannel => 0,
        Slot::Multichannel => 1 << 13,
    }
}

/// For an area's or a file's inode number, the number of the area (the same for an
/// area); `None` for a directory or image key, or an entry of no known kind.
pub fn area_of(ino: u64) -> Option<u64> {
    match ino & (3 << 14) {
        KIND_AREA => Some(ino),
        KIND_FILE => Some(key_of(ino) | KIND_AREA | (ino & (1 << 13))),
        _ => None,
    }
}

/// The inode number of the area in `slot` of the image whose number is `image`.
pub fn area(image: u64, slot: Slot) -> u64 {
    key_of(image) | KIND_AREA | slot_bit(slot)
}

/// The inode number of track `number`'s file in `format` in the area in `slot` of the
/// image `image`; `flavour` is the PCM flavour, used for WAV only.
pub fn file(image: u64, slot: Slot, number: u8, format: Format, flavour: u64) -> u64 {
    let f = match format {
        Format::Dsf => 0,
        Format::Wav => (1 << 4) | (flavour >> 60),
    };
    key_of(image) | KIND_FILE | slot_bit(slot) | (u64::from(number) << 5) | f
}

/// The generation of a track file: see the module documentation. `flavour` is the PCM
/// flavour (WAV only) mixed with the output's samples per frame.
pub fn generation(file: &TrackFile, flavour: u64) -> u64 {
    let mtime = file.disc.mtime.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let (version, flavour) = match file.format() {
        Format::Dsf => (DSF_VERSION, 0),
        Format::Wav => (WAV_VERSION, flavour),
    };
    let fmt: u8 = match file.format() {
        Format::Dsf => 1,
        Format::Wav => 2,
    };
    let h = sha64(&[
        b"sacdfs gen v1\0",
        &file.disc.toc.sectors.to_be_bytes(),
        &mtime.as_secs().to_be_bytes(),
        &mtime.subsec_nanos().to_be_bytes(),
        &[fmt],
        &version.to_be_bytes(),
        &file.size.to_be_bytes(),
        &flavour.to_be_bytes(),
    ]);
    // The kernel keeps 32 bits of it; never 0, so that a file never looks like a directory's
    // generation.
    (h & 0xffff_ffff).max(1)
}

/// The flavour of WAV files: the PCM options' hash mixed with the samples per frame (the
/// rate).
pub fn wav_flavour(pcm: u64, samples_per_frame: u32) -> u64 {
    sha64(&[b"sacdfs wav v1\0", &pcm.to_be_bytes(), &samples_per_frame.to_be_bytes()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let img = key_ino(Domain::Image, b"a/b.iso", 0);
        assert_eq!(entry_of(img), 0);
        assert_ne!(img, 0);
        let st = area(img, Slot::TwoChannel);
        let mc = area(img, Slot::Multichannel);
        assert_ne!(st, mc);
        assert_eq!(key_of(st), img);
        let mut all = HashSet::new();
        for slot in [Slot::TwoChannel, Slot::Multichannel] {
            for n in 1..=255u8 {
                for fmt in [Format::Dsf, Format::Wav] {
                    assert!(all.insert(file(img, slot, n, fmt, u64::MAX)));
                }
            }
        }
        assert!(!all.contains(&st) && !all.contains(&mc) && !all.contains(&img));
        assert!(all.iter().all(|&i| key_of(i) == img && i != ROOT));
    }

    #[test]
    fn keys_are_fixed() {
        // The numbers must never change between versions: pin one.
        assert_eq!(key_ino(Domain::Dir, b"Artist", 0), key_ino(Domain::Dir, b"Artist", 0));
        assert_ne!(key_ino(Domain::Dir, b"x", 0), key_ino(Domain::Image, b"x", 0));
        assert_ne!(key_ino(Domain::Dir, b"x", 0), key_ino(Domain::Dir, b"x", 1));
        let pinned = key_ino(Domain::Image, b"Artist/Album/disc.iso", 0);
        assert_eq!(pinned, PINNED, "inode numbers changed: {pinned:#x}");
    }

    #[test]
    fn collisions_fall_back_deterministically() {
        use std::path::PathBuf;
        // Every first key the same (and one of them 0): every path but the first in order
        // takes a fallback key, the same whatever the walk order.
        let weak = |d: Domain, r: &[u8], n: u32| -> u64 {
            if n == 0 { if r == b"z.iso" { 0 } else { 7 << 16 } } else { key_ino(d, r, n) }
        };
        let dir = |p: &str, dirs: Vec<(&str, SourceDir)>, images: &[&str]| SourceDir {
            path: PathBuf::from(p),
            dirs: dirs.into_iter().map(|(n, d)| (n.into(), d)).collect(),
            images: images.iter().map(|i| PathBuf::from(format!("{p}/{i}"))).collect(),
        };
        let make = |order: bool| {
            let a = dir("/s/a", vec![], &["x.iso"]);
            let b = dir("/s/b", vec![], &["y.iso"]);
            let dirs = if order { vec![("a", a), ("b", b)] } else { vec![("b", b), ("a", a)] };
            dir("/s", dirs, &["z.iso"])
        };
        let paths = [
            (Domain::Dir, "/s/a"),
            (Domain::Dir, "/s/b"),
            (Domain::Image, "/s/a/x.iso"),
            (Domain::Image, "/s/b/y.iso"),
            (Domain::Image, "/s/z.iso"),
        ];
        let numbers = |ids: &Ids| -> Vec<u64> {
            paths.iter().map(|(d, p)| ids.of_with(*d, Path::new(p), weak)).collect()
        };
        let one = Ids::with_keys(&make(true), weak);
        let two = Ids::with_keys(&make(false), weak);
        assert_eq!(numbers(&one), numbers(&two));
        let n = numbers(&one);
        // The first in order (the directory `a`) keeps the key; all differ; none is 0.
        assert_eq!(n[0], 7 << 16);
        let set: HashSet<_> = n.iter().collect();
        assert_eq!(set.len(), n.len());
        assert!(!n.contains(&0));
        assert_eq!(one.len(), 5);
        // The chain of an image runs through its directory's fallback-free number.
        assert_eq!(one.chain(n[3]).unwrap(), vec![n[1], n[3]]);
        assert_eq!(one.chain(area(n[3], Slot::Multichannel)).unwrap(), vec![n[1], n[3]]);
        assert!(one.chain(123 << 16).is_none());
    }

    const PINNED: u64 = 0x0fbd_e4b4_daf9_0000;

    #[test]
    fn flavour_ignores_what_the_converter_ignores() {
        let base = PcmConfig::default();
        let f = |c: PcmConfig| pcm_flavour(&c);
        assert_eq!(f(base), f(PcmConfig { gain_db: -0.0, ..base }));
        assert_eq!(f(base), f(PcmConfig { gain_db: f32::NAN, ..base }));
        assert_ne!(f(base), f(PcmConfig { gain_db: 1.0, ..base }));
        assert_ne!(f(base), f(PcmConfig { cutoff: Cutoff::Khz30, ..base }));
    }
}
