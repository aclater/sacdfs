// SPDX-License-Identifier: GPL-3.0-or-later

//! The size `statfs` reports (#62, #217).

use std::io;
use std::time::Instant;

/// The share's size, as `statfs` reports it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ShareSize {
    pub(crate) blocks: u64,
    pub(crate) frsize: u32,
    pub(crate) read: Instant,
    /// Read from the source's filesystem, for want of images: read again now and then.
    pub(crate) refresh: bool,
    pub(crate) refreshing: bool,
}

impl ShareSize {
    /// The images' total, in blocks of 64 KiB; with no images, what [`ShareSize::of_source`]
    /// says, so that this is never a 0-byte filesystem (#62).
    pub(crate) fn of_images(bytes: u64, source: &std::path::Path) -> ShareSize {
        if bytes == 0 {
            return ShareSize::of_source(source);
        }
        let frsize = 1 << 16;
        ShareSize {
            blocks: bytes.div_ceil(u64::from(frsize)),
            frsize,
            read: Instant::now(),
            refresh: false,
            refreshing: false,
        }
    }

    /// `statvfs` of `path`; 0 blocks if it fails.
    pub(crate) fn of_source(path: &std::path::Path) -> ShareSize {
        let (blocks, frsize) = statvfs(path).unwrap_or_else(|e| {
            log::debug!("{}: statvfs: {e}", path.display());
            (0, 1 << 16)
        });
        ShareSize { blocks, frsize, read: Instant::now(), refresh: true, refreshing: false }
    }
}

/// The total size of the filesystem holding `path`, in blocks of the returned size.
fn statvfs(path: &std::path::Path) -> io::Result<(u64, u32)> {
    let st = nix::sys::statvfs::statvfs(path).map_err(io::Error::from)?;
    let frsize = if st.fragment_size() > 0 { st.fragment_size() } else { st.block_size() };
    Ok((st.blocks(), u32::try_from(frsize).unwrap_or(1 << 16).max(1)))
}
