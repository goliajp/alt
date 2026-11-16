//! Single integration-test binary for alt-store.

mod blob_store;
mod chunk_store;
mod compact_readers;
mod corpus_blobs;
mod damaged_log;
mod index_gap;
mod index_runs;
mod stale_index;
mod synced_tail;
mod tail_basis;

/// A log known to be synced as far as `len`, opened by the holder of the
/// lock its writers append under: what does not read past that length is
/// an append that never finished, and is cut off.
fn synced(len: u64) -> alt_store::Tail {
    alt_store::Tail::judged_by(move |_, _| alt_store::Basis::Boundary(len)).locked()
}
