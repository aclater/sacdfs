// SPDX-License-Identifier: GPL-3.0-or-later

//! Read-ahead for small sequential reads, per open file (#419).
//!
//! *Why:* Windows passes a program's small reads through as they are: 4 KiB reads of a file
//! reach sacdfs-win as 4 KiB reads, even with the cache manager on, where Linux's page cache
//! turns them into 128 KiB reads. Each read is a round trip through WinFsp and a core request,
//! about 70 microseconds in a VM, so reading the test corpus 4 KiB at a time took 5 times as long
//! as on Linux. Once a file's reads are seen to be small and sequential, the next window of the
//! file is read from the core in one request and the following reads are answered from it.
//!
//! The window starts at [`FIRST_WINDOW`] and doubles up to [`MAX_WINDOW`]; it starts only at
//! the third small read in sequence, so that a program reading a file's header or tags doesn't
//! make the core decode audio nobody reads. All windows together hold at most [`BUDGET`]; past
//! it, reads go straight through. A file's bytes never change while it is open, so a window is
//! never stale.

use std::sync::atomic::{AtomicUsize, Ordering};

/// A read smaller than this is small.
pub const SMALL: usize = 64 << 10;
/// The first window.
pub const FIRST_WINDOW: usize = 128 << 10;
/// The largest window.
pub const MAX_WINDOW: usize = 1 << 20;
/// Small reads in sequence before the first window.
const SEQUENTIAL: u32 = 2;
/// What all windows hold together, at most.
pub const BUDGET: usize = 64 << 20;

static HELD: AtomicUsize = AtomicUsize::new(0);

/// One open file's read-ahead.
#[derive(Debug, Default)]
pub struct Ahead {
    /// Where the next read would start if reads go on in sequence.
    next: u64,
    /// Small reads in sequence so far.
    run: u32,
    /// The size of the next window.
    window: usize,
    /// The window: its offset in the file and its bytes.
    at: u64,
    buf: Vec<u8>,
}

impl Drop for Ahead {
    fn drop(&mut self) {
        HELD.fetch_sub(self.buf.len(), Ordering::Relaxed);
    }
}

impl Ahead {
    /// Answers a read of `out.len()` bytes at `offset` from the window, if it holds them all.
    pub fn serve(&mut self, offset: u64, out: &mut [u8]) -> Option<usize> {
        let start = offset.checked_sub(self.at)? as usize;
        let end = start.checked_add(out.len())?;
        if end > self.buf.len() {
            return None;
        }
        out.copy_from_slice(&self.buf[start..end]);
        self.next = offset + out.len() as u64;
        Some(out.len())
    }

    /// For a read of `len` bytes at `offset` that the window doesn't hold, in a file of `size`
    /// bytes: how much to read from the core instead (a window), or `None` to read just the
    /// bytes asked for. Notes the read either way.
    pub fn plan(&mut self, offset: u64, len: usize, size: u64) -> Option<usize> {
        let in_sequence = offset == self.next;
        self.next = offset.saturating_add(len as u64);
        if len >= SMALL || !in_sequence {
            self.run = 0;
            self.window = 0;
            self.release();
            return None;
        }
        self.run += 1;
        if self.run <= SEQUENTIAL {
            return None;
        }
        self.window = if self.window == 0 { FIRST_WINDOW } else { (self.window * 2).min(MAX_WINDOW) };
        let left = size.saturating_sub(offset).min(self.window as u64) as usize;
        if left <= len {
            return None;
        }
        // Within the budget, counting this window's old bytes as given back.
        let old = self.buf.len();
        let mut held = HELD.load(Ordering::Relaxed);
        loop {
            let new = held.saturating_sub(old).checked_add(left)?;
            if new > BUDGET {
                return None;
            }
            match HELD.compare_exchange_weak(held, new, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(h) => held = h,
            }
        }
        // The old bytes are no longer counted: the window about to be filled replaces them.
        self.buf = Vec::new();
        Some(left)
    }

    /// The window read for [`Ahead::plan`]: `data`, at `offset`, of which the read took `used`
    /// bytes. `reserved` is what `plan` counted against the budget.
    pub fn fill(&mut self, offset: u64, data: Vec<u8>, used: usize, reserved: usize) {
        HELD.fetch_sub(reserved.saturating_sub(data.len()), Ordering::Relaxed);
        self.at = offset;
        self.buf = data;
        self.next = offset + used as u64;
    }

    /// Gives a planned window's reservation back (its read failed).
    pub fn cancel(&mut self, reserved: usize) {
        HELD.fetch_sub(reserved, Ordering::Relaxed);
    }

    /// Lets the window go: the file was read to its end, or closed by its program (Windows
    /// keeps a file it caches open after its program closed it, so the close itself may come
    /// much later).
    pub fn clear(&mut self) {
        self.release();
        self.run = 0;
        self.window = 0;
    }

    fn release(&mut self) {
        HELD.fetch_sub(self.buf.len(), Ordering::Relaxed);
        self.buf = Vec::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file whose byte at offset `i` is `i % 251`, read 4 KiB at a time through `Ahead`;
    /// returns the reads that reached the "core" and their sizes.
    fn read_all(size: u64, step: usize) -> (Vec<(u64, usize)>, Vec<u8>) {
        let byte = |i: u64| (i % 251) as u8;
        let mut a = Ahead::default();
        let mut core = Vec::new();
        let mut out = Vec::new();
        let mut off = 0u64;
        while off < size {
            let len = step.min((size - off) as usize);
            let mut buf = vec![0u8; len];
            if a.serve(off, &mut buf).is_none() {
                match a.plan(off, len, size) {
                    Some(w) => {
                        core.push((off, w));
                        let data: Vec<u8> = (off..off + w as u64).map(byte).collect();
                        buf.copy_from_slice(&data[..len]);
                        a.fill(off, data, len, w);
                    }
                    None => {
                        core.push((off, len));
                        buf = (off..off + len as u64).map(byte).collect();
                    }
                }
            }
            out.extend_from_slice(&buf);
            off += len as u64;
        }
        (core, out)
    }

    #[test]
    fn small_sequential_reads_go_to_the_core_in_windows() {
        let size = 3 << 20;
        let (core, out) = read_all(size, 4096);
        assert_eq!(out.len() as u64, size);
        assert!(out.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8), "same bytes");
        // Two reads as asked, then windows of 128 KiB, 256 KiB, 512 KiB, 1 MiB, 1 MiB...
        assert_eq!(&core[..3], &[(0, 4096), (4096, 4096), (8192, FIRST_WINDOW)]);
        assert!(core.len() < 10, "{} core reads for 768 small ones", core.len());
        assert!(core.iter().all(|&(_, n)| n <= MAX_WINDOW));
    }

    #[test]
    fn the_last_window_stops_at_the_end() {
        let size = 200_000;
        let (core, out) = read_all(size, 4096);
        assert_eq!(out.len() as u64, size);
        let (off, n) = *core.last().unwrap();
        assert!(off + n as u64 <= size);
    }

    #[test]
    fn large_or_scattered_reads_go_straight_through() {
        let mut a = Ahead::default();
        assert_eq!(a.plan(0, SMALL, 10 << 20), None);
        for off in [0u64, 100_000, 50_000, 900_000] {
            assert_eq!(a.plan(off, 4096, 10 << 20), None, "a seek restarts the run");
        }
    }

    #[test]
    fn a_header_and_tags_reader_gets_no_window() {
        let mut a = Ahead::default();
        assert_eq!(a.plan(0, 4096, 10 << 20), None);
        assert_eq!(a.plan(4096, 4096, 10 << 20), None);
        assert_eq!(a.plan((10 << 20) - 4096, 4096, 10 << 20), None);
    }
}
