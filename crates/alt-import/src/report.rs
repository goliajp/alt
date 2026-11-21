//! What an import tells of itself.

/// What one import run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// Objects enumerated in the source (packed + loose).
    pub objects_seen: u64,
    /// Objects actually written (the rest were already present).
    pub objects_new: u64,
    /// Refs (incl. HEAD) in the source.
    pub refs_seen: usize,
    /// Refs created or moved by this run.
    pub refs_changed: usize,
    /// Same-path versions re-encoded as lineage deltas (blobs + trees).
    pub lineage_deltas: u64,
    /// The subset of `lineage_deltas` that are tree objects.
    pub tree_lineage_deltas: u64,
    /// The subset of `lineage_deltas` that are commit objects.
    pub commit_lineage_deltas: u64,
    /// The import op — None when state was already converged (rerun).
    pub op: Option<alt_refs::OpId>,
}
