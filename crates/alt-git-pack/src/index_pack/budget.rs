//! What resolving the deltas of one pack may take.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

use super::memory::{Taken, Wait};
use crate::PackError;

/// The memory the walks of a pack make large objects in.
#[derive(Clone, Copy)]
pub struct Objects<'a> {
    /// what the pack was let in with, which says what its largest object
    /// takes
    pub of: &'a Taken<'static>,
    /// what each walk has without asking, as a part of what the pack has
    /// throughout: an object that takes no more is taken for nowhere
    pub each: u64,
}

impl<'a> Objects<'a> {
    /// of the memory a pack was let in with, where it was let in with any
    pub fn of(taken: &'a Option<Taken<'static>>, each: u64) -> Option<Self> {
        taken.as_ref().map(|of| Objects { of, each })
    }

    /// Makes `held` the memory of a walk that has, or is about to make, an
    /// object that takes `bytes`: none where each walk has as much, and
    /// otherwise what the largest object of the pack takes, which the walk
    /// keeps until it has no large object any more.
    pub fn room(
        &self,
        held: &mut Option<Taken<'static>>,
        bytes: u64,
        wait: Wait<'_>,
    ) -> Result<(), PackError> {
        match held {
            _ if bytes <= self.each => *held = None,
            Some(_) => {}
            None => *held = Some(self.of.object(wait)?),
        }
        Ok(())
    }
}

/// The bases one pack has in files at once. Each is a file and a mapping, and
/// a pack of many small objects that wait would have as many as it has bases.
pub const SPILL_FILES: usize = 1024;

/// What the walks of one pack may take, and what they have between them.
pub struct Budget<'a> {
    /// the most bytes one object may have
    pub object_bytes: u64,
    /// the most bytes the entries and the objects may inflate to
    pub inflated_bytes: u64,
    /// the bytes of bases held in memory, and in files of their own
    pub base_bytes: usize,
    pub spill_bytes: u64,
    /// the bases of the pack in files of their own at once, whatever their bytes
    pub spill_files: usize,
    /// the deepest a delta may lie below a plain entry
    pub depth: u32,
    /// where the bases in files are written, and what the packs indexed at
    /// once may take there between them; among the temporary files of the
    /// host, and no common amount, where this is not told
    pub files: Option<&'static super::BaseFiles>,
    /// where the walks take the memory of large objects, one at a time; out
    /// of nothing where this is not told
    pub objects: Option<Objects<'a>>,
    /// How long a walk waits for memory, and for room among the files. Once
    /// whoever waits for the pack is gone, the walks end at the next delta
    /// they come to.
    pub wait: Wait<'a>,
    pub(super) inflated: AtomicU64,
    pub(super) held: AtomicUsize,
    pub(super) spilled: AtomicU64,
    pub(super) spilled_files: AtomicUsize,
    pub(super) most_spilled_files: AtomicUsize,
    pub(super) most_held: AtomicUsize,
    pub(super) most_spilled: AtomicU64,
}

impl Budget<'_> {
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
            spill_files: SPILL_FILES,
            depth,
            files: None,
            objects: None,
            wait: Wait::default(),
            inflated: AtomicU64::new(inflated),
            held: AtomicUsize::new(0),
            spilled: AtomicU64::new(0),
            spilled_files: AtomicUsize::new(0),
            most_spilled_files: AtomicUsize::new(0),
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

    /// The most bases in files at once.
    pub fn most_files(&self) -> usize {
        self.most_spilled_files.load(Relaxed)
    }

    pub(super) fn waited_for(&self) -> Result<(), PackError> {
        match self.wait.gone {
            Some(gone) if gone.load(Relaxed) => Err(PackError::GivenUp),
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

/// The work indexing a pack took.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IndexWork {
    /// zlib streams inflated
    pub inflated: u64,
    /// objects hashed to their ids
    pub hashed: u64,
    /// deltas applied
    pub applied: u64,
    /// the most bytes of delta bases held at once
    pub held: usize,
    /// the most bytes of delta bases in files at once
    pub spilled: u64,
    /// the most delta bases in files at once
    pub spilled_files: usize,
}
