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

/// A log known to be synced as far as `len`: what does not read past it is
/// an append that never finished.
fn synced(len: u64) -> alt_store::Tail {
    alt_store::Tail::synced_to(move |_| Some(len))
}
