// SPDX-License-Identifier: GPL-3.0-or-later

//! How many sidecar files may be open at once: each open sidecar holds a descriptor of its
//! source file until it is released, so without a bound a client that keeps opening sidecars
//! could use up the process's descriptors, and images would then fail to open. An open past
//! the bound fails with `EMFILE` for that client alone.
//!
//! The bound comes from the open files limit (raised to the hard limit at start), less the
//! image files kept open (`--max-open-images`) and a margin for everything else (the FUSE
//! device, logs, the seek index, the images a listing opens, network servers' own files), at
//! most [`MAX_CAP`]. *Why a bound and not a shared descriptor per sidecar:* an open sidecar
//! keeps reading the file it opened when the art is replaced meanwhile (#235).

use std::sync::atomic::{AtomicUsize, Ordering};

/// The bound before [`Slots::set_cap`] (a library user, the tests).
pub const DEFAULT_CAP: usize = 1024;

/// The highest bound, whatever the open files limit.
pub const MAX_CAP: usize = 4096;

/// Descriptors left for everything but image files and sidecars.
pub const MARGIN: u64 = 128;

/// The lowest bound: a few covers open at once always work.
pub const MIN_CAP: usize = 8;

/// The bound and the count of open sidecar files.
#[derive(Debug)]
pub struct Slots {
    cap: AtomicUsize,
    open: AtomicUsize,
}

static GLOBAL: Slots = Slots::new(DEFAULT_CAP);

/// The bound every sidecar open of the process counts against.
pub fn global() -> &'static Slots {
    &GLOBAL
}

/// The bound for an open files limit of `limit` with `max_open_images` image files kept open.
pub fn cap_for(limit: u64, max_open_images: usize) -> usize {
    let left = limit.saturating_sub(max_open_images as u64).saturating_sub(MARGIN);
    usize::try_from(left).unwrap_or(usize::MAX).clamp(MIN_CAP, MAX_CAP)
}

impl Slots {
    /// A bound of `cap` open sidecar files, none open.
    pub const fn new(cap: usize) -> Slots {
        Slots { cap: AtomicUsize::new(cap), open: AtomicUsize::new(0) }
    }

    /// Sets the bound.
    pub fn set_cap(&self, cap: usize) {
        self.cap.store(cap.max(1), Ordering::Relaxed);
    }

    /// The bound.
    pub fn cap(&self) -> usize {
        self.cap.load(Ordering::Relaxed)
    }

    /// Sidecar files open now.
    pub fn open(&self) -> usize {
        self.open.load(Ordering::Relaxed)
    }

    /// A place for one more open sidecar file, or `None` if [`Slots::cap`] are open already.
    pub fn take(&'static self) -> Option<Slot> {
        let cap = self.cap();
        let mut n = self.open.load(Ordering::Acquire);
        loop {
            if n >= cap {
                return None;
            }
            match self.open.compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(Slot(self)),
                Err(now) => n = now,
            }
        }
    }
}

/// One open sidecar file's place under the bound, given back when dropped.
#[derive(Debug)]
pub struct Slot(&'static Slots);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cap_leaves_room_for_images() {
        assert_eq!(cap_for(256, 64), 64);
        assert_eq!(cap_for(1024, 64), 832);
        assert_eq!(cap_for(524_288, 64), MAX_CAP);
        assert_eq!(cap_for(100, 64), MIN_CAP);
    }

    #[test]
    fn slots_are_bounded_and_given_back() {
        let slots: &'static Slots = Box::leak(Box::new(Slots::new(3)));
        let held: Vec<Slot> = (0..3).map(|_| slots.take().unwrap()).collect();
        assert!(slots.take().is_none());
        drop(held);
        assert_eq!(slots.open(), 0);
        assert!(slots.take().is_some());
    }
}
