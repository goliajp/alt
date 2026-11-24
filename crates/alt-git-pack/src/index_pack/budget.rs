//! What resolving the deltas of one pack may take.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};

use crate::PackError;

/// What the walks of one pack may take, and what they have between them.
pub struct Budget {
    /// the most bytes one object may have
    pub object_bytes: u64,
    /// the most bytes the entries and the objects may inflate to
    pub inflated_bytes: u64,
    /// the bytes of bases held in memory, and in files of their own
    pub base_bytes: usize,
    pub spill_bytes: u64,
    /// the deepest a delta may lie below a plain entry
    pub depth: u32,
    /// set by whoever waits for the pack once it waits no longer: the walks end
    /// at the next delta they come to
    pub given_up: Option<Arc<AtomicBool>>,
    /// where the bases in files are written, and what the packs indexed at
    /// once may take there between them; among the temporary files of the
    /// host, and no common amount, where this is not told
    pub files: Option<&'static super::BaseFiles>,
    pub(super) inflated: AtomicU64,
    pub(super) held: AtomicUsize,
    pub(super) spilled: AtomicU64,
    pub(super) most_held: AtomicUsize,
    pub(super) most_spilled: AtomicU64,
}

impl Budget {
    /// `inflated` is what the entries inflated to when they were scanned.
    pub fn new(
        (object_bytes, inflated_bytes): (u64, u64),
        (base_bytes, spill_bytes): (usize, u64),
        depth: u32,
        inflated: u64,
    ) -> Self {
        Budget {
            object_bytes,
            inflated_bytes,
            base_bytes,
            spill_bytes,
            depth,
            given_up: None,
            files: None,
            inflated: AtomicU64::new(inflated),
            held: AtomicUsize::new(0),
            spilled: AtomicU64::new(0),
            most_held: AtomicUsize::new(0),
            most_spilled: AtomicU64::new(0),
        }
    }

    pub fn inflated(&self) -> u64 {
        self.inflated.load(Relaxed)
    }

    /// The most bytes of bases held at once, and in files at once.
    pub fn most(&self) -> (usize, u64) {
        (
            self.most_held.load(Relaxed),
            self.most_spilled.load(Relaxed),
        )
    }

    pub(super) fn waited_for(&self) -> Result<(), PackError> {
        match &self.given_up {
            Some(given_up) if given_up.load(Relaxed) => Err(PackError::GivenUp),
            _ => Ok(()),
        }
    }

    /// Counts an object made of a delta, or made again, against what the
    /// pack may inflate to.
    pub(super) fn count(&self, bytes: usize) -> Result<(), PackError> {
        let within = |total: u64| {
            (total.checked_add(bytes as u64)).filter(|total| *total <= self.inflated_bytes)
        };
        (self.inflated.try_update(Relaxed, Relaxed, within))
            .map(|_| ())
            .map_err(|_| PackError::Limit("inflated bytes"))
    }
}
