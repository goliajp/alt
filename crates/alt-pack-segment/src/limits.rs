//! What verifying a received pack may take.

use alt_git_pack::Memory;
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub objects: u64,
    pub object_bytes: u64,
    /// what the entries and the objects deltas make inflate to, all together, an
    /// object counted again each time it has to be made again
    pub inflated_bytes: u64,
    pub delta_depth: u16,
    /// The parents the commits may name beyond two for each entry of the pack. They
    /// are written into the index of the segment and read with it, and deltas make
    /// commits of many parents out of few bytes.
    pub parents_beside: usize,
    /// the bytes of delta bases held in memory while their deltas are resolved, and
    /// of those written to files of their own when memory has no room for them
    pub base_bytes: usize,
    pub spill_bytes: u64,
    /// what the pack takes its memory out of, and until when it waits for it
    pub memory: Option<&'static Memory>,
    pub memory_by: Option<Instant>,
}

impl Limits {
    /// The bases held as an index of a pack holds them, out of no common amount. The
    /// commits of a history have little more than one parent each; one that joins
    /// sixty thousand branches still fits.
    pub const fn of(objects: u64, object_bytes: u64, inflated_bytes: u64) -> Self {
        Limits {
            objects,
            object_bytes,
            inflated_bytes,
            delta_depth: 4095,
            parents_beside: 1 << 16,
            base_bytes: 96 << 20,
            spill_bytes: 16 * (96 << 20),
            memory: None,
            memory_by: None,
        }
    }
}
