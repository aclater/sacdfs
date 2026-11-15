// SPDX-License-Identifier: GPL-3.0-or-later

//! NFS file handles that survive a restart (#420).
//!
//! ```text
//!  0         1                  9                  17                 25
//! +---------+------------------+------------------+------------------+
//! | version | volume ID        | inode number     | generation       |
//! +---------+------------------+------------------+------------------+
//! ```
//!
//! All big-endian. The inode and generation numbers are the ones sacdfs keeps the same on
//! every mount for the kernel's NFS export ([`crate::ident`], #18); the volume ID names the
//! SOURCE ([`volume_id`]), so that a handle from a server over another SOURCE is stale rather
//! than read as this one's. 25 bytes, within NFSv3's 64.

use std::path::Path;

use sha2::{Digest, Sha256};

/// The layout's version: the first byte.
const VERSION: u8 = 1;

/// A handle's length in bytes.
pub const LEN: usize = 25;

/// What a handle names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle {
    pub volume: u64,
    pub ino: u64,
    pub generation: u64,
}

impl Handle {
    /// The handle's bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(LEN);
        out.push(VERSION);
        out.extend_from_slice(&self.volume.to_be_bytes());
        out.extend_from_slice(&self.ino.to_be_bytes());
        out.extend_from_slice(&self.generation.to_be_bytes());
        out
    }

    /// The handle in `bytes`, if they are one of this layout.
    pub fn decode(bytes: &[u8]) -> Option<Handle> {
        let bytes: &[u8; LEN] = bytes.try_into().ok()?;
        if bytes[0] != VERSION {
            return None;
        }
        let word = |at: usize| {
            let mut w = [0u8; 8];
            w.copy_from_slice(&bytes[at..at + 8]);
            u64::from_be_bytes(w)
        };
        let ino = word(9);
        // Inode 0 is never handed out.
        (ino != 0).then(|| Handle { volume: word(1), ino, generation: word(17) })
    }
}

/// The volume ID of SOURCE `source` (canonical, as the tree's paths are relative to it): the
/// top 64 bits of a SHA-256 of its path, never 0.
pub fn volume_id(source: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    let mut h = Sha256::new();
    h.update(b"sacdfs nfs volume v1\0");
    h.update(source.as_os_str().as_bytes());
    let d = h.finalize();
    let mut w = [0u8; 8];
    w.copy_from_slice(&d[..8]);
    u64::from_be_bytes(w).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let h = Handle { volume: 0x0102_0304_0506_0708, ino: 42, generation: u64::MAX };
        let b = h.encode();
        assert_eq!(b.len(), LEN);
        assert_eq!(Handle::decode(&b), Some(h));
    }

    #[test]
    fn refuses_what_is_not_a_handle() {
        let good = Handle { volume: 7, ino: 1, generation: 0 }.encode();
        assert_eq!(Handle::decode(&good[..LEN - 1]), None);
        assert_eq!(Handle::decode(&[good.clone(), vec![0]].concat()), None);
        let mut other = good.clone();
        other[0] = 2;
        assert_eq!(Handle::decode(&other), None);
        // nfsserve's own handles (16 bytes: start time, inode) are not ours.
        assert_eq!(Handle::decode(&[0u8; 16]), None);
        assert_eq!(Handle::decode(&Handle { volume: 7, ino: 0, generation: 0 }.encode()), None);
    }

    #[test]
    fn volume_ids_follow_the_source() {
        let a = volume_id(Path::new("/music/sacd"));
        assert_eq!(a, volume_id(Path::new("/music/sacd")));
        assert_ne!(a, volume_id(Path::new("/music/sacd2")));
        assert_ne!(a, 0);
    }
}
