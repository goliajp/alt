//! What indexing a pack may take.

use alt_git_codec::ObjectId;

use super::{BaseFiles, Memory, scan::Entry};

/// What indexing keeps of each entry until the index is written: where it
/// is, the id it has twice over, how it is found among the deltas, and its
/// line of the index as it is put in order.
pub const HELD_FOR_AN_INDEXED_ENTRY: u64 = (size_of::<Entry>()
    + 2 * size_of::<Option<ObjectId>>()
    + size_of::<(ObjectId, u32)>()
    + size_of::<u32>()
    + size_of::<(ObjectId, u64, u32)>()) as u64;

/// What indexing a pack may take.
#[derive(Debug, Clone, Copy)]
pub struct IndexLimits<'a> {
    /// the most bytes one object of the pack may inflate to
    pub object_bytes: u64,
    /// the most bytes its entries and its objects may inflate to, all
    /// together, an object counted again each time it has to be made
    /// again
    pub inflated_bytes: u64,
    /// the bytes of delta bases held in memory while their deltas are
    /// resolved
    pub base_bytes: usize,
    /// the bytes of delta bases written to files of their own, to be read
    /// again, when memory has no room for them while their deltas are
    /// resolved. The packs of histories keep a few megabytes of bases
    /// waiting at once and write none; the files are for a pack whose
    /// objects are large. By default they hold sixteen times what memory
    /// does, a gigabyte and a half: the largest object a push may bring
    /// by default and half again, and all that one push takes of the
    /// disk beside its pack. Past them a base is made again when it is
    /// come back to
    pub spill_bytes: u64,
    /// whether the pack is kept: its index is synced then before it
    /// takes its name. One that is read and removed again needs no sync
    pub kept: bool,
    /// the bytes an object may take without memory being taken for it
    /// alone: the pack has as many throughout
    pub each_bytes: u64,
    /// what the pack takes its memory out of, and waits for
    pub memory: Option<&'static Memory>,
    /// until when it waits for its memory, where it does not wait for
    /// as long as it takes
    pub memory_by: Option<std::time::Instant>,
    /// set once whoever sent the pack waits for it no longer: the pack
    /// waits for nothing from then on, and is indexed no further
    pub given_up: Option<&'a std::sync::atomic::AtomicBool>,
    /// where its bases in files are written, and what all the packs take
    /// there between them
    pub files: Option<&'static BaseFiles>,
}

impl Default for IndexLimits<'_> {
    /// No limit to what the pack holds, for a pack of a remote that was
    /// asked for it.
    fn default() -> Self {
        Self {
            object_bytes: u64::MAX,
            inflated_bytes: u64::MAX,
            base_bytes: crate::cache::DEFAULT_BUDGET,
            spill_bytes: 16 * crate::cache::DEFAULT_BUDGET as u64,
            kept: true,
            each_bytes: super::EACH,
            memory: None,
            memory_by: None,
            given_up: None,
            files: None,
        }
    }
}

impl<'a> IndexLimits<'a> {
    /// Lets the pack in with `bytes` throughout, beside what it has for the
    /// objects that need no memory of their own, and with a largest object
    /// that takes `object`.
    pub(super) fn let_in(
        &self,
        bytes: u64,
        object: u64,
    ) -> Result<Option<super::Taken<'static>>, crate::PackError> {
        let Some(memory) = self.memory else {
            return Ok(None);
        };
        let object = if object > self.each_bytes { object } else { 0 };
        let bytes = bytes.saturating_add(self.each_bytes);
        memory.let_in(bytes, object, self.wait()).map(Some)
    }

    /// What the pack has throughout while it is resolved, and what its
    /// largest object takes: the bases that are held and what is kept of
    /// each entry; and the object as a base, as the instructions of a delta
    /// and as what they make.
    pub(super) fn resolving(&self, scanned: &super::scan::Scanned) -> (u64, u64) {
        let held = (self.base_bytes as u64).min(scanned.inflated);
        let entries = super::scan::held(scanned.entries.len());
        (
            held.saturating_add(entries),
            scanned.largest.saturating_mul(3),
        )
    }

    /// where an object that is large has its memory taken
    pub(super) fn objects<'b>(
        &self,
        taken: &'b Option<super::Taken<'static>>,
    ) -> Option<super::ObjectMemory<'b>> {
        super::ObjectMemory::of(taken, self.each_bytes)
    }

    /// The pack is scanned and made whole no further once nobody waits for it.
    pub(super) fn going_on(&self) -> Result<(), crate::PackError> {
        match self.given_up {
            Some(gone) if gone.load(std::sync::atomic::Ordering::Relaxed) => {
                Err(crate::PackError::GivenUp)
            }
            _ => Ok(()),
        }
    }

    /// how long the pack waits for what others have
    pub(super) fn wait(&self) -> super::Wait<'a> {
        super::Wait {
            by: self.memory_by,
            gone: self.given_up,
        }
    }
}
