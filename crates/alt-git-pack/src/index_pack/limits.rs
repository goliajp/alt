//! What indexing a pack may take.

use super::{BaseFiles, Memory};

/// What indexing a pack may take.
#[derive(Debug, Clone, Copy)]
pub struct IndexLimits {
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
    /// what the pack takes its memory out of, and waits for
    pub memory: Option<&'static Memory>,
    /// until when it waits for its memory, where it does not wait for
    /// as long as it takes
    pub memory_by: Option<std::time::Instant>,
    /// where its bases in files are written, and what all the packs take
    /// there between them
    pub files: Option<&'static BaseFiles>,
}

impl Default for IndexLimits {
    /// No limit to what the pack holds, for a pack of a remote that was
    /// asked for it.
    fn default() -> Self {
        Self {
            object_bytes: u64::MAX,
            inflated_bytes: u64::MAX,
            base_bytes: crate::cache::DEFAULT_BUDGET,
            spill_bytes: 16 * crate::cache::DEFAULT_BUDGET as u64,
            kept: true,
            memory: None,
            memory_by: None,
            files: None,
        }
    }
}
